//! A parcel as the apps see it (the `Parcel` of the Parcels API, `docs/lucina-rust/parcels.md`):
//! camelCase JSON, times as RFC 3339 UTC strings. Pure: time is an argument.

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::policy::{cadence, near_destination, Cadence};
use crate::schedule::ScheduleCfg;
use crate::store::{Parcel, ParcelState};

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The local instant of `hour:00` on `date`; an hour that doesn't exist that day (the clocks
/// jumped over it) is the next one that does.
fn local_instant(tz: Tz, date: NaiveDate, hour: u32) -> Option<DateTime<Utc>> {
    (hour.min(23)..24).find_map(|h| {
        tz.from_local_datetime(&date.and_hms_opt(h, 0, 0)?)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    })
}

fn day_key(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// The schedule a parcel is on now.
pub fn schedule_of(p: &Parcel, now: DateTime<Utc>, cfg: &ScheduleCfg) -> Cadence {
    if p.state != ParcelState::Active {
        return Cadence::Stopped;
    }
    let today = now.with_timezone(&cfg.tz).date_naive();
    cadence(p.status.as_ref(), today, cfg.tz, &cfg.home)
}

/// Whether the parcel is close to the owner (never once it is done).
pub fn is_near(p: &Parcel, now: DateTime<Utc>, cfg: &ScheduleCfg) -> bool {
    if p.state != ParcelState::Active {
        return false;
    }
    let today = now.with_timezone(&cfg.tz).date_naive();
    p.status
        .as_ref()
        .is_some_and(|s| near_destination(s, today, cfg.tz, &cfg.home))
}

/// When Lucina looks at the parcel next. Hourly: an hour after the last check (now when it was
/// never checked). Daily: the next digest-hour instant, today's when the digest of `digest_sent`
/// (the local date of the last daily pass) is still to come. Stopped: never.
pub fn next_check(
    p: &Parcel,
    now: DateTime<Utc>,
    cfg: &ScheduleCfg,
    digest_sent: Option<&str>,
) -> Option<DateTime<Utc>> {
    match schedule_of(p, now, cfg) {
        Cadence::Stopped => None,
        Cadence::Hourly => Some(
            p.last_checked
                .map_or(now, |t| t + Duration::minutes(60)),
        ),
        Cadence::Daily => {
            let today = now.with_timezone(&cfg.tz).date_naive();
            let mut date = today;
            for _ in 0..3 {
                let done = digest_sent == Some(day_key(date).as_str());
                if !done {
                    if let Some(at) = local_instant(cfg.tz, date, cfg.digest_hour) {
                        // Past the hour with the day's digest still unsent: due on the next tick.
                        return Some(at.max(now));
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
    }
}

/// The contract's `Parcel`.
pub fn view(
    p: &Parcel,
    now: DateTime<Utc>,
    cfg: &ScheduleCfg,
    digest_sent: Option<&str>,
) -> Value {
    let schedule = match schedule_of(p, now, cfg) {
        Cadence::Hourly => "hourly",
        Cadence::Daily => "daily",
        Cadence::Stopped => "stopped",
    };
    let state = match p.state {
        ParcelState::Active => "active",
        ParcelState::Delivered => "delivered",
        ParcelState::Stopped => "stopped",
    };
    let status = p.status.as_ref().map(|s| {
        json!({
            "stage": s.stage.as_str(),
            "text": s.text,
            "lastUpdate": s.last_update,
            "expectedDelivery": s.expected_delivery,
            "location": s.location,
            "destinationCountry": s.destination_country,
        })
    });
    let events: Vec<Value> = p
        .status
        .as_ref()
        .map(|s| {
            s.events
                .iter()
                .map(|e| {
                    json!({
                        "time": e.time,
                        "description": e.description,
                        "location": e.location,
                        "stage": e.stage.as_str(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": p.id,
        "number": p.number,
        "carrier": p.carrier.clone().or_else(|| p.status.as_ref().map(|s| s.carrier.clone()).filter(|c| !c.is_empty())),
        "label": p.label,
        "state": state,
        "schedule": schedule,
        "near": is_near(p, now, cfg),
        "addedAt": rfc3339(p.added_at),
        "lastChecked": p.last_checked.map(rfc3339),
        "nextCheck": next_check(p, now, cfg, digest_sent).map(rfc3339),
        "lastError": p.last_error,
        "status": status,
        "events": events,
    })
}

/// Active parcels first, then the rest; newest change first within each (a parcel never
/// checked counts from when it was added).
pub fn ordered(mut parcels: Vec<Parcel>) -> Vec<Parcel> {
    parcels.sort_by_key(|p| {
        (
            p.state != ParcelState::Active,
            std::cmp::Reverse(p.last_change.unwrap_or(p.added_at)),
        )
    });
    parcels
}
