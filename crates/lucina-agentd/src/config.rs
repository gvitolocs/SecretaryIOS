//! Where the daemon keeps its state and listens.

use std::path::PathBuf;

/// `LUCINA_AGENTD_SOCKET`, honoured by the daemon and `lagent`.
pub const SOCKET_ENV: &str = "LUCINA_AGENTD_SOCKET";
/// `LUCINA_AGENTD_DEV=1` registers the mock providers (Paseo: `PASEO_NODE_ENV=development`).
pub const DEV_ENV: &str = "LUCINA_AGENTD_DEV";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$LUCINA_AGENTD_SOCKET`, else `$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`, else
/// `~/.local/state/lucina-agentd/agentd.sock`.
pub fn default_socket() -> PathBuf {
    if let Some(p) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(p);
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("lucina-agentd").join("agentd.sock"),
        None => home().join(".local/state/lucina-agentd/agentd.sock"),
    }
}

/// `~/.local/share/lucina-agentd`.
pub fn default_state_dir() -> PathBuf {
    home().join(".local/share/lucina-agentd")
}

pub fn dev_enabled() -> bool {
    std::env::var(DEV_ENV).map(|v| v == "1" || v == "true").unwrap_or(false)
}

/// Replace a leading `$HOME` with `~` (Paseo `shortenPath`).
pub fn shorten_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}
