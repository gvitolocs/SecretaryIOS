//! Server overview: one aggregate for the Server dashboard (web `/server-health`,
//! the iPhone plugin and widget), so no client joins sources itself.
//!
//! - `GET /v1/system/overview[?fresh=1]` (device or session, like
//!   `GET /v1/system/health`) → `{generated_at, overall, attention_count,
//!   flapping_count, checks_ok, checks_total, sources, hosts, attention,
//!   services, timeline}`.
//!
//! Sources, fetched concurrently, each with its own deadline; a failing one is
//! reported in `sources.<name>` (`{ok, at, error}`) and never fails the response:
//! - `sentinel`: the in-process host snapshots and their state changes
//!   ([`crate::system_health`]);
//! - `catalog`: lucina-agentd `GET /catalog/status` and `GET /catalog` (the
//!   tree) through Lucina, like `/v1/agents/catalog*`, as the calling device
//!   (`X-Lucina-Origin: device:<id>`); [`CATALOG_TIMEOUT`];
//! - `ops`: Lucina's ops record, incidents, deploys and decisions of the last
//!   [`OPS_DAYS`] days ([`OPS_LIMIT`] of each); [`OPS_TIMEOUT`].
//!
//! [`build`] merges them (pure): `hosts` in a fixed order (catalog hosts, host
//! components and the sentinel's hosts); `attention` = catalog problems that are
//! down/broken (crit) or warn/degraded (warn) plus the sentinel's warn/crit checks,
//! one item per catalog component when a check matches one of its units or
//! containers, and a component that is broken only because of other listed ones
//! folded into their `caused`. A warn item with no sentinel source, whose every
//! non-ok catalog finding is a unit or container that is up now and whose only
//! problem is the failure count (`active/running`, `active/waiting` or
//! `active/exited`, plus "N failures in 24h"), is `flapping`: it stays in
//! `attention`, but `attention_count` skips it (`flapping_count` counts it) and
//! `overall` ignores it. All flapping and nothing else is `ok`. Its
//! summary is "Running; failed N times in 24 h", N the largest such count.
//! Order is non-flapping first (crit, then warn, oldest `since` first), then
//! flapping by `failures_24h` descending. A summary keeps at most two parts, and
//! one part per kind (the larger 24 h failure count, or the larger job-history
//! count). `public_hostnames_broken` keeps hostnames only. `services` = the
//! catalog tree, trimmed; `timeline` = ops records and sentinel state changes,
//! newest first, at most [`MAX_TIMELINE`]. Every free-text field goes through
//! [`redact`].
//!
//! The response is cached for [`CACHE_FOR`]; `?fresh=1` refetches. Callers that
//! arrive during a fetch wait for it and share it, and a response where every
//! source failed is never cached.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration as StdDuration, Instant};

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use secretary_core::Call;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::auth::Identity;
use crate::error::QueryArgs;
use crate::system_health::{Event, Snapshot, State3};
use crate::AppState;

pub const CACHE_FOR: StdDuration = StdDuration::from_secs(30);
pub const CATALOG_TIMEOUT: StdDuration = StdDuration::from_secs(25);
pub const OPS_TIMEOUT: StdDuration = StdDuration::from_secs(8);
pub const OPS_DAYS: i64 = 14;
pub const OPS_LIMIT: u32 = 30;
pub const OPS_KINDS: [&str; 3] = ["incident", "deploy", "decision"];
pub const MAX_TIMELINE: usize = 150;
/// One plain-words summary part, and a timeline `detail`.
pub const MAX_LINE: usize = 120;
/// Hosts first in this order, then any other by id.
pub const HOST_ORDER: [&str; 7] = [
    "nezopt",
    "p1",
    "oracle-peer1",
    "pi-home",
    "nespc",
    "lubmipc",
    "mac",
];
const MASK: &str = "•••";

/// The last response and when it was built.
pub type Cache = tokio::sync::Mutex<Option<(Instant, Value)>>;

// -- redaction ------------------------------------------------------------------------

/// Free text with credentials replaced by `•••`: `Bearer <x>`, `token=`/`key=`/
/// `secret=`/`password=` values, JWTs, runs of ≥ 32 hex characters, base64-like runs of
/// ≥ 40 (except environment variable names), `sk-`/`ghp_`/`xox?-`-style keys and `://user:pass@`.
/// Names stay (`CARDTRADER_AUTH_TOKEN`); only values go.
pub fn redact(s: &str) -> String {
    let mut c: Vec<char> = s.chars().collect();
    userinfo(&mut c);
    bearer(&mut c);
    key_values(&mut c);
    prefixed(&mut c);
    runs(&mut c);
    c.into_iter().collect()
}

fn splice(c: &mut Vec<char>, from: usize, to: usize) {
    c.splice(from..to, MASK.chars());
}

fn token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '+' | '/' | '=')
}

fn word_start(c: &[char], i: usize) -> bool {
    i == 0 || !(c[i - 1].is_ascii_alphanumeric() || c[i - 1] == '_')
}

fn starts_with_ci(c: &[char], i: usize, pat: &str) -> bool {
    for (k, p) in (i..).zip(pat.chars()) {
        if k >= c.len() || !c[k].eq_ignore_ascii_case(&p) {
            return false;
        }
    }
    true
}

/// `scheme://user:pass@host` → `scheme://•••@host`.
fn userinfo(c: &mut Vec<char>) {
    let mut i = 0;
    while i + 3 <= c.len() {
        if c[i] == ':' && c.get(i + 1) == Some(&'/') && c.get(i + 2) == Some(&'/') {
            let from = i + 3;
            let mut j = from;
            while j < c.len() && !c[j].is_whitespace() && !matches!(c[j], '/' | '@' | '"' | '\'') {
                j += 1;
            }
            if j < c.len() && c[j] == '@' && c[from..j].contains(&':') {
                splice(c, from, j);
                i = from + MASK.chars().count();
                continue;
            }
        }
        i += 1;
    }
}

/// `Bearer <x>` → `Bearer •••`, including short and alphabetic credentials.
fn bearer(c: &mut Vec<char>) {
    let mut i = 0;
    while i < c.len() {
        if word_start(c, i) && starts_with_ci(c, i, "bearer") {
            let mut j = i + 6;
            while j < c.len() && (c[j] == ' ' || c[j] == '\t') {
                j += 1;
            }
            let from = j;
            while j < c.len() && token_char(c[j]) {
                j += 1;
            }
            let tok = &c[from..j];
            if from > i + 6 && !tok.is_empty() {
                splice(c, from, j);
                i = from + MASK.chars().count();
                continue;
            }
        }
        i += 1;
    }
}

/// `…token=<v>`, `…key=<v>`, `…secret=<v>`, `…password=<v>` (any case; the name stays).
fn key_values(c: &mut Vec<char>) {
    const NAMES: [&str; 5] = ["token", "key", "secret", "password", "passwd"];
    let mut i = 0;
    while i < c.len() {
        if c[i] == '=' {
            let mut k = i;
            while k > 0 && (c[k - 1].is_ascii_alphanumeric() || c[k - 1] == '_' || c[k - 1] == '-')
            {
                k -= 1;
            }
            let name: String = c[k..i].iter().collect::<String>().to_ascii_lowercase();
            if NAMES.iter().any(|n| name.ends_with(n)) {
                let from = i + 1;
                let quote = c.get(from).copied().filter(|q| matches!(q, '"' | '\''));
                let start = from + usize::from(quote.is_some());
                let mut j = start;
                while j < c.len()
                    && match quote {
                        Some(q) => c[j] != q,
                        None => {
                            !c[j].is_whitespace()
                                && !matches!(
                                    c[j],
                                    '&' | ';' | ',' | '"' | '\'' | ')' | ']' | '}' | '>'
                                )
                        }
                    }
                {
                    j += 1;
                }
                let value: String = c[start..j].iter().collect();
                if j > start && value != MASK {
                    splice(c, start, j);
                    i = start + MASK.chars().count();
                    continue;
                }
            }
        }
        i += 1;
    }
}

/// Keys with a well-known prefix: `sk-…`, `ghp_…`, `xoxb-…`, `glpat-…`, this server's
/// `sec_`/`svc_` tokens, and JWTs (`eyJ…`).
fn prefixed(c: &mut Vec<char>) {
    const PREFIXES: [&str; 16] = [
        "sk-",
        "sk_",
        "pk_live_",
        "rk_live_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xox",
        "sec_",
        "svc_",
        "AKIA",
        "eyJ",
    ];
    let mut i = 0;
    while i < c.len() {
        if word_start(c, i) {
            if let Some(p) = PREFIXES
                .iter()
                .find(|p| c[i..].starts_with(&p.chars().collect::<Vec<_>>()))
            {
                let mut j = i + p.len();
                // `xox` keys go on with one letter and a dash (`xoxb-`).
                if *p == "xox"
                    && !(c.get(j).is_some_and(char::is_ascii_alphabetic)
                        && c.get(j + 1) == Some(&'-'))
                {
                    i += 1;
                    continue;
                }
                while j < c.len()
                    && (c[j].is_ascii_alphanumeric() || matches!(c[j], '-' | '_' | '.' | '='))
                {
                    j += 1;
                }
                if j - i > p.len() {
                    splice(c, i, j);
                    i += MASK.chars().count();
                    continue;
                }
            }
        }
        i += 1;
    }
}

/// Runs of ≥ 32 hex characters, and base64/url-safe runs of ≥ 40.
fn runs(c: &mut Vec<char>) {
    let b64 = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '_' | '-' | '=');
    let mut i = 0;
    while i < c.len() {
        if !b64(c[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < c.len() && b64(c[j]) {
            j += 1;
        }
        let run: Vec<char> = c[i..j].to_vec();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        // Hex inside the run.
        let mut k = 0;
        while k < run.len() {
            if run[k].is_ascii_hexdigit() {
                let s = k;
                while k < run.len() && run[k].is_ascii_hexdigit() {
                    k += 1;
                }
                if k - s >= 32 {
                    spans.push((s, k));
                }
            } else {
                k += 1;
            }
        }
        if looks_random(&run) {
            spans.clear();
            spans.push((0, run.len()));
        }
        if spans.is_empty() {
            i = j;
            continue;
        }
        let mut out: Vec<char> = Vec::with_capacity(run.len());
        let mut at = 0;
        for (s, e) in spans {
            out.extend_from_slice(&run[at..s]);
            out.extend(MASK.chars());
            at = e;
        }
        out.extend_from_slice(&run[at..]);
        let len = out.len();
        c.splice(i..j, out);
        i += len;
    }
}

/// Long base64/url-safe runs are secrets even without mixed case or digits.
/// Uppercase environment variable names are safe to display as names.
fn looks_random(run: &[char]) -> bool {
    let env_name = run.first().is_some_and(char::is_ascii_uppercase)
        && run.contains(&'_')
        && run
            .iter()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || *ch == '_');
    run.len() >= 40 && !env_name
}

// -- small helpers ---------------------------------------------------------------------

fn st<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)?.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k)?.as_f64().filter(|x| x.is_finite())
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn time_v(t: Option<DateTime<Utc>>) -> Value {
    t.map_or(Value::Null, |t| Value::String(rfc(t)))
}

fn text(s: Option<&str>) -> Value {
    s.map_or(Value::Null, |s| Value::String(redact(s)))
}

fn shorten(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// A catalog `effective` (or level) as the dashboard's state.
pub fn state_of(effective: &str) -> &'static str {
    match effective {
        "down" | "broken" | "crit" => "crit",
        "warn" | "degraded" => "warn",
        "ok" => "ok",
        "idle" => "idle",
        _ => "unknown",
    }
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "crit" => 2,
        "warn" => 1,
        _ => 0,
    }
}

fn s3(s: State3) -> &'static str {
    match s {
        State3::Ok => "ok",
        State3::Warn => "warn",
        State3::Crit => "crit",
    }
}

fn lower_name<T: serde::Serialize>(v: &T) -> Option<String> {
    serde_json::to_value(v).ok()?.as_str().map(str::to_owned)
}

/// One finding in plain words. "last run …, last success …, N failed jobs in
/// history" drops the timestamps.
pub fn plain(what: &str, detail: &str) -> String {
    let d = detail.trim();
    let lower = d.to_ascii_lowercase();
    if lower.ends_with("not listening") {
        if let Some(port) = what.trim().strip_prefix("port ") {
            return format!("port {} not listening", port.trim());
        }
        return "not listening".into();
    }
    if lower.starts_with("inactive") || lower.starts_with("dead") {
        return "Not running".into();
    }
    if lower.starts_with("failed") {
        return "Failed".into();
    }
    if let Some(n) = failures_24h_in(&lower) {
        return format!("failed {n} times in 24 h");
    }
    if let Some(n) = failed_jobs_in(&lower) {
        return format!("{n} failed jobs in history");
    }
    shorten(&redact(if d.is_empty() { what } else { d }), MAX_LINE)
}

/// `N` in "…, N failures in 24h".
fn failures_24h_in(lower: &str) -> Option<u64> {
    let at = lower.find(" failures in 24h")?;
    let head = &lower[..at];
    let digits: String = head
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// `N` in "…, N failed jobs in history", when the rest is only the last-run timestamps.
fn failed_jobs_in(lower: &str) -> Option<u64> {
    let lower = lower.trim();
    let at = lower
        .rfind(" failed jobs in history")
        .or_else(|| lower.rfind(" failed job in history"))?;
    let marker_len = if lower[at..].starts_with(" failed jobs") {
        " failed jobs in history".len()
    } else {
        " failed job in history".len()
    };
    if !lower[at + marker_len..].trim().is_empty() {
        return None;
    }
    let head = lower[..at].trim_end();
    let digits: String = head
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse().ok()?;
    let prefix = head[..head.len() - digits.len()].trim().trim_end_matches(',').trim();
    (prefix.is_empty() || run_and_success(prefix)).then_some(n)
}

/// "last run <time>, last success <time>".
fn run_and_success(prefix: &str) -> bool {
    let Some((run, success)) = prefix
        .strip_prefix("last run ")
        .and_then(|r| r.split_once(", last success "))
    else {
        return false;
    };
    let (run, success) = (run.trim(), success.trim());
    !run.is_empty() && !success.is_empty() && !run.contains(',') && !success.contains(',')
}

/// "failed N times in 24 h" from [`plain`].
fn failure_count_part(s: &str) -> Option<u64> {
    s.strip_prefix("failed ")?.strip_suffix(" times in 24 h")?.parse().ok()
}

/// "N failed jobs in history" from [`plain`].
fn jobs_part(s: &str) -> Option<u64> {
    s.strip_suffix(" failed jobs in history")
        .or_else(|| s.strip_suffix(" failed job in history"))?
        .trim()
        .parse()
        .ok()
}

/// One part per kind: the larger 24 h failure count, the larger job-history count.
fn keep_worse(parts: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut fail_at: Option<usize> = None;
    let mut fail_n = 0u64;
    let mut jobs_at: Option<usize> = None;
    let mut jobs_n = 0u64;
    for p in parts {
        if let Some(n) = failure_count_part(&p) {
            match fail_at {
                Some(i) if n > fail_n => {
                    fail_n = n;
                    out[i] = p;
                }
                Some(_) => {}
                None => {
                    fail_n = n;
                    fail_at = Some(out.len());
                    out.push(p);
                }
            }
            continue;
        }
        if let Some(n) = jobs_part(&p) {
            match jobs_at {
                Some(i) if n > jobs_n => {
                    jobs_n = n;
                    out[i] = p;
                }
                Some(_) => {}
                None => {
                    jobs_n = n;
                    jobs_at = Some(out.len());
                    out.push(p);
                }
            }
            continue;
        }
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn word(what: &str, w: &str) -> bool {
    what.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|p| p == w)
}

/// A unit or container that is up, whose detail is only "N failures in 24h".
fn active_failure_count(what: &str, detail: &str) -> Option<u64> {
    if !word(what, "unit") && !word(what, "container") {
        return None;
    }
    let lower = detail.trim().to_ascii_lowercase();
    let (state, rest) = lower.split_once(',')?;
    if !matches!(
        state.trim(),
        "active/running" | "active/waiting" | "active/exited"
    ) {
        return None;
    }
    let rest = rest.trim();
    let n = failures_24h_in(rest)?;
    (rest == format!("{n} failures in 24h")).then_some(n)
}

/// Largest failure count when every non-ok finding is [`active_failure_count`].
fn active_only(findings: &[&Value]) -> Option<u64> {
    if findings.is_empty() {
        return None;
    }
    let mut max_n = 0u64;
    for f in findings {
        max_n = max_n.max(active_failure_count(
            st(f, "what").unwrap_or(""),
            st(f, "detail").unwrap_or(""),
        )?);
    }
    Some(max_n)
}

fn bad_finding(f: &Value) -> bool {
    !matches!(st(f, "level"), Some("ok" | "idle"))
}

fn finding_line(f: &Value) -> String {
    let what = st(f, "what").unwrap_or("");
    match st(f, "detail") {
        Some(d) if !what.is_empty() => redact(&format!("{what}: {d}")),
        Some(d) => redact(d),
        None => redact(what),
    }
}

/// A unit name without `user:`/`system:` and `.service`/`.timer`.
fn unit_key(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("user:")
        .or_else(|| s.strip_prefix("system:"))
        .unwrap_or(s);
    s.strip_suffix(".service")
        .or_else(|| s.strip_suffix(".timer"))
        .unwrap_or(s)
}

// -- the catalog -----------------------------------------------------------------------

/// The tree's components, in tree order.
fn tree_components(tree: Option<&Value>) -> Vec<&Value> {
    let mut out = Vec::new();
    for g in tree.map_or(&[][..], |t| arr(t, "groups")) {
        for p in arr(g, "projects") {
            out.extend(arr(p, "components"));
        }
    }
    out
}

/// A catalog problem: the status entry, completed from the tree's component.
#[derive(Clone, Copy)]
struct Problem<'a> {
    p: Option<&'a Value>,
    c: Option<&'a Value>,
}

impl<'a> Problem<'a> {
    fn get(&self, k: &str) -> Option<&'a Value> {
        [self.p, self.c, self.c.and_then(|c| c.get("status"))]
            .into_iter()
            .flatten()
            .filter_map(|v| v.get(k))
            .find(|v| !v.is_null() && v.as_str() != Some(""))
    }
    fn str(&self, k: &str) -> Option<&'a str> {
        self.get(k)?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
    fn list(&self, k: &str) -> &'a [Value] {
        self.get(k)
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }
    fn id(&self) -> &'a str {
        self.str("id").unwrap_or("")
    }
    fn effective(&self) -> &'a str {
        self.str("effective").unwrap_or("unknown")
    }
}

/// One attention item, before JSON.
#[derive(Debug, Clone)]
struct Item {
    key: String,
    sources: Vec<&'static str>,
    severity: &'static str,
    title: String,
    component_id: Option<String>,
    host: Option<String>,
    group: Option<String>,
    project: Option<String>,
    kind: Option<String>,
    criticality: Option<String>,
    summary: String,
    details: Vec<String>,
    since: Option<DateTime<Utc>>,
    failures_24h: Option<u64>,
    /// Largest "N failures in 24h" when every non-ok catalog finding is only that
    /// on a unit or container that is up now. Not serialized; decides `flapping`.
    active_failures: Option<u64>,
    flapping: bool,
    impact: Option<Value>,
    caused: Vec<String>,
    logs: Option<String>,
    restart: Option<String>,
    runbook: Option<String>,
}

impl Item {
    fn json(&self) -> Value {
        json!({
            "key": self.key,
            "sources": self.sources,
            "severity": self.severity,
            "flapping": self.flapping,
            "title": self.title,
            "component_id": self.component_id,
            "host": self.host,
            "group": self.group.as_deref().map(redact),
            "project": self.project,
            "kind": self.kind,
            "criticality": self.criticality,
            "summary": redact(&self.summary),
            "details": self.details,
            "since": time_v(self.since),
            "failures_24h": self.failures_24h,
            "impact": self.impact,
            "caused": self.caused,
            "logs": self.logs,
            "restart": self.restart,
            "runbook": self.runbook,
        })
    }
}

/// `{id, name}` for an impact entry (a string id or an object).
fn impact_entry(v: &Value, names: &HashMap<&str, &str>) -> Option<Value> {
    let id = v.as_str().or_else(|| st(v, "id"))?;
    let name = st(v, "name")
        .or_else(|| names.get(id).copied())
        .unwrap_or(id);
    Some(json!({"id": id, "name": redact(name)}))
}

/// A DNS hostname. Drops URLs (`http://:8801`, any scheme with an empty host),
/// paths, and names with no dot.
fn public_hostname(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() || s.to_ascii_lowercase().contains("://") {
        return false;
    }
    if s.contains(['/', '?', '#', '@', ':', ' ']) || !s.contains('.') {
        return false;
    }
    let s = s.strip_suffix('.').unwrap_or(s);
    !s.is_empty()
        && !s.starts_with('.')
        && s.split('.').all(|label| {
            (1..64).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn impact_of(v: Option<&Value>, names: &HashMap<&str, &str>) -> Option<Value> {
    let v = v.filter(|v| v.is_object())?;
    let list = |k: &str| -> Vec<Value> {
        arr(v, k)
            .iter()
            .filter_map(|e| impact_entry(e, names))
            .collect()
    };
    let public: Vec<Value> = arr(v, "public_hostnames_broken")
        .iter()
        .filter_map(|h| h.as_str().filter(|h| public_hostname(h)).map(|h| json!(redact(h))))
        .collect();
    Some(
        json!({"broken": list("broken"), "degraded": list("degraded"), "public_hostnames_broken": public}),
    )
}

fn add_impact(item: &mut Item, which: &str, id: &str, name: &str) {
    let imp = item.impact.get_or_insert_with(
        || json!({"broken": [], "degraded": [], "public_hostnames_broken": []}),
    );
    let Some(list) = imp.get_mut(which).and_then(Value::as_array_mut) else {
        return;
    };
    if !list.iter().any(|e| e["id"] == id) {
        list.push(json!({"id": id, "name": redact(name)}));
    }
}

fn catalog_item(p: Problem, names: &HashMap<&str, &str>) -> Item {
    let id = p.id();
    let effective = p.effective();
    let findings: Vec<&Value> = p
        .list("findings")
        .iter()
        .filter(|f| bad_finding(f))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    for f in &findings {
        let s = plain(st(f, "what").unwrap_or(""), st(f, "detail").unwrap_or(""));
        if !s.is_empty() && !parts.contains(&s) {
            parts.push(s);
        }
    }
    let by: Vec<&str> = p
        .list("impacted_by")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let summary = if !parts.is_empty() {
        keep_worse(parts).into_iter().take(2).collect::<Vec<_>>().join("; ")
    } else if !by.is_empty() {
        let how = if state_of(effective) == "crit" {
            "Broken"
        } else {
            "Degraded"
        };
        shorten(&format!("{how} because of {}", by.join(", ")), MAX_LINE)
    } else if state_of(effective) == "crit" {
        "Down".into()
    } else {
        "Warning".into()
    };
    Item {
        key: format!("catalog:{id}"),
        sources: vec!["catalog"],
        severity: if state_of(effective) == "crit" {
            "crit"
        } else {
            "warn"
        },
        title: redact(p.str("name").unwrap_or(id)),
        component_id: Some(id.to_owned()),
        host: p.str("host").map(str::to_owned),
        group: p.str("group").map(str::to_owned),
        project: p.str("project").map(str::to_owned),
        kind: p.str("kind").map(str::to_owned),
        criticality: p.str("criticality").map(str::to_owned),
        summary,
        details: findings.iter().map(|f| finding_line(f)).collect(),
        since: p.str("since").and_then(parse_time),
        failures_24h: p.get("failures_24h").and_then(Value::as_u64),
        active_failures: active_only(&findings),
        flapping: false,
        impact: impact_of(p.get("impact"), names),
        caused: Vec::new(),
        logs: p.str("logs").map(redact),
        restart: p.str("restart").map(redact),
        runbook: p.str("runbook").map(redact),
    }
}

// -- the sentinel ----------------------------------------------------------------------

struct SentinelHost {
    host: String,
    received_at: Option<DateTime<Utc>>,
    stale: bool,
    snapshot: Option<Snapshot>,
}

fn sentinel_hosts(v: &Value) -> Vec<SentinelHost> {
    arr(v, "hosts")
        .iter()
        .filter_map(|h| {
            Some(SentinelHost {
                host: st(h, "host")?.to_owned(),
                received_at: st(h, "received_at").and_then(parse_time),
                stale: h.get("stale").and_then(Value::as_bool).unwrap_or(false),
                snapshot: h
                    .get("snapshot")
                    .and_then(|s| serde_json::from_value(s.clone()).ok()),
            })
        })
        .collect()
}

/// The catalog component a sentinel check is about: one of its units or containers,
/// on the same host when the component names one.
fn matching<'a>(comps: &[&'a Value], host: &str, name: &str) -> Option<&'a Value> {
    let key = unit_key(name);
    comps.iter().copied().find(|c| {
        st(c, "host").is_none_or(|h| h == host)
            && (arr(c, "units")
                .iter()
                .filter_map(Value::as_str)
                .any(|u| unit_key(u) == key)
                || arr(c, "containers")
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|x| x == name))
    })
}

// -- hosts -----------------------------------------------------------------------------

fn host_name(id: &str) -> String {
    match id {
        "oracle-peer1" => "peer1 / p2".into(),
        _ => id.to_owned(),
    }
}

/// The text in parentheses of a host component's name.
fn role_of(name: &str) -> Option<String> {
    let open = name.find('(')?;
    let close = name[open..].find(')')? + open;
    Some(name[open + 1..close].trim().to_owned()).filter(|s| !s.is_empty())
}

fn load_of(v: Option<&Value>) -> Option<Vec<f64>> {
    let v = v?;
    let nums: Vec<f64> = match v {
        Value::Array(a) => a.iter().filter_map(Value::as_f64).collect(),
        Value::String(s) => s
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter_map(|x| x.parse().ok())
            .collect(),
        _ => return None,
    };
    (nums.len() >= 3).then(|| nums[..3].to_vec())
}

fn uptime_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => {
            let secs = n.as_u64()?;
            let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
            Some(if d > 0 {
                format!("{d}d {h}h")
            } else if h > 0 {
                format!("{h}h {m}m")
            } else {
                format!("{m}m")
            })
        }
        _ => None,
    }
}

fn disks_of(v: Option<&Value>) -> Option<Vec<Value>> {
    let a = v?.as_array()?;
    Some(
        a.iter()
            .filter_map(|d| {
                let mount = st(d, "mount").or_else(|| st(d, "target"))?;
                Some(json!({
                    "mount": redact(mount),
                    "used_pct": num(d, "used_pct").or_else(|| num(d, "use_pct")),
                    "free_gb": num(d, "free_gb").or_else(|| num(d, "avail_gb")),
                    "size_gb": num(d, "size_gb"),
                }))
            })
            .collect(),
    )
}

fn history_of(v: Option<&Value>) -> Value {
    let Some(h) = v.filter(|h| h.is_object()) else {
        return Value::Null;
    };
    let series = |k: &str| -> Value {
        h.get(k).and_then(Value::as_array).map_or(Value::Null, |a| {
            Value::Array(
                a.iter()
                    .map(|x| x.as_f64().map_or(Value::Null, |f| json!(f)))
                    .collect(),
            )
        })
    };
    json!({
        "step_s": h.get("step_s").and_then(Value::as_u64).unwrap_or(900),
        "end": st(h, "end"),
        "tctl": series("tctl"),
        "load1": series("load1"),
        "mem": series("mem"),
    })
}

struct HostOut {
    json: Value,
    state: &'static str,
    optional: bool,
}

fn host(
    id: &str,
    cat: Option<&Value>,
    comp: Option<&Value>,
    sentinel: Option<&SentinelHost>,
    catalog_at: Option<DateTime<Utc>>,
) -> HostOut {
    let status = comp.and_then(|c| c.get("status")).filter(|s| s.is_object());
    let effective = status.and_then(|s| st(s, "effective")).unwrap_or("unknown");
    let optional = comp
        .and_then(|c| c.get("optional"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let has_status = status.is_some() && effective != "unknown";
    let snap = sentinel.and_then(|s| s.snapshot.as_ref());
    let m = snap.map(|s| &s.metrics);

    let reachable = cat
        .and_then(|h| h.get("reachable"))
        .and_then(Value::as_bool)
        .or_else(|| sentinel.map(|s| !s.stale))
        .or(match state_of(effective) {
            "ok" | "warn" => Some(true),
            "crit" => Some(false),
            _ => None,
        });
    let limited = cat.is_none() && has_status;

    let mut last: Option<DateTime<Utc>> = None;
    if cat.is_some() || has_status {
        let scanned = cat
            .and_then(|h| {
                st(h, "at")
                    .or_else(|| st(h, "scanned_at"))
                    .or_else(|| st(h, "taken_at"))
            })
            .and_then(parse_time);
        last = catalog_at.or(scanned);
    }
    if let Some(r) = sentinel.and_then(|s| s.received_at) {
        last = Some(last.map_or(r, |l| l.max(r)));
    }

    let load = load_of(cat.and_then(|h| h.get("load"))).or_else(|| m.map(|m| m.load.to_vec()));
    let mem = cat
        .and_then(|h| num(h, "mem_used_pct"))
        .or_else(|| m.map(|m| m.mem_used_pct));
    let tctl = cat
        .and_then(|h| num(h, "tctl"))
        .or_else(|| m.and_then(|m| m.cpu_temp_c));
    let disks = disks_of(cat.and_then(|h| h.get("disks"))).filter(|d| !d.is_empty()).or_else(|| {
        m.map(|m| m.disks.iter().map(|d| json!({"mount": redact(&d.mount), "used_pct": d.used_pct, "free_gb": d.free_gb, "size_gb": null})).collect())
    });
    let has_metrics = load.is_some() || mem.is_some() || tctl.is_some() || disks.is_some();

    let state = if reachable == Some(false) {
        if optional {
            "unknown"
        } else {
            "crit"
        }
    } else if state_of(effective) == "warn" {
        "warn"
    } else if reachable.is_none() && !has_status && !has_metrics {
        "unknown"
    } else {
        "ok"
    };
    let findings: Vec<String> = status
        .map_or(&[][..], |s| arr(s, "findings"))
        .iter()
        .filter(|f| bad_finding(f))
        .filter_map(|f| st(f, "detail").or_else(|| st(f, "what")).map(redact))
        .collect();
    let json = json!({
        "id": id,
        "name": host_name(id),
        "role": comp.and_then(|c| st(c, "name")).and_then(role_of).map(|r| redact(&r)),
        "state": state,
        "reachable": reachable,
        "limited": limited,
        "last_report": time_v(last),
        "uptime": uptime_of(cat.and_then(|h| h.get("uptime"))).map(|u| redact(&u)),
        "os": cat.and_then(|h| st(h, "os")).map(redact),
        "kernel": cat.and_then(|h| st(h, "kernel")).map(redact),
        "cpus": cat.and_then(|h| h.get("cpus")).and_then(Value::as_u64),
        "load": load,
        "mem_used_pct": mem,
        "tctl": tctl,
        "disks": disks.unwrap_or_default(),
        "history": history_of(cat.and_then(|h| h.get("history"))),
        "findings": findings,
        "component_id": comp.and_then(|c| st(c, "id")),
    });
    HostOut {
        json,
        state,
        optional,
    }
}

// -- the merge -------------------------------------------------------------------------

fn source(ok: bool, at: Option<DateTime<Utc>>, error: Option<String>) -> Value {
    json!({"ok": ok, "at": time_v(at), "error": error.map(|e| shorten(&redact(&e), 200))})
}

/// The whole response from the sources' answers at `now` (pure; module docs).
pub fn build(
    sentinel: &Value,
    catalog_status: &Result<Value, String>,
    catalog_tree: &Result<Value, String>,
    ops: &Result<Vec<Value>, String>,
    now: DateTime<Utc>,
) -> Value {
    let status = catalog_status.as_ref().ok();
    let tree = catalog_tree.as_ref().ok();
    let comps = tree_components(tree);
    let by_id: HashMap<&str, &Value> = comps
        .iter()
        .filter_map(|c| Some((st(c, "id")?, *c)))
        .collect();
    let names: HashMap<&str, &str> = comps
        .iter()
        .filter_map(|c| Some((st(c, "id")?, st(c, "name").or_else(|| st(c, "id"))?)))
        .collect();
    let sentinels = sentinel_hosts(sentinel);
    let catalog_at = status
        .and_then(|s| st(s, "generated_at"))
        .or_else(|| tree.and_then(|t| st(t, "generated_at")))
        .and_then(parse_time);

    // Catalog problems: the status's, else the tree's unhealthy components.
    let problems: Vec<Problem> = match status {
        Some(s) => arr(s, "problems")
            .iter()
            .map(|p| Problem {
                p: Some(p),
                c: st(p, "id").and_then(|id| by_id.get(id).copied()),
            })
            .collect(),
        None => comps
            .iter()
            .map(|c| Problem {
                p: None,
                c: Some(c),
            })
            .collect(),
    };
    let listed: Vec<Problem> = problems
        .into_iter()
        .filter(|p| {
            matches!(state_of(p.effective()), "crit" | "warn")
                && !p.id().is_empty()
                && !(p.str("kind") == Some("host")
                    && state_of(p.effective()) == "crit"
                    && p.get("optional").and_then(Value::as_bool) == Some(true))
        })
        .collect();
    let independently_bad: HashSet<&str> = sentinels
        .iter()
        .flat_map(|h| {
            h.snapshot
                .iter()
                .flat_map(|s| &s.checks)
                .filter(|c| c.state != State3::Ok)
                .filter_map(|c| matching(&comps, &h.host, &c.name).and_then(|c| st(c, "id")))
        })
        .collect();
    let listed_ids: HashSet<&str> = listed.iter().map(Problem::id).collect();
    // Broken or degraded only because of other listed components.
    let folded = |p: &Problem| -> bool {
        let by = p.list("impacted_by");
        matches!(p.effective(), "broken" | "degraded")
            && !matches!(p.str("level"), Some("warn" | "down" | "crit"))
            && !independently_bad.contains(p.id())
            && !by.is_empty()
            && by.iter().all(|b| {
                b.as_str()
                    .is_some_and(|b| listed_ids.contains(b) && b != p.id())
            })
    };
    let mut folded_ids: HashSet<&str> = listed
        .iter()
        .filter(|p| folded(p))
        .map(Problem::id)
        .collect();
    let dependencies: HashMap<&str, Problem> = listed.iter().map(|p| (p.id(), *p)).collect();
    let has_root = |id: &str| {
        let mut seen = HashSet::new();
        let mut todo = vec![id];
        while let Some(id) = todo.pop() {
            if !seen.insert(id) {
                continue;
            }
            if !folded_ids.contains(id) {
                return true;
            }
            todo.extend(
                dependencies[id]
                    .list("impacted_by")
                    .iter()
                    .filter_map(Value::as_str),
            );
        }
        false
    };
    let rootless: Vec<&str> = folded_ids
        .iter()
        .copied()
        .filter(|id| !has_root(id))
        .collect();
    for id in rootless {
        folded_ids.remove(id);
    }
    let mut items: Vec<Item> = listed
        .iter()
        .filter(|p| !folded_ids.contains(p.id()))
        .map(|p| catalog_item(*p, &names))
        .collect();
    let mut index: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(n, i)| (i.key.clone(), n))
        .collect();
    let problem_of: HashMap<&str, Problem> = listed.iter().map(|p| (p.id(), *p)).collect();
    for p in listed.iter().filter(|p| folded_ids.contains(p.id())) {
        // The causes that are items of their own (through folded ones, too).
        let mut roots: BTreeSet<&str> = BTreeSet::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo: Vec<&str> = p
            .list("impacted_by")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        while let Some(c) = todo.pop() {
            if !seen.insert(c) {
                continue;
            }
            if folded_ids.contains(c) {
                todo.extend(
                    problem_of[c]
                        .list("impacted_by")
                        .iter()
                        .filter_map(Value::as_str),
                );
            } else {
                roots.insert(c);
            }
        }
        let which = if p.effective() == "broken" {
            "broken"
        } else {
            "degraded"
        };
        for r in roots {
            if let Some(&n) = index.get(&format!("catalog:{r}")) {
                let it = &mut items[n];
                if !it.caused.iter().any(|x| x == p.id()) {
                    it.caused.push(p.id().to_owned());
                }
                add_impact(
                    it,
                    which,
                    p.id(),
                    names.get(p.id()).copied().unwrap_or(p.id()),
                );
            }
        }
    }

    // Sentinel checks: merged into the component they are about, else their own.
    let (mut sentinel_ok, mut sentinel_total) = (0u64, 0u64);
    for h in &sentinels {
        let Some(snap) = &h.snapshot else { continue };
        for c in &snap.checks {
            let comp = matching(&comps, &h.host, &c.name);
            if comp.is_none() {
                sentinel_total += 1;
                sentinel_ok += u64::from(c.state == State3::Ok);
            }
            if c.state == State3::Ok {
                continue;
            }
            let severity = s3(c.state);
            let since = parse_time(&c.since);
            let line = redact(&format!(
                "sentinel: {}",
                if c.detail.trim().is_empty() {
                    s3(c.state)
                } else {
                    c.detail.trim()
                }
            ));
            let summary = plain(&c.name, &c.detail);
            let comp_id = comp.and_then(|x| st(x, "id"));
            if comp_id.is_some_and(|id| folded_ids.contains(id)) {
                continue;
            }
            let key = match comp_id {
                Some(id) => format!("catalog:{id}"),
                None => format!("sentinel:{}:{}", h.host, c.id),
            };
            if let Some(&n) = index.get(&key) {
                let it = &mut items[n];
                if !it.sources.contains(&"sentinel") {
                    it.sources.push("sentinel");
                }
                if severity_rank(severity) > severity_rank(it.severity) {
                    it.severity = severity;
                }
                if !it.details.contains(&line) {
                    it.details.push(line);
                }
                it.since = match (it.since, since) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                continue;
            }
            let kind =
                lower_name(&c.kind).map(|k| if k == "unit" { "service".to_owned() } else { k });
            let item = match comp.map(|x| Problem {
                p: None,
                c: Some(x),
            }) {
                Some(p) => Item {
                    sources: vec!["catalog", "sentinel"],
                    severity,
                    summary,
                    details: vec![line],
                    since,
                    failures_24h: None,
                    impact: None,
                    caused: Vec::new(),
                    key: key.clone(),
                    ..catalog_item(p, &names)
                },
                None => Item {
                    key: key.clone(),
                    sources: vec!["sentinel"],
                    severity,
                    title: redact(&c.name),
                    component_id: None,
                    host: Some(h.host.clone()),
                    group: lower_name(&c.group),
                    project: None,
                    kind,
                    criticality: None,
                    summary,
                    details: vec![line],
                    since,
                    failures_24h: None,
                    active_failures: None,
                    flapping: false,
                    impact: None,
                    caused: Vec::new(),
                    logs: None,
                    restart: None,
                    runbook: None,
                },
            };
            index.insert(key, items.len());
            items.push(item);
        }
    }
    for it in &mut items {
        if it.severity != "warn" || it.sources.contains(&"sentinel") {
            continue;
        }
        let Some(n) = it.active_failures else {
            continue;
        };
        it.flapping = true;
        it.summary = format!("Running; failed {n} times in 24 h");
    }
    items.sort_by(|a, b| {
        a.flapping.cmp(&b.flapping).then_with(|| {
            let rank = if a.flapping {
                b.failures_24h.unwrap_or(0).cmp(&a.failures_24h.unwrap_or(0))
            } else {
                severity_rank(b.severity).cmp(&severity_rank(a.severity))
            };
            rank.then_with(|| match (a.since, b.since) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.key.cmp(&b.key))
        })
    });

    // Hosts.
    let cat_hosts: Option<&Map<String, Value>> = status
        .and_then(|s| s.get("hosts"))
        .or_else(|| tree.and_then(|t| t.get("hosts")))
        .and_then(Value::as_object);
    let host_comps: HashMap<&str, &Value> = comps
        .iter()
        .filter(|c| st(c, "kind") == Some("host"))
        .filter_map(|c| Some((st(c, "id")?, *c)))
        .collect();
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    ids.extend(
        cat_hosts
            .into_iter()
            .flat_map(|m| m.keys().map(String::as_str)),
    );
    ids.extend(host_comps.keys().copied());
    ids.extend(sentinels.iter().map(|s| s.host.as_str()));
    let mut ordered: Vec<&str> = HOST_ORDER
        .iter()
        .copied()
        .filter(|h| ids.contains(h))
        .collect();
    ordered.extend(ids.iter().copied().filter(|h| !HOST_ORDER.contains(h)));
    let hosts: Vec<HostOut> = ordered
        .iter()
        .map(|id| {
            host(
                id,
                cat_hosts.and_then(|m| m.get(*id)).filter(|v| v.is_object()),
                host_comps.get(id).copied(),
                sentinels.iter().find(|s| s.host == *id),
                catalog_at,
            )
        })
        .collect();

    // Services: the tree, trimmed.
    let services: Vec<Value> = tree
        .map_or(&[][..], |t| arr(t, "groups"))
        .iter()
        .map(|g| {
            let projects: Vec<Value> = arr(g, "projects")
                .iter()
                .map(|p| {
                    let components: Vec<Value> = arr(p, "components")
                        .iter()
                        .map(|c| {
                            let eff = c.get("status").and_then(|s| st(s, "effective")).unwrap_or("unknown");
                            json!({
                                "id": st(c, "id"),
                                "name": text(st(c, "name").or_else(|| st(c, "id"))),
                                "kind": st(c, "kind"),
                                "host": st(c, "host"),
                                "state": state_of(eff),
                                "criticality": st(c, "criticality"),
                                "description": text(st(c, "description")),
                            })
                        })
                        .collect();
                    json!({"id": st(p, "id"), "state": state_of(st(p, "level").unwrap_or("unknown")), "components": components})
                })
                .collect();
            json!({"name": text(st(g, "name")), "state": state_of(st(g, "level").unwrap_or("unknown")), "projects": projects})
        })
        .collect();

    // Timeline: ops records and sentinel changes, newest first.
    let mut timeline: Vec<(DateTime<Utc>, Value)> = Vec::new();
    let mut seen_ops: HashSet<&str> = HashSet::new();
    for r in ops.as_ref().map_or(&[][..], Vec::as_slice) {
        let Some(at) = st(r, "at").and_then(parse_time) else {
            continue;
        };
        let kind = st(r, "kind").unwrap_or("");
        if !OPS_KINDS.contains(&kind) || st(r, "id").is_some_and(|id| !seen_ops.insert(id)) {
            continue;
        }
        let detail = st(r, "body")
            .and_then(|b| b.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(|l| shorten(&redact(l), MAX_LINE));
        timeline.push((
            at,
            json!({
                "at": rfc(at),
                "kind": kind,
                "title": text(st(r, "title")),
                "project": st(r, "project"),
                "host": st(r, "host"),
                "severity": if kind == "incident" { st(r, "severity") } else { None },
                "from": null, "to": null,
                "detail": detail,
                "ops_id": st(r, "id"),
            }),
        ));
    }
    let events: Vec<Event> = arr(sentinel, "events")
        .iter()
        .filter_map(|e| serde_json::from_value(e.clone()).ok())
        .collect();
    for e in &events {
        timeline.push((
            e.at,
            json!({
                "at": rfc(e.at),
                "kind": "change",
                "title": redact(&e.name),
                "project": null,
                "host": e.host,
                "severity": null,
                "from": s3(e.from), "to": s3(e.to),
                "detail": Some(e.detail.trim()).filter(|d| !d.is_empty()).map(|d| shorten(&redact(d), MAX_LINE)),
                "ops_id": null,
            }),
        ));
    }
    timeline.sort_by_key(|a| std::cmp::Reverse(a.0));
    timeline.truncate(MAX_TIMELINE);

    // Checks: the catalog's components ok/idle, plus the sentinel's own.
    let counts = status
        .and_then(|s| s.get("counts"))
        .or_else(|| tree.and_then(|t| t.get("counts")))
        .and_then(Value::as_object);
    let (mut checks_ok, mut checks_total) = match counts {
        Some(m) => {
            let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
            let total = m
                .get("total")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| m.values().filter_map(Value::as_u64).sum());
            (n("ok") + n("idle"), total)
        }
        None => {
            let ok = comps
                .iter()
                .filter(|c| {
                    matches!(
                        c.get("status").and_then(|s| st(s, "effective")),
                        Some("ok" | "idle")
                    )
                })
                .count();
            (ok as u64, comps.len() as u64)
        }
    };
    checks_ok += sentinel_ok;
    checks_total += sentinel_total;

    let worst_host = hosts
        .iter()
        .filter(|h| !h.optional || h.state != "crit")
        .map(|h| severity_rank(h.state))
        .max()
        .unwrap_or(0);
    let worst_item = items
        .iter()
        .filter(|i| !i.flapping)
        .map(|i| severity_rank(i.severity))
        .max()
        .unwrap_or(0);
    let overall = match worst_host.max(worst_item) {
        2 => "crit",
        1 => "warn",
        _ => "ok",
    };

    let catalog_error = match (catalog_status, catalog_tree) {
        (Ok(_), Ok(_)) => None,
        (Err(a), Err(b)) if a == b => Some(a.clone()),
        (Err(a), Err(b)) => Some(format!("status: {a}; tree: {b}")),
        (Err(a), Ok(_)) => Some(format!("status: {a}")),
        (Ok(_), Err(b)) => Some(format!("tree: {b}")),
    };
    let sentinel_at = sentinels.iter().filter_map(|s| s.received_at).max();
    let sources = json!({
        "catalog": source(catalog_error.is_none(), catalog_at, catalog_error),
        "sentinel": source(!sentinels.is_empty(), sentinel_at, sentinels.is_empty().then(|| "no snapshot yet".to_owned())),
        "ops": source(ops.is_ok(), ops.is_ok().then_some(now), ops.as_ref().err().cloned()),
    });

    json!({
        "generated_at": rfc(now),
        "overall": overall,
        "attention_count": items.iter().filter(|i| !i.flapping).count(),
        "flapping_count": items.iter().filter(|i| i.flapping).count(),
        "checks_ok": checks_ok,
        "checks_total": checks_total,
        "sources": sources,
        "hosts": hosts.into_iter().map(|h| h.json).collect::<Vec<_>>(),
        "attention": items.iter().map(Item::json).collect::<Vec<_>>(),
        "services": services,
        "timeline": timeline.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
    })
}

/// Every source failed (such a response is never cached).
pub fn all_failed(v: &Value) -> bool {
    v["sources"]
        .as_object()
        .is_some_and(|m| m.values().all(|s| s["ok"] == false))
}

// -- fetching --------------------------------------------------------------------------

async fn catalog(st: &AppState, path: &str, origin: &str) -> Result<Value, String> {
    let agents = st.agents.as_ref().ok_or("Lucina is not configured")?;
    let v = agents.get_json(path, origin, CATALOG_TIMEOUT).await?;
    if !v.is_object() {
        return Err("agentd sent no catalog object".into());
    }
    Ok(v)
}

fn ops_error(e: &secretary_core::Error) -> String {
    match e {
        secretary_core::Error::Unreachable(_) => "Lucina unreachable".into(),
        secretary_core::Error::Timeout => "Lucina did not answer in time".into(),
        secretary_core::Error::Api { status, code, .. } => {
            format!("Lucina answered {status}: {code}")
        }
        _ => "unexpected answer from Lucina".into(),
    }
}

async fn ops_records(st: &AppState, now: DateTime<Utc>) -> Result<Vec<Value>, String> {
    let lucina = st.lucina.as_ref().ok_or("Lucina is not configured")?;
    let since = (now - Duration::days(OPS_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let one = |kind: &'static str| {
        let since = since.clone();
        async move {
            let call =
                Call::ops_search(None, None, Some(kind), Some(&since), None, Some(OPS_LIMIT))
                    .map_err(|e| ops_error(&e))?
                    .with_caller("system-overview");
            let v: Value = lucina.call(&call).await.map_err(|e| ops_error(&e))?;
            v.get("items")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| "Lucina sent no ops items".to_owned())
        }
    };
    let all = async { futures_util::future::try_join_all(OPS_KINDS.map(one)).await };
    match tokio::time::timeout(OPS_TIMEOUT, all).await {
        Ok(r) => r.map(|v| v.into_iter().flatten().collect()),
        Err(_) => Err("Lucina did not answer in time".into()),
    }
}

/// Fetch every source and build the response.
pub async fn fetch(st: &AppState, device_id: &str, now: DateTime<Utc>) -> Value {
    let origin = format!("device:{device_id}");
    let sentinel = st.system_health.view(now);
    let (status, tree, ops) = tokio::join!(
        catalog(st, "/catalog/status", &origin),
        catalog(st, "/catalog", &origin),
        ops_records(st, now)
    );
    build(&sentinel, &status, &tree, &ops, now)
}

#[derive(Deserialize, Default)]
struct OverviewQuery {
    fresh: Option<String>,
}

async fn get_overview(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    QueryArgs(q): QueryArgs<OverviewQuery>,
) -> Json<Value> {
    let asked = Instant::now();
    let fresh = matches!(q.fresh.as_deref(), Some("1" | "true"));
    // Held while fetching: callers that arrive meanwhile share this fetch.
    let mut cache = st.system_overview.lock().await;
    if let Some((at, v)) = cache.as_ref() {
        // A fetch that finished after this request arrived is fresh enough for `?fresh=1`.
        if *at >= asked || (!fresh && at.elapsed() < CACHE_FOR) {
            return Json(v.clone());
        }
    }
    let v = fetch(&st, &ident.device_id, Utc::now()).await;
    if all_failed(&v) {
        *cache = None;
    } else {
        *cache = Some((Instant::now(), v.clone()));
    }
    Json(v)
}

/// Behind `require_device` (the apps).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/system/overview", get(get_overview))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        parse_time("2026-10-28T21:50:00Z").unwrap()
    }

    fn comp(id: &str, kind: &str, host: &str, effective: &str, extra: Value) -> Value {
        let mut c = json!({
            "id": id, "name": format!("{id} name"), "kind": kind, "group": "Lucina", "project": "secretary",
            "host": host, "criticality": "high", "units": [], "containers": [],
            "status": {"level": effective, "effective": effective, "since": "2026-10-28T19:00:00Z", "impacted_by": [], "findings": []},
        });
        for (k, v) in extra.as_object().unwrap() {
            if k == "status" {
                for (sk, sv) in v.as_object().unwrap() {
                    c["status"][sk] = sv.clone();
                }
            } else {
                c[k] = v.clone();
            }
        }
        c
    }

    fn tree(components: Vec<Value>) -> Value {
        json!({"generated_at": "2026-10-28T21:45:00Z", "counts": {"ok": 3, "idle": 1, "down": 1, "broken": 1},
               "groups": [{"name": "Lucina", "level": "down", "projects": [{"id": "secretary", "level": "down", "components": components}]}]})
    }

    fn sentinel(checks: Value) -> Value {
        json!({"hosts": [{"host": "nezopt", "received_at": "2026-10-28T21:48:00Z", "stale": false, "snapshot": {
            "host": "nezopt", "at": "2026-10-28T21:48:00Z", "overall": "crit",
            "metrics": {"disks": [{"mount": "/", "used_pct": 77.6, "free_gb": 422.0}], "cpu_temp_c": 71.0, "load": [1.0, 2.0, 3.0], "mem_used_pct": 33.3},
            "checks": checks}}],
            "events": [{"at": "2026-10-28T20:00:00Z", "host": "nezopt", "check_id": "unit:gmaps-server", "name": "gmaps-server", "from": "ok", "to": "crit", "detail": "inactive (dead)"}]})
    }

    fn check(name: &str, state: &str, detail: &str) -> Value {
        json!({"id": format!("unit:{name}"), "kind": "unit", "name": name, "group": "Lucina", "state": state, "detail": detail, "since": "2026-10-28T18:00:00Z"})
    }

    fn fixture() -> (Value, Value, Value) {
        let gmaps = comp(
            "gmaps-server",
            "service",
            "nezopt",
            "down",
            json!({
            "units": ["gmaps-server.service"], "logs": "journalctl --user -u gmaps-server -n 100 --no-pager",
            "restart": "systemctl --user restart gmaps-server",
            "status": {"since": "2026-10-28T19:16:04Z", "failures_24h": 142, "findings": [
                {"level": "down", "what": "user unit gmaps-server.service", "detail": "inactive/dead"},
                {"level": "down", "what": "port 18210", "detail": "not listening"},
                {"level": "ok", "what": "x", "detail": "fine"}]}}),
        );
        let maps = comp(
            "lucina-maps",
            "site",
            "nezopt",
            "broken",
            json!({"status": {"level": "ok", "impacted_by": ["gmaps-server"]}}),
        );
        let hot = comp(
            "nezopt",
            "host",
            "",
            "warn",
            json!({"name": "nezopt (home server)", "host": null,
            "status": {"findings": [{"level": "warn", "what": "reachable", "detail": "up 3 days, load 9 5 2, Tctl 95 °C"}]}}),
        );
        let mac = comp(
            "mac",
            "host",
            "",
            "idle",
            json!({"name": "Mac (MacBook Air M4)", "host": null, "optional": true}),
        );
        let nespc = comp(
            "nespc",
            "host",
            "",
            "ok",
            json!({"name": "nespc (Linux Mint laptop)", "host": null, "optional": true}),
        );
        let peer = comp(
            "oracle-peer1",
            "host",
            "",
            "down",
            json!({"name": "oracle-peer1 / p2 (Oracle VM)", "host": null}),
        );
        let sync = comp(
            "rails-sync",
            "timer",
            "nezopt",
            "ok",
            json!({"units": ["user:marketplace-rails-sync.timer"]}),
        );
        let t = tree(vec![gmaps, maps, hot, mac, nespc, peer, sync]);
        let status = json!({"generated_at": "2026-10-28T21:45:00Z",
            "hosts": {"nezopt": {"reachable": true, "uptime": "23d 4h", "load": "9.58 5.03 2.48", "tctl": 95.1, "cpus": 16,
                                 "os": "Ubuntu 26.04.1 LTS", "disks": [{"mount": "/", "used_pct": 77.6, "free_gb": 422.0, "size_gb": 1885.0}]},
                      "mac": {"reachable": false, "error": "timeout"},
                      "oracle-peer1": {"reachable": false, "error": "ssh: connect timed out"},
                      "zz-other": {"reachable": true}},
            "problems": [
                {"id": "lucina-maps", "effective": "broken", "level": "ok", "impacted_by": ["gmaps-server"], "findings": []},
                {"id": "gmaps-server", "effective": "down", "level": "down", "criticality": "medium",
                 "impact": {"broken": [{"id": "lucina-maps"}], "degraded": [], "public_hostnames_broken": ["maps.example.com"]}},
                {"id": "nezopt", "effective": "warn", "level": "warn"},
                {"id": "oracle-peer1", "effective": "down", "level": "down", "findings": [{"level": "down", "what": "unreachable", "detail": "ssh: connect timed out"}]}]});
        let s = sentinel(json!([
            check("gmaps-server", "crit", "inactive (dead)"),
            check("marketplace-rails-sync", "warn", "failed (exit-code)"),
            check("lone-worker", "warn", "active/running, 4 failures in 24h"),
            check("honcho", "ok", "active"),
        ]));
        (s, status, t)
    }

    fn built() -> Value {
        let (s, status, t) = fixture();
        let ops = vec![
            json!({"id": "e8e10d40c994d9f0", "kind": "incident", "project": "nezopt", "title": "Hot CPU", "body": "\nTctl 95 °C token=abc123\nmore",
                   "at": "2026-10-28T21:39:00+00:00", "severity": "high", "host": "nezopt"}),
            json!({"id": "a1", "kind": "deploy", "project": "secretary", "title": "deploy", "body": "", "at": "2026-10-27T10:00:00+00:00", "severity": null}),
            json!({"id": "a1", "kind": "deploy", "project": "secretary", "title": "deploy", "body": "", "at": "2026-10-27T10:00:00+00:00"}),
        ];
        build(&s, &Ok(status), &Ok(t), &Ok(ops), now())
    }

    #[test]
    fn attention_merges_dedupes_and_orders() {
        let v = built();
        let keys: Vec<&str> = v["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        // crit first (oldest since first), then warn (oldest first); lucina-maps is folded.
        assert_eq!(
            keys,
            [
                "catalog:gmaps-server",
                "catalog:oracle-peer1",
                "catalog:rails-sync",
                "sentinel:nezopt:unit:lone-worker",
                "catalog:nezopt"
            ]
        );
        assert_eq!(v["attention_count"], 5);
        let g = &v["attention"][0];
        assert_eq!(g["sources"], json!(["catalog", "sentinel"]));
        assert_eq!(g["severity"], "crit");
        assert_eq!(g["title"], "gmaps-server name");
        assert_eq!(g["summary"], "Not running; port 18210 not listening");
        assert_eq!(
            g["details"],
            json!([
                "user unit gmaps-server.service: inactive/dead",
                "port 18210: not listening",
                "sentinel: inactive (dead)"
            ])
        );
        // The sentinel's check began earlier than the catalog's own since.
        assert_eq!(g["since"], "2026-10-28T18:00:00Z");
        assert_eq!(g["failures_24h"], 142);
        assert_eq!(g["criticality"], "medium");
        assert_eq!(g["caused"], json!(["lucina-maps"]));
        assert_eq!(
            g["impact"]["broken"],
            json!([{"id": "lucina-maps", "name": "lucina-maps name"}])
        );
        assert_eq!(
            g["impact"]["public_hostnames_broken"],
            json!(["maps.example.com"])
        );
        assert_eq!(g["restart"], "systemctl --user restart gmaps-server");
        // A check on a catalog-healthy component takes the component's key and metadata.
        let r = &v["attention"][2];
        assert_eq!(r["sources"], json!(["catalog", "sentinel"]));
        assert_eq!(r["component_id"], "rails-sync");
        assert_eq!(r["summary"], "Failed");
        let lone = &v["attention"][3];
        assert_eq!(lone["summary"], "failed 4 times in 24 h");
        assert_eq!(lone["kind"], "service");
        assert_eq!(lone["group"], "Lucina");
        assert_eq!(lone["component_id"], Value::Null);
    }

    #[test]
    fn hosts_order_state_and_limits() {
        let v = built();
        let hosts = v["hosts"].as_array().unwrap();
        let ids: Vec<&str> = hosts.iter().map(|h| h["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["nezopt", "oracle-peer1", "nespc", "mac", "zz-other"]);
        let n = &hosts[0];
        assert_eq!(
            (
                n["state"].as_str(),
                n["role"].as_str(),
                n["limited"].as_bool()
            ),
            (Some("warn"), Some("home server"), Some(false))
        );
        assert_eq!(n["load"], json!([9.58, 5.03, 2.48]));
        assert_eq!(n["tctl"], 95.1);
        // Memory only from the sentinel.
        assert_eq!(n["mem_used_pct"], 33.3);
        assert_eq!(n["disks"][0]["size_gb"], 1885.0);
        assert_eq!(n["last_report"], "2026-10-28T21:48:00Z");
        assert_eq!(n["findings"], json!(["up 3 days, load 9 5 2, Tctl 95 °C"]));
        let p = &hosts[1];
        assert_eq!(
            (p["name"].as_str(), p["role"].as_str(), p["state"].as_str()),
            (Some("peer1 / p2"), Some("Oracle VM"), Some("crit"))
        );
        let nespc = &hosts[2];
        assert_eq!(
            (
                nespc["limited"].as_bool(),
                nespc["state"].as_str(),
                nespc["reachable"].as_bool()
            ),
            (Some(true), Some("ok"), Some(true))
        );
        assert_eq!(nespc["last_report"], "2026-10-28T21:45:00Z");
        let mac = &hosts[3];
        assert_eq!(
            (mac["state"].as_str(), mac["reachable"].as_bool()),
            (Some("unknown"), Some(false))
        );
        assert_eq!(hosts[4]["state"], "ok");
        assert_eq!(v["overall"], "crit");
    }

    #[test]
    fn optional_host_alone_is_not_crit() {
        let mac = comp(
            "mac",
            "host",
            "",
            "idle",
            json!({"name": "Mac (MacBook Air M4)", "host": null, "optional": true}),
        );
        let status = json!({"generated_at": "2026-10-28T21:45:00Z", "hosts": {"mac": {"reachable": false}}, "problems": []});
        let v = build(
            &json!({"hosts": [], "events": []}),
            &Ok(status),
            &Ok(tree(vec![mac])),
            &Ok(vec![]),
            now(),
        );
        assert_eq!(v["overall"], "ok");
        assert_eq!(v["hosts"][0]["state"], "unknown");
    }

    #[test]
    fn checks_timeline_services_and_sources() {
        let v = built();
        // Catalog 4 of 6, plus the unmatched sentinel checks lone-worker and honcho (one ok).
        assert_eq!(
            (v["checks_ok"].as_u64(), v["checks_total"].as_u64()),
            (Some(5), Some(8))
        );
        let tl = v["timeline"].as_array().unwrap();
        assert_eq!(tl.len(), 3, "the duplicate deploy is dropped");
        assert_eq!(tl[0]["kind"], "incident");
        assert_eq!(tl[0]["at"], "2026-10-28T21:39:00Z");
        assert_eq!(tl[0]["detail"], "Tctl 95 °C token=•••");
        assert_eq!(tl[0]["severity"], "high");
        assert_eq!(tl[0]["ops_id"], "e8e10d40c994d9f0");
        assert_eq!(
            (
                tl[1]["kind"].as_str(),
                tl[1]["from"].as_str(),
                tl[1]["to"].as_str()
            ),
            (Some("change"), Some("ok"), Some("crit"))
        );
        assert_eq!(tl[2]["severity"], Value::Null);
        let svc = &v["services"][0];
        assert_eq!(
            (svc["name"].as_str(), svc["state"].as_str()),
            (Some("Lucina"), Some("crit"))
        );
        assert_eq!(svc["projects"][0]["components"][1]["state"], "crit");
        assert_eq!(
            svc["projects"][0]["components"][0]
                .as_object()
                .unwrap()
                .len(),
            7
        );
        for s in ["catalog", "sentinel", "ops"] {
            assert_eq!(v["sources"][s]["ok"], true, "{s}");
        }
        assert_eq!(v["sources"]["catalog"]["at"], "2026-10-28T21:45:00Z");
        assert!(!all_failed(&v));
    }

    #[test]
    fn failing_sources_leave_the_sentinel_shape() {
        let (s, _, _) = fixture();
        let down: Result<Value, String> = Err("Lucina unreachable".into());
        let v = build(&s, &down, &down, &Err("Lucina unreachable".into()), now());
        assert_eq!(
            v["sources"]["catalog"],
            json!({"ok": false, "at": null, "error": "Lucina unreachable"})
        );
        assert_eq!(v["sources"]["ops"]["ok"], false);
        assert_eq!(v["sources"]["sentinel"]["ok"], true);
        let keys: Vec<&str> = v["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "sentinel:nezopt:unit:gmaps-server",
                "sentinel:nezopt:unit:lone-worker",
                "sentinel:nezopt:unit:marketplace-rails-sync"
            ]
        );
        assert_eq!(v["hosts"][0]["id"], "nezopt");
        assert_eq!(v["hosts"][0]["load"], json!([1.0, 2.0, 3.0]));
        assert_eq!(v["hosts"][0]["tctl"], 71.0);
        assert_eq!(
            (v["checks_ok"].as_u64(), v["checks_total"].as_u64()),
            (Some(1), Some(4))
        );
        assert_eq!(v["services"], json!([]));
        assert_eq!(v["timeline"].as_array().unwrap().len(), 1);
        assert!(!all_failed(&v));
        let none = build(
            &json!({"hosts": [], "events": []}),
            &down,
            &down,
            &Err("x".into()),
            now(),
        );
        assert!(all_failed(&none));
        assert_eq!(none["overall"], "ok");
        // A tree without the status still gives problems and partial sources.
        let (_, _, t) = fixture();
        let v = build(&s, &down, &Ok(t), &Ok(vec![]), now());
        assert_eq!(v["sources"]["catalog"]["ok"], false);
        assert_eq!(
            v["sources"]["catalog"]["error"],
            "status: Lucina unreachable"
        );
        assert_eq!(v["attention"][0]["key"], "catalog:gmaps-server");
        assert_eq!(v["attention"][0]["caused"], json!(["lucina-maps"]));
    }

    #[test]
    fn chained_causes_fold_to_the_root() {
        let a = comp("db", "db", "nezopt", "down", json!({}));
        let b = comp(
            "api",
            "k8s",
            "nezopt",
            "broken",
            json!({"status": {"level": "ok", "impacted_by": ["db"]}}),
        );
        let c = comp(
            "site",
            "site",
            "nezopt",
            "degraded",
            json!({"status": {"level": "ok", "impacted_by": ["api"]}}),
        );
        let d = comp(
            "own",
            "service",
            "nezopt",
            "broken",
            json!({"status": {"level": "down", "impacted_by": ["db"]}}),
        );
        let v = build(
            &json!({"hosts": [], "events": []}),
            &Err("x".into()),
            &Ok(tree(vec![a, b, c, d])),
            &Ok(vec![]),
            now(),
        );
        let keys: Vec<&str> = v["attention"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            ["catalog:db", "catalog:own"],
            "own is down itself, so it stays"
        );
        assert_eq!(v["attention"][0]["caused"], json!(["api", "site"]));
        assert_eq!(v["attention"][0]["impact"]["degraded"][0]["id"], "site");
    }

    #[test]
    fn cycles_and_independent_sentinel_failures_stay_visible() {
        let a = comp(
            "a",
            "service",
            "nezopt",
            "broken",
            json!({"status": {"level": "ok", "impacted_by": ["b"]}}),
        );
        let b = comp(
            "b",
            "service",
            "nezopt",
            "broken",
            json!({"status": {"level": "ok", "impacted_by": ["a"]}}),
        );
        let v = build(
            &json!({}),
            &Err("down".into()),
            &Ok(tree(vec![a, b])),
            &Ok(vec![]),
            now(),
        );
        assert_eq!(v["attention_count"], 2);

        let root = comp("db", "db", "nezopt", "down", json!({}));
        let child = comp(
            "api",
            "service",
            "nezopt",
            "broken",
            json!({
                "units": ["api.service"], "status": {"level": "ok", "impacted_by": ["db"]}
            }),
        );
        let v = build(
            &sentinel(json!([check("api", "crit", "failed")])),
            &Err("down".into()),
            &Ok(tree(vec![root, child])),
            &Ok(vec![]),
            now(),
        );
        assert_eq!(v["attention_count"], 2);
        assert!(v["attention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["key"] == "catalog:api" && i["sources"] == json!(["catalog", "sentinel"])));
    }

    #[test]
    fn optional_unreachable_problem_and_optional_warning() {
        let mut mac = comp("mac", "host", "", "down", json!({"optional": true}));
        let v = build(
            &json!({}),
            &Err("down".into()),
            &Ok(tree(vec![mac.clone()])),
            &Ok(vec![]),
            now(),
        );
        assert_eq!(v["overall"], "ok");
        assert_eq!(v["hosts"][0]["state"], "unknown");
        assert_eq!(v["attention_count"], 0);
        mac["status"]["effective"] = json!("warn");
        let v = build(
            &json!({}),
            &Ok(json!({"problems": []})),
            &Ok(tree(vec![mac])),
            &Ok(vec![]),
            now(),
        );
        assert_eq!(v["overall"], "warn");
    }

    #[test]
    fn state_mapping_unknown_host_and_timeline_cap() {
        for (input, expected) in [
            ("down", "crit"),
            ("broken", "crit"),
            ("degraded", "warn"),
            ("warn", "warn"),
            ("ok", "ok"),
            ("idle", "idle"),
            ("other", "unknown"),
        ] {
            assert_eq!(state_of(input), expected);
        }
        let h = comp("empty", "host", "", "unknown", json!({}));
        let ops = (0..200).map(|i| json!({"id": i.to_string(), "kind": "decision", "at": rfc(now() - Duration::minutes(i)), "title": "Decision"})).collect();
        let v = build(
            &json!({}),
            &Err("down".into()),
            &Ok(tree(vec![h])),
            &Ok(ops),
            now(),
        );
        assert_eq!(v["hosts"][0]["state"], "unknown");
        assert_eq!(v["timeline"].as_array().unwrap().len(), 150);
        assert_eq!(v["timeline"][0]["ops_id"], "0");
    }

    #[test]
    fn all_display_text_is_redacted_and_scan_time_is_current() {
        let secret = "Bearer alphabetic";
        let c = comp(
            "service",
            "service",
            "nezopt",
            "warn",
            json!({
                "name": secret, "description": secret, "group": secret,
                "logs": secret, "restart": secret, "runbook": secret,
                "status": {"findings": [{"level": "warn", "what": "http", "detail": secret}]}
            }),
        );
        let mut t = tree(vec![c]);
        t["groups"][0]["name"] = json!(secret);
        let status = json!({"generated_at": "2026-10-28T21:49:00Z", "hosts": {"nezopt": {
            "taken_at": "2026-10-28T21:40:00Z", "reachable": true, "os": secret, "kernel": secret,
            "disks": [{"mount": secret}]
        }}, "problems": [{"id": "service", "effective": "warn"}]});
        let v = build(
            &sentinel(json!([])),
            &Ok(status),
            &Ok(t),
            &Ok(vec![]),
            now(),
        );
        assert!(!v.to_string().contains("alphabetic"));
        assert_eq!(v["hosts"][0]["last_report"], "2026-10-28T21:49:00Z");
        assert_eq!(
            plain("http", "first line\nsecond line"),
            "first line second line"
        );
    }

    #[test]
    fn plain_words() {
        assert_eq!(plain("user unit a.service", "inactive/dead"), "Not running");
        assert_eq!(
            plain("system unit a.service", "failed/failed (exit-code)"),
            "Failed"
        );
        assert_eq!(
            plain("port 18210", "not listening"),
            "port 18210 not listening"
        );
        assert_eq!(
            plain("user unit a", "active/running, 12 failures in 24h"),
            "failed 12 times in 24 h"
        );
        assert_eq!(
            plain("http", "HTTP 502 from https://x.example.com"),
            "HTTP 502 from https://x.example.com"
        );
        assert_eq!(
            plain("http", &"hello ".repeat(100)).chars().count(),
            MAX_LINE
        );
        assert_eq!(plain("container c", ""), "container c");
    }

    #[test]
    fn redaction_removes_values() {
        for (input, want) in [
            ("Bearer abc", "Bearer •••"),
            ("b64 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "b64 •••"),
            ("b64 abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO", "b64 •••"),
            (
                "Authorization: Bearer abc.def-ghi",
                "Authorization: Bearer •••",
            ),
            (
                "curl https://x/?token=s3cr3t&page=2",
                "curl https://x/?token=•••&page=2",
            ),
            (
                "CARDTRADER_AUTH_TOKEN=hunter2 run",
                "CARDTRADER_AUTH_TOKEN=••• run",
            ),
            ("api_key=\"quoted value\" end", "api_key=\"•••\" end"),
            ("password=pw; secret=x", "password=•••; secret=•••"),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig_abc here",
                "jwt ••• here",
            ),
            ("sha 0123456789abcdef0123456789abcdef!", "sha •••!"),
            (
                "b64 QWxhZGRpbjpvcGVuIHNlc2FtZQ9xYmFzZTY0c3RyaW5nMTIzNDU2 ok",
                "b64 ••• ok",
            ),
            ("key sk-proj-abcdefghijklmnop", "key •••"),
            ("gh ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123", "gh •••"),
            ("slack xoxb-1234567890-abcdef", "slack •••"),
            (
                "postgres://writer:pa55@db:5432/x",
                "postgres://•••@db:5432/x",
            ),
            ("device sec_AAAAAAAAAAAAAAAAAAAAAAAA", "device •••"),
        ] {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn redaction_keeps_plain_words() {
        for keep in [
            "set CARDTRADER_AUTH_TOKEN in Infisical",
            "LONG_CARDTRADER_AUTH_TOKEN_ENVIRONMENT_VARIABLE_NAME",
            "user unit gmaps-server.service: inactive/dead",
            "journalctl --user -u gmaps-server -n 100 --no-pager",
            "https://maps.example.com/v1/tiles",
            "ops id e8e10d40c994d9f0",
            "Tctl 95 °C, load 9.58 5.03 2.48",
            "the monkey key is in the drawer",
        ] {
            assert_eq!(redact(keep), keep);
        }
    }
}
