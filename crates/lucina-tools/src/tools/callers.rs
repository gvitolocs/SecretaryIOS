//! Screening unknown callers with Jev (`caller_screen`), on top of
//! `lucina_phone_core::caller`, plus the small store of numbers the phones
//! reported (earlier calls, last verdicts) that the iOS Call Directory reads.
//!
//! - [`screen`]: first the public caller-ID page (180.dk, cached), then one
//!   Jev call per number (spam yes/no, who is calling, pick up yes/no) on facts
//!   about the number only; Jev unreachable or cooling down → the
//!   deterministic rules, `source: "rules"`.
//! - [`CallerStore`]: `<data>/callers/callers.json` (0600, written atomically),
//!   at most [`MAX_NUMBERS_KEPT`] numbers with their last [`MAX_CALLS_KEPT`] calls.
//!
//! Read-only for the owner's phones: nothing here answers, rejects or places a call.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use lucina_jev::{Jev, Question};
use lucina_phone_core::caller::{
    self, Context, Courier, Known, LineType, Number, Probs, PublicReport, RejectSettings, Verdict,
    KINDS, KIND_QUESTION, MAX_NUMBERS, PICK_UP_QUESTION, SPAM_QUESTION,
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
}

/// A public caller-ID page as read, with when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedPublic {
    pub at: String,
    pub report: PublicReport,
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
    /// A report source had reports for the number (the fallback rules may block only then).
    #[serde(default)]
    pub reported: bool,
}

/// One rejected call, as a phone reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejection {
    pub at: String,
    pub number: String,
    /// `reject_line`: "Rejected likely telemarketer +45 … · 180.dk 6 reports".
    pub line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spam: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick_up: Option<f64>,
    #[serde(default)]
    pub sources: String,
    pub source: String,
}

/// Rejections kept (newest last).
pub const MAX_REJECTIONS_KEPT: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub calls: Vec<SeenCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<SavedVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_label: Option<String>,
    /// The last public caller-ID page read for this number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<CachedPublic>,
    /// The last answers of the report databases (by source).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<CachedPublic>,
    /// The owner allowed this number ("Allow" / "Allow & call back"): never rejected or blocked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allowed: bool,
    /// Last call or lookup (RFC 3339), for pruning.
    #[serde(default)]
    pub active: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    numbers: BTreeMap<String, Entry>,
    #[serde(default)]
    rejections: Vec<Rejection>,
    /// The local date of the last rejected-calls digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest_sent: Option<String>,
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
            owner_label: e
                .owner_label
                .or_else(|| e.allowed.then(|| "Allowed".to_owned())),
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

    /// The cached public page, when it is younger than `max_age`.
    pub fn public(
        &self,
        e164: &str,
        now: DateTime<Utc>,
        max_age: Duration,
    ) -> Option<PublicReport> {
        let c = self.entry(e164)?.public?;
        let at: DateTime<Utc> = c.at.parse().ok()?;
        let age = now.signed_duration_since(at).to_std().unwrap_or_default();
        (age <= max_age).then_some(c.report)
    }

    /// A database's cached answer, when younger than `max_age`.
    pub fn database(
        &self,
        e164: &str,
        source: &str,
        now: DateTime<Utc>,
        max_age: Duration,
    ) -> Option<PublicReport> {
        let c = self
            .entry(e164)?
            .databases
            .into_iter()
            .find(|c| c.report.source == source)?;
        let at: DateTime<Utc> = c.at.parse().ok()?;
        let age = now.signed_duration_since(at).to_std().unwrap_or_default();
        (age <= max_age).then_some(c.report)
    }

    pub fn save_database(&self, e164: &str, report: &PublicReport, at: &str) {
        self.update(|f| {
            let e = f.numbers.entry(e164.to_owned()).or_default();
            e.active = e.active.clone().max(at.to_owned());
            e.databases.retain(|c| c.report.source != report.source);
            e.databases.push(CachedPublic {
                at: at.to_owned(),
                report: report.clone(),
            });
        });
    }

    /// The owner's "Allow": never rejected or blocked again.
    pub fn allow(&self, e164: &str, at: &str) {
        self.update(|f| {
            let e = f.numbers.entry(e164.to_owned()).or_default();
            e.active = e.active.clone().max(at.to_owned());
            e.allowed = true;
        });
    }

    pub fn record_rejection(&self, r: Rejection) {
        self.update(|f| {
            f.rejections.push(r);
            if f.rejections.len() > MAX_REJECTIONS_KEPT {
                let drop = f.rejections.len() - MAX_REJECTIONS_KEPT;
                f.rejections.drain(..drop);
            }
        });
    }

    /// Rejections at or after `since`, oldest first.
    pub fn rejections_since(&self, since: DateTime<Utc>) -> Vec<Rejection> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read()
            .rejections
            .into_iter()
            .filter(|r| r.at.parse::<DateTime<Utc>>().is_ok_and(|t| t >= since))
            .collect()
    }

    pub fn digest_sent(&self) -> Option<String> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.read().digest_sent
    }

    pub fn set_digest_sent(&self, date: &str) {
        self.update(|f| f.digest_sent = Some(date.to_owned()));
    }

    pub fn save_public(&self, e164: &str, report: &PublicReport, at: &str) {
        self.update(|f| {
            let e = f.numbers.entry(e164.to_owned()).or_default();
            e.active = e.active.clone().max(at.to_owned());
            e.public = Some(CachedPublic {
                at: at.to_owned(),
                report: report.clone(),
            });
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
                reported: v.known.checked().iter().any(|r| r.reports > 0),
            });
        });
    }

    /// Call Directory entries (digits with country code, strictly ascending)
    /// and, with `block`, the numbers whose last answer passes the owner's
    /// thresholds (never contacts, allowed numbers, or guesses without evidence).
    pub fn directory(&self, block: Option<&RejectSettings>) -> Directory {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries: BTreeMap<i64, String> = BTreeMap::new();
        let mut blocked: Vec<i64> = Vec::new();
        for (num, e) in self.read().numbers {
            let Some(d) = caller::parse(&num).directory_number() else {
                continue;
            };
            let label = match (&e.owner_label, e.allowed, &e.verdict) {
                (Some(l), _, _) => l.clone(),
                (None, true, _) => "Allowed".to_owned(),
                (None, false, Some(v)) => v.short.clone(),
                (None, false, None) => continue,
            };
            entries.insert(d, label);
            if let (Some(s), Some(v), false, None) = (block, &e.verdict, e.allowed, &e.owner_label)
            {
                let evidence = v.source == "jev" || v.reported;
                let short = caller::is_short(&num) || caller::is_emergency(&num);
                if s.enabled
                    && s.valid()
                    && evidence
                    && !short
                    && v.source != "contact"
                    && (v.spam >= s.spam_min || v.pick_up <= s.pick_up_max)
                {
                    blocked.push(d);
                }
            }
        }
        blocked.sort_unstable();
        blocked.dedup();
        Directory {
            entries: entries.into_iter().collect(),
            block: blocked,
        }
    }
}

/// What the iOS Call Directory gets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Directory {
    pub entries: Vec<(i64, String)>,
    pub block: Vec<i64>,
}

// ---------------------------------------------------------------- screening

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestContext {
    /// Accepted from older phone builds and ignored: who might call him is not
    /// a fact about the number (owner's rule, 2026-10-29).
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

/// Where the public caller-ID pages come from (`LUCINA_CALLERS_PUBLIC_URL`).
///
/// The page is read with the system `curl`: 180.dk's TLS only offers
/// `AES256-SHA256` (TLS 1.2, no forward secrecy), which rustls refuses by
/// design (checked 2026-10-29), so reqwest cannot reach it.
#[derive(Debug, Clone, Default)]
pub struct PublicLookup {
    /// `https://www.180.dk` (a test server in tests): Danish numbers; `None`: no page lookup.
    pub base: Option<String>,
    /// `https://spamcalls.net` (a test server in tests): Italian numbers; `None`: off.
    pub spamcalls_base: Option<String>,
    /// IPQualityScore's phone API (`IPQS_API_KEY`), when the owner has a key.
    pub ipqs: Option<Ipqs>,
    /// tellows' partner Scorelist for Denmark, refreshed daily ([`refresh_tellows`]).
    pub tellows_list: Option<PathBuf>,
}

/// IPQualityScore phone reputation (`/api/json/phone/<key>/<number>`).
#[derive(Clone)]
pub struct Ipqs {
    pub http: reqwest::Client,
    pub key: String,
    /// `https://www.ipqualityscore.com` (a test server in tests).
    pub base: String,
}

impl std::fmt::Debug for Ipqs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ipqs")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

pub const IPQS_BASE: &str = "https://www.ipqualityscore.com";
/// An IPQS answer is reused this long (lookups cost credits).
const IPQS_FRESH: Duration = Duration::from_secs(7 * 24 * 3600);
/// A tellows Scorelist older than this no longer counts ("unknown", not "no reports").
const TELLOWS_MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);

pub const PUBLIC_BASE: &str = "https://www.180.dk";
/// A lookup in chat waits this long for the page; a ringing call much less.
const PUBLIC_TIMEOUT: Duration = Duration::from_secs(4);
const PUBLIC_TIMEOUT_RINGING: Duration = Duration::from_millis(700);
/// A cached page is reused this long for lookups, and longer while a call rings.
const PUBLIC_FRESH: Duration = Duration::from_secs(24 * 3600);
const PUBLIC_FRESH_RINGING: Duration = Duration::from_secs(7 * 24 * 3600);
/// Bytes of a page read (they are about 300 KB).
const PUBLIC_MAX_BYTES: usize = 2 * 1024 * 1024;
const USER_AGENT: &str = "Lucina/1.0 (personal caller lookup for one owner)";

/// Reads the number's public page; `None` on any failure or timeout.
/// One page through curl: its HTTP status and body (`None` when curl failed or timed out).
async fn curl_page(url: &str, timeout: Duration) -> Option<(u16, String)> {
    const MARK: &str = "\n__lucina_status__";
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--proto",
        "=https,http",
        "--max-redirs",
        "0",
    ])
    .arg("--max-time")
    .arg(format!("{:.1}", timeout.as_secs_f64()))
    .arg("--max-filesize")
    .arg(PUBLIC_MAX_BYTES.to_string())
    .arg("--user-agent")
    .arg(USER_AGENT)
    .arg("--write-out")
    .arg(format!("{MARK}%{{http_code}}"))
    .arg("--")
    .arg(url)
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true);
    let out = match tokio::time::timeout(timeout + Duration::from_millis(200), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "caller public page: curl did not run");
            return None;
        }
        Err(_) => {
            tracing::info!("caller public page timed out");
            return None;
        }
    };
    if !out.status.success() || out.stdout.len() > PUBLIC_MAX_BYTES + 64 {
        tracing::info!(
            code = out.status.code(),
            error = %String::from_utf8_lossy(&out.stderr).trim(),
            "caller public page refused"
        );
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text.rsplit_once(MARK)?;
    Some((status.trim().parse().ok()?, body.to_owned()))
}

/// The number's public page: 180.dk for Danish numbers, spamcalls.net for
/// Italian ones (HTTP 410 there = nobody reported it), nothing for other
/// countries (owner's choice: Jev only). `None` on any failure or timeout.
pub async fn fetch_public(p: &PublicLookup, n: &Number, timeout: Duration) -> Option<PublicReport> {
    match n.country {
        "DK" => {
            let url = caller::url_180(p.base.as_deref()?, n)?;
            let (status, html) = curl_page(&url, timeout).await?;
            if status != 200 {
                tracing::info!(status, "180.dk page refused");
                return None;
            }
            // Quote the page people can open, not a test server's.
            let public_url = caller::url_180(PUBLIC_BASE, n).unwrap_or(url);
            caller::parse_180(&html, &public_url)
        }
        "IT" => {
            let url = caller::url_spamcalls(p.spamcalls_base.as_deref()?, n)?;
            let public_url =
                caller::url_spamcalls(SPAMCALLS_BASE, n).unwrap_or_else(|| url.clone());
            let (status, html) = curl_page(&url, timeout).await?;
            match status {
                200 => caller::parse_spamcalls(&html, &public_url, "IT"),
                410 => Some(caller::spamcalls_none(&public_url, "IT")),
                s => {
                    tracing::info!(status = s, "spamcalls.net page refused");
                    None
                }
            }
        }
        _ => None,
    }
}

pub const SPAMCALLS_BASE: &str = "https://spamcalls.net";

/// The public report for `n`: the cache when fresh, else the page (cached on success), else a stale cache.
async fn public_report(
    p: Option<&PublicLookup>,
    store: Option<&CallerStore>,
    n: &Number,
    ringing: bool,
    now: DateTime<Utc>,
) -> Option<PublicReport> {
    if !caller::has_database(n.country) {
        return None;
    }
    let fresh = if ringing {
        PUBLIC_FRESH_RINGING
    } else {
        PUBLIC_FRESH
    };
    let cached = |max_age| {
        store
            .and_then(|s| s.public(&n.e164, now, max_age))
            .and_then(|r| current_source(r, n))
    };
    if let Some(r) = cached(fresh) {
        return Some(r);
    }
    let timeout = if ringing {
        PUBLIC_TIMEOUT_RINGING
    } else {
        PUBLIC_TIMEOUT
    };
    match fetch_public(p?, n, timeout).await {
        Some(r) => {
            if let Some(s) = store {
                s.save_public(&n.e164, &r, &now.to_rfc3339());
            }
            Some(r)
        }
        None => cached(Duration::MAX),
    }
}

/// A cached page counts only when it came from the source for the number's
/// country (an Italian number once read on 180.dk is read again on
/// spamcalls.net); 180.dk pages cached before `covers` existed cover Denmark.
fn current_source(mut r: PublicReport, n: &Number) -> Option<PublicReport> {
    let expected = match n.country {
        "DK" => caller::SOURCE_180,
        "IT" => caller::SOURCE_SPAMCALLS,
        _ => return None,
    };
    if r.source != expected {
        return None;
    }
    if r.covers.is_none() && r.source == caller::SOURCE_180 {
        r.covers = Some("DK".into());
    }
    Some(r)
}

/// IPQS's answer as a report: abuse flags count as a report; the fraud score and
/// line facts are quoted as the database states them.
pub fn ipqs_report(v: &Value, n: &Number) -> Option<PublicReport> {
    let covers = Some(n.country.to_owned()).filter(|c| !c.is_empty());
    if v["success"] != Value::Bool(true) {
        return None;
    }
    let b = |k: &str| v[k].as_bool().unwrap_or(false);
    let fraud = v["fraud_score"].as_u64();
    let abuse = b("spammer") || b("recent_abuse") || fraud.is_some_and(|f| f >= 85);
    let mut flags = Vec::new();
    if let Some(t) = v["line_type"]
        .as_str()
        .filter(|t| !t.is_empty() && *t != "N/A")
    {
        flags.push(format!("line type {t}"));
    }
    for (k, what) in [
        ("spammer", "listed as spammer"),
        ("recent_abuse", "recent abuse"),
        ("VOIP", "VOIP"),
        ("risky", "risky"),
        ("do_not_call", "do-not-call list"),
    ] {
        if b(k) {
            flags.push(what.to_owned());
        }
    }
    Some(PublicReport {
        source: "IPQS".into(),
        url: format!(
            "https://www.ipqualityscore.com/free-phone-number-lookup/lookup/{}",
            n.e164.trim_start_matches('+')
        ),
        reports: u32::from(abuse),
        score: fraud.map(|f| format!("fraud {f}/100")),
        covers,
        operator: v["carrier"]
            .as_str()
            .filter(|c| !c.is_empty() && *c != "N/A")
            .map(str::to_owned),
        flags,
        ..PublicReport::default()
    })
}

async fn fetch_ipqs(q: &Ipqs, n: &Number, timeout: Duration) -> Option<PublicReport> {
    let url = format!(
        "{}/api/json/phone/{}/{}",
        q.base.trim_end_matches('/'),
        q.key,
        n.e164.trim_start_matches('+')
    );
    let resp = q
        .http
        .get(url)
        .query(&[("strictness", "1")])
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| tracing::info!(error = %e.without_url(), "IPQS lookup failed"))
        .ok()?;
    if !resp.status().is_success() {
        tracing::info!(status = resp.status().as_u16(), "IPQS lookup refused");
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    ipqs_report(&v, n)
}

/// One row of tellows' Scorelist CSV (documented header:
/// `Nummer;Score;Kommentare;Land;Vorwahl;Suchanfragen;Anruftyp;Deeplink;…`).
pub fn tellows_rows(csv: &str) -> BTreeMap<String, PublicReport> {
    let mut lines = csv.lines();
    let Some(head) = lines.next() else {
        return BTreeMap::new();
    };
    let cols: Vec<String> = head.split(';').map(|c| c.trim().to_lowercase()).collect();
    let col = |names: &[&str]| cols.iter().position(|c| names.contains(&c.as_str()));
    let (Some(num), Some(score)) = (col(&["nummer", "number"]), col(&["score"])) else {
        return BTreeMap::new();
    };
    let comments = col(&["kommentare", "comments"]);
    let searches = col(&["suchanfragen", "searches"]);
    let kind = col(&["anruftyp", "callertype"]);
    let link = col(&["deeplink"]);
    let mut out = BTreeMap::new();
    for l in lines {
        let f: Vec<&str> = l.split(';').map(str::trim).collect();
        let get = |i: Option<usize>| i.and_then(|i| f.get(i)).copied().unwrap_or("");
        let n = caller::parse(get(Some(num)));
        if !n.valid() {
            continue;
        }
        let reports: u32 = get(comments).parse().unwrap_or(0);
        let mut r = PublicReport {
            source: "tellows".into(),
            url: Some(get(link))
                .filter(|u| u.starts_with("http"))
                .map_or_else(
                    || format!("https://www.tellows.dk/num/{}", n.national),
                    str::to_owned,
                ),
            searches: get(searches).parse().unwrap_or(0),
            reports,
            score: Some(format!("{}/9", get(Some(score)))).filter(|s| s != "/9"),
            covers: Some("DK".into()),
            ..PublicReport::default()
        };
        if let Some(k) = Some(get(kind)).filter(|k| !k.is_empty()) {
            r.flags.push(format!("caller type {k}"));
        }
        out.insert(n.e164.clone(), r);
    }
    out
}

/// The Scorelist in memory, reloaded when the file changes.
fn tellows_lookup(path: &Path, n: &Number, now: DateTime<Utc>) -> Option<PublicReport> {
    use std::sync::OnceLock;
    type Cache = Mutex<Option<(std::time::SystemTime, BTreeMap<String, PublicReport>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let age = std::time::SystemTime::from(now)
        .duration_since(modified)
        .unwrap_or_default();
    if age > TELLOWS_MAX_AGE {
        return None;
    }
    let mut c = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if c.as_ref().map(|(m, _)| *m) != Some(modified) {
        let text = std::fs::read_to_string(path).ok()?;
        *c = Some((modified, tellows_rows(&text)));
    }
    let rows = &c.as_ref()?.1;
    if rows.is_empty() {
        return None;
    }
    // Not on the list (which holds every number with a comment): zero reports.
    Some(rows.get(&n.e164).cloned().unwrap_or_else(|| PublicReport {
        source: "tellows".into(),
        url: format!("https://www.tellows.dk/num/{}", n.national),
        covers: Some("DK".into()),
        ..PublicReport::default()
    }))
}

/// Downloads tellows' Scorelist for Denmark (partner keys) to `path`, atomically.
pub async fn refresh_tellows(
    http: &reqwest::Client,
    base: &str,
    partner: &str,
    apikey: &str,
    path: &Path,
) -> Result<usize, String> {
    let resp = http
        .get(format!(
            "{}/stats/partnerscoredata",
            base.trim_end_matches('/')
        ))
        .query(&[
            ("partner", partner),
            ("apikey", apikey),
            ("country", "dk"),
            ("mincomments", "1"),
            ("showdeeplink", "1"),
            ("numberformatinternational", "1"),
        ])
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("tellows answered {}", resp.status()));
    }
    let text = resp.text().await.map_err(|e| e.without_url().to_string())?;
    let n = tellows_rows(&text).len();
    if n == 0 {
        return Err("tellows sent no rows".into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("csv.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(n)
}

/// The report databases: tellows' list (local) and IPQS (cached, then live).
async fn database_reports(
    p: Option<&PublicLookup>,
    store: Option<&CallerStore>,
    n: &Number,
    ringing: bool,
    now: DateTime<Utc>,
) -> Vec<PublicReport> {
    let Some(p) = p.filter(|_| caller::has_database(n.country)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(path) = p.tellows_list.as_ref().filter(|_| n.country == "DK") {
        out.extend(tellows_lookup(path, n, now));
    }
    if let Some(q) = &p.ipqs {
        let cached = store.and_then(|s| s.database(&n.e164, "IPQS", now, IPQS_FRESH));
        let r = match cached {
            Some(r) => Some(r),
            None => {
                let t = if ringing {
                    PUBLIC_TIMEOUT_RINGING
                } else {
                    PUBLIC_TIMEOUT
                };
                let r = fetch_ipqs(q, n, t).await;
                if let (Some(r), Some(s)) = (&r, store) {
                    s.save_database(&n.e164, r, &now.to_rfc3339());
                }
                r.or_else(|| store.and_then(|s| s.database(&n.e164, "IPQS", now, Duration::MAX)))
            }
        };
        out.extend(r);
    }
    out
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
    public: Option<&PublicLookup>,
    req: &ScreenRequest,
    now: DateTime<Utc>,
    mentions: &(dyn Fn(&Number) -> Vec<String> + Sync),
) -> Result<ScreenResponse, String> {
    req.validate()?;
    let lang = caller::lang(req.lang.as_deref().unwrap_or("en"));
    let rc = req.context.clone().unwrap_or_default();
    let contact = rc.contact.map(|c| c.trim().to_owned());
    let ringing = req.call.is_some();
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
        let mut ctx = Context { known, ringing };
        let at = at.clone();
        async move {
            // The public page first: it changes the verdict the most.
            if n.valid() && ctx.known.contact.is_none() {
                let (page, dbs) = futures_util::future::join(
                    public_report(public, store, &n, ringing, now),
                    database_reports(public, store, &n, ringing, now),
                )
                .await;
                ctx.known.public = page;
                ctx.known.databases = dbs;
            }
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
                    s.record_call(
                        &n.e164,
                        SeenCall {
                            at: at.clone(),
                            source: call.source.clone(),
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
        "Screen phone numbers that called the owner (\"who called me from …\"): Lucina reads the public caller-ID page (180.dk) first, then Jev gives P(spam), who is likely calling (person, business, telemarketing, scam, unknown) and P(pick up) from facts about the number only (country, line type, contacts, his calls with it, the public reports). Show the returned table as is, with the quoted sources."
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
        let public = ctx
            .env
            .callers_public_url
            .as_ref()
            .map(|base| PublicLookup {
                base: Some(base.clone()),
                spamcalls_base: Some(SPAMCALLS_BASE.into()),
                ..PublicLookup::default()
            });
        let r = screen(
            lucina_jev::shared(),
            ctx.env.callers.as_deref(),
            public.as_ref(),
            &req,
            ctx.now,
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
        // Older phone builds still send the shift: accepted, never used.
        r.context = Some(RequestContext {
            courier: Some(Courier {
                on_shift: true,
                order_active: true,
                order_age_min: None,
            }),
            contact: None,
        });
        let out = screen(&off(), Some(&store), None, &r, now, &|_| Vec::new())
            .await
            .unwrap();
        let v = &out.results[0];
        assert_eq!((v.source.as_str(), v.kind.as_str()), ("rules", "business"));
        assert_eq!(v.known.previous_calls, 0);
        assert!(out.table.contains("Source: basic rules"));

        // The second call knows the first.
        let out = screen(&off(), Some(&store), None, &r, now, &|_| Vec::new())
            .await
            .unwrap();
        assert_eq!(out.results[0].known.previous_calls, 1);
        let e = store.entry("+4570405205").unwrap();
        assert_eq!(e.calls.len(), 2);
        assert_eq!(
            store.directory(None).entries,
            vec![(4570405205, "Unknown caller".to_owned())]
        );

        // A lookup saves the answer but is no call; contacts never reach the directory.
        let mut l = req(&["20682758"]);
        l.context = Some(RequestContext {
            courier: None,
            contact: Some("Jerry's".into()),
        });
        let out = screen(&off(), Some(&store), None, &l, now, &|n| {
            vec![format!("seen {}", n.e164)]
        })
        .await
        .unwrap();
        assert_eq!(out.results[0].source, "contact");
        assert_eq!(
            out.results[0].known.mentions,
            vec!["seen +4520682758".to_owned()]
        );
        assert!(store.entry("+4520682758").is_none());
        assert_eq!(store.directory(None).entries.len(), 1);
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
            None,
            &req(&["hello", "+357 94 095733"]),
            now,
            &|_| Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.results[0].line, LineType::Invalid);
        assert_eq!(out.results[1].kind, "unknown");
        assert!(out.table.contains("Not a phone number"));
    }

    /// A 180.dk-like page (the structure the site served on 2026-10-29).
    const PAGE_180: &str = r#"<title>20682758 | ADVARSEL &#x2013; rapporter: 6</title>
<span>20682758</span><span>(Danmark)</span><span>+4520682758</span>
<b>374</b><small>Antal gange s&#xF8;gte</small><b>6</b><small>Rapporteret af brugere</small>
<h3>Seneste kommentar</h3><p>26. oktober 2026</p><p>Irriterende</p><p>De ringer i min arbejdstid</p><p>Kommentar fra bruger</p>
<li>Forsigtig (37,5%)</li><li>Irriterende (37,5%)</li><li>Telemarketing (25%)</li>"#;

    #[tokio::test]
    async fn the_public_page_is_read_first_cached_and_quoted() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let site = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nummer/20682758"))
            .and(header("user-agent", USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_string(PAGE_180))
            .expect(1)
            .mount(&site)
            .await;
        // Italian numbers go to spamcalls.net: 410 = nobody reported it.
        Mock::given(method("GET"))
            .and(path("/en/num/393331740069"))
            .respond_with(
                ResponseTemplate::new(410).set_body_string("<title>+ 393331740069</title>"),
            )
            .expect(1)
            .mount(&site)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let store = CallerStore::in_data_dir(dir.path());
        let lookup = PublicLookup {
            base: Some(site.uri()),
            spamcalls_base: Some(site.uri()),
            ..PublicLookup::default()
        };
        let now: DateTime<Utc> = "2026-10-29T12:00:00Z".parse().unwrap();
        let r = req(&["+45 20 68 27 58", "+357 94 095733", "+39 333 174 0069"]);
        let out = screen(&off(), Some(&store), Some(&lookup), &r, now, &|_| {
            Vec::new()
        })
        .await
        .unwrap();
        let v = &out.results[0];
        let p = v.known.public.as_ref().expect("public report");
        assert_eq!((p.reports, p.searches), (6, 374));
        assert_eq!(p.url, "https://www.180.dk/nummer/20682758");
        // Jev is off here: the rules follow the reports.
        assert_eq!(
            (v.source.as_str(), v.kind.as_str()),
            ("rules", "telemarketing")
        );
        assert!(
            v.reasons.contains(&"180.dk: 6 reports".to_owned()),
            "{:?}",
            v.reasons
        );
        assert!(
            out.table.contains("https://www.180.dk/nummer/20682758"),
            "{}",
            out.table
        );
        // Cyprus: no database, no lookup at all (wiremock would refuse an unknown path).
        assert!(out.results[1].known.public.is_none());
        assert!(
            out.results[1]
                .label
                .ends_with("· no database for this country"),
            "{}",
            out.results[1].label
        );
        let it = out.results[2]
            .known
            .public
            .as_ref()
            .expect("spamcalls report");
        assert_eq!(
            (it.source.as_str(), it.reports, it.covers.as_deref()),
            ("spamcalls.net", 0, Some("IT"))
        );
        assert_eq!(it.url, "https://spamcalls.net/en/num/393331740069");
        // The second lookup within a day uses the cache (the page is asked for once).
        let again = screen(
            &off(),
            Some(&store),
            Some(&lookup),
            &req(&["20682758"]),
            now,
            &|_| Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(again.results[0].known.public.as_ref().unwrap().reports, 6);
        site.verify().await;
    }

    #[test]
    fn ipqs_and_tellows_answers_become_reports() {
        let n = caller::parse("+4570405205");
        let clean = serde_json::json!({"success": true, "valid": true, "fraud_score": 12,
            "recent_abuse": false, "spammer": false, "VOIP": false, "risky": false,
            "line_type": "Landline", "carrier": "TDC"});
        let r = ipqs_report(&clean, &n).unwrap();
        assert_eq!((r.source.as_str(), r.reports), ("IPQS", 0));
        assert_eq!(r.score.as_deref(), Some("fraud 12/100"));
        assert_eq!(r.flags, vec!["line type Landline".to_owned()]);
        let bad = serde_json::json!({"success": true, "fraud_score": 91, "recent_abuse": true, "spammer": true});
        let r = ipqs_report(&bad, &n).unwrap();
        assert_eq!(r.reports, 1);
        assert!(r.flags.contains(&"listed as spammer".to_owned()));
        assert!(ipqs_report(
            &serde_json::json!({"success": false, "message": "Invalid key"}),
            &n
        )
        .is_none());

        let csv = "Nummer;Score;Kommentare;Land;Vorwahl;Suchanfragen;Anruftyp;Deeplink;CallerTypeId\n\
                   +4570405205;8;23;45;70;2139;Telemarketing;https://www.tellows.dk/num/70405205;5\n\
                   bogus;9;1;45;;;;;\n";
        let rows = tellows_rows(csv);
        assert_eq!(rows.len(), 1);
        let t = &rows["+4570405205"];
        assert_eq!(
            (t.reports, t.searches, t.score.as_deref()),
            (23, 2139, Some("8/9"))
        );
        assert_eq!(t.flags, vec!["caller type Telemarketing".to_owned()]);
        assert!(tellows_rows("").is_empty() && tellows_rows("a;b\n1;2").is_empty());

        // A fresh list answers "not listed" with zero reports; a stale one answers nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tellows-dk.csv");
        std::fs::write(&path, csv).unwrap();
        let now = Utc::now();
        assert_eq!(tellows_lookup(&path, &n, now).unwrap().reports, 23);
        let other = tellows_lookup(&path, &caller::parse("+4566778702"), now).unwrap();
        assert_eq!((other.source.as_str(), other.reports), ("tellows", 0));
        assert!(tellows_lookup(&path, &n, now + chrono::Duration::days(4)).is_none());
    }

    #[test]
    fn allow_rejections_digest_and_block_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = CallerStore::in_data_dir(dir.path());
        let at = "2026-10-29T12:00:00Z";
        let save = |num: &str, spam: f64, pick: f64, source: &str, reported: bool| {
            store.update(|f| {
                let e = f.numbers.entry(num.to_owned()).or_default();
                e.verdict = Some(SavedVerdict {
                    at: at.into(),
                    short: "x".into(),
                    kind: "unknown".into(),
                    kind_p: 0.5,
                    spam,
                    pick_up: pick,
                    source: source.into(),
                    reported,
                });
            })
        };
        save("+4570405205", 0.75, 0.16, "jev", true);
        save("+4566778702", 0.30, 0.52, "jev", false);
        save("+4520682758", 0.20, 0.10, "rules", false); // a guess without evidence
        save("+35794095733", 0.99, 0.0, "contact", false);
        let s = RejectSettings::default();
        assert_eq!(store.directory(Some(&s)).block, vec![4570405205]);
        assert!(store.directory(None).block.is_empty());
        let off = RejectSettings {
            enabled: false,
            ..s
        };
        assert!(store.directory(Some(&off)).block.is_empty());

        store.allow("+4570405205", at);
        let d = store.directory(Some(&s));
        assert!(d.block.is_empty());
        assert!(d.entries.contains(&(4570405205, "Allowed".to_owned())));
        assert_eq!(
            store.known("+4570405205").owner_label.as_deref(),
            Some("Allowed")
        );

        for i in 0..3 {
            store.record_rejection(Rejection {
                at: format!("2026-10-29T1{i}:00:00Z"),
                number: "+4570405220".into(),
                line: "Rejected likely telemarketer +45 70 40 52 20 · 180.dk 1 reports".into(),
                spam: Some(0.82),
                pick_up: Some(0.11),
                sources: "180.dk 1 reports".into(),
                source: "android".into(),
            });
        }
        let since: DateTime<Utc> = "2026-10-29T11:00:00Z".parse().unwrap();
        assert_eq!(store.rejections_since(since).len(), 2);
        assert_eq!(store.digest_sent(), None);
        store.set_digest_sent("2026-10-29");
        assert_eq!(store.digest_sent().as_deref(), Some("2026-10-29"));
    }

    #[test]
    fn cached_pages_must_come_from_the_country_s_source() {
        let old_180 = PublicReport {
            source: caller::SOURCE_180.into(),
            url: "u".into(),
            ..PublicReport::default()
        };
        let dk = current_source(old_180.clone(), &caller::parse("+4566778702")).unwrap();
        assert_eq!(dk.covers.as_deref(), Some("DK"));
        assert!(current_source(old_180, &caller::parse("+393383037513")).is_none());
        let sc = caller::spamcalls_none("u", "IT");
        assert!(current_source(sc.clone(), &caller::parse("+393383037513")).is_some());
        assert!(current_source(sc, &caller::parse("+35794095733")).is_none());
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
