//! When a parcel is "near" the owner, and how often it is checked. Pure: no IO, no clock reads.

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A parcel's stage, as the scraper names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Pending,
    Registered,
    Accepted,
    InTransit,
    OutForDelivery,
    Delivered,
    Customs,
    FailedAttempt,
    ReadyForPickup,
    Returned,
    Exception,
    Unknown,
}

impl Stage {
    pub fn parse(s: &str) -> Stage {
        match s.trim() {
            "pending" => Stage::Pending,
            "registered" => Stage::Registered,
            "accepted" => Stage::Accepted,
            "in_transit" => Stage::InTransit,
            "out_for_delivery" => Stage::OutForDelivery,
            "delivered" => Stage::Delivered,
            "customs" => Stage::Customs,
            "failed_attempt" => Stage::FailedAttempt,
            "ready_for_pickup" => Stage::ReadyForPickup,
            "returned" => Stage::Returned,
            "exception" => Stage::Exception,
            _ => Stage::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Pending => "pending",
            Stage::Registered => "registered",
            Stage::Accepted => "accepted",
            Stage::InTransit => "in_transit",
            Stage::OutForDelivery => "out_for_delivery",
            Stage::Delivered => "delivered",
            Stage::Customs => "customs",
            Stage::FailedAttempt => "failed_attempt",
            Stage::ReadyForPickup => "ready_for_pickup",
            Stage::Returned => "returned",
            Stage::Exception => "exception",
            Stage::Unknown => "unknown",
        }
    }

    /// Delivered or on its way back: nothing more to follow.
    pub fn is_final(self) -> bool {
        matches!(self, Stage::Delivered | Stage::Returned)
    }
}

/// One scan in a parcel's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub time: Option<String>,
    pub description: String,
    pub location: Option<String>,
    pub stage: Stage,
}

/// What the carrier last said about a parcel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub carrier: String,
    pub stage: Stage,
    pub text: Option<String>,
    pub last_update: Option<String>,
    pub expected_delivery: Option<String>,
    /// The location of the newest event.
    pub location: Option<String>,
    pub destination_country: Option<String>,
    /// The scans, newest first, at most [`MAX_EVENTS`]. Lists saved before events were kept load empty.
    #[serde(default)]
    pub events: Vec<Event>,
}

/// The longest history kept per parcel.
pub const MAX_EVENTS: usize = 50;

/// Where the owner lives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Home {
    pub city: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Hourly,
    Daily,
    Stopped,
}

/// The date of an `expected_delivery`: an RFC 3339 instant is read in `tz`, anything else by its
/// leading `YYYY-MM-DD`.
pub fn expected_date(s: &str, tz: Tz) -> Option<NaiveDate> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&tz).date_naive());
    }
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

/// `city` occurs in `haystack` as a whole word, ignoring case.
fn has_word(haystack: &str, city: &str) -> bool {
    let (hay, city) = (haystack.to_lowercase(), city.trim().to_lowercase());
    if city.is_empty() {
        return false;
    }
    hay.match_indices(&city).any(|(i, m)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + m.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// The parcel is close enough to the owner to be worth an hourly look.
pub fn near_destination(s: &Status, today: NaiveDate, tz: Tz, home: &Home) -> bool {
    match s.stage {
        Stage::Delivered | Stage::Returned => return false,
        Stage::OutForDelivery | Stage::ReadyForPickup | Stage::FailedAttempt => return true,
        _ => {}
    }
    if s.expected_delivery
        .as_deref()
        .and_then(|e| expected_date(e, tz))
        == Some(today)
    {
        return true;
    }
    s.stage == Stage::InTransit
        && match (home.city.as_deref(), s.location.as_deref()) {
            (Some(city), Some(loc)) => has_word(loc, city),
            _ => false,
        }
}

pub fn cadence(status: Option<&Status>, today: NaiveDate, tz: Tz, home: &Home) -> Cadence {
    match status {
        Some(s) if s.stage.is_final() => Cadence::Stopped,
        Some(s) if near_destination(s, today, tz, home) => Cadence::Hourly,
        _ => Cadence::Daily,
    }
}

/// Stage, text or last update differ (no old status: changed).
pub fn changed(old: Option<&Status>, new: &Status) -> bool {
    old.is_none_or(|o| {
        o.stage != new.stage || o.text != new.text || o.last_update != new.last_update
    })
}
