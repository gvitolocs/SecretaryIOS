//! Cheap HTTP health checks of the owner's services (`[health]` targets),
//! with hysteresis so a single blip never pages him:
//!
//! - **down** after `fail_threshold` failed checks in a row (error, non-2xx/3xx,
//!   or slower than `slow_ms`) spanning at least `min_down_minutes`;
//! - **back up** after `recover_threshold` good checks in a row.
//!
//! Each outage is one `service_down` event (dedup on the outage start) and
//! one `service_recovered` event. The state lives in `source_state`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{HealthProbe, Probe, Source};
use crate::config::Config;
use crate::model::{Decision, Event, Kind};
use crate::score;
use crate::store::Store;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetState {
    pub fails: u32,
    pub oks: u32,
    pub first_fail_at: Option<DateTime<Utc>>,
    pub down: bool,
    pub down_since: Option<DateTime<Utc>>,
    pub last_status: u16,
    pub last_ms: u64,
    pub last_error: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
}

/// What one check changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    None,
    WentDown { since: DateTime<Utc> },
    Recovered { since: DateTime<Utc> },
}

/// The hysteresis, pure: the next state after `probe` at `now`.
pub fn step(
    mut s: TargetState,
    probe: &Probe,
    now: DateTime<Utc>,
    fail_threshold: u32,
    min_down: chrono::Duration,
    recover_threshold: u32,
) -> (TargetState, Transition) {
    s.last_status = probe.status;
    s.last_ms = probe.ms;
    s.last_error = probe.error.clone();
    s.checked_at = Some(now);
    let mut t = Transition::None;
    if probe.ok {
        s.oks += 1;
        s.fails = 0;
        s.first_fail_at = None;
        if s.down && s.oks >= recover_threshold {
            t = Transition::Recovered {
                since: s.down_since.unwrap_or(now),
            };
            s.down = false;
            s.down_since = None;
        }
    } else {
        s.fails += 1;
        s.oks = 0;
        let first = *s.first_fail_at.get_or_insert(now);
        if !s.down && s.fails >= fail_threshold && now - first >= min_down {
            s.down = true;
            s.down_since = Some(first);
            t = Transition::WentDown { since: first };
        }
    }
    (s, t)
}

pub struct HealthSource {
    pub cfg: Arc<Config>,
    pub probe: Arc<dyn HealthProbe>,
    /// `url_env` targets resolved (name → URL).
    pub env_urls: BTreeMap<String, String>,
}

impl HealthSource {
    /// The targets with their URLs (env targets without a value are skipped).
    pub fn targets(&self) -> Vec<(String, String, Option<String>)> {
        self.cfg
            .health
            .targets
            .iter()
            .filter_map(|t| {
                let url = t.url.clone().or_else(|| {
                    t.url_env
                        .as_ref()
                        .and_then(|n| self.env_urls.get(n).cloned())
                })?;
                Some((t.name.clone(), url, t.topic.clone()))
            })
            .collect()
    }
}

/// Is this target one of the owner's own products?
fn own_product(topic: Option<&str>) -> bool {
    matches!(topic, Some("pokoin" | "secretary" | "lucina"))
}

#[async_trait::async_trait]
impl Source for HealthSource {
    fn name(&self) -> &'static str {
        "health"
    }

    fn schedule(&self) -> (Duration, Duration) {
        let h = &self.cfg.health;
        (
            Duration::from_secs(h.interval_secs),
            Duration::from_secs(h.jitter_secs),
        )
    }

    async fn observe(&self, now: DateTime<Utc>, store: &dyn Store) -> Result<Vec<Event>> {
        let h = &self.cfg.health;
        let mut out = Vec::new();
        for (name, url, topic) in self.targets() {
            let probe = self
                .probe
                .probe(&url, Duration::from_secs(h.timeout_secs), h.slow_ms)
                .await;
            let prev: TargetState = store
                .get_state("health", &name)
                .await?
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let (next, t) = step(
                prev,
                &probe,
                now,
                h.fail_threshold,
                chrono::Duration::minutes(h.min_down_minutes),
                h.recover_threshold,
            );
            store
                .put_state("health", &name, &serde_json::to_value(&next)?, now)
                .await?;
            let own = own_product(topic.as_deref());
            match t {
                Transition::None => {}
                Transition::WentDown { since } => {
                    let minutes = (now - since).num_minutes();
                    out.push(
                        Event::new(
                            "health",
                            Kind::ServiceDown,
                            format!("service_down:{name}:{}", since.timestamp_millis()),
                            now,
                            format!("{name} down"),
                        )
                        .scores(score::service_down(minutes, own))
                        .signal(format!(
                            "{name}: {} failed checks in a row over {minutes} min (last: {})",
                            next.fails,
                            next.last_error.as_deref().unwrap_or("error")
                        ))
                        .payload(json!({
                            "service": name, "url": url, "topic": topic,
                            "downSince": since, "minutesDown": minutes,
                            "failedChecks": next.fails, "lastError": next.last_error, "lastStatus": next.last_status,
                        })),
                    );
                }
                Transition::Recovered { since } => {
                    let down_key = format!("service_down:{name}:{}", since.timestamp_millis());
                    let alerted = store
                        .candidate_by_key(&down_key)
                        .await?
                        .is_some_and(|c| c.decision == Decision::Sent);
                    let minutes = (now - since).num_minutes();
                    out.push(
                        Event::new(
                            "health",
                            Kind::ServiceRecovered,
                            format!("service_recovered:{name}:{}", since.timestamp_millis()),
                            now,
                            format!("{name} back up"),
                        )
                        .expires(now + chrono::Duration::hours(12))
                        .scores(score::service_recovered(alerted))
                        .signal(format!(
                            "{name} answers again after about {minutes} min{}",
                            if alerted {
                                "; he was told it was down"
                            } else {
                                ""
                            }
                        ))
                        .payload(json!({
                            "service": name, "url": url, "topic": topic,
                            "downSince": since, "minutesDown": minutes, "weAlerted": alerted,
                        })),
                    );
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(ok: bool) -> Probe {
        Probe {
            ok,
            status: if ok { 200 } else { 502 },
            ms: 100,
            error: (!ok).then(|| "HTTP 502".to_owned()),
        }
    }

    #[test]
    fn hysteresis_never_on_a_blip() {
        let t0 = DateTime::parse_from_rfc3339("2026-10-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let min = |m: i64| t0 + chrono::Duration::minutes(m);
        let run = |s: TargetState, ok: bool, at: DateTime<Utc>| {
            step(s, &p(ok), at, 3, chrono::Duration::minutes(5), 2)
        };

        // One blip, then fine.
        let (s, t) = run(TargetState::default(), false, min(0));
        assert_eq!(t, Transition::None);
        let (s, t) = run(s, true, min(2));
        assert_eq!(t, Transition::None);
        assert!(!s.down && s.fails == 0);

        // Three fails but within 4 minutes: not yet.
        let (s, _) = run(s, false, min(10));
        let (s, _) = run(s, false, min(12));
        let (s, t) = run(s, false, min(14));
        assert_eq!(t, Transition::None);
        // Fourth fail, 6 minutes after the first: down since the first.
        let (s, t) = run(s, false, min(16));
        assert_eq!(t, Transition::WentDown { since: min(10) });
        let (s, t) = run(s, false, min(18));
        assert_eq!(t, Transition::None, "one event per outage");
        // One good check is not a recovery.
        let (s, t) = run(s, true, min(20));
        assert_eq!(t, Transition::None);
        assert!(s.down);
        let (s, t) = run(s, true, min(22));
        assert_eq!(t, Transition::Recovered { since: min(10) });
        assert!(!s.down && s.down_since.is_none());
    }
}
