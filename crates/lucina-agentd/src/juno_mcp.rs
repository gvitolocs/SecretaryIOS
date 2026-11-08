//! `lagent juno-mcp`: the Juno shell tool (`juno_bash`) for CodeCor's Claude agents,
//! over stdio. agentd attaches it (server name `juno`) to a Claude agent whose labels
//! carry `codecor.juno=bash`. By default (owner decision 2026-11-08) juno tools are
//! allowed: no card, preapproved like agentd's own tools. In ask mode
//! (`juno-decide.ask`, see `juno_ask_mode`) the mode must ask before each call, and
//! nothing here is ever preapproved — every call shows the owner an Allow/Deny card in
//! agentd's permission book and runs only after Allow (see `manager/permissions.rs`
//! and `providers/claude.rs`).
//!
//! The command runs on the Juno laptop as root over SSH, with the host key pinned
//! (`StrictHostKeyChecking=yes`, `HostKeyAlias=juno`, a dedicated key and known_hosts
//! under `~/.config/lucina-agentd/`). Commands that mention credentials are refused
//! before anything runs, secrets in the output are redacted, and every call is
//! appended to `~/.local/state/lucina-agentd/juno-bash.jsonl` (never the output).
//! When `~/.config/lucina-agentd/juno-decide` exists and is executable, each call also
//! goes through it (`lucina_decide`) before it runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// The server name agents see (`mcp__juno__<tool>`).
pub const SERVER: &str = "juno";
/// Same protocol version as `lagent mcp`.
const PROTOCOL: &str = "2025-06-18";
/// Same output cap as `lagent mcp` (`mcp.rs`'s `MAX_TEXT`).
const MAX_TEXT: usize = 12_000;
const MAX_COMMAND: usize = 8_000;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 600;
/// The `lucina_decide` hook gets a little more than the longest call.
const DECIDE_LIMIT_SECS: u64 = 330;
/// `LUCINA_JUNO_TARGET`, else the Juno laptop on the owner's LAN.
const TARGET_ENV: &str = "LUCINA_JUNO_TARGET";
const DEFAULT_TARGET: &str = "root@192.168.178.47";

/// Whether a permission request is for one of these tools. In ask mode (see
/// [`juno_ask_mode`]) they are never preapproved: every call asks the owner again.
pub fn is_juno_tool(name: &str) -> bool {
    name.starts_with("mcp__juno__")
}

/// Owner decision 2026-11-08: juno tools are allowed by default. Ask mode (the 2026-11-07
/// behaviour: a card on every call, never preapproved) is on when
/// `$HOME/.config/lucina-agentd/juno-decide.ask` exists or `JUNO_DECIDE_ASK=1`.
pub fn juno_ask_mode() -> bool {
    let file = home().join(".config/lucina-agentd/juno-decide.ask").exists();
    ask_mode_from(
        std::env::var_os("JUNO_DECIDE_ASK")
            .as_deref()
            .and_then(|s| s.to_str()),
        file,
    )
}

/// The pure switch behind [`juno_ask_mode`]: on iff the env var is exactly `1` or the
/// switch file exists.
fn ask_mode_from(env_value: Option<&str>, ask_file_exists: bool) -> bool {
    env_value == Some("1") || ask_file_exists
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_dir_at(home: &Path) -> PathBuf {
    home.join(".config/lucina-agentd")
}

fn key_path_at(home: &Path) -> PathBuf {
    config_dir_at(home).join("juno_bash_ed25519")
}

fn known_hosts_path_at(home: &Path) -> PathBuf {
    config_dir_at(home).join("juno_known_hosts")
}

fn decide_path_at(home: &Path) -> PathBuf {
    config_dir_at(home).join("juno-decide")
}

fn audit_path_at(home: &Path) -> PathBuf {
    home.join(".local/state/lucina-agentd/juno-bash.jsonl")
}

fn target() -> String {
    std::env::var(TARGET_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_TARGET.into())
}

/// The server as a Claude `mcpServers` entry (next to `lucina-agents`).
pub fn attach_with(lagent: &Path) -> Value {
    json!({"type": "stdio", "command": lagent.display().to_string(), "args": ["juno-mcp"]})
}

/// The server entry, when `lagent` sits next to this binary.
pub fn attach() -> Option<Value> {
    crate::mcp::lagent_path().as_deref().map(attach_with)
}

/// The exact ssh argument list. The command is one argv element after `--`, so no
/// `-o` value can be injected by it (ssh never parses it as an option).
pub fn ssh_argv(command: &str, key: &Path, known_hosts: &Path, target: &str) -> Vec<String> {
    vec![
        "-n".to_string(),
        "-i".to_string(),
        key.display().to_string(),
        "-o".to_string(),
        "IdentitiesOnly=yes".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=yes".to_string(),
        "-o".to_string(),
        format!("UserKnownHostsFile={}", known_hosts.display()),
        "-o".to_string(),
        "HostKeyAlias=juno".to_string(),
        "-o".to_string(),
        "ConnectTimeout=8".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=15".to_string(),
        target.to_string(),
        "--".to_string(),
        command.to_string(),
    ]
}

/// The key and known_hosts must be there, and the host key must be pinned under the
/// `juno` alias; otherwise refuse (never fall back to other options).
pub fn check_setup_at(home: &Path) -> Result<(PathBuf, PathBuf), String> {
    let key = key_path_at(home);
    let known_hosts = known_hosts_path_at(home);
    if !key.is_file() {
        return Err(format!(
            "missing SSH key {}; the Juno shell tool never falls back to other options",
            key.display()
        ));
    }
    let text = std::fs::read_to_string(&known_hosts)
        .map_err(|e| format!("cannot read {}: {e}", known_hosts.display()))?;
    if !text
        .lines()
        .any(|l| l.split_whitespace().next() == Some("juno"))
    {
        return Err(format!(
            "{} has no `juno` host-key line; pin the host key first",
            known_hosts.display()
        ));
    }
    Ok((key, known_hosts))
}

/// Substrings a command may never mention (credentials and secret stores on Juno).
const FORBIDDEN: &[&str] = &[
    "/data/lucinaos/coder/",
    "gateway_key",
    "tunnel_key",
    "mistral_key",
    "/data/lucinaos/account",
    "account.sock",
    "/data/lucina/secretary",
    "wifi.cfg",
    ".ssh/",
    "authorized_keys",
    "/etc/shadow",
    "/environ",
    "printenv",
    "secret",
];

/// `env ` at a word start (not inside `printenv` or `FOO_ENV`).
fn env_at_word_start(command: &str) -> bool {
    Regex::new(r"(?:^|[^A-Za-z0-9_])env\s")
        .unwrap()
        .is_match(command)
}

/// Refuse a command that mentions credentials: nothing runs.
pub fn check_command(command: &str) -> Result<(), String> {
    for needle in FORBIDDEN {
        if command.contains(needle) {
            return Err(format!("refused: the command mentions `{needle}`"));
        }
    }
    if env_at_word_start(command) {
        return Err("refused: the command runs `env`".into());
    }
    Ok(())
}

/// Redact secrets from output: `sec_…`, `sk-…` and private key blocks.
pub fn redact(text: &str) -> String {
    let sec = Regex::new(r"sec_[A-Za-z0-9_-]{16,}").unwrap();
    let sk = Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap();
    let key =
        Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*KEY-----").unwrap();
    let t = sec.replace_all(text, "[redacted]");
    let t = sk.replace_all(&t, "[redacted]");
    key.replace_all(&t, "[redacted]").into_owned()
}

/// Clip stdout+stderr to `MAX_TEXT` characters in total; `true` when something was cut.
fn clip_pair(stdout: String, stderr: String) -> (String, String, bool) {
    let so = stdout.chars().count();
    let se = stderr.chars().count();
    if so + se <= MAX_TEXT {
        return (stdout, stderr, false);
    }
    if so < MAX_TEXT {
        let budget = MAX_TEXT - so;
        (stdout, stderr.chars().take(budget).collect(), true)
    } else {
        (stdout.chars().take(MAX_TEXT).collect(), String::new(), true)
    }
}

/// What the `lucina_decide` hook said.
#[derive(Debug)]
enum Decide {
    /// No hook: the owner's permission card already gated the call.
    NotConfigured,
    Approved,
    Refused(String),
}

fn is_executable(m: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        false
    }
}

/// Run the `juno-decide` hook (when it exists and is executable) with the call's JSON
/// on stdin, under a 330 s limit. Only exit 0 with first stdout line `approve` lets
/// the call run.
async fn decide(
    home: &Path,
    command: &str,
    agent_id: Option<&str>,
    timeout_seconds: u64,
) -> Decide {
    let path = decide_path_at(home);
    match std::fs::metadata(&path) {
        Ok(m) if m.is_file() && is_executable(&m) => {}
        _ => return Decide::NotConfigured,
    }
    let input =
        json!({"command": command, "agent_id": agent_id, "timeout_seconds": timeout_seconds});
    let mut child = match Command::new(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Decide::Refused(format!("lucina_decide cannot run: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes()).await;
        let _ = stdin.flush().await;
    }
    let out = match tokio::time::timeout(
        Duration::from_secs(DECIDE_LIMIT_SECS),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Decide::Refused(format!("lucina_decide failed: {e}")),
        // The timed-out future (and with it the child) is dropped here; kill_on_drop
        // kills the hook.
        Err(_) => return Decide::Refused("lucina_decide timed out".into()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().unwrap_or("").trim();
    if out.status.success() && first == "approve" {
        Decide::Approved
    } else {
        let msg = if !first.is_empty() {
            first.to_string()
        } else {
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("refused by lucina_decide")
                .trim()
                .to_string()
        };
        Decide::Refused(msg)
    }
}

struct Outcome {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    duration_ms: u64,
}

/// Run ssh with `argv`; kill the child on timeout. stdout/stderr are read while the
/// child runs, so a full pipe can't stall it into the timeout.
async fn run_ssh(argv: &[String], timeout_seconds: u64) -> std::io::Result<Outcome> {
    use tokio::io::AsyncReadExt;
    let start = Instant::now();
    let mut child = Command::new("ssh")
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let read_out = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = out_pipe.read_to_end(&mut b).await;
        b
    });
    let read_err = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = err_pipe.read_to_end(&mut b).await;
        b
    });
    let (status, timed_out) =
        match tokio::time::timeout(Duration::from_secs(timeout_seconds), child.wait()).await {
            Ok(Ok(s)) => (Some(s), false),
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (None, true)
            }
        };
    let stdout = read_out.await.unwrap_or_default();
    let stderr = read_err.await.unwrap_or_default();
    Ok(Outcome {
        exit_code: status.and_then(|s| s.code()).unwrap_or(-1),
        stdout,
        stderr,
        timed_out,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// One audit line per call (never the output).
fn audit(home: &Path, line: &Value) {
    let path = audit_path_at(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Validate, gate and run one `juno_bash` call; returns the result JSON. Every call
/// (refused or run) is audited.
async fn run_call(command: &str, timeout_seconds: u64, home: &Path) -> Result<Value, String> {
    let agent_id = std::env::var("LUCINA_AGENT_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let audit = |decide: &str, exit_code: Value, timed_out: bool, duration_ms: u64| {
        audit(
            home,
            &json!({
                "ts": crate::now_iso(),
                "agent_id": agent_id,
                "command": command,
                "decide": decide,
                "exit_code": exit_code,
                "timed_out": timed_out,
                "duration_ms": duration_ms,
            }),
        );
    };
    if let Err(e) = check_command(command) {
        audit("not configured", Value::Null, false, 0);
        return Err(e);
    }
    let (key, known_hosts) = match check_setup_at(home) {
        Ok(v) => v,
        Err(e) => {
            audit("not configured", Value::Null, false, 0);
            return Err(e);
        }
    };
    let decide = decide(home, command, agent_id.as_deref(), timeout_seconds).await;
    let decide_str = match &decide {
        Decide::NotConfigured => "not configured".to_string(),
        Decide::Approved => "approve".to_string(),
        Decide::Refused(m) => format!("refused: {m}"),
    };
    if let Decide::Refused(m) = &decide {
        audit(&decide_str, Value::Null, false, 0);
        return Err(format!("lucina_decide refused this call: {m}"));
    }
    let argv = ssh_argv(command, &key, &known_hosts, &target());
    let out = run_ssh(&argv, timeout_seconds).await.map_err(|e| {
        audit(&decide_str, Value::Null, false, 0);
        format!("cannot run ssh: {e}")
    })?;
    let stdout = redact(&String::from_utf8_lossy(&out.stdout));
    let stderr = redact(&String::from_utf8_lossy(&out.stderr));
    let (stdout, stderr, clipped) = clip_pair(stdout, stderr);
    let stderr = if clipped {
        format!("{stderr}\n[output clipped to {MAX_TEXT} characters]")
    } else {
        stderr
    };
    audit(
        &decide_str,
        json!(out.exit_code),
        out.timed_out,
        out.duration_ms,
    );
    Ok(json!({
        "exit_code": out.exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "timed_out": out.timed_out,
        "duration_ms": out.duration_ms,
    }))
}

fn tools() -> Vec<Value> {
    vec![json!({
        "name": "juno_bash",
        "description": "Run a shell command on the Juno laptop as root over SSH. Every call asks the owner first (an Allow/Deny card); nothing is auto-allowed. Never pass credentials through it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run on Juno", "minLength": 1, "maxLength": MAX_COMMAND},
                "timeout_seconds": {"type": "integer", "description": "Seconds before the call is killed (default 60)", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "default": DEFAULT_TIMEOUT_SECS},
            },
            "required": ["command"],
            "additionalProperties": false,
        }
    })]
}

async fn call(args: &Value) -> Result<String, String> {
    let command = args["command"].as_str().ok_or("`command` is required")?;
    let n = command.chars().count();
    if !(1..=MAX_COMMAND).contains(&n) {
        return Err(format!("`command`: 1 to {MAX_COMMAND} characters"));
    }
    let timeout_seconds = match &args["timeout_seconds"] {
        Value::Null => DEFAULT_TIMEOUT_SECS,
        v => v
            .as_u64()
            .filter(|u| (1..=MAX_TIMEOUT_SECS).contains(u))
            .ok_or("`timeout_seconds`: an integer 1..600")?,
    };
    let result = run_call(command, timeout_seconds, &home()).await?;
    Ok(serde_json::to_string(&result).unwrap_or_else(|_| "{}".into()))
}

/// One JSON-RPC message in, at most one out.
pub async fn handle(msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
            "instructions": "The Juno laptop's shell (juno_bash). Every call shows the owner an Allow/Deny card and runs only after Allow; nothing is auto-allowed.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"]["arguments"].clone();
            let (text, is_error) = match if name == "juno_bash" {
                call(&args).await
            } else {
                Err(format!("unknown tool {name}"))
            } {
                Ok(t) => (t, false),
                Err(t) => (t, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// Serve stdio until EOF (the shared loop in `mcp.rs`).
pub async fn serve() -> std::io::Result<()> {
    crate::mcp::serve_stdio(|msg| handle(msg)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_mode_switch() {
        assert!(!ask_mode_from(None, false));
        assert!(ask_mode_from(Some("1"), false));
        assert!(!ask_mode_from(Some("0"), false));
        assert!(!ask_mode_from(Some("true"), false), "only exactly `1`");
        assert!(ask_mode_from(None, true), "the switch file alone turns ask mode on");
        assert!(ask_mode_from(Some("0"), true), "the switch file wins over `JUNO_DECIDE_ASK=0`");
    }

    #[test]
    fn juno_tool_names() {
        assert!(is_juno_tool("mcp__juno__juno_bash"));
        assert!(is_juno_tool("mcp__juno__"));
        assert!(!is_juno_tool("mcp__lucina-agents__create_agent"));
        assert!(!is_juno_tool("mcp__juno-evil__juno_bash"));
        assert!(!is_juno_tool("Bash"));
    }

    #[test]
    fn ssh_argv_is_exact_and_injection_proof() {
        let key = Path::new("/home/nez/.config/lucina-agentd/juno_bash_ed25519");
        let kh = Path::new("/home/nez/.config/lucina-agentd/juno_known_hosts");
        let argv = ssh_argv("ls -la", key, kh, "root@192.168.178.47");
        assert_eq!(
            argv,
            vec![
                "-n",
                "-i",
                "/home/nez/.config/lucina-agentd/juno_bash_ed25519",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "UserKnownHostsFile=/home/nez/.config/lucina-agentd/juno_known_hosts",
                "-o",
                "HostKeyAlias=juno",
                "-o",
                "ConnectTimeout=8",
                "-o",
                "ServerAliveInterval=15",
                "root@192.168.178.47",
                "--",
                "ls -la",
            ]
        );
        // Never `=no`, always `=yes`.
        for (i, a) in argv.iter().enumerate() {
            if a == "-o" {
                let v = &argv[i + 1];
                assert!(
                    v.contains("StrictHostKeyChecking=yes") || !v.contains("StrictHostKeyChecking"),
                    "{v}"
                );
                assert!(!v.contains("StrictHostKeyChecking=no"), "{v}");
            }
        }
        assert!(argv.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        assert!(!argv.iter().any(|a| a.contains("StrictHostKeyChecking=no")));
        // A hostile command cannot inject an `-o`: it stays one argv element after `--`.
        let evil = "-oProxyCommand=evil -oStrictHostKeyChecking=no; cat /etc/shadow";
        let argv = ssh_argv(evil, key, kh, "root@192.168.178.47");
        assert_eq!(argv.last().unwrap(), evil);
        assert_eq!(argv[argv.len() - 2], "--");
        let o_values: Vec<String> = argv
            .windows(2)
            .filter(|w| w[0] == "-o")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(
            o_values,
            vec![
                "IdentitiesOnly=yes".to_string(),
                "BatchMode=yes".to_string(),
                "StrictHostKeyChecking=yes".to_string(),
                format!("UserKnownHostsFile={}", kh.display()),
                "HostKeyAlias=juno".to_string(),
                "ConnectTimeout=8".to_string(),
                "ServerAliveInterval=15".to_string(),
            ]
        );
        // Only the fixed `-o` values sit before the `--`; the command is one element after it.
        let dashdash = argv.iter().rposition(|a| a == "--").unwrap();
        assert_eq!(dashdash, argv.len() - 2);
        assert_eq!(
            argv[..dashdash]
                .iter()
                .filter(|a| a.starts_with("-o"))
                .count(),
            7
        );
    }

    #[test]
    fn setup_must_be_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // No key, no known_hosts.
        assert!(check_setup_at(home)
            .unwrap_err()
            .contains("missing SSH key"));
        std::fs::create_dir_all(config_dir_at(home)).unwrap();
        std::fs::write(key_path_at(home), "key").unwrap();
        // known_hosts missing.
        assert!(check_setup_at(home).unwrap_err().contains("cannot read"));
        std::fs::write(known_hosts_path_at(home), "otherhost ssh-ed25519 AAAA\n").unwrap();
        assert!(check_setup_at(home)
            .unwrap_err()
            .contains("no `juno` host-key line"));
        std::fs::write(
            known_hosts_path_at(home),
            "# comment\njuno ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI juno\n",
        )
        .unwrap();
        let (k, kh) = check_setup_at(home).unwrap();
        assert!(k.is_file() && kh.is_file());
    }

    #[test]
    fn credentials_guard_refuses() {
        for cmd in [
            "cat /data/lucinaos/coder/config.toml",
            "cat gateway_key",
            "cat tunnel_key",
            "cat mistral_key",
            "ls /data/lucinaos/account",
            "cat account.sock",
            "ls /data/lucina/secretary",
            "cat wifi.cfg",
            "ls ~/.ssh/",
            "cat /root/.ssh/authorized_keys",
            "cat /etc/shadow",
            "cat /proc/1/environ",
            "printenv",
            "env | grep KEY",
            "  env FOO=1",
            "cat /x/secret",
            "echo my_secret",
        ] {
            assert!(check_command(cmd).is_err(), "{cmd:?} must be refused");
        }
        for cmd in [
            "ls -la",
            "echo FOO_ENV=1",
            "envsubcommand",
            "cat /etc/hosts",
            "cat /proc/1/cmdline",
            "echo environment",
        ] {
            assert!(check_command(cmd).is_ok(), "{cmd:?} must be allowed");
        }
        // `env` at a word start is refused, inside a word it is not.
        assert!(
            check_command("env").is_ok(),
            "bare `env` without a space is not `env `"
        );
        assert!(check_command("printenv").is_err());
        assert!(check_command("x=1 env FOO=bar").is_err());
        assert!(
            check_command("printenvx env").is_err(),
            "contains `printenv`"
        );
        assert!(
            check_command("echo env").is_ok(),
            "bare `env` at the end is not `env `"
        );
    }

    #[test]
    fn redaction() {
        let out = "token sec_AbCdEfGhIjKlMnOp1234 here\nsk-abcdefghijklmnopqrstuvwxyz1234\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\nok";
        let r = redact(out);
        assert_eq!(r, "token [redacted] here\n[redacted]\n[redacted]\nok");
        assert_eq!(redact("sec_short"), "sec_short", "too short to be a secret");
        assert_eq!(redact("sk-short"), "sk-short");
        assert_eq!(
            redact("-----BEGIN OPENSSH PRIVATE KEY-----\nx\n-----END OPENSSH PRIVATE KEY-----"),
            "[redacted]"
        );
    }

    #[test]
    fn clipping_is_total() {
        let s = "a".repeat(MAX_TEXT + 10);
        let (so, se, clipped) = clip_pair(s.clone(), "err".into());
        assert!(clipped);
        assert_eq!(so.chars().count(), MAX_TEXT);
        assert_eq!(se, "");
        let (so, se, clipped) = clip_pair("out".into(), "e".repeat(MAX_TEXT));
        assert!(clipped);
        assert_eq!(so, "out");
        assert_eq!(se.chars().count(), MAX_TEXT - 3);
        let (so, se, clipped) = clip_pair("out".into(), "err".into());
        assert!(!clipped);
        assert_eq!((so.as_str(), se.as_str()), ("out", "err"));
    }

    #[test]
    fn tool_schema_and_attach() {
        let t = &tools()[0];
        assert_eq!(t["name"], "juno_bash");
        assert_eq!(t["inputSchema"]["required"], json!(["command"]));
        assert_eq!(
            t["inputSchema"]["properties"]["command"]["maxLength"],
            json!(MAX_COMMAND)
        );
        assert_eq!(
            t["inputSchema"]["properties"]["timeout_seconds"]["maximum"],
            json!(MAX_TIMEOUT_SECS)
        );
        assert_eq!(
            t["inputSchema"]["properties"]["timeout_seconds"]["default"],
            json!(DEFAULT_TIMEOUT_SECS)
        );
        let a = attach_with(Path::new("/opt/bin/lagent"));
        assert_eq!(
            a,
            json!({"type": "stdio", "command": "/opt/bin/lagent", "args": ["juno-mcp"]})
        );
    }

    #[tokio::test]
    async fn decide_hook_gates_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(config_dir_at(home)).unwrap();
        // No hook: not configured.
        assert!(matches!(
            decide(home, "ls", Some("agent-1"), 60).await,
            Decide::NotConfigured
        ));
        // A hook that approves.
        let hook = decide_path_at(home);
        std::fs::write(&hook, "#!/bin/sh\ncat >/dev/null\necho approve\n").unwrap();
        make_executable(&hook);
        assert!(matches!(
            decide(home, "ls", Some("agent-1"), 60).await,
            Decide::Approved
        ));
        // A hook that refuses with a message.
        std::fs::write(
            &hook,
            "#!/bin/sh\ncat >/dev/null\necho 'no, not that one'\nexit 1\n",
        )
        .unwrap();
        make_executable(&hook);
        match decide(home, "ls", None, 60).await {
            Decide::Refused(m) => assert_eq!(m, "no, not that one"),
            other => panic!("expected refusal, got {other:?}"),
        }
        // A non-executable hook is not configured.
        std::fs::write(&hook, "#!/bin/sh\necho approve\n").unwrap();
        match std::fs::metadata(&hook) {
            Ok(m) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &hook,
                        std::fs::Permissions::from_mode(m.permissions().mode() & !0o111),
                    )
                    .unwrap();
                }
            }
            Err(_) => {}
        }
        assert!(matches!(
            decide(home, "ls", None, 60).await,
            Decide::NotConfigured
        ));
    }

    #[cfg(unix)]
    fn make_executable(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_p: &Path) {}

    #[tokio::test]
    async fn run_call_refuses_before_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(config_dir_at(home)).unwrap();
        std::fs::write(key_path_at(home), "key").unwrap();
        std::fs::write(known_hosts_path_at(home), "juno ssh-ed25519 AAAA\n").unwrap();
        // Credentials: refused, nothing runs, but audited.
        let err = run_call("cat /etc/shadow", 60, home).await.unwrap_err();
        assert!(err.contains("refused"), "{err}");
        // Missing key: refused.
        let dir2 = tempfile::tempdir().unwrap();
        let err = run_call("ls", 60, dir2.path()).await.unwrap_err();
        assert!(err.contains("missing SSH key"), "{err}");
        // Audit line written for the refused call (the command, never an output).
        let audit = std::fs::read_to_string(audit_path_at(home)).unwrap();
        let line: Value = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
        assert_eq!(line["command"], "cat /etc/shadow");
        assert_eq!(line["decide"], "not configured");
        assert!(line["exit_code"].is_null());
        assert!(line.get("stdout").is_none() && line.get("stderr").is_none());
    }
}
