//! `Link`: the cached, failover-aware front end for library users (agents, apps).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{Config, Route, Service};
use crate::policy::Traffic;
use crate::probe::{self, ServiceReport};
use crate::secrets::AccessToken;

pub const DEFAULT_TTL: Duration = Duration::from_secs(60);
pub const BACKOFF_START: Duration = Duration::from_secs(30);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
const NET_CHECK_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("unknown service {0}")]
    UnknownService(String),
    #[error("no usable route to {}", .0.service)]
    NoRoute(Box<ServiceReport>),
}

#[derive(Debug, Clone)]
pub struct Chosen {
    pub route: Route,
    pub report: ServiceReport,
}

type Prober = dyn Fn(&Service, Traffic, Option<&AccessToken>, &dyn Fn(&Route) -> Option<String>) -> ServiceReport
    + Send
    + Sync;

struct Backoff {
    failures: u32,
    until: Instant,
}

/// Traffic that the policy treats the same way shares a cache entry: same class, same allowed routes.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    service: String,
    class: u8,
    allowed: Vec<bool>,
}

struct State {
    cache: HashMap<CacheKey, (Instant, Chosen)>,
    backoff: HashMap<(String, String), Backoff>,
    net_checked: Option<Instant>,
    net_source: Option<IpAddr>,
}

pub struct Link {
    config: Config,
    token: Option<AccessToken>,
    ttl: Duration,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    net: Box<dyn Fn() -> Option<IpAddr> + Send + Sync>,
    prober: Box<Prober>,
    state: Mutex<State>,
}

pub fn backoff_for(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    BACKOFF_START.saturating_mul(1u32 << shift).min(BACKOFF_MAX)
}

impl Link {
    pub fn new(config: Config, token: Option<AccessToken>) -> Self {
        Self {
            config,
            token,
            ttl: DEFAULT_TTL,
            clock: Box::new(Instant::now),
            net: Box::new(probe::default_source),
            prober: Box::new(probe::probe_service),
            state: Mutex::new(State {
                cache: HashMap::new(),
                backoff: HashMap::new(),
                net_checked: None,
                net_source: None,
            }),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Replaces the clock, the network-change probe and the route prober (tests, embedding).
    pub fn with_hooks(
        mut self,
        clock: impl Fn() -> Instant + Send + Sync + 'static,
        net: impl Fn() -> Option<IpAddr> + Send + Sync + 'static,
        prober: impl Fn(
                &Service,
                Traffic,
                Option<&AccessToken>,
                &dyn Fn(&Route) -> Option<String>,
            ) -> ServiceReport
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.clock = Box::new(clock);
        self.net = Box::new(net);
        self.prober = Box::new(prober);
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn key(service: &Service, traffic: Traffic) -> CacheKey {
        CacheKey {
            service: service.name.clone(),
            class: match traffic {
                Traffic::Api { .. } => 0,
                Traffic::Shell => 1,
                Traffic::Bulk { .. } => 2,
            },
            allowed: service
                .routes
                .iter()
                .map(|r| r.denial(traffic).is_none())
                .collect(),
        }
    }

    /// Drops every cached choice (and backoff) when the machine's source address changed.
    fn check_network(&self, st: &mut State, now: Instant) {
        if st
            .net_checked
            .is_some_and(|t| now.saturating_duration_since(t) < NET_CHECK_EVERY)
        {
            return;
        }
        st.net_checked = Some(now);
        let src = (self.net)();
        if src != st.net_source {
            st.net_source = src;
            st.cache.clear();
            st.backoff.clear();
        }
    }

    /// The best route to `service` for `traffic`, from the cache while it is fresh.
    pub fn route(&self, service: &str, traffic: Traffic) -> Result<Chosen, LinkError> {
        let svc = self
            .config
            .service(service)
            .ok_or_else(|| LinkError::UnknownService(service.to_string()))?;
        let key = Self::key(svc, traffic);
        let now = (self.clock)();
        let held: HashMap<String, Duration> = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            self.check_network(&mut st, now);
            if let Some((at, chosen)) = st.cache.get(&key) {
                if now.saturating_duration_since(*at) < self.ttl {
                    return Ok(chosen.clone());
                }
            }
            st.backoff
                .iter()
                .filter(|((s, _), b)| s == service && b.until > now)
                .map(|((_, r), b)| (r.clone(), b.until - now))
                .collect()
        };
        let report = (self.prober)(svc, traffic, self.token.as_ref(), &|r: &Route| {
            held.get(&r.id)
                .map(|left| format!("backing off after failures ({}s left)", left.as_secs()))
        });
        let Some(id) = report.chosen.clone() else {
            return Err(LinkError::NoRoute(Box::new(report)));
        };
        let route = svc
            .routes
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| LinkError::NoRoute(Box::new(report.clone())))?;
        let chosen = Chosen { route, report };
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.cache.insert(key, (now, chosen.clone()));
        Ok(chosen)
    }

    /// The caller could not use `route_id`: back it off and choose again on the next call.
    pub fn report_failure(&self, service: &str, route_id: &str) {
        let now = (self.clock)();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let b = st
            .backoff
            .entry((service.to_string(), route_id.to_string()))
            .or_insert(Backoff {
                failures: 0,
                until: now,
            });
        b.failures = b.failures.saturating_add(1);
        b.until = now + backoff_for(b.failures);
        st.cache.retain(|k, _| k.service != service);
    }

    pub fn report_success(&self, service: &str, route_id: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.backoff
            .remove(&(service.to_string(), route_id.to_string()));
    }

    /// Seconds of backoff left for a route, if any.
    pub fn backoff_left(&self, service: &str, route_id: &str) -> Option<Duration> {
        let now = (self.clock)();
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.backoff
            .get(&(service.to_string(), route_id.to_string()))
            .filter(|b| b.until > now)
            .map(|b| b.until - now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{self, Candidate, Health};
    use crate::probe::{Outcome, RouteReport};
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    /// A prober that marks every route Up unless held, and counts its calls.
    fn fake(calls: Arc<AtomicU32>) -> Box<Prober> {
        Box::new(
            move |svc: &Service,
                  traffic: Traffic,
                  _tok: Option<&AccessToken>,
                  held: &dyn Fn(&Route) -> Option<String>| {
                calls.fetch_add(1, Ordering::SeqCst);
                let outcomes: Vec<Outcome> = svc
                    .routes
                    .iter()
                    .map(|r| match held(r) {
                        Some(reason) => Outcome::Down { reason },
                        None => Outcome::Up {
                            rtt_ms: 1,
                            note: None,
                        },
                    })
                    .collect();
                let cands: Vec<Candidate> = svc
                    .routes
                    .iter()
                    .zip(&outcomes)
                    .map(|(r, o)| Candidate {
                        kind: r.kind,
                        max_body: r.max_body,
                        health: if r.denial(traffic).is_none() {
                            o.health()
                        } else {
                            Health::Down
                        },
                    })
                    .collect();
                ServiceReport {
                    service: svc.name.clone(),
                    traffic,
                    chosen: policy::select(&cands, traffic).map(|i| svc.routes[i].id.clone()),
                    routes: svc
                        .routes
                        .iter()
                        .zip(outcomes)
                        .map(|(r, outcome)| RouteReport {
                            id: r.id.clone(),
                            kind: r.kind,
                            outcome,
                        })
                        .collect(),
                }
            },
        )
    }

    struct Rig {
        link: Link,
        calls: Arc<AtomicU32>,
        secs: Arc<AtomicU64>,
        net: Arc<AtomicU32>,
    }

    fn rig() -> Rig {
        let calls = Arc::new(AtomicU32::new(0));
        let secs = Arc::new(AtomicU64::new(0));
        let net = Arc::new(AtomicU32::new(1));
        let base = Instant::now();
        let (s2, n2) = (secs.clone(), net.clone());
        let link = Link::new(Config::embedded(), None).with_hooks(
            move || base + Duration::from_secs(s2.load(Ordering::SeqCst)),
            move || Some(IpAddr::from([10, 0, 0, n2.load(Ordering::SeqCst) as u8])),
            fake(calls.clone()),
        );
        Rig {
            link,
            calls,
            secs,
            net,
        }
    }

    #[test]
    fn cache_hits_within_ttl_and_expires_after() {
        let r = rig();
        assert_eq!(
            r.link.route("nezopt-ssh", Traffic::Shell).unwrap().route.id,
            "lan"
        );
        r.secs.store(59, Ordering::SeqCst);
        r.link.route("nezopt-ssh", Traffic::Shell).unwrap();
        assert_eq!(r.calls.load(Ordering::SeqCst), 1);
        r.secs.store(61, Ordering::SeqCst);
        r.link.route("nezopt-ssh", Traffic::Shell).unwrap();
        assert_eq!(r.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failure_backs_off_and_fails_over() {
        let r = rig();
        r.link.route("nezopt-ssh", Traffic::Shell).unwrap();
        r.link.report_failure("nezopt-ssh", "lan");
        assert_eq!(
            r.link.route("nezopt-ssh", Traffic::Shell).unwrap().route.id,
            "tailnet"
        );
        assert_eq!(
            r.link.backoff_left("nezopt-ssh", "lan"),
            Some(Duration::from_secs(30))
        );
        r.link.report_failure("nezopt-ssh", "lan");
        assert_eq!(
            r.link.backoff_left("nezopt-ssh", "lan"),
            Some(Duration::from_secs(60))
        );
        r.link.report_success("nezopt-ssh", "lan");
        assert_eq!(r.link.backoff_left("nezopt-ssh", "lan"), None);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_for(1), Duration::from_secs(30));
        assert_eq!(backoff_for(2), Duration::from_secs(60));
        assert_eq!(backoff_for(4), Duration::from_secs(240));
        assert_eq!(backoff_for(5), BACKOFF_MAX);
        assert_eq!(backoff_for(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn backoff_expires_with_time() {
        let r = rig();
        r.link.report_failure("nezopt-ssh", "lan");
        r.secs.store(31, Ordering::SeqCst);
        assert_eq!(r.link.backoff_left("nezopt-ssh", "lan"), None);
        assert_eq!(
            r.link.route("nezopt-ssh", Traffic::Shell).unwrap().route.id,
            "lan"
        );
    }

    #[test]
    fn network_change_drops_the_cache() {
        let r = rig();
        r.link.route("nezopt-ssh", Traffic::Shell).unwrap();
        r.net.store(2, Ordering::SeqCst);
        r.secs.store(6, Ordering::SeqCst);
        r.link.route("nezopt-ssh", Traffic::Shell).unwrap();
        assert_eq!(r.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn traffic_with_other_allowed_routes_is_cached_apart() {
        let r = rig();
        r.link
            .route("lucina-media", Traffic::Api { body_bytes: 10 })
            .unwrap();
        r.link
            .route("lucina-media", Traffic::Api { body_bytes: 20 })
            .unwrap();
        assert_eq!(r.calls.load(Ordering::SeqCst), 1);
        r.link.route("lucina-media", Traffic::Shell).unwrap_err();
        assert_eq!(r.calls.load(Ordering::SeqCst), 2);
        assert!(matches!(
            r.link.route("nope", Traffic::Shell),
            Err(LinkError::UnknownService(_))
        ));
    }
}
