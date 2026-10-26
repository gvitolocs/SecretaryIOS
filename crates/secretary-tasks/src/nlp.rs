//! Quick add: Todoist-style parsing of one line in Italian, English and
//! Danish.
//!
//! - Dates: `today`/`oggi`/`i dag`, `tomorrow`/`domani`/`i morgen`,
//!   `dopodomani`, weekdays (`monday`, `lunedì prossimo`, `på fredag`),
//!   `next week`/`la prossima settimana`/`næste uge`, `weekend`,
//!   `next month`, `in 3 days`/`tra 3 giorni`/`om 3 dage`, `27 oct`,
//!   `27 ottobre`, `27. oktober`, `27/10`, `2026-10-27`, plus a time
//!   (`at 9am`, `alle 9`, `kl. 9.30`, `21:00`).
//! - Recurrence: `every day`, `ogni giorno`, `tutti i giorni`, `hver dag`,
//!   `daily`, `every 2 weeks`, `every mon, wed`, `ogni lunedì e giovedì`,
//!   `hver mandag`, `every weekday`/`giorni feriali`/`hverdag`,
//!   `every morning`, `every 15` (day of the month), `every!` (from the
//!   completion date).
//! - `#Project`, `/Section`, `@label`, `p1`…`p4` (or `!!1`), `{deadline}`
//!   and ` // description`.
//!
//! The date phrase is kept exactly as typed and sent to Todoist as
//! `due.string` with the phrase's language, so Todoist's own parser has the
//! last word and recurrence round-trips. The date computed here is for the
//! instant local display only.

use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Weekday};

use crate::models::{Parsed, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Every n days.
    Days(u32),
    /// Every n weeks, on these weekdays (empty: the weekday of the base).
    Weeks(u32, Vec<Weekday>),
    /// Every n months, on this day of the month (None: the base's day).
    Months(u32, Option<u32>),
    Years(u32),
    /// Monday to Friday.
    Workdays,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recurrence {
    pub rule: Rule,
    /// `every!` / `ogni!` / `hver!`: counted from the completion date.
    pub from_completion: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateMatch {
    /// Byte range in the input.
    pub start: usize,
    pub end: usize,
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    pub recurrence: Option<Recurrence>,
    /// The phrase's language (`it`, `en`, `da`), when its words tell.
    pub lang: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub today: NaiveDate,
    /// Weekday of "next week" (1 = Monday).
    pub next_week: u8,
}

impl Ctx {
    pub fn new(today: NaiveDate) -> Self {
        Self {
            today,
            next_week: 1,
        }
    }
}

#[derive(Clone, Debug)]
struct Tok {
    start: usize,
    end: usize,
    /// Lowercased, Italian accents folded, edge punctuation trimmed.
    w: String,
    /// The raw token ended with `,` (lists of weekdays).
    comma: bool,
}

fn fold(c: char) -> char {
    match c {
        'à' | 'á' => 'a',
        'è' | 'é' => 'e',
        'ì' | 'í' => 'i',
        'ò' | 'ó' => 'o',
        'ù' | 'ú' => 'u',
        _ => c,
    }
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut start = None;
    let push = |out: &mut Vec<Tok>, s: usize, e: usize| {
        let raw = &text[s..e];
        let trim: &[char] = &[',', ';', '(', ')', '"', '\'', '?', '¿', '«', '»'];
        let lead = raw.len() - raw.trim_start_matches(trim).len();
        let core = raw.trim_matches(trim);
        if core.is_empty() {
            return;
        }
        let w: String = core
            .chars()
            .flat_map(char::to_lowercase)
            .map(fold)
            .collect();
        out.push(Tok {
            start: s + lead,
            end: s + lead + core.len(),
            w,
            comma: raw.ends_with(','),
        });
    };
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                push(&mut out, s, i);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push(&mut out, s, text.len());
    }
    out
}

// -- vocabulary ----------------------------------------------------------------------

const WEEKDAYS: &[(&str, Weekday, &str, bool)] = &[
    // (word, day, lang, full name: allowed without a keyword)
    ("monday", Weekday::Mon, "en", true),
    ("tuesday", Weekday::Tue, "en", true),
    ("wednesday", Weekday::Wed, "en", true),
    ("thursday", Weekday::Thu, "en", true),
    ("friday", Weekday::Fri, "en", true),
    ("saturday", Weekday::Sat, "en", true),
    ("sunday", Weekday::Sun, "en", true),
    ("mon", Weekday::Mon, "en", false),
    ("tue", Weekday::Tue, "en", false),
    ("tues", Weekday::Tue, "en", false),
    ("wed", Weekday::Wed, "en", false),
    ("thu", Weekday::Thu, "en", false),
    ("thurs", Weekday::Thu, "en", false),
    ("fri", Weekday::Fri, "en", false),
    ("sat", Weekday::Sat, "en", false),
    ("sun", Weekday::Sun, "en", false),
    ("lunedi", Weekday::Mon, "it", true),
    ("martedi", Weekday::Tue, "it", true),
    ("mercoledi", Weekday::Wed, "it", true),
    ("giovedi", Weekday::Thu, "it", true),
    ("venerdi", Weekday::Fri, "it", true),
    ("sabato", Weekday::Sat, "it", true),
    ("domenica", Weekday::Sun, "it", true),
    ("lun", Weekday::Mon, "it", false),
    ("mar", Weekday::Tue, "it", false),
    ("mer", Weekday::Wed, "it", false),
    ("gio", Weekday::Thu, "it", false),
    ("ven", Weekday::Fri, "it", false),
    ("sab", Weekday::Sat, "it", false),
    ("dom", Weekday::Sun, "it", false),
    ("mandag", Weekday::Mon, "da", true),
    ("tirsdag", Weekday::Tue, "da", true),
    ("onsdag", Weekday::Wed, "da", true),
    ("torsdag", Weekday::Thu, "da", true),
    ("fredag", Weekday::Fri, "da", true),
    ("lørdag", Weekday::Sat, "da", true),
    ("søndag", Weekday::Sun, "da", true),
    ("man", Weekday::Mon, "da", false),
    ("tir", Weekday::Tue, "da", false),
    ("ons", Weekday::Wed, "da", false),
    ("tor", Weekday::Thu, "da", false),
    ("fre", Weekday::Fri, "da", false),
    ("lør", Weekday::Sat, "da", false),
    ("søn", Weekday::Sun, "da", false),
];

const MONTHS: &[(&str, u32, &str)] = &[
    ("january", 1, "en"),
    ("february", 2, "en"),
    ("march", 3, "en"),
    ("april", 4, "en"),
    ("may", 5, "en"),
    ("june", 6, "en"),
    ("july", 7, "en"),
    ("august", 8, "en"),
    ("september", 9, "en"),
    ("october", 10, "en"),
    ("november", 11, "en"),
    ("december", 12, "en"),
    ("jan", 1, "en"),
    ("feb", 2, "en"),
    ("mar", 3, "en"),
    ("apr", 4, "en"),
    ("jun", 6, "en"),
    ("jul", 7, "en"),
    ("aug", 8, "en"),
    ("sep", 9, "en"),
    ("sept", 9, "en"),
    ("oct", 10, "en"),
    ("nov", 11, "en"),
    ("dec", 12, "en"),
    ("gennaio", 1, "it"),
    ("febbraio", 2, "it"),
    ("marzo", 3, "it"),
    ("aprile", 4, "it"),
    ("maggio", 5, "it"),
    ("giugno", 6, "it"),
    ("luglio", 7, "it"),
    ("agosto", 8, "it"),
    ("settembre", 9, "it"),
    ("ottobre", 10, "it"),
    ("novembre", 11, "it"),
    ("dicembre", 12, "it"),
    ("gen", 1, "it"),
    ("mag", 5, "it"),
    ("giu", 6, "it"),
    ("lug", 7, "it"),
    ("ago", 8, "it"),
    ("set", 9, "it"),
    ("ott", 10, "it"),
    ("dic", 12, "it"),
    ("januar", 1, "da"),
    ("februar", 2, "da"),
    ("marts", 3, "da"),
    ("maj", 5, "da"),
    ("juni", 6, "da"),
    ("juli", 7, "da"),
    ("oktober", 10, "da"),
    ("okt", 10, "da"),
];

fn weekday(w: &str, full_only: bool) -> Option<(Weekday, &'static str)> {
    WEEKDAYS
        .iter()
        .find(|(n, _, _, full)| *n == w && (*full || !full_only))
        .map(|(_, d, l, _)| (*d, *l))
}

fn month(w: &str) -> Option<(u32, &'static str)> {
    let w = w.trim_end_matches('.');
    MONTHS
        .iter()
        .find(|(n, _, _)| *n == w)
        .map(|(_, m, l)| (*m, *l))
}

/// `(word, lang)` lists; a match tells the phrase's language.
const TODAY: &[(&str, &str)] = &[
    ("today", "en"),
    ("tod", "en"),
    ("oggi", "it"),
    ("idag", "da"),
];
const TOMORROW: &[(&str, &str)] = &[
    ("tomorrow", "en"),
    ("tmr", "en"),
    ("tom", "en"),
    ("domani", "it"),
    ("imorgen", "da"),
];
const EVERY: &[(&str, &str)] = &[
    ("every", "en"),
    ("ev", "en"),
    ("ogni", "it"),
    ("hver", "da"),
    ("hvert", "da"),
    ("hver.", "da"),
];
const UNIT_DAY: &[(&str, &str)] = &[
    ("day", "en"),
    ("days", "en"),
    ("d", "en"),
    ("giorno", "it"),
    ("giorni", "it"),
    ("di", "it"),
    ("dag", "da"),
    ("dage", "da"),
    ("dagen", "da"),
];
const UNIT_WEEK: &[(&str, &str)] = &[
    ("week", "en"),
    ("weeks", "en"),
    ("w", "en"),
    ("settimana", "it"),
    ("settimane", "it"),
    ("uge", "da"),
    ("uger", "da"),
];
const UNIT_MONTH: &[(&str, &str)] = &[
    ("month", "en"),
    ("months", "en"),
    ("mese", "it"),
    ("mesi", "it"),
    ("måned", "da"),
    ("måneder", "da"),
    ("maaned", "da"),
];
const UNIT_YEAR: &[(&str, &str)] = &[
    ("year", "en"),
    ("years", "en"),
    ("anno", "it"),
    ("anni", "it"),
    ("år", "da"),
];
const WORKDAY: &[(&str, &str)] = &[
    ("weekday", "en"),
    ("weekdays", "en"),
    ("workday", "en"),
    ("workdays", "en"),
    ("hverdag", "da"),
    ("hverdage", "da"),
    ("feriale", "it"),
    ("feriali", "it"),
    ("lavorativo", "it"),
    ("lavorativi", "it"),
];
const NEXT: &[(&str, &str)] = &[
    ("next", "en"),
    ("prossimo", "it"),
    ("prossima", "it"),
    ("næste", "da"),
    ("naeste", "da"),
];
const NUMBER_WORDS: &[(&str, u32)] = &[
    ("a", 1),
    ("an", 1),
    ("one", 1),
    ("un", 1),
    ("una", 1),
    ("uno", 1),
    ("en", 1),
    ("et", 1),
    ("two", 2),
    ("due", 2),
    ("to", 2),
    ("other", 2),
    ("three", 3),
    ("tre", 3),
];

fn find(list: &[(&str, &'static str)], w: &str) -> Option<&'static str> {
    list.iter().find(|(n, _)| *n == w).map(|(_, l)| *l)
}

// -- date math -----------------------------------------------------------------------

/// The first `day` strictly after `after`.
pub fn next_weekday(after: NaiveDate, day: Weekday) -> NaiveDate {
    let delta = (day.num_days_from_monday() as i64 - after.weekday().num_days_from_monday() as i64)
        .rem_euclid(7);
    after + Duration::days(if delta == 0 { 7 } else { delta })
}

/// `day` on or after `from`.
fn weekday_on_or_after(from: NaiveDate, day: Weekday) -> NaiveDate {
    next_weekday(from - Duration::days(1), day)
}

pub fn add_months(d: NaiveDate, n: u32, day: Option<u32>) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + n as i32;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let want = day.unwrap_or(d.day());
    (1..=want)
        .rev()
        .find_map(|dd| NaiveDate::from_ymd_opt(y, m, dd))
        .unwrap_or(d)
}

fn is_workday(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

impl Recurrence {
    /// The first occurrence on or after `from` (for a new recurring task).
    pub fn first_on_or_after(&self, from: NaiveDate) -> NaiveDate {
        match &self.rule {
            Rule::Days(_) | Rule::Years(_) => from,
            Rule::Weeks(_, days) if days.is_empty() => from,
            Rule::Weeks(_, days) => days
                .iter()
                .map(|d| weekday_on_or_after(from, *d))
                .min()
                .unwrap_or(from),
            Rule::Months(_, None) => from,
            Rule::Months(_, Some(day)) => {
                let this = add_months(
                    NaiveDate::from_ymd_opt(from.year(), from.month(), 1).unwrap_or(from),
                    0,
                    Some(*day),
                );
                if this >= from && this.day() == *day {
                    this
                } else {
                    add_months(this, 1, Some(*day))
                }
            }
            Rule::Workdays => {
                let mut d = from;
                while !is_workday(d) {
                    d += Duration::days(1);
                }
                d
            }
        }
    }

    /// This engine's guess of the next occurrence after completing an
    /// occurrence due `due` on `today`: the first one after both (Todoist
    /// recomputes it; the sync replaces the guess).
    pub fn next_after(&self, due: NaiveDate, today: NaiveDate) -> NaiveDate {
        let base = if self.from_completion { today } else { due };
        let floor = due.max(today);
        let mut d = base;
        for _ in 0..4000 {
            d = match &self.rule {
                Rule::Days(n) => d + Duration::days(i64::from((*n).max(1))),
                Rule::Weeks(n, days) if days.is_empty() => {
                    d + Duration::days(7 * i64::from((*n).max(1)))
                }
                Rule::Weeks(_, days) => days
                    .iter()
                    .map(|w| next_weekday(d, *w))
                    .min()
                    .unwrap_or(d + Duration::days(7)),
                Rule::Months(n, day) => add_months(d, (*n).max(1), *day),
                Rule::Years(n) => add_months(d, 12 * (*n).max(1), None),
                Rule::Workdays => {
                    let mut x = d + Duration::days(1);
                    while !is_workday(x) {
                        x += Duration::days(1);
                    }
                    x
                }
            };
            if d > floor {
                return d;
            }
        }
        floor + Duration::days(1)
    }
}

// -- parser --------------------------------------------------------------------------

struct P<'a> {
    t: &'a [Tok],
    ctx: Ctx,
}

#[derive(Clone, Debug)]
struct M {
    end: usize,
    date: NaiveDate,
    time: Option<NaiveTime>,
    rec: Option<Recurrence>,
    lang: Option<&'static str>,
}

fn merge_lang(a: Option<&'static str>, b: Option<&'static str>) -> Option<&'static str> {
    match (a, b) {
        (Some(x), _) if x != "en" => Some(x),
        (_, Some(y)) if y != "en" => Some(y),
        (x, y) => x.or(y),
    }
}

fn parse_clock(w: &str) -> Option<(u32, u32, bool)> {
    // "9", "09", "9:30", "9.30", "21:00", "9am", "9:30pm"; returns (h, m, explicit)
    let (num, ampm) = if let Some(n) = w.strip_suffix("am") {
        (n, Some(false))
    } else if let Some(n) = w.strip_suffix("pm") {
        (n, Some(true))
    } else {
        (w, None)
    };
    let num = num.trim_end_matches('.');
    let (h, m, colon) = match num.split_once([':', '.']) {
        Some((h, m)) => (h, m, true),
        None => (num, "0", false),
    };
    if h.is_empty() || h.len() > 2 || m.len() > 2 || (colon && m.len() != 2) {
        return None;
    }
    let mut h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if m > 59 {
        return None;
    }
    match ampm {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h = match (h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            };
        }
        None if h > 23 => return None,
        None => {}
    }
    Some((h, m, ampm.is_some() || colon))
}

impl P<'_> {
    fn w(&self, i: usize) -> Option<&str> {
        self.t.get(i).map(|t| t.w.as_str())
    }

    fn number(&self, i: usize) -> Option<u32> {
        let w = self.w(i)?;
        if let Some((_, n)) = NUMBER_WORDS.iter().find(|(n, _)| *n == w) {
            return Some(*n);
        }
        let digits = w.trim_end_matches('.');
        if digits.is_empty() || digits.len() > 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }

    /// A time at `i`: `[at|alle|ore|kl|kl.|@] (9 | 9am | 9 am | 21:30 | noon)`.
    /// Bare numbers need the keyword; "at 9" needs `strict = false`.
    fn time(&self, i: usize, strict: bool) -> Option<(usize, NaiveTime, Option<&'static str>)> {
        let kw = match self.w(i)? {
            "at" | "@" => Some("en"),
            "alle" | "ore" | "h" => Some("it"),
            "kl" | "kl." | "klokken" => Some("da"),
            _ => None,
        };
        let j = if kw.is_some() { i + 1 } else { i };
        let w = self.w(j)?;
        if matches!(w, "noon" | "mezzogiorno" | "middag") {
            return Some((j + 1, NaiveTime::from_hms_opt(12, 0, 0)?, kw));
        }
        let (mut h, m, mut explicit) = parse_clock(w)?;
        let mut end = j + 1;
        if !w.ends_with("am") && !w.ends_with("pm") {
            match self.w(j + 1) {
                Some("am") | Some("a.m.") if (1..=12).contains(&h) => {
                    if h == 12 {
                        h = 0;
                    }
                    explicit = true;
                    end += 1;
                }
                Some("pm") | Some("p.m.") if (1..=12).contains(&h) => {
                    if h != 12 {
                        h += 12;
                    }
                    explicit = true;
                    end += 1;
                }
                _ => {}
            }
        }
        let ok = match kw {
            Some("it") | Some("da") => true,
            Some(_) => explicit || !strict,
            None => explicit && (w.contains(':') || w.ends_with('m') || end > j + 1),
        };
        if !ok {
            return None;
        }
        Some((end, NaiveTime::from_hms_opt(h, m, 0)?, kw.filter(|_| true)))
    }

    /// An explicit calendar date at `i`.
    fn calendar(&self, i: usize) -> Option<(usize, NaiveDate, Option<&'static str>)> {
        let today = self.ctx.today;
        let mut i = i;
        let mut lang = None;
        if let Some(w) = self.w(i) {
            if matches!(w, "il" | "on" | "den" | "the" | "på") && self.w(i + 1).is_some() {
                lang = match w {
                    "il" => Some("it"),
                    "den" | "på" => Some("da"),
                    _ => None,
                };
                i += 1;
            }
        }
        let w = self.w(i)?;
        let fix_year = |m: u32, d: u32, y: Option<i32>| -> Option<NaiveDate> {
            match y {
                Some(y) => NaiveDate::from_ymd_opt(y, m, d),
                None => {
                    let this = NaiveDate::from_ymd_opt(today.year(), m, d)?;
                    if this < today {
                        NaiveDate::from_ymd_opt(today.year() + 1, m, d)
                    } else {
                        Some(this)
                    }
                }
            }
        };
        // 2026-10-27
        if w.len() == 10 && w.as_bytes()[4] == b'-' && w.as_bytes()[7] == b'-' {
            let d = NaiveDate::parse_from_str(w, "%Y-%m-%d").ok()?;
            return Some((i + 1, d, lang));
        }
        // 27/10, 27/10/2026, 27.10.2026, 27-10-2026
        if w.chars().next()?.is_ascii_digit() && w.contains(['/', '.', '-']) {
            let parts: Vec<&str> = w.trim_end_matches('.').split(['/', '.', '-']).collect();
            if (2..=3).contains(&parts.len())
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                let d: u32 = parts[0].parse().ok()?;
                let m: u32 = parts[1].parse().ok()?;
                let y = match parts.get(2) {
                    Some(y) if y.len() == 4 => Some(y.parse().ok()?),
                    Some(y) if y.len() == 2 => Some(2000 + y.parse::<i32>().ok()?),
                    Some(_) => return None,
                    None => None,
                };
                if parts.len() == 2 && w.contains(':') {
                    return None;
                }
                return fix_year(m, d, y).map(|d| (i + 1, d, lang));
            }
        }
        let year_at = |j: usize| -> Option<i32> {
            let y = self.w(j)?;
            (y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()))
                .then(|| y.parse().ok())
                .flatten()
        };
        // 27 oct [2026] / 27. oktober
        if let Some(d) = self.number(i).filter(|d| (1..=31).contains(d)) {
            if self
                .w(i)
                .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit() || c == '.'))
            {
                if let Some((m, l)) = self.w(i + 1).and_then(month) {
                    let y = year_at(i + 2);
                    let end = if y.is_some() { i + 3 } else { i + 2 };
                    return fix_year(m, d, y).map(|d| (end, d, merge_lang(lang, Some(l))));
                }
            }
        }
        // oct 27 [2026]
        if let Some((m, l)) = month(w) {
            if let Some(d) = self.number(i + 1).filter(|d| (1..=31).contains(d)) {
                if self
                    .w(i + 1)
                    .is_some_and(|w| w.chars().all(|c| c.is_ascii_digit()))
                {
                    let y = year_at(i + 2);
                    let end = if y.is_some() { i + 3 } else { i + 2 };
                    return fix_year(m, d, y).map(|d| (end, d, merge_lang(lang, Some(l))));
                }
            }
        }
        None
    }

    /// A relative date word or phrase at `i`.
    fn relative(&self, i: usize) -> Option<(usize, NaiveDate, Option<&'static str>)> {
        let today = self.ctx.today;
        let w = self.w(i)?;
        let next = self.w(i + 1);
        if let Some(l) = find(TODAY, w) {
            return Some((i + 1, today, Some(l)));
        }
        if let Some(l) = find(TOMORROW, w) {
            return Some((i + 1, today + Duration::days(1), Some(l)));
        }
        if matches!(w, "dopodomani" | "overmorgen") {
            let l = if w == "dopodomani" { "it" } else { "da" };
            return Some((i + 1, today + Duration::days(2), Some(l)));
        }
        if w == "i" {
            match next {
                Some("dag") => return Some((i + 2, today, Some("da"))),
                Some("morgen") => return Some((i + 2, today + Duration::days(1), Some("da"))),
                Some("overmorgen") => return Some((i + 2, today + Duration::days(2), Some("da"))),
                Some("weekenden") => {
                    return Some((i + 2, weekday_on_or_after(today, Weekday::Sat), Some("da")))
                }
                _ => {}
            }
        }
        if w == "day" && next == Some("after") && self.w(i + 2) == Some("tomorrow") {
            return Some((i + 3, today + Duration::days(2), Some("en")));
        }
        // weekend
        let weekend = |j: usize, l: Option<&'static str>| {
            Some((j, weekday_on_or_after(today, Weekday::Sat), l))
        };
        match (w, next) {
            ("weekend", _) => return weekend(i + 1, None),
            ("weekenden", _) => return weekend(i + 1, Some("da")),
            ("this", Some("weekend")) => return weekend(i + 2, Some("en")),
            ("questo" | "nel", Some("weekend")) => return weekend(i + 2, Some("it")),
            _ => {}
        }
        // next week / next month / next <weekday>
        let nw = Weekday::try_from(self.ctx.next_week.clamp(1, 7) - 1).unwrap_or(Weekday::Mon);
        let mut j = i;
        if w == "la" && next.is_some_and(|n| find(NEXT, n).is_some()) {
            j += 1;
        }
        if let Some(l) = self.w(j).and_then(|w| find(NEXT, w)) {
            let unit = self.w(j + 1)?;
            if let Some(l2) = find(UNIT_WEEK, unit) {
                return Some((
                    j + 2,
                    next_weekday(today, nw).max(today + Duration::days(1)),
                    merge_lang(Some(l), Some(l2)),
                ));
            }
            if let Some(l2) = find(UNIT_MONTH, unit) {
                let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
                return Some((
                    j + 2,
                    add_months(first, 1, Some(1)),
                    merge_lang(Some(l), Some(l2)),
                ));
            }
            if let Some((d, l2)) = weekday(unit, false) {
                return Some((j + 2, next_weekday(today, d), merge_lang(Some(l), Some(l2))));
            }
        }
        // settimana prossima / mese prossimo / lunedì prossimo
        if let Some(n) = next {
            if find(NEXT, n) == Some("it") {
                if find(UNIT_WEEK, w).is_some() {
                    return Some((i + 2, next_weekday(today, nw), Some("it")));
                }
                if find(UNIT_MONTH, w).is_some() {
                    let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?;
                    return Some((i + 2, add_months(first, 1, Some(1)), Some("it")));
                }
                if let Some((d, _)) = weekday(w, true) {
                    return Some((i + 2, next_weekday(today, d), Some("it")));
                }
            }
        }
        // in 3 days / tra 3 giorni / fra una settimana / om 2 uger
        if matches!(w, "in" | "tra" | "fra" | "om") {
            let l = match w {
                "tra" | "fra" => Some("it"),
                "om" => Some("da"),
                _ => None,
            };
            let n = self.number(i + 1)?;
            if n == 0 || n > 999 {
                return None;
            }
            let unit = self.w(i + 2)?;
            let n64 = i64::from(n);
            if let Some(l2) = find(UNIT_DAY, unit) {
                return Some((i + 3, today + Duration::days(n64), merge_lang(l, Some(l2))));
            }
            if let Some(l2) = find(UNIT_WEEK, unit) {
                return Some((
                    i + 3,
                    today + Duration::days(7 * n64),
                    merge_lang(l, Some(l2)),
                ));
            }
            if let Some(l2) = find(UNIT_MONTH, unit) {
                return Some((i + 3, add_months(today, n, None), merge_lang(l, Some(l2))));
            }
            return None;
        }
        // [on|på|di] monday / lunedì / mandag
        let (j, kw_lang) = match w {
            "on" => (i + 1, Some("en")),
            "på" => (i + 1, Some("da")),
            _ => (i, None),
        };
        let full_only = kw_lang.is_none();
        if let Some((d, l)) = self.w(j).and_then(|x| weekday(x, full_only)) {
            return Some((j + 1, next_weekday(today, d), merge_lang(kw_lang, Some(l))));
        }
        None
    }

    /// A recurrence at `i`.
    fn recurrence(&self, i: usize) -> Option<M> {
        let today = self.ctx.today;
        let w = self.w(i)?;
        let mut lang;
        let mut j;
        let mut from_completion = false;
        // daily / weekly / monthly / yearly
        let simple = match w {
            "daily" => Some((Rule::Days(1), "en")),
            "weekly" => Some((Rule::Weeks(1, vec![]), "en")),
            "monthly" => Some((Rule::Months(1, None), "en")),
            "yearly" | "annually" => Some((Rule::Years(1), "en")),
            "giornalmente" | "quotidianamente" => Some((Rule::Days(1), "it")),
            "settimanalmente" => Some((Rule::Weeks(1, vec![]), "it")),
            "mensilmente" => Some((Rule::Months(1, None), "it")),
            "daglig" | "dagligt" => Some((Rule::Days(1), "da")),
            "ugentlig" | "ugentligt" => Some((Rule::Weeks(1, vec![]), "da")),
            "månedlig" | "månedligt" => Some((Rule::Months(1, None), "da")),
            "årlig" | "årligt" => Some((Rule::Years(1), "da")),
            _ => None,
        };
        let rule;
        let mut time = None;
        if let Some((r, l)) = simple {
            rule = r;
            lang = Some(l);
            j = i + 1;
        } else {
            // every / every! / ogni / tutti i / tutte le / hver / alle
            let bare = w.trim_end_matches('!');
            from_completion = w.ends_with('!');
            if let Some(l) = find(EVERY, bare) {
                lang = Some(l);
                j = i + 1;
            } else if matches!(bare, "tutti" | "tutte")
                && matches!(self.w(i + 1), Some("i" | "le" | "gli"))
            {
                lang = Some("it");
                j = i + 2;
            } else if bare == "alle"
                && self.w(i + 1).is_some_and(|x| {
                    find(UNIT_DAY, x) == Some("da")
                        || weekday(x, true).is_some_and(|(_, l)| l == "da")
                })
            {
                lang = Some("da");
                j = i + 1;
            } else {
                return None;
            }
            let mut n = 1;
            if let Some(k) = self.number(j) {
                // "every 15" = day of the month; "every 2 weeks" = interval
                let next = self.w(j + 1);
                let is_unit = next.is_some_and(|u| {
                    find(UNIT_DAY, u).is_some()
                        || find(UNIT_WEEK, u).is_some()
                        || find(UNIT_MONTH, u).is_some()
                        || find(UNIT_YEAR, u).is_some()
                        || find(WORKDAY, u).is_some()
                });
                if is_unit {
                    n = k.max(1);
                    j += 1;
                } else if self
                    .w(j)
                    .is_some_and(|x| x.chars().all(|c| c.is_ascii_digit() || c == '.'))
                    && (1..=31).contains(&k)
                {
                    j += 1;
                    let r = Recurrence {
                        rule: Rule::Months(1, Some(k)),
                        from_completion,
                    };
                    let date = r.first_on_or_after(today);
                    return Some(self.with_time(M {
                        end: j,
                        date,
                        time: None,
                        rec: Some(r),
                        lang,
                    }));
                } else {
                    return None;
                }
            }
            let u = self.w(j)?;
            if let Some(l) = find(UNIT_DAY, u) {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Days(n);
                j += 1;
            } else if let Some(l) = find(UNIT_WEEK, u) {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Weeks(n, vec![]);
                j += 1;
            } else if let Some(l) = find(UNIT_MONTH, u) {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Months(n, None);
                j += 1;
            } else if let Some(l) = find(UNIT_YEAR, u) {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Years(n);
                j += 1;
            } else if let Some(l) = find(WORKDAY, u) {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Workdays;
                j += 1;
            } else if matches!(u, "giorni" | "giorno")
                && matches!(
                    self.w(j + 1),
                    Some("feriali" | "lavorativi" | "feriale" | "lavorativo")
                )
            {
                lang = Some("it");
                rule = Rule::Workdays;
                j += 2;
            } else if let Some((h, l)) = match u {
                "morning" => Some((9, "en")),
                "evening" => Some((19, "en")),
                "afternoon" => Some((13, "en")),
                "night" => Some((22, "en")),
                "mattina" => Some((9, "it")),
                "pomeriggio" => Some((13, "it")),
                "sera" => Some((19, "it")),
                "morgen" => Some((9, "da")),
                "eftermiddag" => Some((13, "da")),
                "aften" => Some((19, "da")),
                _ => None,
            } {
                lang = merge_lang(lang, Some(l));
                rule = Rule::Days(n);
                time = NaiveTime::from_hms_opt(h, 0, 0);
                j += 1;
            } else if weekday(u, false).is_some() {
                let mut days = Vec::new();
                while let Some((d, l)) = self.w(j).and_then(|x| weekday(x, false)) {
                    if !days.contains(&d) {
                        days.push(d);
                    }
                    lang = merge_lang(lang, Some(l));
                    j += 1;
                    let sep = self.t.get(j - 1).is_some_and(|t| t.comma);
                    match self.w(j) {
                        Some("and" | "e" | "og" | "&")
                            if self.w(j + 1).and_then(|x| weekday(x, false)).is_some() =>
                        {
                            j += 1
                        }
                        Some(x) if sep && weekday(x, false).is_some() => {}
                        _ => break,
                    }
                }
                rule = Rule::Weeks(n, days);
            } else {
                return None;
            }
        }
        let r = Recurrence {
            rule,
            from_completion,
        };
        let date = r.first_on_or_after(today);
        let m = self.with_time(M {
            end: j,
            date,
            time,
            rec: Some(r),
            lang,
        });
        Some(m)
    }

    fn with_time(&self, mut m: M) -> M {
        if let Some((end, t, l)) = self.time(m.end, false) {
            m.end = end;
            m.time = Some(t);
            m.lang = merge_lang(m.lang, l);
        }
        m
    }

    /// The longest date phrase starting at `i`.
    fn at(&self, i: usize) -> Option<M> {
        if let Some(m) = self.recurrence(i) {
            return Some(m);
        }
        let date = self.relative(i).or_else(|| self.calendar(i));
        if let Some((end, date, lang)) = date {
            return Some(self.with_time(M {
                end,
                date,
                time: None,
                rec: None,
                lang,
            }));
        }
        // a time first: "9am tomorrow", or alone (today at that time)
        let (end, t, l) = self.time(i, true)?;
        if let Some((e2, d, l2)) = self.relative(end).or_else(|| self.calendar(end)) {
            return Some(M {
                end: e2,
                date: d,
                time: Some(t),
                rec: None,
                lang: merge_lang(l, l2),
            });
        }
        Some(M {
            end,
            date: self.ctx.today,
            time: Some(t),
            rec: None,
            lang: l,
        })
    }
}

/// The longest (then the last) date phrase in `text`, if any.
pub fn find_date(text: &str, ctx: Ctx) -> Option<DateMatch> {
    let toks = tokenize(text);
    find_in(&toks, ctx, |_| true)
}

fn find_in(toks: &[Tok], ctx: Ctx, allowed: impl Fn(&Tok) -> bool) -> Option<DateMatch> {
    let p = P { t: toks, ctx };
    let mut best: Option<(usize, M)> = None;
    for i in 0..toks.len() {
        if !allowed(&toks[i]) {
            continue;
        }
        if let Some(m) = p.at(i) {
            if !(i..m.end).all(|k| allowed(&toks[k])) {
                continue;
            }
            let len = m.end - i;
            if best.as_ref().is_none_or(|(bi, bm)| len >= bm.end - bi) {
                best = Some((i, m));
            }
        }
    }
    let (i, m) = best?;
    Some(DateMatch {
        start: toks[i].start,
        end: toks[m.end - 1].end,
        date: m.date,
        time: m.time,
        recurrence: m.rec,
        lang: m.lang,
    })
}

/// A whole due string ("tomorrow", "every day at 9", "ogni lunedì"): the
/// phrase must start at the first word.
pub fn parse_due(s: &str, ctx: Ctx) -> Option<DateMatch> {
    let toks = tokenize(s);
    let p = P { t: &toks, ctx };
    let m = p.at(0)?;
    Some(DateMatch {
        start: toks.first()?.start,
        end: toks[m.end - 1].end,
        date: m.date,
        time: m.time,
        recurrence: m.rec,
        lang: m.lang,
    })
}

/// The recurrence of a stored Todoist due string, wherever it starts
/// ("every day", "ogni giorno alle 9", "every mon, fri starting 1 Nov").
pub fn recurrence_of(s: &str, today: NaiveDate) -> Option<Recurrence> {
    let toks = tokenize(s);
    let p = P {
        t: &toks,
        ctx: Ctx::new(today),
    };
    (0..toks.len()).find_map(|i| p.recurrence(i).and_then(|m| m.rec))
}

/// Parse one quick add line.
pub fn quick_add(text: &str, default_lang: &str, ctx: Ctx, projects: &[String]) -> Parsed {
    let mut spans: Vec<Span> = Vec::new();
    let mut taken = vec![false; text.len() + 1];
    let mark = |taken: &mut Vec<bool>, s: usize, e: usize| {
        for t in taken.iter_mut().take(e).skip(s) {
            *t = true;
        }
    };
    let mut out = Parsed {
        lang: default_lang.to_owned(),
        ..Parsed::default()
    };
    // " // description" at the end
    let mut body_end = text.len();
    if let Some(pos) = text.find(" //") {
        let desc = text[pos + 3..].trim();
        if !desc.is_empty() {
            out.description = Some(desc.to_owned());
        }
        spans.push(Span {
            start: pos + 1,
            end: text.len(),
            kind: "description".into(),
        });
        mark(&mut taken, pos, text.len());
        body_end = pos;
    }
    let body = &text[..body_end];
    // {deadline}
    if let (Some(a), Some(b)) = (body.find('{'), body.find('}')) {
        if a < b {
            spans.push(Span {
                start: a,
                end: b + 1,
                kind: "deadline".into(),
            });
            mark(&mut taken, a, b + 1);
        }
    }
    // #Project (longest known name, spaces allowed), /Section, @label, p1
    let lower = body.to_lowercase();
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        let at_word_start = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if !at_word_start || taken[i] {
            i += 1;
            continue;
        }
        let c = bytes[i];
        let word_end = body[i..]
            .find(char::is_whitespace)
            .map_or(body.len(), |e| i + e);
        if c == b'#' && word_end > i + 1 {
            let mut end = word_end;
            let rest = &lower[i + 1..];
            if let Some(name) = projects
                .iter()
                .filter(|p| {
                    let pl = p.to_lowercase();
                    rest.starts_with(&pl)
                        && rest[pl.len()..]
                            .chars()
                            .next()
                            .is_none_or(char::is_whitespace)
                })
                .max_by_key(|p| p.len())
            {
                end = i + 1 + name.len();
                out.project = Some(name.clone());
            } else {
                out.project = Some(body[i + 1..word_end].replace("\\ ", " "));
            }
            spans.push(Span {
                start: i,
                end,
                kind: "project".into(),
            });
            mark(&mut taken, i, end);
            i = end;
            continue;
        }
        if c == b'/' && word_end > i + 1 && !body[i + 1..word_end].contains('/') {
            out.section = Some(body[i + 1..word_end].to_owned());
            spans.push(Span {
                start: i,
                end: word_end,
                kind: "section".into(),
            });
            mark(&mut taken, i, word_end);
            i = word_end;
            continue;
        }
        if c == b'@' && word_end > i + 1 {
            let name = body[i + 1..word_end].trim_end_matches([',', '.']);
            if !name.is_empty() && !out.labels.iter().any(|l| l == name) {
                out.labels.push(name.to_owned());
            }
            spans.push(Span {
                start: i,
                end: word_end,
                kind: "label".into(),
            });
            mark(&mut taken, i, word_end);
            i = word_end;
            continue;
        }
        let w = &lower[i..word_end];
        let prio = match w {
            "p1" | "!!1" => Some(1),
            "p2" | "!!2" => Some(2),
            "p3" | "!!3" => Some(3),
            "p4" | "!!4" => Some(4),
            _ => None,
        };
        if let Some(p) = prio {
            out.priority = Some(p);
            spans.push(Span {
                start: i,
                end: word_end,
                kind: "priority".into(),
            });
            mark(&mut taken, i, word_end);
            i = word_end;
            continue;
        }
        i += 1;
    }
    // the date phrase among the remaining words
    let toks = tokenize(body);
    if let Some(m) = find_in(&toks, ctx, |t| !taken[t.start]) {
        let phrase = body[m.start..m.end].to_owned();
        out.is_recurring = m.recurrence.is_some();
        out.due_date = Some(match m.time {
            Some(t) => format!("{}T{}", m.date.format("%Y-%m-%d"), t.format("%H:%M")),
            None => m.date.format("%Y-%m-%d").to_string(),
        });
        out.lang = m.lang.unwrap_or(default_lang).to_owned();
        out.due_string = Some(phrase);
        spans.push(Span {
            start: m.start,
            end: m.end,
            kind: "date".into(),
        });
        mark(&mut taken, m.start, m.end);
    }
    let content: String = body
        .char_indices()
        .filter(|(i, _)| !taken[*i])
        .map(|(_, c)| c)
        .collect();
    out.content = content.split_whitespace().collect::<Vec<_>>().join(" ");
    spans.sort_by_key(|s| s.start);
    out.spans = spans;
    out
}

/// The `{…}` deadline of a quick add line, as `YYYY-MM-DD`.
pub fn deadline_of(text: &str, ctx: Ctx) -> Option<String> {
    let a = text.find('{')?;
    let b = text[a..].find('}')? + a;
    parse_due(&text[a + 1..b], ctx).map(|m| m.date.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        // Thursday 22 October 2026
        Ctx::new(NaiveDate::from_ymd_opt(2026, 10, 22).unwrap())
    }
    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn qa(s: &str) -> Parsed {
        quick_add(
            s,
            "en",
            ctx(),
            &["Inbox".into(), "Lingue".into(), "My Project".into()],
        )
    }

    #[test]
    fn relative_dates_in_three_languages() {
        for (s, want, lang) in [
            ("call mum today", d(10, 22), "en"),
            ("chiamare mamma oggi", d(10, 22), "it"),
            ("ring til mor i dag", d(10, 22), "da"),
            ("pay rent tomorrow", d(10, 23), "en"),
            ("pagare affitto domani", d(10, 23), "it"),
            ("betal husleje i morgen", d(10, 23), "da"),
            ("dentista dopodomani", d(10, 24), "it"),
            ("gym monday", d(10, 26), "en"),
            ("palestra lunedì", d(10, 26), "it"),
            ("palestra lunedi prossimo", d(10, 26), "it"),
            ("træning på fredag", d(10, 23), "da"),
            ("review next week", d(10, 26), "en"),
            ("revisione la prossima settimana", d(10, 26), "it"),
            ("revisione settimana prossima", d(10, 26), "it"),
            ("gennemgang næste uge", d(10, 26), "da"),
            ("trip in 3 days", d(10, 25), "en"),
            ("viaggio tra 3 giorni", d(10, 25), "it"),
            ("rejse om 2 uger", d(11, 5), "da"),
            ("clean this weekend", d(10, 24), "en"),
            ("bills next month", d(11, 1), "en"),
        ] {
            let p = qa(s);
            assert_eq!(
                p.due_date.as_deref(),
                Some(want.format("%Y-%m-%d").to_string().as_str()),
                "{s}"
            );
            assert_eq!(p.lang, lang, "{s}");
            assert!(!p.is_recurring, "{s}");
        }
    }

    #[test]
    fn calendar_dates_and_times() {
        let p = qa("dentist 27 oct at 9am");
        assert_eq!(p.due_date.as_deref(), Some("2026-10-27T09:00"));
        assert_eq!(p.content, "dentist");
        assert_eq!(p.due_string.as_deref(), Some("27 oct at 9am"));
        let p = qa("dentista il 3 novembre alle 15:30");
        assert_eq!(p.due_date.as_deref(), Some("2026-11-03T15:30"));
        assert_eq!(p.lang, "it");
        let p = qa("tandlæge 5. januar kl. 9.30");
        assert_eq!(p.due_date.as_deref(), Some("2027-01-05T09:30"));
        assert_eq!(p.lang, "da");
        assert_eq!(qa("x 27/10").due_date.as_deref(), Some("2026-10-27"));
        assert_eq!(qa("x 1/2/2027").due_date.as_deref(), Some("2027-02-01"));
        assert_eq!(qa("x 2026-12-24").due_date.as_deref(), Some("2026-12-24"));
        assert_eq!(qa("x oct 30").due_date.as_deref(), Some("2026-10-30"));
        assert_eq!(
            qa("standup tomorrow 9am").due_date.as_deref(),
            Some("2026-10-23T09:00")
        );
        assert_eq!(
            qa("standup 9am tomorrow").due_date.as_deref(),
            Some("2026-10-23T09:00")
        );
        assert_eq!(
            qa("call at 5pm").due_date.as_deref(),
            Some("2026-10-22T17:00")
        );
        assert_eq!(
            qa("chiamare alle 18").due_date.as_deref(),
            Some("2026-10-22T18:00")
        );
        assert_eq!(
            qa("ring kl 8").due_date.as_deref(),
            Some("2026-10-22T08:00")
        );
    }

    #[test]
    fn plain_text_is_not_a_date() {
        for s in [
            "15 min shadowing",
            "10 min anki/memrise",
            "20min speaking",
            "10 min duolingo",
            "Brain Training",
            "look at 3 options",
            "the man on the moon",
            "may the force",
            "ogni tanto",
        ] {
            let p = qa(s);
            assert_eq!(p.due_string, None, "{s}");
            assert_eq!(p.content, s, "{s}");
        }
    }

    #[test]
    fn recurrence() {
        let r = |s: &str| {
            let p = qa(s);
            assert!(p.is_recurring, "{s}");
            (p.due_date.unwrap(), p.lang, p.content)
        };
        assert_eq!(
            r("15 min shadowing every day"),
            ("2026-10-22".into(), "en".into(), "15 min shadowing".into())
        );
        assert_eq!(r("10 min duolingo ogni giorno").1, "it");
        assert_eq!(r("anki tutti i giorni").0, "2026-10-22");
        assert_eq!(r("anki hver dag").1, "da");
        assert_eq!(r("speaking daily").0, "2026-10-22");
        assert_eq!(r("gym every monday").0, "2026-10-26");
        assert_eq!(r("palestra ogni lunedì").0, "2026-10-26");
        assert_eq!(r("træning hver mandag").0, "2026-10-26");
        assert_eq!(r("gym every mon, wed and fri").0, "2026-10-23");
        assert_eq!(r("palestra ogni lunedì e giovedì").0, "2026-10-22");
        assert_eq!(r("standup every weekday at 9").0, "2026-10-22T09:00");
        assert_eq!(r("standup ogni giorno alle 9").0, "2026-10-22T09:00");
        assert_eq!(r("journal every morning").0, "2026-10-22T09:00");
        assert_eq!(r("rent every 1").0, "2026-11-01");
        assert_eq!(r("review every 2 weeks").0, "2026-10-22");
        let p = qa("water plants every! 3 days");
        assert!(p.is_recurring);
        assert_eq!(p.due_string.as_deref(), Some("every! 3 days"));
    }

    #[test]
    fn markers() {
        let p = qa("Buy milk tomorrow #Lingue /Ripasso @shop @home p1 // two litres");
        assert_eq!(p.content, "Buy milk");
        assert_eq!(p.project.as_deref(), Some("Lingue"));
        assert_eq!(p.section.as_deref(), Some("Ripasso"));
        assert_eq!(p.labels, vec!["shop", "home"]);
        assert_eq!(p.priority, Some(1));
        assert_eq!(p.description.as_deref(), Some("two litres"));
        assert_eq!(p.due_date.as_deref(), Some("2026-10-23"));
        let p = qa("Plan #My Project trip");
        assert_eq!(p.project.as_deref(), Some("My Project"));
        assert_eq!(p.content, "Plan trip");
        let kinds: Vec<_> = qa("a today p2").spans.into_iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["date", "priority"]);
    }

    #[test]
    fn next_occurrence_guess() {
        let today = d(10, 22);
        let daily = recurrence_of("every day", today).unwrap();
        assert_eq!(daily.next_after(d(10, 22), today), d(10, 23));
        assert_eq!(daily.next_after(d(10, 19), today), d(10, 23));
        let mon = recurrence_of("ogni lunedì alle 9", today).unwrap();
        assert_eq!(mon.next_after(d(10, 19), today), d(10, 26));
        let monthly = recurrence_of("every 15", today).unwrap();
        assert_eq!(monthly.next_after(d(10, 15), today), d(11, 15));
        let wd = recurrence_of("hver hverdag", today).unwrap();
        assert_eq!(wd.next_after(d(10, 23), d(10, 23)), d(10, 26));
        assert_eq!(recurrence_of("tomorrow", today), None);
        assert_eq!(add_months(d(1, 31), 1, None), d(2, 28));
    }

    #[test]
    fn whole_due_strings() {
        let c = ctx();
        assert_eq!(parse_due("tomorrow", c).unwrap().date, d(10, 23));
        assert_eq!(parse_due("domani", c).unwrap().date, d(10, 23));
        assert_eq!(parse_due("next week", c).unwrap().date, d(10, 26));
        assert!(parse_due("banana", c).is_none());
        assert_eq!(deadline_of("x {27 oct}", c).as_deref(), Some("2026-10-27"));
    }
}
