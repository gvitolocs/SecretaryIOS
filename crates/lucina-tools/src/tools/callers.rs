//! Screening unknown callers with Jev (`caller_screen`), on top of
//! `lucina_phone_core::caller`, plus the small store of numbers the phones
//! reported (earlier calls, last verdicts) that the iOS Call Directory reads.
//!
//! - [`screen`]: one Jev call per number (spam yes/no, who is calling, pick up
//!   yes/no) on a state enriched with what Lucina knows; Jev unreachable or
//!   cooling down → the deterministic rules, `source: "rules"`.
//! - [`CallerStore`]: `<data>/callers/callers.json` (0600, written atomically),
//!   at most [`MAX_NUMBERS_KEPT`] numbers with their last [`MAX_CALLS_KEPT`] calls.
//!
//! Read-only for the owner's phones: nothing here answers, rejects or places a call.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_jev::{Jev, Question};
use lucina_phone_core::caller::{
    self, Context, Courier, Known, LineType, Number, Probs, Verdict, KINDS, KIND_QUESTION,
    MAX_NUMBERS, PICK_UP_QUESTION, SPAM_QUESTION,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

pub const NAMES: &[&str] = &["caller_screen"];
/// Numbers kept in the store (the least recently active go first).
pub const MAX_NUMBERS_KEPT: usize = 5000;
/// Calls kept per number.
pub const MAX_CALLS_KEPT: usize = 50;
/// Characters of one number as given.
const MAX_RAW: usize = 40;
/// Characters of a contact name.
const MAX_CONTACT: usize = 80;

// ---------------------------------------------------------------- store

/// One reported incoming call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenCall {
    pub at: String,
    pub source: String,
    #[serde(default)]
    pub on_shift: bool,
    #[serde(default)]
    pub order_active: bool,
}

/// The last answer for a number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedVerdict {
    pub at: String,
    pub short: String,
    pub kind: String,
    pub kind_p: f64,
    pub spam: f64,
    pub pick_up: f64,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub calls: Vec<SeenCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<SavedVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_label: Option<String>,
    /// Last call or lookup (RFC 3339), for pruning.
    #[serde(default)]
    pub active: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    numbers: BTreeMap<String, Entry>,
}

/// The numbers the phones reported and the answers Lucina gave.
pub struct CallerStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl CallerStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    /// `<data_dir>/callers/callers.json`.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("callers").join("callers.json"))
    }

    fn read(&self) -> StoreFile {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, f: &StoreFile) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        out.write_all(&serde_json::to_vec(f).map_err(std::io::Error::other)?)?;
        out.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    fn update(&self, change: impl FnOnce(&mut StoreFile)) {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = self.read();
        change(&mut f);
        if f.numbers.len() > MAX_NUMBERS_KEPT {
            let mut by_age: Vec<(String, String)> = f
                .numbers
                .iter()
                .map(|(k, e)| (e.active.clone(), k.clone()))
                .collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(f.numbers.len() - MAX_NUMBERS_KEPT) {
                f.numbers.remove(&k);
            }
        }
        if let Err(e) = self.write(&f) {
            tracing::warn!(error = %e, "caller store not written");
        }
    }

    pub fn entry(&self, e164: &str) -> Option<Entry> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read().numbers.remove(e164)
    }

    /// What the store knows about a number, before this call is recorded.
    pub fn known(&self, e164: &str) -> Known {
        let Some(e) = self.entry(e164) else {
            return Known::default();
        };
        Known {
            previous_calls: e.calls.len() as u32,
            last_call: e.calls.last().map(|c| c.at.clone()),
            called_during_shift: e.calls.iter().any(|c| c.on_shift),
            owner_label: e.owner_label,
            ..Known::default()
        }
    }

    pub fn record_call(&self, e164: &str, call: SeenCall) {
        self.update(|f| {
            let e = f.numbers.entry(e164.to_owned()).or_default();
            e.active = call.at.clone();
            e.calls.push(call);
            if e.calls.len() > MAX_CALLS_KEPT {
                let drop = e.calls.len() - MAX_CALLS_KEPT;
                e.calls.drain(..drop);
            }
        });
    }

    /// Keeps the answer (not for contacts: iOS shows their own name).
    pub fn save_verdict(&self, v: &Verdict, at: &str) {
        if v.source == "contact" || v.line == LineType::Invalid {
            return;
        }
        self.update(|f| {
            let e = f.numbers.entry(v.number.clone()).or_default();
            e.active = e.active.clone().max(at.to_owned());
            e.verdict = Some(SavedVerdict {
                at: at.to_owned(),
                short: e.owner_label.clone().unwrap_or_else(|| v.short.clone()),
                kind: v.kind.clone(),
                kind_p: v.kind_p,
                spam: v.spam,
                pick_up: v.pick_up,
                source: v.source.clone(),
            });
        });
    }

    /// Call Directory entries: digits with country code, strictly ascending.
    pub fn directory(&self) -> Vec<(i64, String)> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: BTreeMap<i64, String> = BTreeMap::new();
        for (num, e) in self.read().numbers {
            let label = match (&e.owner_label, &e.verdict) {
                (Some(l), _) => l.clone(),
                (None, Some(v)) => v.short.clone(),
                (None, None) => continue,
            };
            if let Some(d) = caller::parse(&num).directory_number() {
                out.insert(d, label);
            }
        }
        out.into_iter().collect()
    }
}

// ---------------------------------------------------------------- screening

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestContext {
    pub courier: Option<Courier>,
    /// The contact's name, when the phone found the number in his contacts.
    pub contact: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestCall {
    /// `android` or `ios`.
    pub source: String,
    pub at: Option<DateTime<Utc>>,
}

/// `POST /callers/screen`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenRequest {
    pub numbers: Vec<String>,
    pub lang: Option<String>,
    pub context: Option<RequestContext>,
    /// Record this as an incoming call ringing now.
    pub call: Option<RequestCall>,
}

impl ScreenRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.numbers.is_empty() || self.numbers.len() > MAX_NUMBERS {
            return Err(format!("numbers must hold 1-{MAX_NUMBERS} entries"));
        }
        if self.numbers.iter().any(|n| n.chars().count() > MAX_RAW) {
            return Err(format!("each number must be at most {MAX_RAW} characters"));
        }
        if let Some(l) = &self.lang {
            if !matches!(l.as_str(), "en" | "it" | "da") {
                return Err("lang must be en, it or da".into());
            }
        }
        if let Some(c) = self.context.as_ref().and_then(|c| c.contact.as_ref()) {
            if self.numbers.len() != 1 || c.trim().is_empty() || c.chars().count() > MAX_CONTACT {
                return Err(format!(
                    "context.contact needs exactly one number and 1-{MAX_CONTACT} characters"
                ));
            }
        }
        if let Some(call) = &self.call {
            if self.numbers.len() != 1 {
                return Err("call needs exactly one number".into());
            }
            if !matches!(call.source.as_str(), "android" | "ios") {
                return Err("call.source must be android or ios".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenResponse {
    pub lang: &'static str,
    pub results: Vec<Verdict>,
    pub table: String,
}

fn questions() -> [(&'static str, Question); 3] {
    [
        ("spam", Question::noul(SPAM_QUESTION)),
        ("kind", Question::choice(KIND_QUESTION, KINDS)),
        ("pick_up", Question::noul(PICK_UP_QUESTION)),
    ]
}

/// Jev's probabilities, or `None` (then the rules decide).
async fn ask_jev(jev: &Jev, n: &Number, ctx: &Context) -> Option<Probs> {
    if !jev.available() {
        return None;
    }
    let d = match jev
        .decide("caller", caller::state(n, ctx), &questions())
        .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::info!(target: "lucina_jev::caller", error = %e, "jev caller screen failed; rules");
            return None;
        }
    };
    let kinds: BTreeMap<String, f64> = KINDS
        .iter()
        .map(|(k, _)| ((*k).to_owned(), d.probability("kind", k).unwrap_or(0.0)))
        .collect();
    let probs = Probs {
        spam: d.yes("spam")?,
        kinds,
        pick_up: d.yes("pick_up")?,
    };
    tracing::info!(target: "lucina_jev::caller", ms = d.elapsed_ms, kind = probs.top().0, "jev caller screen");
    Some(probs)
}

/// Screens every number of `req`. `mentions` returns the number's mentions in
/// the owner's messages (empty when there are none or nobody looks).
pub async fn screen(
    jev: &Jev,
    store: Option<&CallerStore>,
    req: &ScreenRequest,
    now: DateTime<Utc>,
    tz: Tz,
    mentions: &(dyn Fn(&Number) -> Vec<String> + Sync),
) -> Result<ScreenResponse, String> {
    req.validate()?;
    let lang = caller::lang(req.lang.as_deref().unwrap_or("en"));
    let rc = req.context.clone().unwrap_or_default();
    let (courier, contact) = (rc.courier, rc.contact.map(|c| c.trim().to_owned()));
    let local_time = now.with_timezone(&tz).format("%A %H:%M").to_string();
    let at = req
        .call
        .as_ref()
        .and_then(|c| c.at)
        .unwrap_or(now)
        .to_rfc3339();
    let one = |raw: &String| {
        let n = caller::parse(raw);
        let mut known = match (store, n.valid()) {
            (Some(s), true) => s.known(&n.e164),
            _ => Known::default(),
        };
        known.contact = contact.clone();
        if n.valid() {
            known.mentions = mentions(&n);
        }
        let ctx = Context {
            courier,
            known,
            local_time: Some(local_time.clone()),
            ringing: req.call.is_some(),
        };
        let at = at.clone();
        async move {
            let jev_probs = if n.valid() && ctx.known.contact.is_none() {
                ask_jev(jev, &n, &ctx).await
            } else {
                None
            };
            let (probs, source) = match jev_probs {
                Some(p) => (p, "jev"),
                None => (caller::rules(&n, &ctx), "rules"),
            };
            let v = caller::verdict(&n, &ctx, &probs, source, lang);
            if let (Some(s), true) = (store, n.valid()) {
                if let Some(call) = &req.call {
                    let c = courier.unwrap_or_default();
                    s.record_call(
                        &n.e164,
                        SeenCall {
                            at: at.clone(),
                            source: call.source.clone(),
                            on_shift: c.on_shift,
                            order_active: c.order_active,
                        },
                    );
                }
                s.save_verdict(&v, &at);
            }
            v
        }
    };
    let results = futures_util::future::join_all(req.numbers.iter().map(one)).await;
    let table = caller::table(&results, lang);
    Ok(ScreenResponse {
        lang,
        results,
        table,
    })
}

// ---------------------------------------------------------------- the tool

#[derive(Deserialize, JsonSchema)]
struct ScreenArgs {
    /// Phone numbers as the owner wrote them (one or a pasted list, up to 20).
    numbers: Vec<String>,
    /// Reply language: en, it or da.
    lang: Option<String>,
}

pub struct CallerScreenTool;

#[async_trait]
impl Tool for CallerScreenTool {
    fn name(&self) -> &'static str {
        "caller_screen"
    }
    fn description(&self) -> &'static str {
        "Screen phone numbers that called the owner (\"who called me from …\"): for each number Jev gives P(spam), who is likely calling (person, business, Wolt delivery, telemarketing, scam, unknown) and P(pick up), from the number's country and line type and what Lucina knows (earlier calls, his Wolt shift). Not a spam database: say so. Show the returned table as is."
    }
    fn parameters(&self) -> Value {
        schema_for::<ScreenArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: ScreenArgs = super::args(args)?;
        let req = ScreenRequest {
            numbers: a.numbers,
            lang: a.lang.map(|l| caller::lang(&l).to_owned()),
            ..ScreenRequest::default()
        };
        let r = screen(
            lucina_jev::shared(),
            ctx.env.callers.as_deref(),
            &req,
            ctx.now,
            ctx.env.time_zone,
            &|_| Vec::new(),
        )
        .await
        .map_err(ToolError::new)?;
        serde_json::to_value(r).map_err(|e| ToolError::new(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_jev::Config;
    use serde_json::json;

    fn off() -> Jev {
        Jev::new(Config::from_lookup(&|_: &str| None))
    }

    fn req(numbers: &[&str]) -> ScreenRequest {
        ScreenRequest {
            numbers: numbers.iter().map(|s| s.to_string()).collect(),
            ..ScreenRequest::default()
        }
    }

    #[test]
    fn requests_are_validated() {
        assert!(req(&[]).validate().is_err());
        assert!(req(&["1"; 21]).validate().is_err());
        assert!(req(&["+45 70 40 52 05"]).validate().is_ok());
        let mut r = req(&["1", "2"]);
        r.call = Some(RequestCall {
            source: "android".into(),
            at: None,
        });
        assert!(r.validate().is_err());
        let mut r = req(&["1"]);
        r.call = Some(RequestCall {
            source: "fax".into(),
            at: None,
        });
        assert!(r.validate().is_err());
        let mut r = req(&["1"]);
        r.lang = Some("fr".into());
        assert!(r.validate().is_err());
        let r: Result<ScreenRequest, _> =
            serde_json::from_value(json!({"numbers": ["1"], "extra": 1}));
        assert!(r.is_err());
        let r: ScreenRequest = serde_json::from_value(json!({
            "numbers": ["+45 20 68 27 58"], "lang": "da",
            "context": {"courier": {"onShift": true, "orderActive": true, "orderAgeMin": 7}},
            "call": {"source": "android", "at": "2026-10-29T12:00:00Z"}
        }))
        .unwrap();
        assert!(r.validate().is_ok());
    }

    #[tokio::test]
    async fn without_jev_the_rules_answer_and_the_store_learns() {
        let dir = tempfile::tempdir().unwrap();
        let store = CallerStore::in_data_dir(dir.path());
        let now: DateTime<Utc> = "2026-10-29T12:00:00Z".parse().unwrap();
        let mut r = req(&["+45 70 40 52 05"]);
        r.call = Some(RequestCall {
            source: "android".into(),
            at: None,
        });
        r.context = Some(RequestContext {
            courier: Some(Courier {
                on_shift: true,
                order_active: false,
                order_age_min: None,
            }),
            contact: None,
        });
        let out = screen(
            &off(),
            Some(&store),
            &r,
            now,
            chrono_tz::Europe::Copenhagen,
            &|_| Vec::new(),
        )
        .await
        .unwrap();
        let v = &out.results[0];
        assert_eq!(
            (v.source.as_str(), v.kind.as_str()),
            ("rules", "telemarketing")
        );
        assert_eq!(v.known.previous_calls, 0);
        assert!(out.table.contains("Source: basic rules"));

        // The second call knows the first.
        let out = screen(
            &off(),
            Some(&store),
            &r,
            now,
            chrono_tz::Europe::Copenhagen,
            &|_| Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].known.previous_calls, 1);
        assert!(out.results[0].known.called_during_shift);
        let e = store.entry("+4570405205").unwrap();
        assert_eq!(e.calls.len(), 2);
        assert_eq!(
            store.directory(),
            vec![(4570405205, "Telemarketing 60%".to_owned())]
        );

        // A lookup saves the answer but is no call; contacts never reach the directory.
        let mut l = req(&["20682758"]);
        l.context = Some(RequestContext {
            courier: None,
            contact: Some("Jerry's".into()),
        });
        let out = screen(
            &off(),
            Some(&store),
            &l,
            now,
            chrono_tz::Europe::Copenhagen,
            &|n| vec![format!("seen {}", n.e164)],
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].source, "contact");
        assert_eq!(
            out.results[0].known.mentions,
            vec!["seen +4520682758".to_owned()]
        );
        assert!(store.entry("+4520682758").is_none());
        assert_eq!(store.directory().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("callers/callers.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn invalid_numbers_are_answered_not_failed() {
        let now = Utc::now();
        let out = screen(
            &off(),
            None,
            &req(&["hello", "+357 94 095733"]),
            now,
            chrono_tz::UTC,
            &|_| Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].line, LineType::Invalid);
        assert_eq!(out.results[1].kind, "scam");
        assert!(out.table.contains("Not a phone number"));
    }

    #[test]
    fn the_store_prunes_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let store = CallerStore::in_data_dir(dir.path());
        for i in 0..(MAX_CALLS_KEPT + 3) {
            store.record_call(
                "+4570405205",
                SeenCall {
                    at: format!("2026-10-29T12:{:02}:00Z", i % 60),
                    source: "android".into(),
                    on_shift: false,
                    order_active: false,
                },
            );
        }
        assert_eq!(
            store.entry("+4570405205").unwrap().calls.len(),
            MAX_CALLS_KEPT
        );
        assert_eq!(
            store.known("+4570405205").previous_calls as usize,
            MAX_CALLS_KEPT
        );
        assert_eq!(store.known("+4599999999"), Known::default());
    }
}
