//! What a tick of the clock does: check the parcels that are due, and say what the owner should
//! hear. Hourly parcels (close to him) are looked at every hour and reported when they change;
//! everything else is looked at once a local day and goes into one morning digest.

use chrono::{DateTime, Duration, Timelike, Utc};
use chrono_tz::Tz;

use crate::client::{Track, TrackError};
use crate::policy::{cadence, changed, Cadence, Home, Stage, Status};
use crate::store::{Parcel, ParcelState, Store};
use crate::text;

const HOURLY_EVERY_MINUTES: i64 = 60;

#[derive(Debug, Clone)]
pub struct ScheduleCfg {
    pub tz: Tz,
    /// The local hour (0-23) of the daily pass.
    pub digest_hour: u32,
    pub home: Home,
    /// A parcel with no change for this long is dropped.
    pub stale_days: i64,
}

impl ScheduleCfg {
    pub fn new(tz: Tz, digest_hour: u32, home: Home) -> Self {
        Self {
            tz,
            digest_hour,
            home,
            stale_days: 45,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Digest,
    Change,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerMessage {
    pub kind: MessageKind,
    pub text: String,
    pub dedup_key: String,
}

fn parcel_key(p: &Parcel, s: &Status) -> String {
    format!(
        "parcel:{}:{}:{}",
        p.id,
        s.stage.as_str(),
        s.last_update.as_deref().unwrap_or("-")
    )
}

/// A lookup done: the parcel as stored afterwards, and the status it had before.
pub(crate) struct Checked {
    pub(crate) old: Option<Status>,
    pub(crate) parcel: Parcel,
    pub(crate) result: Result<Status, TrackError>,
}

/// Looks the parcel up and records the outcome. `None`: it was removed meanwhile.
pub(crate) async fn check<T: Track>(
    store: &Store,
    tracker: &T,
    p: &Parcel,
    now: DateTime<Utc>,
) -> Option<Checked> {
    let old = p.status.clone();
    match tracker.track(&p.number, p.carrier.as_deref()).await {
        Ok(new) => {
            let differs = changed(old.as_ref(), &new);
            let stored = new.clone();
            let parcel = store.update(&p.id, |q| {
                q.last_checked = Some(now);
                q.last_error = None;
                if differs {
                    q.last_change = Some(now);
                }
                q.status = Some(stored);
            })?;
            Some(Checked {
                old,
                parcel,
                result: Ok(new),
            })
        }
        Err(e) => {
            let line = text::failure_line(&e);
            let parcel = store.update(&p.id, |q| {
                q.last_checked = Some(now);
                q.last_error = Some(line.clone());
            })?;
            Some(Checked {
                old,
                parcel,
                result: Err(e),
            })
        }
    }
}

/// A parcel that arrived (or is going back) is not followed any more.
fn finish(store: &Store, id: &str, stage: Stage) {
    store.update(id, |p| {
        p.state = if stage == Stage::Returned {
            ParcelState::Stopped
        } else {
            ParcelState::Delivered
        };
        p.final_sent = true;
    });
}

/// One tick. Time is an argument; the store is changed as the lookups come in.
pub async fn tick<T: Track>(
    store: &Store,
    tracker: &T,
    now: DateTime<Utc>,
    cfg: &ScheduleCfg,
) -> Vec<OwnerMessage> {
    let local = now.with_timezone(&cfg.tz);
    let today = local.date_naive();
    let mut out = Vec::new();

    for p in store.list() {
        if p.state != ParcelState::Active
            || cadence(p.status.as_ref(), today, cfg.tz, &cfg.home) != Cadence::Hourly
        {
            continue;
        }
        if p.last_checked
            .is_some_and(|t| now - t < Duration::minutes(HOURLY_EVERY_MINUTES))
        {
            continue;
        }
        let Some(c) = check(store, tracker, &p, now).await else {
            continue;
        };
        let Ok(new) = &c.result else { continue };
        if new.stage.is_final() {
            finish(store, &c.parcel.id, new.stage);
            out.push(OwnerMessage {
                kind: MessageKind::Final,
                text: text::final_message(&c.parcel, new, cfg.tz),
                dedup_key: parcel_key(&c.parcel, new),
            });
        } else if changed(c.old.as_ref(), new) {
            out.push(OwnerMessage {
                kind: MessageKind::Change,
                text: text::change_message(&c.parcel, new, cfg.tz),
                dedup_key: parcel_key(&c.parcel, new),
            });
        }
    }

    let day = today.format("%Y-%m-%d").to_string();
    if local.hour() >= cfg.digest_hour && store.digest_sent().as_deref() != Some(day.as_str()) {
        out.extend(daily_pass(store, tracker, now, cfg, &day).await);
    }
    out
}

async fn daily_pass<T: Track>(
    store: &Store,
    tracker: &T,
    now: DateTime<Utc>,
    cfg: &ScheduleCfg,
    day: &str,
) -> Option<OwnerMessage> {
    let today = now.with_timezone(&cfg.tz).date_naive();
    let mut lines = Vec::new();
    for p in store
        .list()
        .into_iter()
        .filter(|p| p.state == ParcelState::Active)
    {
        let name = text::name(&p);
        if now - p.last_change.unwrap_or(p.added_at) >= Duration::days(cfg.stale_days) {
            store.update(&p.id, |q| q.state = ParcelState::Stopped);
            lines.push(format!(
                "• {name}: no change in {} days, so I've stopped tracking it.",
                cfg.stale_days
            ));
            continue;
        }
        let mut shown = p.clone();
        let mut failed = None;
        if cadence(p.status.as_ref(), today, cfg.tz, &cfg.home) == Cadence::Daily {
            match check(store, tracker, &p, now).await {
                Some(c) => {
                    shown = c.parcel;
                    failed = c.result.err().map(|e| text::failure_line(&e));
                }
                None => continue,
            }
        }
        let name = text::name(&shown);
        let line = match (&failed, &shown.status) {
            (Some(why), _) => format!("• {name}: {why}"),
            (None, Some(s)) if s.stage.is_final() => {
                finish(store, &shown.id, s.stage);
                format!(
                    "• {name}: {}. I've stopped tracking it.",
                    text::status_line(s, cfg.tz)
                )
            }
            (None, Some(s)) => {
                let hourly = cadence(Some(s), today, cfg.tz, &cfg.home) == Cadence::Hourly;
                format!(
                    "• {name}: {}{}",
                    text::status_line(s, cfg.tz),
                    if hourly { " (checking hourly now)" } else { "" }
                )
            }
            (None, None) => format!("• {name}: no status yet"),
        };
        lines.push(line);
    }
    store.set_digest_sent(day);
    if lines.is_empty() {
        return None;
    }
    Some(OwnerMessage {
        kind: MessageKind::Digest,
        text: format!("📦 Your parcels\n{}", lines.join("\n")),
        dedup_key: format!("parcel-digest:{day}"),
    })
}
