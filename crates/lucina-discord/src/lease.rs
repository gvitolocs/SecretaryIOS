//! The message session's lease: an exclusive, non-blocking `flock` on
//! `discord-<application id>.flock` in the gateway lock directory
//! (`HERMES_GATEWAY_LOCK_DIR`, default `~/.local/state/hermes-gateway-locks`).
//!
//! The session never connects without it and keeps it while it is connected;
//! closing the file (drop, or the process dying) releases it. The Node
//! Lucina takes no Discord lock, so the lease only keeps a second Rust
//! instance off the token: the cutover is stop Node, then start Rust. The
//! presence watcher (`lucina presence-watch`) keeps its own read-only gateway
//! session and does not take this lease.
//!
//! The file also carries a one-line holder note (instance, pid, host, start
//! time) for diagnostics; the flock alone decides who holds the lease.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

/// `HERMES_GATEWAY_LOCK_DIR`, else `~/.local/state/hermes-gateway-locks`.
pub fn default_lock_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("HERMES_GATEWAY_LOCK_DIR").filter(|s| !s.is_empty()) {
        return Ok(dir.into());
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".local/state/hermes-gateway-locks"))
        .ok_or_else(|| io::Error::other("gateway lock directory unavailable: HOME is unset"))
}

/// A bot token's application (bot user) id: its first segment is the id in
/// base64 (Node `Buffer.from(token.split('.')[0], 'base64url')`).
pub fn application_id_from_token(token: &str) -> Option<u64> {
    let t = token.trim();
    let head = t.strip_prefix("Bot ").unwrap_or(t).split('.').next()?;
    let bytes = base64_decode(head)?;
    std::str::from_utf8(&bytes)
        .ok()?
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
}

/// Standard or URL-safe base64, padding optional.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    };
    let data = s.trim_end_matches('=').as_bytes();
    if data.is_empty() || data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= val(*c)? << (18 - 6 * i);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

pub fn lock_path(dir: &Path, application_id: u64) -> PathBuf {
    dir.join(format!("discord-{application_id}.flock"))
}

/// The holder note of the current (or last) holder, for reporting a lease
/// held elsewhere. Never contains the token.
pub fn read_holder(dir: &Path, application_id: u64) -> Option<String> {
    let text = std::fs::read_to_string(lock_path(dir, application_id)).ok()?;
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let instance = v["instance"].as_str().unwrap_or("unknown");
    let pid = v["pid"]
        .as_u64()
        .map(|p| format!(" pid {p}"))
        .unwrap_or_default();
    let host = v["hostname"].as_str().unwrap_or("");
    Some(format!("{instance}{pid} on {host}").trim().to_owned())
}

/// The held lease. Dropping it releases the flock.
#[derive(Debug)]
pub struct DiscordLease {
    _file: File,
    path: PathBuf,
}

impl DiscordLease {
    /// `Ok(None)`: another session holds the lease.
    pub fn try_acquire(
        dir: &Path,
        application_id: u64,
        instance: &str,
    ) -> io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(dir).map_err(|e| {
            io::Error::new(e.kind(), format!("gateway lock directory unusable: {e}"))
        })?;
        let path = lock_path(dir, application_id);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(&path)
            .map_err(|e| io::Error::new(e.kind(), format!("discord flock unusable: {e}")))?;
        match file.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("discord flock failed: {e}"),
                ))
            }
        }
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let note = serde_json::json!({
            "instance": instance,
            "pid": std::process::id(),
            "hostname": hostname,
            "startedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
        // Best effort: the note is diagnostics only.
        let _ = file
            .set_len(0)
            .and_then(|_| file.rewind())
            .and_then(|_| writeln!(file, "{note}"));
        Ok(Some(Self { _file: file, path }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_id_from_a_token_head() {
        // "123456789012345678" in base64, without and with padding.
        assert_eq!(
            application_id_from_token("MTIzNDU2Nzg5MDEyMzQ1Njc4.GxYz.abc"),
            Some(123_456_789_012_345_678)
        );
        assert_eq!(application_id_from_token("MTIz.x.y"), Some(123));
        assert_eq!(application_id_from_token("Bot MTIz.x.y"), Some(123));
        assert_eq!(application_id_from_token("MTI=.x.y"), Some(12));
        assert_eq!(application_id_from_token("not base64!.x"), None);
        assert_eq!(application_id_from_token(""), None);
        // "abc" decodes but is not a number.
        assert_eq!(application_id_from_token("YWJj.x.y"), None);
    }

    #[test]
    fn a_second_session_cannot_acquire_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let first = DiscordLease::try_acquire(dir.path(), 42, "lucina-rs")
            .unwrap()
            .expect("first acquire");
        assert!(first.path().ends_with("discord-42.flock"));
        assert!(DiscordLease::try_acquire(dir.path(), 42, "second")
            .unwrap()
            .is_none());
        // Another application is another lease.
        let other = DiscordLease::try_acquire(dir.path(), 43, "lucina-rs").unwrap();
        assert!(other.is_some());
        let holder = read_holder(dir.path(), 42).unwrap();
        assert!(holder.starts_with("lucina-rs pid "), "{holder}");
        drop(first);
        assert!(DiscordLease::try_acquire(dir.path(), 42, "second")
            .unwrap()
            .is_some());
    }
}
