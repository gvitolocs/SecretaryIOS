//! What the tool, the chat turn and the CLI share: add a parcel and look it up once, refresh one.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::client::{Scraper, TrackError};
use crate::policy::{cadence, Cadence, Status};
use crate::schedule::{check, ScheduleCfg};
use crate::store::{normalize_number, Parcel, ParcelState, Store};
use crate::Track;

/// A parcel looked up less than this many minutes ago is not looked up again on request.
pub const FRESH_MINUTES: i64 = 10;

pub struct Service {
    pub store: Arc<Store>,
    pub scraper: Scraper,
    pub cfg: ScheduleCfg,
}

/// A parcel added and looked up once.
pub struct Added {
    pub parcel: Parcel,
    /// `false`: the number was already tracked.
    pub created: bool,
    /// The first lookup; a failure keeps the parcel, except a number the carrier rejects.
    pub lookup: Result<Status, TrackError>,
    /// Whether the parcel is in the list now (`false`: a self-test, or a number the carrier rejects).
    pub saved: bool,
}

impl Service {
    pub fn new(store: Arc<Store>, scraper: Scraper, cfg: ScheduleCfg) -> Self {
        Self {
            store,
            scraper,
            cfg,
        }
    }

    /// Tracks `number` once. Without a carrier the scraper's detection supplies one, but only a
    /// sure one. `save: false` (a self-test) looks it up without touching the list.
    pub async fn add(
        &self,
        number: &str,
        carrier: Option<&str>,
        label: Option<&str>,
        now: DateTime<Utc>,
        save: bool,
    ) -> Added {
        let number = normalize_number(number);
        let detected = match carrier {
            Some(_) => None,
            None => self
                .scraper
                .detect(&number)
                .await
                .ok()
                .and_then(|d| d.sure_carrier().map(str::to_owned)),
        };
        let carrier = carrier.map(str::to_owned).or(detected);
        if !save {
            let lookup = self.scraper.track(&number, carrier.as_deref()).await;
            let parcel = Parcel {
                id: "pc-unsaved".into(),
                number,
                carrier,
                label: label.map(str::to_owned),
                added_at: now,
                state: ParcelState::Active,
                status: lookup.as_ref().ok().cloned(),
                last_checked: Some(now),
                last_change: None,
                last_error: None,
                final_sent: false,
            };
            return Added {
                parcel,
                created: true,
                lookup,
                saved: false,
            };
        }
        let added = self.store.add(&number, carrier.as_deref(), label, now);
        let lookup = self.refresh(&added.parcel, now).await;
        let parcel = self.store.get(&added.parcel.id).unwrap_or(added.parcel);
        // A number the carrier rejects is an answer, not an outage: don't follow it.
        let rejected = added.created && lookup == Err(TrackError::Invalid);
        if rejected {
            self.store.remove(&parcel.id);
        }
        Added {
            parcel,
            created: added.created,
            lookup,
            saved: !rejected,
        }
    }

    /// Looks the parcel up and records the outcome. A parcel that turns out delivered stops.
    pub async fn refresh(&self, p: &Parcel, now: DateTime<Utc>) -> Result<Status, TrackError> {
        match check(&self.store, &self.scraper, p, now).await {
            Some(c) => {
                if let Ok(s) = &c.result {
                    if s.stage.is_final() && p.state == ParcelState::Active {
                        self.store.update(&p.id, |q| {
                            q.state = if s.stage == crate::Stage::Returned {
                                ParcelState::Stopped
                            } else {
                                ParcelState::Delivered
                            };
                            q.final_sent = true;
                        });
                    }
                }
                c.result
            }
            None => Err(TrackError::Unavailable("the parcel was removed".into())),
        }
    }

    /// The parcel as stored after a lookup, unless it was checked under [`FRESH_MINUTES`] ago or
    /// is no longer followed (then as stored). The error is the lookup's, when it failed.
    /// `None`: no such id.
    pub async fn refresh_unless_fresh(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Option<(Parcel, Option<TrackError>)> {
        let p = self.store.get_id(id)?;
        let fresh = p
            .last_checked
            .is_some_and(|t| now - t < chrono::Duration::minutes(FRESH_MINUTES));
        if fresh || p.state != ParcelState::Active {
            return Some((p, None));
        }
        let failed = self.refresh(&p, now).await.err();
        Some((self.store.get(&p.id).unwrap_or(p), failed))
    }

    /// The Parcels API's `Parcel` JSON for one parcel.
    pub fn view(&self, p: &Parcel, now: DateTime<Utc>) -> serde_json::Value {
        crate::view::view(p, now, &self.cfg, self.store.digest_sent().as_deref())
    }

    pub fn cadence_of(&self, p: &Parcel, now: DateTime<Utc>) -> Cadence {
        if p.state != ParcelState::Active {
            return Cadence::Stopped;
        }
        let today = now.with_timezone(&self.cfg.tz).date_naive();
        cadence(p.status.as_ref(), today, self.cfg.tz, &self.cfg.home)
    }
}
