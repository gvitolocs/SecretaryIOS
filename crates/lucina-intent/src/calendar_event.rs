//! Questions about one event of the owner's calendar: where, when or who ("Dove si tiene la
//! Hack Night di domani e a che ora?", "when is my next lecture?", "hvor er mødet i morgen?").
//! The calendar answers them, never memory or notes: a question with no matching event falls
//! through to the planner with the calendar's "nothing matches" in its context.
//!
//! Also the titles as the owner should read them: [`clean_title`] drops the calendar noise
//! subscriptions add ("(Aarhus Universitet - Personal timetable: 810677)", account emails).

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::calendar::{day_label, day_of_text, guess_lang, ymd_plus};
use crate::jsre::Re;
use crate::notes::words_src;
use crate::re;

/// How far ahead (days after today) an event is looked for when no day is named.
pub const LOOKAHEAD_DAYS: i64 = 13;

/// What a topic names: an event by its title, a lecture, or any event ("the next meeting").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Named,
    Lecture,
    Any,
}

/// A question about an event: look in `from..=to` for events matching `topic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuestion {
    /// Lowercased, articles and the day removed ("hack night"); for a lecture or any event,
    /// the kind word ("lezione").
    pub topic: String,
    pub kind: EventKind,
    pub from: String,
    pub to: String,
    /// The day was named ("domani"): every match of that day is answered.
    pub day_named: bool,
    pub lang: &'static str,
}

fn w(src: &str) -> Re {
    Re::new(&words_src(src), "iu")
}

/// Where / when / who cues, it/en/da.
const CUE: &str = r"dove\s+(?:si\s+(?:tiene|svolge|terr[àa]|fa)|[èe]|sar[àa]|c['’][èe])|dov['’][èe]|a\s+che\s+ora\s+(?:[èe]|inizia|comincia|finisce|c['’][èe]|ho|sar[àa])|quando\s+(?:[èe]|inizia|comincia|finisce|c['’][èe]|ho|sar[àa])|chi\s+(?:c['’][èe]|viene|partecipa)|in\s+(?:che|quale)\s+aula|where\s+(?:is|will|does|do\s+i\s+have)|where['’]s|what\s+time\s+(?:is|does|do\s+i\s+have)|when\s+(?:is|does|do\s+i\s+have)|when['’]s|who(?:['’]s|\s+is)\s+(?:coming|going|at)|which\s+room|hvor\s+(?:er|foregår|holdes|ligger)|hvornår\s+(?:er|starter|begynder|har\s+jeg)|hvad\s+tid\s+(?:er|starter|begynder)|hvem\s+kommer|hvilket\s+lokale";
/// "e a che ora?", "and where": a second question about the same event.
const MORE: &str = r"\s*(?:,|(?<![\p{L}\p{N}_])(?:e|ed|and|og)(?![\p{L}\p{N}_]))\s*(?:a\s+che\s+ora|quando|dove|chi|what\s+time|when|where|who|hvornår|hvor|hvem|hvad\s+tid)(?![\p{L}\p{N}_]).*$";
const NEXT: &str = r"prossim[oaie]|next|næste";
const LECTURE: &str = r"lezion[ei]|lecture|lectures|class|lesson|forelæsning(?:en)?|undervisning(?:en)?|lektion(?:en)?|time[nr]?";
const ANY: &str = r"impegn[oi]|event[oi]?|appuntament[oi]|riunion[ei]|events?|appointments?|meetings?|aftale[n]?|møde[t]?|begivenhed(?:en)?";
/// Words a title need not contain ("il corso di Security" names "Security and Privacy").
const FILLER: &[&str] = &[
    "corso",
    "course",
    "kursus",
    "kurset",
    "lezione",
    "lecture",
    "forelæsning",
    "evento",
    "event",
    "di",
    "del",
    "della",
    "of",
    "the",
    "af",
    "med",
    "with",
];
/// A calendar entry from the university timetable (title, calendar or description).
const TIMETABLE: &str = r"timetable|skema|lecture|lezione|forelæsning|undervisning|holdundervisning|exercise\s+class|øvelse";

/// `parseEventQuestion`: `None` unless the text asks where/when/who about a named event, or
/// about the next lecture or event.
pub fn parse_event_question(text: &str, now: DateTime<Utc>, tz: Tz) -> Option<EventQuestion> {
    let t = text.trim();
    if t.is_empty()
        || t.chars().count() > 200
        || crate::calendar::parse_create(t, now, tz).is_some()
    {
        return None;
    }
    let cue = w(CUE);
    let (topic_src, asked) = match cue.find(t) {
        Some((start, end, _)) => {
            let after = Re::new(MORE, "iu").replace_all(&t[end..], "");
            let after = after.trim_matches(|c: char| c.is_whitespace() || "?!.".contains(c));
            if after.is_empty() {
                (t[..start].to_owned(), true)
            } else {
                (after.to_owned(), true)
            }
        }
        // "prossima lezione?", "next lecture": asks when and where on its own.
        None if w(NEXT).test(t) && t.split_whitespace().count() <= 6 => (t.to_owned(), false),
        None => return None,
    };
    let lang = match guess_lang(t) {
        "en" if w(r"dove|dov['’]|quando|chi|prossim[oaie]|lezion[ei]|riunion[ei]|impegn[oi]|appuntament[oi]|aula").test(t) => "it",
        l => l,
    };
    let (day, rest) = match day_of_text(&topic_src, now, tz) {
        Some((ymd, rest)) => (Some(ymd), rest),
        None => (None, topic_src.clone()),
    };
    let mut topic = rest.to_lowercase();
    for (src, rep) in [
        (r"[?!.,;:«»“”\x22]+", " "),
        (
            r"\b(?:what|che|cosa|hvad)\s+(?:ho|i\s+have|jeg\s+har)\b",
            " ",
        ),
    ] {
        topic = Re::new(src, "iu").replace_all(&topic, rep);
    }
    topic = w(NEXT).replace_all(&topic, " ");
    let edge = r"(?:la|il|lo|l['’]|le|i|gli|un|una|uno|mia|mio|mie|miei|al|alla|allo|del|della|dello|di|da|per|in|the|my|a|an|on|for|this|at|to|den|det|de|min|mit|mine|på|til|om|i)";
    // Unicode word edges: an ASCII `\b` would split "mødet" after the "m".
    topic = Re::new(&format!(r"^(?:\s*{edge}(?![\p{{L}}\p{{N}}_]))+"), "iu")
        .replace_all(topic.trim(), "");
    topic = Re::new(&format!(r"(?:(?<![\p{{L}}\p{{N}}_]){edge}\s*)+$"), "iu")
        .replace_all(topic.trim(), "");
    let topic = topic.split_whitespace().collect::<Vec<_>>().join(" ");
    if topic.chars().count() < 3 {
        return None;
    }
    let kind = if Re::new(&format!("^(?:{LECTURE})$"), "iu").test(&topic) {
        EventKind::Lecture
    } else if Re::new(&format!("^(?:{ANY})$"), "iu").test(&topic) {
        EventKind::Any
    } else if asked {
        EventKind::Named
    } else {
        // "prossima Hack Night" without where/when is not a question.
        return None;
    };
    let (from, to) = match &day {
        Some(ymd) => (ymd.clone(), ymd.clone()),
        None => (ymd_plus(now, tz, 0), ymd_plus(now, tz, LOOKAHEAD_DAYS)),
    };
    Some(EventQuestion {
        topic,
        kind,
        from,
        to,
        day_named: day.is_some(),
        lang,
    })
}

fn text<'a>(e: &'a Value, key: &str) -> &'a str {
    e[key].as_str().map(str::trim).unwrap_or("")
}

/// The event answers the question (by title, location, or kind).
pub fn matches(q: &EventQuestion, event: &Value) -> bool {
    match q.kind {
        EventKind::Any => true,
        EventKind::Lecture => {
            let hay = format!(
                "{} {} {}",
                text(event, "title"),
                text(event, "calendar"),
                text(event, "description")
            );
            Re::new(TIMETABLE, "iu").test(&hay)
                || re!(r"^\d{4}-\d{2,4}[A-Za-z]?\b").test(text(event, "location"))
        }
        EventKind::Named => {
            let title = clean_title(text(event, "title")).to_lowercase();
            let location = text(event, "location").to_lowercase();
            let words: Vec<&str> = q
                .topic
                .split_whitespace()
                .filter(|w| !FILLER.contains(w))
                .collect();
            [title, location].iter().any(|hay| {
                hay.contains(&q.topic)
                    || (!words.is_empty() && words.iter().all(|w| hay.contains(w)))
            })
        }
    }
}

/// The event has not ended at `now` (all-day events: not before today).
fn not_over(e: &Value, now: DateTime<Utc>, tz: Tz) -> bool {
    let end = text(e, "end");
    if e["allDay"].as_bool().unwrap_or(false) {
        let today = now.with_timezone(&tz).date_naive();
        return NaiveDate::parse_from_str(end, "%Y-%m-%d").map_or(true, |d| d > today);
    }
    DateTime::parse_from_rfc3339(end).map_or(true, |d| d > now)
}

/// The events that answer `q`, in calendar order: every match of a named day, else the next
/// one that has not ended.
pub fn select(q: &EventQuestion, events: &[Value], now: DateTime<Utc>, tz: Tz) -> Vec<Value> {
    let hits = events.iter().filter(|e| matches(q, e));
    if q.day_named {
        return hits.cloned().collect();
    }
    hits.filter(|e| not_over(e, now, tz))
        .take(1)
        .cloned()
        .collect()
}

/// A title without calendar noise: parenthesised or bracketed parts that name a timetable, a
/// calendar or an account (an email, a long number), and bare email addresses. Empty only when
/// the title was.
pub fn clean_title(title: &str) -> String {
    let noise = Re::new(
        r"[(\[][^()\[\]]*(?:timetable|skema|calendar|kalender|calendario|universit|@|\d{4,})[^()\[\]]*[)\]]",
        "iu",
    );
    let out = noise.replace_all(title, " ");
    let out = re!(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", "u").replace_all(&out, " ");
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = out
        .trim_matches(|c: char| c.is_whitespace() || "-–—:|·,;".contains(c))
        .to_owned();
    if out.is_empty() {
        title.trim().to_owned()
    } else {
        out
    }
}

fn local(iso: &str, tz: Tz) -> Option<DateTime<Tz>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.with_timezone(&tz))
}

/// "oggi", "domani", "lunedì 2 novembre" (`en`: "today", "tomorrow", "on Monday 2 November").
pub fn day_words(ymd: &str, now: DateTime<Utc>, tz: Tz, lang: &str) -> String {
    let (today, tomorrow) = match lang {
        "it" => ("oggi", "domani"),
        "da" => ("i dag", "i morgen"),
        _ => ("today", "tomorrow"),
    };
    if ymd == ymd_plus(now, tz, 0) {
        today.into()
    } else if ymd == ymd_plus(now, tz, 1) {
        tomorrow.into()
    } else if lang == "en" {
        format!("on {}", day_label(ymd, lang))
    } else {
        day_label(ymd, lang)
    }
}

/// The one-sentence answer for the first event: what, when, where ("Hack Night: Rust meets AI è
/// domani alle 17:00, presso Folkehuset Møllestien.").
pub fn answer_sentence(event: &Value, now: DateTime<Utc>, tz: Tz, lang: &str) -> String {
    let title = clean_title(text(event, "title"));
    let all_day = event["allDay"].as_bool().unwrap_or(false);
    let start = text(event, "start");
    let (ymd, hm) = match (all_day, local(start, tz)) {
        (false, Some(t)) => (
            t.format("%Y-%m-%d").to_string(),
            Some(t.format("%H:%M").to_string()),
        ),
        _ => (start.chars().take(10).collect(), None),
    };
    let day = day_words(&ymd, now, tz, lang);
    let location = text(event, "location");
    let when = match (lang, hm) {
        ("it", Some(hm)) => format!("{day} alle {hm}"),
        ("da", Some(hm)) => format!("{day} kl. {hm}"),
        (_, Some(hm)) => format!("{day} at {hm}"),
        ("it", None) => format!("{day}, tutto il giorno"),
        ("da", None) => format!("{day}, hele dagen"),
        (_, None) => format!("{day}, all day"),
    };
    match (lang, location.is_empty()) {
        ("it", true) => format!("{title} è {when}."),
        ("it", false) => format!("{title} è {when}, presso {location}."),
        ("da", true) => format!("{title} er {when}."),
        ("da", false) => format!("{title} er {when} på {location}."),
        (_, true) => format!("{title} is {when}."),
        (_, false) => format!("{title} is {when}, at {location}."),
    }
}

/// The one sentence above an agenda's cards ("Domani hai 3 impegni 📅", "This week you have
/// one event 📅"). `n` > 0.
pub fn listing_sentence(
    q: &crate::calendar::Question,
    n: usize,
    now: DateTime<Utc>,
    tz: Tz,
) -> String {
    let when = if q.week {
        match q.lang {
            "it" => "questa settimana".to_owned(),
            "da" => "i denne uge".to_owned(),
            _ => "this week".to_owned(),
        }
    } else {
        day_words(&q.from, now, tz, q.lang)
    };
    let mut c = when.chars();
    let when = c
        .next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default();
    match (q.lang, n) {
        ("it", 1) => format!("{when} hai un impegno 📅"),
        ("it", _) => format!("{when} hai {n} impegni 📅"),
        ("da", 1) => format!("{when} har du én aftale 📅"),
        ("da", _) => format!("{when} har du {n} aftaler 📅"),
        (_, 1) => format!("{when} you have one event 📅"),
        (_, _) => format!("{when} you have {n} events 📅"),
    }
}

/// What the calendar says when nothing matches (for the planner's context).
pub fn no_match_context(q: &EventQuestion) -> String {
    let range = if q.from == q.to {
        q.from.clone()
    } else {
        format!("{}..{}", q.from, q.to)
    };
    format!(
        "Giuseppe's calendar {range}: no event matches \"{}\". Say it is not in his calendar; do not guess a time or place.",
        q.topic
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    const TZ: Tz = chrono_tz::Europe::Copenhagen;

    fn now() -> DateTime<Utc> {
        // Monday 26 October 2026, 10:00 in Copenhagen.
        Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap()
    }

    fn hack_night() -> Value {
        json!({"id": "iphone:42", "title": "Hack Night: Rust meets AI", "calendar": "vitologiuseppe17@gmail.com",
               "start": "2026-10-27T16:00:00.000Z", "end": "2026-10-27T19:00:00.000Z", "allDay": false,
               "location": "Folkehuset Møllestien"})
    }

    fn lecture() -> Value {
        json!({"id": "ics:au:1", "title": "Security and Privacy (Aarhus Universitet - Personal timetable: 810677)",
               "calendar": "AU timetable", "start": "2026-10-27T07:15:00.000Z", "end": "2026-10-27T09:00:00.000Z",
               "allDay": false, "location": "5789-003B Seminarrum"})
    }

    #[test]
    fn the_hack_night_question_is_a_calendar_question() {
        let q = parse_event_question(
            "Dove si tiene la Hack Night di domani e a che ora?",
            now(),
            TZ,
        )
        .unwrap();
        assert_eq!(
            q,
            EventQuestion {
                topic: "hack night".into(),
                kind: EventKind::Named,
                from: "2026-10-27".into(),
                to: "2026-10-27".into(),
                day_named: true,
                lang: "it",
            }
        );
        let hits = select(&q, &[lecture(), hack_night()], now(), TZ);
        assert_eq!(hits, vec![hack_night()]);
        assert_eq!(
            answer_sentence(&hits[0], now(), TZ, q.lang),
            "Hack Night: Rust meets AI è domani alle 17:00, presso Folkehuset Møllestien."
        );
    }

    #[test]
    fn other_where_and_when_questions() {
        let cases = [
            (
                "A che ora è la Hack Night?",
                "hack night",
                EventKind::Named,
                false,
                "it",
            ),
            (
                "quando inizia il corso di Security domani?",
                "corso di security",
                EventKind::Named,
                true,
                "it",
            ),
            (
                "Where is the hack night tomorrow?",
                "hack night",
                EventKind::Named,
                true,
                "en",
            ),
            (
                "when's my next lecture?",
                "lecture",
                EventKind::Lecture,
                false,
                "en",
            ),
            (
                "prossima lezione?",
                "lezione",
                EventKind::Lecture,
                false,
                "it",
            ),
            (
                "hvor er mødet i morgen?",
                "mødet",
                EventKind::Any,
                true,
                "da",
            ),
            (
                "La Hack Night dove si tiene?",
                "hack night",
                EventKind::Named,
                false,
                "it",
            ),
        ];
        for (text, topic, kind, day_named, lang) in cases {
            let q = parse_event_question(text, now(), TZ).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(
                (q.topic.as_str(), q.kind, q.day_named, q.lang),
                (topic, kind, day_named, lang),
                "{text}"
            );
            if !day_named {
                assert_eq!(
                    (q.from.as_str(), q.to.as_str()),
                    ("2026-10-26", "2026-11-08")
                );
            }
        }
        for text in [
            "ciao come stai",
            "dove sei?",
            "che ore sono?",
            "aggiungi in calendario hack night domani alle 18",
            "cosa ho in calendario domani?",
            "next",
        ] {
            assert_eq!(parse_event_question(text, now(), TZ), None, "{text}");
        }
    }

    #[test]
    fn the_next_lecture_is_the_first_one_not_over() {
        let q = parse_event_question("when is my next lecture?", now(), TZ).unwrap();
        let mut past = lecture();
        past["start"] = json!("2026-10-26T07:15:00.000Z");
        past["end"] = json!("2026-10-26T08:00:00.000Z");
        let hits = select(&q, &[past, hack_night(), lecture()], now(), TZ);
        assert_eq!(hits, vec![lecture()]);
        assert_eq!(
            answer_sentence(&hits[0], now(), TZ, "en"),
            "Security and Privacy is tomorrow at 08:15, at 5789-003B Seminarrum."
        );
        let q = parse_event_question("hvornår er Hack Night?", now(), TZ).unwrap();
        let hits = select(&q, &[hack_night()], now(), TZ);
        assert_eq!(
            answer_sentence(&hits[0], now(), TZ, "da"),
            "Hack Night: Rust meets AI er i morgen kl. 17:00 på Folkehuset Møllestien."
        );
        let q =
            parse_event_question("quando inizia il corso di Security domani?", now(), TZ).unwrap();
        assert_eq!(
            select(&q, &[hack_night(), lecture()], now(), TZ),
            vec![lecture()]
        );
        let q = parse_event_question("dove si tiene la cena da Anna?", now(), TZ).unwrap();
        assert!(select(&q, &[hack_night(), lecture()], now(), TZ).is_empty());
        assert!(no_match_context(&q).contains("no event matches \"cena da anna\""));
    }

    #[test]
    fn listing_sentences() {
        let q =
            crate::calendar::parse_question("cosa ho in calendario domani?", now(), TZ).unwrap();
        assert_eq!(
            listing_sentence(&q, 3, now(), TZ),
            "Domani hai 3 impegni 📅"
        );
        assert_eq!(
            listing_sentence(&q, 1, now(), TZ),
            "Domani hai un impegno 📅"
        );
        let q =
            crate::calendar::parse_question("what's on my calendar this week?", now(), TZ).unwrap();
        assert_eq!(
            listing_sentence(&q, 5, now(), TZ),
            "This week you have 5 events 📅"
        );
        let q = crate::calendar::parse_question("hvad har jeg i kalenderen torsdag", now(), TZ)
            .unwrap();
        assert_eq!(
            listing_sentence(&q, 1, now(), TZ),
            "Torsdag 29. oktober har du én aftale 📅"
        );
    }

    #[test]
    fn titles_without_calendar_noise() {
        assert_eq!(
            clean_title("Security and Privacy (Aarhus Universitet - Personal timetable: 810677)"),
            "Security and Privacy"
        );
        assert_eq!(
            clean_title("Standup – vitologiuseppe17@gmail.com"),
            "Standup"
        );
        assert_eq!(clean_title("Call [giuseppe@example.org]"), "Call");
        assert_eq!(
            clean_title("Ethical hacking report (optional)"),
            "Ethical hacking report (optional)"
        );
        assert_eq!(
            clean_title("Hack Night: Rust meets AI"),
            "Hack Night: Rust meets AI"
        );
        assert_eq!(clean_title("(123456)"), "(123456)");
    }
}
