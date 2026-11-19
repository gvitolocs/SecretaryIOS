//! Test helpers: a `lucina-agentd` process on a temp socket and state dir, and
//! `lagent` runs against it. Every spawned daemon runs in its own process group
//! and is killed on drop, also when a test panics.

#![allow(dead_code)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The same helper the unit tests use, compiled from one file rather than copied.
#[path = "../../src/testutil.rs"]
mod testutil;
#[allow(unused_imports)] // Only the binaries with a stand-in to write reach for it.
pub use testutil::write_exec;

pub const AGENTD: &str = env!("CARGO_BIN_EXE_lucina-agentd");
pub const LAGENT: &str = env!("CARGO_BIN_EXE_lagent");

/// Kills the child's process group on drop.
pub struct Guard {
    pub child: Option<Child>,
}

impl Guard {
    pub fn pid(&self) -> u32 {
        self.child.as_ref().map(|c| c.id()).unwrap_or(0)
    }

    /// SIGTERM the group, then wait up to `limit`; returns the exit code.
    pub fn terminate(&mut self, limit: Duration) -> Option<i32> {
        let child = self.child.as_mut()?;
        let _ = Command::new("kill")
            .args(["-s", "TERM", "--", &format!("-{}", child.id())])
            .status();
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Command::new("kill")
                .args(["-s", "KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.wait();
        }
    }
}

pub struct Daemon {
    pub dir: tempfile::TempDir,
    pub guard: Guard,
}

impl Daemon {
    pub fn socket(&self) -> PathBuf {
        self.dir.path().join("agentd.sock")
    }

    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub fn start() -> Daemon {
        Self::start_with_env(&[])
    }

    pub fn start_with_env(env: &[(&str, String)]) -> Daemon {
        let dir = tempfile::tempdir().expect("tempdir");
        let guard = spawn_daemon_env(dir.path(), env);
        Daemon { dir, guard }
    }

    /// Stop and start again on the same state.
    pub fn restart(&mut self) {
        assert_eq!(
            self.guard.terminate(Duration::from_secs(5)),
            Some(0),
            "clean exit on SIGTERM"
        );
        self.guard = spawn_daemon(self.dir.path());
    }

    pub fn lagent(&self, args: &[&str]) -> Run {
        run(Command::new(LAGENT)
            .args(args)
            .env("LUCINA_AGENTD_SOCKET", self.socket()))
    }

    /// `lagent <args> --json`, parsed.
    pub fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        let r = self.lagent(&a);
        assert_eq!(r.code, 0, "lagent {args:?} failed: {}", r.stderr);
        serde_json::from_str(&r.stdout)
            .unwrap_or_else(|e| panic!("lagent {args:?}: {e}: {}", r.stdout))
    }
}

pub fn spawn_daemon(dir: &Path) -> Guard {
    spawn_daemon_env(dir, &[])
}

pub fn spawn_daemon_env(dir: &Path, env: &[(&str, String)]) -> Guard {
    let socket = dir.join("agentd.sock");
    let child = Command::new(AGENTD)
        // The caps are the defaults, whatever the developer's shell exports.
        .env_remove("LUCINA_AGENTD_MAX_AGENTS_PER_CALLER")
        .env_remove("LUCINA_AGENTD_MAX_CREATES_PER_MINUTE")
        .env_remove("LUCINA_AGENTD_MAX_AGENTS_HOST")
        .envs(env.iter().map(|(k, v)| (k.to_string(), v.clone())))
        .args([
            "--state-dir",
            &dir.join("state").display().to_string(),
            "--socket",
            &socket.display().to_string(),
        ])
        .arg("--no-real-providers")
        .env("LUCINA_AGENTD_DEV", "1")
        .env("LUCINA_AGENTD_SWEEP_IDLE_SECS", "0")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(
            std::fs::File::create(dir.join("agentd.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        )
        .process_group(0)
        .spawn()
        .expect("spawn lucina-agentd");
    let guard = Guard { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "lucina-agentd did not create its socket"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    guard
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(cmd: &mut Command) -> Run {
    let out = cmd.stdin(Stdio::null()).output().expect("run command");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}
