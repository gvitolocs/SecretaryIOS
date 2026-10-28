//! Delivery-path monitor and task supervision.
//!
//! Every minute each registered delivery path (Telegram poller, Discord session,
//! reminder scheduler, app notifications) reports up or down. A path that stays
//! down for `LUCINA_DELIVERY_ALERT_AFTER_MIN` (default 5) minutes raises one alert,
//! and its recovery one more. Alerts go to the app (`/v1/notify`, category
//! `system`); when the app path itself is down they go to the owner's Telegram
//! chat instead. The state is kept in `<data>/lucina-rs/delivery-monitor.json`, so
//! a restart neither repeats an alert nor forgets an outage.
//!
//! [`Supervisor`] keeps the handles of the background loops: `/health` answers 503
//! when one of them has exited, which is what the watchdog timer restarts on.
//! Upstream outages (Telegram, DeepSeek) never fail `/health`: they are alerts.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

/// What one path reports on a check. `None` from a probe: the path is not active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub up: bool,
    pub detail: String,
}

pub type Probe = Arc<dyn Fn(DateTime<Utc>) -> Option<Observation> + Send + Sync>;

/// Per-path monitor state (persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathState {
    /// Milliseconds since the epoch of the first failed check of this outage.
    pub down_since: Option<i64>,
    /// The outage alert was sent.
    pub alerted: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Down,
    Recovered,
}

/// The pure step (one check of one path).
///
/// - up: an outage that was alerted gets one `Recovered`; the state resets;
/// - down: the outage starts now unless it already started; once it has lasted
///   `after_ms` (and the process has been up `after_ms`, the start-up grace) one
///   `Down` is sent, never a second for the same outage.
pub fn step(
    prev: &PathState,
    up: bool,
    now_ms: i64,
    started_ms: i64,
    after_ms: i64,
) -> (PathState, Option<Alert>) {
    if up {
        let alert = prev.alerted.then_some(Alert::Recovered);
        return (PathState::default(), alert);
    }
    let since = prev.down_since.unwrap_or(now_ms).min(now_ms);
    let due =
        now_ms.saturating_sub(since) >= after_ms && now_ms.saturating_sub(started_ms) >= after_ms;
    if !prev.alerted && due {
        (
            PathState {
                down_since: Some(since),
                alerted: true,
                detail: String::new(),
            },
            Some(Alert::Down),
        )
    } else {
        (
            PathState {
                down_since: Some(since),
                alerted: prev.alerted,
                detail: String::new(),
            },
            None,
        )
    }
}

/// Probes, persisted states, and the last check (for `/health`).
pub struct Monitor {
    pub after: Duration,
    pub state_path: PathBuf,
    probes: Mutex<BTreeMap<String, Probe>>,
    states: Mutex<BTreeMap<String, PathState>>,
    started_ms: i64,
}

impl Monitor {
    pub fn new(after: Duration, state_path: PathBuf) -> Self {
        let states = std::fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            after,
            state_path,
            probes: Mutex::default(),
            states: Mutex::new(states),
            started_ms: Utc::now().timestamp_millis(),
        }
    }

    /// A delivery path registers itself (Telegram, Discord, …).
    pub fn register(&self, name: &str, probe: Probe) {
        self.probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), probe);
    }

    /// One check of every path: the alerts to send, as `(path, alert, detail)`.
    pub fn check(&self, now: DateTime<Utc>) -> Vec<(String, Alert, String)> {
        let probes: Vec<(String, Probe)> = self
            .probes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let now_ms = now.timestamp_millis();
        let mut alerts = Vec::new();
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        for (name, probe) in probes {
            let Some(obs) = probe(now) else {
                states.remove(&name);
                continue;
            };
            let prev = states.get(&name).cloned().unwrap_or_default();
            let (mut next, alert) = step(
                &prev,
                obs.up,
                now_ms,
                self.started_ms,
                self.after.as_millis() as i64,
            );
            if !obs.up {
                next.detail = obs.detail.chars().take(300).collect();
            }
            if let Some(a) = alert {
                alerts.push((name.clone(), a, obs.detail.clone()));
            }
            if next == PathState::default() {
                states.remove(&name);
            } else {
                states.insert(name, next);
            }
        }
        if let Ok(text) = serde_json::to_vec_pretty(&*states) {
            if let Some(dir) = self.state_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = lucina_store::atomic::write_atomic(&self.state_path, &text) {
                tracing::warn!(error = %e, "delivery monitor: state not saved");
            }
        }
        alerts
    }

    pub fn health(&self) -> serde_json::Value {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let probes = self.probes.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "alertAfterMinutes": self.after.as_secs() / 60,
            "paths": probes.keys().map(|k| {
                let s = states.get(k).cloned().unwrap_or_default();
                (k.clone(), json!({"up": s.down_since.is_none(), "downSince": s.down_since.and_then(DateTime::from_timestamp_millis).map(|d| d.to_rfc3339()), "alerted": s.alerted, "detail": s.detail}))
            }).collect::<serde_json::Map<_, _>>(),
        })
    }
}

pub fn alert_text(path: &str, alert: Alert, detail: &str, after: Duration) -> String {
    let label = match path {
        "telegram" => "Telegram (@Lucina00_bot)",
        "discord" => "Discord",
        "reminders" => "Reminder delivery",
        "app" => "App notifications",
        other => other,
    };
    match alert {
        Alert::Down => {
            let detail = if detail.is_empty() {
                String::new()
            } else {
                format!(" ({detail})")
            };
            format!(
                "⚠️ {label} has been down for more than {} minutes{detail}. Lucina keeps retrying.",
                after.as_secs() / 60
            )
        }
        Alert::Recovered => format!("✅ {label}: working again."),
    }
}

// ------------------------------------------------------------ app notify health

static NOTIFY_LAST_OK: AtomicI64 = AtomicI64::new(0);
static NOTIFY_LAST_FAIL: AtomicI64 = AtomicI64::new(0);

/// Called by `bridges::notify` after every attempt sequence.
pub fn record_notify(ok: bool) {
    let now = Utc::now().timestamp_millis();
    if ok {
        NOTIFY_LAST_OK.store(now, Ordering::Relaxed);
    } else {
        NOTIFY_LAST_FAIL.store(now, Ordering::Relaxed);
    }
}

/// The app path is down while the last notification failed and none succeeded since.
fn app_probe() -> Probe {
    Arc::new(|_| {
        let (ok, fail) = (
            NOTIFY_LAST_OK.load(Ordering::Relaxed),
            NOTIFY_LAST_FAIL.load(Ordering::Relaxed),
        );
        (ok != 0 || fail != 0).then(|| Observation {
            up: fail <= ok,
            detail: if fail > ok {
                "secretary-server /v1/notify failing".into()
            } else {
                String::new()
            },
        })
    })
}

/// The scheduler is down when it stopped ticking (3 poll intervals) or its last
/// tick could not deliver a due reminder.
fn scheduler_probe(st: &AppState) -> Probe {
    let health = st.scheduler_health.clone();
    let poll = st.cfg.scheduler_poll;
    let initial = st.cfg.scheduler_initial_delay;
    let started = Utc::now();
    Arc::new(move |now| {
        let h = health.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !h.enabled {
            return None;
        }
        let last = h
            .last_tick_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(started + chrono::Duration::from_std(initial).unwrap_or_default());
        let stale = now - last > chrono::Duration::from_std(poll * 3).unwrap_or_default();
        Some(if stale {
            Observation {
                up: false,
                detail: "the reminder scheduler stopped ticking".into(),
            }
        } else if let Some(e) = h.last_error {
            Observation {
                up: false,
                detail: e.chars().take(200).collect(),
            }
        } else {
            Observation {
                up: true,
                detail: String::new(),
            }
        })
    })
}

/// Telegram is down while the poller holds no lease, the token is refused, or no
/// `getUpdates` succeeded for 2 minutes (a long poll is 30 s).
pub fn telegram_probe(health: lucina_telegram::poller::SharedHealth) -> Probe {
    let started = Utc::now();
    Arc::new(move |now| {
        let h = health.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !h.enabled {
            return None;
        }
        let last_ok = h
            .last_poll_ok_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(started);
        let fresh = now - last_ok <= chrono::Duration::minutes(2);
        let up = h.lease_held && h.state != "unauthorized" && fresh;
        Some(Observation {
            up,
            detail: if up {
                String::new()
            } else if !h.lease_held {
                match &h.holder {
                    Some(o) => format!("another poller holds the bot ({})", o.instance),
                    None => "no polling lease".into(),
                }
            } else if h.state == "unauthorized" {
                "the bot token was refused (401)".into()
            } else {
                h.last_error.unwrap_or_else(|| "no successful poll".into())
            },
        })
    })
}

/// Discord is down while the message session holds no lease, the gateway is
/// not connected, the token was refused, or no gateway event (heartbeat
/// acknowledgements included, every ~41 s) arrived for 2 minutes.
pub fn discord_probe(health: lucina_discord::session::SharedHealth) -> Probe {
    let started = Utc::now();
    Arc::new(move |now| {
        let h = health.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !h.enabled {
            return None;
        }
        let last = h
            .last_event_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(started);
        let fresh = now - last <= chrono::Duration::minutes(2);
        let up = h.lease_held && h.connected && fresh;
        Some(Observation {
            up,
            detail: if up {
                String::new()
            } else if h.state == "unauthorized" {
                "the Discord token was refused (4004)".into()
            } else if !h.lease_held {
                match &h.lease_holder {
                    Some(o) => format!("another session holds the Discord lease ({o})"),
                    None => h
                        .last_error
                        .unwrap_or_else(|| "no Discord session lease".into()),
                }
            } else if !h.connected {
                h.last_error
                    .unwrap_or_else(|| "the Discord gateway is not connected".into())
            } else {
                "no Discord gateway event or heartbeat for 2 minutes".into()
            },
        })
    })
}

/// Owner Telegram message (used when the app path is the one down).
async fn telegram_owner(st: &AppState, text: &str) {
    let (Some(token), Some(chat)) = (&st.cfg.telegram_token, &st.cfg.owner_chat_id) else {
        return;
    };
    let bot = lucina_telegram::BotApi::new(token.clone());
    let opts = lucina_telegram::SendOptions {
        disable_web_page_preview: true,
        ..Default::default()
    };
    if let Err(e) = bot.send_text(chat, text, &opts).await {
        tracing::warn!(error = %e, "delivery monitor: Telegram alert failed");
    }
}

/// Register the built-in paths and start the minute loop.
pub fn spawn(st: &AppState) -> tokio::task::JoinHandle<()> {
    let m = st.monitor.clone();
    if st.cfg.app_notify {
        m.register("app", app_probe());
    }
    m.register("reminders", scheduler_probe(st));
    let st = st.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(60));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            for (path, alert, detail) in m.check(Utc::now()) {
                let text = alert_text(&path, alert, &detail, m.after);
                tracing::warn!(%path, ?alert, "delivery path alert");
                crate::ops::delivery_alert(&st, &path, alert == Alert::Down, &text);
                if path == "app" {
                    telegram_owner(&st, &text).await;
                } else {
                    let since = m
                        .states
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&path)
                        .and_then(|s| s.down_since)
                        .unwrap_or_default();
                    let kind = match alert {
                        Alert::Down => "down",
                        Alert::Recovered => "up",
                    };
                    let body = json!({"category": "system", "body": text, "route": "/", "channel": "other", "dedupKey": format!("delivery:{path}:{kind}:{since}:{}", Utc::now().timestamp_millis() / 60_000)});
                    let r = crate::bridges::notify(&st.secretary, &body).await;
                    if !r.ok {
                        // The app path itself is failing: fall back to Telegram.
                        telegram_owner(&st, &text).await;
                    }
                }
            }
        }
    })
}

// ------------------------------------------------------------ supervision

/// Background loops whose exit makes `/health` fail.
#[derive(Default)]
pub struct Supervisor {
    tasks: Mutex<Vec<(String, tokio::task::JoinHandle<()>)>>,
    /// Loops whose handle is owned elsewhere (the Telegram poller: `main` awaits it
    /// at shutdown); the flag turns false when the loop ends.
    flags: Mutex<Vec<(String, Arc<std::sync::atomic::AtomicBool>)>>,
}

impl Supervisor {
    pub fn watch(&self, name: &str, handle: tokio::task::JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name.to_owned(), handle));
    }

    /// A loop whose handle someone else owns: returns the flag it clears on exit.
    pub fn watch_flag(&self, name: &str) -> Arc<std::sync::atomic::AtomicBool> {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.flags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name.to_owned(), flag.clone()));
        flag
    }

    /// Names of loops that exited or panicked.
    pub fn dead(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(n, _)| n.clone())
            .collect();
        out.extend(
            self.flags
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(_, f)| !f.load(Ordering::Relaxed))
                .map(|(n, _)| n.clone()),
        );
        out
    }

    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        out.extend(
            self.flags
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(n, _)| n.clone()),
        );
        out
    }
}

/// Liveness problems that justify a restart (empty: alive).
pub fn liveness(st: &AppState, now: DateTime<Utc>) -> Vec<String> {
    let mut out: Vec<String> = st
        .supervisor
        .dead()
        .into_iter()
        .map(|n| format!("{n} loop exited"))
        .collect();
    let h = st
        .scheduler_health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if h.enabled {
        let grace = st.cfg.scheduler_initial_delay + st.cfg.scheduler_poll * 10;
        let up = st.started.elapsed();
        let last = h
            .last_tick_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.with_timezone(&Utc));
        let stalled = match last {
            Some(t) => {
                now - t > chrono::Duration::from_std(st.cfg.scheduler_poll * 10).unwrap_or_default()
            }
            None => up > grace,
        };
        if stalled {
            out.push("reminder scheduler made no tick for 10 poll intervals".into());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn one_alert_per_outage_and_one_recovery() {
        let after = 5 * MIN;
        let mut s = PathState::default();
        let mut alerts = Vec::new();
        // Down from minute 10 to minute 20, up at 21; the process started at 0.
        for m in 10..=21 {
            let (n, a) = step(&s, m == 21, m * MIN, 0, after);
            s = n;
            alerts.extend(a.map(|a| (m, a)));
        }
        assert_eq!(alerts, vec![(15, Alert::Down), (21, Alert::Recovered)]);
        assert_eq!(s, PathState::default());
    }

    #[test]
    fn short_outage_is_silent_and_startup_grace_holds() {
        let after = 5 * MIN;
        let (s, a) = step(&PathState::default(), false, 10 * MIN, 0, after);
        assert!(a.is_none());
        let (s, a) = step(&s, true, 12 * MIN, 0, after);
        assert!(a.is_none());
        assert_eq!(s, PathState::default());
        // Down since minute 0 but the process only started at minute 3: wait until 5 after start.
        let mut s = PathState {
            down_since: Some(0),
            alerted: false,
            detail: String::new(),
        };
        for m in 3..8 {
            let (n, a) = step(&s, false, m * MIN, 3 * MIN, after);
            assert!(a.is_none(), "minute {m}");
            s = n;
        }
        let (_, a) = step(&s, false, 8 * MIN, 3 * MIN, after);
        assert_eq!(a, Some(Alert::Down));
    }

    #[test]
    fn restart_keeps_an_alerted_outage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.json");
        let m = Monitor::new(Duration::from_secs(300), path.clone());
        let down: Probe = Arc::new(|_| {
            Some(Observation {
                up: false,
                detail: "x".into(),
            })
        });
        m.register("telegram", down);
        let t0 = Utc::now() + chrono::Duration::minutes(10);
        assert!(m.check(t0).is_empty());
        let alerts = m.check(t0 + chrono::Duration::minutes(6));
        assert_eq!(alerts.len(), 1);
        // A new process reads the state: no second Down, one Recovered later.
        let m2 = Monitor::new(Duration::from_secs(300), path);
        let up = Arc::new(Mutex::new(false));
        let u = up.clone();
        m2.register(
            "telegram",
            Arc::new(move |_| {
                Some(Observation {
                    up: *u.lock().unwrap(),
                    detail: String::new(),
                })
            }),
        );
        assert!(m2.check(t0 + chrono::Duration::minutes(30)).is_empty());
        *up.lock().unwrap() = true;
        let alerts = m2.check(t0 + chrono::Duration::minutes(31));
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].1, Alert::Recovered);
    }

    #[test]
    fn discord_probe_needs_lease_connection_and_fresh_events() {
        use lucina_discord::session::SessionHealth;
        let h = Arc::new(Mutex::new(SessionHealth::default()));
        let probe = discord_probe(h.clone());
        let now = Utc::now();
        // Off: not a path.
        assert_eq!(probe(now), None);
        *h.lock().unwrap() = SessionHealth {
            enabled: true,
            state: "lease_wait".into(),
            lease_holder: Some("lucina-rs pid 7 on nezopt".into()),
            ..Default::default()
        };
        let o = probe(now).unwrap();
        assert!(!o.up);
        assert_eq!(
            o.detail,
            "another session holds the Discord lease (lucina-rs pid 7 on nezopt)"
        );
        {
            let mut g = h.lock().unwrap();
            g.lease_held = true;
            g.connected = true;
            g.state = "connected".into();
            g.last_event_at = Some(now.to_rfc3339());
        }
        assert!(probe(now + chrono::Duration::seconds(60)).unwrap().up);
        let stale = probe(now + chrono::Duration::minutes(3)).unwrap();
        assert!(!stale.up);
        assert_eq!(
            stale.detail,
            "no Discord gateway event or heartbeat for 2 minutes"
        );
        {
            let mut g = h.lock().unwrap();
            g.lease_held = false;
            g.connected = false;
            g.state = "unauthorized".into();
        }
        assert_eq!(
            probe(now).unwrap().detail,
            "the Discord token was refused (4004)"
        );
    }

    #[test]
    fn texts() {
        assert_eq!(
            alert_text("telegram", Alert::Down, "401", Duration::from_secs(300)),
            "⚠️ Telegram (@Lucina00_bot) has been down for more than 5 minutes (401). Lucina keeps retrying."
        );
        assert_eq!(
            alert_text("app", Alert::Recovered, "", Duration::from_secs(300)),
            "✅ App notifications: working again."
        );
    }
}
