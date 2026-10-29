//! Caller screening for the Android `CallScreeningService`: what to show for
//! an incoming number, and whether the owner's own settings say to silence or
//! reject it. Nothing here answers, places or ends a call.
//!
//! | Flow | What happens |
//! |---|---|
//! | [`local`] | No network. The shared core's fallback rules ([`lucina_phone_core::caller::rules`]) give the first, instant verdict. |
//! | [`screen`] | `POST /v1/callers/screen` with `call: {source: "android"}` and the courier state, about [`SCREEN_TIMEOUT`] in total. Any error, bad answer or a different number falls back to [`local`] with `source: "rules"`. |
//! | [`Store`] | The settings, the last courier state from Lucina Maps and the last screened call, as small files in the app's files directory (mode 0600, written atomically). |
//!
//! Settings: the label (heads-up notification) is on by default; "silence
//! likely scams" and "reject likely scams" are off by default and act only on
//! a Jev verdict ([`Verdict`]'s `source == "jev"`) whose spam probability
//! reaches the threshold (0.90). The fallback rules never act, and neither
//! does a contact. If both toggles are on, reject wins.
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

use lucina_phone_core::caller::{self, Context, Courier, Known, Number, Verdict};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{net::Backend, Result};

/// Total time allowed for the screening request.
pub const SCREEN_TIMEOUT: Duration = Duration::from_millis(1200);
/// The server path.
pub const SCREEN_PATH: &str = "/v1/callers/screen";
/// An order link from Lucina Maps counts as active for this long.
pub const ORDER_ACTIVE_MS: u64 = 45 * 60_000;
/// A courier state older than this is ignored (Lucina Maps stopped telling).
pub const COURIER_STALE_MS: u64 = 16 * 3_600_000;
/// The default spam probability from which silence/reject may act.
pub const DEFAULT_THRESHOLD: f64 = 0.90;
/// Largest settings, courier or last-call file read.
const MAX_FILE_BYTES: u64 = 16 * 1024;
/// Longest label, text or hint passed to the notification, in characters.
const MAX_SHOWN_CHARS: usize = 240;

/// Serialises every read-modify-write of the files within the process.
static LOCK: Mutex<()> = Mutex::new(());

// ------------------------------------------------------------------ files

/// The owner's switches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Show the heads-up notification with the verdict.
    #[serde(default = "yes")]
    pub label: bool,
    /// Silence the ringer of a likely scam (the call still shows as missed).
    #[serde(default)]
    pub silence: bool,
    /// Reject a likely scam (the call log keeps it).
    #[serde(default)]
    pub reject: bool,
    /// Spam probability from which silence/reject act.
    #[serde(default = "threshold")]
    pub threshold: f64,
}

fn yes() -> bool {
    true
}

fn threshold() -> f64 {
    DEFAULT_THRESHOLD
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            label: true,
            silence: false,
            reject: false,
            threshold: DEFAULT_THRESHOLD,
        }
    }
}

impl Settings {
    /// Whether the service has to wait for the verdict before it answers.
    pub fn acts(&self) -> bool {
        self.silence || self.reject
    }

    /// Apply `{"label"?, "silence"?, "reject"?, "threshold"?}`.
    fn patch(&mut self, v: &Value) -> Result<()> {
        let obj = v.as_object().ok_or("Invalid settings.")?;
        for (k, val) in obj {
            match (k.as_str(), val) {
                ("label", Value::Bool(b)) => self.label = *b,
                ("silence", Value::Bool(b)) => self.silence = *b,
                ("reject", Value::Bool(b)) => self.reject = *b,
                ("threshold", Value::Number(n)) => {
                    let t = n.as_f64().unwrap_or(f64::NAN);
                    if !(0.5..=1.0).contains(&t) {
                        return Err("The threshold must be between 0.5 and 1.".into());
                    }
                    self.threshold = t;
                }
                _ => return Err("Invalid settings.".into()),
            }
        }
        Ok(())
    }
}

/// The last broadcast from Lucina Maps.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CourierState {
    pub on_shift: bool,
    /// The last Wolt link (ms since the epoch), 0 = none.
    pub order_at_ms: u64,
    /// When this phone received it.
    pub received_at_ms: u64,
}

impl CourierState {
    /// The courier context for a call at `now`, or `None` when the state is stale.
    pub fn at(&self, now: u64) -> Option<Courier> {
        if now.saturating_sub(self.received_at_ms) > COURIER_STALE_MS {
            return None;
        }
        let active = self.order_at_ms > 0
            && self.order_at_ms <= now.saturating_add(60_000)
            && now.saturating_sub(self.order_at_ms) <= ORDER_ACTIVE_MS;
        Some(Courier {
            on_shift: self.on_shift,
            order_active: active,
            order_age_min: active.then(|| (now.saturating_sub(self.order_at_ms) / 60_000) as u32),
        })
    }
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
    dir: PathBuf,
}

impl Store {
    /// Open the store in `files_dir`, which must be the app's own files directory.
    pub fn open(files_dir: &str) -> Result<Self> {
        Ok(Self {
            dir: crate::journal::app_files(files_dir)?,
        })
    }

    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        let path = self.dir.join(name);
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
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
        if !(0.5..=1.0).contains(&s.threshold) {
            s.threshold = DEFAULT_THRESHOLD;
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

    /// The last courier state, if Lucina Maps ever sent one.
    pub fn courier(&self) -> Option<CourierState> {
        self.read("caller-courier.json")
    }

    /// Keep what Lucina Maps broadcast: `{"onShift": bool, "orderAtMs": ms}`.
    pub fn set_courier(&self, v: &Value, now: u64) -> Result<CourierState> {
        let on_shift = match &v["onShift"] {
            Value::Bool(b) => *b,
            Value::Null => false,
            _ => return Err("Invalid courier state.".into()),
        };
        let order_at_ms = match &v["orderAtMs"] {
            Value::Null => 0,
            n => n.as_u64().ok_or("Invalid courier state.")?,
        };
        // A link from the future would stay "active" for ever.
        if order_at_ms > now.saturating_add(10 * 60_000) {
            return Err("Invalid courier state.".into());
        }
        let state = CourierState {
            on_shift,
            order_at_ms,
            received_at_ms: now,
        };
        let _lock = LOCK.lock().map_err(|_| "Screening busy.")?;
        self.write("caller-courier.json", &state)?;
        Ok(state)
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

    /// Settings, courier state and last call, with the lines the app shows.
    pub fn state(&self, now: u64) -> Value {
        let settings = self.settings();
        let courier = self.courier();
        let last = self.last();
        json!({
            "settings": settings,
            "courier": courier.map(|c| json!({
                "onShift": c.on_shift,
                "orderAtMs": c.order_at_ms,
                "receivedAtMs": c.received_at_ms,
                "fresh": c.at(now).is_some(),
            })),
            "courierLine": courier_line(courier, now),
            "last": last,
            "lastLine": last_line(last.as_ref(), now),
        })
    }
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

fn courier_line(c: Option<CourierState>, now: u64) -> String {
    let Some(c) = c else {
        return "Nothing received from Lucina Maps yet.".into();
    };
    let shift = if c.on_shift { "On shift" } else { "Off shift" };
    let order = match c.at(now) {
        None => {
            return format!(
                "{shift}, but the report is old ({}).",
                ago(c.received_at_ms, now)
            )
        }
        Some(courier) => match courier.order_age_min {
            Some(0) => "order link just now".to_owned(),
            Some(m) => format!("order link {m} min ago"),
            None => "no recent order link".to_owned(),
        },
    };
    format!(
        "{shift}, {order} (reported {}).",
        ago(c.received_at_ms, now)
    )
}

fn last_line(last: Option<&Last>, now: u64) -> String {
    match last {
        None => "No call screened yet.".into(),
        Some(l) => format!("{} · {} · {}", l.label, l.display, ago(l.at_ms, now)),
    }
}

// ----------------------------------------------------------------- verdicts

/// What the shell may be told to do with the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Silence,
    Reject,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Silence => "silence",
            Self::Reject => "reject",
        }
    }
}

/// Silence or reject only on the owner's switches, only for Jev's own
/// verdict (never the fallback rules) and only from the threshold.
pub fn decide(settings: &Settings, source: &str, spam: f64) -> Action {
    if source != "jev" || !spam.is_finite() || spam < settings.threshold {
        return Action::Allow;
    }
    if settings.reject {
        Action::Reject
    } else if settings.silence {
        Action::Silence
    } else {
        Action::Allow
    }
}

/// What is shown about one number, from the server or from the rules.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    label: String,
    kind: String,
    spam: f64,
    pick_up: f64,
    source: String,
    display: String,
    number: String,
    reasons: Vec<String>,
}

impl Shown {
    fn from_verdict(v: &Verdict) -> Self {
        Self {
            label: v.label.clone(),
            kind: v.kind.clone(),
            spam: v.spam,
            pick_up: v.pick_up,
            source: v.source.clone(),
            display: v.display.clone(),
            number: v.number.clone(),
            reasons: v.reasons.clone(),
        }
    }

    /// The first result of a server answer for `n`; `None` if it is not
    /// what was asked (another number, no label, probabilities out of range).
    fn from_response(v: &Value, n: &Number) -> Option<Self> {
        let r = v["results"].as_array()?;
        if r.len() != 1 {
            return None;
        }
        let r = &r[0];
        if r["number"].as_str()? != n.e164 {
            return None;
        }
        let label = clean(r["label"].as_str()?, MAX_SHOWN_CHARS);
        let p = |k: &str| r[k].as_f64().filter(|x| (0.0..=1.0).contains(x));
        let source = match r["source"].as_str().unwrap_or("jev") {
            s @ ("jev" | "rules" | "contact") => s.to_owned(),
            _ => "rules".to_owned(),
        };
        if label.is_empty() {
            return None;
        }
        Some(Self {
            label,
            kind: r["kind"].as_str().unwrap_or("unknown").to_owned(),
            spam: p("spam")?,
            pick_up: p("pickUp").unwrap_or(0.5),
            source,
            display: r["display"]
                .as_str()
                .map_or_else(|| n.display(), |s| clean(s, 40)),
            number: n.e164.clone(),
            reasons: r["reasons"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .take(6)
                        .map(|s| clean(s, 80))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
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
    /// `allow`, `silence` or `reject`.
    pub action: &'static str,
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

/// What the app tells about a caller besides the courier state.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// The contact's name, when the number is in his contacts.
    pub contact: Option<String>,
}

impl Input {
    /// `{"contact"?: string}`.
    pub fn from_json(v: &Value) -> Self {
        Self {
            contact: v["contact"]
                .as_str()
                .map(|s| clean(s, 80))
                .filter(|s| !s.is_empty()),
        }
    }
}

fn context(store: &Store, input: &Input, now: u64) -> Context {
    Context {
        courier: store.courier().and_then(|c| c.at(now)),
        known: Known {
            contact: input.contact.clone(),
            ..Known::default()
        },
        local_time: None,
        ringing: true,
    }
}

fn local_shown(n: &Number, ctx: &Context, lang: &str) -> Shown {
    let p = caller::rules(n, ctx);
    Shown::from_verdict(&caller::verdict(n, ctx, &p, "rules", lang))
}

/// The instant verdict, without the network.
pub fn local(store: &Store, number: &str, input: &Input, lang: &str, now: u64) -> Screen {
    let lang = caller::lang(lang);
    let n = caller::parse(number);
    let ctx = context(store, input, now);
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
    let ctx = context(store, input, now);
    let fallback = local_shown(&n, &ctx, lang);
    let settings = store.settings();
    // Nothing leaves the phone for a hidden number or one of his contacts.
    if !n.valid() || input.contact.is_some() {
        return present(&n, &fallback, &settings, input, lang);
    }
    let shown = backend
        .post(SCREEN_PATH, &request(&n, &ctx, lang, now))
        .ok()
        .and_then(|v| Shown::from_response(&v, &n))
        .unwrap_or(fallback);
    let out = present(&n, &shown, &settings, input, lang);
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

/// The request body of `POST /v1/callers/screen`.
fn request(n: &Number, ctx: &Context, lang: &str, now: u64) -> Value {
    let mut context = json!({});
    if let Some(c) = ctx.courier {
        context["courier"] = serde_json::to_value(c).unwrap_or(Value::Null);
    }
    json!({
        "numbers": [n.e164],
        "lang": lang,
        "context": context,
        "call": {"source": "android", "at": rfc3339(now)},
    })
}

fn present(n: &Number, s: &Shown, settings: &Settings, input: &Input, lang: &str) -> Screen {
    let action = if input.contact.is_some() {
        Action::Allow
    } else {
        decide(settings, &s.source, s.spam)
    };
    Screen {
        notify: settings.label && n.valid() && input.contact.is_none(),
        action: action.as_str(),
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
    fn settings_default_to_label_only_and_persist() {
        let s = store("settings");
        assert_eq!(s.settings(), Settings::default());
        assert!(s.settings().label && !s.settings().silence && !s.settings().reject);
        assert_eq!(s.settings().threshold, 0.90);
        let on = s.patch_settings(&json!({"silence": true})).unwrap();
        assert!(on.silence && on.label && !on.reject);
        assert_eq!(s.settings(), on);
        assert!(s.patch_settings(&json!({"reject": "yes"})).is_err());
        assert!(s.patch_settings(&json!({"nope": true})).is_err());
        assert!(s.patch_settings(&json!({"threshold": 0.2})).is_err());
        assert!(s.patch_settings(&json!({"threshold": 0.8})).is_ok());
        assert_eq!(s.settings().threshold, 0.8);
        // A failed patch changes nothing.
        assert!(s
            .patch_settings(&json!({"label": false, "reject": 3}))
            .is_err());
        assert!(s.settings().label);
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
        fs::write(format!("{dir}/caller-courier.json"), vec![b' '; 20_000]).unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.settings(), Settings::default());
        assert_eq!(s.courier(), None);
    }

    #[test]
    fn courier_state_windows() {
        let s = store("courier");
        assert_eq!(s.courier(), None);
        let order = NOW - 7 * 60_000;
        s.set_courier(&json!({"onShift": true, "orderAtMs": order}), NOW)
            .unwrap();
        let c = s.courier().unwrap().at(NOW).unwrap();
        assert_eq!(
            c,
            Courier {
                on_shift: true,
                order_active: true,
                order_age_min: Some(7)
            }
        );
        // 46 minutes later the order is no longer active.
        let later = s.courier().unwrap().at(NOW + 39 * 60_000).unwrap();
        assert!(later.on_shift && !later.order_active && later.order_age_min.is_none());
        // Stopped recording, no link yet.
        s.set_courier(&json!({"onShift": false, "orderAtMs": 0}), NOW)
            .unwrap();
        let c = s.courier().unwrap().at(NOW).unwrap();
        assert!(!c.on_shift && !c.order_active);
        // Old reports are ignored.
        assert_eq!(s.courier().unwrap().at(NOW + COURIER_STALE_MS + 1), None);
        // Missing fields are false/0; wrong types and future links are refused.
        assert!(s.set_courier(&json!({}), NOW).is_ok());
        assert!(s.set_courier(&json!({"onShift": "yes"}), NOW).is_err());
        assert!(s.set_courier(&json!({"orderAtMs": -5}), NOW).is_err());
        assert!(s
            .set_courier(&json!({"orderAtMs": NOW + 3_600_000}), NOW)
            .is_err());
    }

    #[test]
    fn state_lines() {
        let s = store("state");
        let st = s.state(NOW);
        assert_eq!(st["courierLine"], "Nothing received from Lucina Maps yet.");
        assert_eq!(st["lastLine"], "No call screened yet.");
        assert_eq!(st["settings"]["label"], true);
        assert_eq!(st["settings"]["silence"], false);
        s.set_courier(
            &json!({"onShift": true, "orderAtMs": NOW - 120_000}),
            NOW - 60_000,
        )
        .unwrap();
        let st = s.state(NOW);
        assert_eq!(
            st["courierLine"],
            "On shift, order link 2 min ago (reported 1 min ago)."
        );
        assert_eq!(st["courier"]["fresh"], true);
    }

    #[test]
    fn the_local_verdict_is_instant_and_quick_guess() {
        let s = store("local");
        let out = local(&s, "70405205", &Input::default(), "en", NOW);
        assert!(out.notify);
        assert_eq!(out.action, "allow");
        assert_eq!(out.source, "rules");
        assert!(
            out.title.starts_with("Likely telemarketing"),
            "{}",
            out.title
        );
        assert_eq!(out.sub, "+45 70 40 52 05");
        assert!(out
            .text
            .starts_with("Telemarketing · probably not worth picking up · quick guess"));
        assert!(out.text.contains("Danish 70-number"));
        let da = local(&s, "70405205", &Input::default(), "da-DK", NOW);
        assert!(
            da.title.starts_with("Sandsynligvis telemarketing"),
            "{}",
            da.title
        );
        assert!(da.text.contains("hurtigt gæt"));
        let it = local(&s, "+39 333 1234567", &Input::default(), "it", NOW);
        assert!(it.text.contains("stima rapida"), "{}", it.text);
        // Nothing was saved: only a screened call is "the last".
        assert_eq!(s.last(), None);
    }

    #[test]
    fn the_courier_state_changes_the_rules_verdict() {
        let s = store("ctx");
        let plain = local(&s, "+4522334455", &Input::default(), "en", NOW);
        assert!(!plain.title.contains("Wolt"));
        s.set_courier(&json!({"onShift": true, "orderAtMs": NOW - 60_000}), NOW)
            .unwrap();
        let on = local(&s, "+4522334455", &Input::default(), "en", NOW);
        assert_eq!(on.title, "Likely Wolt customer");
        assert!(on.text.contains("order in progress"), "{}", on.text);
        assert!(on.pick_up > 0.5);
    }

    #[test]
    fn the_server_verdict_replaces_the_local_one() {
        let s = store("screen");
        s.set_courier(
            &json!({"onShift": true, "orderAtMs": NOW - 7 * 60_000}),
            NOW,
        )
        .unwrap();
        let fake = Fake::ok(server_answer(0.93, "jev"));
        let out = screen(&s, &fake, "70405205", &Input::default(), "en", NOW);
        assert_eq!(out.title, "Likely scam (0.93)");
        assert_eq!(out.source, "jev");
        assert_eq!(out.action, "allow");
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
                "context": {"courier": {"onShift": true, "orderActive": true, "orderAgeMin": 7}},
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
    fn without_courier_state_the_request_has_no_courier() {
        let s = store("nocourier");
        let fake = Fake::ok(server_answer(0.5, "jev"));
        screen(&s, &fake, "+4570405205", &Input::default(), "da", NOW);
        let body = fake.sent.borrow()[0].1.clone();
        assert_eq!(body["context"], json!({}));
        assert_eq!(body["lang"], "da");
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
            assert!(out.title.starts_with("Likely telemarketing"));
            assert_eq!(fake.sent.borrow().len(), 1);
        }
        assert_eq!(s.last().unwrap().source, "rules");
    }

    #[test]
    fn silence_and_reject_need_the_owners_switch_jev_and_the_threshold() {
        let off = Settings::default();
        assert_eq!(decide(&off, "jev", 0.99), Action::Allow);
        let silence = Settings {
            silence: true,
            ..off
        };
        let reject = Settings {
            reject: true,
            ..off
        };
        let both = Settings {
            silence: true,
            reject: true,
            ..off
        };
        assert_eq!(decide(&silence, "jev", 0.93), Action::Silence);
        assert_eq!(decide(&silence, "jev", 0.90), Action::Silence);
        assert_eq!(decide(&silence, "jev", 0.89), Action::Allow);
        assert_eq!(decide(&reject, "jev", 0.93), Action::Reject);
        assert_eq!(decide(&both, "jev", 0.93), Action::Reject);
        // The fallback rules and contacts never act, whatever the number.
        assert_eq!(decide(&both, "rules", 1.0), Action::Allow);
        assert_eq!(decide(&both, "contact", 1.0), Action::Allow);
        assert_eq!(decide(&both, "jev", f64::NAN), Action::Allow);
    }

    #[test]
    fn a_screen_follows_the_settings() {
        let s = store("acts");
        s.patch_settings(&json!({"silence": true})).unwrap();
        let out = screen(
            &s,
            &Fake::ok(server_answer(0.93, "jev")),
            "70405205",
            &Input::default(),
            "en",
            NOW,
        );
        assert_eq!(out.action, "silence");
        let low = screen(
            &s,
            &Fake::ok(server_answer(0.6, "jev")),
            "70405205",
            &Input::default(),
            "en",
            NOW,
        );
        assert_eq!(low.action, "allow");
        // Jev down: the rules guess (0.65) is far below the threshold anyway, and never acts.
        let down = screen(&s, &Fake::down(), "70405205", &Input::default(), "en", NOW);
        assert_eq!(down.action, "allow");
        // The instant local verdict never silences.
        assert_eq!(
            local(&s, "70405205", &Input::default(), "en", NOW).action,
            "allow"
        );
        // Label off: no notification, the action still follows.
        s.patch_settings(&json!({"label": false, "reject": true}))
            .unwrap();
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
        s.patch_settings(&json!({"silence": true, "reject": true}))
            .unwrap();
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
