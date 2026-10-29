//! Screening unknown callers: what a number is, what Jev is asked about it,
//! and what every surface shows (the chat table, the Android notification,
//! the iOS Call Directory label). No I/O: Lucina asks Jev with [`state`] and
//! [`SPAM_QUESTION`] / [`KIND_QUESTION`] / [`PICK_UP_QUESTION`], the phones
//! use [`rules`] when Lucina is out of reach, and both build a [`Verdict`].
//!
//! Jev has no phone-number reputation database: it judges from the number's
//! country and type and from what Lucina knows (contact, earlier calls,
//! mentions in her messages, the owner's Wolt shift). Replies say so.
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

/// The owner's Wolt shift, as Lucina Maps reports it on the courier phone.
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

/// What Lucina knows about one number.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Known {
    /// The contact's name, when the number is in his contacts.
    pub contact: Option<String>,
    /// Earlier calls from this number that a phone reported.
    pub previous_calls: u32,
    /// The last of them (RFC 3339).
    pub last_call: Option<String>,
    /// Whether he was on a Wolt shift during the earlier calls (any of them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub called_during_shift: bool,
    /// The number's mentions in his messages to Lucina (short snippets).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// A label the owner gave the number himself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_label: Option<String>,
}

impl Known {
    pub fn messages(&self) -> usize {
        self.mentions.len()
    }
}

/// Everything Jev is told about one call or lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    pub courier: Option<Courier>,
    pub known: Known,
    /// "Thursday 13:05" in his time zone.
    pub local_time: Option<String>,
    /// Is this a call ringing now (a phone asked) or a lookup afterwards?
    pub ringing: bool,
}

/// Who the owner is, for Jev.
pub const OWNER: &str = "The owner lives in Aarhus, Denmark, and works as a Wolt bike courier. While he delivers, restaurants and customers often call him from Danish mobile numbers (Wolt also routes calls through Danish numbers). He is Italian and has Danish, Italian and international contacts. Most cold calls he gets are Danish 70-numbers (call centres) and foreign mobiles from Cyprus or the UK (investment and crypto scams).";

fn line_phrase(n: &Number) -> String {
    match (n.country, n.line) {
        ("DK", LineType::NonGeographic) => {
            "Danish 70-number: a company line with no location, typical of call centres and customer service".to_owned()
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

/// The state Jev decides on.
pub fn state(n: &Number, ctx: &Context) -> Value {
    let k = &ctx.known;
    let mut known = vec![];
    match &k.contact {
        Some(name) => known.push(format!("In his contacts as \"{name}\".")),
        None => known.push("Not in his contacts.".to_owned()),
    }
    if let Some(l) = &k.owner_label {
        known.push(format!("He labelled this number himself: \"{l}\"."));
    }
    known.push(match k.previous_calls {
        0 => "No earlier calls from this number that Lucina saw.".to_owned(),
        1 => format!(
            "Called once before{}{}.",
            k.last_call
                .as_deref()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default(),
            if k.called_during_shift {
                ", during a Wolt shift"
            } else {
                ""
            }
        ),
        c => format!(
            "Called {c} times before, last {}{}.",
            k.last_call.as_deref().unwrap_or("recently"),
            if k.called_during_shift {
                "; some during his Wolt shifts"
            } else {
                ""
            }
        ),
    });
    for m in &k.mentions {
        known.push(format!("He mentioned it to Lucina: \"{m}\""));
    }
    let shift = match ctx.courier {
        Some(c) if c.order_active => format!(
            "He is on a Wolt shift with an order in progress{}.",
            c.order_age_min
                .map(|m| format!(" (started {m} min ago)"))
                .unwrap_or_default()
        ),
        Some(c) if c.on_shift => "He is on a Wolt shift, between orders.".to_owned(),
        Some(_) => "He is not on a Wolt shift right now.".to_owned(),
        None => "His Wolt shift status is unknown.".to_owned(),
    };
    json!({
        "owner": OWNER,
        "call": if ctx.ringing { "An incoming call is ringing now." } else { "He asks about a number that called him." },
        "number": n.display(),
        "country": if n.country_name.is_empty() { "unknown" } else { n.country_name },
        "line": line_phrase(n),
        "known": known,
        "wolt": shift,
        "localTime": ctx.local_time.clone().unwrap_or_default(),
        "note": "There is no spam database: judge from the number, its country and type, and what Lucina knows.",
    })
}

pub const SPAM_QUESTION: &str =
    "Is this caller most likely unwanted: telemarketing, a robocall or a scam?";
pub const KIND_QUESTION: &str = "Who is most likely calling?";
pub const PICK_UP_QUESTION: &str = "Should the owner pick up this call?";

/// The kinds, in table order, with what each means for Jev.
pub const KINDS: [(&str, &str); 6] = [
    (
        "person",
        "A private person he may know (friend, family, classmate, landlord).",
    ),
    (
        "business",
        "A legitimate business or service: bank, doctor, delivery firm, employer, a shop.",
    ),
    (
        "delivery",
        "About a Wolt delivery: the customer or the restaurant of an order he is on.",
    ),
    (
        "telemarketing",
        "A sales or survey call centre, legitimate but unwanted.",
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

/// The deterministic guess when Jev is out of reach (the phone offline, the
/// quota spent). Deliberately modest: never above 0.7.
pub fn rules(n: &Number, ctx: &Context) -> Probs {
    let shift = ctx.courier.unwrap_or_default();
    if ctx.known.contact.is_some() {
        return probs(0.02, "person", 0.9, 0.95);
    }
    match (n.country, n.line) {
        (_, LineType::Invalid) => probs(0.0, "unknown", 1.0, 0.0),
        (_, LineType::Premium) => probs(0.7, "scam", 0.6, 0.05),
        ("DK", LineType::NonGeographic) => probs(0.65, "telemarketing", 0.6, 0.2),
        ("DK", LineType::Freephone) => probs(0.5, "telemarketing", 0.45, 0.3),
        ("DK", LineType::Mobile) if shift.order_active => probs(0.08, "delivery", 0.65, 0.85),
        ("DK", LineType::Mobile) if shift.on_shift => probs(0.12, "delivery", 0.45, 0.7),
        ("DK", LineType::Mobile) => probs(0.2, "person", 0.4, 0.55),
        ("DK", LineType::Landline) => probs(0.3, "business", 0.45, 0.5),
        ("DK", _) => probs(0.35, "unknown", 0.4, 0.4),
        ("IT", _) => probs(0.25, "person", 0.4, 0.5),
        ("CY" | "GB", LineType::Mobile) => probs(0.7, "scam", 0.6, 0.1),
        (_, LineType::Mobile) => probs(0.5, "scam", 0.4, 0.25),
        _ => probs(0.45, "unknown", 0.4, 0.3),
    }
}

// ---------------------------------------------------------------- words

#[derive(Debug, Clone, Copy)]
struct Words {
    kinds: [&'static str; 6],
    likely: &'static str,
    unknown_caller: &'static str,
    contact: &'static str,
    not_a_number: &'static str,
    wolt_customer: &'static str,
    maybe_delivery: &'static str,
    spam: &'static str,
    head: [&'static str; 4],
    source_jev: &'static str,
    source_rules: &'static str,
    called_before: &'static str,
    once_before: &'static str,
    on_shift: &'static str,
    order: &'static str,
    mentioned: &'static str,
    own_label: &'static str,
    in_contacts: &'static str,
    dk70: &'static str,
}

fn words(lang: &str) -> Words {
    match lang {
        "it" => Words {
            kinds: ["persona", "azienda", "consegna Wolt", "telemarketing", "truffa", "sconosciuto"],
            likely: "Probabile",
            unknown_caller: "Chiamante sconosciuto",
            contact: "Contatto",
            not_a_number: "Non è un numero di telefono",
            wolt_customer: "Probabile cliente Wolt",
            maybe_delivery: "Forse una consegna Wolt",
            spam: "spam",
            head: ["Numero", "Spam", "Tipo", "Rispondere"],
            source_jev: "Fonte: Jev (TypeSafe), in base a numero, paese, tipo di linea e a quello che so (contatti, chiamate precedenti, turno Wolt). Non è un database antispam.",
            source_rules: "Fonte: regole di base (Jev non raggiungibile): solo paese e tipo di linea.",
            called_before: "ha già chiamato {n} volte",
            once_before: "ha già chiamato una volta",
            on_shift: "sei in turno Wolt",
            order: "hai un ordine in corso",
            mentioned: "citato nei tuoi messaggi",
            own_label: "etichetta tua",
            in_contacts: "nei contatti",
            dk70: "numero 70 danese (linea aziendale)",
        },
        "da" => Words {
            kinds: ["privatperson", "virksomhed", "Wolt-levering", "telemarketing", "svindel", "ukendt"],
            likely: "Sandsynligvis",
            unknown_caller: "Ukendt opkald",
            contact: "Kontakt",
            not_a_number: "Ikke et telefonnummer",
            wolt_customer: "Sandsynligvis Wolt-kunde",
            maybe_delivery: "Måske en Wolt-levering",
            spam: "spam",
            head: ["Nummer", "Spam", "Type", "Tag den"],
            source_jev: "Kilde: Jev (TypeSafe) ud fra nummer, land, linjetype og det jeg ved (kontakter, tidligere opkald, Wolt-vagt). Ikke en spamdatabase.",
            source_rules: "Kilde: enkle regler (Jev kan ikke nås): kun land og linjetype.",
            called_before: "har ringet {n} gange før",
            once_before: "har ringet én gang før",
            on_shift: "du er på Wolt-vagt",
            order: "du har en ordre i gang",
            mentioned: "nævnt i dine beskeder",
            own_label: "din egen etiket",
            in_contacts: "i kontakter",
            dk70: "dansk 70-nummer (firmalinje)",
        },
        _ => Words {
            kinds: ["person", "business", "Wolt delivery", "telemarketing", "scam", "unknown"],
            likely: "Likely",
            unknown_caller: "Unknown caller",
            contact: "Contact",
            not_a_number: "Not a phone number",
            wolt_customer: "Likely Wolt customer",
            maybe_delivery: "Maybe a Wolt delivery",
            spam: "spam",
            head: ["Number", "Spam", "Kind", "Pick up"],
            source_jev: "Source: Jev (TypeSafe), judging from the number, country, line type and what I know (contacts, earlier calls, your Wolt shift). Not a spam database.",
            source_rules: "Source: basic rules (Jev unreachable): country and line type only.",
            called_before: "called {n} times before",
            once_before: "called once before",
            on_shift: "you are on a Wolt shift",
            order: "you have an order in progress",
            mentioned: "mentioned in your messages",
            own_label: "your own label",
            in_contacts: "in contacts",
            dk70: "Danish 70-number (company line)",
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
        .map_or(w.kinds[5], |i| w.kinds[i])
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

/// Builds the answer for one number. A contact or the owner's own label wins
/// over the probabilities for the label.
pub fn verdict(n: &Number, ctx: &Context, p: &Probs, source: &str, lang: &str) -> Verdict {
    let w = words(lang);
    let k = &ctx.known;
    let (kind, kind_p) = p.top();
    let shift = ctx.courier.unwrap_or_default();
    let mut source = source.to_owned();
    let (label, short) = if !n.valid() {
        (w.not_a_number.to_owned(), w.not_a_number.to_owned())
    } else if let Some(name) = &k.contact {
        source = "contact".into();
        (format!("{}: {name}", w.contact), name.clone())
    } else if let Some(l) = &k.owner_label {
        (l.clone(), l.clone())
    } else if kind == "delivery" && kind_p >= 0.5 && shift.on_shift {
        (w.wolt_customer.to_owned(), w.wolt_customer.to_owned())
    } else if kind == "delivery" && kind_p >= 0.4 {
        (
            format!("{} ({})", w.maybe_delivery, p2(kind_p, lang)),
            w.maybe_delivery.to_owned(),
        )
    } else if kind != "unknown" && kind_p >= 0.5 {
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
    match k.previous_calls {
        0 => {}
        1 => reasons.push(w.once_before.to_owned()),
        c => reasons.push(w.called_before.replace("{n}", &c.to_string())),
    }
    if !k.mentions.is_empty() {
        reasons.push(w.mentioned.to_owned());
    }
    if shift.order_active {
        reasons.push(w.order.to_owned());
    } else if shift.on_shift {
        reasons.push(w.on_shift.to_owned());
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

/// The table every surface shows, with the source line under it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::default()
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
        let found = find_numbers("Who called me from +45 70 40 52 05? Also 0035794095733 and 20682758, again +4570405205");
        let e: Vec<_> = found.iter().map(|n| n.e164.as_str()).collect();
        assert_eq!(e, ["+4570405205", "+35794095733", "+4520682758"]);
        let list = "+45 70 40 52 05\n+357 94 095733\n+45 70 40 52 20\n+357 94 462808\n+45 20 68 27 58\n+44 7459 508537\n+45 66 77 87 02";
        assert_eq!(find_numbers(list).len(), 7);
        assert!(find_numbers("meet at 12.10.2026 10:30").is_empty());
        assert!(find_numbers("order 1234 costs 89 kr").is_empty());
    }

    #[test]
    fn rules_are_modest_and_sensible() {
        let mut c = ctx();
        let (k, p) = rules(&parse("+4570405205"), &c).top();
        assert_eq!(k, "telemarketing");
        assert!(p <= 0.7);
        assert_eq!(rules(&parse("+35794095733"), &c).top().0, "scam");
        assert_eq!(rules(&parse("+447459508537"), &c).top().0, "scam");
        assert_eq!(rules(&parse("+4520682758"), &c).top().0, "person");
        c.courier = Some(Courier {
            on_shift: true,
            order_active: true,
            order_age_min: Some(4),
        });
        let p = rules(&parse("+4520682758"), &c);
        assert_eq!(p.top().0, "delivery");
        assert!(p.pick_up > 0.8);
        c.known.contact = Some("Mamma".into());
        assert_eq!(rules(&parse("+393331234567"), &c).top().0, "person");
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

        let mut c = ctx();
        c.courier = Some(Courier {
            on_shift: true,
            order_active: true,
            order_age_min: None,
        });
        let d = verdict(
            &parse("+4520682758"),
            &c,
            &probs(0.05, "delivery", 0.7, 0.9),
            "jev",
            "en",
        );
        assert_eq!(d.label, "Likely Wolt customer");
        assert!(d.reasons.iter().any(|r| r.contains("order")));

        let u = verdict(
            &parse("+4566778702"),
            &ctx(),
            &probs(0.3, "unknown", 0.6, 0.4),
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
    fn state_tells_jev_what_lucina_knows() {
        let mut c = ctx();
        c.known.previous_calls = 2;
        c.known.last_call = Some("2026-10-28T10:00:00Z".into());
        c.known.mentions = vec!["the 70 number again".into()];
        c.courier = Some(Courier {
            on_shift: true,
            order_active: false,
            order_age_min: None,
        });
        c.ringing = true;
        let s = state(&parse("+4570405205"), &c);
        let text = s.to_string();
        assert!(text.contains("Danish 70-number"));
        assert!(text.contains("Called 2 times before"));
        assert!(text.contains("between orders"));
        assert!(text.contains("ringing now"));
        assert!(text.contains("the 70 number again"));
        assert!(text.contains("no spam database"));
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
        assert!(table(&[a.clone()], "it").contains("| Numero | Spam | Tipo | Rispondere |"));
        assert!(table(&[a.clone()], "da")
            .contains("| +45 70 40 52 05 | 0,86 | telemarketing 0,81 | 0,12 |"));
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
        assert_eq!(kind_name("delivery", "da"), "Wolt-levering");
        assert_eq!(p2(1.3, "en"), "1.00");
    }
}
