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
use crate::name_match::{self, PersonAliases};
use crate::notes::words_src;
use crate::re;

/// How far ahead (days after today) an event is looked for when no day is named.
pub const LOOKAHEAD_DAYS: i64 = 13;
/// Events listed in [`no_match_context_in`].
const LISTED: usize = 15;

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
pub(crate) const FILLER: &[&str] = &[
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
/// A terse question with no verb: "Dove la mia prossima lezione?", "dove la riunione con Teresa?".
const BARE_CUE: &str = r"^\s*(?:dove|where|hvor)";
/// What a bare question must name besides the cue (so "dove sei?" stays no calendar question).
const BARE_KIND: &str = r"prossim[oaie]|next|næste|lezion[ei]|lectures?|class|lesson|forelæsning(?:en)?|undervisning(?:en)?|lektion(?:en)?|riunion[ei]|appuntament[oi]|impegn[oi]|event[oi]?|events?|appointments?|meetings?|møde[t]?|aftale[n]?";
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
        // "Dove la mia prossima lezione?": the cue without its verb.
        None if t.split_whitespace().count() <= 8 && bare_question(t).is_some() => {
            (bare_question(t).unwrap_or_default(), true)
        }
        // "prossima lezione?", "next lecture": asks when and where on its own.
        None if w(NEXT).test(t) && t.split_whitespace().count() <= 6 => (t.to_owned(), false),
        None => return None,
    };
    // Italian cues are understood above; the answer is English or Danish (owner rule 2026-11-02).
    let lang = guess_lang(t);
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

/// What follows a verbless "dove"/"where"/"hvor" when it names a kind of event or the next one.
fn bare_question(t: &str) -> Option<String> {
    let (_, end, _) = w(BARE_CUE).find(t)?;
    let rest = t[end..].trim();
    w(BARE_KIND).test(rest).then(|| rest.to_owned())
}

fn text<'a>(e: &'a Value, key: &str) -> &'a str {
    e[key].as_str().map(str::trim).unwrap_or("")
}

/// The event answers the question (by title, location, or kind).
pub fn matches(q: &EventQuestion, event: &Value) -> bool {
    matches_with(q, event, &PersonAliases::default())
}

/// [`matches`], with the spellings the owner told Lucina are one person.
pub fn matches_with(q: &EventQuestion, event: &Value, aliases: &PersonAliases) -> bool {
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
            let title = clean_title(text(event, "title"));
            let location = text(event, "location").to_owned();
            [title, location]
                .iter()
                .any(|hay| name_match::topic_matches(&q.topic, hay, aliases))
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
    select_with(q, events, now, tz, &PersonAliases::default())
}

/// [`select`], with the spellings the owner told Lucina are one person. An event that has begun
/// and not ended (or begins now) counts as not over.
pub fn select_with(
    q: &EventQuestion,
    events: &[Value],
    now: DateTime<Utc>,
    tz: Tz,
    aliases: &PersonAliases,
) -> Vec<Value> {
    let hits = events.iter().filter(|e| matches_with(q, e, aliases));
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

/// A title this long (characters) is the owner's sentence, not an event name.
const TITLE_MAX_CHARS: usize = 80;
/// A title with this many words is the owner's sentence, not an event name.
const TITLE_MAX_WORDS: usize = 10;
/// How many characters [`short_title`] keeps, cut at a word boundary.
const TITLE_KEEP_CHARS: usize = 60;
/// A message of this many words or fewer can be its own event name.
const MESSAGE_MAX_WORDS: usize = 6;
/// What an event with no name at all is called.
pub const UNTITLED: &str = "Event";

/// The title lowercased and stripped of its surrounding punctuation and inner double spaces,
/// for comparing it with the message it came from.
fn fold_words(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The title of a `calendar_event` is not a usable event name when it is the owner's whole
/// sentence ("That is the hour of the chat, not of the meeting, …"), a long ramble, or empty.
/// A short message that is itself the name is usable: "Dentist" out of "Dentist tomorrow at 10".
pub fn title_is_sentence(title: &str, message: &str) -> bool {
    let title = clean_title(title);
    let words = title.split_whitespace().count();
    if words == 0 {
        return true;
    }
    if words > TITLE_MAX_WORDS || title.chars().count() > TITLE_MAX_CHARS {
        return true;
    }
    message.split_whitespace().count() > MESSAGE_MAX_WORDS
        && fold_words(&title) == fold_words(message)
}

/// The title to keep when nothing better is known: [`clean_title`], of the first
/// [`TITLE_KEEP_CHARS`] characters when the title is longer than that — cut at a word boundary,
/// so the word the cut lands in is dropped rather than left half. Never empty ([`UNTITLED`] when
/// nothing of a word is kept).
pub fn short_title(title: &str) -> String {
    let cleaned = clean_title(title);
    let too_long = cleaned.chars().count() > TITLE_KEEP_CHARS;
    let kept = if too_long {
        let head: String = cleaned.chars().take(TITLE_KEEP_CHARS).collect();
        match head.rfind(' ') {
            Some(i) if i > 0 => head[..i].to_owned(),
            _ => head,
        }
    } else {
        cleaned
    };
    let words: Vec<&str> = kept
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    if words.is_empty() {
        UNTITLED.to_owned()
    } else {
        words.join(" ")
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

/// The one sentence above an agenda's cards ("Tomorrow you have 3 events 📅", "This week you have
/// one event 📅"). `n` > 0.
pub fn listing_sentence(
    q: &crate::calendar::Question,
    n: usize,
    now: DateTime<Utc>,
    tz: Tz,
) -> String {
    let when = if q.week {
        crate::calendar::range_words(q, now, tz)
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

/// [`no_match_context`] with the events of the range, so the model can recognise the one asked
/// about under another spelling or name (at most [`LISTED`], in the owner's zone).
pub fn no_match_context_in(
    q: &EventQuestion,
    events: &[Value],
    now: DateTime<Utc>,
    tz: Tz,
) -> String {
    let upcoming: Vec<&Value> = events
        .iter()
        .filter(|e| q.day_named || not_over(e, now, tz))
        .collect();
    if upcoming.is_empty() {
        return no_match_context(q);
    }
    let lines: Vec<String> = upcoming
        .iter()
        .take(LISTED)
        .map(|e| {
            let when = match (
                e["allDay"].as_bool().unwrap_or(false),
                local(text(e, "start"), tz),
            ) {
                (false, Some(t)) => t.format("%a %d %b %H:%M").to_string(),
                (_, Some(t)) => format!("{} all day", t.format("%a %d %b")),
                _ => text(e, "start").chars().take(10).collect(),
            };
            let place = Some(text(e, "location")).filter(|l| !l.is_empty());
            let place = place.map(|l| format!(" @ {l}")).unwrap_or_default();
            format!("{when} {}{place}", clean_title(text(e, "title")))
        })
        .collect();
    let more = match upcoming.len().saturating_sub(LISTED) {
        0 => String::new(),
        n => format!("; +{n} more"),
    };
    let range = if q.from == q.to {
        q.from.clone()
    } else {
        format!("{}..{}", q.from, q.to)
    };
    format!(
        "Giuseppe's calendar {range} ({}): no event matches \"{}\". Events in that range: {}{more}. If one of these is the event asked about under another spelling or name, answer with it; otherwise say it is not in his calendar. Do not guess a time or place.",
        tz.name(),
        q.topic,
        lines.join("; ")
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
                lang: "en",
            }
        );
        let hits = select(&q, &[lecture(), hack_night()], now(), TZ);
        assert_eq!(hits, vec![hack_night()]);
        assert_eq!(
            answer_sentence(&hits[0], now(), TZ, q.lang),
            "Hack Night: Rust meets AI is tomorrow at 17:00, at Folkehuset Møllestien."
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
                "en",
            ),
            (
                "quando inizia il corso di Security domani?",
                "corso di security",
                EventKind::Named,
                true,
                "en",
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
                "en",
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
                "en",
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
            "Tomorrow you have 3 events 📅"
        );
        assert_eq!(
            listing_sentence(&q, 1, now(), TZ),
            "Tomorrow you have one event 📅"
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

    /// The owner correcting one hour for another: his whole sentence, which the
    /// direct rule reads as the event name.
    const CORRECTION: &str =
        "That is the hour of the chat, not of the meeting, the meeting is tomorrow at 10";

    #[test]
    fn a_whole_sentence_is_not_an_event_title() {
        assert!(title_is_sentence(CORRECTION, CORRECTION));
        // The rule's title, with the day and the time removed: still the sentence.
        assert!(title_is_sentence(
            "That is the hour of the chat not of the meeting the meeting is",
            CORRECTION
        ));
        // Same words, other punctuation and case (9 words: only the whole-sentence rule fires).
        let said = "Should we move the design review to next friday";
        assert!(title_is_sentence(&said.to_uppercase(), &format!("{said}!")));
        assert!(title_is_sentence(
            "Standup with the design crew and the two new mentors on Thursday",
            "when suits you?"
        ));
        // Ten words (so not the word rule) but 81 characters.
        assert!(title_is_sentence(
            "International Workshop on Machine Learning for Network Operations and Maintenance",
            "put it down"
        ));
        assert!(title_is_sentence("", CORRECTION));
        assert!(title_is_sentence("   ", CORRECTION));
    }

    #[test]
    fn a_short_name_stays_the_title_of_its_message() {
        assert!(!title_is_sentence("Dentist", "Dentist tomorrow at 10"));
        // Six words used whole: a name, not a sentence.
        let six = "Team standup with the two mentors";
        assert!(!title_is_sentence(six, six));
        assert!(!title_is_sentence(
            "Mentor session",
            "Mentor session 6 november at 12:30"
        ));
        assert!(!title_is_sentence("Hack Night", CORRECTION));
    }

    #[test]
    fn a_title_kept_short_cuts_at_a_word() {
        let short = short_title(CORRECTION);
        assert_eq!(
            short,
            "That is the hour of the chat, not of the meeting, the"
        );
        assert!(short.chars().count() <= 60, "{short}");
        assert!(!short.ends_with("meetin"), "{short}");
        // Nothing to cut at: no word kept, and still not an empty title.
        assert_eq!(short_title("!!! ??? ..."), UNTITLED);
        assert_eq!(short_title(""), UNTITLED);
        // A word longer than the keep: it stays, whole, at the cut.
        assert_eq!(short_title(&"x".repeat(90)).chars().count(), 60);
        // A title the keep covers is not cut at all: its last word is not a spare.
        let whole = "Call the clinic about the invoice today";
        assert_eq!(short_title(whole), whole);
        // Ordinary words past the keep: the cut lands between two of them.
        let long = "Please bring the annual general meeting of the association of civil engineers to a vote today";
        let kept = short_title(long);
        assert_eq!(
            kept,
            "Please bring the annual general meeting of the association"
        );
        assert!(kept.chars().count() <= 60, "{kept}");
        assert!(long.chars().count() > 90, "{long}");
        assert_eq!(
            long.chars().nth(kept.chars().count()),
            Some(' '),
            "cut at a word, not inside one: {kept}"
        );
    }

    fn at(h: u32, m: u32, sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 28, h, m, sec).unwrap()
    }

    /// 2026-10-28, 12:00 in Copenhagen: the owner's iPhone calendar.
    fn therese_zoom() -> Value {
        json!({"id": "iphone:77", "title": "Therese zoom", "calendar": "iPhone",
               "start": "2026-10-28T11:00:00.000Z", "end": "2026-10-28T12:00:00.000Z",
               "allDay": false, "location": "Home"})
    }

    fn afternoon_lecture() -> Value {
        json!({"id": "ics:au:2", "title": "Security and Privacy (Aarhus Universitet - Personal timetable: 810677)",
               "calendar": "AU timetable", "start": "2026-10-28T13:15:00.000Z", "end": "2026-10-28T15:00:00.000Z",
               "allDay": false, "location": "5789-003B Seminarrum"})
    }

    #[test]
    fn therese_is_teresa_finds_the_zoom_an_hour_ahead() {
        let now = at(10, 0, 47);
        let text = "You know what time is the meeting therese è teresa";
        let q = parse_event_question(text, now, TZ).unwrap();
        assert_eq!(q.kind, EventKind::Named);
        assert_eq!(
            (q.from.as_str(), q.to.as_str()),
            ("2026-10-28", "2026-11-10")
        );
        assert_eq!(q.lang, "en");
        let events = [afternoon_lecture(), therese_zoom()];
        let hits = select(&q, &events, now, TZ);
        assert_eq!(hits, vec![therese_zoom()]);
        assert_eq!(
            answer_sentence(&hits[0], now, TZ, "en"),
            "Therese zoom is today at 12:00, at Home."
        );
    }

    #[test]
    fn other_spellings_and_languages_find_the_same_zoom() {
        let now = at(10, 0, 47);
        let events = [afternoon_lecture(), therese_zoom()];
        for text in [
            "a che ora è la riunione con Teresa?",
            "when is the Theresa call?",
            "quando è la chiamata con Tereza?",
            "hvornår er mødet med Therese?",
        ] {
            let q = parse_event_question(text, now, TZ).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(select(&q, &events, now, TZ), vec![therese_zoom()], "{text}");
        }
    }

    #[test]
    fn accents_fold_and_strangers_do_not_match() {
        let named = |topic: &str| EventQuestion {
            topic: topic.into(),
            kind: EventKind::Named,
            from: "2026-10-28".into(),
            to: "2026-10-28".into(),
            day_named: true,
            lang: "en",
        };
        let ev = |title: &str| json!({"title": title, "start": "2026-10-28T11:00:00.000Z", "end": "2026-10-28T12:00:00.000Z", "allDay": false});
        assert!(matches(&named("café"), &ev("Cafe meetup")));
        assert!(matches(&named("Jørgen"), &ev("Jorgen")));
        assert!(!matches(&named("teresa"), &ev("Tessa party")));
        assert!(!matches(&named("teresa"), &ev("Theater")));
        let now = at(8, 0, 0);
        let q = parse_event_question("dove si tiene la cena da Anna?", now, TZ).unwrap();
        assert!(select(&q, &[hack_night(), lecture(), therese_zoom()], now, TZ).is_empty());
    }

    #[test]
    fn a_person_alias_from_the_store_finds_the_event() {
        let now = at(10, 0, 47);
        let mut aliases = PersonAliases::default();
        let q = parse_event_question("when is the meeting with Resa?", now, TZ).unwrap();
        assert!(select_with(&q, &[therese_zoom()], now, TZ, &aliases).is_empty());
        assert_eq!(
            alias_to_remember_of("meeting therese è resa"),
            Some(("therese".into(), "resa".into()))
        );
        aliases.add("therese", "resa");
        assert_eq!(
            select_with(&q, &[therese_zoom()], now, TZ, &aliases).len(),
            1
        );
    }

    fn alias_to_remember_of(topic: &str) -> Option<(String, String)> {
        name_match::alias_to_remember(topic)
    }

    #[test]
    fn an_event_in_progress_or_starting_now_is_selected() {
        let q = parse_event_question("when is the Therese zoom?", at(10, 0, 0), TZ).unwrap();
        let zoom = [therese_zoom()];
        // Started five minutes ago, and starting right now.
        assert_eq!(select(&q, &zoom, at(11, 5, 0), TZ).len(), 1);
        assert_eq!(select(&q, &zoom, at(11, 0, 0), TZ).len(), 1);
        // Ended a minute ago (and exactly now).
        assert!(select(&q, &zoom, at(12, 1, 0), TZ).is_empty());
        assert!(select(&q, &zoom, at(12, 0, 0), TZ).is_empty());
    }

    #[test]
    fn no_match_lists_the_range_in_local_time() {
        let now = at(10, 0, 47);
        let q = parse_event_question("quando è la cena da Anna?", now, TZ).unwrap();
        let ctx = no_match_context_in(&q, &[therese_zoom(), afternoon_lecture()], now, TZ);
        assert!(ctx.contains("no event matches \"cena da anna\""), "{ctx}");
        assert!(
            ctx.contains("Wed 28 Oct 12:00 Therese zoom @ Home"),
            "{ctx}"
        );
        assert!(
            ctx.contains("Wed 28 Oct 14:15 Security and Privacy @ 5789-003B Seminarrum"),
            "{ctx}"
        );
        assert!(
            ctx.contains("under another spelling or name, answer with it"),
            "{ctx}"
        );
        assert!(
            ctx.contains("otherwise say it is not in his calendar"),
            "{ctx}"
        );
        assert!(!ctx.contains("11:00"), "{ctx}");
        // An event that is over is not offered; an empty range keeps the plain text.
        assert_eq!(no_match_context_in(&q, &[], now, TZ), no_match_context(&q));
        let many: Vec<Value> = (0..20).map(|_| therese_zoom()).collect();
        let ctx = no_match_context_in(&q, &many, now, TZ);
        assert_eq!(ctx.matches("Therese zoom").count(), 15);
        assert!(ctx.contains("+5 more"), "{ctx}");
    }

    #[test]
    fn a_question_without_its_verb_is_still_a_question() {
        let q = parse_event_question("Dove la mia prossima lezione?", now(), TZ).unwrap();
        assert_eq!(
            (q.kind, q.lang, q.topic.as_str()),
            (EventKind::Lecture, "en", "lezione")
        );
        assert!(!q.day_named);
        let mut past = lecture();
        past["start"] = json!("2026-10-26T07:15:00.000Z");
        past["end"] = json!("2026-10-26T08:00:00.000Z");
        assert_eq!(
            select(&q, &[past, hack_night(), lecture()], now(), TZ),
            vec![lecture()]
        );
        let q = parse_event_question("dove la riunione con Teresa?", now(), TZ).unwrap();
        assert_eq!(q.kind, EventKind::Named);
        let q = parse_event_question("where my next lecture?", now(), TZ).unwrap();
        assert_eq!(q.kind, EventKind::Lecture);
        for text in [
            "dove sei?",
            "where are you",
            "hvor er du",
            "dove vai domani",
        ] {
            assert_eq!(parse_event_question(text, now(), TZ), None, "{text}");
        }
    }
}
