//! The Hermes computer-use engine (`createComputerUseClient` in
//! `src/computer-use.js`): one task at a time, on a detached process group,
//! with a 200 s deadline and a cancellation that also stops the gateway.
//!
//! The child never inherits the host environment. Node built a fresh
//! `{HOME, USER, LANG, PATH}` map and passed only `GEMINI_API_KEY` through;
//! this does the same, so the agent cannot reach a host credential through
//! the orchestrator's environment.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::time::timeout;

use crate::{parse_computer_result, ComputerResult, Error};

/// `run-hermes-computer.sh`: the Hermes agent with the official Cua plugin,
/// pinned to `local:lucina-workspace`.
pub const DEFAULT_LAUNCHER: &str =
    "/home/nez/Projects/Lucina/lucina-cua/scripts/run-hermes-computer.sh";

/// Node's `timeoutMs` default.
pub const TASK_TIMEOUT: Duration = Duration::from_secs(200);
/// Node's `task.length > 16000`.
pub const MAX_PROMPT_CHARS: usize = 16_000;

/// Node's `2 * 1024 * 1024` output cap.
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// How long a stopped task may take to die before `SIGKILL`.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// Node's `15000` for `--stop-gateway`.
const GATEWAY_STOP_LIMIT: Duration = Duration::from_secs(15);

/// A running task's cancellation handle (`AbortSignal` in Node).
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the task to stop. Idempotent, and safe from any thread.
    pub fn cancel(&self) {
        if !self.flag.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolves once [`Self::cancel`] has been called.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

/// How the engine is reached.
#[derive(Debug, Clone)]
pub struct ComputerUseConfig {
    pub launcher: PathBuf,
    /// `LUCINA_COMPUTER_USE_ENABLED` exactly `true` (Node's `===`).
    pub enabled: bool,
    pub timeout: Duration,
    /// The one host credential the agent is allowed (`GEMINI_API_KEY`).
    pub gemini_api_key: Option<String>,
    /// `HOME` for the agent (`/home/nez`).
    pub home: PathBuf,
    /// `USER` for the agent (`nez`).
    pub user: String,
    /// Drop to this uid/gid when the server runs as root (Node: `1000`).
    pub drop_to: Option<u32>,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        Self {
            launcher: PathBuf::from(DEFAULT_LAUNCHER),
            enabled: false,
            timeout: TASK_TIMEOUT,
            gemini_api_key: None,
            home: PathBuf::from("/home/nez"),
            user: "nez".into(),
            drop_to: None,
        }
    }
}

impl ComputerUseConfig {
    /// Reads Node's env names. `LUCINA_COMPUTER_USE_LIVE=1` is not consulted
    /// here: it only gates the ignored live test.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let text = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let home = text("LUCINA_COMPUTER_USE_HOME").unwrap_or_else(|| "/home/nez".into());
        let user = text("LUCINA_COMPUTER_USE_USER").unwrap_or_else(|| "nez".into());
        let drop_to = if unsafe { libc::getuid() } == 0 {
            Some(
                text("LUCINA_COMPUTER_USE_UID")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1000),
            )
        } else {
            None
        };
        Self {
            launcher: text("LUCINA_COMPUTER_USE_LAUNCHER")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_LAUNCHER)),
            enabled: get("LUCINA_COMPUTER_USE_ENABLED").as_deref() == Some("true"),
            timeout: text("LUCINA_COMPUTER_USE_TIMEOUT_SECS")
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(TASK_TIMEOUT),
            gemini_api_key: text("GEMINI_API_KEY"),
            home: PathBuf::from(home),
            user,
            drop_to,
        }
    }

    /// `isComputerUseReady`: the flag and an existing launcher.
    pub fn ready(&self) -> bool {
        self.enabled && self.launcher.exists()
    }
}

/// `isComputerUseReady()` against a plain lookup (config or env).
pub fn is_computer_use_ready(get: &impl Fn(&str) -> Option<String>, launcher: &Path) -> bool {
    get("LUCINA_COMPUTER_USE_ENABLED").as_deref() == Some("true") && launcher.exists()
}

#[derive(Debug, Default)]
struct State {
    active: bool,
    recovery_required: bool,
}

/// The computer-use client. One instance per process: the desktop is single.
#[derive(Debug)]
pub struct Client {
    cfg: ComputerUseConfig,
    state: Mutex<State>,
}

impl Client {
    pub fn new(cfg: ComputerUseConfig) -> Self {
        Self {
            cfg,
            state: Mutex::new(State::default()),
        }
    }

    pub fn config(&self) -> &ComputerUseConfig {
        &self.cfg
    }

    pub fn ready(&self) -> bool {
        self.cfg.ready()
    }

    /// A stopped task whose gateway never answered leaves the desktop locked
    /// until the operator clears it (`unconfirmed_cancel`).
    pub fn needs_recovery(&self) -> bool {
        self.state.lock().map(|s| s.recovery_required).unwrap_or(true)
    }

    /// Clear the recovery lock after the gateway was stopped by hand.
    pub fn clear_recovery(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.recovery_required = false;
        }
    }

    fn command(&self, args: &[&str]) -> io::Result<Command> {
        let mut cmd = Command::new(&self.cfg.launcher);
        cmd.args(args)
            .env_clear()
            .env("HOME", &self.cfg.home)
            .env("USER", &self.cfg.user)
            .env("LANG", "C.UTF-8")
            .env(
                "PATH",
                format!(
                    "{}:/usr/local/bin:/usr/bin:/bin",
                    self.cfg.home.join(".local/bin").display()
                ),
            )
            .current_dir("/tmp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(key) = &self.cfg.gemini_api_key {
            cmd.env("GEMINI_API_KEY", key);
        }
        let std_cmd = cmd.as_std_mut();
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            std_cmd.process_group(0);
            if let Some(uid) = self.cfg.drop_to {
                std_cmd.uid(uid).gid(uid);
            }
        }
        Ok(cmd)
    }

    /// Run one task. `prompt` is passed literally on stdin; it is never
    /// interpolated into a shell.
    pub async fn run(&self, prompt: &str, cancel: &Cancel) -> Result<ComputerResult, Error> {
        if !self.cfg.ready() {
            return Err(Error::NotEnabled);
        }
        {
            let st = self.state.lock().map_err(|_| Error::Busy)?;
            if st.recovery_required {
                return Err(Error::RecoveryRequired);
            }
            if st.active {
                return Err(Error::Busy);
            }
        }
        let task = prompt.trim();
        if task.is_empty() || task.chars().count() > MAX_PROMPT_CHARS {
            return Err(Error::BadRequest);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        {
            let mut st = self.state.lock().map_err(|_| Error::Busy)?;
            if st.active {
                return Err(Error::Busy);
            }
            st.active = true;
        }
        let out = self.run_task(task, cancel).await;
        if let Ok(mut st) = self.state.lock() {
            st.active = false;
        }
        out
    }

    async fn run_task(&self, task: &str, cancel: &Cancel) -> Result<ComputerResult, Error> {
        let mut cmd = self.command(&[]).map_err(|_| Error::LauncherUnavailable)?;
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|_| Error::LauncherUnavailable)?;
        let pid = child.id();
        let Some(mut stdin) = child.stdin.take() else {
            return Err(Error::LauncherUnavailable);
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(Error::LauncherUnavailable);
        };
        let prompt = task.to_owned();
        // A write error means the launcher refused the prompt; the exit code
        // and the stream decide the outcome, exactly as in Node.
        let _writer = tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
        let mut reader = tokio::spawn(read_capped(stdout));

        let mut output: Option<Vec<u8>> = None;
        let mut status: Option<std::process::ExitStatus> = None;
        let mut failure: Option<Error> = None;
        let deadline = tokio::time::sleep(self.cfg.timeout);
        tokio::pin!(deadline);
        let cancelled = cancel.cancelled();
        tokio::pin!(cancelled);
        loop {
            if failure.is_some() || (status.is_some() && output.is_some()) {
                break;
            }
            tokio::select! {
                r = &mut reader, if output.is_none() => {
                    output = Some(match r {
                        Ok(Ok(buf)) => buf,
                        Ok(Err(e)) => { failure = Some(e); Vec::new() }
                        Err(_) => { failure = Some(Error::OutputLimit); Vec::new() }
                    });
                }
                s = child.wait(), if status.is_none() => {
                    match s {
                        Ok(st) => status = Some(st),
                        Err(_) => failure = Some(Error::LauncherUnavailable),
                    }
                }
                () = &mut deadline, if failure.is_none() => failure = Some(Error::Deadline),
                () = &mut cancelled, if failure.is_none() => failure = Some(Error::Cancelled),
            }
        }

        if let Some(error) = failure {
            reader.abort();
            kill_group(pid, libc::SIGINT);
            if timeout(KILL_GRACE, child.wait()).await.is_err() {
                kill_group(pid, libc::SIGKILL);
                let _ = child.wait().await;
            }
            return Err(match self.stop_gateway().await {
                Ok(()) => error,
                Err(_) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.recovery_required = true;
                    }
                    Error::UnconfirmedCancel
                }
            });
        }

        let text = String::from_utf8_lossy(output.as_deref().unwrap_or_default()).into_owned();
        let code = status.and_then(|s| s.code());
        if code != Some(0) {
            return Err(match parse_computer_result(&text) {
                Err(e) => e,
                Ok(_) => Error::NotCompleted,
            });
        }
        parse_computer_result(&text)
    }

    /// `--stop-gateway`: Hermes detaches accepted work on SIGINT, so the
    /// profile's gateway has to be stopped explicitly.
    async fn stop_gateway(&self) -> Result<(), Error> {
        let mut cmd = self
            .command(&["--stop-gateway"])
            .map_err(|_| Error::GatewayStopFailed)?;
        let mut child: Child = cmd.spawn().map_err(|_| Error::GatewayStopFailed)?;
        match timeout(GATEWAY_STOP_LIMIT, child.wait()).await {
            Ok(Ok(s)) if s.success() => Ok(()),
            Ok(_) => Err(Error::GatewayStopFailed),
            Err(_) => {
                let _ = child.kill().await;
                Err(Error::GatewayStopFailed)
            }
        }
    }
}

/// `process.kill(-pid, sig)`: the child leads its own process group.
fn kill_group(pid: Option<u32>, sig: i32) {
    if let Some(pid) = pid {
        // SAFETY: a plain kill(2) on a process group the caller created.
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }
}

/// Read the whole stream, refusing more than 2 MiB.
async fn read_capped(mut stdout: tokio::process::ChildStdout) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stdout.read(&mut chunk).await {
            Ok(0) => return Ok(buf),
            Ok(n) => {
                if buf.len() + n > MAX_OUTPUT_BYTES {
                    return Err(Error::OutputLimit);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => return Ok(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher_in(dir: &Path) -> (PathBuf, ComputerUseConfig) {
        let path = dir.join("fake-hermes.sh");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let cfg = ComputerUseConfig { launcher: path.clone(), enabled: true, ..Default::default() };
        (path, cfg)
    }

    #[test]
    fn readiness_needs_the_flag_and_a_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = launcher_in(dir.path());
        let on = |k: &str| (k == "LUCINA_COMPUTER_USE_ENABLED").then(|| "true".to_owned());
        assert!(is_computer_use_ready(&on, &path));
        assert!(!is_computer_use_ready(&|_| None, &path));
        assert!(!is_computer_use_ready(&on, &dir.path().join("missing")));
        // Node compares with `===`, so `True` is off.
        let odd = |k: &str| (k == "LUCINA_COMPUTER_USE_ENABLED").then(|| "True".to_owned());
        assert!(!is_computer_use_ready(&odd, &path));
    }

    #[tokio::test]
    async fn refuses_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut cfg) = launcher_in(dir.path());
        cfg.enabled = false;
        let client = Client::new(cfg);
        assert_eq!(client.run("x", &Cancel::new()).await.unwrap_err(), Error::NotEnabled);

        let (_, cfg) = launcher_in(dir.path());
        let client = Client::new(cfg);
        assert_eq!(client.run("   ", &Cancel::new()).await.unwrap_err(), Error::BadRequest);
        let long = "x".repeat(MAX_PROMPT_CHARS + 1);
        assert_eq!(client.run(&long, &Cancel::new()).await.unwrap_err(), Error::BadRequest);
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(client.run("uname -a", &cancel).await.unwrap_err(), Error::Cancelled);

        let (_, mut cfg) = launcher_in(dir.path());
        cfg.launcher = dir.path().join("absent.sh");
        let client = Client::new(cfg);
        assert_eq!(client.run("x", &Cancel::new()).await.unwrap_err(), Error::NotEnabled);
    }
}
