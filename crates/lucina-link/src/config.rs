//! The route table: services and the routes that reach them.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::policy::RouteKind;

/// The route table compiled into the binary.
pub const DEFAULT_ROUTES: &str = include_str!("../routes.toml");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing the route table: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("service {service}, route {route}: {problem}")]
    Route {
        service: String,
        route: String,
        problem: String,
    },
    #[error("service {service}: {problem}")]
    Service { service: String, problem: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Raw TCP (SSH and the like).
    Tcp { host: String, port: u16 },
    /// An HTTP(S) base URL, optionally with a health path probed under it.
    Http {
        base: String,
        health: Option<String>,
    },
    /// A Cloudflare Access SSH hostname (`cloudflared access ssh`).
    CloudflareSsh { hostname: String },
    /// `ssh -W host:port jump`.
    SshJump {
        jump: String,
        host: String,
        port: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Cidr {
    net: u32,
    prefix: u8,
}

impl Ipv4Cidr {
    pub fn parse(s: &str) -> Option<Self> {
        let (addr, prefix) = s.split_once('/')?;
        let addr: Ipv4Addr = addr.parse().ok()?;
        let prefix: u8 = prefix.parse().ok()?;
        if prefix > 32 {
            return None;
        }
        Some(Self {
            net: u32::from(addr) & Self::mask(prefix),
            prefix,
        })
    }

    fn mask(prefix: u8) -> u32 {
        if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => u32::from(v4) & Self::mask(self.prefix) == self.net,
            IpAddr::V6(_) => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Route {
    pub id: String,
    pub kind: RouteKind,
    pub target: Target,
    pub lan_cidr: Option<Ipv4Cidr>,
    pub max_body: Option<u64>,
    pub timeout: Duration,
}

impl Route {
    /// Why this route's target cannot carry `traffic` at all (an HTTP base for a shell, a raw socket for an API
    /// call), before the size and media policy is even asked. `None` when it can.
    pub fn target_mismatch(&self, traffic: crate::policy::Traffic) -> Option<&'static str> {
        let shell_target = !matches!(self.target, Target::Http { .. });
        match (traffic, shell_target) {
            (crate::policy::Traffic::Shell, false) => Some("an HTTP route does not carry shells"),
            (crate::policy::Traffic::Shell, true) => None,
            (_, true) => Some("not an HTTP route"),
            (_, false) => None,
        }
    }

    /// Why this route may not carry `traffic` (target first, then the policy), or `None` when it may.
    pub fn denial(&self, traffic: crate::policy::Traffic) -> Option<&'static str> {
        self.target_mismatch(traffic)
            .or_else(|| crate::policy::denial(self.kind, traffic, self.max_body))
    }
}

#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    pub user: Option<String>,
    pub identity: Option<String>,
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub services: BTreeMap<String, Service>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    services: BTreeMap<String, RawService>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    description: Option<String>,
    user: Option<String>,
    identity: Option<String>,
    routes: Vec<RawRoute>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    id: String,
    kind: RouteKind,
    tcp: Option<String>,
    http: Option<String>,
    health: Option<String>,
    ssh_hostname: Option<String>,
    jump: Option<String>,
    lan_cidr: Option<String>,
    max_body_bytes: Option<u64>,
    timeout_ms: Option<u64>,
}

pub fn default_timeout(kind: RouteKind) -> Duration {
    Duration::from_millis(match kind {
        RouteKind::Lan => 400,
        RouteKind::Tailnet => 1500,
        RouteKind::Public => 3000,
        RouteKind::Cloudflare => 4000,
        RouteKind::SshJump => 3000,
    })
}

fn parse_host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text)?;
        let mut services = BTreeMap::new();
        for (name, rs) in raw.services {
            if rs.routes.is_empty() {
                return Err(ConfigError::Service {
                    service: name,
                    problem: "no routes".into(),
                });
            }
            let mut routes: Vec<Route> = Vec::new();
            for rr in rs.routes {
                let err = |problem: &str| ConfigError::Route {
                    service: name.clone(),
                    route: rr.id.clone(),
                    problem: problem.to_string(),
                };
                if routes.iter().any(|r| r.id == rr.id) {
                    return Err(err("duplicate route id"));
                }
                let set = [
                    rr.tcp.is_some(),
                    rr.http.is_some(),
                    rr.ssh_hostname.is_some(),
                ]
                .iter()
                .filter(|b| **b)
                .count();
                if set != 1 {
                    return Err(err("needs exactly one of tcp, http, ssh_hostname"));
                }
                if rr.health.is_some() && rr.http.is_none() {
                    return Err(err("health needs http"));
                }
                let target = if let Some(jump) = &rr.jump {
                    if rr.kind != RouteKind::SshJump {
                        return Err(err("jump is only for kind ssh_jump"));
                    }
                    let (host, port) = rr
                        .tcp
                        .as_deref()
                        .and_then(parse_host_port)
                        .ok_or_else(|| err("ssh_jump needs tcp = \"host:port\""))?;
                    Target::SshJump {
                        jump: jump.clone(),
                        host,
                        port,
                    }
                } else if rr.kind == RouteKind::SshJump {
                    return Err(err("kind ssh_jump needs jump"));
                } else if let Some(tcp) = &rr.tcp {
                    if rr.kind == RouteKind::Cloudflare {
                        return Err(err("cloudflare routes use ssh_hostname or an https url"));
                    }
                    let (host, port) =
                        parse_host_port(tcp).ok_or_else(|| err("tcp must be \"host:port\""))?;
                    Target::Tcp { host, port }
                } else if let Some(url) = &rr.http {
                    let ok = url.starts_with("https://")
                        || (url.starts_with("http://") && rr.kind != RouteKind::Cloudflare);
                    if !ok {
                        return Err(err(
                            "http must be an http:// or https:// url (https for cloudflare)",
                        ));
                    }
                    Target::Http {
                        base: url.trim_end_matches('/').to_string(),
                        health: rr.health.clone(),
                    }
                } else {
                    if rr.kind != RouteKind::Cloudflare {
                        return Err(err("ssh_hostname is only for kind cloudflare"));
                    }
                    Target::CloudflareSsh {
                        hostname: rr.ssh_hostname.clone().unwrap_or_default(),
                    }
                };
                let lan_cidr = match &rr.lan_cidr {
                    None => None,
                    Some(s) => Some(Ipv4Cidr::parse(s).ok_or_else(|| err("bad lan_cidr"))?),
                };
                routes.push(Route {
                    id: rr.id.clone(),
                    kind: rr.kind,
                    target,
                    lan_cidr,
                    max_body: rr.max_body_bytes,
                    timeout: rr
                        .timeout_ms
                        .map(Duration::from_millis)
                        .unwrap_or_else(|| default_timeout(rr.kind)),
                });
            }
            services.insert(
                name.clone(),
                Service {
                    name,
                    description: rs.description,
                    user: rs.user,
                    identity: rs.identity,
                    routes,
                },
            );
        }
        Ok(Self { services })
    }

    pub fn embedded() -> Self {
        Self::parse(DEFAULT_ROUTES).expect("the embedded route table is valid")
    }

    pub fn load_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// `$LUCINA_LINK_CONFIG`, else `~/.config/lucina-link/routes.toml` if it exists, else the embedded table.
    pub fn load() -> Result<Self, ConfigError> {
        if let Some(p) = std::env::var_os("LUCINA_LINK_CONFIG") {
            return Self::load_file(Path::new(&p));
        }
        if let Some(p) = config_dir().map(|d| d.join("routes.toml")) {
            if p.exists() {
                return Self::load_file(&p);
            }
        }
        Ok(Self::embedded())
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.get(name)
    }
}

/// `~/.config/lucina-link`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/lucina-link"))
}

/// Expands a leading `~/`.
pub fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_parses() {
        let c = Config::embedded();
        let names: Vec<&str> = c.services.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            [
                "kindle-ssh",
                "lucina-media",
                "lucina-memo",
                "lucina-rs",
                "nezopt-ssh",
                "secretary"
            ]
        );
        let nez = c.service("nezopt-ssh").unwrap();
        assert_eq!(nez.routes.len(), 4);
        assert_eq!(nez.routes[0].timeout, Duration::from_millis(400));
        assert!(matches!(
            &nez.routes[3].target,
            Target::SshJump { jump, port: 2224, .. } if jump == "oracle-peer1"
        ));
        assert_eq!(
            c.service("kindle-ssh").unwrap().identity.as_deref(),
            Some("~/.ssh/kindle_pw3")
        );
    }

    fn route_err(toml: &str) -> String {
        Config::parse(toml).unwrap_err().to_string()
    }

    #[test]
    fn bad_tables_name_the_route() {
        let e = route_err(
            r#"[services.a]
routes = [{ id = "x", kind = "lan", tcp = "h:1" }, { id = "x", kind = "lan", tcp = "h:2" }]"#,
        );
        assert!(e.contains("service a, route x: duplicate"), "{e}");
        let e = route_err(
            r#"[services.b]
routes = [{ id = "y", kind = "lan", tcp = "h:1", http = "http://h" }]"#,
        );
        assert!(e.contains("route y: needs exactly one"), "{e}");
        let e = route_err(
            r#"[services.c]
routes = [{ id = "z", kind = "ssh_jump", tcp = "h:1" }]"#,
        );
        assert!(e.contains("needs jump"), "{e}");
        let e = route_err(
            r#"[services.d]
routes = [{ id = "w", kind = "cloudflare", http = "http://plain" }]"#,
        );
        assert!(e.contains("https for cloudflare"), "{e}");
        let e = route_err(
            r#"[services.e]
routes = [{ id = "v", kind = "lan", tcp = "nohostport" }]"#,
        );
        assert!(e.contains("host:port"), "{e}");
        assert!(route_err(
            r#"[services.f]
routes = [{ id = "u", kind = "warp", tcp = "h:1" }]"#
        )
        .contains("parsing"));
        assert!(route_err("[services.g]\nroutes = []").contains("no routes"));
    }

    #[test]
    fn cidr_contains() {
        let c = Ipv4Cidr::parse("192.168.178.0/24").unwrap();
        assert!(c.contains("192.168.178.55".parse().unwrap()));
        assert!(!c.contains("192.168.179.1".parse().unwrap()));
        assert!(!c.contains("::1".parse().unwrap()));
        assert!(Ipv4Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains("8.8.8.8".parse().unwrap()));
        assert!(Ipv4Cidr::parse("1.2.3.4/33").is_none());
    }
}
