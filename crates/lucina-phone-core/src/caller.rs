//! Screening unknown callers: what a number is, what Jev is asked about it,
//! and what every surface shows (the chat table, the Android notification,
//! the iOS Call Directory label). No I/O: Lucina asks Jev with [`state`] and
//! [`SPAM_QUESTION`] / [`KIND_QUESTION`] / [`PICK_UP_QUESTION`], the phones
//! use [`rules`] when Lucina is out of reach, and both build a [`Verdict`].
//!
//! Jev judges from facts about the number only: its country and line type,
//! his contacts, his own calls with it, his messages about it and public
//! caller-ID reports (180.dk), which Lucina looks up first. Never guesses
//! about who might call him (owner's rule, 2026-10-29).
//!
//! Nothing here answers, rejects or silences a call.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Most numbers in one request.
pub const MAX_NUMBERS: usize = 20;
/// Characters of a Call Directory label (iOS shows about this many).
pub const MAX_SHORT: usize = 32;

/// What kind of line a number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineType {
    Mobile,
    Landline,
    /// A company number with no place (Danish 70, UK 03/084/087, …).
    NonGeographic,
    Freephone,
    Premium,
    Unknown,
    Invalid,
}

impl LineType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Landline => "landline",
            Self::NonGeographic => "non_geographic",
            Self::Freephone => "freephone",
            Self::Premium => "premium",
            Self::Unknown => "unknown",
            Self::Invalid => "invalid",
        }
    }
}

/// A parsed phone number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Number {
    /// What the owner or the phone gave.
    pub raw: String,
    /// `+<cc><national>`, or the raw text for an invalid number.
    pub e164: String,
    /// Country calling code ("45"), empty when unknown.
    pub cc: &'static str,
    /// ISO country ("DK"), empty when unknown.
    pub country: &'static str,
    pub country_name: &'static str,
    pub national: String,
    pub line: LineType,
}

impl Number {
    pub fn valid(&self) -> bool {
        self.line != LineType::Invalid
    }

    /// Grouped for reading: `+45 70 40 52 05`, `+357 94 095733`, `+44 7459 508537`.
    pub fn display(&self) -> String {
        if !self.valid() {
            return self.raw.trim().to_owned();
        }
        let n = &self.national;
        let grouped = match self.cc {
            "45" if n.len() == 8 => n
                .as_bytes()
                .chunks(2)
                .map(|c| std::str::from_utf8(c).unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" "),
            "357" if n.len() == 8 => format!("{} {}", &n[..2], &n[2..]),
            "44" if n.len() == 10 && n.starts_with('7') => format!("{} {}", &n[..4], &n[4..]),
            "39" if n.len() >= 9 && n.starts_with('3') => format!("{} {}", &n[..3], &n[3..]),
            _ => n.clone(),
        };
        if self.cc.is_empty() {
            format!("+{grouped}")
        } else {
            format!("+{} {grouped}", self.cc)
        }
    }

    /// All digits with the country code, as iOS's Call Directory wants them.
    pub fn directory_number(&self) -> Option<i64> {
        if !self.valid() {
            return None;
        }
        self.e164.trim_start_matches('+').parse().ok()
    }
}

/// Calling codes the owner meets (longest prefix wins). Not exhaustive: an
/// unknown code still parses, with an empty country.
const COUNTRIES: &[(&str, &str, &str)] = &[
    ("1", "US", "USA/Canada"),
    ("7", "RU", "Russia"),
    ("20", "EG", "Egypt"),
    ("27", "ZA", "South Africa"),
    ("30", "GR", "Greece"),
    ("31", "NL", "Netherlands"),
    ("32", "BE", "Belgium"),
    ("33", "FR", "France"),
    ("34", "ES", "Spain"),
    ("36", "HU", "Hungary"),
    ("39", "IT", "Italy"),
    ("40", "RO", "Romania"),
    ("41", "CH", "Switzerland"),
    ("43", "AT", "Austria"),
    ("44", "GB", "United Kingdom"),
    ("45", "DK", "Denmark"),
    ("46", "SE", "Sweden"),
    ("47", "NO", "Norway"),
    ("48", "PL", "Poland"),
    ("49", "DE", "Germany"),
    ("51", "PE", "Peru"),
    ("52", "MX", "Mexico"),
    ("54", "AR", "Argentina"),
    ("55", "BR", "Brazil"),
    ("60", "MY", "Malaysia"),
    ("61", "AU", "Australia"),
    ("62", "ID", "Indonesia"),
    ("63", "PH", "Philippines"),
    ("66", "TH", "Thailand"),
    ("81", "JP", "Japan"),
    ("82", "KR", "South Korea"),
    ("84", "VN", "Vietnam"),
    ("86", "CN", "China"),
    ("90", "TR", "Turkey"),
    ("91", "IN", "India"),
    ("92", "PK", "Pakistan"),
    ("212", "MA", "Morocco"),
    ("213", "DZ", "Algeria"),
    ("216", "TN", "Tunisia"),
    ("225", "CI", "Côte d'Ivoire"),
    ("233", "GH", "Ghana"),
    ("234", "NG", "Nigeria"),
    ("351", "PT", "Portugal"),
    ("352", "LU", "Luxembourg"),
    ("353", "IE", "Ireland"),
    ("354", "IS", "Iceland"),
    ("355", "AL", "Albania"),
    ("356", "MT", "Malta"),
    ("357", "CY", "Cyprus"),
    ("358", "FI", "Finland"),
    ("359", "BG", "Bulgaria"),
    ("370", "LT", "Lithuania"),
    ("371", "LV", "Latvia"),
    ("372", "EE", "Estonia"),
    ("380", "UA", "Ukraine"),
    ("381", "RS", "Serbia"),
    ("385", "HR", "Croatia"),
    ("386", "SI", "Slovenia"),
    ("420", "CZ", "Czechia"),
    ("421", "SK", "Slovakia"),
    ("852", "HK", "Hong Kong"),
    ("880", "BD", "Bangladesh"),
    ("961", "LB", "Lebanon"),
    ("962", "JO", "Jordan"),
    ("966", "SA", "Saudi Arabia"),
    ("971", "AE", "United Arab Emirates"),
    ("972", "IL", "Israel"),
    ("995", "GE", "Georgia"),
];

fn country_of(digits: &str) -> Option<(&'static str, &'static str, &'static str)> {
    (1..=3)
        .rev()
        .filter(|&n| digits.len() > n)
        .find_map(|n| COUNTRIES.iter().find(|c| c.0 == &digits[..n]).copied())
}

/// The line type from the national number, for the countries whose plans are simple enough.
fn line_type(cc: &str, n: &str) -> LineType {
    let two = n.get(..2).and_then(|s| s.parse::<u8>().ok());
    let starts = |p: &str| n.starts_with(p);
    match cc {
        // Danish numbering plan (Energistyrelsen): 8 digits.
        "45" => {
            if n.len() != 8 {
                return LineType::Invalid;
            }
            match two.unwrap_or(0) {
                20..=31 | 40..=42 | 50..=53 | 60 | 61 | 71 | 81 | 91..=93 => LineType::Mobile,
                70 => LineType::NonGeographic,
                80 => LineType::Freephone,
                90 => LineType::Premium,
                32..=39 | 43..=49 | 54..=59 | 62..=69 | 72..=79 | 82..=89 | 94..=99 => {
                    LineType::Landline
                }
                _ => LineType::Unknown,
            }
        }
        "44" => {
            if !(9..=10).contains(&n.len()) {
                return LineType::Invalid;
            }
            if starts("70") || starts("76") {
                LineType::NonGeographic
            } else if starts("7") {
                LineType::Mobile
            } else if starts("1") || starts("2") {
                LineType::Landline
            } else if starts("80") {
                LineType::Freephone
            } else if starts("3") || starts("84") || starts("87") {
                LineType::NonGeographic
            } else if starts("9") {
                LineType::Premium
            } else {
                LineType::Unknown
            }
        }
        "357" => match n.len() {
            8 if starts("9") => LineType::Mobile,
            8 if starts("2") => LineType::Landline,
            8 if starts("800") => LineType::Freephone,
            8 if starts("90") => LineType::Premium,
            8 => LineType::Unknown,
            _ => LineType::Invalid,
        },
        "39" => {
            if starts("3") {
                LineType::Mobile
            } else if starts("0") {
                LineType::Landline
            } else if starts("80") {
                LineType::Freephone
            } else if starts("89") {
                LineType::Premium
            } else {
                LineType::Unknown
            }
        }
        "46" if ["70", "72", "73", "76", "79"].iter().any(|p| starts(p)) => LineType::Mobile,
        "46" => LineType::Landline,
        "47" if n.len() == 8 && (starts("4") || starts("9")) => LineType::Mobile,
        "47" if n.len() == 8 => LineType::Landline,
        "49" if starts("15") || starts("16") || starts("17") => LineType::Mobile,
        "49" => LineType::Landline,
        _ => LineType::Unknown,
    }
}

fn invalid(raw: &str) -> Number {
    Number {
        raw: raw.to_owned(),
        e164: raw.trim().to_owned(),
        cc: "",
        country: "",
        country_name: "",
        national: String::new(),
        line: LineType::Invalid,
    }
}

/// Parses a number as the owner writes it or a phone reports it. Eight digits
/// without a prefix are Danish (he lives in Aarhus).
pub fn parse(raw: &str) -> Number {
    let t = raw.trim();
    if t.is_empty()
        || t.len() > 40
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || " +-().\u{a0}/".contains(c))
    {
        return invalid(raw);
    }
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    let international = t.starts_with('+') || digits.starts_with("00");
    let digits = if !t.starts_with('+') && digits.starts_with("00") {
        digits[2..].to_owned()
    } else {
        digits
    };
    if !(international || digits.len() == 8 || (digits.len() == 10 && digits.starts_with("45"))) {
        return invalid(raw);
    }
    let full = if international || digits.len() == 10 {
        digits
    } else {
        format!("45{digits}")
    };
    if !(8..=15).contains(&full.len()) {
        return invalid(raw);
    }
    let (cc, country, country_name) = country_of(&full).unwrap_or(("", "", ""));
    let national = full[cc.len()..].to_owned();
    let line = if cc.is_empty() {
        LineType::Unknown
    } else {
        line_type(cc, &national)
    };
    if line == LineType::Invalid {
        return invalid(raw);
    }
    Number {
        raw: raw.to_owned(),
        e164: format!("+{full}"),
        cc,
        country,
        country_name,
        national,
        line,
    }
}

/// Every phone number in a message or a pasted list, in order, without repeats
/// (at most [`MAX_NUMBERS`]). Dates, times and short codes are not numbers.
pub fn find_numbers(text: &str) -> Vec<Number> {
    static RX: OnceLock<Regex> = OnceLock::new();
    let rx = RX.get_or_init(|| {
        Regex::new(r"(?:\+|\b00)?\(?\d[\d \u{a0}().-]{6,20}\d\b").expect("caller number regex")
    });
    let mut out: Vec<Number> = Vec::new();
    for m in rx.find_iter(text) {
        let s = m.as_str();
        // "12.10.2026", "10:30-11:00": dates and ranges, not numbers.
        if s.matches('.').count() >= 2 && !s.starts_with('+') {
            continue;
        }
        let n = parse(s);
        if n.valid() && !out.iter().any(|o| o.e164 == n.e164) {
            out.push(n);
            if out.len() == MAX_NUMBERS {
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- context

/// Kept for the phones' request format (the Android app still sends it): never
/// put into the state, the rules or the labels. Who might call him is not a
/// fact about the number (owner's rule, 2026-10-29).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Courier {
    #[serde(default)]
    pub on_shift: bool,
    #[serde(default)]
    pub order_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_age_min: Option<u32>,
}

/// One category share of the public reports ("Telemarketing (25%)").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportShare {
    /// English name: caution, annoying, robocall, telemarketing, market research,
    /// poll, safe, delivery, other.
    pub category: String,
    pub percent: f64,
}

/// What a public caller-ID site says about the number, as read from its page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicReport {
    /// "180.dk".
    pub source: String,
    /// The page read, quoted with every answer that uses it.
    pub url: String,
    /// How often people looked the number up.
    #[serde(default)]
    pub searches: u32,
    /// Reports from users.
    #[serde(default)]
    pub reports: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<ReportShare>,
    /// The latest user comment: date, title and text as written (clipped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_comment: Option<String>,
    /// The operator the site names, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// A database's own score, as it states it ("tellows 9/9", "IPQS fraud 92/100").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<String>,
    /// Other facts the database states ("line type: VOIP", "recent abuse").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

impl PublicReport {
    /// Categories with a share, largest first.
    pub fn top_categories(&self, n: usize) -> Vec<&ReportShare> {
        let mut v: Vec<&ReportShare> = self.categories.iter().filter(|c| c.percent > 0.0).collect();
        v.sort_by(|a, b| b.percent.total_cmp(&a.percent));
        v.truncate(n);
        v
    }

    /// One line for Jev and the reply: what the page says, nothing inferred.
    pub fn summary(&self) -> String {
        let mut s = if self.reports == 0 {
            format!(
                "{}: no user reports (looked up {} times)",
                self.source, self.searches
            )
        } else {
            format!(
                "{}: {} user report{} (looked up {} times)",
                self.source,
                self.reports,
                if self.reports == 1 { "" } else { "s" },
                self.searches
            )
        };
        let top = self.top_categories(4);
        if self.reports > 0 && !top.is_empty() {
            s.push_str("; categories: ");
            s.push_str(
                &top.iter()
                    .map(|c| format!("{} {}%", c.category, fmt_percent(c.percent)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(score) = &self.score {
            s.push_str(&format!("; score {score}"));
        }
        if !self.flags.is_empty() {
            s.push_str(&format!("; {}", self.flags.join(", ")));
        }
        if let Some(op) = &self.operator {
            s.push_str(&format!("; operator {op}"));
        }
        if self.reports > 0 {
            if let Some(c) = &self.latest_comment {
                s.push_str(&format!("; latest comment: \"{c}\""));
            }
        }
        s
    }
}

fn fmt_percent(p: f64) -> String {
    if (p - p.round()).abs() < 0.05 {
        format!("{:.0}", p)
    } else {
        format!("{:.1}", p)
    }
}

/// What Lucina knows about one number: facts about the number only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Known {
    /// The contact's name, when the number is in his contacts.
    pub contact: Option<String>,
    /// Earlier calls from this number that a phone reported.
    pub previous_calls: u32,
    /// The last of them (RFC 3339).
    pub last_call: Option<String>,
    /// The number's mentions in his messages to Lucina (short snippets).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// A label the owner gave the number himself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_label: Option<String>,
    /// Public caller-ID reports (180.dk), looked up first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<PublicReport>,
    /// Report databases asked through their APIs (tellows, IPQS, …), when keys exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<PublicReport>,
}

impl Known {
    /// Every report source that answered (the page and the APIs).
    pub fn checked(&self) -> Vec<&PublicReport> {
        self.public.iter().chain(self.databases.iter()).collect()
    }

    /// At least one source answered and none has a report: the owner's "safe
    /// indicator" (2026-10-29). Failed or timed-out lookups never count.
    pub fn no_reports(&self) -> bool {
        let c = self.checked();
        !c.is_empty() && c.iter().all(|r| r.reports == 0)
    }

    /// "180.dk, tellows".
    pub fn checked_sources(&self) -> String {
        self.checked()
            .iter()
            .map(|r| r.source.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Known {
    pub fn messages(&self) -> usize {
        self.mentions.len()
    }
}

/// Everything Jev is told about one call or lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    pub known: Known,
    /// Is this a call ringing now (a phone asked) or a lookup afterwards?
    pub ringing: bool,
}

fn line_phrase(n: &Number) -> String {
    match (n.country, n.line) {
        ("DK", LineType::NonGeographic) => {
            "Danish 70-number (a non-geographic company number)".to_owned()
        }
        (_, LineType::Invalid) => "not a valid phone number".to_owned(),
        (_, l) => format!(
            "{} {}",
            if n.country_name.is_empty() {
                "unknown-country"
            } else {
                n.country_name
            },
            match l {
                LineType::Mobile => "mobile number",
                LineType::Landline => "landline (geographic) number",
                LineType::NonGeographic => "non-geographic company number",
                LineType::Freephone => "freephone number",
                LineType::Premium => "premium-rate number",
                _ => "number of unknown type",
            }
        ),
    }
}

/// The state Jev decides on: facts about this number only (its country and
/// line type, his contacts, his own call history with it, his messages about
/// it, public caller-ID reports). Never guesses about who might call him.
pub fn state(n: &Number, ctx: &Context) -> Value {
    let k = &ctx.known;
    let mut facts = vec![];
    match &k.contact {
        Some(name) => facts.push(format!("In his contacts as \"{name}\".")),
        None => facts.push("Not in his contacts.".to_owned()),
    }
    if let Some(l) = &k.owner_label {
        facts.push(format!("He labelled this number himself: \"{l}\"."));
    }
    facts.push(match k.previous_calls {
        0 => "No earlier calls from this number that Lucina saw.".to_owned(),
        1 => format!(
            "Called him once before{}.",
            k.last_call
                .as_deref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default()
        ),
        c => format!(
            "Called him {c} times before, last {}.",
            k.last_call.as_deref().unwrap_or("recently")
        ),
    });
    for m in &k.mentions {
        facts.push(format!("He wrote about it to Lucina: \"{m}\""));
    }
    let mut public: Vec<String> = k
        .checked()
        .iter()
        .map(|p| format!("{} ({})", p.summary(), p.url))
        .collect();
    if k.no_reports() {
        public.push(format!(
            "Checked {}: zero reports, not on any spam list.",
            k.checked_sources()
        ));
    }
    let public = if public.is_empty() {
        "No public caller-ID report could be checked.".to_owned()
    } else {
        public.join(" ")
    };
    json!({
        "call": if ctx.ringing { "An incoming call is ringing now." } else { "The owner asks about a number that called him." },
        "number": n.display(),
        "country": if n.country_name.is_empty() { "unknown" } else { n.country_name },
        "line": line_phrase(n),
        "facts": facts,
        "publicReports": public,
        "note": "Judge only from these facts about the number. There is no other information about who calls him.",
    })
}

pub const SPAM_QUESTION: &str =
    "Is this caller most likely unwanted: telemarketing, a robocall or a scam?";
pub const KIND_QUESTION: &str = "Who is most likely calling?";
pub const PICK_UP_QUESTION: &str = "Should the owner pick up this call?";

/// The kinds, in table order, with what each means for Jev.
pub const KINDS: [(&str, &str); 5] = [
    ("person", "A private person he may know."),
    (
        "business",
        "A legitimate business or service: bank, doctor, employer, authority, a shop.",
    ),
    (
        "telemarketing",
        "A sales, survey or market-research call centre: legitimate but unwanted.",
    ),
    (
        "scam",
        "A scam or fraud: fake investment, crypto, prize or impersonation call.",
    ),
    ("unknown", "Not enough to tell."),
];

/// One number's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub number: String,
    pub display: String,
    pub country: String,
    pub country_name: String,
    #[serde(rename = "type")]
    pub line: LineType,
    pub spam: f64,
    pub kind: String,
    pub kind_p: f64,
    /// Every kind's probability (Jev), or the rules' guess.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kinds: BTreeMap<String, f64>,
    pub pick_up: f64,
    pub label: String,
    pub short: String,
    /// `jev`, `rules` (Jev unreachable) or `contact`.
    pub source: String,
    pub known: Known,
    pub reasons: Vec<String>,
}

/// Probabilities from Jev or [`rules`].
#[derive(Debug, Clone, PartialEq)]
pub struct Probs {
    pub spam: f64,
    pub kinds: BTreeMap<String, f64>,
    pub pick_up: f64,
}

impl Probs {
    /// The most likely kind (first of equal maxima in [`KINDS`] order).
    pub fn top(&self) -> (&'static str, f64) {
        let mut best = ("unknown", -1.0);
        for (k, _) in KINDS {
            let p = self.kinds.get(k).copied().unwrap_or(0.0);
            if p > best.1 {
                best = (k, p);
            }
        }
        (best.0, best.1.max(0.0))
    }
}

fn probs(spam: f64, kind: &str, p: f64, pick_up: f64) -> Probs {
    let rest = (1.0 - p) / (KINDS.len() - 1) as f64;
    Probs {
        spam,
        kinds: KINDS
            .iter()
            .map(|(k, _)| ((*k).to_owned(), if *k == kind { p } else { rest }))
            .collect(),
        pick_up,
    }
}

/// Unwanted shares of the public reports (telemarketing, robocall, research,
/// polls, "caution", "annoying"), in percent.
fn unwanted_share(p: &PublicReport) -> f64 {
    p.categories
        .iter()
        .filter(|c| {
            matches!(
                c.category.as_str(),
                "caution" | "annoying" | "robocall" | "telemarketing" | "market research" | "poll"
            )
        })
        .map(|c| c.percent)
        .sum()
}

/// The deterministic guess when Jev is out of reach: from the facts only
/// (contact, public reports, line type). Deliberately modest: never above 0.7.
pub fn rules(n: &Number, ctx: &Context) -> Probs {
    if ctx.known.contact.is_some() {
        return probs(0.02, "person", 0.9, 0.95);
    }
    if n.line == LineType::Invalid {
        return probs(0.0, "unknown", 1.0, 0.0);
    }
    if let Some(p) = ctx.known.public.as_ref().filter(|p| p.reports >= 3) {
        let unwanted = unwanted_share(p);
        if unwanted >= 50.0 {
            return probs(0.7, "telemarketing", 0.5, 0.15);
        }
        if p.categories
            .iter()
            .any(|c| c.category == "safe" && c.percent >= 50.0)
        {
            return probs(0.15, "business", 0.5, 0.6);
        }
    }
    match (n.country, n.line) {
        (_, LineType::Premium) => probs(0.5, "unknown", 0.4, 0.2),
        ("DK", LineType::NonGeographic) => probs(0.4, "business", 0.45, 0.4),
        _ => probs(0.3, "unknown", 0.5, 0.4),
    }
}

// ---------------------------------------------------------------- zero reports

/// Spam is multiplied by this when every checked source has zero reports.
pub const NO_REPORTS_SPAM_FACTOR: f64 = 0.6;
/// Pick-up is raised by this (capped at [`NO_REPORTS_PICK_UP_CAP`]).
pub const NO_REPORTS_PICK_UP_BOOST: f64 = 0.2;
pub const NO_REPORTS_PICK_UP_CAP: f64 = 0.95;

/// The owner's rule (2026-10-29): no online reports is a safe indicator. Applied
/// after Jev, deterministic and bounded; only when a source really answered.
pub fn no_reports_boost(p: &Probs, k: &Known) -> Probs {
    if !k.no_reports() || k.contact.is_some() {
        return p.clone();
    }
    Probs {
        spam: (p.spam * NO_REPORTS_SPAM_FACTOR).clamp(0.0, 1.0),
        kinds: p.kinds.clone(),
        pick_up: (p.pick_up + NO_REPORTS_PICK_UP_BOOST).min(NO_REPORTS_PICK_UP_CAP.max(p.pick_up)),
    }
}

// ---------------------------------------------------------------- auto-reject

/// The owner's auto-reject setting (his words 2026-10-29: "spam ≥ 0.60, or
/// pick-up ≤ 0.25"). On by default, editable in the apps.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectSettings {
    pub enabled: bool,
    pub spam_min: f64,
    pub pick_up_max: f64,
    /// While a Wolt order is active, only spam at or above this rejects.
    pub order_spam_min: f64,
}

impl Default for RejectSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            spam_min: 0.60,
            pick_up_max: 0.25,
            order_spam_min: 0.97,
        }
    }
}

impl RejectSettings {
    /// Thresholds in 0..=1 (anything else is refused by the apps).
    pub fn valid(&self) -> bool {
        [self.spam_min, self.pick_up_max, self.order_spam_min]
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    }
}

/// What only the phone knows at ring time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Guards {
    /// In his contacts.
    pub contact: bool,
    /// He called this number, or answered a call from it, before.
    pub called_or_answered: bool,
    /// He allowed it himself ("Allow & call back").
    pub allowed: bool,
    /// He is on an active Wolt order (Lucina Maps; never sent to Jev).
    pub order_active: bool,
}

/// Why a call is not rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    Off,
    Invalid,
    Emergency,
    ShortNumber,
    Contact,
    CalledOrAnswered,
    Allowed,
    NoEvidence,
    OrderActive,
    BelowThresholds,
}

/// Why a call is rejected: which threshold, with the value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reject {
    pub spam: Option<f64>,
    pub pick_up: Option<f64>,
    /// "180.dk 6 reports" etc., for the log and the notification.
    pub sources: String,
}

/// Emergency numbers on European and North American networks (the phone also
/// asks its own emergency-number table; this is the floor).
pub fn is_emergency(raw: &str) -> bool {
    let d: String = raw.chars().filter(char::is_ascii_digit).collect();
    matches!(
        d.as_str(),
        "112"
            | "911"
            | "999"
            | "000"
            | "114"
            | "1813"
            | "110"
            | "113"
            | "118"
            | "119"
            | "15"
            | "17"
            | "18"
    )
}

/// Short service numbers (fewer than 8 digits nationally): never rejected.
pub fn is_short(raw: &str) -> bool {
    let d: String = raw.chars().filter(char::is_ascii_digit).collect();
    !raw.trim_start().starts_with('+') && !d.starts_with("00") && d.len() < 8
}

/// The decision for a ringing call. Never rejects what the owner knows or
/// needs, never on a guess without evidence (the rules without reports).
pub fn reject_decision(
    raw: &str,
    v: &Verdict,
    g: &Guards,
    s: &RejectSettings,
) -> Result<Reject, Keep> {
    if !s.enabled || !s.valid() {
        return Err(Keep::Off);
    }
    if is_emergency(raw) {
        return Err(Keep::Emergency);
    }
    if is_short(raw) {
        return Err(Keep::ShortNumber);
    }
    if v.line == LineType::Invalid {
        return Err(Keep::Invalid);
    }
    if g.contact || v.source == "contact" || v.known.contact.is_some() {
        return Err(Keep::Contact);
    }
    if g.called_or_answered {
        return Err(Keep::CalledOrAnswered);
    }
    if g.allowed || v.known.owner_label.is_some() {
        return Err(Keep::Allowed);
    }
    // The fallback rules decide only from report facts; without any, never reject.
    let reported = v.known.checked().iter().any(|r| r.reports > 0);
    if v.source != "jev" && !reported {
        return Err(Keep::NoEvidence);
    }
    let sources = v
        .known
        .checked()
        .iter()
        .map(|r| match &r.score {
            Some(sc) => format!("{} {sc}", r.source),
            None if r.reports > 0 => format!("{} {} reports", r.source, r.reports),
            None => format!("{} no reports", r.source),
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if g.order_active {
        return if v.spam >= s.order_spam_min {
            Ok(Reject {
                spam: Some(v.spam),
                pick_up: None,
                sources,
            })
        } else {
            Err(Keep::OrderActive)
        };
    }
    let spam = (v.spam >= s.spam_min).then_some(v.spam);
    let pick_up = (v.pick_up <= s.pick_up_max).then_some(v.pick_up);
    if spam.is_none() && pick_up.is_none() {
        return Err(Keep::BelowThresholds);
    }
    Ok(Reject {
        spam,
        pick_up,
        sources,
    })
}

/// "Rejected likely telemarketer +45 20 68 27 58 · 180.dk 6 reports".
pub fn reject_line(v: &Verdict, r: &Reject) -> String {
    let what = match v.kind.as_str() {
        "telemarketing" => "likely telemarketer",
        "scam" => "likely scam",
        _ if r.spam.is_some() => "likely spam",
        _ => "unlikely to need you",
    };
    let mut s = format!("Rejected {what} {}", v.display);
    if !r.sources.is_empty() {
        s.push_str(&format!(" · {}", r.sources));
    }
    s
}

// ---------------------------------------------------------------- words

#[derive(Debug, Clone, Copy)]
struct Words {
    kinds: [&'static str; 5],
    likely: &'static str,
    unknown_caller: &'static str,
    contact: &'static str,
    not_a_number: &'static str,
    spam: &'static str,
    head: [&'static str; 4],
    source_jev: &'static str,
    source_rules: &'static str,
    called_before: &'static str,
    once_before: &'static str,
    mentioned: &'static str,
    own_label: &'static str,
    in_contacts: &'static str,
    dk70: &'static str,
    reports: &'static str,
    no_reports: &'static str,
    likely_ok: &'static str,
}

fn words(lang: &str) -> Words {
    match lang {
        "it" => Words {
            kinds: ["persona", "azienda", "telemarketing", "truffa", "sconosciuto"],
            likely: "Probabile",
            unknown_caller: "Chiamante sconosciuto",
            contact: "Contatto",
            not_a_number: "Non è un numero di telefono",
            spam: "spam",
            head: ["Numero", "Spam", "Tipo", "Rispondere"],
            source_jev: "Fonte: Jev (TypeSafe), solo dai fatti sul numero: paese, tipo di linea, contatti, le tue chiamate con quel numero e le segnalazioni pubbliche (180.dk).",
            source_rules: "Fonte: regole di base (Jev non raggiungibile): contatti, segnalazioni pubbliche e tipo di linea.",
            called_before: "ha già chiamato {n} volte",
            once_before: "ha già chiamato una volta",
            mentioned: "citato nei tuoi messaggi",
            own_label: "etichetta tua",
            in_contacts: "nei contatti",
            dk70: "numero 70 danese (linea aziendale)",
            reports: "{src}: {n} segnalazioni",
            no_reports: "{src}: nessuna segnalazione",
            likely_ok: "Nessuna segnalazione su {src} → probabilmente ok",
        },
        "da" => Words {
            kinds: ["privatperson", "virksomhed", "telemarketing", "svindel", "ukendt"],
            likely: "Sandsynligvis",
            unknown_caller: "Ukendt opkald",
            contact: "Kontakt",
            not_a_number: "Ikke et telefonnummer",
            spam: "spam",
            head: ["Nummer", "Spam", "Type", "Tag den"],
            source_jev: "Kilde: Jev (TypeSafe), kun ud fra fakta om nummeret: land, linjetype, kontakter, dine opkald med nummeret og offentlige anmeldelser (180.dk).",
            source_rules: "Kilde: enkle regler (Jev kan ikke nås): kontakter, offentlige anmeldelser og linjetype.",
            called_before: "har ringet {n} gange før",
            once_before: "har ringet én gang før",
            mentioned: "nævnt i dine beskeder",
            own_label: "din egen etiket",
            in_contacts: "i kontakter",
            dk70: "dansk 70-nummer (firmalinje)",
            reports: "{src}: {n} anmeldelser",
            no_reports: "{src}: ingen anmeldelser",
            likely_ok: "Ingen anmeldelser på {src} → sandsynligvis ok",
        },
        _ => Words {
            kinds: ["person", "business", "telemarketing", "scam", "unknown"],
            likely: "Likely",
            unknown_caller: "Unknown caller",
            contact: "Contact",
            not_a_number: "Not a phone number",
            spam: "spam",
            head: ["Number", "Spam", "Kind", "Pick up"],
            source_jev: "Source: Jev (TypeSafe), from facts about each number only: country, line type, your contacts, your calls with it and public caller-ID reports (180.dk).",
            source_rules: "Source: basic rules (Jev unreachable): contacts, public reports and line type.",
            called_before: "called {n} times before",
            once_before: "called once before",
            mentioned: "mentioned in your messages",
            own_label: "your own label",
            in_contacts: "in contacts",
            dk70: "Danish 70-number (company line)",
            reports: "{src}: {n} reports",
            no_reports: "{src}: no reports",
            likely_ok: "No reports in {src} → likely OK",
        },
    }
}

/// `en`, `it` or `da` (anything else is English).
pub fn lang(l: &str) -> &'static str {
    match l.trim().to_ascii_lowercase().get(..2) {
        Some("it") => "it",
        Some("da") => "da",
        _ => "en",
    }
}

/// `0.93` (en) or `0,93` (it, da).
pub fn p2(p: f64, lang: &str) -> String {
    let s = format!("{:.2}", p.clamp(0.0, 1.0));
    if lang == "en" {
        s
    } else {
        s.replace('.', ",")
    }
}

fn kind_word(w: &Words, kind: &str) -> &'static str {
    KINDS
        .iter()
        .position(|(k, _)| *k == kind)
        .map_or(w.kinds[4], |i| w.kinds[i])
}

/// The localized kind name.
pub fn kind_name(kind: &str, lang: &str) -> &'static str {
    kind_word(&words(lang), kind)
}

fn country_line(n: &Number, lang: &str) -> String {
    let w = words(lang);
    if n.country == "DK" && n.line == LineType::NonGeographic {
        return w.dk70.to_owned();
    }
    let line = match (lang, n.line) {
        ("it", LineType::Mobile) => "cellulare",
        ("it", LineType::Landline) => "fisso",
        ("it", LineType::NonGeographic) => "numero aziendale",
        ("it", LineType::Freephone) => "numero verde",
        ("it", LineType::Premium) => "numero a pagamento",
        ("da", LineType::Mobile) => "mobil",
        ("da", LineType::Landline) => "fastnet",
        ("da", LineType::NonGeographic) => "firmanummer",
        ("da", LineType::Freephone) => "gratisnummer",
        ("da", LineType::Premium) => "overtakseret nummer",
        (_, LineType::Mobile) => "mobile",
        (_, LineType::Landline) => "landline",
        (_, LineType::NonGeographic) => "company number",
        (_, LineType::Freephone) => "freephone",
        (_, LineType::Premium) => "premium-rate",
        _ => "?",
    };
    let country = if n.country.is_empty() { "?" } else { n.country };
    format!("{country} {line}")
}

/// A telemarketing or scam top kind is named from this probability when Jev
/// also finds the call unwanted (live 2026-10-29: a Danish 70-number came back
/// telemarketing 0.46 with spam 0.62 and was labelled "Unknown caller").
const UNWANTED_MIN: f64 = 0.4;

fn unwanted(kind: &str) -> bool {
    matches!(kind, "telemarketing" | "scam")
}

/// Builds the answer for one number. A contact or the owner's own label wins
/// over the probabilities for the label.
pub fn verdict(n: &Number, ctx: &Context, p: &Probs, source: &str, lang: &str) -> Verdict {
    let w = words(lang);
    let k = &ctx.known;
    let boosted = no_reports_boost(p, k);
    let p = &boosted;
    let (kind, kind_p) = p.top();
    let mut source = source.to_owned();
    let (label, short) = if !n.valid() {
        (w.not_a_number.to_owned(), w.not_a_number.to_owned())
    } else if let Some(name) = &k.contact {
        source = "contact".into();
        (format!("{}: {name}", w.contact), name.clone())
    } else if let Some(l) = &k.owner_label {
        (l.clone(), l.clone())
    } else if kind != "unknown"
        && (kind_p >= 0.5 || (unwanted(kind) && kind_p >= UNWANTED_MIN && p.spam >= 0.5))
    {
        let name = kind_word(&w, kind);
        (
            format!("{} {name} ({})", w.likely, p2(kind_p, lang)),
            format!("{} {:.0}%", capitalize(name), kind_p * 100.0),
        )
    } else if p.spam >= 0.7 {
        (
            format!("{} {} ({})", w.likely, w.spam, p2(p.spam, lang)),
            format!("Spam {:.0}%", p.spam * 100.0),
        )
    } else if k.no_reports() {
        let l = w
            .likely_ok
            .replace("{src}", &k.checked_sources().replace(", ", "/"));
        (l, "No reports · OK?".to_owned())
    } else {
        (w.unknown_caller.to_owned(), w.unknown_caller.to_owned())
    };
    let mut reasons = Vec::new();
    if n.valid() {
        reasons.push(country_line(n, lang));
    }
    if k.contact.is_some() {
        reasons.push(w.in_contacts.to_owned());
    }
    if k.owner_label.is_some() {
        reasons.push(w.own_label.to_owned());
    }
    for pr in k.checked() {
        reasons.push(if pr.reports == 0 {
            w.no_reports.replace("{src}", &pr.source)
        } else {
            w.reports
                .replace("{src}", &pr.source)
                .replace("{n}", &pr.reports.to_string())
        });
    }
    match k.previous_calls {
        0 => {}
        1 => reasons.push(w.once_before.to_owned()),
        c => reasons.push(w.called_before.replace("{n}", &c.to_string())),
    }
    if !k.mentions.is_empty() {
        reasons.push(w.mentioned.to_owned());
    }
    Verdict {
        number: n.e164.clone(),
        display: n.display(),
        country: n.country.to_owned(),
        country_name: n.country_name.to_owned(),
        line: n.line,
        spam: round2(p.spam),
        kind: kind.to_owned(),
        kind_p: round2(kind_p),
        kinds: p
            .kinds
            .iter()
            .map(|(k, v)| (k.clone(), round2(*v)))
            .collect(),
        pick_up: round2(p.pick_up),
        label,
        short: clip(&short, MAX_SHORT),
        source,
        known: k.clone(),
        reasons,
    }
}

fn round2(p: f64) -> f64 {
    (p.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars()
            .take(max - 1)
            .chain(std::iter::once('…'))
            .collect()
    }
}

/// The table every surface shows, with the source lines under it (each public
/// report page quoted with its URL).
pub fn table(results: &[Verdict], lang: &str) -> String {
    let w = words(lang);
    let mut out = format!(
        "| {} | {} | {} | {} |\n|---|---|---|---|\n",
        w.head[0], w.head[1], w.head[2], w.head[3]
    );
    for v in results {
        if v.line == LineType::Invalid {
            out.push_str(&format!("| {} | – | {} | – |\n", v.display, w.not_a_number));
            continue;
        }
        let kind = if v.source == "contact" {
            v.label.clone()
        } else {
            format!("{} {}", kind_word(&w, &v.kind), p2(v.kind_p, lang))
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            v.display,
            p2(v.spam, lang),
            kind,
            p2(v.pick_up, lang)
        ));
    }
    let jev = results.iter().any(|v| v.source == "jev");
    let rules = results
        .iter()
        .any(|v| v.source == "rules" && v.line != LineType::Invalid);
    if jev {
        out.push('\n');
        out.push_str(w.source_jev);
    }
    if rules {
        out.push('\n');
        out.push_str(w.source_rules);
    }
    for v in results {
        if let Some(p) = v.known.public.as_ref().filter(|p| p.reports > 0) {
            out.push_str(&format!("\n- {}: {} — {}", v.display, p.summary(), p.url));
        }
    }
    out
}

/// The Call Directory entries for these answers: strictly ascending, unique,
/// contacts left out (iOS shows the contact's own name).
pub fn directory(verdicts: &[Verdict]) -> Vec<(i64, String)> {
    let mut out: BTreeMap<i64, String> = BTreeMap::new();
    for v in verdicts {
        if v.source == "contact" || v.line == LineType::Invalid {
            continue;
        }
        if let Some(d) = parse(&v.number).directory_number() {
            out.insert(d, v.short.clone());
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------- 180.dk

/// The public caller-ID site Lucina reads first.
pub const SOURCE_180: &str = "180.dk";
/// Characters of a quoted comment.
const MAX_COMMENT: usize = 200;

/// The 180.dk page of a number: Danish numbers by their 8 digits, others as
/// `00<country code><number>` (the site's own links use these forms).
pub fn url_180(base: &str, n: &Number) -> Option<String> {
    if !n.valid() || n.cc.is_empty() {
        return None;
    }
    let base = base.trim_end_matches('/');
    Some(if n.cc == "45" {
        format!("{base}/nummer/{}", n.national)
    } else {
        format!("{base}/nummer/00{}{}", n.cc, n.national)
    })
}

/// `&amp;`, `&#x2013;`, `&#248;` and friends.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The page's visible text, one trimmed line per text node.
fn text_lines(html: &str) -> Vec<String> {
    let mut body = String::with_capacity(html.len() / 4);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let skip = ["<script", "<style"]
            .iter()
            .find(|t| lower[i..].starts_with(*t))
            .map(|t| {
                if *t == "<script" {
                    "</script>"
                } else {
                    "</style>"
                }
            });
        if let Some(close) = skip {
            match lower[i..].find(close) {
                Some(j) => {
                    i += j + close.len();
                    continue;
                }
                None => break,
            }
        }
        if html.as_bytes()[i] == b'<' {
            match html[i..].find('>') {
                Some(j) => {
                    body.push('\n');
                    i += j + 1;
                    continue;
                }
                None => break,
            }
        }
        let next = html[i..].find('<').map_or(html.len(), |j| i + j);
        body.push_str(&html[i..next]);
        i = next;
    }
    unescape(&body)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect()
}

/// "2.135" → 2135.
fn count(s: &str) -> Option<u32> {
    let d: String = s.chars().filter(|c| *c != '.' && *c != ' ').collect();
    d.parse().ok()
}

/// The page's report categories in English.
fn category_180(da: &str) -> Option<&'static str> {
    Some(match da {
        "Forsigtig" => "caution",
        "Irriterende" => "annoying",
        "Robocall" => "robocall",
        "Telemarketing" => "telemarketing",
        "Markedsundersøgelse" => "market research",
        "Meningsmåling" => "poll",
        "Sikkert" => "safe",
        "Bud/levering" => "delivery",
        "Andet" => "other",
        _ => return None,
    })
}

/// Reads a 180.dk number page: searches, user reports, report categories,
/// operator and the latest comment. `None` when the page is not a number page.
pub fn parse_180(html: &str, url: &str) -> Option<PublicReport> {
    let lines = text_lines(html);
    let at = |label: &str| lines.iter().position(|l| l == label);
    let searched = at("Antal gange søgte")?;
    let reported = at("Rapporteret af brugere")?;
    let searches = searched
        .checked_sub(1)
        .and_then(|i| count(&lines[i]))
        .unwrap_or(0);
    let reports = reported
        .checked_sub(1)
        .and_then(|i| count(&lines[i]))
        .unwrap_or(0);
    let operator = searched.checked_sub(2).map(|i| &lines[i]).filter(|l| {
        !l.starts_with('+')
            && !l.starts_with('(')
            && !l
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == ' ')
    });
    let mut categories = Vec::new();
    for l in &lines {
        let Some(open) = l.rfind(" (") else { continue };
        if !l.ends_with("%)") {
            continue;
        }
        let (name, pct) = (&l[..open], &l[open + 2..l.len() - 2]);
        if let (Some(cat), Ok(p)) = (category_180(name), pct.replace(',', ".").parse::<f64>()) {
            if !categories.iter().any(|c: &ReportShare| c.category == cat) {
                categories.push(ReportShare {
                    category: cat.to_owned(),
                    percent: p,
                });
            }
        }
    }
    let latest_comment = if reports > 0 {
        at("Seneste kommentar").and_then(|i| {
            let parts: Vec<&str> = lines[i + 1..]
                .iter()
                .take(3)
                .map(String::as_str)
                .take_while(|l| {
                    !l.starts_with("Kommentar fra")
                        && !l.starts_with("Stop ")
                        && !l.starts_with("Dette nummer")
                })
                .collect();
            match parts.as_slice() {
                [date, title, text] => Some(format!("{date} — {title}: {text}")),
                [date, title] => Some(format!("{date} — {title}")),
                _ => None,
            }
        })
    } else {
        None
    };
    Some(PublicReport {
        source: SOURCE_180.into(),
        url: url.to_owned(),
        searches,
        reports,
        categories: if reports > 0 { categories } else { Vec::new() },
        latest_comment: latest_comment.map(|c| clip(&c, MAX_COMMENT)),
        operator: operator.cloned(),
        score: None,
        flags: Vec::new(),
    })
}

#[cfg(test)]
mod tests_180 {
    use super::*;

    /// The structure of 180.dk's page for 20682758 on 2026-10-29 (text as served, tags reduced).
    const PAGE: &str = r#"<html><head><title>20682758 | ADVARSEL &#x2013; rapporter: 6</title>
<style>.x{color:red}</style><script>var a = "<div>Antal gange søgte</div>";</script></head><body>
<h1>20682758</h1><span>(Danmark)</span><span>+4520682758</span>
<div><b>374</b><small>Antal gange s&#xF8;gte</small></div>
<div><b>6</b><small>Rapporteret af brugere</small></div>
<div><b>29. okt.</b><small>Sidste s&#xF8;gte</small></div>
<h3>Seneste kommentar</h3><p>26. oktober 2026</p><p>Irriterende</p><p>De ringer i min arbejdstid</p>
<p>Kommentar fra bruger</p>
<ul><li>Forsigtig (37,5%)</li><li>Irriterende (37,5%)</li><li>Robocall (0%)</li><li>Telemarketing (25%)</li>
<li>Bud/levering (0%)</li><li>Andet (0%)</li></ul></body></html>"#;

    const EMPTY: &str = r#"<title>+35794095733 &#x2013; Ukendt nummer?</title>
<span>+35794095733</span><span>(Cypern)</span><span>+35794095733</span><span>Lemontel</span>
<b>1</b><small>Antal gange søgte</small><b>0</b><small>Rapporteret af brugere</small>
<p>Dette nummer har ingen rapporter eller vurderinger endnu.</p><h3>Seneste kommentar</h3><p>Stop robotopkald</p>"#;

    #[test]
    fn reads_a_reported_number() {
        let r = parse_180(PAGE, "https://www.180.dk/nummer/20682758").unwrap();
        assert_eq!((r.searches, r.reports), (374, 6));
        assert_eq!(r.operator, None);
        assert_eq!(
            r.latest_comment.as_deref(),
            Some("26. oktober 2026 — Irriterende: De ringer i min arbejdstid")
        );
        let top: Vec<_> = r
            .top_categories(5)
            .iter()
            .map(|c| (c.category.clone(), c.percent))
            .collect();
        assert_eq!(
            top,
            vec![
                ("caution".to_owned(), 37.5),
                ("annoying".to_owned(), 37.5),
                ("telemarketing".to_owned(), 25.0)
            ]
        );
        assert!(r.summary().starts_with(
            "180.dk: 6 user reports (looked up 374 times); categories: caution 37.5%"
        ));
    }

    #[test]
    fn reads_an_unreported_number_and_refuses_other_pages() {
        let r = parse_180(EMPTY, "u").unwrap();
        assert_eq!((r.searches, r.reports), (1, 0));
        assert_eq!(r.operator.as_deref(), Some("Lemontel"));
        assert!(r.categories.is_empty() && r.latest_comment.is_none());
        assert_eq!(
            r.summary(),
            "180.dk: no user reports (looked up 1 times); operator Lemontel"
        );
        assert!(parse_180("<html><title>404</title><p>Not found</p></html>", "u").is_none());
        assert_eq!(count("2.135"), Some(2135));
        assert_eq!(
            unescape("a &amp; b &#x2013; &#248; &bogus; &"),
            "a & b – ø &bogus; &"
        );
    }

    #[test]
    fn page_urls() {
        let b = "https://www.180.dk/";
        assert_eq!(
            url_180(b, &parse("+45 20 68 27 58")).unwrap(),
            "https://www.180.dk/nummer/20682758"
        );
        assert_eq!(
            url_180(b, &parse("+357 94 095733")).unwrap(),
            "https://www.180.dk/nummer/0035794095733"
        );
        assert_eq!(
            url_180(b, &parse("+44 7459 508537")).unwrap(),
            "https://www.180.dk/nummer/00447459508537"
        );
        assert!(url_180(b, &parse("hello")).is_none());
    }

    /// The real pages, when saved: `CALLER_180_PAGES=<dir with n<number>.html>`.
    #[test]
    fn real_pages_when_given() {
        let Ok(dir) = std::env::var("CALLER_180_PAGES") else {
            return;
        };
        for (file, reports) in [
            ("n20682758.html", 6),
            ("n70405205.html", 23),
            ("n66778702.html", 0),
        ] {
            let html = std::fs::read_to_string(format!("{dir}/{file}")).unwrap();
            let r = parse_180(&html, file).unwrap();
            assert_eq!(r.reports, reports, "{file}: {r:?}");
            assert!(r.searches > 0, "{file}");
            eprintln!("{file}: {}", r.summary());
        }
    }
}

#[cfg(test)]
mod reject_tests {
    use super::*;

    fn report(source: &str, reports: u32) -> PublicReport {
        PublicReport {
            source: source.into(),
            url: format!("https://example/{source}"),
            searches: 10,
            reports,
            ..PublicReport::default()
        }
    }

    /// A verdict from Jev's probabilities (spam, pick-up) and the report facts.
    fn v(raw: &str, spam: f64, pick_up: f64, reports: Option<u32>) -> Verdict {
        let mut ctx = Context::default();
        ctx.known.public = reports.map(|n| report("180.dk", n));
        let p = Probs {
            spam,
            kinds: KINDS
                .iter()
                .map(|(k, _)| ((*k).to_owned(), if *k == "unknown" { 0.6 } else { 0.1 }))
                .collect(),
            pick_up,
        };
        verdict(&parse(raw), &ctx, &p, "jev", "en")
    }

    fn decide(raw: &str, verdict: &Verdict) -> Result<Reject, Keep> {
        reject_decision(raw, verdict, &Guards::default(), &RejectSettings::default())
    }

    #[test]
    fn the_defaults_are_the_owners() {
        let s = RejectSettings::default();
        assert!(s.enabled && s.valid());
        assert_eq!(
            (s.spam_min, s.pick_up_max, s.order_spam_min),
            (0.60, 0.25, 0.97)
        );
    }

    #[test]
    fn thresholds() {
        assert!(decide("+4520000001", &v("+4520000001", 0.60, 0.5, Some(3))).is_ok());
        assert_eq!(
            decide("+4520000001", &v("+4520000001", 0.59, 0.5, Some(3))),
            Err(Keep::BelowThresholds)
        );
        let r = decide("+4520000001", &v("+4520000001", 0.2, 0.25, Some(3))).unwrap();
        assert_eq!((r.spam, r.pick_up), (None, Some(0.25)));
        assert_eq!(
            decide("+4520000001", &v("+4520000001", 0.2, 0.26, Some(3))),
            Err(Keep::BelowThresholds)
        );
        let off = RejectSettings {
            enabled: false,
            ..RejectSettings::default()
        };
        assert_eq!(
            reject_decision(
                "+4520000001",
                &v("+4520000001", 0.99, 0.0, Some(9)),
                &Guards::default(),
                &off
            ),
            Err(Keep::Off)
        );
        let bad = RejectSettings {
            spam_min: 1.5,
            ..RejectSettings::default()
        };
        assert_eq!(
            reject_decision(
                "+4520000001",
                &v("+4520000001", 0.99, 0.0, Some(9)),
                &Guards::default(),
                &bad
            ),
            Err(Keep::Off)
        );
    }

    #[test]
    fn never_reject_what_he_knows_or_needs() {
        let spammy = v("+4520000001", 0.99, 0.01, Some(20));
        let s = RejectSettings::default();
        for (g, keep) in [
            (
                Guards {
                    contact: true,
                    ..Guards::default()
                },
                Keep::Contact,
            ),
            (
                Guards {
                    called_or_answered: true,
                    ..Guards::default()
                },
                Keep::CalledOrAnswered,
            ),
            (
                Guards {
                    allowed: true,
                    ..Guards::default()
                },
                Keep::Allowed,
            ),
        ] {
            assert_eq!(reject_decision("+4520000001", &spammy, &g, &s), Err(keep));
        }
        for raw in ["112", "1813", "114", "911"] {
            assert_eq!(
                reject_decision(raw, &spammy, &Guards::default(), &s),
                Err(Keep::Emergency),
                "{raw}"
            );
        }
        for raw in ["1850", "90909", "4444"] {
            assert_eq!(
                reject_decision(raw, &spammy, &Guards::default(), &s),
                Err(Keep::ShortNumber),
                "{raw}"
            );
        }
        assert_eq!(
            reject_decision(
                "hidden",
                &v("hidden", 0.99, 0.0, None),
                &Guards::default(),
                &s
            ),
            Err(Keep::ShortNumber)
        );
        // During a Wolt order only near-certain spam is rejected.
        let order = Guards {
            order_active: true,
            ..Guards::default()
        };
        assert_eq!(
            reject_decision(
                "+4520000001",
                &v("+4520000001", 0.95, 0.01, Some(20)),
                &order,
                &s
            ),
            Err(Keep::OrderActive)
        );
        assert!(reject_decision(
            "+4520000001",
            &v("+4520000001", 0.98, 0.5, Some(20)),
            &order,
            &s
        )
        .is_ok());
        // The fallback rules never reject without report facts.
        let mut rules = v("+4520000001", 0.99, 0.0, None);
        rules.source = "rules".into();
        assert_eq!(decide("+4520000001", &rules), Err(Keep::NoEvidence));
    }

    #[test]
    fn zero_reports_is_a_safe_indicator() {
        // A "scam-looking" Italian mobile with zero reports: never rejected at 0.60 / 0.25.
        for (spam, pick) in [(0.65, 0.3), (0.95, 0.2), (0.99, 0.1)] {
            let x = v("+39 338 303 7513", spam, pick, Some(0));
            assert!(
                x.spam < 0.6 && x.pick_up > 0.25,
                "{spam} {pick} → {} {}",
                x.spam,
                x.pick_up
            );
            assert!(decide("+39 338 303 7513", &x).is_err());
            assert_eq!(x.label, "No reports in 180.dk → likely OK");
            assert!(x.reasons.contains(&"180.dk: no reports".to_owned()));
        }
        // A failed lookup is unknown, not safe: no boost.
        let unknown = v("+39 338 303 7513", 0.65, 0.3, None);
        assert_eq!((unknown.spam, unknown.pick_up), (0.65, 0.3));
        assert!(decide("+39 338 303 7513", &unknown).is_ok());
        // Two sources, one with reports: no boost.
        let mut ctx = Context::default();
        ctx.known.public = Some(report("180.dk", 0));
        ctx.known.databases = vec![report("tellows", 4)];
        assert!(!ctx.known.no_reports());
        let st = state(&parse("+4520000001"), &ctx).to_string();
        assert!(!st.contains("zero reports"));
        ctx.known.databases = vec![report("tellows", 0)];
        assert!(state(&parse("+4520000001"), &ctx)
            .to_string()
            .contains("Checked 180.dk, tellows: zero reports, not on any spam list."));
    }

    /// Today's 7 numbers (2026-10-29) with live Jev's probabilities and 180.dk's facts.
    #[test]
    fn replay_of_the_owners_seven_callers() {
        let cases = [
            // (number, Jev spam, Jev pick-up, 180.dk reports, rejected?)
            ("+45 70 40 52 05", 0.75, 0.16, Some(23), true),
            ("+45 70 40 52 20", 0.82, 0.11, Some(1), true),
            ("+45 20 68 27 58", 0.67, 0.15, Some(6), true),
            // The same number when the report lookup failed: Jev alone.
            ("+45 20 68 27 58", 0.43, 0.34, None, false),
            ("+45 66 77 87 02", 0.50, 0.32, Some(0), false),
            // Zero reports on 180.dk: the owner's safe indicator keeps them (spam × 0.6, pick-up + 0.2).
            ("+357 94 095733", 0.30, 0.50, Some(0), false),
            ("+357 94 462808", 0.30, 0.47, Some(0), false),
            ("+44 7459 508537", 0.36, 0.43, Some(0), false),
        ];
        for (raw, spam, pick, reports, rejected) in cases {
            let x = v(raw, spam, pick, reports);
            let d = decide(raw, &x);
            assert_eq!(
                d.is_ok(),
                rejected,
                "{raw} {spam}/{pick} {reports:?}: {d:?} ({} / {})",
                x.spam,
                x.pick_up
            );
        }
        let x = v("+45 20 68 27 58", 0.67, 0.15, Some(6));
        let line = reject_line(&x, &decide("+45 20 68 27 58", &x).unwrap());
        assert_eq!(
            line,
            "Rejected likely spam +45 20 68 27 58 · 180.dk 6 reports"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::default()
    }

    fn report_180(reports: u32, cats: &[(&str, f64)]) -> PublicReport {
        PublicReport {
            source: "180.dk".into(),
            url: "https://www.180.dk/nummer/20682758".into(),
            searches: 374,
            reports,
            categories: cats
                .iter()
                .map(|(c, p)| ReportShare {
                    category: (*c).into(),
                    percent: *p,
                })
                .collect(),
            latest_comment: Some("26 Oct 2026 — Irriterende: De ringer i min arbejdstid".into()),
            operator: None,
            score: None,
            flags: Vec::new(),
        }
    }

    #[test]
    fn parses_the_owners_callers() {
        let n = parse("+45 70 40 52 05");
        assert_eq!(n.e164, "+4570405205");
        assert_eq!((n.country, n.line), ("DK", LineType::NonGeographic));
        assert_eq!(n.display(), "+45 70 40 52 05");
        assert_eq!(n.directory_number(), Some(4570405205));

        let n = parse("+357 94 095733");
        assert_eq!(
            (n.e164.as_str(), n.country, n.line),
            ("+35794095733", "CY", LineType::Mobile)
        );
        assert_eq!(n.display(), "+357 94 095733");

        let n = parse("+44 7459 508537");
        assert_eq!((n.country, n.line), ("GB", LineType::Mobile));
        assert_eq!(n.display(), "+44 7459 508537");

        assert_eq!(parse("20 68 27 58").line, LineType::Mobile);
        assert_eq!(parse("20682758").e164, "+4520682758");
        assert_eq!(parse("4566778702").e164, "+4566778702");
        assert_eq!(parse("+4566778702").line, LineType::Landline);
        assert_eq!(parse("0045 70 40 52 20").e164, "+4570405220");
        assert_eq!(parse("(+45) 80 20 20 20").line, LineType::Freephone);
        assert_eq!(parse("90 12 34 56").line, LineType::Premium);
        assert_eq!(parse("+39 333 123 4567").line, LineType::Mobile);
    }

    #[test]
    fn rejects_what_is_not_a_number() {
        for s in [
            "",
            "hello",
            "1234",
            "+45 1234",
            "+44 12",
            "112",
            "*21#",
            "tel:12345678",
            "+45 70 40 52 05 99 99 99 99 99",
        ] {
            assert!(!parse(s).valid(), "{s:?}");
        }
        assert_eq!(parse("1234").display(), "1234");
        assert_eq!(parse("1234").directory_number(), None);
    }

    #[test]
    fn finds_numbers_in_messages_and_lists() {
        let found = find_numbers(
            "Who called me from +45 70 40 52 05? Also 0035794095733 and 20682758, again +4570405205",
        );
        let e: Vec<_> = found.iter().map(|n| n.e164.as_str()).collect();
        assert_eq!(e, ["+4570405205", "+35794095733", "+4520682758"]);
        let list = "+45 70 40 52 05\n+357 94 095733\n+45 70 40 52 20\n+357 94 462808\n+45 20 68 27 58\n+44 7459 508537\n+45 66 77 87 02";
        assert_eq!(find_numbers(list).len(), 7);
        assert!(find_numbers("meet at 12.10.2026 10:30").is_empty());
        assert!(find_numbers("order 1234 costs 89 kr").is_empty());
    }

    #[test]
    fn no_delivery_kind_and_no_guesses_about_callers() {
        assert!(KINDS.iter().all(|(k, d)| *k != "delivery"
            && !d.to_lowercase().contains("wolt")
            && !d.to_lowercase().contains("deliver")));
        for lang in ["en", "it", "da"] {
            let w = words(lang);
            let all = format!("{:?}", w).to_lowercase();
            assert!(!all.contains("wolt"), "{lang}: {all}");
        }
        let mut c = ctx();
        c.ringing = true;
        let text = state(&parse("+4520682758"), &c).to_string().to_lowercase();
        for banned in [
            "wolt",
            "courier",
            "restaurant",
            "deliver",
            "shift",
            "cyprus or the uk",
        ] {
            assert!(!text.contains(banned), "{banned} in {text}");
        }
    }

    #[test]
    fn rules_use_facts_only() {
        let c = ctx();
        // Without facts, a number is just unknown, whatever its country.
        for n in [
            "+35794095733",
            "+447459508537",
            "+4520682758",
            "+4566778702",
        ] {
            assert_eq!(rules(&parse(n), &c).top().0, "unknown", "{n}");
        }
        assert_eq!(rules(&parse("+4570405205"), &c).top().0, "business");
        // Public reports decide when there are enough of them.
        let mut r = ctx();
        r.known.public = Some(report_180(
            6,
            &[
                ("caution", 37.5),
                ("annoying", 37.5),
                ("telemarketing", 25.0),
            ],
        ));
        let p = rules(&parse("+4520682758"), &r);
        assert_eq!(p.top().0, "telemarketing");
        assert!(p.spam <= 0.7 && p.pick_up < 0.3);
        r.known.public = Some(report_180(2, &[("telemarketing", 100.0)]));
        assert_eq!(rules(&parse("+4520682758"), &r).top().0, "unknown");
        let mut k = ctx();
        k.known.contact = Some("Mamma".into());
        assert_eq!(rules(&parse("+393331234567"), &k).top().0, "person");
        for (_, v) in rules(&parse("+4570405205"), &ctx()).kinds {
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn labels_say_what_the_owner_needs() {
        let n = parse("+35794095733");
        let p = probs(0.93, "scam", 0.93, 0.04);
        let v = verdict(&n, &ctx(), &p, "jev", "en");
        assert_eq!(v.label, "Likely scam (0.93)");
        assert_eq!(v.short, "Scam 93%");
        assert_eq!(v.source, "jev");
        assert_eq!(
            verdict(&n, &ctx(), &p, "jev", "da").label,
            "Sandsynligvis svindel (0,93)"
        );
        assert_eq!(
            verdict(&n, &ctx(), &p, "jev", "it").label,
            "Probabile truffa (0,93)"
        );

        let mut r = ctx();
        r.known.public = Some(report_180(6, &[("telemarketing", 25.0)]));
        let t = verdict(
            &parse("+4520682758"),
            &r,
            &probs(0.67, "telemarketing", 0.56, 0.15),
            "jev",
            "en",
        );
        assert_eq!(t.label, "Likely telemarketing (0.56)");
        assert!(
            t.reasons.contains(&"180.dk: 6 reports".to_owned()),
            "{:?}",
            t.reasons
        );

        // An unwanted kind under 0.5 still names itself when the call is spam.
        let t = verdict(
            &parse("+4570405205"),
            &ctx(),
            &probs(0.62, "telemarketing", 0.46, 0.3),
            "jev",
            "en",
        );
        assert_eq!(t.label, "Likely telemarketing (0.46)");
        let t = verdict(
            &parse("+4570405205"),
            &ctx(),
            &probs(0.45, "telemarketing", 0.46, 0.3),
            "jev",
            "en",
        );
        assert_eq!(t.label, "Unknown caller");
        let u = verdict(
            &parse("+4566778702"),
            &ctx(),
            &probs(0.3, "unknown", 0.68, 0.34),
            "jev",
            "en",
        );
        assert_eq!(u.label, "Unknown caller");
        let s = verdict(
            &parse("+4566778702"),
            &ctx(),
            &probs(0.8, "business", 0.35, 0.2),
            "jev",
            "en",
        );
        assert_eq!(s.label, "Likely spam (0.80)");

        let mut c = ctx();
        c.known.contact = Some("Jerry's Pizza".into());
        let k = verdict(
            &parse("+4520682758"),
            &c,
            &probs(0.02, "person", 0.9, 0.95),
            "jev",
            "en",
        );
        assert_eq!(
            (k.label.as_str(), k.source.as_str()),
            ("Contact: Jerry's Pizza", "contact")
        );
        assert!(directory(&[k]).is_empty());

        let bad = verdict(
            &parse("hello"),
            &ctx(),
            &rules(&parse("hello"), &ctx()),
            "rules",
            "en",
        );
        assert_eq!(bad.label, "Not a phone number");
        assert_eq!(bad.line, LineType::Invalid);
    }

    #[test]
    fn state_holds_the_facts_and_the_public_reports() {
        let mut c = ctx();
        c.known.previous_calls = 2;
        c.known.last_call = Some("2026-10-28T10:00:00Z".into());
        c.known.mentions = vec!["the 70 number again".into()];
        c.known.public = Some(report_180(
            6,
            &[
                ("caution", 37.5),
                ("annoying", 37.5),
                ("telemarketing", 25.0),
            ],
        ));
        c.ringing = true;
        let s = state(&parse("+4520682758"), &c);
        let text = s.to_string();
        assert!(text.contains("Called him 2 times before"));
        assert!(text.contains("ringing now"));
        assert!(text.contains("the 70 number again"));
        assert!(text.contains(
            "180.dk: 6 user reports (looked up 374 times); categories: caution 37.5%, annoying 37.5%, telemarketing 25%"
        ));
        assert!(text.contains("https://www.180.dk/nummer/20682758"));
        assert!(text.contains("De ringer i min arbejdstid"));
        let none = state(&parse("+4566778702"), &ctx()).to_string();
        assert!(none.contains("No public caller-ID report could be checked."));
        assert!(state(&parse("+4570405205"), &ctx())
            .to_string()
            .contains("Danish 70-number (a non-geographic company number)"));
    }

    #[test]
    fn table_and_directory() {
        let a = verdict(
            &parse("+4570405205"),
            &ctx(),
            &probs(0.86, "telemarketing", 0.81, 0.12),
            "jev",
            "en",
        );
        let b = verdict(
            &parse("+35794095733"),
            &ctx(),
            &probs(0.93, "scam", 0.9, 0.04),
            "jev",
            "en",
        );
        let t = table(&[a.clone(), b.clone()], "en");
        assert!(t.starts_with("| Number | Spam | Kind | Pick up |\n|---|---|---|---|\n"));
        assert!(t.contains("| +45 70 40 52 05 | 0.86 | telemarketing 0.81 | 0.12 |"));
        assert!(t.contains("Source: Jev (TypeSafe)"));
        assert!(
            table(std::slice::from_ref(&a), "it").contains("| Numero | Spam | Tipo | Rispondere |")
        );
        assert!(table(std::slice::from_ref(&a), "da")
            .contains("| +45 70 40 52 05 | 0,86 | telemarketing 0,81 | 0,12 |"));
        let mut r = ctx();
        r.known.public = Some(report_180(6, &[("telemarketing", 25.0)]));
        let q = verdict(
            &parse("+4520682758"),
            &r,
            &probs(0.67, "telemarketing", 0.56, 0.15),
            "jev",
            "en",
        );
        assert!(table(&[q], "en").contains(
            "\n- +45 20 68 27 58: 180.dk: 6 user reports (looked up 374 times); categories: telemarketing 25%"
        ));
        let d = directory(&[b, a]);
        assert_eq!(
            d,
            vec![
                (4570405205, "Telemarketing 81%".to_owned()),
                (35794095733, "Scam 90%".to_owned())
            ]
        );
    }

    #[test]
    fn verdict_json_matches_the_contract() {
        let v = verdict(
            &parse("+4570405205"),
            &ctx(),
            &probs(0.86, "telemarketing", 0.81, 0.12),
            "jev",
            "en",
        );
        let j = serde_json::to_value(&v).unwrap();
        for k in [
            "number",
            "display",
            "country",
            "countryName",
            "type",
            "spam",
            "kind",
            "kindP",
            "pickUp",
            "label",
            "short",
            "source",
            "known",
            "reasons",
        ] {
            assert!(j.get(k).is_some(), "{k}");
        }
        assert_eq!(j["type"], "non_geographic");
        assert_eq!(j["known"]["previousCalls"], 0);
        let back: Verdict = serde_json::from_value(j).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn languages() {
        assert_eq!(lang("da-DK"), "da");
        assert_eq!(lang("it"), "it");
        assert_eq!(lang("fr"), "en");
        assert_eq!(kind_name("telemarketing", "da"), "telemarketing");
        assert_eq!(kind_name("delivery", "en"), "unknown");
        assert_eq!(p2(1.3, "en"), "1.00");
    }
}
