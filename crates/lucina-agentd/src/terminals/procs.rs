//! Processes and their listening ports, from `/proc` (Linux only).
//!
//! - [`Table`]: every process's `(pid, ppid, pgrp, session)`, read once per scan.
//! - [`listening`]: the TCP ports a set of processes listens on (`/proc/net/tcp{,6}`
//!   in LISTEN state, matched to the processes' socket inodes).
//! - [`signal_session`]: signal every process of a terminal's session, also those
//!   that shell job control moved into their own process groups.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgrp: u32,
    pub session: u32,
}

/// `(ppid, pgrp, session)` of every process.
pub struct Table(pub Vec<Proc>);

fn stat(pid: u32) -> Option<Proc> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut f = s[s.rfind(')')? + 2..].split(' ');
    let _state = f.next()?;
    Some(Proc { pid, ppid: f.next()?.parse().ok()?, pgrp: f.next()?.parse().ok()?, session: f.next()?.parse().ok()? })
}

impl Table {
    pub fn read() -> Table {
        let Ok(dir) = std::fs::read_dir("/proc") else { return Table(Vec::new()) };
        Table(dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).filter_map(stat).collect())
    }

    /// The processes of `session`, plus the descendants of `roots` (for an agent:
    /// its provider's group and session, and what it started).
    pub fn members(&self, sessions: &HashSet<u32>, roots: &HashSet<u32>) -> HashSet<u32> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in &self.0 {
            children.entry(p.ppid).or_default().push(p.pid);
        }
        let mut out: HashSet<u32> = self
            .0
            .iter()
            .filter(|p| sessions.contains(&p.session) || roots.contains(&p.pgrp) || roots.contains(&p.session))
            .map(|p| p.pid)
            .collect();
        out.extend(roots.iter().copied().filter(|r| self.0.iter().any(|p| p.pid == *r)));
        let mut stack: Vec<u32> = out.iter().copied().collect();
        while let Some(pid) = stack.pop() {
            for c in children.get(&pid).into_iter().flatten() {
                if out.insert(*c) {
                    stack.push(*c);
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Listen {
    pub port: u16,
    pub host: String,
    pub pid: u32,
}

/// `inode -> (host, port)` of the sockets in LISTEN state.
fn listen_inodes() -> HashMap<u64, (String, u16)> {
    let mut out = HashMap::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                continue;
            }
            let Some((addr, port)) = f[1].split_once(':') else { continue };
            let (Ok(port), Ok(inode)) = (u16::from_str_radix(port, 16), f[9].parse::<u64>()) else { continue };
            out.insert(inode, (host(addr, v6), port));
        }
    }
    out
}

/// A `/proc/net/tcp` address (little-endian words) as text.
fn host(hex: &str, v6: bool) -> String {
    let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    let word = |w: &[u8]| [w[3], w[2], w[1], w[0]];
    if !v6 && bytes.len() == 4 {
        return std::net::Ipv4Addr::from(word(&bytes)).to_string();
    }
    if bytes.len() == 16 {
        let mut b = [0u8; 16];
        for (i, chunk) in bytes.chunks(4).enumerate() {
            b[4 * i..4 * i + 4].copy_from_slice(&word(chunk));
        }
        let ip = std::net::Ipv6Addr::from(b);
        return ip.to_ipv4_mapped().map(|v4| v4.to_string()).unwrap_or_else(|| ip.to_string());
    }
    hex.to_string()
}

/// The TCP ports `pids` listen on (one entry per port, the lowest pid).
pub fn listening(pids: &HashSet<u32>) -> Vec<Listen> {
    if pids.is_empty() {
        return Vec::new();
    }
    let inodes = listen_inodes();
    let mut found: BTreeSet<Listen> = BTreeSet::new();
    for pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let t = target.to_string_lossy();
            let Some(inode) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')).and_then(|n| n.parse::<u64>().ok()) else { continue };
            if let Some((host, port)) = inodes.get(&inode) {
                found.insert(Listen { port: *port, host: host.clone(), pid: *pid });
            }
        }
    }
    let mut out: Vec<Listen> = Vec::new();
    for l in found {
        if !out.iter().any(|o| o.port == l.port) {
            out.push(l);
        }
    }
    out
}

/// Send `signal` to every process of `session` (and to the session leader's group).
/// Returns how many processes were signalled.
pub fn signal_session(session: u32, signal: &str) -> usize {
    let pids: Vec<String> = Table::read().0.iter().filter(|p| p.session == session).map(|p| p.pid.to_string()).collect();
    let _ = std::process::Command::new("kill")
        .args(["-s", signal, "--", &format!("-{session}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !pids.is_empty() {
        let _ = std::process::Command::new("kill")
            .args(["-s", signal, "--"])
            .args(&pids)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    pids.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_listening_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let me: HashSet<u32> = [std::process::id()].into();
        let found = listening(&me);
        assert!(found.iter().any(|x| x.port == port && x.host == "127.0.0.1"), "{found:?}");
        assert_eq!(host("0100007F", false), "127.0.0.1");
        let t = Table::read();
        let mine = stat(std::process::id()).unwrap();
        let members = t.members(&[mine.session].into(), &HashSet::new());
        assert!(members.contains(&std::process::id()));
    }
}
