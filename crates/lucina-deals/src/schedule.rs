//! The deal watch scheduler (`startDealWatch`'s `tick` in the Node
//! Lucina's `src/grocery/deals/watch.js`) and its settings.
//!
//! Every minute (and once at start), in the owner's zone:
//! - from `GROCERY_DEALS_DIGEST_TIME` (07:30) until 11:00, a real digest run
//!   (at most one per local day, recorded in the state file; a failed
//!   delivery is retried after the 10-minute cooldown);
//! - inside `GROCERY_DEALS_ALERT_WINDOW` (09:00-20:00), an alert run every
//!   `GROCERY_DEALS_ALERT_INTERVAL_MIN` (90) minutes, capped per day by
//!   `GROCERY_DEALS_ALERT_MAX_PER_DAY` (4).
//!
//! Runs are serialized by [`DealWatch`]. A failed digest skips that tick's
//! alert check, as on Node.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono_tz::Tz;

use crate::format::clock;
use crate::watch::{DealWatch, WatchSettings};
use crate::{optional, optional_number, DealsError};

/// `config.grocery.deals` (the watcher part) and `GROCERY_OFFER_WATCH_ENABLED`.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchConfig {
    /// `GROCERY_OFFER_WATCH_ENABLED=true` (default false).
    pub enabled: bool,
    /// `GROCERY_DEALS_CHANNEL` (`app` | `telegram` | `both`; default `app`).
    pub channel: String,
    /// `GROCERY_DEALS_DIGEST_TIME` (`07:30`).
    pub digest_time: String,
    /// `GROCERY_DEALS_ALERT_INTERVAL_MIN` (90).
    pub alert_interval_min: f64,
    /// `GROCERY_DEALS_ALERT_WINDOW` (`09:00-20:00`).
    pub alert_window: String,
    /// `GROCERY_DEALS_ALERT_MAX_PER_DAY` (4).
    pub alert_max_per_day: f64,
    /// `GROCERY_DEALS_STATE_PATH` (`data/grocery/deal-watch.json`).
    pub state_path: PathBuf,
    /// `LUCINA_REMINDER_TIMEZONE`, else Europe/Copenhagen.
    pub zone: Tz,
    /// `GROCERY_HOME_LABEL` (`home`), for the digest title.
    pub home_label: String,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: "app".into(),
            digest_time: "07:30".into(),
            alert_interval_min: 90.0,
            alert_window: "09:00-20:00".into(),
            alert_max_per_day: 4.0,
            state_path: PathBuf::from("data/grocery/deal-watch.json"),
            zone: chrono_tz::Europe::Copenhagen,
            home_label: "home".into(),
        }
    }
}

impl WatchConfig {
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(get: &F, data_dir: &Path) -> Self {
        let d = Self::default();
        let zone = match optional(get, "LUCINA_REMINDER_TIMEZONE") {
            Some(name) => name.parse().unwrap_or_else(|_| {
                tracing::warn!(zone = %name, "deal watch: unknown time zone; using Europe/Copenhagen");
                d.zone
            }),
            None => d.zone,
        };
        Self {
            enabled: optional(get, "GROCERY_OFFER_WATCH_ENABLED").as_deref() == Some("true"),
            channel: optional(get, "GROCERY_DEALS_CHANNEL").unwrap_or(d.channel),
            digest_time: optional(get, "GROCERY_DEALS_DIGEST_TIME").unwrap_or(d.digest_time),
            alert_interval_min: optional_number(get, "GROCERY_DEALS_ALERT_INTERVAL_MIN", 90.0),
            alert_window: optional(get, "GROCERY_DEALS_ALERT_WINDOW").unwrap_or(d.alert_window),
            alert_max_per_day: optional_number(get, "GROCERY_DEALS_ALERT_MAX_PER_DAY", 4.0),
            state_path: optional(get, "GROCERY_DEALS_STATE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("grocery/deal-watch.json")),
            zone,
            home_label: optional(get, "GROCERY_HOME_LABEL").unwrap_or(d.home_label),
        }
    }

    /// The settings of one [`DealWatch`].
    pub fn watch_settings(&self) -> WatchSettings {
        WatchSettings {
            state_path: self.state_path.clone(),
            zone: self.zone,
            home_label: Some(self.home_label.clone()),
            alert_max_per_day: self.alert_max_per_day,
        }
    }

    /// `settings.digestTime || '07:30'`.
    fn digest_time(&self) -> &str {
        if self.digest_time.is_empty() {
            "07:30"
        } else {
            &self.digest_time
        }
    }

    /// `(settings.alertWindow || '09:00-20:00').split('-')`.
    fn alert_window(&self) -> (&str, Option<&str>) {
        let window = if self.alert_window.is_empty() {
            "09:00-20:00"
        } else {
            &self.alert_window
        };
        let mut parts = window.split('-');
        (parts.next().unwrap_or(""), parts.next())
    }

    /// `(settings.alertIntervalMin || 90) * 60000`.
    fn alert_interval_ms(&self) -> f64 {
        let min = if self.alert_interval_min == 0.0 || self.alert_interval_min.is_nan() {
            90.0
        } else {
            self.alert_interval_min
        };
        min * 60_000.0
    }

    /// The startup line Node logs.
    pub fn describe(&self) -> String {
        format!(
            "Grocery deal watch: digest {} {}; alerts every {} min, {}",
            self.digest_time(),
            self.zone.name(),
            crate::js::num_to_string(self.alert_interval_ms() / 60_000.0),
            if self.alert_window.is_empty() {
                "09:00-20:00"
            } else {
                &self.alert_window
            }
        )
    }
}

/// What one tick ran (for logs and tests).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub digest: Option<crate::RunOutcome>,
    pub alerts: Option<crate::RunOutcome>,
}

/// The minute ticker over one [`DealWatch`].
pub struct Scheduler {
    watch: Arc<DealWatch>,
    config: WatchConfig,
    last_alert_at: tokio::sync::Mutex<f64>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(watch: Arc<DealWatch>, config: WatchConfig) -> Self {
        Self {
            watch,
            config,
            last_alert_at: tokio::sync::Mutex::new(0.0),
        }
    }

    pub fn watch(&self) -> &Arc<DealWatch> {
        &self.watch
    }

    /// Node's `tick()`.
    pub async fn tick(&self) -> Result<TickReport, DealsError> {
        let time = self.watch.now();
        let now = clock(time, self.config.zone);
        let now = now.as_str();
        let mut report = TickReport::default();
        if now >= self.config.digest_time() && now < "11:00" {
            report.digest = Some(self.watch.run_digest(false).await?);
        }
        let (start, end) = self.config.alert_window();
        let due = {
            let mut last = self.last_alert_at.lock().await;
            let due = now >= start
                && end.is_some_and(|end| now < end)
                && time as f64 - *last >= self.config.alert_interval_ms();
            if due {
                *last = time as f64;
            }
            due
        };
        if due {
            report.alerts = Some(self.watch.run_alerts(false).await?);
        }
        Ok(report)
    }

    /// Tick now and then every minute, until the task is dropped.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tracing::info!("{}", self.config.describe());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(60));
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                match self.tick().await {
                    Ok(report) => {
                        for (kind, outcome) in
                            [("digest", &report.digest), ("alerts", &report.alerts)]
                        {
                            if let Some(o) = outcome {
                                if o.sent {
                                    tracing::info!(kind, "grocery deal watch: notification sent");
                                } else {
                                    tracing::debug!(kind, reason = %o.reason, "grocery deal watch: nothing sent");
                                }
                            }
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "Grocery deal watch tick failed"),
                }
            }
        })
    }
}
