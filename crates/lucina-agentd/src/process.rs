//! Provider child processes. Each runs in its own process group, so closing a
//! session (or dropping it) kills the whole tree; its stderr tail is kept for error
//! messages and quota detection. Every live child is recorded under
//! `<state>/procs/<pid>.json` with its start time, so a restarted daemon reaps what
//! a crash left behind without ever killing a reused pid (Paseo:
//! `runtime/managed-processes/`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const STDERR_TAIL_LINES: usize = 200;

/// What to run.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    /// Environment variables removed from the child (e.g. tokens the daemon holds).
    pub env_remove: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    pid: u32,
    start_ticks: u64,
    program: String,
}

/// The registry of live children (`<state>/procs`).
#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
}

/// Field 22 of `/proc/<pid>/stat`: the process start time in clock ticks.
fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 2..];
    after_comm.split(' ').nth(19)?.parse().ok()
}

fn kill_group(pid: u32, signal: &str) {
    let _ = std::process::Command::new("kill")
        .args(["-s", signal, "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Registry {
    pub fn open(state_dir: &Path) -> std::io::Result<Self> {
        let dir = state_dir.join("procs");
        std::fs::create_dir_all(&dir)?;
        Ok(Registry { dir })
    }

    fn record(&self, pid: u32, program: &str) {
        let Some(start_ticks) = start_ticks(pid) else {
            return;
        };
        let rec = Record {
            pid,
            start_ticks,
            program: program.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&rec) {
            let _ = std::fs::write(self.dir.join(format!("{pid}.json")), json);
        }
    }

    fn forget(&self, pid: u32) {
        let _ = std::fs::remove_file(self.dir.join(format!("{pid}.json")));
    }

    /// Record a process agentd started some other way (a terminal's session leader):
    /// it counts as an agent process, and the next start reaps it if it survived.
    pub fn track(&self, pid: u32, program: &str) {
        self.record(pid, program);
    }

    pub fn untrack(&self, pid: u32) {
        self.forget(pid);
    }

    /// The pids of the provider processes alive now (a record whose pid was reused
    /// by another process doesn't count).
    pub fn live_pids(&self) -> std::collections::HashSet<u32> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Default::default();
        };
        entries
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str::<Record>(&s).ok())
            .filter(|r| start_ticks(r.pid) == Some(r.start_ticks))
            .map(|r| r.pid)
            .collect()
    }

    /// Kill the process groups a previous daemon left running. A record whose pid
    /// now belongs to another process (different start time) is only removed.
    /// Returns the pids that were killed.
    pub fn reap_leftovers(&self) -> Vec<u32> {
        let mut killed = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return killed;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rec: Option<Record> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
            if let Some(rec) = rec {
                if start_ticks(rec.pid) == Some(rec.start_ticks) {
                    tracing::warn!(pid = rec.pid, program = %rec.program, "reaping a provider process left by a previous daemon");
                    kill_group(rec.pid, "KILL");
                    killed.push(rec.pid);
                }
            }
            let _ = std::fs::remove_file(path);
        }
        killed
    }
}

/// A running child. Dropping it kills its process group.
pub struct Proc {
    pub pid: u32,
    child: Option<Child>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    registry: Option<Registry>,
}

impl Proc {
    /// Start `spec` with piped stdin/stdout; stderr is read into the tail buffer. The child
    /// gets the built environment ([`crate::env`], from this process's own), and `spec.env`
    /// on top of it.
    pub fn spawn(
        spec: &Spec,
        registry: Option<&Registry>,
    ) -> std::io::Result<(Proc, ChildStdin, ChildStdout)> {
        Self::spawn_from(spec, registry, std::env::vars_os().collect())
    }

    /// [`Proc::spawn`], built from `source` instead of this process's environment — a
    /// parameter, so tests pass a stand-in in instead of mutating the process environment.
    pub fn spawn_from(
        spec: &Spec,
        registry: Option<&Registry>,
        source: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> std::io::Result<(Proc, ChildStdin, ChildStdout)> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .env_clear()
            .envs(crate::env::scrubbed_env_from(source, &[]).0);
        for key in &spec.env_remove {
            cmd.env_remove(key);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout"))?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut t = tail.lock().unwrap_or_else(|p| p.into_inner());
                    if t.len() == STDERR_TAIL_LINES {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            });
        }
        if let Some(r) = registry {
            r.record(pid, &spec.program.display().to_string());
        }
        Ok((
            Proc {
                pid,
                child: Some(child),
                stderr_tail: tail,
                registry: registry.cloned(),
            },
            stdin,
            stdout,
        ))
    }

    /// The last stderr lines, joined.
    pub fn stderr_tail(&self) -> String {
        let t = self.stderr_tail.lock().unwrap_or_else(|p| p.into_inner());
        t.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// SIGTERM the group, wait up to `grace`, then SIGKILL.
    pub async fn shutdown(&mut self, grace: std::time::Duration) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        kill_group(self.pid, "TERM");
        if tokio::time::timeout(grace, child.wait()).await.is_err() {
            kill_group(self.pid, "KILL");
            let _ = child.wait().await;
        }
        if let Some(r) = &self.registry {
            r.forget(self.pid);
        }
    }

    /// The child has exited (its exit code, if any).
    pub fn try_exit(&mut self) -> Option<Option<i32>> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => None,
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if self.child.is_some() {
            kill_group(self.pid, "KILL");
            if let Some(r) = &self.registry {
                r.forget(self.pid);
            }
        }
    }
}

/// Resolve `~/` and bare names against `$PATH`.
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    let expanded = match name.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").ok()?).join(rest),
        None => PathBuf::from(name),
    };
    if expanded.components().count() > 1 {
        return expanded.is_file().then_some(expanded);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(&expanded))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| !s.contains(") Z "))
                .unwrap_or(false)
    }

    #[tokio::test]
    async fn leftovers_of_a_crashed_daemon_are_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path()).unwrap();
        let spec = Spec {
            program: "sleep".into(),
            args: vec!["300".into()],
            cwd: "/tmp".into(),
            ..Spec::default()
        };
        let (proc_, _stdin, _stdout) = Proc::spawn(&spec, Some(&reg)).unwrap();
        let pid = proc_.pid;
        // A crash: the daemon dies without running destructors.
        std::mem::forget(proc_);
        assert!(alive(pid));
        let fresh = Registry::open(dir.path()).unwrap();
        assert_eq!(fresh.reap_leftovers(), vec![pid]);
        for _ in 0..50 {
            if !alive(pid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "the leftover was killed");
        assert!(fresh.reap_leftovers().is_empty(), "records are gone");
    }

    #[tokio::test]
    async fn a_reused_pid_is_never_killed() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path()).unwrap();
        // A record whose start time doesn't match the live process with that pid.
        let me = std::process::id();
        std::fs::write(
            dir.path().join("procs").join(format!("{me}.json")),
            format!(r#"{{"pid":{me},"start_ticks":1,"program":"x"}}"#),
        )
        .unwrap();
        assert!(reg.reap_leftovers().is_empty());
    }

    #[tokio::test]
    async fn shutdown_kills_the_group_and_forgets_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path()).unwrap();
        let spec = Spec {
            program: "sh".into(),
            args: vec!["-c".into(), "sleep 300 & sleep 300".into()],
            cwd: "/tmp".into(),
            ..Spec::default()
        };
        let (mut proc_, _i, _o) = Proc::spawn(&spec, Some(&reg)).unwrap();
        let pid = proc_.pid;
        proc_.shutdown(std::time::Duration::from_secs(2)).await;
        assert!(!alive(pid));
        assert_eq!(
            std::fs::read_dir(dir.path().join("procs")).unwrap().count(),
            0
        );
    }

    #[tokio::test]
    async fn a_provider_process_gets_the_allowlisted_environment() {
        let dir = tempfile::tempdir().unwrap();
        let names_path = dir.path().join("env-names.txt");
        crate::testutil::write_exec(
            &dir.path().join("dump-env"),
            &format!("#!/bin/sh\nenv | cut -d= -f1 > {}\n", names_path.display()),
        );
        let spec = Spec {
            program: dir.path().join("dump-env"),
            args: vec![],
            cwd: dir.path().display().to_string(),
            env: vec![("SPEC_CALL_SITE".into(), "1".into())],
            env_remove: vec![],
        };
        let source = crate::testutil::env_source(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/fake"),
            ("FAKE_API_TOKEN", "fake-token"),
            ("CLOUDFLARE_FAKE_ID", "fake-id"),
        ]);
        let (mut proc_, _stdin, _stdout) = Proc::spawn_from(&spec, None, source).unwrap();
        let mut exited = false;
        for _ in 0..100 {
            if proc_.try_exit().is_some() {
                exited = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(exited, "the dump did not finish");
        let names = std::fs::read_to_string(&names_path).unwrap();
        for n in ["FAKE_API_TOKEN", "CLOUDFLARE_FAKE_ID"] {
            assert!(!names.lines().any(|l| l == n), "{names}");
        }
        for n in ["PATH", "HOME", "SPEC_CALL_SITE"] {
            assert!(names.lines().any(|l| l == n), "{names}");
        }
    }
}
