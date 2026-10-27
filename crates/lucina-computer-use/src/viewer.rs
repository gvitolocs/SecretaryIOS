//! Live-view tickets for the apps: Cua's own web viewer (`/viewer/` on each
//! sandbox's cua-spacesd, H.264 or JPEG over a WebSocket plus Connect RPCs),
//! reached only through lucina-server's view proxy.
//!
//! A ticket comes from the sandbox's `SystemService/CreateViewerTicket`. That
//! RPC speaks binary protobuf, so it goes through the installed Cua CLI
//! (`cua spacesd call`, proto3 JSON in and out), started with a fixed argv,
//! no shell, and an environment holding only the sandbox's own token. The
//! sandbox URL is resolved on every call: Docker gives `lucina-workspace` a new
//! loopback port on each restart (`sync-sandbox-ports.sh` keeps the registry
//! in step), and only `127.0.0.1` URLs are accepted.
//!
//! Tickets are secrets: never log one.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

use crate::control::SandboxId;
use crate::Error;

/// The Lucina Cua profile (registry and credentials).
pub const DEFAULT_CUA_HOME: &str = "/home/nez/Projects/Lucina/data/cua-home";
/// The Cua CLI.
pub const DEFAULT_CUA_BIN: &str = "/home/nez/.local/bin/cua";
/// How long a view lasts before the app asks for a new one.
pub const VIEW_TTL: Duration = Duration::from_secs(600);
const CLI_TIMEOUT: Duration = Duration::from_secs(15);

/// What a view may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Watch only (`SESSION_POLICY_VIEW_ONLY`).
    Watch,
    /// Mouse, keyboard and text clipboard (`SESSION_POLICY_ALLOW_ACTIVATION`).
    Control,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Control => "control",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "watch" | "view" => Some(Self::Watch),
            "control" => Some(Self::Control),
            _ => None,
        }
    }
}

/// A minted viewer ticket.
#[derive(Clone)]
pub struct ViewTicket {
    /// The bearer the viewer sends (`#ticket=` in its URL). Secret.
    pub ticket: String,
    /// RFC 3339, from the sandbox.
    pub expires_at: String,
    /// `http://127.0.0.1:<port>` of the sandbox's cua-spacesd.
    pub machine: String,
}

impl std::fmt::Debug for ViewTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewTicket")
            .field("ticket", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("machine", &self.machine)
            .finish()
    }
}

/// Where the Cua profile and CLI live (`LUCINA_CUA_HOME`, `LUCINA_CUA_BIN`).
#[derive(Debug, Clone)]
pub struct ViewerConfig {
    pub cua_home: PathBuf,
    pub cua_bin: PathBuf,
}

impl ViewerConfig {
    pub fn from_env() -> Self {
        let text = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Self {
            cua_home: PathBuf::from(text("LUCINA_CUA_HOME").unwrap_or_else(|| DEFAULT_CUA_HOME.into())),
            cua_bin: PathBuf::from(text("LUCINA_CUA_BIN").unwrap_or_else(|| DEFAULT_CUA_BIN.into())),
        }
    }
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Viewer(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| Error::Viewer(format!("{}: {e}", path.display())))
}

/// `http://127.0.0.1:<port>` only.
fn loopback_url(port: u64) -> Result<String, Error> {
    if port == 0 || port > 65535 {
        return Err(Error::Viewer(format!("bad sandbox port {port}")));
    }
    Ok(format!("http://127.0.0.1:{port}"))
}

/// The sandbox's cua-spacesd URL from the Lucina Cua registry.
pub fn machine_url(cfg: &ViewerConfig, sb: SandboxId) -> Result<String, Error> {
    match sb {
        SandboxId::Workspace => {
            let v = read_json(&cfg.cua_home.join("sandboxes/lucina-workspace.json"))?;
            let port = v
                .pointer("/exposed_ports/3211")
                .and_then(Value::as_u64)
                .or_else(|| v.get("api_port").and_then(Value::as_u64))
                .ok_or_else(|| Error::Viewer("lucina-workspace has no api port".into()))?;
            loopback_url(port)
        }
        SandboxId::Android => {
            // `direct:127.0.0.1:<port>`: the id is the address.
            let port = sb
                .space()
                .strip_prefix("direct:127.0.0.1:")
                .and_then(|p| p.parse::<u64>().ok())
                .ok_or_else(|| Error::Viewer("android space is not a loopback address".into()))?;
            loopback_url(port)
        }
    }
}

fn token(cfg: &ViewerConfig, sb: SandboxId) -> Result<String, Error> {
    let v = read_json(&cfg.cua_home.join("spaces-credentials.json"))?;
    v.get(sb.space())
        .and_then(|c| c.get("token"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::Viewer(format!("no Cua credential for {}", sb.space())))
}

/// The `CreateViewerTicket` request body.
pub fn ticket_request(mode: ViewMode, ttl: Duration) -> Value {
    json!({
        "ttl": format!("{}s", ttl.as_secs()),
        "policy": match mode {
            ViewMode::Watch => "SESSION_POLICY_VIEW_ONLY",
            ViewMode::Control => "SESSION_POLICY_ALLOW_ACTIVATION",
        },
        "clipboard": mode == ViewMode::Control,
        "filesRoot": "",
        "audioUplink": false,
    })
}

/// Parses the CLI's proto3 JSON answer.
pub fn parse_ticket(stdout: &str, machine: String) -> Result<ViewTicket, Error> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| Error::Viewer("the sandbox returned no ticket".into()))?;
    let ticket = v
        .get("ticket")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && t.len() < 4096)
        .ok_or_else(|| Error::Viewer("the sandbox returned no ticket".into()))?;
    Ok(ViewTicket {
        ticket: ticket.to_owned(),
        expires_at: v
            .get("expiresAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        machine,
    })
}

/// Mints a viewer ticket on the sandbox.
pub async fn mint(cfg: &ViewerConfig, sb: SandboxId, mode: ViewMode, ttl: Duration) -> Result<ViewTicket, Error> {
    let machine = machine_url(cfg, sb)?;
    let token = token(cfg, sb)?;
    let body = ticket_request(mode, ttl).to_string();
    let mut cmd = Command::new(&cfg.cua_bin);
    cmd.args(["spacesd", "call", &machine, "SystemService/CreateViewerTicket", &body])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into()))
        .env("CUA_HOME", &cfg.cua_home)
        .env("CUA_ENV_TOKEN", token)
        .env("DO_NOT_TRACK", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(CLI_TIMEOUT, cmd.output())
        .await
        .map_err(|_| Error::Viewer("ticket request timed out".into()))?
        .map_err(|e| Error::Viewer(format!("cua CLI: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("").chars().take(200).collect::<String>();
        return Err(Error::Viewer(format!("ticket refused: {first}")));
    }
    parse_ticket(&String::from_utf8_lossy(&out.stdout), machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_map_to_cua_policies() {
        let w = ticket_request(ViewMode::Watch, Duration::from_secs(600));
        assert_eq!(w["policy"], "SESSION_POLICY_VIEW_ONLY");
        assert_eq!(w["clipboard"], false);
        assert_eq!(w["ttl"], "600s");
        let c = ticket_request(ViewMode::Control, Duration::from_secs(60));
        assert_eq!(c["policy"], "SESSION_POLICY_ALLOW_ACTIVATION");
        assert_eq!(c["clipboard"], true);
        assert_eq!(c["filesRoot"], "");
        assert_eq!(c["audioUplink"], false);
    }

    #[test]
    fn machine_urls_come_from_the_registry_and_stay_on_loopback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sandboxes")).unwrap();
        std::fs::write(
            dir.path().join("sandboxes/lucina-workspace.json"),
            r#"{"name":"lucina-workspace","api_port":32771,"exposed_ports":{"3211":32999}}"#,
        )
        .unwrap();
        let cfg = ViewerConfig { cua_home: dir.path().into(), cua_bin: "/bin/false".into() };
        assert_eq!(machine_url(&cfg, SandboxId::Workspace).unwrap(), "http://127.0.0.1:32999");
        assert_eq!(machine_url(&cfg, SandboxId::Android).unwrap(), "http://127.0.0.1:32792");
        std::fs::write(dir.path().join("sandboxes/lucina-workspace.json"), r#"{"api_port":0}"#).unwrap();
        assert!(machine_url(&cfg, SandboxId::Workspace).is_err());
    }

    #[test]
    fn ticket_parsing_and_redaction() {
        let t = parse_ticket(
            r#"{"ticket":"v1.secret","expiresAt":"2026-10-27T17:00:00Z","viewerPath":"/viewer/#ticket=v1.secret"}"#,
            "http://127.0.0.1:1".into(),
        )
        .unwrap();
        assert_eq!(t.ticket, "v1.secret");
        assert!(!format!("{t:?}").contains("secret"));
        assert!(parse_ticket("{}", String::new()).is_err());
        assert!(parse_ticket("not json", String::new()).is_err());
    }

    #[tokio::test]
    async fn a_failing_cli_is_an_error_without_the_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("spaces-credentials.json"),
            r#"{"direct:127.0.0.1:32792":{"token":"tok-secret"}}"#,
        )
        .unwrap();
        let cfg = ViewerConfig { cua_home: dir.path().into(), cua_bin: "/bin/false".into() };
        let e = mint(&cfg, SandboxId::Android, ViewMode::Watch, VIEW_TTL).await.unwrap_err();
        assert!(matches!(e, Error::Viewer(_)));
        assert!(!e.to_string().contains("tok-secret"));
    }
}
