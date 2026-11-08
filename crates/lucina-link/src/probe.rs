//! Probing: is a route usable right now, and how fast? Plain threads and std sockets, no runtime.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{Ipv4Cidr, Route, Service, Target};
use crate::policy::{self, Candidate, Health, RouteKind, Traffic};
use crate::secrets::AccessToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Outcome {
    Up {
        rtt_ms: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Down {
        reason: String,
    },
    /// Not finished when a better route had already won.
    Unknown {
        reason: String,
    },
    /// The policy does not allow this route for this traffic, so it was not probed.
    Skipped {
        reason: String,
    },
}

impl Outcome {
    fn down(reason: impl Into<String>) -> Self {
        Outcome::Down {
            reason: reason.into(),
        }
    }
    fn up(start: Instant, note: Option<&str>) -> Self {
        Outcome::Up {
            rtt_ms: start.elapsed().as_millis().min(u32::MAX as u128) as u32,
            note: note.map(str::to_string),
        }
    }
    pub fn health(&self) -> Health {
        match self {
            Outcome::Up { rtt_ms, .. } => Health::Up { rtt_ms: *rtt_ms },
            Outcome::Unknown { .. } => Health::Unknown,
            Outcome::Down { .. } | Outcome::Skipped { .. } => Health::Down,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteReport {
    pub id: String,
    pub kind: RouteKind,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceReport {
    pub service: String,
    pub traffic: Traffic,
    pub routes: Vec<RouteReport>,
    /// The id of the chosen route.
    pub chosen: Option<String>,
}

/// The local address the OS would send from to reach `target`. Connecting a UDP socket sends no packet.
pub fn local_source_for(target: SocketAddr) -> Option<IpAddr> {
    let bind = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(target).ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// The machine's default source address (changes when it moves between networks).
pub fn default_source() -> Option<IpAddr> {
    local_source_for(SocketAddr::from(([1, 1, 1, 1], 53)))
}

fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("dns: {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("dns: {host}: no addresses"));
    }
    Ok(addrs)
}

fn connect_any(addrs: &[SocketAddr], deadline: Instant) -> Result<TcpStream, String> {
    let mut last = String::from("timeout");
    for a in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timeout".into());
        }
        match TcpStream::connect_timeout(a, left) {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => last = "timeout".into(),
            Err(e) => last = format!("{a}: {e}"),
        }
    }
    Err(last)
}

/// Down when the OS would not send to this LAN target from an address inside `cidr`.
fn lan_check(addrs: &[SocketAddr], cidr: &Ipv4Cidr) -> Result<(), String> {
    let target = addrs.iter().find(|a| a.is_ipv4()).unwrap_or(&addrs[0]);
    match local_source_for(*target) {
        Some(src) if cidr.contains(src) => Ok(()),
        Some(src) => Err(format!("not on the home LAN (source {src})")),
        None => Err("not on the home LAN (no route)".into()),
    }
}

fn probe_tcp(host: &str, port: u16, cidr: Option<&Ipv4Cidr>, timeout: Duration) -> Outcome {
    let start = Instant::now();
    let addrs = match resolve(host, port) {
        Ok(a) => a,
        Err(e) => return Outcome::down(e),
    };
    if let Some(c) = cidr {
        if let Err(e) = lan_check(&addrs, c) {
            return Outcome::down(e);
        }
    }
    match connect_any(&addrs, start + timeout) {
        Ok(_) => Outcome::up(start, None),
        Err(e) => Outcome::down(e),
    }
}

struct Url<'a> {
    https: bool,
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn split_url(url: &str) -> Option<Url<'_>> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, if https { 443 } else { 80 }),
    };
    Some(Url {
        https,
        host,
        port,
        path,
    })
}

/// Any answer below 500 means the service is reachable (401 without credentials included).
pub fn classify_http(status: u16) -> Result<(), String> {
    if status < 500 {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

/// A Cloudflare Access hostname probed with the service token. Learned against the live tunnel (2026-10-25): a
/// valid token gets 200 when cloudflared reaches the origin and 502 when the tunnel is up but the origin is not; a
/// missing or wrong token gets 401 (or a redirect to the Access login); 530 (error 1033) means no connector.
pub fn classify_access(status: u16, location: Option<&str>) -> Result<(), String> {
    match status {
        300..=399 if location.is_some_and(|l| l.contains("cloudflareaccess.com")) => {
            Err("access denied: check the service token (login redirect)".into())
        }
        401 | 403 => Err("access denied: check the service token".into()),
        502 | 504 => Err("tunnel up, origin unreachable".into()),
        530 => Err("tunnel down (no connector)".into()),
        s if s >= 500 => Err(format!("HTTP {s}")),
        _ => Ok(()),
    }
}

/// A plain HTTP/1.1 GET; returns the status code.
fn http_get_plain(host: &str, port: u16, path: &str, deadline: Instant) -> Result<u16, String> {
    let addrs = resolve(host, port)?;
    let mut s = connect_any(&addrs, deadline)?;
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1));
    s.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(left)).map_err(|e| e.to_string())?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: lucina-link\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < 1024 {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("timeout".into())
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = String::from_utf8_lossy(&buf);
    let mut parts = line.split_whitespace();
    match (
        parts.next(),
        parts.next().and_then(|c| c.parse::<u16>().ok()),
    ) {
        (Some(v), Some(code)) if v.starts_with("HTTP/") => Ok(code),
        _ => Err("not an HTTP answer".into()),
    }
}

/// GET without following redirects; returns the status and the Location header.
#[cfg(feature = "https")]
fn https_get(
    url: &str,
    headers: &[(&str, &str)],
    timeout: Duration,
) -> Result<(u16, Option<String>), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .user_agent("lucina-link")
        .build()
        .into();
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.call().map_err(|e| match e {
        ureq::Error::Timeout(_) => "timeout".to_string(),
        other => other.to_string(),
    })?;
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((resp.status().as_u16(), location))
}

#[cfg(feature = "https")]
fn access_headers(token: Option<&AccessToken>) -> Vec<(&str, &str)> {
    match token {
        Some(t) => vec![
            ("CF-Access-Client-Id", t.client_id.expose()),
            ("CF-Access-Client-Secret", t.client_secret.expose()),
        ],
        None => Vec::new(),
    }
}

fn probe_http(
    url: &str,
    access: bool,
    token: Option<&AccessToken>,
    cidr: Option<&Ipv4Cidr>,
    timeout: Duration,
) -> Outcome {
    let start = Instant::now();
    let Some(u) = split_url(url) else {
        return Outcome::down(format!("bad url {url}"));
    };
    if let Some(c) = cidr {
        match resolve(u.host, u.port) {
            Ok(addrs) => {
                if let Err(e) = lan_check(&addrs, c) {
                    return Outcome::down(e);
                }
            }
            Err(e) => return Outcome::down(e),
        }
    }
    if !u.https {
        return match http_get_plain(u.host, u.port, u.path, start + timeout).and_then(classify_http)
        {
            Ok(()) => Outcome::up(start, None),
            Err(e) => Outcome::down(e),
        };
    }
    #[cfg(feature = "https")]
    {
        let headers = if access {
            access_headers(token)
        } else {
            Vec::new()
        };
        match https_get(url, &headers, timeout).and_then(|(status, loc)| {
            if access {
                classify_access(status, loc.as_deref())
            } else {
                classify_http(status)
            }
        }) {
            Ok(()) => Outcome::up(start, None),
            Err(e) => Outcome::down(e),
        }
    }
    #[cfg(not(feature = "https"))]
    {
        let _ = (access, token);
        match resolve(u.host, u.port).and_then(|a| connect_any(&a, start + timeout)) {
            Ok(_) => Outcome::up(
                start,
                Some("partial: tcp connect only (built without https)"),
            ),
            Err(e) => Outcome::down(e),
        }
    }
}

/// `ssh -G <alias>`: the host and port the alias really points at.
pub fn ssh_alias_endpoint(alias: &str) -> Option<(String, u16)> {
    let out = std::process::Command::new("ssh")
        .args(["-G", alias])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut host = None;
    let mut port = 22;
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("hostname", h)) => host = Some(h.trim().to_string()),
            Some(("port", p)) => port = p.trim().parse().unwrap_or(22),
            _ => {}
        }
    }
    host.map(|h| (h, port))
}

/// Probes one route, ignoring policy.
pub fn probe_route(route: &Route, token: Option<&AccessToken>) -> Outcome {
    match &route.target {
        Target::Tcp { host, port } => {
            probe_tcp(host, *port, route.lan_cidr.as_ref(), route.timeout)
        }
        Target::Http { base, health } => {
            let url = format!("{base}{}", health.as_deref().unwrap_or(""));
            probe_http(
                &url,
                route.kind == RouteKind::Cloudflare,
                token,
                route.lan_cidr.as_ref(),
                route.timeout,
            )
        }
        Target::CloudflareSsh { hostname } => probe_http(
            &format!("https://{hostname}/"),
            true,
            token,
            None,
            route.timeout,
        ),
        Target::SshJump { jump, .. } => match ssh_alias_endpoint(jump) {
            None => Outcome::down(format!("ssh -G {jump} failed")),
            Some((h, p)) => match probe_tcp(&h, p, None, route.timeout) {
                Outcome::Up { rtt_ms, .. } => Outcome::Up {
                    rtt_ms,
                    note: Some("partial: jump host reachable".into()),
                },
                other => other,
            },
        },
    }
}

/// True when no unfinished route can still beat the best finished Up route.
/// A route of the same rank that is still running would have a higher rtt, so it cannot win either.
fn can_stop(routes: &[Route], outcomes: &[Option<Outcome>], traffic: Traffic) -> bool {
    let best_rank = routes
        .iter()
        .zip(outcomes)
        .filter_map(|(r, o)| match o {
            Some(Outcome::Up { .. }) => policy::rank(r.kind, traffic),
            _ => None,
        })
        .min();
    let Some(best) = best_rank else {
        return false;
    };
    routes
        .iter()
        .zip(outcomes)
        .all(|(r, o)| o.is_some() || policy::rank(r.kind, traffic).is_none_or(|rank| rank >= best))
}

/// Probes every route the policy allows for `traffic`, in parallel, and picks the best.
/// `held` returns a reason for routes on failure backoff; those count as down without a probe.
pub fn probe_service(
    service: &Service,
    traffic: Traffic,
    token: Option<&AccessToken>,
    held: &dyn Fn(&Route) -> Option<String>,
) -> ServiceReport {
    let routes = &service.routes;
    let mut outcomes: Vec<Option<Outcome>> = vec![None; routes.len()];
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    let mut pending = 0usize;
    let mut longest = Duration::ZERO;
    for (i, r) in routes.iter().enumerate() {
        if let Some(why) = r.denial(traffic) {
            outcomes[i] = Some(Outcome::Skipped {
                reason: why.to_string(),
            });
            continue;
        }
        if let Some(why) = held(r) {
            outcomes[i] = Some(Outcome::down(why));
            continue;
        }
        let needs_token = matches!(r.target, Target::CloudflareSsh { .. })
            || (r.kind == RouteKind::Cloudflare && matches!(r.target, Target::Http { .. }));
        if needs_token && token.is_none() {
            outcomes[i] = Some(Outcome::down("no service token"));
            continue;
        }
        let route = r.clone();
        let token = token.cloned();
        let tx = tx.clone();
        // Detached on purpose: a slow loser finishes on its own timeout after we have returned.
        std::thread::spawn(move || {
            let _ = tx.send((i, probe_route(&route, token.as_ref())));
        });
        pending += 1;
        longest = longest.max(r.timeout);
    }
    drop(tx);
    let deadline = Instant::now() + longest + Duration::from_millis(500);
    let mut early = false;
    while pending > 0 {
        if can_stop(routes, &outcomes, traffic) {
            early = true;
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((i, o)) => {
                outcomes[i] = Some(o);
                pending -= 1;
            }
            Err(_) => break,
        }
    }
    let outcomes: Vec<Outcome> = outcomes
        .into_iter()
        .map(|o| {
            o.unwrap_or_else(|| {
                if early {
                    Outcome::Unknown {
                        reason: "not waited for".into(),
                    }
                } else {
                    Outcome::down("timeout")
                }
            })
        })
        .collect();
    let candidates: Vec<Candidate> = routes
        .iter()
        .zip(&outcomes)
        .map(|(r, o)| Candidate {
            kind: r.kind,
            max_body: r.max_body,
            health: o.health(),
        })
        .collect();
    let chosen = policy::select(&candidates, traffic).map(|i| routes[i].id.clone());
    ServiceReport {
        service: service.name.clone(),
        traffic,
        routes: routes
            .iter()
            .zip(outcomes)
            .map(|(r, outcome)| RouteReport {
                id: r.id.clone(),
                kind: r.kind,
                outcome,
            })
            .collect(),
        chosen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::net::TcpListener;

    fn route(id: &str, kind: RouteKind, target: Target, ms: u64) -> Route {
        Route {
            id: id.into(),
            kind,
            target,
            lan_cidr: None,
            max_body: None,
            timeout: Duration::from_millis(ms),
        }
    }
    fn tcp(port: u16) -> Target {
        Target::Tcp {
            host: "127.0.0.1".into(),
            port,
        }
    }

    /// A one-shot HTTP server answering every connection with `status`.
    fn http_server(status: u16) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\n\r\n");
            }
        });
        port
    }

    fn closed_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[test]
    fn tcp_up_and_down() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let up = route(
            "a",
            RouteKind::Lan,
            tcp(l.local_addr().unwrap().port()),
            500,
        );
        assert!(matches!(probe_route(&up, None), Outcome::Up { .. }));
        let down = route("b", RouteKind::Lan, tcp(closed_port()), 500);
        assert!(matches!(probe_route(&down, None), Outcome::Down { .. }));
    }

    #[test]
    fn unroutable_address_times_out_or_fails_fast() {
        let r = route(
            "c",
            RouteKind::Tailnet,
            Target::Tcp {
                host: "10.255.255.1".into(),
                port: 9,
            },
            50,
        );
        let t = Instant::now();
        assert!(matches!(probe_route(&r, None), Outcome::Down { .. }));
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn http_status_rules() {
        for (status, want_up) in [(200, true), (401, true), (503, false)] {
            let port = http_server(status);
            let r = route(
                "h",
                RouteKind::Tailnet,
                Target::Http {
                    base: format!("http://127.0.0.1:{port}"),
                    health: Some("/health".into()),
                },
                1000,
            );
            let got = probe_route(&r, None);
            assert_eq!(
                matches!(got, Outcome::Up { .. }),
                want_up,
                "{status}: {got:?}"
            );
        }
    }

    #[test]
    fn access_status_rules() {
        assert!(classify_access(200, None).is_ok());
        assert!(classify_access(404, None).is_ok());
        assert!(classify_access(401, None)
            .unwrap_err()
            .contains("access denied"));
        assert!(classify_access(
            302,
            Some("https://pokoin.cloudflareaccess.com/cdn-cgi/access/login")
        )
        .unwrap_err()
        .contains("login redirect"));
        assert!(classify_access(302, Some("/elsewhere")).is_ok());
        assert!(classify_access(502, None)
            .unwrap_err()
            .contains("origin unreachable"));
        assert!(classify_access(530, None)
            .unwrap_err()
            .contains("no connector"));
    }

    #[test]
    fn lan_heuristic_rejects_a_foreign_source() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut r = route(
            "lan",
            RouteKind::Lan,
            tcp(l.local_addr().unwrap().port()),
            500,
        );
        r.lan_cidr = Ipv4Cidr::parse("192.168.178.0/24");
        match probe_route(&r, None) {
            Outcome::Down { reason } => assert!(reason.contains("not on the home LAN"), "{reason}"),
            other => panic!("{other:?}"),
        }
        r.lan_cidr = Ipv4Cidr::parse("127.0.0.0/8");
        assert!(matches!(probe_route(&r, None), Outcome::Up { .. }));
    }

    #[test]
    fn early_exit_does_not_wait_for_a_slow_worse_route() {
        let fast = http_server(200);
        // Accepts (the kernel backlog does) but never answers HTTP.
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let service = Service {
            name: "s".into(),
            description: None,
            user: None,
            identity: None,
            routes: vec![
                route(
                    "public",
                    RouteKind::Public,
                    Target::Http {
                        base: format!("http://127.0.0.1:{}", silent.local_addr().unwrap().port()),
                        health: None,
                    },
                    5000,
                ),
                route(
                    "lan",
                    RouteKind::Lan,
                    Target::Http {
                        base: format!("http://127.0.0.1:{fast}"),
                        health: None,
                    },
                    500,
                ),
            ],
        };
        let t = Instant::now();
        let rep = probe_service(&service, Traffic::Api { body_bytes: 0 }, None, &|_| None);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        assert_eq!(rep.chosen.as_deref(), Some("lan"));
        assert!(matches!(rep.routes[0].outcome, Outcome::Unknown { .. }));
    }

    #[test]
    fn policy_skips_and_missing_token_are_reported() {
        let c = Config::embedded();
        let svc = c.service("nezopt-ssh").unwrap().clone();
        // Hold every probed route so the test needs no network.
        let rep = probe_service(
            &svc,
            Traffic::Bulk {
                bytes: 1,
                media: crate::policy::Media::Video,
            },
            None,
            &|_| Some("held".into()),
        );
        let by_id = |id: &str| {
            rep.routes
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .outcome
                .clone()
        };
        assert!(matches!(by_id("cloudflare"), Outcome::Skipped { .. }));
        assert!(matches!(by_id("oracle"), Outcome::Skipped { .. }));
        // A raw socket does not carry bulk HTTP transfers.
        assert!(matches!(by_id("lan"), Outcome::Skipped { .. }));
        assert_eq!(rep.chosen, None);
        let rep = probe_service(&svc, Traffic::Shell, None, &|r| {
            (r.kind != RouteKind::Cloudflare).then(|| "held".to_string())
        });
        let cf = rep.routes.iter().find(|r| r.id == "cloudflare").unwrap();
        assert_eq!(
            cf.outcome,
            Outcome::Down {
                reason: "no service token".into()
            }
        );
    }

    #[test]
    fn urls_split() {
        let u = split_url("https://getsecretary.tech/v1/media/health").unwrap();
        assert!(
            u.https
                && u.host == "getsecretary.tech"
                && u.port == 443
                && u.path == "/v1/media/health"
        );
        let u = split_url("http://100.107.164.33:8830").unwrap();
        assert!(!u.https && u.port == 8830 && u.path == "/");
        assert!(split_url("ftp://x").is_none());
    }
}
