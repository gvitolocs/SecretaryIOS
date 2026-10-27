//! Who is calling the daemon, for permission answers and external requests.
//!
//! Every connection records the peer's pid (`SO_PEERCRED` on the unix socket). A
//! caller is an [`Origin::Agent`] when it descends from a provider process agentd
//! launched: one of its ancestors, its process group or its session is a provider's
//! pid (providers run as their own group leaders). An agent can't answer its own
//! permission request with `lagent permit allow`, whatever header it sends.
//!
//! This is best-effort while agents run as the owner's uid: a process that
//! double-forks out of its session escapes the check. The fix is the separate agents
//! uid (design, Security requirement 6), which can't open the 0600 socket at all.
//!
//! Otherwise the `X-Lucina-Origin` header names the origin: `device:<id>` (set by
//! secretary-server for the owner's devices), `mcp[:<token id>]`, or nothing / `local`
//! for `lagent` on the host. Loopback TCP callers are [`Origin::Unknown`]: they can
//! read, never answer.

use std::collections::HashSet;

use axum::extract::connect_info::Connected;
use axum::http::HeaderMap;
use axum::serve::IncomingStream;
use lucina_agents::permission_gate::Origin;
use tokio::net::{TcpListener, UnixListener};

pub const ORIGIN_HEADER: &str = "x-lucina-origin";

#[derive(Debug, Clone, Copy, Default)]
pub struct Peer {
    pub pid: Option<i32>,
    pub unix: bool,
}

impl Connected<IncomingStream<'_, UnixListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        let pid = stream.io().peer_cred().ok().and_then(|c| c.pid()).filter(|p| *p > 0);
        Peer { pid, unix: true }
    }
}

impl Connected<IncomingStream<'_, TcpListener>> for Peer {
    fn connect_info(_: IncomingStream<'_, TcpListener>) -> Self {
        Peer { pid: None, unix: false }
    }
}

/// `(ppid, pgrp, session)` from `/proc/<pid>/stat`.
fn stat_ids(pid: i32) -> Option<(i32, i32, i32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut f = stat[stat.rfind(')')? + 2..].split(' ');
    let _state = f.next()?;
    let ppid = f.next()?.parse().ok()?;
    let pgrp = f.next()?.parse().ok()?;
    let session = f.next()?.parse().ok()?;
    Some((ppid, pgrp, session))
}

/// Whether `pid` runs under one of `providers` (ancestor, process group or session).
pub fn descends_from(pid: i32, providers: &HashSet<u32>) -> bool {
    if providers.is_empty() {
        return false;
    }
    let hit = |p: i32| p > 0 && providers.contains(&(p as u32));
    let mut current = pid;
    for _ in 0..64 {
        if hit(current) {
            return true;
        }
        let Some((ppid, pgrp, session)) = stat_ids(current) else { return false };
        if hit(pgrp) || hit(session) {
            return true;
        }
        if ppid <= 1 {
            return false;
        }
        current = ppid;
    }
    false
}

/// The origin of a request (see the module docs).
pub fn origin(peer: &Peer, headers: &HeaderMap, providers: &HashSet<u32>) -> Origin {
    if !peer.unix {
        return Origin::Unknown;
    }
    let Some(pid) = peer.pid else { return Origin::Unknown };
    if descends_from(pid, providers) {
        return Origin::Agent;
    }
    let header = headers.get(ORIGIN_HEADER).map(|v| v.to_str().unwrap_or("?"));
    match header {
        None | Some("local") => Origin::Local,
        Some("mcp") => Origin::Mcp,
        Some(h) if h.starts_with("mcp:") => Origin::Mcp,
        Some(h) if h.strip_prefix("device:").is_some_and(|d| !d.is_empty()) => Origin::OwnerDevice,
        Some(_) => Origin::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        let me = Peer { pid: Some(std::process::id() as i32), unix: true };
        let none = HashSet::new();
        let mut h = HeaderMap::new();
        assert_eq!(origin(&me, &h, &none), Origin::Local);
        h.insert(ORIGIN_HEADER, "mcp:hermes".parse().unwrap());
        assert_eq!(origin(&me, &h, &none), Origin::Mcp);
        h.insert(ORIGIN_HEADER, "device:dev_1".parse().unwrap());
        assert_eq!(origin(&me, &h, &none), Origin::OwnerDevice);
        h.insert(ORIGIN_HEADER, "device:".parse().unwrap());
        assert_eq!(origin(&me, &h, &none), Origin::Unknown);
        assert_eq!(origin(&Peer { pid: None, unix: false }, &HeaderMap::new(), &none), Origin::Unknown);
        // This test process runs under its own parent: as a "provider", it is an agent.
        let parent = stat_ids(std::process::id() as i32).unwrap().0 as u32;
        let providers: HashSet<u32> = [parent].into();
        assert_eq!(origin(&me, &h, &providers), Origin::Agent);
    }
}
