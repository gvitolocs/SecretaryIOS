//! Bot Hospital: the daily Poko-news proof audit (port of the Node Lucina's
//! `src/bot-hospital.js` `runDailyNewsAudit` → `handleIncident`).
//!
//! Poko (hermes-poko on Oracle p2) records every news post with the Telegram and
//! Discord message ids. Once a day the audit reads that state through the
//! oracle-poko-tunnel (`127.0.0.1:18789`), checks the latest post, and when the
//! proof is missing it opens an incident (`data/bot-hospital/incidents.json`, the
//! Node layout), tells the owner, retries the scout, re-checks the proof, and when
//! that is not enough hands the repair to the DeepSeek Harness with Node's prompt.
//!
//! Not ported: the Gemini CLI consultation (`BOT_HOSPITAL_GEMINI_ENABLED`, off on
//! nezopt), the SSH restart of hermes-poko on the Oracle box (only for
//! `bot_runtime_unhealthy`, which the daily audit never raises), the incident
//! intake endpoint `/api/internal/bot-hospital/incident` (no caller: Poko reports to
//! its own hospital on p2) and the hypemeter / active-task diagnosis lines.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::AppState;

const MAX_INCIDENTS: usize = 200;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const NEWS_TARGETS: [&str; 2] = ["telegram", "discord"];
const NEWS_POST_PROOF_INCIDENTS: [&str; 3] = [
    "news_post_partial_failure",
    "news_post_image_failure",
    "news_post_platform_failure",
];
const RETRY_INCIDENTS: [&str; 6] = [
    "hypemeter_context_unavailable",
    "hypemeter_snapshot_stale",
    "news_post_partial_failure",
    "news_post_image_failure",
    "news_post_platform_failure",
    "llm_generation_failure",
];
const RETRY_HEALTHY_REASONS: [&str; 2] = ["already_posted", "no_new_tcg_article_above_threshold"];

/// `config.botHospital` (+ `poko.newsAuditWindowHours`).
#[derive(Clone)]
pub struct HospitalConfig {
    pub enabled: bool,
    pub daily_audit_enabled: bool,
    pub tokens: Vec<String>,
    pub alert_chat_id: Option<String>,
    pub store_path: PathBuf,
    pub dedupe: Duration,
    pub repair_enabled: bool,
    pub simulate_only: bool,
    pub poko_health_url: String,
    pub poko_scout_run_url: String,
    pub poko_news_state_url: String,
    pub interval: Duration,
    pub initial_delay: Duration,
    pub window_hours: f64,
}

impl std::fmt::Debug for HospitalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HospitalConfig")
            .field("enabled", &self.enabled)
            .field("tokens", &self.tokens.len())
            .field("store_path", &self.store_path)
            .field("poko_news_state_url", &self.poko_news_state_url)
            .finish_non_exhaustive()
    }
}

impl HospitalConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let val = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let flag = |k: &str, d: bool| val(k).map(|v| v == "true").unwrap_or(d);
        let num = |k: &str, d: f64| val(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
        let first_allowed = val("TELEGRAM_ALLOWED_CHAT_IDS").and_then(|v| {
            v.split(',')
                .map(str::trim)
                .find(|s| !s.is_empty())
                .map(str::to_owned)
        });
        // Node's order: botHospital.token first, then poko.hospitalToken.
        let mut tokens = Vec::new();
        for chain in [
            &[
                "BOT_HOSPITAL_TOKEN",
                "HERMES_INTERNAL_ALERT_TOKEN",
                "POKO_HOSPITAL_TOKEN",
                "POKO_NEWS_ALERT_TOKEN",
            ][..],
            &[
                "POKO_HOSPITAL_TOKEN",
                "POKO_NEWS_ALERT_TOKEN",
                "BOT_HOSPITAL_TOKEN",
                "HERMES_INTERNAL_ALERT_TOKEN",
            ][..],
        ] {
            if let Some(t) = chain.iter().find_map(|k| val(k)) {
                if !tokens.contains(&t) {
                    tokens.push(t);
                }
            }
        }
        // The container wrote `/app/data/...`; on the host that is the data dir.
        let store_path = match val("BOT_HOSPITAL_INCIDENT_STORE_PATH") {
            Some(p) => match p.strip_prefix("/app/data/") {
                Some(rest) => data_dir.join(rest),
                None => PathBuf::from(p),
            },
            None => data_dir.join("bot-hospital").join("incidents.json"),
        };
        let ms = |k: &str, d: f64| Duration::from_millis(num(k, d).max(0.0) as u64);
        Self {
            enabled: flag("BOT_HOSPITAL_ENABLED", false),
            daily_audit_enabled: flag("BOT_HOSPITAL_DAILY_AUDIT_ENABLED", true),
            tokens,
            alert_chat_id: val("BOT_HOSPITAL_ALERT_CHAT_ID")
                .or(first_allowed)
                .or_else(|| val("TELEGRAM_CHAT_ID_POKO")),
            store_path,
            dedupe: ms("BOT_HOSPITAL_DEDUPE_MS", 30.0 * 60_000.0),
            repair_enabled: flag("BOT_HOSPITAL_REPAIR_ENABLED", true),
            simulate_only: flag("BOT_HOSPITAL_SIMULATE_ONLY", false),
            poko_health_url: val("BOT_HOSPITAL_POKO_HEALTH_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8789/health".into()),
            poko_scout_run_url: val("BOT_HOSPITAL_POKO_SCOUT_RUN_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8789/api/internal/poko-news-scout/run".into()),
            poko_news_state_url: val("BOT_HOSPITAL_POKO_NEWS_STATE_URL").unwrap_or_else(|| {
                "http://127.0.0.1:8789/api/internal/poko-news-scout/state".into()
            }),
            interval: ms("BOT_HOSPITAL_DAILY_AUDIT_INTERVAL_MS", 86_400_000.0),
            initial_delay: ms("BOT_HOSPITAL_DAILY_AUDIT_INITIAL_DELAY_MS", 300_000.0),
            window_hours: num("POKO_NEWS_AUDIT_WINDOW_HOURS", 26.0),
        }
    }

    fn token(&self) -> Option<&str> {
        self.tokens.first().map(String::as_str)
    }
}

/// What the last audit found (shown in `/health`).
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HospitalHealth {
    pub enabled: bool,
    pub last_audit_at: Option<String>,
    pub last_audit_ok: Option<bool>,
    pub last_missing: Vec<String>,
    pub last_incident_id: Option<String>,
}

// ------------------------------------------------------------------ proof

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Validation {
    pub ok: bool,
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn js_date(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// `latestProofFromState`.
pub fn latest_proof(state: &Value) -> Option<Value> {
    if let Some(history) = state["postHistory"].as_array().filter(|h| !h.is_empty()) {
        // Newest `postedAt` first; unparseable dates sort as the epoch (Date.parse → NaN → 0).
        let ts = |p: &Value| {
            js_date(&p["postedAt"])
                .map(|d| d.timestamp_millis())
                .unwrap_or(0)
        };
        let mut best = &history[0];
        for p in history.iter().skip(1) {
            if ts(p) > ts(best) {
                best = p;
            }
        }
        return Some(best.clone());
    }
    let last = &state["lastArticle"];
    if s(&state["lastPostedAt"]).is_empty() || !last.is_object() {
        return None;
    }
    let targets = state["postedArticleTargets"].as_object();
    let article_id = targets
        .and_then(|m| {
            m.iter().find(|(_, t)| {
                t.as_array().is_some_and(|a| {
                    a.iter().any(|x| x == "telegram") && a.iter().any(|x| x == "discord")
                })
            })
        })
        .map(|(k, _)| k.clone())
        .unwrap_or_default();
    let posted_targets = if article_id.is_empty() {
        json!([])
    } else {
        targets
            .and_then(|m| m.get(&article_id))
            .cloned()
            .unwrap_or(json!([]))
    };
    Some(json!({
        "articleId": article_id,
        "postedAt": state["lastPostedAt"],
        "title": last["title"],
        "link": last["link"],
        "score": last["score"],
        "postedTargets": posted_targets,
        "imageUrl": "",
        "results": [],
        "legacy": true,
    }))
}

/// `validateDailyNewsProof`.
pub fn validate(proof: Option<&Value>, now: DateTime<Utc>, window_hours: f64) -> Validation {
    let mut missing = Vec::new();
    let mut warnings = Vec::new();
    let window = window_hours.max(1.0);
    let window_label = if window.fract() == 0.0 {
        format!("{}", window as i64)
    } else {
        format!("{window}")
    };
    match proof {
        None => missing.push("no recorded Poko news post".to_owned()),
        Some(p) => {
            let fresh = js_date(&p["postedAt"]).is_some_and(|t| {
                (now - t).num_milliseconds() as f64 <= window * 3_600_000.0
                    && t.timestamp_millis() != 0
            });
            if !fresh {
                missing.push(format!("no Poko news post within {window_label}h"));
            }
            let targets: Vec<&str> = p["postedTargets"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for t in NEWS_TARGETS {
                if !targets.contains(&t) {
                    missing.push(format!("missing {t} target"));
                }
            }
            let policy = &p["imagePolicy"];
            let allows_missing = policy["allowMissingImage"].as_bool().unwrap_or(false)
                || s(&policy["imagePolicy"]) == "seo_metadata_only";
            if s(&p["imageUrl"]).is_empty() {
                if allows_missing {
                    let reason = [s(&policy["reason"]), s(&policy["imagePolicy"])]
                        .into_iter()
                        .find(|r| !r.is_empty())
                        .unwrap_or("sensitive content policy");
                    warnings.push(format!("image intentionally omitted: {reason}"));
                } else if p["legacy"].as_bool().unwrap_or(false) {
                    warnings.push("image proof unavailable for legacy post".into());
                } else {
                    missing.push("missing image URL".into());
                }
            }
            if let Some(results) = p["results"].as_array().filter(|r| !r.is_empty()) {
                for t in NEWS_TARGETS {
                    let ok = results.iter().find(|r| r["target"] == t).is_some_and(|r| {
                        r["ok"].as_bool() == Some(true) && truthy(&r["messageId"])
                    });
                    if !ok {
                        missing.push(format!("missing {t} message proof"));
                    }
                }
            }
        }
    }
    Validation {
        ok: missing.is_empty(),
        missing,
        warnings,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn truncate(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        t.to_owned()
    } else {
        format!(
            "{}...",
            t.chars().take(max.saturating_sub(3)).collect::<String>()
        )
    }
}

// --------------------------------------------------------------- incidents

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Article {
    pub id: String,
    pub title: String,
    pub link: String,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub ok: bool,
    pub reason: String,
    pub message: String,
    pub http_status: Option<f64>,
}

/// `normalizeIncident`'s record, in Node's field order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub severity: String,
    pub source: String,
    pub created_at: String,
    pub dedupe_key: String,
    pub operation: String,
    pub target: String,
    pub article: Article,
    pub status: Status,
    pub context: Value,
    pub next_action: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IncidentStore {
    #[serde(default)]
    incidents: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

fn now_iso(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn safe_id(v: &str) -> String {
    v.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.:-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(180)
        .collect()
}

/// The incident the daily audit opens (`runDailyNewsAudit`).
pub fn audit_incident(
    proof: Option<&Value>,
    validation: &Validation,
    state_status: Option<u16>,
    now: DateTime<Utc>,
) -> Incident {
    let has_image = proof.is_some_and(|p| !s(&p["imageUrl"]).is_empty());
    let kind = if has_image {
        "news_post_partial_failure"
    } else {
        "news_post_image_failure"
    };
    let created_at = now_iso(now);
    let dedupe_key = truncate(
        &format!(
            "daily-news-audit:{}:{}",
            &created_at[..10],
            validation.missing.join("|")
        ),
        240,
    );
    let p = proof.cloned().unwrap_or(Value::Null);
    Incident {
        id: safe_id(&format!("{}:{dedupe_key}", now.timestamp_millis())),
        kind: kind.into(),
        severity: "warning".into(),
        source: "lucina-daily-news-audit".into(),
        created_at,
        dedupe_key,
        operation: "daily_news_audit".into(),
        target: "daily_news_audit".into(),
        article: Article {
            id: truncate(s(&p["articleId"]), 900),
            title: truncate(s(&p["title"]), 240),
            link: truncate(s(&p["link"]), 900),
            score: p["score"].as_f64(),
        },
        status: Status {
            ok: false,
            reason: "daily_news_audit_failed".into(),
            message: truncate(&validation.missing.join("; "), 1200),
            http_status: None,
        },
        context: json!({"proof": p, "stateResponseStatus": state_status}),
        next_action: "retry".into(),
    }
}

fn read_store(path: &Path) -> IncidentStore {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_store(path: &Path, mut store: IncidentStore, now: DateTime<Utc>) -> std::io::Result<()> {
    let skip = store.incidents.len().saturating_sub(MAX_INCIDENTS);
    store.incidents.drain(..skip);
    store.updated_at = Some(now_iso(now));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&store).map_err(std::io::Error::other)?;
    lucina_store::atomic::write_atomic(path, text.as_bytes()).map_err(std::io::Error::other)
}

/// `isDuplicate`, then append. `Ok(false)`: a duplicate inside the dedupe window.
pub fn record_incident(
    path: &Path,
    incident: &Incident,
    dedupe: Duration,
    now: DateTime<Utc>,
) -> std::io::Result<bool> {
    let mut store = read_store(path);
    let dup = store.incidents.iter().any(|e| {
        e["dedupeKey"] == incident.dedupe_key.as_str()
            && js_date(&e["createdAt"])
                .is_some_and(|t| (now - t).num_milliseconds() < dedupe.as_millis() as i64)
    });
    if dup {
        return Ok(false);
    }
    store
        .incidents
        .push(serde_json::to_value(incident).map_err(std::io::Error::other)?);
    write_store(path, store, now)?;
    Ok(true)
}

/// `progressMessage`.
pub fn progress_message(i: &Incident) -> String {
    [
        format!("Bot Hospital: Poko is sick ({}).", i.kind),
        if i.article.title.is_empty() {
            String::new()
        } else {
            format!("Article: {}", i.article.title)
        },
        if i.status.message.is_empty() {
            String::new()
        } else {
            format!("Symptom: {}", i.status.message)
        },
        "Lucina is diagnosing now.".into(),
    ]
    .into_iter()
    .filter(|l| !l.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

#[derive(Debug, Default, Clone)]
pub struct Repair {
    pub ok: bool,
    pub healed_by: Option<String>,
    pub blocker: Option<String>,
    pub actions: Vec<String>,
}

/// `finalMessage` without the Gemini and hypemeter lines.
pub fn final_message(i: &Incident, r: &Repair) -> String {
    [
        format!(
            "Bot Hospital final: Poko {}.",
            if r.ok { "healed" } else { "not healed" }
        ),
        format!("Incident: {}", i.kind),
        r.healed_by
            .as_ref()
            .map(|h| format!("Healed by: {h}"))
            .unwrap_or_default(),
        r.blocker
            .as_ref()
            .map(|b| format!("Blocker: {b}"))
            .unwrap_or_default(),
        if r.actions.is_empty() {
            String::new()
        } else {
            format!("Repairs tried: {}", r.actions.join(", "))
        },
    ]
    .into_iter()
    .filter(|l| !l.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

/// `compactRepairPrompt`, with today's topology (Poko on p2 behind the tunnel,
/// Lucina = the Rust lucina-rs service).
pub fn repair_prompt(
    i: &Incident,
    retry: Option<&Value>,
    proof_after: Option<&Value>,
    poko_ok: Option<bool>,
) -> String {
    let proof = &i.context["proof"];
    let incident = json!({
        "type": i.kind, "source": i.source, "operation": i.operation,
        "status": i.status, "article": i.article,
        "proof": if proof.is_object() { json!({
            "postedTargets": proof["postedTargets"], "results": proof["results"],
            "postedAt": proof["postedAt"], "title": proof["title"],
        }) } else { Value::Null },
    });
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    [
        "You are Lucina using DeepSeek Harness to repair Poko/Hermes from nezopt.".to_owned(),
        "Execute the task now. Do not say you are ready. Do not wait for more instructions.".into(),
        "Scope: Poko news posting proof only. Do not touch unrelated services or expose secrets.".into(),
        "Use focused shell/file inspection. If you patch code/config, run the relevant check and restart only the affected service.".into(),
        "Poko lives on Oracle p2 at /opt/hermes-poko as hermes-poko.service on port 8789; nezopt reaches it at 127.0.0.1:18789 through oracle-poko-tunnel.service.".into(),
        "Lucina is the Rust lucina-rs user service on nezopt (port 8790); never restart or edit it, and never start a Lucina on the Oracle boxes.".into(),
        "If hermes-poko.service is masked or stopped on p2, restore/start only Poko.".into(),
        "If a user explicitly asks for a post now and scout says no_new_tcg_article_above_threshold, post through Poko's social-post code and record Telegram/Discord message IDs in poko-news-state.json.".into(),
        "Never claim healed until the news state shows a fresh post with Telegram and Discord message IDs.".into(),
        "If the missing piece cannot be edited from this runtime, report that blocker exactly.".into(),
        "Final response <= 900 chars with: root cause, actions taken, verification result.".into(),
        String::new(),
        format!("Incident: {}", pretty(&incident)),
        String::new(),
        format!("Retry result: {}", pretty(retry.unwrap_or(&Value::Null))),
        String::new(),
        format!("Proof after retry: {}", pretty(proof_after.unwrap_or(&Value::Null))),
        String::new(),
        format!("Health summary: {}", pretty(&json!({"lucinaOk": true, "pokoOk": poko_ok}))),
    ]
    .join("\n")
}

// --------------------------------------------------------------- runtime

/// The I/O the audit needs, so tests can run it against stubs.
pub struct Hospital {
    pub cfg: HospitalConfig,
    pub http: reqwest::Client,
    pub health: Arc<Mutex<HospitalHealth>>,
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub ok: bool,
    pub status: Option<u16>,
    pub body: Value,
}

impl Hospital {
    async fn request(&self, method: reqwest::Method, url: &str) -> Fetched {
        let mut req = self
            .http
            .request(method.clone(), url)
            .timeout(REQUEST_TIMEOUT);
        if let Some(t) = self.cfg.token() {
            req = req.bearer_auth(t);
        }
        if method == reqwest::Method::POST {
            req = req.json(&json!({}));
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let ok_http = resp.status().is_success();
                let text = resp.text().await.unwrap_or_default();
                let body: Value = if text.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(&text)
                        .unwrap_or_else(|_| json!({"raw": truncate(&text, 500)}))
                };
                Fetched {
                    ok: ok_http && body["ok"] != json!(false),
                    status: Some(status),
                    body,
                }
            }
            Err(e) => Fetched {
                ok: false,
                status: None,
                // reqwest errors carry the URL; ours holds no secret, but keep it short.
                body: json!({"error": truncate(&e.to_string(), 200)}),
            },
        }
    }

    pub async fn fetch_state(&self) -> Fetched {
        self.request(reqwest::Method::GET, &self.cfg.poko_news_state_url)
            .await
    }

    /// One audit: `Ok(None)` when healthy, else the incident (or `None` for a duplicate)
    /// and the repair outcome.
    pub async fn audit<N: Notifier>(&self, notify: &N, now: DateTime<Utc>) -> AuditOutcome {
        let state = self.fetch_state().await;
        let proof = latest_proof(&state.body["state"]);
        let validation = if state.ok || state.status.is_some() {
            validate(proof.as_ref(), now, self.cfg.window_hours)
        } else {
            Validation {
                ok: false,
                missing: vec![format!(
                    "Poko news state unavailable: {}",
                    s(&state.body["error"])
                )],
                warnings: vec![],
            }
        };
        {
            let mut h = self.health.lock().unwrap_or_else(|e| e.into_inner());
            h.last_audit_at = Some(now_iso(now));
            h.last_audit_ok = Some(validation.ok);
            h.last_missing = validation.missing.clone();
        }
        if validation.ok {
            return AuditOutcome::Healthy;
        }
        let incident = audit_incident(proof.as_ref(), &validation, state.status, now);
        match record_incident(&self.cfg.store_path, &incident, self.cfg.dedupe, now) {
            Ok(true) => {}
            Ok(false) => return AuditOutcome::Duplicate(incident.dedupe_key),
            Err(e) => tracing::warn!(error = %e, "bot hospital: incident store not writable"),
        }
        self.health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_incident_id = Some(incident.id.clone());
        notify.operator(&progress_message(&incident)).await;
        let poko = self
            .request(reqwest::Method::GET, &self.cfg.poko_health_url)
            .await;
        notify
            .operator(&format!(
                "Bot Hospital: diagnosis complete for {}. Repairing now.",
                incident.kind
            ))
            .await;
        let repair = self.repair(&incident, Some(poko.ok), notify).await;
        let text = final_message(&incident, &repair);
        notify.operator(&text).await;
        notify.app(&text, &incident.dedupe_key).await;
        AuditOutcome::Incident(Box::new((incident, repair)))
    }

    /// `repairIncident` for the audit's incident types.
    async fn repair<N: Notifier>(&self, i: &Incident, poko_ok: Option<bool>, notify: &N) -> Repair {
        let mut r = Repair::default();
        if !self.cfg.repair_enabled {
            r.blocker = Some("repair_disabled".into());
            return r;
        }
        if self.cfg.simulate_only {
            r.ok = true;
            r.healed_by = Some("simulation".into());
            r.actions.push("simulate_only".into());
            return r;
        }
        let needs_proof = NEWS_POST_PROOF_INCIDENTS.contains(&i.kind.as_str());
        let mut retry_body = None;
        let mut proof_after = None;
        if RETRY_INCIDENTS.contains(&i.kind.as_str()) {
            let retry = self
                .request(reqwest::Method::POST, &self.cfg.poko_scout_run_url)
                .await;
            r.actions.push("retry_poko_news_scout".into());
            let posted = retry.body["posted"] == json!(true);
            let reason = s(&retry.body["reason"]).to_owned();
            retry_body = Some(
                json!({"ok": retry.ok, "posted": posted, "reason": reason, "httpStatus": retry.status}),
            );
            if retry.ok && (needs_proof || posted || reason == "already_posted") {
                let state = self.fetch_state().await;
                let proof = latest_proof(&state.body["state"]);
                let v = validate(proof.as_ref(), Utc::now(), self.cfg.window_hours);
                r.actions.push("verify_news_post_proof_after_retry".into());
                proof_after = Some(json!({"ok": state.ok && v.ok, "missing": v.missing}));
                if state.ok && v.ok {
                    r.ok = true;
                    r.healed_by = Some("allowlisted_retry_with_proof".into());
                    return r;
                }
            }
            if !needs_proof && retry.ok && RETRY_HEALTHY_REASONS.contains(&reason.as_str()) {
                r.ok = true;
                r.healed_by = Some("allowlisted_retry".into());
                return r;
            }
        }
        let prompt = repair_prompt(i, retry_body.as_ref(), proof_after.as_ref(), poko_ok);
        match notify.harness(&prompt).await {
            None => {
                r.blocker = Some("project_work_unavailable".into());
                return r;
            }
            Some(Err(e)) => {
                tracing::warn!(error = %truncate(&e, 300), "bot hospital: harness repair failed");
                r.actions.push("harness_repair".into());
                return r;
            }
            Some(Ok(_)) => r.actions.push("harness_repair".into()),
        }
        if needs_proof {
            let state = self.fetch_state().await;
            let proof = latest_proof(&state.body["state"]);
            let v = validate(proof.as_ref(), Utc::now(), self.cfg.window_hours);
            r.actions
                .push("verify_news_post_proof_after_project_work".into());
            if !(state.ok && v.ok) {
                r.blocker = Some("proof_still_missing_after_project_work".into());
                return r;
            }
        }
        r.ok = true;
        r.healed_by = Some("DeepSeek Harness".into());
        r
    }
}

#[derive(Debug)]
pub enum AuditOutcome {
    Healthy,
    Duplicate(String),
    Incident(Box<(Incident, Repair)>),
}

/// Where the audit's messages and repairs go.
#[async_trait::async_trait]
pub trait Notifier: Send + Sync {
    async fn operator(&self, text: &str);
    async fn app(&self, text: &str, dedup_key: &str);
    /// `None`: no harness configured.
    async fn harness(&self, prompt: &str) -> Option<Result<String, String>>;
}

/// The live notifier: Telegram to the alert chat, `/v1/notify`, dsh-remote.
pub struct Live {
    pub st: AppState,
    pub alert_chat_id: Option<String>,
}

#[async_trait::async_trait]
impl Notifier for Live {
    async fn operator(&self, text: &str) {
        let (Some(token), Some(chat)) = (&self.st.cfg.telegram_token, &self.alert_chat_id) else {
            return;
        };
        let bot = lucina_telegram::BotApi::new(token.clone());
        let opts = lucina_telegram::SendOptions {
            disable_web_page_preview: true,
            ..Default::default()
        };
        if let Err(e) = bot.send_text(chat, &truncate(text, 3900), &opts).await {
            tracing::warn!(error = %e, "bot hospital: operator message failed");
        }
    }
    async fn app(&self, text: &str, dedup_key: &str) {
        let body = json!({"category": "system", "body": text, "route": "/", "channel": "other", "dedupKey": format!("bot-hospital:{dedup_key}")});
        let _ = crate::bridges::notify(&self.st.secretary, &body).await;
    }
    async fn harness(&self, prompt: &str) -> Option<Result<String, String>> {
        if !self.st.cfg.dsh.ready() {
            return None;
        }
        let http = reqwest::Client::new();
        Some(crate::jobs::run_harness(&http, &self.st.cfg.dsh, prompt).await)
    }
}

/// The daily loop (`BOT_HOSPITAL_ENABLED=true`, `BOT_HOSPITAL_DAILY_AUDIT_ENABLED`).
pub fn spawn(st: &AppState, hospital: Arc<Hospital>) -> Option<tokio::task::JoinHandle<()>> {
    if !(hospital.cfg.enabled && hospital.cfg.daily_audit_enabled) {
        return None;
    }
    let live = Live {
        st: st.clone(),
        alert_chat_id: hospital.cfg.alert_chat_id.clone(),
    };
    tracing::info!(
        interval_ms = hospital.cfg.interval.as_millis() as u64,
        initial_delay_ms = hospital.cfg.initial_delay.as_millis() as u64,
        "Bot Hospital daily news audit started"
    );
    let state = hospital.cfg.store_path.with_file_name("last-audit.json");
    Some(tokio::spawn(async move {
        let interval = hospital.cfg.interval.max(Duration::from_secs(60));
        // A restart does not audit again before the day is over (Node audited 5 min
        // after every start, so every deploy could alert the owner once more).
        let first = first_delay(
            read_last_audit(&state),
            Utc::now(),
            interval,
            hospital.cfg.initial_delay,
        );
        tracing::info!(
            first_in_s = first.as_secs(),
            "bot hospital: next audit scheduled"
        );
        tokio::time::sleep(first).await;
        let mut every = tokio::time::interval(interval);
        loop {
            every.tick().await;
            let now = Utc::now();
            write_last_audit(&state, now);
            let out = hospital.audit(&live, now).await;
            match &out {
                AuditOutcome::Healthy => {
                    tracing::info!("bot hospital daily news audit completed ok")
                }
                AuditOutcome::Duplicate(_) => {
                    tracing::info!("bot hospital daily news audit: duplicate incident")
                }
                AuditOutcome::Incident(b) => {
                    tracing::warn!(kind = %b.0.kind, healed = b.1.ok, "bot hospital daily news audit opened an incident")
                }
            }
        }
    }))
}

fn read_last_audit(path: &Path) -> Option<DateTime<Utc>> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    js_date(&v["lastAuditAt"])
}

fn write_last_audit(path: &Path, at: DateTime<Utc>) {
    let body = json!({"lastAuditAt": now_iso(at)}).to_string();
    if let Err(e) = lucina_store::atomic::write_atomic(path, body.as_bytes()) {
        tracing::warn!(error = %e, "bot hospital: last audit time not saved");
    }
}

/// Wait for the next audit: one interval after the last one, but at least the
/// start-up delay; no saved audit means the start-up delay.
pub fn first_delay(
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    interval: Duration,
    initial: Duration,
) -> Duration {
    let due = last
        .and_then(|l| chrono::Duration::from_std(interval).ok().map(|i| l + i))
        .map(|d| (d - now).to_std().unwrap_or_default())
        .unwrap_or_default();
    due.max(initial)
}

pub fn health_value(h: &Mutex<HospitalHealth>) -> Value {
    serde_json::to_value(h.lock().unwrap_or_else(|e| e.into_inner()).clone()).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-26T09:15:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn good_proof(posted_at: &str) -> Value {
        json!({"articleId": "a1", "title": "T", "postedAt": posted_at,
               "postedTargets": ["telegram", "discord"], "imageUrl": "https://x/i.png",
               "results": [{"target": "telegram", "ok": true, "messageId": "1"},
                           {"target": "discord", "ok": true, "messageId": "2"}]})
    }

    #[test]
    fn validation_rules() {
        assert!(validate(Some(&good_proof("2026-10-26T01:00:00Z")), now(), 26.0).ok);
        let v = validate(Some(&good_proof("2026-10-24T01:00:00Z")), now(), 26.0);
        assert_eq!(v.missing, vec!["no Poko news post within 26h"]);
        let mut p = good_proof("2026-10-26T01:00:00Z");
        p["imageUrl"] = json!("");
        p["results"][1]["messageId"] = json!("");
        p["postedTargets"] = json!(["telegram"]);
        let v = validate(Some(&p), now(), 26.0);
        assert_eq!(
            v.missing,
            vec![
                "missing discord target",
                "missing image URL",
                "missing discord message proof"
            ]
        );
        p["imagePolicy"] = json!({"imagePolicy": "seo_metadata_only"});
        let v = validate(Some(&p), now(), 26.0);
        assert_eq!(
            v.warnings,
            vec!["image intentionally omitted: seo_metadata_only"]
        );
        assert_eq!(
            validate(None, now(), 26.0).missing,
            vec!["no recorded Poko news post"]
        );
    }

    #[test]
    fn latest_proof_picks_newest_and_legacy() {
        let st = json!({"postHistory": [good_proof("2026-10-20T00:00:00Z"), {"postedAt": "2026-10-25T00:00:00Z", "title": "new"}]});
        assert_eq!(latest_proof(&st).unwrap()["title"], "new");
        let legacy = json!({"lastPostedAt": "2026-10-25T00:00:00Z", "lastArticle": {"title": "L"},
            "postedArticleTargets": {"x": ["telegram"], "y": ["telegram", "discord"]}});
        let p = latest_proof(&legacy).unwrap();
        assert_eq!(p["articleId"], "y");
        assert_eq!(p["legacy"], true);
        let v = validate(Some(&p), now(), 26.0);
        assert_eq!(v.warnings, vec!["image proof unavailable for legacy post"]);
        assert!(latest_proof(&json!({})).is_none());
    }

    #[derive(Default)]
    struct Rec {
        operator: Mutex<Vec<String>>,
        app: AtomicUsize,
        harness: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Notifier for Rec {
        async fn operator(&self, t: &str) {
            self.operator.lock().unwrap().push(t.to_owned());
        }
        async fn app(&self, _: &str, _: &str) {
            self.app.fetch_add(1, Ordering::SeqCst);
        }
        async fn harness(&self, _: &str) -> Option<Result<String, String>> {
            self.harness.fetch_add(1, Ordering::SeqCst);
            Some(Ok("done".into()))
        }
    }

    fn hospital(server: &MockServer, dir: &Path) -> Hospital {
        let get = |k: &str| match k {
            "BOT_HOSPITAL_ENABLED" => Some("true".to_owned()),
            "BOT_HOSPITAL_TOKEN" => Some("t0k".to_owned()),
            "BOT_HOSPITAL_POKO_NEWS_STATE_URL" => Some(format!("{}/state", server.uri())),
            "BOT_HOSPITAL_POKO_SCOUT_RUN_URL" => Some(format!("{}/run", server.uri())),
            "BOT_HOSPITAL_POKO_HEALTH_URL" => Some(format!("{}/health", server.uri())),
            "BOT_HOSPITAL_INCIDENT_STORE_PATH" => {
                Some("/app/data/bot-hospital/incidents.json".into())
            }
            "TELEGRAM_ALLOWED_CHAT_IDS" => Some("42,43".into()),
            _ => None,
        };
        Hospital {
            cfg: HospitalConfig::from_lookup(&get, dir),
            http: reqwest::Client::new(),
            health: Arc::default(),
        }
    }

    #[tokio::test]
    async fn healthy_audit_sends_nothing() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(method("GET"))
            .and(path("/state"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "state": {"postHistory": [good_proof("2026-10-26T01:00:00Z")]}}),
            ))
            .mount(&server)
            .await;
        let h = hospital(&server, dir.path());
        assert_eq!(h.cfg.alert_chat_id.as_deref(), Some("42"));
        assert_eq!(
            h.cfg.store_path,
            dir.path().join("bot-hospital/incidents.json")
        );
        let rec = Rec::default();
        assert!(matches!(h.audit(&rec, now()).await, AuditOutcome::Healthy));
        assert!(rec.operator.lock().unwrap().is_empty());
        assert!(!h.cfg.store_path.exists());
    }

    #[tokio::test]
    async fn stale_proof_opens_one_incident_retries_and_dedupes() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        Mock::given(method("GET"))
            .and(path("/state"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "state": {"postHistory": [good_proof("2026-10-20T01:00:00Z")]}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "posted": false, "reason": "no_new_tcg_article_above_threshold"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let h = hospital(&server, dir.path());
        let rec = Rec::default();
        let out = h.audit(&rec, now()).await;
        let AuditOutcome::Incident(b) = out else {
            panic!("expected an incident")
        };
        assert_eq!(b.0.kind, "news_post_partial_failure");
        assert_eq!(
            b.0.dedupe_key,
            "daily-news-audit:2026-10-26:no Poko news post within 26h"
        );
        // Proof still stale after the retry → harness repair → proof still missing.
        assert!(!b.1.ok);
        assert_eq!(
            b.1.blocker.as_deref(),
            Some("proof_still_missing_after_project_work")
        );
        assert_eq!(rec.harness.load(Ordering::SeqCst), 1);
        assert_eq!(rec.app.load(Ordering::SeqCst), 1);
        let msgs = rec.operator.lock().unwrap().clone();
        assert_eq!(msgs.len(), 3);
        assert!(msgs[0].starts_with("Bot Hospital: Poko is sick (news_post_partial_failure)."));
        assert!(msgs[2].starts_with("Bot Hospital final: Poko not healed."));
        // Same audit again inside the dedupe window: no second incident.
        let rec2 = Rec::default();
        assert!(matches!(
            h.audit(&rec2, now()).await,
            AuditOutcome::Duplicate(_)
        ));
        assert!(rec2.operator.lock().unwrap().is_empty());
        let store: Value =
            serde_json::from_slice(&std::fs::read(&h.cfg.store_path).unwrap()).unwrap();
        assert_eq!(store["incidents"].as_array().unwrap().len(), 1);
        let keys: Vec<&str> = store["incidents"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(keys.contains(&"nextAction") && keys.contains(&"dedupeKey"));
    }

    #[test]
    fn restarts_do_not_audit_again_within_the_day() {
        let day = Duration::from_secs(86_400);
        let five = Duration::from_secs(300);
        assert_eq!(first_delay(None, now(), day, five), five);
        let two_h_ago = now() - chrono::Duration::hours(2);
        assert_eq!(
            first_delay(Some(two_h_ago), now(), day, five),
            Duration::from_secs(22 * 3600)
        );
        let long_ago = now() - chrono::Duration::days(3);
        assert_eq!(first_delay(Some(long_ago), now(), day, five), five);
    }

    #[test]
    fn token_order_and_debug_hides_tokens() {
        let get = |k: &str| match k {
            "POKO_HOSPITAL_TOKEN" => Some("p".to_owned()),
            "BOT_HOSPITAL_TOKEN" => Some("b".to_owned()),
            _ => None,
        };
        let c = HospitalConfig::from_lookup(&get, Path::new("/d"));
        assert_eq!(c.tokens, vec!["b", "p"]);
        assert!(!format!("{c:?}").contains("\"b\""));
    }
}
