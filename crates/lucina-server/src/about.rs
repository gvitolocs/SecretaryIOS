//! "About you": Lucina's memory about the owner as a person (design: docs/design/about-you.md).
//!
//! The store ([`lucina_store::about`]) is the source of truth; Honcho stays for conversation recall.
//! - **Always in context:** [`context_block`] puts the summary of what holds into every owner turn,
//!   whatever the intent policy; [`lucina_store::recent::clip_memory_for_reply`] keeps it whole.
//! - **She keeps it true herself:** the `about_me` tool on her own turns ("that's wrong", "I moved"), and
//!   [`extract`]: local Qwen only, over his new chat messages, shared documents and calendar events,
//!   applying the confident changes with their source. Dates expire facts here ([`spawn`]).
//! - **Only his own words write it:** agent and MCP turns never do. A self-test turn reads his facts but
//!   writes a store of its own, so a test never changes what she knows about him.

pub mod extract;

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use lucina_store::about::{self as store, AboutError, AboutStore, Draft, Source};
use serde_json::{json, Value};

use crate::api::ApiError;

pub const FILE: &str = "about-you.json";
const SELFTEST_FILE: &str = "selftest-about-you.json";

pub struct AboutYou {
    pub owner: AboutStore,
    pub selftest: AboutStore,
    pub queue: Mutex<Vec<extract::Item>>,
    queue_file: std::path::PathBuf,
    pub cfg: extract::ExtractConfig,
}

impl std::fmt::Debug for AboutYou {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AboutYou").finish_non_exhaustive()
    }
}

impl AboutYou {
    pub fn new(data_dir: &Path, cfg: extract::ExtractConfig) -> Self {
        let queue_file = data_dir.join("about-you-queue.json");
        let queue = std::fs::read(&queue_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            owner: AboutStore::new(data_dir.join(FILE)),
            selftest: AboutStore::new(data_dir.join(SELFTEST_FILE)),
            queue: Mutex::new(queue),
            queue_file,
            cfg,
        }
    }

    /// Where a turn of `session` writes.
    pub fn store_for(&self, session: &str) -> &AboutStore {
        if crate::selftest::is_session(session) {
            &self.selftest
        } else {
            &self.owner
        }
    }

    /// What a turn of `session` reads: his facts, plus a self-test's own.
    pub fn facts_for(&self, session: &str) -> Vec<store::Fact> {
        let mut out = self.owner.all();
        if crate::selftest::is_session(session) {
            out.extend(self.selftest.all());
        }
        out
    }

    pub(crate) fn persist_queue(&self, q: &[extract::Item]) {
        if let Ok(bytes) = serde_json::to_vec(q) {
            if let Err(e) = lucina_store::atomic::write_atomic(&self.queue_file, &bytes) {
                tracing::warn!(error = %e.kind(), "about-you queue not written");
            }
        }
    }
}

fn about_err(e: AboutError) -> ApiError {
    match e {
        AboutError::Secret => ApiError::new(400, "secret_refused", &e.to_string()),
        AboutError::NotFound => ApiError::new(404, "not_found", "no such fact"),
        AboutError::Io => ApiError::new(500, "store_failed", "the memory could not be written"),
        AboutError::Invalid(m) => ApiError::new(400, "bad_request", &m),
    }
}

/// The always-on block for an owner turn; empty when nothing is known.
pub fn context_block(st: &crate::AppState, session: &str) -> String {
    store::summary(&st.about.facts_for(session), store::SUMMARY_CHARS)
}

/// His own words, a shared document or a calendar event: queued for the extractor. Never an agent's
/// or an MCP caller's text (the callers check), never a secret (masked first).
pub fn observe(st: &crate::AppState, session: &str, kind: &str, reference: &str, text: &str) {
    // Juno's chat is also where agents test by typing as him (evdev): not his words for certain.
    if !st.about.cfg.enabled || text.trim().is_empty() || session.starts_with("secretary_juno") {
        return;
    }
    let masked = lucina_secrets::labelled::mask(text);
    let masked = lucina_secrets::redactor().redact(&masked).into_owned();
    let item = extract::Item {
        kind: kind.to_owned(),
        reference: reference.to_owned(),
        text: masked.chars().take(extract::MAX_ITEM_CHARS).collect(),
        selftest: crate::selftest::is_session(session),
        at: Utc::now(),
        tries: 0,
    };
    let mut q = st.about.queue.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= extract::MAX_QUEUE {
        q.remove(0);
    }
    q.push(item);
    st.about.persist_queue(&q);
}

/// `/api/secretary/about…` (device logins through secretary-server, and Lucina's API token).
pub async fn route(
    st: &crate::AppState,
    method: &str,
    rest: &[&str],
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let s = &st.about.owner;
    let now = Utc::now();
    let draft = |b: &Value| -> Result<Draft, ApiError> {
        serde_json::from_value(b.clone())
            .map_err(|_| ApiError::bad_request("text, category, validUntil, validUntilCondition"))
    };
    let reason = |b: &Option<Value>| {
        b.as_ref()
            .and_then(|v| v["reason"].as_str().map(str::to_owned))
    };
    match (method, rest) {
        ("GET", []) => {
            let (facts, history): (Vec<_>, Vec<_>) = s
                .all()
                .into_iter()
                .partition(|f| f.status == store::Status::Active);
            Ok(json!({"facts": facts, "history": history}))
        }
        ("POST", []) => {
            let b = body.as_ref().unwrap_or(&Value::Null);
            let d = draft(b)?;
            // The page adds his own edits; a seed names where a fact came from (a note, a document).
            let kind = b["source"]["kind"].as_str().filter(|k| store::SOURCES.contains(k)).unwrap_or("owner-edit");
            let reference = b["source"]["reference"].as_str().unwrap_or("").chars().take(120).collect();
            let by = if kind == "owner-edit" { "owner" } else { "seed" };
            let f = s
                .add(&d, Source { kind: kind.to_owned(), reference }, by, now)
                .map_err(about_err)?;
            Ok(serde_json::to_value(f).unwrap_or_default())
        }
        ("PATCH", [id]) => {
            let d = draft(body.as_ref().unwrap_or(&Value::Null))?;
            Ok(
                serde_json::to_value(s.update(id, &d, "owner", now).map_err(about_err)?)
                    .unwrap_or_default(),
            )
        }
        ("POST", [id, "end"]) => Ok(serde_json::to_value(
            s.end(id, reason(&body).as_deref(), "owner", now)
                .map_err(about_err)?,
        )
        .unwrap_or_default()),
        ("POST", [id, "retract"]) => Ok(serde_json::to_value(
            s.retract(id, reason(&body).as_deref(), "owner", now)
                .map_err(about_err)?,
        )
        .unwrap_or_default()),
        ("GET", [id]) => s
            .get(id)
            .map(|f| serde_json::to_value(f).unwrap_or_default())
            .ok_or_else(|| about_err(AboutError::NotFound)),
        _ => Err(ApiError::not_found()),
    }
}

/// Lucina's `about_me` tool.
#[async_trait::async_trait]
impl lucina_tools::tools::about_me::AboutMemory for AboutYou {
    async fn act(&self, session_id: &str, action: &str, body: Value) -> Result<Value, String> {
        let now = Utc::now();
        let s = self.store_for(session_id);
        let id = body["id"].as_str().unwrap_or("");
        let reason = body["reason"].as_str();
        let draft: Draft = serde_json::from_value(body.clone()).unwrap_or_default();
        let one = |r: Result<store::Fact, AboutError>| {
            r.map(|f| json!({"ok": true, "fact": store::line(&f)}))
                .map_err(|e| e.to_string())
        };
        match action {
            "lookup" => {
                let q = body["query"].as_str().unwrap_or("");
                let history = body["history"].as_bool().unwrap_or(false);
                let mut facts: Vec<store::Fact> = self.facts_for(session_id);
                let words: Vec<String> = q
                    .to_lowercase()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                facts.retain(|f| {
                    (history || f.status == store::Status::Active)
                        && words
                            .iter()
                            .all(|w| f.text.to_lowercase().contains(w.as_str()) || f.category == *w)
                });
                let lines: Vec<String> = facts
                    .iter()
                    .take(40)
                    .map(|f| {
                        let src =
                            format!(" (from {}, {})", f.source.kind, f.learned_at.date_naive());
                        format!("{}{src}", store::line(f))
                    })
                    .collect();
                Ok(json!({"facts": lines, "count": facts.len()}))
            }
            "add" => {
                let source = Source {
                    kind: "chat".into(),
                    reference: session_id.to_owned(),
                };
                one(s.add(&draft, source, "lucina", now))
            }
            "update" => one(s.update(id, &draft, "lucina", now)),
            "confirm" => one(s.confirm(id, "lucina", now)),
            "end" => one(s.end(id, reason, "lucina", now)),
            "retract" => one(s.retract(id, reason, "lucina", now)),
            other => Err(format!("unknown action {other}")),
        }
    }
}

/// The worker: dates expire facts; the extractor reads what was queued; calendar events are looked at.
pub fn spawn(st: &crate::AppState) -> Option<tokio::task::JoinHandle<()>> {
    if !st.about.cfg.enabled {
        return None;
    }
    let st = st.clone();
    Some(tokio::spawn(async move {
        let mut last_calendar = tokio::time::Instant::now() - Duration::from_secs(3600);
        loop {
            let now = Utc::now();
            let today = now.with_timezone(&st.cfg.time_zone).date_naive();
            for s in [&st.about.owner, &st.about.selftest] {
                for f in s.expire_due(today, now) {
                    tracing::info!(fact = %f.id, category = %f.category, "about-you fact expired");
                }
            }
            if last_calendar.elapsed() >= Duration::from_secs(3600) {
                last_calendar = tokio::time::Instant::now();
                extract::look_at_calendar(&st).await;
            }
            extract::run_queue(&st).await;
            tokio::time::sleep(st.about.cfg.every).await;
        }
    }))
}
