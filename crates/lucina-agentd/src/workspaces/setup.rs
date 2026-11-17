//! `paseo.json` (Paseo `protocol/paseo-config-schema.js`), read unchanged: the
//! `worktree.setup` commands run in a new worktree one after another and stop at the
//! first failure; `worktree.teardown` runs before the worktree is removed. Parse
//! failures count as `{}`, as in Paseo.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use lucina_agents::workspace::{SetupCommand, SetupSnapshot};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub const CONFIG_FILE: &str = "paseo.json";
const LOG_LIMIT: usize = 64 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(900);

pub fn read_config(root: &Path) -> Value {
    std::fs::read_to_string(root.join(CONFIG_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Default::default()))
}

/// `"cmd"` or `["cmd", ...]`.
pub fn commands(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|c| c.as_str())
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The environment setup and teardown see (Paseo's names, plus Lucina aliases).
pub fn env(
    source: &Path,
    worktree: &Path,
    branch: &str,
    port: Option<u16>,
) -> Vec<(String, String)> {
    let mut e = vec![
        (
            "PASEO_SOURCE_CHECKOUT_PATH".to_string(),
            source.display().to_string(),
        ),
        ("PASEO_ROOT_PATH".to_string(), source.display().to_string()),
        (
            "PASEO_WORKTREE_PATH".to_string(),
            worktree.display().to_string(),
        ),
        ("PASEO_BRANCH_NAME".to_string(), branch.to_string()),
        (
            "LUCINA_SOURCE_CHECKOUT_PATH".to_string(),
            source.display().to_string(),
        ),
        (
            "LUCINA_WORKTREE_PATH".to_string(),
            worktree.display().to_string(),
        ),
        ("LUCINA_BRANCH_NAME".to_string(), branch.to_string()),
    ];
    if let Some(p) = port {
        e.push(("PASEO_WORKTREE_PORT".into(), p.to_string()));
        e.push(("LUCINA_WORKTREE_PORT".into(), p.to_string()));
    }
    e
}

/// Run one shell command in `dir`; returns (exit code, log). The shell gets the built
/// environment ([`crate::env`], from this process's own), and `env` on top of it.
pub async fn run_command(cmd: &str, dir: &Path, env: &[(String, String)]) -> (Option<i32>, String) {
    run_command_from(cmd, dir, env, std::env::vars_os().collect()).await
}

/// [`run_command`], built from `source` instead of this process's environment — a
/// parameter, so tests pass a stand-in in instead of mutating the process environment.
async fn run_command_from(
    cmd: &str,
    dir: &Path,
    env: &[(String, String)],
    source: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> (Option<i32>, String) {
    let child = Command::new("/bin/sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .env_clear()
        .envs(crate::env::scrubbed_env_from(source, &[]).0)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env_remove(crate::notify::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (None, format!("cannot start: {e}")),
    };
    let pid = child.id();
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let read = async {
        // Both pipes at once: a command that fills one while the other is read would hang.
        let read_out = async {
            let mut a = Vec::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_end(&mut a).await;
            }
            a
        };
        let read_err = async {
            let mut b = Vec::new();
            if let Some(e) = err.as_mut() {
                let _ = e.read_to_end(&mut b).await;
            }
            b
        };
        let (a, b) = tokio::join!(read_out, read_err);
        let status = child.wait().await;
        (status, a, b)
    };
    match tokio::time::timeout(COMMAND_TIMEOUT, read).await {
        Ok((status, a, b)) => {
            let mut log = String::from_utf8_lossy(&a).to_string();
            log.push_str(&String::from_utf8_lossy(&b));
            if log.len() > LOG_LIMIT {
                let cut = log.len() - LOG_LIMIT;
                let start = (cut..log.len())
                    .find(|i| log.is_char_boundary(*i))
                    .unwrap_or(cut);
                log = format!("…{}", &log[start..]);
            }
            (status.ok().and_then(|s| s.code()), log)
        }
        Err(_) => {
            if let Some(pid) = pid {
                let _ = std::process::Command::new("kill")
                    .args(["-s", "KILL", "--", &format!("-{pid}")])
                    .status();
            }
            (
                None,
                format!("timed out after {} s", COMMAND_TIMEOUT.as_secs()),
            )
        }
    }
}

/// Run setup commands, reporting each step through `progress`.
pub async fn run_setup(
    cmds: &[String],
    dir: &Path,
    env: &[(String, String)],
    mut progress: impl FnMut(&SetupSnapshot),
) -> SetupSnapshot {
    let cwd = dir.display().to_string();
    let mut snap = SetupSnapshot {
        status: "running".into(),
        commands: cmds
            .iter()
            .enumerate()
            .map(|(index, command)| SetupCommand {
                index,
                command: command.clone(),
                cwd: cwd.clone(),
                log: String::new(),
                status: "pending".into(),
                exit_code: None,
                duration_ms: None,
            })
            .collect(),
        error: None,
    };
    for (i, cmd) in cmds.iter().enumerate() {
        snap.commands[i].status = "running".into();
        progress(&snap);
        let started = Instant::now();
        let (code, log) = run_command(cmd, dir, env).await;
        let c = &mut snap.commands[i];
        c.duration_ms = Some(started.elapsed().as_millis() as u64);
        c.exit_code = code;
        c.log = log;
        if code == Some(0) {
            c.status = "completed".into();
        } else {
            c.status = "failed".into();
            snap.error = Some(format!("Setup command {} failed: {cmd}", i + 1));
            for rest in snap.commands.iter_mut().skip(i + 1) {
                rest.status = "skipped".into();
            }
            snap.status = "failed".into();
            progress(&snap);
            return snap;
        }
    }
    snap.status = "completed".into();
    progress(&snap);
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn setup_stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cmds = commands(&serde_json::json!([
            "echo one $PASEO_BRANCH_NAME",
            "false",
            "echo never"
        ]));
        let env = env(dir.path(), dir.path(), "feat/x", Some(4100));
        let mut steps = 0;
        let snap = run_setup(&cmds, dir.path(), &env, |_| steps += 1).await;
        assert_eq!(snap.status, "failed");
        assert_eq!(snap.commands[0].log.trim(), "one feat/x");
        assert_eq!(snap.commands[1].exit_code, Some(1));
        assert_eq!(snap.commands[2].status, "skipped");
        assert!(steps >= 3);
        assert_eq!(commands(&serde_json::json!("echo x")), vec!["echo x"]);
        assert!(commands(&serde_json::json!({"a": 1})).is_empty());
    }

    #[tokio::test]
    async fn a_setup_command_gets_the_allowlisted_environment() {
        let dir = tempfile::tempdir().unwrap();
        let names_path = dir.path().join("env-names.txt");
        let source = crate::testutil::env_source(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/fake"),
            ("SSH_AUTH_SOCK", "/run/user/1000/ssh.sock"),
            ("FAKE_API_TOKEN", "fake-token"),
            ("CLOUDFLARE_FAKE_ID", "fake-id"),
        ]);
        let env = env(dir.path(), dir.path(), "feat/x", Some(4100));
        let (code, log) = run_command_from(
            &format!("env | cut -d= -f1 > {}", names_path.display()),
            dir.path(),
            &env,
            source,
        )
        .await;
        assert_eq!(code, Some(0), "{log}");
        let names = std::fs::read_to_string(&names_path).unwrap();
        for n in ["FAKE_API_TOKEN", "CLOUDFLARE_FAKE_ID", "SSH_AUTH_SOCK"] {
            assert!(!names.lines().any(|l| l == n), "{names}");
        }
        for n in ["PATH", "HOME", "PASEO_BRANCH_NAME", "LUCINA_BRANCH_NAME"] {
            assert!(names.lines().any(|l| l == n), "{names}");
        }
    }
}
