//! The English chat/notification text (`format.js`). Offer dates are shown
//! in Europe/Copenhagen (the Danish sources' clock), as on Node.

use chrono::TimeZone;
use chrono_tz::Tz;
use serde_json::Value;

use crate::engine::{LunchResult, MatchResult};
use crate::js::{self, display, parse_date, utf16_len, utf16_prefix};
use crate::sources::{Deal, DealSource};
use crate::validity::Status;

pub const ZONE: Tz = chrono_tz::Europe::Copenhagen;

pub fn money(value: f64) -> String {
    format!("{} DKK", js::to_fixed(value, 2))
}

fn local(ms: i64, zone: Tz) -> Option<chrono::DateTime<Tz>> {
    zone.timestamp_millis_opt(ms).single()
}

/// `YYYY-MM-DD` in `zone` (survives Copenhagen's DST change).
pub fn local_date(ms: i64, zone: Tz) -> String {
    local(ms, zone).map_or_else(
        || "Invalid Date".into(),
        |t| t.format("%Y-%m-%d").to_string(),
    )
}

/// `Sat 24/10` in `zone`.
pub fn day_label(ms: i64, zone: Tz) -> String {
    local(ms, zone).map_or_else(
        || "Invalid Date".into(),
        |t| t.format("%a %d/%m").to_string(),
    )
}

/// `HH:MM` (24 h) in `zone`.
pub fn clock(ms: i64, zone: Tz) -> String {
    local(ms, zone).map_or_else(|| "Invalid Date".into(), |t| t.format("%H:%M").to_string())
}

/// `new Date(value)` of a raw date value.
fn time_of(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::String(s)) => parse_date(s),
        Some(Value::Number(n)) => n.as_f64().filter(|n| n.is_finite()).map(|n| n as i64),
        _ => None,
    }
}

fn label_of(value: Option<&Value>) -> String {
    time_of(value).map_or_else(|| "Invalid Date".into(), |ms| day_label(ms, ZONE))
}

/// `until today 23:59` for a clearance ending today, else `until Sat 24/10`.
pub fn until(deal: &Deal, now_ms: i64) -> String {
    let end = time_of(deal.valid_to.as_ref());
    if deal.source == DealSource::FoodWaste {
        if let Some(end) = end {
            if local_date(end, ZONE) == local_date(now_ms, ZONE) {
                return format!("until today {}", clock(end, ZONE));
            }
        }
    }
    format!("until {}", label_of(deal.valid_to.as_ref()))
}

/// One compact offer with branch, availability and truthful price provenance.
pub fn format_deal(deal: &Deal, now_ms: i64) -> String {
    let was = if deal.normal_price_dkk.unwrap_or(0.0) > deal.price_dkk {
        let pct = deal
            .discount_pct
            .map(|p| format!(", -{}%", js::num_to_string(js::round(p))))
            .unwrap_or_default();
        format!(
            " (was {}{pct})",
            money(deal.normal_price_dkk.unwrap_or(0.0))
        )
    } else {
        String::new()
    };
    let stock = deal
        .stock
        .map(|s| {
            let amount = if deal.stock_unit.as_ref().and_then(Value::as_str) == Some("kg") {
                format!(
                    "{} kg",
                    js::num_to_string(js::string_to_number(&js::to_fixed(s, 2)))
                )
            } else {
                js::num_to_string(s)
            };
            format!("{amount} left")
        })
        .unwrap_or_default();
    let clearance = if deal.source == DealSource::FoodWaste {
        "clearance"
    } else {
        ""
    };
    let availability = [clearance, stock.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let place = if deal.source == DealSource::FoodWaste {
        display(deal.store.name.as_ref())
    } else {
        display(deal.chain.as_ref())
    };
    let unit = deal
        .unit_price
        .as_ref()
        .filter(|u| !u.is_empty())
        .map(|u| format!(" ({u})"))
        .unwrap_or_default();
    let more = deal
        .more_stores
        .filter(|m| *m != 0)
        .map(|m| format!(" +{m} stores"))
        .unwrap_or_default();
    let head = format!(
        "{}: {}{was}{unit} — {place}, {}, {} km{more}",
        display(deal.name.as_ref()),
        money(deal.price_dkk),
        display(deal.store.street.as_ref()),
        js::to_fixed(deal.store.distance_m / 1000.0, 1),
    );
    let from = if deal.status == Status::Upcoming {
        format!("from {}", label_of(deal.valid_from.as_ref()))
    } else {
        String::new()
    };
    [head, availability, from, until(deal, now_ms)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `formatMatches` options. `misses: None` lists every item without an
/// offer as `No deal:`; notifications pass their own (possibly empty) list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    pub active_only: bool,
    pub max_length: usize,
    pub misses: Option<Vec<String>>,
    pub omitted: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            active_only: false,
            max_length: 3500,
            misses: None,
            omitted: 0,
        }
    }
}

/// Bound messages by dropping whole offers; include misses on one final line.
pub fn format_matches(result: &MatchResult, options: &FormatOptions) -> String {
    let now_ms = parse_date(&result.now).unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    let mut length = 0usize; // UTF-16 length of lines.join('\n')
    let mut misses = options.misses.clone().unwrap_or_default();
    for item in &result.items {
        let deals: Vec<&Deal> = item
            .deals
            .iter()
            .filter(|d| !options.active_only || d.status == Status::Active)
            .collect();
        if deals.is_empty() {
            if options.misses.is_none() {
                misses.push(item.name.clone());
            }
            continue;
        }
        for deal in deals {
            let line = format!("{}: {}", item.name, format_deal(deal, now_ms));
            let joined = length + usize::from(!lines.is_empty()) + utf16_len(&line);
            if (joined as i64) <= options.max_length as i64 - 180 {
                lines.push(line);
                length = joined;
            }
        }
    }
    if lines.is_empty() {
        lines.push("No matching deals nearby.".into());
    }
    if !misses.is_empty() {
        lines.push(format!("No deal: {}.", misses.join(", ")));
    }
    if options.omitted != 0 {
        lines.push(format!(
            "+{} more item{}",
            options.omitted,
            if options.omitted == 1 { "" } else { "s" }
        ));
    }
    if !result.failures.is_empty() {
        lines.push(format!("Unavailable: {}.", result.failures.join(", ")));
    }
    utf16_prefix(&lines.join("\n"), options.max_length)
}

pub fn format_lunch(result: &LunchResult) -> String {
    if result.suggestions.is_empty() {
        let unavailable = if result.failures.is_empty() {
            String::new()
        } else {
            format!(" Unavailable: {}.", result.failures.join(", "))
        };
        return format!(
            "No healthy lunch combination within {} nearby.{unavailable}",
            money(result.budget)
        );
    }
    let now_ms = parse_date(&result.now).unwrap_or(0);
    result
        .suggestions
        .iter()
        .map(|s| {
            let items = s
                .items
                .iter()
                .map(|d| {
                    format!(
                        "{} {}",
                        display(d.deal.name.as_ref()),
                        money(d.deal.price_dkk)
                    )
                })
                .collect::<Vec<_>>()
                .join(" + ");
            let save = if s.saving > 0.0 {
                format!(" · save {}", money(s.saving))
            } else {
                String::new()
            };
            let clearance = if s
                .items
                .iter()
                .all(|d| d.deal.source == DealSource::FoodWaste)
            {
                " · clearance"
            } else {
                ""
            };
            let first = Deal {
                valid_to: s.valid_to.clone(),
                ..s.items[0].deal.clone()
            };
            format!(
                "{items}\nTotal {}{save} — {}, {}, {} km{clearance} · {}",
                money(s.total),
                display(s.store.name.as_ref()),
                display(s.store.street.as_ref()),
                js::to_fixed(s.store.distance_m / 1000.0, 1),
                until(&first, now_ms)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The `grocery-deals map` summary: a header and one line per store.
pub fn format_map_summary(map: &crate::engine::MapData) -> String {
    let unavailable = if map.failures.is_empty() {
        String::new()
    } else {
        format!(" (unavailable: {})", map.failures.join(", "))
    };
    let mut lines = vec![format!(
        "Deals map: {} stores within {} m of {}{unavailable}",
        map.stores.len(),
        js::num_to_string(map.radius_m),
        map.home_label.as_deref().unwrap_or("home"),
    )];
    for s in &map.stores {
        lines.push(format!(
            "{} — {} m · {} clearances · {} matches · {} flyer offers",
            display(s.name.as_ref()),
            s.distance_m,
            s.clearances.len(),
            s.list_matches.len(),
            s.flyer_offers.len()
        ));
    }
    lines.join("\n")
}
