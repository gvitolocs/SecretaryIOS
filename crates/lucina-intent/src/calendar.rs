//! The calendar bridge's parser and texts (`src/calendar-bridge.js`):
//! questions ("cosa ho in calendario domani?", "what's on my calendar",
//! "hvad har jeg i kalenderen"), event creation (direct for the owner's own
//! turns, with an undo; a proposal in Links otherwise), and the it/en/da
//! answers.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::jsre::{group, Re};
use crate::notes::words_src;
use crate::re;

fn w(src: &str) -> Re {
    Re::new(&words_src(src), "iu")
}

const WEEKDAYS: [[&str; 3]; 7] = [
    ["luned[iì]", "monday", "mandag"],
    ["marted[iì]", "tuesday", "tirsdag"],
    ["mercoled[iì]", "wednesday", "onsdag"],
    ["gioved[iì]", "thursday", "torsdag"],
    ["venerd[iì]", "friday", "fredag"],
    ["sabato", "saturday", "lørdag"],
    ["domenica", "sunday", "søndag"],
];

const MONTHS: &[(&str, u32)] = &[
    ("gennaio", 1),
    ("febbraio", 2),
    ("marzo", 3),
    ("aprile", 4),
    ("maggio", 5),
    ("giugno", 6),
    ("luglio", 7),
    ("agosto", 8),
    ("settembre", 9),
    ("ottobre", 10),
    ("novembre", 11),
    ("dicembre", 12),
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
    ("januar", 1),
    ("februar", 2),
    ("marts", 3),
    ("maj", 5),
    ("juni", 6),
    ("juli", 7),
    ("oktober", 10),
];

fn local_date(now: DateTime<Utc>, tz: Tz) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

/// `ymdPlus`.
pub fn ymd_plus(now: DateTime<Utc>, tz: Tz, days: i64) -> String {
    (local_date(now, tz) + Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

/// A wall time in `tz` as `YYYY-MM-DDTHH:MM:00+HH:MM`.
pub fn local_iso(ymd: &str, hh: u32, mm: u32, tz: Tz) -> Option<String> {
    let d = NaiveDate::parse_from_str(ymd, "%Y-%m-%d").ok()?;
    let naive = d.and_hms_opt(hh, mm, 0)?;
    let dt = match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => t,
        LocalResult::Ambiguous(a, _) => a,
        // A wall time in the spring gap: JS lands on the offset after it.
        LocalResult::None => tz
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()?,
    };
    Some(dt.format("%Y-%m-%dT%H:%M:00%:z").to_string())
}

/// How the day was named (removed from an event title).
#[derive(Debug, Clone)]
enum Remover {
    Pattern(&'static str, &'static str),
    Dynamic(String),
    Literal(String),
}

impl Remover {
    fn apply(&self, t: &str) -> String {
        match self {
            Self::Pattern(src, flags) => Re::new(src, flags).replace_all(t, " "),
            Self::Dynamic(src) => Re::new(src, "iu").replace_all(t, " "),
            Self::Literal(s) => t.replace(s.as_str(), " "),
        }
    }
}

const DAY_WORDS: [(&str, i64); 3] = [
    ("dopodomani|day after tomorrow|i overmorgen|overmorgen", 2),
    ("domani|tomorrow|i morgen|imorgen", 1),
    (
        "oggi|stamattina|stasera|today|tonight|this evening|i dag|idag|i aften",
        0,
    ),
];

fn weekday_offset(text: &str, now: DateTime<Utc>, tz: Tz) -> Option<(i64, String)> {
    let today = now.with_timezone(&tz).weekday().num_days_from_monday() as i64;
    for (i, names) in WEEKDAYS.iter().enumerate() {
        if let Some(name) = names.iter().find(|n| w(n).test(text)) {
            let ahead = (i as i64 - today + 7) % 7;
            return Some((if ahead == 0 { 7 } else { ahead }, words_src(name)));
        }
    }
    None
}

fn day_of(text: &str, now: DateTime<Utc>, tz: Tz) -> Option<(String, Remover)> {
    if let Some(c) = re!(r"\b(\d{4})-(\d{2})-(\d{2})\b").exec(text) {
        return Some((
            group(&c, 0).to_owned(),
            Remover::Pattern(r"\b\d{4}-\d{2}-\d{2}\b", ""),
        ));
    }
    let year = now.with_timezone(&tz).year();
    if let Some(c) = re!(r"(?<![\d/])(\d{1,2})[/.](\d{1,2})(?:[/.](\d{2,4}))?(?![\d/])").exec(text)
    {
        let y = match group(&c, 3) {
            "" => year,
            y if y.len() == 2 => 2000 + y.parse::<i32>().unwrap_or(0),
            y => y.parse().unwrap_or(year),
        };
        let ymd = format!("{y}-{:0>2}-{:0>2}", group(&c, 2), group(&c, 1));
        return Some((ymd, Remover::Literal(group(&c, 0).to_owned())));
    }
    let names = MONTHS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("|");
    let named = Re::new(
        &format!(r"(?<![\p{{L}}\p{{N}}])(\d{{1,2}})\.?\s+({names})(?![\p{{L}}])"),
        "iu",
    );
    if let Some(c) = named.exec(text) {
        let m = MONTHS
            .iter()
            .find(|(n, _)| *n == group(&c, 2).to_lowercase())
            .map(|(_, m)| *m)
            .unwrap_or(1);
        let ymd = format!("{year}-{m:02}-{:0>2}", group(&c, 1));
        return Some((ymd, Remover::Literal(group(&c, 0).to_owned())));
    }
    for (src, offset) in DAY_WORDS {
        if w(src).test(text) {
            return Some((ymd_plus(now, tz, offset), Remover::Dynamic(words_src(src))));
        }
    }
    weekday_offset(text, now, tz).map(|(o, src)| (ymd_plus(now, tz, o), Remover::Dynamic(src)))
}

/// The day a text names (`YYYY-MM-DD`) and the text without it.
pub(crate) fn day_of_text(text: &str, now: DateTime<Utc>, tz: Tz) -> Option<(String, String)> {
    day_of(text, now, tz).map(|(ymd, remover)| (ymd, remover.apply(text)))
}

fn time_of(text: &str) -> Option<(u32, u32, Remover)> {
    let patterns: [(&'static str, &'static str); 5] = [
        (
            r"(?<![\p{L}])(?:alle|all'|verso le|dalle)\s*(\d{1,2})(?:[:.](\d{2}))?",
            "iu",
        ),
        (r"(?<![\p{L}])at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?", "iu"),
        (r"(?<![\p{L}])kl\.?\s*(\d{1,2})(?:[:.](\d{2}))?", "iu"),
        (r"(?<![\d])(\d{1,2})[:.](\d{2})(?![\d])", "u"),
        (r"(?<![\d])(\d{1,2})\s*(am|pm)(?![\p{L}])", "iu"),
    ];
    for (src, flags) in patterns {
        let Some(c) = Re::new(src, flags).exec(text) else {
            continue;
        };
        let mut hh: u32 = group(&c, 1).parse().unwrap_or(99);
        let mut mm = 0;
        let mut ampm = String::new();
        let g2 = group(&c, 2);
        if !g2.is_empty() && g2.bytes().all(|b| b.is_ascii_digit()) {
            mm = g2.parse().unwrap_or(99);
        } else if !g2.is_empty() {
            ampm = g2.to_lowercase();
        }
        if !group(&c, 3).is_empty() {
            ampm = group(&c, 3).to_lowercase();
        }
        if ampm == "pm" && hh < 12 {
            hh += 12;
        }
        if ampm == "am" && hh == 12 {
            hh = 0;
        }
        if hh > 23 || mm > 59 {
            return None;
        }
        return Some((hh, mm, Remover::Pattern(src, flags)));
    }
    None
}

/// A calendar read needs a calendar word, or a pure time question ([`ask_day`]).
const CAL_WORDS: &str = r"calendario|agenda|impegni|appuntamenti|eventi|riunioni|calendar|schedule|appointments|meetings|kalender(?:en)?|aftaler|møder|program(?:met)?";
/// "Train schedule", "workout schedule": someone else's timetable.
const NOT_CAL_SCHEDULE: &str =
    r"(?:train|bus|flight|ferry|tv|workout|training|gym|class|school|garbage|bin)\s+schedules?";
const QUESTION_CUES: &str = r"cosa|che|quali|quanti|ho|c'è|ci sono|what|what's|whats|do i have|am i|anything|is there|hvad|har jeg|skal jeg|er der|hvilke|show|mostra|dimmi|tell me|vis";
const DAY_REF: &str = r"(?:(?:il|on|per|for|di|på)\s+)?(?:oggi|domani|dopodomani|stamattina|stasera|stanotte|today|tonight|tomorrow|this\s+(?:morning|afternoon|evening|week|weekend)|next\s+week|questa\s+settimana|in\s+settimana|(?:questo|nel)\s+weekend|i\s+dag|idag|i\s+morgen|imorgen|i\s+overmorgen|overmorgen|i\s+aften|i\s+weekenden|denne\s+uge|luned[iì]|marted[iì]|mercoled[iì]|gioved[iì]|venerd[iì]|sabato|domenica|monday|tuesday|wednesday|thursday|friday|saturday|sunday|mandag|tirsdag|onsdag|torsdag|fredag|lørdag|søndag)";
const ASK_PHRASE: &str = r"(?:cosa|cos['’]|che)\s*(?:ho|faccio)(?:\s+(?:in\s+programma|da\s+fare))?|what\s+(?:do\s+i\s+have|am\s+i\s+doing|have\s+i\s+got)(?:\s+(?:on|planned|going\s+on))?|what(?:['’]s|\s+is)\s+(?:on|planned|happening)|hvad\s+(?:har|skal)\s+jeg(?:\s+(?:lave|på\s+programmet))?";

/// "Cosa ho domani?", "domani cosa ho?", "hvad skal jeg i dag": the day follows the
/// question directly ("cosa ho nella nota X", "cosa ho in lista" are not calendar questions).
fn ask_day(t: &str) -> bool {
    w(&format!(r"(?:{ASK_PHRASE})\s+{DAY_REF}")).test(t)
        || w(&format!(
            r"{DAY_REF}\s*,?\s*(?:{ASK_PHRASE})(?=\s*[?.!]*$|\s*[?.!]|\s+(?:e|and|og)\s)"
        ))
        .test(t)
}
const WEEK_WORDS: &str = r"settimana|questa settimana|prossimi giorni|this week|the week|next few days|denne uge|ugen|i denne uge";
/// "nei prossimi 7 giorni", "in the next 10 days", "de næste 5 dage": today and the N - 1 days after.
const NEXT_DAYS: &str = r"(?:prossimi|next|næste)\s+(\d{1,2})\s+(?:giorni|days|dage)";
/// Next week, Monday to Sunday (checked before WEEK_WORDS, whose bare "settimana" would take it).
const NEXT_WEEK: &str =
    r"(?:la\s+)?prossima\s+settimana|settimana\s+prossima|next\s+week|næste\s+uge";

/// The days an agenda covers, in words ("questa settimana", "nei prossimi 10 giorni", "next
/// week"); a single day uses the day's own words.
pub fn range_words(q: &Question, now: DateTime<Utc>, tz: Tz) -> String {
    let today = local_date(now, tz);
    let from = NaiveDate::parse_from_str(&q.from, "%Y-%m-%d").unwrap_or(today);
    let to = NaiveDate::parse_from_str(&q.to, "%Y-%m-%d").unwrap_or(from);
    let span = (to - from).num_days() + 1;
    if from > today {
        return match q.lang {
            "it" => "la prossima settimana".into(),
            "da" => "i næste uge".into(),
            _ => "next week".into(),
        };
    }
    if span == 7 {
        return match q.lang {
            "it" => "questa settimana".into(),
            "da" => "i denne uge".into(),
            _ => "this week".into(),
        };
    }
    match q.lang {
        "it" => format!("nei prossimi {span} giorni"),
        "da" => format!("de næste {span} dage"),
        _ => format!("in the next {span} days"),
    }
}
const CREATE: [&str; 3] = [
    r"(?:aggiungi|aggiungere|metti|mettere|segna|segnami|inserisci|crea|fissa)(?:mi)?\b.*?\b(?:in|al|nel|sul|nell')\s*(?:mio\s+)?(?:calendario|agenda)",
    r"(?:add|put|schedule|create|book)\b.*?\b(?:to|in|on|into)\s+(?:my\s+)?calendar",
    r"(?:tilføj|sæt|læg|opret|skriv)\b.*?\b(?:i|til|ind\s+i)\s+(?:min\s+)?kalender(?:en)?",
];

fn is_create(t: &str) -> bool {
    CREATE.iter().any(|src| w(src).test(t))
}

/// `guessLang`.
pub fn guess_lang(text: &str) -> &'static str {
    if re!(r"[æøå]|\b(hvad|jeg|har|kalender|i morgen|i dag)\b", "i").test(text) {
        "da"
    } else if re!(
        r"\b(cosa|ho|domani|oggi|calendario|che|aggiungi|metti|impegni|alle)\b",
        "i"
    )
    .test(text)
    {
        "it"
    } else {
        "en"
    }
}

/// A calendar question: days `from..=to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub from: String,
    pub to: String,
    pub week: bool,
    pub lang: &'static str,
}

/// `parseCalendarQuestion`.
pub fn parse_question(text: &str, now: DateTime<Utc>, tz: Tz) -> Option<Question> {
    let t = text.trim();
    if t.is_empty() || lucina_store::timeutil::utf16_len(t) > 300 || is_create(t) {
        return None;
    }
    let cal_text = w(NOT_CAL_SCHEDULE).replace_all(t, " ");
    let asks = (w(CAL_WORDS).test(&cal_text) && (w(QUESTION_CUES).test(t) || t.ends_with('?')))
        || ask_day(t);
    if !asks {
        return None;
    }
    let lang = guess_lang(t);
    if let Some(c) = w(NEXT_DAYS).exec(t) {
        if let Ok(n @ 2..=31) = group(&c, 1).parse::<i64>() {
            return Some(Question {
                from: ymd_plus(now, tz, 0),
                to: ymd_plus(now, tz, n - 1),
                week: true,
                lang,
            });
        }
    }
    if w(NEXT_WEEK).test(t) {
        let monday = 7 - i64::from(local_date(now, tz).weekday().num_days_from_monday());
        return Some(Question {
            from: ymd_plus(now, tz, monday),
            to: ymd_plus(now, tz, monday + 6),
            week: true,
            lang,
        });
    }
    if w(WEEK_WORDS).test(t) {
        return Some(Question {
            from: ymd_plus(now, tz, 0),
            to: ymd_plus(now, tz, 6),
            week: true,
            lang,
        });
    }
    let ymd = day_of(t, now, tz)
        .map(|d| d.0)
        .unwrap_or_else(|| ymd_plus(now, tz, 0));
    Some(Question {
        from: ymd.clone(),
        to: ymd,
        week: false,
        lang,
    })
}

/// An event to create.
#[derive(Debug, Clone, PartialEq)]
pub struct Create {
    /// `{title, start, end?}` for `POST /v1/calendar/events`.
    pub event: Value,
    pub all_day: bool,
    pub lang: &'static str,
}

/// `parseCalendarCreate`: `Ok(create)`, `Err((error, lang))`, or `None`.
pub fn parse_create(
    text: &str,
    now: DateTime<Utc>,
    tz: Tz,
) -> Option<Result<Create, (&'static str, &'static str)>> {
    let t = text.trim();
    if t.is_empty() || lucina_store::timeutil::utf16_len(t) > 300 || !is_create(t) {
        return None;
    }
    let lang = guess_lang(t);
    let day = day_of(t, now, tz);
    let time = time_of(t);
    let mut removers: Vec<Remover> = Vec::new();
    if let Some((_, r)) = &day {
        removers.push(r.clone());
    }
    if let Some((_, _, r)) = &time {
        removers.push(r.clone());
    }
    removers.push(Remover::Dynamic(words_src("oggi|today|i dag")));
    let title: String = clean_title(t, &removers).chars().take(200).collect();
    if title.is_empty() {
        return Some(Err(("missing_title", lang)));
    }
    if day.is_none() && time.is_none() {
        return Some(Err(("missing_when", lang)));
    }
    let mut ymd = day
        .as_ref()
        .map(|d| d.0.clone())
        .unwrap_or_else(|| ymd_plus(now, tz, 0));
    // An impossible date ("kl. 9.30" read as day 9 of month 30): Node throws
    // `RangeError: Invalid time value` here (a 500); ask instead.
    if NaiveDate::parse_from_str(&ymd, "%Y-%m-%d").is_err() {
        return Some(Err(("missing_when", lang)));
    }
    let Some((hh, mm, _)) = time else {
        return Some(Ok(Create {
            event: json!({"title": title, "start": ymd}),
            all_day: true,
            lang,
        }));
    };
    let mut start = local_iso(&ymd, hh, mm, tz)?;
    let parse = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .ok()
    };
    if day.is_none() && parse(&start).is_some_and(|s| s <= now) {
        ymd = ymd_plus(now, tz, 1);
        start = local_iso(&ymd, hh, mm, tz)?;
    }
    let start_at = parse(&start)?;
    if start_at <= now {
        return Some(Err(("in_past", lang)));
    }
    let end_local = (start_at + Duration::hours(1)).with_timezone(&tz);
    let end = local_iso(
        &end_local.format("%Y-%m-%d").to_string(),
        end_local.hour(),
        end_local.minute(),
        tz,
    )?;
    Some(Ok(Create {
        event: json!({"title": title, "start": start, "end": end}),
        all_day: false,
        lang,
    }))
}

fn clean_title(text: &str, removers: &[Remover]) -> String {
    let mut t = text.to_owned();
    for r in removers {
        t = r.apply(&t);
    }
    let t = w(r"aggiungi(?:mi)?|aggiungere|metti(?:mi)?|mettere|segna(?:mi)?|inserisci|crea|fissa(?:mi)?|add|put|schedule|create|book|tilføj|sæt|læg|opret|skriv").replace(&t, " ").into_owned();
    let t = w(r"(?:in|al|nel|sul|nell')\s*(?:mio\s+)?(?:calendario|agenda)|(?:to|in|on|into)\s+(?:my\s+)?calendar|(?:i|til|ind\s+i)\s+(?:min\s+)?kalender(?:en)?").replace(&t, " ").into_owned();
    let t = w(r"per favore|please|tak|venligst|lucina")
        .replace(&t, " ")
        .into_owned();
    let t = re!(r"[,;:!?]+").replace_all(&t, " ");
    let t = re!(r"\s+").replace_all(&t, " ");
    let t = re!(r"^(?:un|una|uno|il|la|lo|l'|a|an|the|en|et)\s+", "i")
        .replace(t.trim(), "")
        .into_owned();
    let t = re!(r"\s+(?:di|on|for|the|il|la|på|den|om)$", "i")
        .replace(&t, "")
        .into_owned();
    let t = t.trim();
    let mut chars = t.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------- formatting

fn hm(iso: &str, tz: Tz) -> String {
    DateTime::parse_from_rfc3339(iso)
        .map(|d| d.with_timezone(&tz).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// `Intl.DateTimeFormat(locale, {weekday: long, day: numeric, month: long})`.
pub fn day_label(ymd: &str, lang: &str) -> String {
    let Ok(d) = NaiveDate::parse_from_str(ymd, "%Y-%m-%d") else {
        return ymd.to_owned();
    };
    let wd = d.weekday().num_days_from_monday() as usize;
    let m = d.month0() as usize;
    match lang {
        "it" => {
            const W: [&str; 7] = [
                "lunedì",
                "martedì",
                "mercoledì",
                "giovedì",
                "venerdì",
                "sabato",
                "domenica",
            ];
            const M: [&str; 12] = [
                "gennaio",
                "febbraio",
                "marzo",
                "aprile",
                "maggio",
                "giugno",
                "luglio",
                "agosto",
                "settembre",
                "ottobre",
                "novembre",
                "dicembre",
            ];
            format!("{} {} {}", W[wd], d.day(), M[m])
        }
        "da" => {
            const W: [&str; 7] = [
                "mandag", "tirsdag", "onsdag", "torsdag", "fredag", "lørdag", "søndag",
            ];
            const M: [&str; 12] = [
                "januar",
                "februar",
                "marts",
                "april",
                "maj",
                "juni",
                "juli",
                "august",
                "september",
                "oktober",
                "november",
                "december",
            ];
            format!("{} {}. {}", W[wd], d.day(), M[m])
        }
        _ => {
            const W: [&str; 7] = [
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ];
            const M: [&str; 12] = [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ];
            format!("{} {} {}", W[wd], d.day(), M[m])
        }
    }
}

/// `eventLines`: one line per event in the owner's zone.
pub fn event_lines(events: &[Value], tz: Tz, lang: &str) -> Vec<String> {
    let all_day = match lang {
        "it" => "tutto il giorno",
        "da" => "hele dagen",
        _ => "all day",
    };
    events
        .iter()
        .map(|e| {
            let when = if e["allDay"].as_bool().unwrap_or(false) {
                all_day.to_owned()
            } else {
                format!(
                    "{}–{}",
                    hm(e["start"].as_str().unwrap_or(""), tz),
                    hm(e["end"].as_str().unwrap_or(""), tz)
                )
            };
            let s = |k: &str| e[k].as_str().filter(|v| !v.is_empty());
            let cal = s("calendar").map(|c| format!(" ({c})")).unwrap_or_default();
            let place = s("location").map(|l| format!(" @ {l}")).unwrap_or_default();
            format!("{when} {}{cal}{place}", e["title"].as_str().unwrap_or(""))
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn relative_day(q: &Question, now: DateTime<Utc>, tz: Tz) -> String {
    let (today, tomorrow) = match q.lang {
        "it" => ("Oggi", "Domani"),
        "da" => ("I dag", "I morgen"),
        _ => ("Today", "Tomorrow"),
    };
    if q.from == ymd_plus(now, tz, 0) {
        return today.into();
    }
    if q.from == ymd_plus(now, tz, 1) {
        return tomorrow.into();
    }
    let label = day_label(&q.from, q.lang);
    if q.lang == "en" {
        format!("On {label}")
    } else {
        capitalize(&label)
    }
}

/// `formatCalendarAnswer`.
pub fn format_answer(q: &Question, events: &[Value], now: DateTime<Utc>, tz: Tz) -> String {
    let lang = q.lang;
    if q.week {
        let range = range_words(q, now, tz);
        if events.is_empty() {
            return match lang {
                "it" => format!("{} non hai niente in calendario.", capitalize(&range)),
                "da" => format!("Du har intet i kalenderen {range}."),
                _ => format!("Nothing on your calendar {range}."),
            };
        }
        let mut by_day: Vec<(String, Vec<Value>)> = Vec::new();
        for e in events {
            let ymd = if e["allDay"].as_bool().unwrap_or(false) {
                e["start"].as_str().unwrap_or("").chars().take(10).collect()
            } else {
                DateTime::parse_from_rfc3339(e["start"].as_str().unwrap_or(""))
                    .map(|d| d.with_timezone(&tz).format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            };
            match by_day.iter_mut().find(|(d, _)| *d == ymd) {
                Some((_, l)) => l.push(e.clone()),
                None => by_day.push((ymd, vec![e.clone()])),
            }
        }
        let head = format!("{}:", capitalize(&range));
        let parts: Vec<String> = by_day
            .iter()
            .map(|(ymd, l)| {
                format!(
                    "{}: {}",
                    day_label(ymd, lang),
                    event_lines(l, tz, lang).join("; ")
                )
            })
            .collect();
        return format!("{head}\n{}", parts.join("\n"));
    }
    let when = relative_day(q, now, tz);
    if events.is_empty() {
        return match lang {
            "it" => format!("{when} non hai niente in calendario."),
            "da" => format!("{when} har du intet i kalenderen."),
            _ => format!("{when} your calendar is clear."),
        };
    }
    let lines = event_lines(events, tz, lang).join("; ");
    match lang {
        "it" => format!("{when} hai: {lines}."),
        "da" => format!("{when} har du: {lines}."),
        _ => format!("{when} you have: {lines}."),
    }
}

/// `whenText`.
pub fn when_text(c: &Create, tz: Tz) -> String {
    let start = c.event["start"].as_str().unwrap_or("");
    let day = day_label(&start.chars().take(10).collect::<String>(), c.lang);
    if c.all_day {
        return day;
    }
    let at = match c.lang {
        "it" => "alle",
        "da" => "kl.",
        _ => "at",
    };
    format!("{day} {at} {}", hm(start, tz))
}

/// `confirmQuestion`.
pub fn confirm_question(c: &Create, tz: Tz) -> String {
    let title = c.event["title"].as_str().unwrap_or("");
    let when = when_text(c, tz);
    match c.lang {
        "it" => {
            format!("Confermi? Aggiungo «{title}» {when} al tuo calendario. Rispondi sì o no.")
        }
        "da" => {
            format!("Skal jeg tilføje \"{title}\" {when} i din kalender? Svar ja eller nej.")
        }
        _ => format!("Shall I add \"{title}\" {when} to your calendar? Reply yes or no."),
    }
}

/// `classifyYesNo`.
pub fn classify_yes_no(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let t = re!(r"[^\p{L}\p{N}' ]+", "u").replace_all(&t, " ");
    let t = re!(r"\s+").replace_all(&t, " ");
    let t = t.trim();
    if re!(r"^(?:s[iìí]|ok(?:ay)?|va bene|confermo|conferma|certo|procedi|vai|fallo|yes|yep|yeah|sure|do it|go ahead|confirm(?:ed)?|ja|jep|bekræft|gør det)(?:\s+(?:grazie|please|tak|procedi|confermo|fallo))?$", "iu").test(t) {
        "yes"
    } else if re!(r"^(?:no|nope|annulla|lascia stare|non farlo|no grazie|cancel|stop|don'?t|nej|lad være|nej tak)$", "iu").test(t) {
        "no"
    } else {
        "other"
    }
}

/// The texts of a calendar turn, by language.
pub fn text(lang: &str, key: &str) -> &'static str {
    match (key, lang) {
        ("unclear", "it") => "Cosa aggiungo e quando? Per esempio: «aggiungi in calendario dentista domani alle 9:30».",
        ("unclear", "da") => "Hvad skal jeg tilføje, og hvornår? For eksempel: \"sæt tandlæge i kalenderen i morgen kl. 9.30\".",
        ("unclear", _) => "What should I add, and when? For example: \"add dentist to my calendar tomorrow at 9:30\".",
        ("unavailable", "it") => "Non riesco a leggere il calendario adesso.",
        ("unavailable", "da") => "Jeg kan ikke læse din kalender lige nu.",
        ("unavailable", _) => "I can't read your calendar right now.",
        ("reconnect_note", "it") => " (Google Calendar va ricollegato in Collegamenti.)",
        ("reconnect_note", "da") => " (Google Kalender skal forbindes igen under Forbindelser.)",
        ("reconnect_note", _) => " (Google Calendar needs reconnecting in Links.)",
        ("reconnect", "it") => "Non ci sono riuscita: Google Calendar va ricollegato in Secretary → Collegamenti.",
        ("reconnect", "da") => "Det lykkedes ikke: Google Kalender skal forbindes igen i Secretary → Forbindelser.",
        ("reconnect", _) => "I couldn't: Google Calendar needs reconnecting in Secretary → Links.",
        ("declined", "it") => "Va bene, non lo aggiungo.",
        ("declined", "da") => "Okay, jeg tilføjer den ikke.",
        ("declined", _) => "Okay, I won't add it.",
        _ => "",
    }
}

pub fn created_text(c: &Create, tz: Tz) -> String {
    let title = c.event["title"].as_str().unwrap_or("");
    let when = when_text(c, tz);
    match c.lang {
        "it" => format!("Fatto: «{title}» è in calendario, {when}."),
        "da" => format!("Klaret: \"{title}\" er i din kalender, {when}."),
        _ => format!("Done: \"{title}\" is on your calendar, {when}."),
    }
}

/// The owner's own request, created at once: "Added to your calendar: <title>, <day date time>",
/// plus the event's Google Calendar link when there is one.
pub fn added_text(c: &Create, tz: Tz, link: Option<&str>) -> String {
    let title = c.event["title"].as_str().unwrap_or("");
    let when = when_text(c, tz);
    let (head, open) = match c.lang {
        "it" => (format!("Aggiunto al calendario: «{title}», {when}."), "Apri in Google Calendar"),
        "da" => (format!("Tilføjet til din kalender: \"{title}\", {when}."), "Åbn i Google Kalender"),
        _ => (format!("Added to your calendar: \"{title}\", {when}."), "Open in Google Calendar"),
    };
    match link.filter(|l| l.starts_with("https://")) {
        Some(l) => format!("{head} [{open}]({l})"),
        None => head,
    }
}

/// Why a direct create did not happen, for the reply and the proposal's `reason`.
pub fn reason_text(lang: &str, code: &str) -> String {
    match (code, lang) {
        ("google_reconnect_needed", "it") => "Google Calendar va ricollegato".into(),
        ("google_reconnect_needed", "da") => "Google Kalender skal forbindes igen".into(),
        ("google_reconnect_needed", _) => "Google Calendar needs reconnecting".into(),
        ("google_not_connected", "it") => "Google Calendar non è collegato".into(),
        ("google_not_connected", "da") => "Google Kalender er ikke forbundet".into(),
        ("google_not_connected", _) => "Google Calendar isn't connected".into(),
        (_, "it") => format!("Google Calendar ha risposto con un errore ({code})"),
        (_, "da") => format!("Google Kalender svarede med en fejl ({code})"),
        _ => format!("Google Calendar answered with an error ({code})"),
    }
}

/// A proposal waiting in Links: `reason` says why it was not created directly
/// (`None`: the caller may only propose, e.g. an MCP client).
pub fn proposed_text(c: &Create, tz: Tz, reason: Option<&str>) -> String {
    let title = c.event["title"].as_str().unwrap_or("");
    let when = when_text(c, tz);
    match (c.lang, reason) {
        ("it", Some(r)) => format!("Non sono riuscita ad aggiungerlo direttamente ({r}), quindi l'ho proposto: «{title}», {when}. Confermalo in Secretary → Collegamenti."),
        ("da", Some(r)) => format!("Jeg kunne ikke tilføje den direkte ({r}), så jeg har foreslået den: \"{title}\", {when}. Bekræft den i Secretary → Forbindelser."),
        (_, Some(r)) => format!("I couldn't add it directly ({r}), so I proposed it instead: \"{title}\", {when}. Confirm it in Secretary → Links."),
        ("it", None) => format!("Ho proposto «{title}», {when}: confermalo in Secretary → Collegamenti."),
        ("da", None) => format!("Jeg har foreslået \"{title}\", {when}: bekræft den i Secretary → Forbindelser."),
        _ => format!("I proposed \"{title}\", {when}: confirm it in Secretary → Links."),
    }
}

/// How long the undo of a direct create stays available.
pub const UNDO_MINUTES: i64 = 10;

/// The Undo button: `(label, text it sends)`.
pub fn undo_action(lang: &str) -> (&'static str, &'static str) {
    match lang {
        "it" => ("Annulla", "annulla"),
        "da" => ("Fortryd", "fortryd"),
        _ => ("Undo", "undo"),
    }
}

/// "annulla" / "undo" / "fortryd" (the Undo button's text, or the owner typing it).
pub fn is_undo(text: &str) -> bool {
    let t = text.to_lowercase();
    let t = re!(r"[^\p{L}\p{N}' ]+", "u").replace_all(&t, " ");
    let t = re!(r"\s+").replace_all(&t, " ");
    re!(r"^(?:annulla(?:lo)?|annulla l'evento|toglilo|undo|undo that|undo it|remove it|fortryd|fortryd det|slet den)$", "iu").test(t.trim())
}

pub fn undone_text(lang: &str, title: &str) -> String {
    match lang {
        "it" => format!("Annullato: ho tolto «{title}» dal calendario."),
        "da" => format!("Fortrudt: \"{title}\" er fjernet fra din kalender."),
        _ => format!("Undone: \"{title}\" is off your calendar."),
    }
}

pub fn undo_failed_text(lang: &str, code: &str) -> String {
    match lang {
        "it" => format!("Non sono riuscita a toglierlo ({code}). Puoi eliminarlo da Google Calendar."),
        "da" => format!("Jeg kunne ikke fjerne den ({code}). Du kan slette den i Google Kalender."),
        _ => format!("I couldn't remove it ({code}). You can delete it in Google Calendar."),
    }
}

pub fn create_failed_text(lang: &str, code: &str) -> String {
    match lang {
        "it" => format!("Non sono riuscita ad aggiungerlo ({code}). Riprova tra poco."),
        "da" => format!("Jeg kunne ikke tilføje den ({code}). Prøv igen om lidt."),
        _ => format!("I couldn't add it ({code}). Try again in a moment."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        // Thursday 22 October 2026, 13:00 in Rome.
        Utc.with_ymd_and_hms(2026, 10, 22, 11, 0, 0).unwrap()
    }
    const TZ: Tz = chrono_tz::Europe::Rome;

    #[test]
    fn questions() {
        let q = parse_question("cosa ho in calendario domani?", now(), TZ).unwrap();
        assert_eq!(
            (q.from.as_str(), q.lang, q.week),
            ("2026-10-23", "it", false)
        );
        let q = parse_question("what's on my calendar this week?", now(), TZ).unwrap();
        assert_eq!(
            (q.from.as_str(), q.to.as_str(), q.week),
            ("2026-10-22", "2026-10-28", true)
        );
        // "the next N days" and "next week" (not this week: the bare "settimana" used to take it).
        let q = parse_question("Cosa ho in calendario nei prossimi 7 giorni?", now(), TZ).unwrap();
        assert_eq!(
            (q.from.as_str(), q.to.as_str(), q.week),
            ("2026-10-22", "2026-10-28", true)
        );
        assert_eq!(range_words(&q, now(), TZ), "questa settimana");
        let q = parse_question("What is on my calendar in the next 10 days?", now(), TZ).unwrap();
        assert_eq!(
            (q.from.as_str(), q.to.as_str()),
            ("2026-10-22", "2026-10-31")
        );
        assert_eq!(range_words(&q, now(), TZ), "in the next 10 days");
        let q = parse_question("cosa ho in calendario la prossima settimana?", now(), TZ).unwrap();
        assert_eq!(
            (q.from.as_str(), q.to.as_str()),
            ("2026-10-26", "2026-11-01")
        );
        assert_eq!(range_words(&q, now(), TZ), "la prossima settimana");
        let q = parse_question("hvad har jeg i kalenderen de næste 3 dage?", now(), TZ).unwrap();
        assert_eq!((q.to.as_str(), q.lang), ("2026-10-24", "da"));
        assert_eq!(range_words(&q, now(), TZ), "de næste 3 dage");
        let q = parse_question("hvad har jeg i kalenderen i morgen", now(), TZ).unwrap();
        assert_eq!((q.from.as_str(), q.lang), ("2026-10-23", "da"));
        assert!(parse_question("ciao come stai", now(), TZ).is_none());
        assert!(parse_question(
            "aggiungi in calendario dentista domani alle 9:30",
            now(),
            TZ
        )
        .is_none());
        let ev = vec![
            json!({"title": "Dentista", "start": "2026-10-23T09:30:00+02:00", "end": "2026-10-23T10:30:00+02:00", "allDay": false}),
        ];
        let q = parse_question("cosa ho in calendario domani?", now(), TZ).unwrap();
        assert_eq!(
            format_answer(&q, &ev, now(), TZ),
            "Domani hai: 09:30–10:30 Dentista."
        );
        let q = parse_question("cosa ho in calendario lunedì?", now(), TZ).unwrap();
        assert_eq!(
            format_answer(&q, &[], now(), TZ),
            "Lunedì 26 ottobre non hai niente in calendario."
        );
    }

    #[test]
    fn creates() {
        let c = parse_create(
            "aggiungi in calendario dentista domani alle 9:30",
            now(),
            TZ,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            c.event,
            json!({"title": "Dentista", "start": "2026-10-23T09:30:00+02:00", "end": "2026-10-23T10:30:00+02:00"})
        );
        assert_eq!(confirm_question(&c, TZ), "Confermi? Aggiungo «Dentista» venerdì 23 ottobre alle 09:30 al tuo calendario. Rispondi sì o no.");
        let c = parse_create("add dentist to my calendar on 30/10", now(), TZ)
            .unwrap()
            .unwrap();
        assert!(c.all_day);
        assert_eq!(c.event["start"], "2026-10-30");
        assert_eq!(
            parse_create("aggiungi in calendario", now(), TZ)
                .unwrap()
                .unwrap_err()
                .0,
            "missing_title"
        );
        // After the DST switch (25 October) the offset is +01:00.
        let c = parse_create(
            "aggiungi in calendario palestra il 27.10 alle 18",
            now(),
            TZ,
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.event["start"], "2026-10-27T18:00:00+01:00");
        assert_eq!(
            added_text(&c, TZ, Some("https://calendar.google.com/event?eid=x")),
            "Aggiunto al calendario: «Palestra», martedì 27 ottobre alle 18:00. [Apri in Google Calendar](https://calendar.google.com/event?eid=x)"
        );
        assert_eq!(
            added_text(&c, TZ, Some("javascript:alert(1)")),
            "Aggiunto al calendario: «Palestra», martedì 27 ottobre alle 18:00."
        );
        assert_eq!(
            proposed_text(&c, TZ, Some(&reason_text("it", "google_reconnect_needed"))),
            "Non sono riuscita ad aggiungerlo direttamente (Google Calendar va ricollegato), quindi l'ho proposto: «Palestra», martedì 27 ottobre alle 18:00. Confermalo in Secretary → Collegamenti."
        );
        let en = parse_create("add Claude Max 5x unlocks to my calendar on 30/10 at 23:00", now(), TZ)
            .unwrap()
            .unwrap();
        assert_eq!(
            added_text(&en, TZ, None),
            "Added to your calendar: \"Claude Max 5x unlocks\", Friday 30 October at 23:00."
        );
        assert_eq!(
            proposed_text(&en, TZ, None),
            "I proposed \"Claude Max 5x unlocks\", Friday 30 October at 23:00: confirm it in Secretary → Links."
        );
        assert_eq!(reason_text("en", "http_500"), "Google Calendar answered with an error (http_500)");
        for t in ["annulla", "Annulla!", "undo", "Undo that", "fortryd"] {
            assert!(is_undo(t), "{t}");
        }
        for t in ["annulla il promemoria", "no", "undo the last reminder"] {
            assert!(!is_undo(t), "{t}");
        }
        assert_eq!(undo_action("en"), ("Undo", "undo"));
        assert!(is_undo(undo_action("it").1) && is_undo(undo_action("da").1));
        assert_eq!(classify_yes_no("Sì, grazie!"), "yes");
        assert_eq!(classify_yes_no("no"), "no");
        assert_eq!(classify_yes_no("domani invece"), "other");
        assert_eq!(day_label("2026-10-22", "da"), "torsdag 22. oktober");
        // Node throws on this one (its own help example); Rust asks again.
        assert_eq!(
            parse_create("sæt tandlæge i kalenderen i morgen kl. 9.30", now(), TZ)
                .unwrap()
                .unwrap_err(),
            ("missing_when", "da")
        );
    }
}
