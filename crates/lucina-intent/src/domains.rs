//! Which of the owner's data a chat message asks to read (`src/turn-domains.js`):
//! the shopping list, a saved note (or the notes list), the calendar, the
//! reminders. One domain goes to its deterministic bridge; two or more
//! ("cosa c'è sulla lista della spesa e cosa ho nella nota X?") skip every
//! shortcut, so a single bridge never answers half the question.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::heuristics::{looks_like_reminder_list, looks_like_reminder_request};
use crate::notes::NotesIntent;
use crate::{calendar, grocery, notes, re};

pub const SHOPPING: &str = "shopping";
pub const NOTES: &str = "notes";
pub const CALENDAR: &str = "calendar";
pub const REMINDERS: &str = "reminders";

/// `readDomains`: the domains a message reads, in that order. Empty when it
/// also writes something (a note save or its next part, a calendar event, a
/// reminder): the write path keeps the turn.
pub fn read_domains(
    text: &str,
    now: DateTime<Utc>,
    tz: Tz,
    pending_part: bool,
) -> Vec<&'static str> {
    let t = text.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let note = notes::parse_notes_intent(t, pending_part);
    if matches!(
        note,
        Some(NotesIntent::Save { .. } | NotesIntent::Part { .. })
    ) || looks_like_reminder_request(t)
        || calendar::parse_create(t, now, tz).is_some()
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    if grocery::parse_grocery_intent_at(t, now)
        .is_some_and(|g| g.intent == "grocery.shopping_list.query")
    {
        out.push(SHOPPING);
    }
    if note.is_some() {
        out.push(NOTES);
    }
    if calendar::parse_question(t, now, tz).is_some()
        || crate::calendar_event::parse_event_question(t, now, tz).is_some()
    {
        out.push(CALENDAR);
    }
    if re!(
        r"(?<![\p{L}\p{N}_])(?:promemoria|reminders?|påmindelse(?:r|rne)?)(?![\p{L}\p{N}_])",
        "iu"
    )
    .test(t)
        && looks_like_reminder_list(t)
    {
        out.push(REMINDERS);
    }
    out
}

/// `shoppingListBlock`: the shopping list (legacy list view items) as a
/// context block.
pub fn shopping_list_block(items: &[Value]) -> String {
    let names = |purchased: bool, max: usize| -> Vec<&str> {
        items
            .iter()
            .filter(|i| (i["status"].as_str() == Some("purchased")) == purchased)
            .filter_map(|i| i["product"].as_str())
            .take(max)
            .collect()
    };
    let needed = names(false, 40);
    let recent = names(true, 20);
    let mut out = format!(
        "Shopping list (Secretary app), to buy: {}.",
        if needed.is_empty() {
            "nothing".to_owned()
        } else {
            needed.join(", ")
        }
    );
    if !recent.is_empty() {
        out.push_str(&format!(" Recently bought: {}.", recent.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str =
        "Cosa c è sulla lista della spesa e cosa ho nella nota craftit-meeting (una riga)?";

    fn d(t: &str) -> Vec<&'static str> {
        let now = DateTime::parse_from_rfc3339("2026-10-22T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        read_domains(t, now, chrono_tz::Europe::Rome, false)
    }

    #[test]
    fn compound_and_single() {
        assert_eq!(d(OWNER), [SHOPPING, NOTES]);
        assert_eq!(
            d("cosa ho in calendario domani e quali promemoria ho?"),
            [CALENDAR, REMINDERS]
        );
        assert_eq!(
            d("what's on my calendar today and what's on my shopping list?"),
            [SHOPPING, CALENDAR]
        );
        assert_eq!(
            d("hvad har jeg i kalenderen i morgen og hvad står på min indkøbsliste?"),
            [SHOPPING, CALENDAR]
        );
        assert_eq!(
            d("cosa ho domani e cosa c'è in lista?"),
            [SHOPPING, CALENDAR]
        );
        assert_eq!(d("cosa ho domani?"), [CALENDAR]);
        assert_eq!(d("cosa ho in lista?"), [SHOPPING]);
        assert_eq!(d("cosa ho nella nota craftit-meeting?"), [NOTES]);
        assert!(d("salva questo come nota agenda: cosa ho domani e cosa c'è in lista").is_empty());
        assert!(
            d("aggiungi in calendario dentista domani alle 9:30 e cosa c'è in lista?").is_empty()
        );
    }

    /// The over-broad patterns of 2026-10-22 (same corpus as Node's
    /// tests/turn-domains.test.mjs).
    #[test]
    fn negatives() {
        let now = DateTime::parse_from_rfc3339("2026-10-22T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let tz = chrono_tz::Europe::Rome;
        for t in [
            "cosa ho nella nota craftit-meeting?",
            "cosa ho in lista?",
            "cosa ho comprato ieri?",
            "che cosa ho sbagliato?",
            "what do I have to buy?",
            "what's the train schedule to Aarhus?",
            "hvad skal jeg købe?",
            OWNER,
        ] {
            assert_eq!(calendar::parse_question(t, now, tz), None, "{t}");
        }
        for t in [
            "cosa ho comprato ieri?",
            "I got a new job",
            "abbiamo preso il treno ieri",
            "siamo senza parole",
            "ci serve una vacanza",
            "dimmi la lista dei film da vedere",
            "cosa c'è da vedere sulla costa amalfitana?",
            "how much is 15% of 80?",
            "what's the most beautiful city in Denmark?",
            "what do I have tomorrow?",
        ] {
            assert_eq!(
                grocery::parse_grocery_intent(t).map(|g| g.intent),
                None,
                "{t}"
            );
        }
        assert_eq!(
            grocery::parse_grocery_intent("abbiamo preso il pane").map(|g| g.intent),
            Some("grocery.purchase")
        );
        for t in ["read the document I sent you", "I lost my notes from class"] {
            assert_eq!(notes::parse_notes_intent(t, false), None, "{t}");
        }
        for t in [
            "cancella i promemoria di oggi",
            "ricordami oggi pomeriggio di chiamare la mamma",
            "show me my workout schedule",
        ] {
            assert!(!looks_like_reminder_list(t), "{t}");
        }
        assert!(looks_like_reminder_list("hvilke påmindelser har jeg?"));
    }

    #[test]
    fn shopping_block() {
        let items = [
            json!({"product": "Latte", "status": "needed"}),
            json!({"product": "Pane", "status": "purchased"}),
        ];
        assert_eq!(
            shopping_list_block(&items),
            "Shopping list (Secretary app), to buy: Latte. Recently bought: Pane."
        );
        assert_eq!(
            shopping_list_block(&[]),
            "Shopping list (Secretary app), to buy: nothing."
        );
    }
}
