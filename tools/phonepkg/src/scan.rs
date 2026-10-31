//! TCP connect scan for the phone's wireless-debugging port, used by `rearm`
//! when mDNS finds nothing (mDNS does not cross Tailscale).

use crate::adb::check_interrupt;
use anyhow::Result;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Open ports of `ip` within `ports` (sorted), skipping `skip`. `workers` threads pull
/// ports off a shared counter; each connect waits at most `timeout`.
pub fn scan(
    ip: IpAddr,
    ports: RangeInclusive<u16>,
    skip: &[u16],
    workers: usize,
    timeout: Duration,
) -> Result<Vec<u16>> {
    let next = AtomicU32::new(u32::from(*ports.start()));
    let end = u32::from(*ports.end());
    let found = Mutex::new(Vec::new());
    thread::scope(|s| {
        for _ in 0..workers.max(1) {
            s.spawn(|| loop {
                if check_interrupt().is_err() {
                    break;
                }
                let p = next.fetch_add(1, Ordering::Relaxed);
                if p > end {
                    break;
                }
                let port = p as u16;
                if skip.contains(&port) {
                    continue;
                }
                if TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout).is_ok() {
                    found.lock().unwrap_or_else(|e| e.into_inner()).push(port);
                }
            });
        }
    });
    check_interrupt()?;
    let mut ports = found.into_inner().unwrap_or_else(|e| e.into_inner());
    ports.sort_unstable();
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn finds_open_and_honours_skip() {
        let a = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpListener::bind("127.0.0.1:0").unwrap();
        let pa = a.local_addr().unwrap().port();
        let pb = b.local_addr().unwrap().port();
        let (lo, hi) = (pa.min(pb), pa.max(pb));
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let t = Duration::from_millis(400);

        let open = scan(ip, lo..=hi, &[], 8, t).unwrap();
        assert!(open.contains(&pa) && open.contains(&pb), "{open:?}");
        assert!(open.windows(2).all(|w| w[0] < w[1]));

        assert_eq!(scan(ip, pa..=pa, &[pa], 4, t).unwrap(), Vec::<u16>::new());
    }
}
