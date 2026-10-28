//! Agenda answers (`docs/lucina-message-format.md`, "Agenda answers"): a calendar question the
//! calendar answered becomes one short sentence plus one card per event, never a wall of text.
//!
//! - The first three events with a location are `place` cards (kind `event`: time, title,
//!   address, travel time when known); the others are `card` blocks (time and location in the
//!   subtitle). Past the block limit, the rest is one `list`.
//! - Every place gets a "Naviga" `route` button to the in-app place map (`/map/place?…`, which
//!   iOS also opens as `secretary://open/map/place?…`), never an external maps URL.
//! - Titles lose the calendar noise ([`clean_title`]).
//! - Plain surfaces (Telegram, MCP callers, the timeline text) get one compact line per event.

use chrono::DateTime;
use chrono_tz::Tz;
use lucina_intent::calendar::day_label;
use lucina_intent::calendar_event::clean_title;
use lucina_store::rich::{
    normalize_action, normalize_block, travel_text, valid_map_route, RichMessage, FORMAT,
    MAX_ACTIONS, MAX_BLOCKS, MAX_PLACES,
};
use serde_json::{json, Value};

/// The sentence above the cards, in the question's language.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Agenda {
    pub sentence: String,
    pub lang: &'static str,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().map(str::trim).unwrap_or("")
}

fn local(iso: &str, tz: Tz) -> Option<DateTime<Tz>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.with_timezone(&tz))
}

/// The event's first local day, `YYYY-MM-DD`.
fn day_of(e: &Value, tz: Tz) -> String {
    match local(text(e, "start"), tz) {
        Some(t) if !e["allDay"].as_bool().unwrap_or(false) => t.format("%Y-%m-%d").to_string(),
        _ => text(e, "start").chars().take(10).collect(),
    }
}

/// "17:00–20:00", or "tutto il giorno" / "all day" / "hele dagen".
fn time_range(e: &Value, tz: Tz, lang: &str) -> String {
    if e["allDay"].as_bool().unwrap_or(false) {
        return match lang {
            "it" => "tutto il giorno",
            "da" => "hele dagen",
            _ => "all day",
        }
        .into();
    }
    let hm = |k: &str| local(text(e, k), tz).map(|t| t.format("%H:%M").to_string());
    match (hm("start"), hm("end")) {
        (Some(a), Some(b)) => format!("{a}–{b}"),
        (Some(a), None) => a,
        _ => String::new(),
    }
}

fn multi_day(events: &[Value], tz: Tz) -> bool {
    let mut days = events.iter().map(|e| day_of(e, tz));
    let first = days.next();
    days.any(|d| Some(d) != first)
}

/// The time line of a card: "17:00–20:00", with the day in front when the agenda spans days.
fn when_label(e: &Value, tz: Tz, lang: &str, multi: bool) -> String {
    let range = time_range(e, tz, lang);
    if multi {
        format!("{} · {range}", day_label(&day_of(e, tz), lang))
    } else {
        range
    }
}

fn overflow(events: &[Value], tz: Tz, lang: &str, multi: bool) -> Value {
    let title = match lang {
        "it" => format!("Altri {}", events.len()),
        "da" => format!("{} mere", events.len()),
        _ => format!("{} more", events.len()),
    };
    let items: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({"emoji": "📅", "text": format!("**{}** {}", when_label(e, tz, lang, multi), clean_title(text(e, "title"))),
                   "meta": text(e, "location")})
        })
        .collect();
    json!({"type": "list", "title": title, "items": items})
}

/// The cards, in calendar order.
pub fn blocks(events: &[Value], tz: Tz, lang: &str) -> Vec<Value> {
    let multi = multi_day(events, tz);
    let mut out = Vec::new();
    let mut places = 0;
    for (i, e) in events.iter().enumerate() {
        if out.len() == MAX_BLOCKS - 1 && events.len() - i > 1 {
            out.extend(normalize_block(&overflow(&events[i..], tz, lang, multi)));
            break;
        }
        let title = clean_title(text(e, "title"));
        let location = text(e, "location");
        let block = if !location.is_empty() && places < MAX_PLACES {
            places += 1;
            json!({"type": "place", "kind": "event", "title": title, "address": location,
                   "when": e["start"], "end": e["end"], "eventId": e["id"]})
        } else {
            let when = when_label(e, tz, lang, multi);
            let subtitle = match (when.is_empty(), location.is_empty()) {
                (_, true) => when,
                (true, false) => location.to_owned(),
                (false, false) => format!("{when} · {location}"),
            };
            json!({"type": "card", "emoji": "📅", "title": title, "subtitle": subtitle, "tone": "info"})
        };
        out.extend(normalize_block(&block));
    }
    out
}

/// RFC 3986 unreserved bytes stay, everything else is `%XX` (spaces too: iOS does not read `+`).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The in-app place map of a place block (`/map/place?…`); an over-long query drops its
/// optional parts (event id, end, address) before giving up.
pub fn map_route(place: &Value) -> Option<String> {
    route_with(place, false)
}

/// The in-app navigation to a place (`/map/place?navigate=1&…`): the app opens its map already
/// navigating, never another maps app.
pub fn navigate_route(place: &Value) -> Option<String> {
    route_with(place, true)
}

fn route_with(place: &Value, navigate: bool) -> Option<String> {
    let mut keys = vec!["lat", "lng", "title", "when", "address", "end", "eventId"];
    loop {
        // `navigate=1` leads, so trimming an over-long query never drops it.
        let query: Vec<String> = navigate
            .then(|| "navigate=1".to_owned())
            .into_iter()
            .chain(keys.iter().filter_map(|k| match &place[*k] {
                Value::Number(n) => Some(format!("{k}={n}")),
                Value::String(s) if !s.trim().is_empty() => Some(format!("{k}={}", encode(s))),
                _ => None,
            }))
            .collect();
        let path = format!("/map/place?{}", query.join("&"));
        if valid_map_route(&path) {
            return Some(path);
        }
        if keys.len() <= 4 {
            return None;
        }
        keys.pop();
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!(
            "{}…",
            s.chars().take(max - 1).collect::<String>().trim_end()
        )
    }
}

/// One "Naviga" button per place card: the in-app navigation, never Apple or Google Maps.
pub fn navigate_actions(blocks: &[Value], lang: &str) -> Vec<Value> {
    let label = match lang {
        "en" => "Navigate",
        "da" => "Navigér",
        _ => "Naviga",
    };
    let places: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "place").collect();
    places
        .iter()
        .take(MAX_ACTIONS)
        .enumerate()
        .filter_map(|(i, b)| {
            let label = if places.len() == 1 {
                label.to_owned()
            } else {
                clip(&format!("{label} · {}", text(b, "title")), 40)
            };
            let path = navigate_route(b)?;
            normalize_action(
                &json!({"id": format!("nav-{}", i + 1), "label": label, "emoji": "🧭",
                        "kind": "route", "payload": {"path": path}}),
                i,
            )
        })
        .collect()
}

/// The agenda as a v1 message (the place cards still without coordinates and travel times:
/// the caller geocodes, adds travel times, then [`with_navigation`]).
pub fn message(agenda: &Agenda, events: &[Value], tz: Tz) -> RichMessage {
    RichMessage {
        format: FORMAT.into(),
        bubbles: vec![agenda.sentence.clone()],
        lang: Some(agenda.lang.to_owned()),
        blocks: blocks(events, tz, agenda.lang),
        actions: Vec::new(),
    }
}

/// The "Naviga" buttons, once the places have their coordinates.
pub fn with_navigation(m: &mut RichMessage) {
    let lang = m.lang.clone().unwrap_or_default();
    m.actions = navigate_actions(&m.blocks, &lang);
}

/// The compact text for plain surfaces: the sentence, then one line per event (grouped by day
/// when the agenda spans days), with the place and the travel time.
pub fn plain_text(agenda: &Agenda, events: &[Value], blocks: &[Value], tz: Tz) -> String {
    let lang = agenda.lang;
    let multi = multi_day(events, tz);
    let mut lines = vec![agenda.sentence.clone()];
    let mut day = String::new();
    for e in events {
        if multi && day_of(e, tz) != day {
            day = day_of(e, tz);
            lines.push(String::new());
            let label = day_label(&day, lang);
            let mut c = label.chars();
            lines.push(
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default(),
            );
        } else if lines.len() == 1 {
            lines.push(String::new());
        }
        let title = clean_title(text(e, "title"));
        let mut line = format!("• {} {title}", time_range(e, tz, lang));
        let location = text(e, "location");
        if !location.is_empty() {
            line.push_str(&format!(" — {location}"));
        }
        let travel = blocks
            .iter()
            .find(|b| b["type"] == "place" && b["eventId"] == e["id"] && !e["id"].is_null())
            .and_then(travel_text);
        if let Some(t) = travel {
            line.push_str(&format!(" · {t}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Europe::Copenhagen;

    /// The owner's real Tuesday: a timetable lecture, a meeting without a place, the Hack Night.
    fn tuesday() -> Vec<Value> {
        vec![
            json!({"id": "ics:au:1", "title": "Security and Privacy (Aarhus Universitet - Personal timetable: 810677)",
                   "calendar": "Aarhus Universitet - Personal timetable: 810677",
                   "start": "2026-10-27T07:15:00.000Z", "end": "2026-10-27T09:00:00.000Z", "allDay": false,
                   "location": "5789-003B Seminarrum"}),
            json!({"id": "google:standup", "title": "Standup vitologiuseppe17@gmail.com", "calendar": "vitologiuseppe17@gmail.com",
                   "start": "2026-10-27T11:00:00.000Z", "end": "2026-10-27T11:15:00.000Z", "allDay": false}),
            json!({"id": "iphone:42", "title": "Hack Night: Rust meets AI", "calendar": "vitologiuseppe17@gmail.com",
                   "start": "2026-10-27T16:00:00.000Z", "end": "2026-10-27T19:00:00.000Z", "allDay": false,
                   "location": "Folkehuset Møllestien"}),
        ]
    }

    fn agenda() -> Agenda {
        Agenda {
            sentence: "Domani hai 3 impegni 📅".into(),
            lang: "it",
        }
    }

    #[test]
    fn an_agenda_is_one_sentence_and_one_card_per_event() {
        let mut m = message(&agenda(), &tuesday(), TZ);
        m.blocks[0]["lat"] = json!(56.1685);
        m.blocks[0]["lng"] = json!(10.2034);
        with_navigation(&mut m);
        assert_eq!(m.bubbles, vec!["Domani hai 3 impegni 📅"]);
        assert_eq!(
            m.blocks,
            vec![
                json!({"type": "place", "kind": "event", "title": "Security and Privacy",
                       "address": "5789-003B Seminarrum", "lat": 56.1685, "lng": 10.2034,
                       "when": "2026-10-27T07:15:00.000Z", "end": "2026-10-27T09:00:00.000Z", "eventId": "ics:au:1"}),
                json!({"type": "card", "emoji": "📅", "title": "Standup", "subtitle": "12:00–12:15", "tone": "info"}),
                json!({"type": "place", "kind": "event", "title": "Hack Night: Rust meets AI",
                       "address": "Folkehuset Møllestien", "when": "2026-10-27T16:00:00.000Z",
                       "end": "2026-10-27T19:00:00.000Z", "eventId": "iphone:42"}),
            ]
        );
        let paths: Vec<&str> = m
            .actions
            .iter()
            .map(|a| a["payload"]["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "/map/place?navigate=1&lat=56.1685&lng=10.2034&title=Security%20and%20Privacy&when=2026-10-27T07%3A15%3A00.000Z&address=5789-003B%20Seminarrum&end=2026-10-27T09%3A00%3A00.000Z&eventId=ics%3Aau%3A1",
                "/map/place?navigate=1&title=Hack%20Night%3A%20Rust%20meets%20AI&when=2026-10-27T16%3A00%3A00.000Z&address=Folkehuset%20M%C3%B8llestien&end=2026-10-27T19%3A00%3A00.000Z&eventId=iphone%3A42",
            ]
        );
        assert_eq!(m.actions[0]["label"], "Naviga · Security and Privacy");
        assert_eq!(m.actions[0]["kind"], "route");
        // Never an external maps link, never the noise, never a 📍 list in the text.
        let all = serde_json::to_string(&m).unwrap();
        for bad in [
            "maps.apple.com",
            "google.com/maps",
            "timetable",
            "@gmail.com",
            "📍",
        ] {
            assert!(!all.contains(bad), "{bad}");
        }
    }

    #[test]
    fn plain_surfaces_get_one_compact_line_per_event() {
        let mut m = message(&agenda(), &tuesday(), TZ);
        m.blocks[2]["travelMin"] = json!(18);
        m.blocks[2]["travelMode"] = json!("transit");
        let m = RichMessage {
            blocks: m.blocks.iter().filter_map(normalize_block).collect(),
            ..m
        };
        assert_eq!(
            plain_text(&agenda(), &tuesday(), &m.blocks, TZ),
            "Domani hai 3 impegni 📅\n\n\
             • 08:15–10:00 Security and Privacy — 5789-003B Seminarrum\n\
             • 12:00–12:15 Standup\n\
             • 17:00–20:00 Hack Night: Rust meets AI — Folkehuset Møllestien · 🚌 18 min"
        );
    }

    #[test]
    fn a_week_spans_days_and_overflows_into_a_list() {
        let events: Vec<Value> = (0..10)
            .map(|i| {
                json!({"id": format!("g:{i}"), "title": format!("Event {i}"),
                       "start": format!("2026-10-{:02}T08:00:00.000Z", 26 + i / 3),
                       "end": format!("2026-10-{:02}T09:00:00.000Z", 26 + i / 3), "allDay": false,
                       "location": if i % 2 == 0 { "Aarhus" } else { "" }})
            })
            .collect();
        let a = Agenda {
            sentence: "This week you have 10 events 📅".into(),
            lang: "en",
        };
        let b = blocks(&events, TZ, "en");
        assert_eq!(b.len(), MAX_BLOCKS);
        assert_eq!(
            b.iter().filter(|b| b["type"] == "place").count(),
            MAX_PLACES
        );
        assert_eq!(b[1]["subtitle"], "Monday 26 October · 09:00–10:00");
        assert_eq!(
            b[6]["subtitle"],
            "Wednesday 28 October · 09:00–10:00 · Aarhus"
        );
        assert_eq!(b[7]["type"], "list");
        assert_eq!(b[7]["title"], "3 more");
        assert_eq!(b[7]["items"].as_array().unwrap().len(), 3);
        let text = plain_text(&a, &events, &b, TZ);
        assert!(text.contains("\n\nMonday 26 October\n• 09:00–10:00 Event 0 — Aarhus\n"));
        assert_eq!(text.matches('•').count(), 10);
        assert_eq!(navigate_actions(&b, "en").len(), MAX_PLACES);
    }

    #[test]
    fn an_overlong_title_drops_optional_route_parts() {
        let place = json!({"type": "place", "title": "x".repeat(120), "address": "y".repeat(200),
                           "when": "2026-10-27T16:00:00.000Z", "end": "2026-10-27T19:00:00.000Z",
                           "eventId": "z".repeat(200)});
        let path = map_route(&place).unwrap();
        assert!(path.len() <= lucina_store::rich::MAX_MAP_ROUTE);
        assert!(path.contains("title=") && path.contains("when=") && !path.contains("eventId="));
        let nav = navigate_route(&place).unwrap();
        assert!(
            nav.starts_with("/map/place?navigate=1&title=")
                && nav.len() <= lucina_store::rich::MAX_MAP_ROUTE
        );
    }

    #[test]
    fn naviga_starts_the_in_app_navigation() {
        let b = vec![
            json!({"type": "place", "kind": "event", "title": "Security and Privacy",
                            "address": "5789-003B Seminarrum", "lat": 56.172282, "lng": 10.192006,
                            "when": "2026-11-02T07:00:00.000Z"}),
        ];
        let actions = navigate_actions(&b, "it");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["label"], "Naviga");
        let path = actions[0]["payload"]["path"].as_str().unwrap();
        assert!(path.starts_with(
            "/map/place?navigate=1&lat=56.172282&lng=10.192006&title=Security%20and%20Privacy"
        ));
        assert!(!path.contains("maps.apple") && !path.contains("google"));
        assert_eq!(
            map_route(&b[0]).unwrap().contains("navigate"),
            false,
            "the card itself only opens the map"
        );
    }
}
