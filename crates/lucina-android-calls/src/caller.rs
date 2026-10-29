//! Caller screening for the Android `CallScreeningService`: what to show for
//! an incoming number, and whether the owner's own settings say to silence or
//! reject it. Nothing here answers, places or ends a call.
//!
//! | Flow | What happens |
//! |---|---|
//! | [`local`] | No network. The shared core's fallback rules ([`lucina_phone_core::caller::rules`]) give the first, instant verdict. |
//! | [`screen`] | `POST /v1/callers/screen` with `call: {source: "android"}`, about [`SCREEN_TIMEOUT`] in total. Any error, bad answer or a different number falls back to [`local`] with `source: "rules"`. |
//! | [`Store`] | The settings, the last screened call and the remembered verdicts, as small files in the app's files directory (mode 0600, written atomically). |
//!
//! Settings: the label (heads-up notification) and the owner's auto-reject
//! ([`RejectSettings`]), both on by default. The decision is the shared core's
//! [`caller::reject_decision`]; this module only gathers the facts it needs
//! (the phone's guards, the allow list, the order signal) and never rejects
//! when a guard could not be checked, for a hidden number, for a contact, or on
//! the fallback rules without report facts.
//!
//! No number is logged here. The number stays in the request and, as the
//! display form, in `caller-last.json` for the app's own screen.

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

use lucina_phone_core::caller::{self, Context, Guards, Known, Number, RejectSettings, Verdict};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{net::Backend, Result};

/// Total time allowed for the screening request.
pub const SCREEN_TIMEOUT: Duration = Duration::from_millis(1200);
/// The server path.
pub const SCREEN_PATH: &str = "/v1/callers/screen";
/// Largest settings or last-call file read.
const MAX_FILE_BYTES: u64 = 16 * 1024;
/// Largest cache file read.
const MAX_CACHE_BYTES: u64 = 1024 * 1024;
/// Numbers whose verdict is remembered; the oldest go first.
pub const CACHE_ENTRIES: usize = 300;
/// A remembered verdict is used for this long.
pub const CACHE_TTL_MS: u64 = 30 * 24 * 3_600_000;
/// A Jev verdict this fresh is not replaced by the fallback rules.
const CACHE_KEEP_JEV_MS: u64 = 6 * 3_600_000;
/// Longest label, text or hint passed to the notification, in characters.
const MAX_SHOWN_CHARS: usize = 240;

/// Serialises every read-modify-write of the files within the process.
pub(crate) static LOCK: Mutex<()> = Mutex::new(());

// ------------------------------------------------------------------ files

/// The owner's switches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Show the heads-up notification with the verdict.
    #[serde(default = "yes")]
    pub label: bool,
    /// Auto-reject likely spam (on by default; his explicit choice).
    #[serde(default, deserialize_with = "lenient_reject")]
    pub reject: RejectSettings,
}

fn yes() -> bool {
    true
}

/// A settings file from 0.6 has `"reject": false` and a `silence` switch: they were off by default and are
/// replaced by the one rule, so anything that is not the new object means the defaults.
fn lenient_reject<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<RejectSettings, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            label: true,
            reject: RejectSettings::default(),
        }
    }
}

impl Settings {
    /// Apply `{"label"?: bool, "reject"?: {"enabled"?, "spamMin"?, "pickUpMax"?, "orderSpamMin"?}}`.
    /// Thresholds must be numbers in 0..=1; nothing changes if any part is refused.
    fn patch(&mut self, v: &Value) -> Result<()> {
        let obj = v.as_object().ok_or("Invalid settings.")?;
        let mut next = *self;
        for (k, val) in obj {
            match (k.as_str(), val) {
                ("label", Value::Bool(b)) => next.label = *b,
                ("reject", Value::Object(r)) => {
                    for (rk, rv) in r {
                        match (rk.as_str(), rv) {
                            ("enabled", Value::Bool(b)) => next.reject.enabled = *b,
                            ("spamMin", n) => next.reject.spam_min = unit(n)?,
                            ("pickUpMax", n) => next.reject.pick_up_max = unit(n)?,
                            ("orderSpamMin", n) => next.reject.order_spam_min = unit(n)?,
                            _ => return Err("Invalid settings.".into()),
                        }
                    }
                }
                _ => return Err("Invalid settings.".into()),
            }
        }
        if !next.reject.valid() {
            return Err("The limits must be between 0 and 1.".into());
        }
        *self = next;
        Ok(())
    }
}

fn unit(v: &Value) -> Result<f64> {
    v.as_f64()
        .filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
        .ok_or_else(|| "The limits must be between 0 and 1.".into())
}

/// The last screened call, for the app's own screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Last {
    pub at_ms: u64,
    pub display: String,
    pub label: String,
    pub kind: String,
    pub spam: f64,
    pub pick_up: f64,
    pub source: String,
    pub action: String,
}

/// The files of caller screening in the app's files directory.
#[derive(Debug, Clone)]
pub struct Store {
    pub(crate) dir: PathBuf,
}

impl Store {
    /// Open the store in `files_dir`, which must be the app's own files directory.
    pub fn open(files_dir: &str) -> Result<Self> {
        Ok(Self {
            dir: crate::journal::app_files(files_dir)?,
        })
    }

    pub(crate) fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.read_max(name, MAX_FILE_BYTES)
    }

    fn read_max<T: serde::de::DeserializeOwned>(&self, name: &str, max: u64) -> Option<T> {
        let path = self.dir.join(name);
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(max + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > max {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    pub(crate) fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let encoded = serde_json::to_vec(value).map_err(|_| "Screening save failed.")?;
        let path = self.dir.join(name);
        let temp = self.dir.join(format!("{name}.tmp"));
        let mut f = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|_| "Screening save failed.")?;
        f.write_all(&encoded)
            .and_then(|()| f.sync_all())
            .map_err(|_| "Screening save failed.")?;
        fs::rename(&temp, &path).map_err(|_| "Screening save failed.")?;
        Ok(())
    }

    /// The settings; the defaults when none were saved or the file is unreadable.
    pub fn settings(&self) -> Settings {
        let mut s: Settings = self.read("caller-settings.json").unwrap_or_default();
        if !s.reject.valid() {
            s.reject = RejectSettings::default();
        }
        s
    }

    /// Change settings and save them.
    pub fn patch_settings(&self, patch: &Value) -> Result<Settings> {
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        let mut s = self.settings();
        s.patch(patch)?;
        self.write("caller-settings.json", &s)?;
        Ok(s)
    }

    /// The last screened call.
    pub fn last(&self) -> Option<Last> {
        self.read("caller-last.json")
    }

    fn set_last(&self, last: &Last) {
        if let Ok(_lock) = LOCK.lock() {
            let _ = self.write("caller-last.json", last);
        }
    }

    fn cache(&self) -> Vec<Cached> {
        self.read_max("caller-cache.json", MAX_CACHE_BYTES)
            .unwrap_or_default()
    }

    /// Remember the verdict for a number (the call screen and the call log
    /// show it later without asking again). A fresh Jev verdict is not
    /// replaced by the fallback rules.
    fn cache_put(&self, shown: &Shown, now: u64) {
        let Ok(_lock) = LOCK.lock() else { return };
        let key = digits(&shown.number);
        if key.is_empty() {
            return;
        }
        let mut all = self.cache();
        if let Some(old) = all.iter().find(|e| digits(&e.shown.number) == key) {
            if shown.source == "rules"
                && old.shown.source != "rules"
                && now.saturating_sub(old.at_ms) < CACHE_KEEP_JEV_MS
            {
                return;
            }
        }
        all.retain(|e| digits(&e.shown.number) != key);
        all.push(Cached {
            at_ms: now,
            shown: shown.clone(),
        });
        if all.len() > CACHE_ENTRIES {
            let excess = all.len() - CACHE_ENTRIES;
            all.drain(..excess);
        }
        let _ = self.write("caller-cache.json", &all);
    }

    fn cache_get(&self, number: &str, now: u64) -> Option<Shown> {
        let key = digits(number);
        self.cache()
            .into_iter()
            .find(|e| digits(&e.shown.number) == key && now.saturating_sub(e.at_ms) <= CACHE_TTL_MS)
            .map(|e| e.shown)
    }

    /// Settings and the last call, with the line the app shows.
    pub fn state(&self, now: u64) -> Value {
        let settings = self.settings();
        let last = self.last();
        json!({
            "settings": settings,
            "last": last,
            "lastLine": last_line(last.as_ref(), now),
        })
    }
}

/// One remembered verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    at_ms: u64,
    #[serde(flatten)]
    shown: Shown,
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

fn ago(ms: u64, now: u64) -> String {
    let min = now.saturating_sub(ms) / 60_000;
    match min {
        0 => "just now".into(),
        1..=59 => format!("{min} min ago"),
        60..=2879 => format!("{} h ago", min / 60),
        _ => format!("{} days ago", min / 1440),
    }
}

fn last_line(last: Option<&Last>, now: u64) -> String {
    match last {
        None => "No call screened yet.".into(),
        Some(l) => format!("{} · {} · {}", l.label, l.display, ago(l.at_ms, now)),
    }
}

// ----------------------------------------------------------------- verdicts

/// What is shown about one number: the server's verdict, or the rules' guess.
type Shown = Verdict;

/// The first result of a server answer for `n`, cleaned; `None` if it is not
/// what was asked (another number, no label, probabilities out of range).
fn from_response(v: &Value, n: &Number) -> Option<Shown> {
    let r = v["results"].as_array()?;
    if r.len() != 1 {
        return None;
    }
    let r = &r[0];
    if r["number"].as_str()? != n.e164 {
        return None;
    }
    let p = |k: &str| r[k].as_f64().filter(|x| (0.0..=1.0).contains(x));
    let mut shown = serde_json::from_value::<Verdict>(r.clone()).unwrap_or_else(|_| Verdict {
        number: n.e164.clone(),
        display: n.display(),
        country: n.country.to_owned(),
        country_name: n.country_name.to_owned(),
        line: n.line,
        spam: 0.0,
        kind: r["kind"].as_str().unwrap_or("unknown").to_owned(),
        kind_p: 0.5,
        kinds: Default::default(),
        pick_up: 0.5,
        label: String::new(),
        short: String::new(),
        source: String::new(),
        known: Known::default(),
        reasons: r["reasons"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    });
    shown.spam = p("spam")?;
    shown.pick_up = p("pickUp").unwrap_or(0.5);
    shown.label = clean(r["label"].as_str()?, MAX_SHOWN_CHARS);
    if shown.label.is_empty() {
        return None;
    }
    shown.short = clean(&shown.short, 40);
    shown.display = if shown.display.trim().is_empty() {
        n.display()
    } else {
        clean(&shown.display, 40)
    };
    shown.number = n.e164.clone();
    shown.source = match r["source"].as_str().unwrap_or("jev") {
        s @ ("jev" | "rules" | "contact") => s.to_owned(),
        _ => "rules".to_owned(),
    };
    shown.reasons = shown.reasons.iter().take(6).map(|s| clean(s, 80)).collect();
    Some(shown)
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The notification for a call, as Kotlin shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    /// Show a notification (false for a hidden number, a contact or when the label is off).
    pub notify: bool,
    /// `allow` or `reject` (the owner's auto-reject rule, from the core).
    pub action: &'static str,
    /// Why it is rejected, when it is.
    pub reject: Option<RejectOut>,
    /// The verdict's label.
    pub title: String,
    /// Kind, pick-up hint and reasons.
    pub text: String,
    /// The number as written for reading.
    pub sub: String,
    /// `jev`, `rules` or `contact`.
    pub source: String,
    pub spam: f64,
    pub pick_up: f64,
    pub kind: String,
}

/// A rejection as the shell reports it back after the call was really rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectOut {
    /// E.164.
    pub number: String,
    /// "Rejected likely telemarketer +45 … · 180.dk 6 reports".
    pub line: String,
    pub spam: Option<f64>,
    pub pick_up: Option<f64>,
    pub sources: String,
}

/// What the app tells about a caller: facts the phone checked at ring time.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// The contact's name, when the number is in his contacts.
    pub contact: Option<String>,
    /// He called this number, or answered a call from it, before (the call log).
    pub called_or_answered: bool,
    /// Telephony says this is an emergency number.
    pub emergency: bool,
    /// A check could not run (permission missing, a query failed): never reject.
    pub unchecked: bool,
}

impl Input {
    /// `{"contact"?: string, "calledOrAnswered"?, "emergency"?, "unchecked"?}`.
    pub fn from_json(v: &Value) -> Self {
        Self {
            contact: v["contact"]
                .as_str()
                .map(|s| clean(s, 80))
                .filter(|s| !s.is_empty()),
            called_or_answered: v["calledOrAnswered"].as_bool() == Some(true),
            emergency: v["emergency"].as_bool() == Some(true),
            unchecked: v["unchecked"].as_bool() == Some(true),
        }
    }
}

/// Only facts about the number: the phone knows a contact's name, nothing else.
fn context(input: &Input) -> Context {
    Context {
        known: Known {
            contact: input.contact.clone(),
            ..Known::default()
        },
        ringing: true,
    }
}

fn local_shown(n: &Number, ctx: &Context, lang: &str) -> Shown {
    let p = caller::rules(n, ctx);
    caller::verdict(n, ctx, &p, "rules", lang)
}

/// The instant verdict, without the network.
pub fn local(store: &Store, number: &str, input: &Input, lang: &str, _now: u64) -> Screen {
    let lang = caller::lang(lang);
    let n = caller::parse(number);
    let ctx = context(input);
    let shown = local_shown(&n, &ctx, lang);
    present(&n, &shown, &store.settings(), input, lang)
}

/// The verdict from the server, or [`local`]'s when the server cannot answer.
/// It never fails; the result is saved as the last screened call.
pub fn screen(
    store: &Store,
    backend: &dyn Backend,
    number: &str,
    input: &Input,
    lang: &str,
    now: u64,
) -> Screen {
    let lang = caller::lang(lang);
    let n = caller::parse(number);
    let ctx = context(input);
    let fallback = local_shown(&n, &ctx, lang);
    let settings = store.settings();
    // Nothing leaves the phone for a hidden number or one of his contacts.
    if !n.valid() || input.contact.is_some() {
        return present(&n, &fallback, &settings, input, lang);
    }
    let shown = backend
        .post(SCREEN_PATH, &request(&n, lang, now))
        .ok()
        .and_then(|v| from_response(&v, &n))
        .unwrap_or(fallback);
    let mut out = present(&n, &shown, &settings, input, lang);
    if let Some(r) = rejection(store, &n, number, &shown, input, &settings, now) {
        out.action = "reject";
        out.reject = Some(r);
    }
    store.cache_put(&shown, now);
    store.set_last(&Last {
        at_ms: now,
        display: shown.display.clone(),
        label: shown.label.clone(),
        kind: shown.kind.clone(),
        spam: shown.spam,
        pick_up: shown.pick_up,
        source: shown.source.clone(),
        action: out.action.to_owned(),
    });
    out
}

/// What to show for `number` on the call screen: the remembered verdict
/// (Jev's, if the screening already heard back), else the instant rules
/// guess. Nothing is sent anywhere.
pub fn label(store: &Store, number: &str, input: &Input, lang: &str, now: u64) -> Screen {
    let lang = caller::lang(lang);
    let n = caller::parse(number);
    if n.valid() && input.contact.is_none() {
        if let Some(shown) = store.cache_get(&n.e164, now) {
            return present(&n, &shown, &store.settings(), input, lang);
        }
    }
    local(store, number, input, lang, now)
}

/// The number written for reading (`+45 70 40 52 05`), if it is a phone number.
pub fn display_of(number: &str) -> Option<String> {
    let n = caller::parse(number);
    n.valid().then(|| n.display())
}

/// The remembered verdict for `number`, if there is one.
pub fn cached(store: &Store, number: &str, lang: &str, now: u64) -> Option<Screen> {
    let lang = caller::lang(lang);
    let n = caller::parse(number);
    let shown = store.cache_get(&n.e164, now).filter(|_| n.valid())?;
    Some(present(
        &n,
        &shown,
        &store.settings(),
        &Input::default(),
        lang,
    ))
}

/// The owner's rule for this call, or `None` to put it through. Every check that
/// could not run counts as a reason to put the call through.
fn rejection(
    store: &Store,
    n: &Number,
    raw: &str,
    v: &Verdict,
    input: &Input,
    settings: &Settings,
    now: u64,
) -> Option<RejectOut> {
    if input.unchecked || input.emergency || input.contact.is_some() || !n.valid() {
        return None;
    }
    let guards = Guards {
        contact: false,
        called_or_answered: input.called_or_answered,
        allowed: store.is_allowed(&n.e164),
        order_active: store.order_active(now),
    };
    let r = caller::reject_decision(raw, v, &guards, &settings.reject).ok()?;
    Some(RejectOut {
        number: n.e164.clone(),
        line: caller::reject_line(v, &r),
        spam: r.spam,
        pick_up: r.pick_up,
        sources: r.sources,
    })
}

/// Whether the screening has to wait for the server before it answers: the rule is on and no
/// phone-side guard already puts the call through. Decided with a verdict that would be rejected,
/// so the core's own guard order applies.
pub fn must_wait(store: &Store, number: &str, input: &Input, now: u64) -> bool {
    let n = caller::parse(number);
    let settings = store.settings();
    if !settings.reject.enabled
        || input.unchecked
        || input.emergency
        || input.contact.is_some()
        || !n.valid()
    {
        return false;
    }
    let ctx = context(input);
    let mut worst = caller::verdict(
        &n,
        &ctx,
        &caller::Probs {
            spam: 1.0,
            kinds: Default::default(),
            pick_up: 0.0,
        },
        "jev",
        "en",
    );
    worst.spam = 1.0;
    worst.pick_up = 0.0;
    rejection(store, &n, number, &worst, input, &settings, now).is_some()
}

/// The request body of `POST /v1/callers/screen`.
fn request(n: &Number, lang: &str, now: u64) -> Value {
    json!({
        "numbers": [n.e164],
        "lang": lang,
        "call": {"source": "android", "at": rfc3339(now)},
    })
}

fn present(n: &Number, s: &Shown, settings: &Settings, input: &Input, lang: &str) -> Screen {
    Screen {
        notify: settings.label && n.valid() && input.contact.is_none(),
        action: "allow",
        reject: None,
        title: s.label.clone(),
        text: body(s, lang),
        sub: s.display.clone(),
        source: s.source.clone(),
        spam: s.spam,
        pick_up: s.pick_up,
        kind: s.kind.clone(),
    }
}

/// `Kind · pick-up hint`, then the reasons.
fn body(s: &Shown, lang: &str) -> String {
    let w = hints(lang);
    let kind = caller::kind_name(&s.kind, lang);
    let hint = if s.pick_up >= 0.6 {
        w.yes
    } else if s.pick_up <= 0.25 {
        w.no
    } else {
        w.unsure
    };
    let mut first = format!("{} · {hint}", capitalize(kind));
    if s.source == "rules" {
        first.push_str(" · ");
        first.push_str(w.quick);
    }
    if s.reasons.is_empty() {
        first
    } else {
        format!("{first}\n{}", s.reasons.join("; "))
    }
}

struct Hints {
    yes: &'static str,
    no: &'static str,
    unsure: &'static str,
    quick: &'static str,
}

fn hints(lang: &str) -> Hints {
    match lang {
        "it" => Hints {
            yes: "conviene rispondere",
            no: "probabilmente non conviene rispondere",
            unsure: "non è chiaro se rispondere",
            quick: "stima rapida",
        },
        "da" => Hints {
            yes: "det kan betale sig at tage den",
            no: "det kan næppe betale sig at tage den",
            unsure: "uvist om du skal tage den",
            quick: "hurtigt gæt",
        },
        _ => Hints {
            yes: "worth picking up",
            no: "probably not worth picking up",
            unsure: "unsure whether to pick up",
            quick: "quick guess",
        },
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `2026-10-29T12:00:00Z` for milliseconds since the epoch.
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::journal::tests::app_dir;

    const NOW: u64 = 1_793_275_200_000; // 2026-10-29T12:00:00Z

    fn store(name: &str) -> Store {
        Store::open(&app_dir(name)).unwrap()
    }

    /// Answers every request with `answer` (or an error) and remembers what was sent.
    struct Fake {
        answer: Result<Value>,
        sent: RefCell<Vec<(String, Value)>>,
    }

    impl Fake {
        fn ok(answer: Value) -> Self {
            Self {
                answer: Ok(answer),
                sent: RefCell::new(Vec::new()),
            }
        }
        fn down() -> Self {
            Self {
                answer: Err("The request failed.".into()),
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl Backend for Fake {
        fn post(&self, path: &str, body: &Value) -> Result<Value> {
            self.sent.borrow_mut().push((path.into(), body.clone()));
            self.answer.clone()
        }
    }

    fn server_answer(spam: f64, source: &str) -> Value {
        json!({
            "lang": "en",
            "results": [{
                "number": "+4570405205", "display": "+45 70 40 52 05",
                "country": "DK", "countryName": "Denmark", "type": "non_geographic",
                "spam": spam, "kind": "scam", "kindP": 0.91, "pickUp": 0.04,
                "label": "Likely scam (0.93)", "short": "Scam 91%", "source": source,
                "known": {"contact": null, "previousCalls": 2, "lastCall": null, "messages": 0},
                "reasons": ["Danish 70 number (company line)", "called 2 times before"],
            }],
            "table": "|",
        })
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(rfc3339(NOW), "2026-10-29T12:00:00Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(951_782_400_000 + 86_399_000),
            "2000-02-29T23:59:59Z"
        );
    }

    #[test]
    fn settings_default_to_label_and_auto_reject_and_persist() {
        let s = store("settings");
        assert_eq!(s.settings(), Settings::default());
        let d = s.settings();
        assert!(d.label && d.reject.enabled);
        assert_eq!(
            (
                d.reject.spam_min,
                d.reject.pick_up_max,
                d.reject.order_spam_min
            ),
            (0.60, 0.25, 0.97)
        );
        let off = s
            .patch_settings(&json!({"reject": {"enabled": false}}))
            .unwrap();
        assert!(!off.reject.enabled && off.label);
        assert_eq!(s.settings(), off);
        let edited = s
            .patch_settings(
                &json!({"reject": {"spamMin": 0.7, "pickUpMax": 0.1, "orderSpamMin": 1.0}}),
            )
            .unwrap();
        assert_eq!(
            (
                edited.reject.spam_min,
                edited.reject.pick_up_max,
                edited.reject.order_spam_min
            ),
            (0.7, 0.1, 1.0)
        );
        // Limits outside 0..1, wrong types and unknown keys are refused, and a refused patch changes nothing.
        for bad in [
            json!({"reject": {"spamMin": 1.5}}),
            json!({"reject": {"spamMin": -0.1}}),
            json!({"reject": {"pickUpMax": "low"}}),
            json!({"reject": {"enabled": "yes"}}),
            json!({"reject": true}),
            json!({"silence": true}),
            json!({"nope": true}),
            json!({"label": false, "reject": {"spamMin": 7}}),
        ] {
            assert!(s.patch_settings(&bad).is_err(), "{bad}");
        }
        assert_eq!(s.settings(), edited);
    }

    #[test]
    fn a_settings_file_from_0_6_gives_way_to_the_one_rule() {
        let dir = app_dir("migrate");
        fs::write(
            format!("{dir}/caller-settings.json"),
            br#"{"label":false,"silence":true,"reject":false,"threshold":0.9}"#,
        )
        .unwrap();
        let s = Store::open(&dir).unwrap().settings();
        assert!(!s.label, "the label choice is kept");
        assert_eq!(
            s.reject,
            RejectSettings::default(),
            "the old switches are replaced by the defaults"
        );
        // Limits that are not valid on disk fall back to the defaults.
        fs::write(
            format!("{dir}/caller-settings.json"),
            br#"{"label":true,"reject":{"enabled":true,"spamMin":9,"pickUpMax":0.25,"orderSpamMin":0.97}}"#,
        )
        .unwrap();
        assert_eq!(
            Store::open(&dir).unwrap().settings().reject,
            RejectSettings::default()
        );
    }

    #[test]
    fn files_are_private_and_only_in_the_app_directory() {
        let dir = app_dir("mode");
        let s = Store::open(&dir).unwrap();
        s.patch_settings(&json!({"label": false})).unwrap();
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(format!("{dir}/caller-settings.json"))
                .unwrap()
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o600);
        assert!(!PathBuf::from(&dir)
            .join("caller-settings.json.tmp")
            .exists());
        assert!(Store::open("/tmp/elsewhere").is_err());
        assert!(Store::open("/data/x/tech.getsecretary.secretary/files/../../y").is_err());
    }

    #[test]
    fn a_corrupt_file_means_the_defaults() {
        let dir = app_dir("corrupt");
        fs::write(format!("{dir}/caller-settings.json"), b"{not json").unwrap();
        fs::write(format!("{dir}/caller-last.json"), vec![b' '; 20_000]).unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.settings(), Settings::default());
        assert_eq!(s.last(), None);
    }

    #[test]
    fn state_lines() {
        let s = store("state");
        let st = s.state(NOW);
        assert_eq!(st["lastLine"], "No call screened yet.");
        assert_eq!(st["settings"]["label"], true);
        assert_eq!(st["settings"]["reject"]["enabled"], true);
    }

    #[test]
    fn the_local_verdict_is_instant_and_quick_guess() {
        let s = store("local");
        let out = local(&s, "70405205", &Input::default(), "en", NOW);
        assert!(out.notify);
        assert_eq!(out.action, "allow");
        assert_eq!(out.source, "rules");
        // From the number alone (a Danish 70 company line) the phone claims nothing.
        assert_eq!(out.title, "Unknown caller");
        assert_eq!(out.sub, "+45 70 40 52 05");
        assert!(
            out.text
                .starts_with("Business · unsure whether to pick up · quick guess"),
            "{}",
            out.text
        );
        assert!(out.text.contains("Danish 70-number"));
        let da = local(&s, "70405205", &Input::default(), "da-DK", NOW);
        assert_eq!(da.title, "Ukendt opkald");
        assert!(da.text.contains("hurtigt gæt"));
        let it = local(&s, "+39 333 1234567", &Input::default(), "it", NOW);
        assert!(it.text.contains("stima rapida"), "{}", it.text);
        // Nothing was saved: only a screened call is "the last".
        assert_eq!(s.last(), None);
    }

    #[test]
    fn no_label_assumes_who_is_calling() {
        // Labels come from facts about the number only: never a shift, a delivery or a customer.
        let s = store("neutral");
        for number in [
            "+4522334455",
            "70405205",
            "+35799123456",
            "+447459508537",
            "+393331234567",
            "+4586123456",
            "80123456",
            "+4590123456",
        ] {
            for lang in ["en", "it", "da"] {
                let out = local(&s, number, &Input::default(), lang, NOW);
                let all = format!("{} {} {}", out.title, out.text, out.kind).to_lowercase();
                for word in [
                    "wolt", "deliver", "levering", "consegna", "customer", "kunde", "cliente",
                    "courier", "shift", "vagt", "turno", "order", "ordre", "ordine",
                ] {
                    assert!(!all.contains(word), "{number} {lang}: {all}");
                }
                assert_ne!(out.kind, "delivery");
            }
        }
    }

    #[test]
    fn the_server_verdict_replaces_the_local_one() {
        let s = store("screen");
        let fake = Fake::ok(server_answer(0.93, "jev"));
        let out = screen(&s, &fake, "70405205", &Input::default(), "en", NOW);
        assert_eq!(out.title, "Likely scam (0.93)");
        assert_eq!(out.source, "jev");
        assert_eq!(
            out.text,
            "Scam · probably not worth picking up\nDanish 70 number (company line); called 2 times before"
        );
        let sent = fake.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "/v1/callers/screen");
        assert_eq!(
            sent[0].1,
            json!({
                "numbers": ["+4570405205"],
                "lang": "en",
                "call": {"source": "android", "at": "2026-10-29T12:00:00Z"},
            })
        );
        let last = s.last().unwrap();
        assert_eq!(
            (
                last.label.as_str(),
                last.display.as_str(),
                last.source.as_str()
            ),
            ("Likely scam (0.93)", "+45 70 40 52 05", "jev")
        );
        assert_eq!(last.at_ms, NOW);
    }

    #[test]
    fn the_request_carries_the_number_the_language_and_the_call_only() {
        let s = store("request");
        let fake = Fake::ok(server_answer(0.5, "jev"));
        screen(&s, &fake, "+4570405205", &Input::default(), "da", NOW);
        let body = fake.sent.borrow()[0].1.clone();
        assert_eq!(
            body,
            json!({
                "numbers": ["+4570405205"],
                "lang": "da",
                "call": {"source": "android", "at": "2026-10-29T12:00:00Z"},
            })
        );
    }

    #[test]
    fn any_server_trouble_falls_back_to_the_rules() {
        let s = store("fallback");
        let bad: Vec<Fake> = vec![
            Fake::down(),
            Fake::ok(json!({})),
            Fake::ok(json!({"results": []})),
            // A different number than the one asked about.
            Fake::ok({
                let mut v = server_answer(0.99, "jev");
                v["results"][0]["number"] = json!("+4511111111");
                v
            }),
            // Probabilities out of range, or no label.
            Fake::ok(server_answer(7.0, "jev")),
            Fake::ok({
                let mut v = server_answer(0.9, "jev");
                v["results"][0]["label"] = json!("");
                v
            }),
        ];
        for fake in &bad {
            let out = screen(&s, fake, "70405205", &Input::default(), "en", NOW);
            assert_eq!(out.source, "rules");
            assert_eq!(out.action, "allow");
            assert_eq!(out.title, "Unknown caller");
            assert_eq!(fake.sent.borrow().len(), 1);
        }
        assert_eq!(s.last().unwrap().source, "rules");
    }

    fn rejects(s: &Store, answer: Value, input: &Input) -> Option<RejectOut> {
        screen(s, &Fake::ok(answer), "70405205", input, "en", NOW).reject
    }

    #[test]
    fn the_owners_rule_rejects_with_evidence() {
        let s = store("rule");
        let out = screen(
            &s,
            &Fake::ok(server_answer(0.93, "jev")),
            "70405205",
            &Input::default(),
            "en",
            NOW,
        );
        assert_eq!(out.action, "reject");
        let r = out.reject.unwrap();
        assert_eq!(r.number, "+4570405205");
        assert_eq!(r.spam, Some(0.93));
        assert_eq!(r.pick_up, Some(0.04));
        assert!(
            r.line.starts_with("Rejected likely scam +45 70 40 52 05"),
            "{}",
            r.line
        );
        // Either threshold is enough; neither means keep.
        let mut low_spam = server_answer(0.3, "jev");
        low_spam["results"][0]["pickUp"] = json!(0.1);
        assert_eq!(rejects(&s, low_spam, &Input::default()).unwrap().spam, None);
        let mut middle = server_answer(0.5, "jev");
        middle["results"][0]["pickUp"] = json!(0.5);
        assert_eq!(rejects(&s, middle, &Input::default()), None);
        // The rule off: nothing is rejected.
        s.patch_settings(&json!({"reject": {"enabled": false}}))
            .unwrap();
        assert_eq!(
            rejects(&s, server_answer(0.99, "jev"), &Input::default()),
            None
        );
    }

    #[test]
    fn every_guard_and_every_unknown_keeps_the_call() {
        let s = store("guards");
        let bad = server_answer(0.99, "jev");
        assert!(rejects(&s, bad.clone(), &Input::default()).is_some());
        for (name, input) in [
            (
                "called or answered before",
                Input {
                    called_or_answered: true,
                    ..Default::default()
                },
            ),
            (
                "emergency",
                Input {
                    emergency: true,
                    ..Default::default()
                },
            ),
            (
                "a check could not run",
                Input {
                    unchecked: true,
                    ..Default::default()
                },
            ),
            ("contact", Input::from_json(&json!({"contact": "Mamma"}))),
        ] {
            assert_eq!(rejects(&s, bad.clone(), &input), None, "{name}");
        }
        // The core's floor: emergency and short numbers are never rejected, whatever the phone says.
        for number in ["112", "999", "1813", "12345", "+4570405205x"] {
            let out = screen(
                &s,
                &Fake::ok(bad.clone()),
                number,
                &Input::default(),
                "en",
                NOW,
            );
            assert_eq!(out.action, "allow", "{number}");
        }
        // No evidence (the fallback rules, Jev unreachable): never.
        let down = screen(&s, &Fake::down(), "70405205", &Input::default(), "en", NOW);
        assert_eq!((down.action, down.reject), ("allow", None));
        // The owner's own allow list.
        s.allow("+4570405205").unwrap();
        assert_eq!(rejects(&s, bad, &Input::default()), None);
    }

    #[test]
    fn an_active_order_raises_the_bar_and_stays_on_the_phone() {
        let s = store("order");
        assert!(!s.order_active(NOW));
        s.set_order(NOW - 10 * 60_000, NOW).unwrap();
        assert!(s.order_active(NOW));
        assert!(
            !s.order_active(NOW + 40 * 60_000),
            "46 minutes later it is over"
        );
        assert!(
            s.set_order(NOW + 3_600_000, NOW).is_err(),
            "a link from the future is refused"
        );
        // During an order only 0.97 and up rejects.
        let fake = Fake::ok(server_answer(0.93, "jev"));
        let out = screen(&s, &fake, "70405205", &Input::default(), "en", NOW);
        assert_eq!(out.action, "allow");
        let out = screen(
            &s,
            &Fake::ok(server_answer(0.98, "jev")),
            "70405205",
            &Input::default(),
            "en",
            NOW,
        );
        assert_eq!(out.action, "reject");
        // After it is over the normal rule applies again.
        let out = screen(
            &s,
            &fake,
            "70405205",
            &Input::default(),
            "en",
            NOW + 50 * 60_000,
        );
        assert_eq!(out.action, "reject");
        // The signal is in no request.
        for (_, body) in fake.sent.borrow().iter() {
            assert!(!body.to_string().to_lowercase().contains("order"), "{body}");
        }
    }

    #[test]
    fn the_screening_waits_for_the_server_only_when_it_could_reject() {
        let s = store("wait");
        let n = "70405205";
        assert!(must_wait(&s, n, &Input::default(), NOW));
        for (name, input) in [
            (
                "called",
                Input {
                    called_or_answered: true,
                    ..Default::default()
                },
            ),
            (
                "emergency",
                Input {
                    emergency: true,
                    ..Default::default()
                },
            ),
            (
                "unchecked",
                Input {
                    unchecked: true,
                    ..Default::default()
                },
            ),
            ("contact", Input::from_json(&json!({"contact": "Mamma"}))),
        ] {
            assert!(!must_wait(&s, n, &input, NOW), "{name}");
        }
        assert!(!must_wait(&s, "", &Input::default(), NOW), "hidden number");
        assert!(
            !must_wait(&s, "112", &Input::default(), NOW),
            "emergency number"
        );
        s.allow("70405205").unwrap();
        assert!(!must_wait(&s, n, &Input::default(), NOW), "allowed");
        let off = store("wait3");
        off.patch_settings(&json!({"reject": {"enabled": false}}))
            .unwrap();
        assert!(!must_wait(&off, n, &Input::default(), NOW), "rule off");
        // With an order active it still waits (0.97 can reject).
        let order = store("wait4");
        order.set_order(NOW, NOW).unwrap();
        assert!(must_wait(&order, n, &Input::default(), NOW));
    }

    #[test]
    fn label_off_keeps_the_rule() {
        let s = store("acts");
        s.patch_settings(&json!({"label": false})).unwrap();
        let quiet = screen(
            &s,
            &Fake::ok(server_answer(0.95, "jev")),
            "70405205",
            &Input::default(),
            "en",
            NOW,
        );
        assert!(!quiet.notify);
        assert_eq!(quiet.action, "reject");
        // The instant local verdict never rejects.
        assert_eq!(
            local(&s, "70405205", &Input::default(), "en", NOW).action,
            "allow"
        );
    }

    #[test]
    fn hidden_numbers_and_contacts_stay_on_the_phone() {
        let s = store("private");
        let fake = Fake::ok(server_answer(0.99, "jev"));
        for hidden in ["", "  ", "anonymous", "1234"] {
            let out = screen(&s, &fake, hidden, &Input::default(), "en", NOW);
            assert!(!out.notify, "{hidden}");
            assert_eq!(out.action, "allow");
        }
        let contact = Input::from_json(&json!({"contact": "Mamma"}));
        let out = screen(&s, &fake, "+4570405205", &contact, "en", NOW);
        assert!(!out.notify);
        assert_eq!(out.action, "allow");
        assert_eq!(out.source, "contact");
        assert_eq!(out.title, "Contact: Mamma");
        assert!(fake.sent.borrow().is_empty());
        assert_eq!(s.last(), None);
    }

    #[test]
    fn server_text_is_cleaned() {
        let s = store("clean");
        let mut v = server_answer(0.5, "jev");
        v["results"][0]["label"] = json!("Bad\u{7}\nlabel ");
        v["results"][0]["source"] = json!("surprise");
        let out = screen(&s, &Fake::ok(v), "70405205", &Input::default(), "en", NOW);
        assert_eq!(out.title, "Badlabel");
        assert_eq!(out.source, "rules");
        let long = "x".repeat(1000);
        let mut v = server_answer(0.5, "jev");
        v["results"][0]["label"] = json!(long);
        let out = screen(&s, &Fake::ok(v), "70405205", &Input::default(), "en", NOW);
        assert_eq!(out.title.chars().count(), MAX_SHOWN_CHARS);
    }

    #[test]
    fn the_real_transport_keeps_its_checks_with_a_timeout() {
        use crate::net::Https;
        assert_eq!(
            Https::with_timeout("", SCREEN_TIMEOUT)
                .post(SCREEN_PATH, &json!({}))
                .unwrap_err(),
            "Sign in to Secretary first."
        );
        assert_eq!(
            Https::with_timeout("a=b", SCREEN_TIMEOUT)
                .post("/other", &json!({}))
                .unwrap_err(),
            "Invalid request."
        );
    }
}
