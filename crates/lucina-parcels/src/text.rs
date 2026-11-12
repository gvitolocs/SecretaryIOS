//! What Lucina says about a parcel, in English.

use chrono::DateTime;
use chrono_tz::Tz;

use crate::client::TrackError;
use crate::policy::{expected_date, Stage, Status};
use crate::store::Parcel;

/// "poste-italiane" as "Poste Italiane".
pub fn carrier_name(id: &str) -> String {
    match id.trim().to_lowercase().as_str() {
        "postnord" => "PostNord".into(),
        "dhl" | "gls" | "ups" | "dpd" | "tnt" | "usps" => id.trim().to_uppercase(),
        "fedex" => "FedEx".into(),
        other => other
            .split(['-', '_', ' '])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

pub fn stage_name(s: Stage) -> &'static str {
    match s {
        Stage::Pending => "waiting for the carrier",
        Stage::Registered => "registered",
        Stage::Accepted => "accepted by the carrier",
        Stage::InTransit => "in transit",
        Stage::OutForDelivery => "out for delivery",
        Stage::Delivered => "delivered",
        Stage::Customs => "in customs",
        Stage::FailedAttempt => "delivery attempt failed",
        Stage::ReadyForPickup => "ready for pickup",
        Stage::Returned => "returned to the sender",
        Stage::Exception => "a problem was reported",
        Stage::Unknown => "status unknown",
    }
}

/// The label, else "<carrier> <number>" (no carrier: the number).
pub fn name(p: &Parcel) -> String {
    if let Some(l) = p.label.as_deref() {
        return l.to_owned();
    }
    let carrier = p
        .status
        .as_ref()
        .map(|s| s.carrier.as_str())
        .filter(|c| !c.is_empty())
        .or(p.carrier.as_deref());
    match carrier {
        Some(c) => format!("{} {}", carrier_name(c), p.number),
        None => p.number.clone(),
    }
}

fn clock(update: &str, tz: Tz) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(update.trim())
        .ok()?
        .with_timezone(&tz);
    Some(t.format("%-d %b %H:%M").to_string())
}

/// "<text> · <location> · <local time> · expected <date>".
pub fn status_line(s: &Status, tz: Tz) -> String {
    let mut parts = vec![s
        .text
        .clone()
        .unwrap_or_else(|| stage_name(s.stage).to_owned())];
    parts.extend(s.location.clone());
    parts.extend(s.last_update.as_deref().and_then(|u| clock(u, tz)));
    if let Some(d) = s.expected_delivery.as_deref() {
        parts.push(format!(
            "expected {}",
            expected_date(d, tz).map_or_else(|| d.to_owned(), |d| d.format("%-d %b").to_string())
        ));
    }
    parts.join(" · ")
}

/// The line for a lookup that failed.
pub fn failure_line(e: &TrackError) -> String {
    match e {
        TrackError::NotFound => "the carrier has no record of it yet".into(),
        other => format!("couldn't check ({other})"),
    }
}

pub fn change_message(p: &Parcel, s: &Status, tz: Tz) -> String {
    format!("📦 {}: {}", name(p), status_line(s, tz))
}

pub fn final_message(p: &Parcel, s: &Status, tz: Tz) -> String {
    let what = if s.stage == Stage::Returned {
        "is being returned to the sender"
    } else {
        "was delivered"
    };
    format!(
        "📦 {} {what} ({}). I've stopped tracking it.",
        name(p),
        status_line(s, tz)
    )
}
