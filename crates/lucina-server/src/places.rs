//! Place blocks for calendar answers (`docs/lucina-message-format.md`, `place`):
//! the events the turn read from the calendar that have a location and that the
//! reply names get a map card, whether or not the composer made one.

use lucina_store::rich::{normalize_block, RichMessage, MAX_BLOCKS, MAX_PLACES};
use serde_json::{json, Value};

fn text(v: &Value) -> &str {
    v.as_str().map(str::trim).unwrap_or("")
}

/// The place block of a calendar event (secretary-server shape: `id`,
/// `title`, `start`, `end`, `location`), if the event has a location.
pub fn event_place(event: &Value) -> Option<Value> {
    let address = text(&event["location"]);
    if address.is_empty() {
        return None;
    }
    normalize_block(&json!({
        "type": "place", "kind": "event", "title": event["title"], "address": address,
        "when": event["start"], "end": event["end"], "eventId": event["id"],
    }))
}

/// Append a place for every event with a location whose title appears in
/// `reply` (case-insensitive), unless one with the same title and address is
/// already there; at most three places in all.
pub fn add_calendar_places(message: &mut RichMessage, events: &[Value], reply: &str) {
    let haystack = reply.to_lowercase();
    for event in events {
        let places = message
            .blocks
            .iter()
            .filter(|b| b["type"] == "place")
            .count();
        if places >= MAX_PLACES || message.blocks.len() >= MAX_BLOCKS {
            return;
        }
        let title = text(&event["title"]).to_lowercase();
        if title.is_empty() || !haystack.contains(&title) {
            continue;
        }
        let Some(place) = event_place(event) else {
            continue;
        };
        let same = |b: &Value| {
            b["type"] == "place"
                && text(&b["title"]).to_lowercase() == text(&place["title"]).to_lowercase()
                && text(&b["address"]).to_lowercase() == text(&place["address"]).to_lowercase()
        };
        if !message.blocks.iter().any(same) {
            message.blocks.push(place);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_store::rich::from_plain_text;

    fn hack_night() -> Value {
        json!({"id": "iphone:42", "title": "Hack Night: Rust meets AI", "start": "2026-10-27T17:00:00+01:00",
               "end": "2026-10-27T20:00:00+01:00", "allDay": false, "location": "Folkehuset Møllestien"})
    }

    #[test]
    fn a_named_event_with_a_location_gets_one_place() {
        let reply = "Domani alle 17 hai la hack night: rust meets ai 📅";
        let mut m = from_plain_text(reply, "it", 3, Vec::new());
        add_calendar_places(&mut m, &[hack_night(), hack_night()], reply);
        assert_eq!(
            m.blocks,
            vec![json!({
                "type": "place", "kind": "event", "title": "Hack Night: Rust meets AI",
                "address": "Folkehuset Møllestien", "when": "2026-10-27T17:00:00+01:00",
                "end": "2026-10-27T20:00:00+01:00", "eventId": "iphone:42"
            })]
        );
    }

    #[test]
    fn no_place_when_the_reply_does_not_name_it_or_it_has_no_location() {
        let reply = "Domani sei libero.";
        let mut m = from_plain_text(reply, "it", 3, Vec::new());
        add_calendar_places(&mut m, &[hack_night()], reply);
        assert!(m.blocks.is_empty());
        let all_day = json!({"id": "g:1", "title": "Compleanno di Anna", "start": "2026-10-27",
                             "end": "2026-10-28", "allDay": true, "location": null});
        let reply = "Domani è il Compleanno di Anna.";
        let mut m = from_plain_text(reply, "it", 3, Vec::new());
        add_calendar_places(&mut m, &[all_day], reply);
        assert!(m.blocks.is_empty());
    }

    #[test]
    fn existing_places_count_and_are_not_repeated() {
        let reply = "Hack Night: Rust meets AI, Padel, Dentista, Cena.";
        let mut m = from_plain_text(reply, "it", 3, Vec::new());
        m.blocks.push(json!({"type": "place", "title": "hack night: rust meets ai", "address": "Folkehuset Møllestien", "kind": "place"}));
        let events: Vec<Value> = [
            ("Padel", "Club"),
            ("Dentista", "Via Roma 1"),
            ("Cena", "Noma"),
        ]
        .iter()
        .map(|(t, l)| json!({"title": t, "start": "2026-10-27T09:00:00Z", "location": l}))
        .chain([hack_night()])
        .collect();
        add_calendar_places(&mut m, &events, reply);
        let titles: Vec<&str> = m
            .blocks
            .iter()
            .map(|b| b["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, ["hack night: rust meets ai", "Padel", "Dentista"]);
    }
}
