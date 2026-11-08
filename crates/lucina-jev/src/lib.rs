//! TypeSafe **Jev** client: a model that decides instead of writing.
//!
//! One call, `POST https://api.typesafe.ai/v1/systemone`, sends a `state`
//! (text or JSON) and named `questions`; every question is answered on its own
//! with a typed answer:
//!
//! - [`Question::choice`]: one of up to 255 named options, with probabilities
//!   and a confidence;
//! - [`Question::score`]: a level `0..n` of 2 to 10 ordered levels;
//! - [`Question::noul`]: the probability that the answer is yes.
//!
//! Jev reads text only and bills input tokens only (output is free), so Lucina
//! uses it for every routine decision (intent routing, the next click on a
//! page or phone screen, "is the task done?") and keeps her text model for the
//! words she says. Every caller has a fallback: a [`JevError`] means "decide
//! the old way", never "do nothing".
//!
//! - The key comes from `JEV_API_KEY` (Infisical `Lucina/prod#JEV_API_KEY_LUCINA`
//!   through infisical-brokerd) and is never logged.
//! - Quota, credit and auth failures open a breaker: for a while every call
//!   fails fast with [`JevError::CoolingDown`] and callers fall back at once.
//! - Each call is logged on `tracing` target `lucina_jev::usage` (purpose,
//!   latency, input tokens, outcome) and counted in [`Jev::stats`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

pub const DEFAULT_URL: &str = "https://api.typesafe.ai/v1/systemone";
/// Pinned, not `jev-latest`: a new release must not change Lucina's routing unseen.
pub const DEFAULT_MODEL: &str = "jev-1.14.0";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);
/// Jev takes up to 255 options per choice question.
pub const MAX_OPTIONS: usize = 255;
/// The default context is 64k tokens per request and 32k per question; keep
/// the state well under that (about 4 characters a token).
pub const MAX_STATE_CHARS: usize = 60_000;
/// Answers whose probabilities miss 1.0 by more than this are refused (the API caps it at 0.05).
const PROBABILITY_SLACK: f64 = 0.06;

/// How long the breaker stays open after each kind of failure.
const COOL_QUOTA: Duration = Duration::from_secs(120);
const COOL_CREDITS: Duration = Duration::from_secs(30 * 60);
const COOL_OVERLOADED: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum JevError {
    #[error("Jev is not configured (no JEV_API_KEY, or LUCINA_JEV=off)")]
    NotConfigured,
    #[error("Jev is cooling down after {0}")]
    CoolingDown(&'static str),
    #[error("Jev rate limit or quota (HTTP {0})")]
    Quota(u16),
    #[error("Jev credits exhausted or key refused (HTTP {0})")]
    Credits(u16),
    #[error("Jev overloaded (HTTP {0})")]
    Overloaded(u16),
    #[error("Jev refused the request (HTTP {status}): {detail}")]
    BadRequest { status: u16, detail: String },
    #[error("Jev request invalid before sending: {0}")]
    Invalid(String),
    #[error("Jev timed out")]
    Timeout,
    #[error("Jev transport error: {0}")]
    Transport(String),
    #[error("Jev answer malformed: {0}")]
    Malformed(String),
}

impl JevError {
    /// A short stable code for logs and stats.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::CoolingDown(_) => "cooling_down",
            Self::Quota(_) => "quota",
            Self::Credits(_) => "credits",
            Self::Overloaded(_) => "overloaded",
            Self::BadRequest { .. } => "bad_request",
            Self::Invalid(_) => "invalid",
            Self::Timeout => "timeout",
            Self::Transport(_) => "transport",
            Self::Malformed(_) => "malformed",
        }
    }
}

/// One question. Build with [`Question::choice`], [`Question::noul`] or [`Question::score`].
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    Choice {
        instructions: String,
        /// Option name → what it means. Order is kept for the caller's sake only.
        options: Vec<(String, String)>,
    },
    Noul {
        instructions: String,
        /// What "yes" and "no" mean, when the instructions alone are ambiguous.
        criteria: Option<(String, String)>,
    },
    Score {
        instructions: String,
        /// Level descriptions, lowest first (2 to 10).
        levels: Vec<String>,
    },
}

impl Question {
    pub fn choice<I, K, V>(instructions: impl Into<String>, options: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::Choice {
            instructions: instructions.into(),
            options: options
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }
    pub fn noul_with(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some((yes.into(), no.into())),
        }
    }
    pub fn score<I, S>(instructions: impl Into<String>, levels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Score {
            instructions: instructions.into(),
            levels: levels.into_iter().map(Into::into).collect(),
        }
    }

    pub fn validate(&self, name: &str) -> Result<(), JevError> {
        let bad = |m: String| Err(JevError::Invalid(format!("{name}: {m}")));
        match self {
            Self::Choice { options, .. } => {
                if options.len() < 2 || options.len() > MAX_OPTIONS {
                    return bad(format!("{} options (2..={MAX_OPTIONS})", options.len()));
                }
                let mut seen = std::collections::HashSet::new();
                if let Some((k, _)) = options.iter().find(|(k, _)| !seen.insert(k.as_str())) {
                    return bad(format!("duplicate option {k:?}"));
                }
                if options.iter().any(|(k, _)| k.trim().is_empty()) {
                    return bad("empty option name".into());
                }
            }
            Self::Score { levels, .. } if !(2..=10).contains(&levels.len()) => {
                return bad(format!("{} levels (2..=10)", levels.len()));
            }
            _ => {}
        }
        Ok(())
    }

    /// The question as JSON text, choice options in the caller's order.
    /// (`serde_json::Map` sorts keys without `preserve_order`, which this
    /// workspace does not enable; Jev leans towards the first option, so the
    /// order is the caller's to set.)
    fn to_wire_text(&self) -> String {
        match self {
            Self::Choice {
                instructions,
                options,
            } => {
                let criteria: Vec<String> = options
                    .iter()
                    .map(|(k, v)| format!("{}:{}", Value::from(k.as_str()), Value::from(v.as_str())))
                    .collect();
                format!(
                    "{{\"type\":\"choice\",\"instructions\":{},\"criteria\":{{{}}}}}",
                    Value::from(instructions.as_str()),
                    criteria.join(",")
                )
            }
            _ => self.to_wire().to_string(),
        }
    }

    fn to_wire(&self) -> Value {
        match self {
            Self::Choice {
                instructions,
                options,
            } => {
                let criteria: Map<String, Value> = options
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                json!({"type": "choice", "instructions": instructions, "criteria": criteria})
            }
            Self::Noul {
                instructions,
                criteria,
            } => {
                let mut q = json!({"type": "noul", "instructions": instructions});
                if let Some((yes, no)) = criteria {
                    q["criteria"] = json!({"true": yes, "false": no});
                }
                q
            }
            Self::Score {
                instructions,
                levels,
            } => json!({"type": "score", "instructions": instructions, "criteria": levels}),
        }
    }
}

/// A typed answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Choice {
        choice: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    /// Probability of "yes", 0..=1.
    Noul(f64),
    Score {
        score: u32,
        confidence: f64,
    },
}

/// Jev's answers to one call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Decision {
    /// The versioned model that answered (not the alias that was asked for).
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    pub input_tokens: u64,
    pub elapsed_ms: u64,
}

impl Decision {
    /// The chosen option and its confidence.
    pub fn choice(&self, name: &str) -> Option<(&str, f64)> {
        match self.answers.get(name)? {
            Answer::Choice {
                choice, confidence, ..
            } => Some((choice.as_str(), *confidence)),
            _ => None,
        }
    }
    /// The probability of one option of a choice question.
    pub fn probability(&self, name: &str, option: &str) -> Option<f64> {
        match self.answers.get(name)? {
            Answer::Choice { probabilities, .. } => probabilities.get(option).copied(),
            _ => None,
        }
    }
    /// Probability of "yes".
    pub fn yes(&self, name: &str) -> Option<f64> {
        match self.answers.get(name)? {
            Answer::Noul(p) => Some(*p),
            _ => None,
        }
    }
    pub fn score(&self, name: &str) -> Option<(u32, f64)> {
        match self.answers.get(name)? {
            Answer::Score { score, confidence } => Some((*score, *confidence)),
            _ => None,
        }
    }
}

/// Settings, normally from the environment ([`Config::from_lookup`]).
#[derive(Clone)]
pub struct Config {
    pub url: String,
    pub model: String,
    pub key: Option<String>,
    pub timeout: Duration,
    pub enabled: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Config {
    /// `JEV_API_KEY`, `LUCINA_JEV` (`off`/`0`/`false` disables), `LUCINA_JEV_URL`,
    /// `LUCINA_JEV_MODEL`, `LUCINA_JEV_TIMEOUT_MS`.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let val = |k: &str| get(k).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let enabled = !matches!(
            val("LUCINA_JEV").map(|v| v.to_ascii_lowercase()).as_deref(),
            Some("off" | "0" | "false" | "no")
        );
        Self {
            url: val("LUCINA_JEV_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            model: val("LUCINA_JEV_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into()),
            key: val("JEV_API_KEY"),
            timeout: val("LUCINA_JEV_TIMEOUT_MS")
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_TIMEOUT),
            enabled,
        }
    }
}

#[derive(Default)]
struct Counters {
    calls: AtomicU64,
    ok: AtomicU64,
    failed: AtomicU64,
    skipped: AtomicU64,
    input_tokens: AtomicU64,
}

/// The client. One per process: [`shared`].
pub struct Jev {
    cfg: Config,
    http: reqwest::Client,
    /// Breaker: no calls until this instant, and why.
    cool: Mutex<Option<(Instant, &'static str)>>,
    counters: Counters,
    last_error: Mutex<Option<String>>,
    by_purpose: Mutex<BTreeMap<&'static str, [u64; 2]>>,
}

impl Jev {
    pub fn new(cfg: Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(4))
            .build()
            .expect("reqwest client");
        Self {
            cfg,
            http,
            cool: Mutex::new(None),
            counters: Counters::default(),
            last_error: Mutex::new(None),
            by_purpose: Mutex::new(BTreeMap::new()),
        }
    }

    /// Configured and enabled (the breaker may still be open).
    pub fn configured(&self) -> bool {
        self.cfg.enabled && self.cfg.key.is_some()
    }

    /// Configured, enabled and not cooling down: worth trying now.
    pub fn available(&self) -> bool {
        self.configured() && self.cooling().is_none()
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    fn cooling(&self) -> Option<&'static str> {
        let mut g = self.cool.lock().unwrap();
        match *g {
            Some((until, why)) if Instant::now() < until => Some(why),
            Some(_) => {
                *g = None;
                None
            }
            None => None,
        }
    }

    fn open_breaker(&self, d: Duration, why: &'static str) {
        *self.cool.lock().unwrap() = Some((Instant::now() + d, why));
    }

    /// Ask `questions` about `state`. `purpose` names the caller in logs and stats
    /// (`intent`, `browse`, `android`, …).
    pub async fn decide(
        &self,
        purpose: &'static str,
        state: Value,
        questions: &[(&str, Question)],
    ) -> Result<Decision, JevError> {
        let r = self.decide_inner(purpose, state, questions).await;
        let mut bp = self.by_purpose.lock().unwrap();
        let e = bp.entry(purpose).or_insert([0, 0]);
        match &r {
            Ok(_) => e[0] += 1,
            Err(_) => e[1] += 1,
        }
        r
    }

    async fn decide_inner(
        &self,
        purpose: &'static str,
        state: Value,
        questions: &[(&str, Question)],
    ) -> Result<Decision, JevError> {
        let Some(key) = self.cfg.key.as_deref().filter(|_| self.cfg.enabled) else {
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return Err(JevError::NotConfigured);
        };
        if let Some(why) = self.cooling() {
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return Err(JevError::CoolingDown(why));
        }
        if questions.is_empty() {
            return Err(JevError::Invalid("no questions".into()));
        }
        for (name, q) in questions {
            q.validate(name)?;
        }
        let body = request_body(&self.cfg.model, clip_state(state), questions);
        self.counters.calls.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let out = self.send(key, &body).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let out = out.and_then(|v| parse_decision(&v, questions)).map(|mut d| {
            d.elapsed_ms = elapsed_ms;
            d
        });
        match &out {
            Ok(d) => {
                self.counters.ok.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .input_tokens
                    .fetch_add(d.input_tokens, Ordering::Relaxed);
                tracing::info!(target: "lucina_jev::usage", purpose, model = %d.model, ms = elapsed_ms,
                    input_tokens = d.input_tokens, questions = questions.len(), "jev ok");
            }
            Err(e) => {
                self.counters.failed.fetch_add(1, Ordering::Relaxed);
                *self.last_error.lock().unwrap() = Some(format!("{purpose}: {e}"));
                tracing::warn!(target: "lucina_jev::usage", purpose, ms = elapsed_ms, error = %e,
                    code = e.code(), "jev failed; caller falls back");
            }
        }
        out
    }

    async fn send(&self, key: &str, body: &str) -> Result<Value, JevError> {
        let resp = self
            .http
            .post(&self.cfg.url)
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    JevError::Timeout
                } else {
                    JevError::Transport(e.without_url().to_string())
                }
            })?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return resp
                .json::<Value>()
                .await
                .map_err(|e| JevError::Malformed(e.without_url().to_string()));
        }
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let detail: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        Err(match status {
            429 => {
                self.open_breaker(
                    retry_after.unwrap_or(COOL_QUOTA).min(COOL_CREDITS),
                    "rate limit",
                );
                JevError::Quota(status)
            }
            401..=403 => {
                self.open_breaker(COOL_CREDITS, "credits or key refused");
                JevError::Credits(status)
            }
            500..=599 => {
                self.open_breaker(COOL_OVERLOADED, "overload");
                JevError::Overloaded(status)
            }
            _ => JevError::BadRequest { status, detail },
        })
    }

    /// Counters for status pages: calls, failures, fallbacks, tokens, breaker.
    pub fn stats(&self) -> Value {
        let c = &self.counters;
        let by: Map<String, Value> = self
            .by_purpose
            .lock()
            .unwrap()
            .iter()
            .map(|(k, [ok, failed])| ((*k).to_owned(), json!({"ok": ok, "failed": failed})))
            .collect();
        json!({
            "configured": self.configured(),
            "model": self.cfg.model,
            "calls": c.calls.load(Ordering::Relaxed),
            "ok": c.ok.load(Ordering::Relaxed),
            "failed": c.failed.load(Ordering::Relaxed),
            "skipped": c.skipped.load(Ordering::Relaxed),
            "inputTokens": c.input_tokens.load(Ordering::Relaxed),
            "coolingDown": self.cooling(),
            "lastError": self.last_error.lock().unwrap().clone(),
            "byPurpose": by,
        })
    }
}

/// The request as JSON text: questions and choice options in the caller's order.
pub fn request_body(model: &str, state: Value, questions: &[(&str, Question)]) -> String {
    let qs: Vec<String> = questions
        .iter()
        .map(|(name, q)| format!("{}:{}", Value::from(*name), q.to_wire_text()))
        .collect();
    format!(
        "{{\"model\":{},\"state\":{},\"questions\":{{{}}}}}",
        Value::from(model),
        state,
        qs.join(",")
    )
}

fn clip_state(state: Value) -> Value {
    match state {
        Value::String(s) if s.chars().count() > MAX_STATE_CHARS => {
            Value::String(clip(&s, MAX_STATE_CHARS))
        }
        Value::String(s) => Value::String(s),
        other => {
            let text = other.to_string();
            if text.chars().count() > MAX_STATE_CHARS {
                Value::String(clip(&text, MAX_STATE_CHARS))
            } else {
                other
            }
        }
    }
}

/// The first `max` characters, cut at a character boundary, with a marker.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(12)).collect();
    out.push_str(" […clipped]");
    out
}

fn prob(v: &Value, what: &str) -> Result<f64, JevError> {
    let p = v
        .as_f64()
        .ok_or_else(|| JevError::Malformed(format!("{what}: not a number")))?;
    if !(0.0..=1.0).contains(&p) {
        return Err(JevError::Malformed(format!("{what}: {p} outside 0..1")));
    }
    Ok(p)
}

fn probabilities(v: &Value, what: &str) -> Result<BTreeMap<String, f64>, JevError> {
    let Some(m) = v.as_object() else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (k, p) in m {
        out.insert(k.clone(), prob(p, what)?);
    }
    let sum: f64 = out.values().sum();
    if !out.is_empty() && (sum - 1.0).abs() > PROBABILITY_SLACK {
        return Err(JevError::Malformed(format!(
            "{what}: probabilities sum to {sum:.3}"
        )));
    }
    Ok(out)
}

/// Parse and check a response against the questions asked: every question must
/// be answered with its own type, and a choice must name one of its options.
/// A missing or wrong answer is an error, never a default.
pub fn parse_decision(v: &Value, questions: &[(&str, Question)]) -> Result<Decision, JevError> {
    let answers = v["answers"]
        .as_object()
        .ok_or_else(|| JevError::Malformed("no answers object".into()))?;
    let mut out = BTreeMap::new();
    for (name, q) in questions {
        let a = answers
            .get(*name)
            .ok_or_else(|| JevError::Malformed(format!("{name}: missing")))?;
        let answer = match q {
            Question::Choice { options, .. } => {
                let choice = a["choice"]
                    .as_str()
                    .ok_or_else(|| JevError::Malformed(format!("{name}: no choice")))?;
                if !options.iter().any(|(k, _)| k == choice) {
                    return Err(JevError::Malformed(format!(
                        "{name}: unknown option {choice:?}"
                    )));
                }
                let probabilities = probabilities(&a["probabilities"], name)?;
                let confidence = match a.get("confidence") {
                    Some(c) if !c.is_null() => prob(c, name)?,
                    _ => probabilities.get(choice).copied().unwrap_or(0.0),
                };
                Answer::Choice {
                    choice: choice.to_owned(),
                    confidence,
                    probabilities,
                }
            }
            Question::Noul { .. } => Answer::Noul(prob(&a["noul"], name)?),
            Question::Score { levels, .. } => {
                // The live API answers the expected level (`3.77`), not an integer: the nearest level.
                let score = a["score"]
                    .as_u64()
                    .or_else(|| {
                        a["score"]
                            .as_f64()
                            .filter(|s| s.is_finite() && *s >= 0.0)
                            .map(|s| s.round() as u64)
                    })
                    .ok_or_else(|| JevError::Malformed(format!("{name}: no score")))?;
                if score as usize >= levels.len() {
                    return Err(JevError::Malformed(format!(
                        "{name}: score {score} out of range"
                    )));
                }
                let confidence = match a.get("confidence") {
                    Some(c) if !c.is_null() => prob(c, name)?,
                    _ => 0.0,
                };
                Answer::Score {
                    score: score as u32,
                    confidence,
                }
            }
        };
        out.insert((*name).to_owned(), answer);
    }
    Ok(Decision {
        model: v["model"].as_str().unwrap_or_default().to_owned(),
        answers: out,
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        elapsed_ms: 0,
    })
}

/// The process-wide client (settings from the environment at first use).
pub fn shared() -> &'static Jev {
    static J: OnceLock<Jev> = OnceLock::new();
    J.get_or_init(|| Jev::new(Config::from_lookup(&|k: &str| std::env::var(k).ok())))
}

#[cfg(test)]
mod tests;
