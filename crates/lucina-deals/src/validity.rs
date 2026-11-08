//! Offer validity (`validity.js`): end times are exclusive, including
//! food-waste boundaries; offers starting within three days are upcoming.

use serde::Serialize;

use crate::js::parse_date;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Upcoming,
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Upcoming => "upcoming",
            Status::Expired => "expired",
        }
    }
}

pub fn validity(valid_from: Option<&str>, valid_to: Option<&str>, now_ms: i64) -> Status {
    let (Some(start), Some(end)) = (
        valid_from.and_then(parse_date),
        valid_to.and_then(parse_date),
    ) else {
        return Status::Expired;
    };
    if end <= now_ms || end <= start {
        return Status::Expired;
    }
    if start <= now_ms {
        return Status::Active;
    }
    if start <= now_ms + 3 * 86_400_000 {
        Status::Upcoming
    } else {
        Status::Expired
    }
}
