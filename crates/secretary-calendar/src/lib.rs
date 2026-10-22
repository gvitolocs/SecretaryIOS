//! Calendar engine for Secretary.
//!
//! Every source (Google Calendar, ICS subscriptions, the iPhone's EventKit
//! snapshot) is reduced to the same [`Item`]s: single events, recurring
//! masters with an RRULE, and per-instance overrides or cancellations.
//! [`view::window`] expands them over a time window (DST-correct, in each
//! event's own time zone) and merges the sources into one sorted list of
//! [`Event`]s; [`view::free_slots`] finds gaps.
//!
//! - [`rrule`]: the RRULE subset (DAILY / WEEKLY / MONTHLY / YEARLY,
//!   INTERVAL, COUNT, UNTIL, BYDAY with ordinals, BYMONTHDAY, BYMONTH,
//!   BYSETPOS, WKST) plus EXDATE.
//! - [`ics`]: RFC 5545 text → items (VEVENT only; VALARM etc. ignored).
//! - [`google`]: Google Calendar API event JSON → items, and the
//!   incremental-sync merge.
//!
//! No I/O here: the server fetches and stores, this crate computes.

pub mod google;
pub mod ics;
pub mod model;
pub mod rrule;
pub mod view;

pub use model::{Event, Item, Moment, Source};
pub use view::{free_slots, window, SlotQuery, Window};

/// Upper bound on expanded instances per query (protects the server from a
/// pathological feed).
pub const MAX_INSTANCES: usize = 5000;
/// Longest window a query may ask for.
pub const MAX_WINDOW_DAYS: i64 = 400;

/// Map an IANA name, a `/vendor/…/Europe/Rome` style id or a common Windows
/// zone name (Outlook feeds) to a time zone.
pub fn parse_tz(name: &str) -> Option<chrono_tz::Tz> {
    let n = name.trim().trim_matches('"');
    if n.is_empty() {
        return None;
    }
    if let Ok(tz) = n.parse::<chrono_tz::Tz>() {
        return Some(tz);
    }
    // "/mozilla.org/20050126_1/Europe/Rome", "/citadel.org/.../Europe/Rome"
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
    for k in (1..=3).rev() {
        if parts.len() >= k {
            if let Ok(tz) = parts[parts.len() - k..].join("/").parse::<chrono_tz::Tz>() {
                return Some(tz);
            }
        }
    }
    let windows = [
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Romance Standard Time", "Europe/Paris"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("Central European Standard Time", "Europe/Warsaw"),
        ("GMT Standard Time", "Europe/London"),
        ("Greenwich Standard Time", "Atlantic/Reykjavik"),
        ("E. Europe Standard Time", "Europe/Chisinau"),
        ("FLE Standard Time", "Europe/Kiev"),
        ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"),
        ("Mountain Standard Time", "America/Denver"),
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("China Standard Time", "Asia/Shanghai"),
        ("India Standard Time", "Asia/Kolkata"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
        ("UTC", "UTC"),
        ("Coordinated Universal Time", "UTC"),
    ];
    windows
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(n))
        .and_then(|(_, iana)| iana.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tz_names() {
        assert_eq!(parse_tz("Europe/Rome"), Some(chrono_tz::Europe::Rome));
        assert_eq!(
            parse_tz("/mozilla.org/20050126_1/Europe/Rome"),
            Some(chrono_tz::Europe::Rome)
        );
        assert_eq!(
            parse_tz("W. Europe Standard Time"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(
            parse_tz("\"America/New_York\""),
            Some(chrono_tz::America::New_York)
        );
        assert_eq!(parse_tz("Mars/Olympus"), None);
        assert_eq!(parse_tz(""), None);
    }
}
