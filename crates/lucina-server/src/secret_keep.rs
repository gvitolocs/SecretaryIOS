//! Lucina moves a secret into Infisical herself: from a note or a chat message, through the local
//! model only, then clears it from where it was (design: docs/design/secret-keep.md).
//!
//! The cloud model calls `secret_keep` with a **reference** (`note:<slug>`, `chat:<item id>`) and
//! names, and gets back names and an outcome. Everything that touches the value runs in this file,
//! in a queue worker that has no way to reach a cloud model:
//!
//! read the source; the local Qwen picks the value ([`local`], loopback only, nothing else to fall
//! back to); the value must stand verbatim in the source; broker op `deposit` (create-only: it can't
//! change or read what exists) with its read-back hash compared here; only then the value leaves
//! the note and the stored chats, and the note is read again.
//!
//! If the local model is busy, stopped or failing, the job waits and retries, and in the end fails
//! with `local_model_unavailable`. No step falls through to a cloud model, the source stays as it
//! was, and the owner hears about it.
//!
//! Nothing here writes a value: the queue file holds references and names, errors are codes, the
//! local model's answers and the source's text are never logged or returned.
//!
//! "Auto add": a value found in the owner's own message ([`crate::owner_secrets::mask_message`], a
//! deterministic pattern, before the model and every sink) is judged and named by the local model and
//! saved on its own; when the local model can't, the old "Save as secret?" card comes instead. Notes
//! are swept for secret shapes ([`sweep`]) and offered: a note is the owner's text, so moving one
//! out starts with his tap.

pub mod local;
pub mod sweep;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_secrets::SecretValue;
use lucina_store::timeline::TimelineStore;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use zeroize::{Zeroize, Zeroizing};

use crate::api::ApiError;
use crate::bridges::Secretary;
use crate::owner_secrets::{broker_call, lucina_item, request_folder, slug_ok};

/// Shortest value the store-wide clear looks for: anything shorter (a PIN) could be part of
/// anything, so only its own source is cleared.
pub const MIN_GLOBAL_CHARS: usize = 8;
/// Shortest value at all.
pub const MIN_VALUE_CHARS: usize = 4;
/// Longest value: a pasted key file fits, a whole document doesn't (the broker's own limit).
pub const MAX_VALUE_BYTES: usize = 65_536;
/// Waits between tries while the local model or the broker can't answer.
const BACKOFF: [u64; 6] = [60, 120, 300, 600, 1200, 1800];
/// Finished jobs kept for `status`.
const MAX_CLOSED: usize = 200;
const FILE: &str = "secret-keep.json";

#[derive(Debug, Clone)]
pub struct KeepConfig {
    /// `LUCINA_SECRET_KEEP=off` turns the tools off. On when the token file is set.
    pub enabled: bool,
    /// `LUCINA_BROKER_KEEPER_TOKEN_FILE`: a caller with `deposit` grants only.
    pub token_file: Option<PathBuf>,
    /// `LUCINA_BROKER_SOCKET`.
    pub socket: PathBuf,
    pub local: local::LocalConfig,
    /// `LUCINA_SECRET_KEEP_PLACE`, default `personal/prod`: where a secret with no place goes.
    pub default_project: String,
    pub default_environment: String,
    /// `LUCINA_SECRET_KEEP_AUTO=off`: a value in a chat message gets the card, not a save.
    pub auto: bool,
    /// How long a job waits for the local model or the broker.
    pub retry_for: Duration,
    /// How long an automatic save of a held value waits before the card comes instead.
    pub auto_wait: Duration,
    /// `LUCINA_SECRET_KEEP_BACKUP_DIR`: dated copies of the note store (`<dir>/<day>/<note id>.json`).
    pub backup_dir: Option<PathBuf>,
    /// `LUCINA_SECRET_KEEP_SCRUB_DIRS` (`:`): other trees cleared of the value (JSON and text).
    pub scrub_dirs: Vec<PathBuf>,
    /// `LUCINA_SECRET_KEEP_SWEEP_SECS`, default an hour, 0 off.
    pub sweep: Option<Duration>,
}

impl KeepConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let token_file = get("LUCINA_BROKER_KEEPER_TOKEN_FILE")
            .map(|p| PathBuf::from(p.trim()))
            .filter(|p| !p.as_os_str().is_empty());
        let off = |k: &str| get(k).is_some_and(|v| v.trim().eq_ignore_ascii_case("off"));
        let place = get("LUCINA_SECRET_KEEP_PLACE").unwrap_or_else(|| "personal/prod".into());
        let (project, environment) = place.trim().split_once('/').unwrap_or(("personal", "prod"));
        let scrub_dirs = match get("LUCINA_SECRET_KEEP_SCRUB_DIRS") {
            Some(v) => v.split(':').filter(|p| !p.trim().is_empty()).map(|p| PathBuf::from(p.trim())).collect(),
            None => {
                let mut dirs = vec![data_dir.join("lucina-rs"), data_dir.join("decisions"), data_dir.join("ops")];
                dirs.extend(get("LUCINA_OPS_VAULT_DIR").map(PathBuf::from));
                dirs
            }
        };
        let sweep = get("LUCINA_SECRET_KEEP_SWEEP_SECS").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(3600);
        Self {
            enabled: token_file.is_some() && !off("LUCINA_SECRET_KEEP"),
            token_file,
            socket: get("LUCINA_BROKER_SOCKET")
                .map(|p| PathBuf::from(p.trim()))
                .unwrap_or_else(|| PathBuf::from(crate::owner_secrets::DEFAULT_SOCKET)),
            local: local::LocalConfig::from_lookup(get),
            default_project: project.to_lowercase(),
            default_environment: environment.to_owned(),
            auto: !off("LUCINA_SECRET_KEEP_AUTO"),
            retry_for: Duration::from_secs(6 * 3600),
            auto_wait: Duration::from_secs(360),
            backup_dir: get("LUCINA_SECRET_KEEP_BACKUP_DIR").map(PathBuf::from),
            scrub_dirs,
            sweep: (sweep > 0).then(|| Duration::from_secs(sweep)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Saved,
    Failed,
}

/// Where the value was left and what could not be cleared. Counts and names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cleared {
    pub note: bool,
    pub chat_items: usize,
    pub recent_turns: usize,
    pub files: usize,
    pub backups: usize,
    /// Where the value may still be, in words (never the value).
    pub remains: Vec<String>,
}

/// One job: references and names, never a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    /// `note:<slug>`, `chat:<item id>` or `held:<held id>`.
    pub source: String,
    pub name: String,
    pub project: String,
    pub environment: String,
    pub path: String,
    /// Which secret, in words, for the local model; never the value.
    pub what: String,
    /// The value is the note's whole text.
    #[serde(default)]
    pub whole: bool,
    /// Found on its own (a value in a message): the local model judges it and picks the name.
    #[serde(default)]
    pub auto: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default)]
    pub tries: u32,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared: Option<Cleared>,
    #[serde(default)]
    pub lang: String,
}

impl Job {
    /// What goes back to the model or the CLI: names and the outcome.
    pub fn outcome(&self) -> Value {
        let mut v = json!({
            "job": self.id,
            "state": self.state,
            "name": self.name,
            "project": self.project,
            "environment": self.environment,
            "path": self.path,
            "source": self.source,
        });
        if let Some(c) = &self.code {
            v["code"] = json!(c);
        }
        if let Some(c) = &self.cleared {
            v["cleared"] = serde_json::to_value(c).unwrap_or(Value::Null);
        }
        v
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn random_id() -> String {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).expect("OS random");
    format!("sk-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// A failed step: a code for the owner and whether trying later can help.
#[derive(Debug)]
struct Stop {
    code: &'static str,
    retry: bool,
}

fn stop(code: &'static str) -> Stop {
    Stop { code, retry: false }
}

fn wait(code: &'static str) -> Stop {
    Stop { code, retry: true }
}

pub struct Keeper {
    pub cfg: KeepConfig,
    file: PathBuf,
    timeline: Arc<TimelineStore>,
    secretary: Secretary,
    jobs: Mutex<Option<Vec<Job>>>,
    /// New work for the one worker.
    wake: Notify,
    http: reqwest::Client,
}

impl std::fmt::Debug for Keeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keeper").field("file", &self.file).finish_non_exhaustive()
    }
}

impl Keeper {
    pub fn new(data_dir: &Path, cfg: KeepConfig, timeline: Arc<TimelineStore>, secretary: Secretary) -> Self {
        Self {
            cfg,
            file: data_dir.join(FILE),
            timeline,
            secretary,
            jobs: Mutex::new(None),
            wake: Notify::new(),
            http: reqwest::Client::new(),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Vec<Job>) -> T) -> T {
        let mut guard = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let jobs = guard.get_or_insert_with(|| self.load());
        let out = f(jobs);
        self.persist(jobs);
        out
    }

    fn load(&self) -> Vec<Job> {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %self.file.display(), error = %e, "secret keep queue unreadable: starting empty");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        }
    }

    fn persist(&self, jobs: &[Job]) {
        let Ok(bytes) = serde_json::to_vec(jobs) else { return };
        if let Err(e) = lucina_store::atomic::write_atomic(&self.file, &bytes) {
            tracing::warn!(path = %self.file.display(), error = %e.kind(), "secret keep queue not written");
        }
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        self.with(|jobs| jobs.iter().find(|j| j.id == id).cloned())
    }

    /// Put a job in the queue. `body`: `{source, name?, project?, environment?, path?, what?, whole?, request?}`.
    pub fn enqueue(&self, body: &Value) -> Result<Job, ApiError> {
        if !self.cfg.enabled {
            return Err(ApiError::new(503, "secret_keep_off", "saving secrets locally is not configured"));
        }
        let bad = |m: &str| ApiError::new(400, "bad_request", m);
        let text = |k: &str| body[k].as_str().map(str::trim).unwrap_or("");
        let source = text("source");
        let ok_source = match source.split_once(':') {
            Some(("note", k)) => !k.is_empty() && k.len() <= 128,
            Some(("chat", k)) => lucina_store::ulid::is_timeline_id(k),
            _ => false,
        };
        if !ok_source {
            return Err(bad("source: note:<slug> or chat:<item id>"));
        }
        let what = text("what");
        // The hint is the cloud model's words: a value there would already have left.
        if what.chars().count() > 300 || !lucina_secrets::redact::detect(what).is_empty() || !lucina_secrets::labelled::detect(what).is_empty() {
            return Err(ApiError::new(400, "value_refused", "what: say which secret in words, without the value"));
        }
        let project = match text("project") {
            "" => self.cfg.default_project.clone(),
            p if slug_ok(p, 128) => p.to_lowercase(),
            _ => return Err(bad("project: letters, digits, _ . @ -")),
        };
        let environment = match text("environment") {
            "" => self.cfg.default_environment.clone(),
            e if slug_ok(e, 128) => e.to_owned(),
            _ => return Err(bad("environment: letters, digits, _ . @ -")),
        };
        let path = match text("path") {
            "" => "/".to_owned(),
            p => request_folder(p).ok_or_else(|| bad("path: folders of letters, digits, _ . @ -"))?,
        };
        let name = text("name");
        if name.is_empty() || !slug_ok(name, 256) {
            return Err(bad("name: required, letters, digits, _ . @ -"));
        }
        let request = text("request");
        let job = Job {
            id: random_id(),
            source: source.to_owned(),
            name: name.to_owned(),
            project,
            environment,
            path,
            what: what.to_owned(),
            whole: body["whole"].as_bool().unwrap_or(false),
            auto: false,
            request: (!request.is_empty() && request.len() <= 64).then(|| request.to_owned()),
            state: State::Queued,
            code: None,
            tries: 0,
            created_at: now(),
            retry_at: None,
            cleared: None,
            lang: text("lang").to_owned(),
        };
        if job.whole && !job.source.starts_with("note:") {
            return Err(bad("whole: only a note"));
        }
        self.add(job.clone());
        Ok(job)
    }

    fn add(&self, job: Job) {
        tracing::info!(job = %job.id, name = %job.name, project = %job.project, environment = %job.environment, source = %job.source, "secret keep queued");
        self.with(|jobs| {
            jobs.push(job);
            let closed = jobs.iter().filter(|j| j.state != State::Queued).count();
            let mut drop = closed.saturating_sub(MAX_CLOSED);
            jobs.retain(|j| {
                if drop > 0 && j.state != State::Queued {
                    drop -= 1;
                    return false;
                }
                true
            });
        });
        self.wake.notify_one();
    }

    fn next_due(&self) -> Option<Job> {
        let t = now();
        self.with(|jobs| jobs.iter().find(|j| j.state == State::Queued && j.retry_at.is_none_or(|r| r <= t)).cloned())
    }

    fn finish(&self, id: &str, f: impl FnOnce(&mut Job)) -> Option<Job> {
        self.with(|jobs| {
            let j = jobs.iter_mut().find(|j| j.id == id)?;
            f(j);
            j.what.clear();
            Some(j.clone())
        })
    }

    /// Run the oldest due job. False: nothing was due.
    pub async fn run_next(&self, st: &crate::AppState) -> bool {
        let Some(job) = self.next_due() else { return false };
        let outcome = self.process(st, &job).await;
        match outcome {
            Ok((created_name, cleared)) => {
                let name = created_name.clone();
                let done = self.finish(&job.id, |j| {
                    j.state = State::Saved;
                    j.code = None;
                    j.name = created_name;
                    j.retry_at = None;
                    j.cleared = Some(cleared);
                });
                tracing::info!(job = %job.id, name = %name, "secret kept");
                if let Some(j) = done {
                    self.tell_saved(st, &j);
                }
            }
            Err(s) => {
                let waited = now() - job.created_at;
                let limit = if job.auto { self.cfg.auto_wait } else { self.cfg.retry_for }.as_secs() as i64;
                let delay = BACKOFF[(job.tries as usize).min(BACKOFF.len() - 1)];
                // A held value has minutes; a note can wait hours.
                let delay = if job.auto { delay.min(60) } else { delay };
                if s.retry && waited < limit {
                    tracing::info!(job = %job.id, code = s.code, delay, "secret keep waits");
                    self.with(|jobs| {
                        if let Some(j) = jobs.iter_mut().find(|j| j.id == job.id) {
                            j.tries += 1;
                            j.code = Some(s.code.to_owned());
                            j.retry_at = Some(now() + delay as i64);
                        }
                    });
                } else {
                    tracing::warn!(job = %job.id, name = %job.name, code = s.code, "secret keep failed");
                    let done = self.finish(&job.id, |j| {
                        j.state = State::Failed;
                        j.code = Some(s.code.to_owned());
                        j.retry_at = None;
                    });
                    if let Some(j) = done {
                        self.tell_failed(st, &j);
                    }
                }
            }
        }
        true
    }

    /// Everything that touches the value. Returns the name it was saved under.
    async fn process(&self, st: &crate::AppState, job: &Job) -> Result<(String, Cleared), Stop> {
        let (kind, key) = job.source.split_once(':').ok_or(stop("bad_request"))?;
        if !self.cfg.local.is_loopback() {
            return Err(stop("local_model_not_local"));
        }
        // The value, and for a note the note as it is now.
        let mut note: Option<Value> = None;
        let mut name = job.name.clone();
        let value: SecretValue = match kind {
            "held" => {
                let value = st.held_secrets.get(key).ok_or(stop("held_expired"))?;
                // A held value has minutes: a stuck slot must not outlast it.
                let quick = local::LocalConfig { timeout: self.cfg.local.timeout.min(Duration::from_secs(240)), ..self.cfg.local.clone() };
                let verdict = local::judge(&self.http, &quick, &job.what, value.expose_secret()).await.map_err(local_stop)?;
                if !verdict.credential {
                    st.held_secrets.drop_id(key);
                    return Err(stop("not_a_secret"));
                }
                name = format!("KEPT_{}", verdict.name);
                value
            }
            "note" => {
                let n = self.read_note(key).await?;
                let value = if job.whole {
                    let text = Zeroizing::new(note_text(&n));
                    SecretValue::new(text.trim().to_owned())
                } else {
                    let text = Zeroizing::new(note_text(&n));
                    let v = local::extract(&self.http, &self.cfg.local, &job.name, &job.what, &text).await.map_err(local_stop)?;
                    check_verbatim(&v, &text)?;
                    v
                };
                note = Some(n);
                value
            }
            "chat" => {
                let text = Zeroizing::new(self.read_chat(key)?);
                let v = local::extract(&self.http, &self.cfg.local, &job.name, &job.what, &text).await.map_err(local_stop)?;
                check_verbatim(&v, &text)?;
                v
            }
            _ => return Err(stop("bad_request")),
        };
        if value.expose_secret().chars().count() < MIN_VALUE_CHARS {
            return Err(stop("no_secret"));
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(stop("too_long"));
        }
        // A placeholder from an earlier clear is not a value.
        if value.expose_secret().contains("[secret:") {
            return Err(stop("no_secret"));
        }
        let name = self.deposit(job, name, &value).await?;
        let cleared = self.clear(st, job, &name, &value, note.as_ref()).await;
        if kind == "held" {
            st.held_secrets.drop_id(key);
        }
        if let Err(why) = lucina_secrets::redactor().try_register(&name, value.expose_secret()) {
            tracing::warn!(name = %name, error = %why, "kept secret not registered with the redactor");
        }
        Ok((name, cleared))
    }

    async fn read_note(&self, key: &str) -> Result<Value, Stop> {
        let r = self.secretary.call(Method::GET, &format!("/v1/notes/{}", crate::bridges::enc(key)), None).await;
        if r.ok {
            return Ok(r.data);
        }
        Err(match r.status {
            404 => stop("source_not_found"),
            0 | 500..=599 => wait("notes_unavailable"),
            _ => stop("notes_refused"),
        })
    }

    fn read_chat(&self, id: &str) -> Result<String, Stop> {
        let item = self.timeline.get(id).map_err(|_| wait("timeline_unavailable"))?.ok_or(stop("source_not_found"))?;
        let mut v: Value = serde_json::from_str(&item.raw).map_err(|_| stop("source_not_found"))?;
        let text = v["text"].as_str().unwrap_or_default().to_owned();
        crate::owner_secrets::wipe_value(&mut v);
        Ok(text)
    }

    /// Broker op `deposit`, then the read-back compared here. `Err` leaves the source untouched.
    async fn deposit(&self, job: &Job, name: String, value: &SecretValue) -> Result<String, Stop> {
        let file = self.cfg.token_file.as_ref().ok_or(stop("secret_keep_off"))?;
        let mut raw = std::fs::read_to_string(file).map_err(|_| stop("broker_token_unreadable"))?;
        let token = raw.trim().to_owned();
        raw.zeroize();
        lucina_secrets::redactor().register("LUCINA_BROKER_KEEPER_TOKEN", &token);
        let token = SecretValue::new(token);
        let folder = if job.path == "/" { String::new() } else { job.path.clone() };
        let want = Sha256::digest(value.expose_secret().as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>();
        // A found secret has no name anyone picked: another value under it moves to the next.
        let tries = if job.auto { 5 } else { 1 };
        for i in 0..tries {
            let key = if i == 0 { name.clone() } else { format!("{name}_{}", i + 1) };
            let reference = format!("secret://{}/{}{folder}#{key}", job.project, job.environment);
            let req = json!({"token": token.expose_secret(), "op": "deposit", "ref": reference,
                             "value": value.expose_secret(), "create_folder": true});
            match broker_call(&self.cfg.socket, req).await {
                Ok(answer) => {
                    if answer["readback_sha256"].as_str() != Some(want.as_str()) {
                        return Err(stop("readback_mismatch"));
                    }
                    return Ok(key);
                }
                Err((code, _)) if code == "exists" => {
                    if i + 1 == tries {
                        return Err(stop("exists"));
                    }
                }
                Err((code, _)) => {
                    return Err(match code.as_str() {
                        "broker_unavailable" | "upstream_unavailable" | "upstream_error" => wait("broker_unavailable"),
                        "not_allowed" | "unauthorized" => stop("not_allowed"),
                        "bad_ref" | "bad_request" => stop("bad_request"),
                        "not_found" | "unknown_project" | "unknown_environment" => stop("unknown_place"),
                        _ => stop("broker_refused"),
                    });
                }
            }
        }
        Err(stop("exists"))
    }

    /// After a confirmed save. Never fails: what it could not clear goes in `remains`, and the save
    /// stands. Clears the value only: the rest of the note and of each message stays.
    async fn clear(&self, st: &crate::AppState, job: &Job, name: &str, value: &SecretValue, note: Option<&Value>) -> Cleared {
        let mut out = Cleared::default();
        let placeholder = lucina_secrets::redact::placeholder(name);
        let raw = value.expose_secret().to_owned();
        let raw = Zeroizing::new(raw);
        let encodings = Zeroizing::new(lucina_secrets::redact::encodings(&raw));
        let global = raw.chars().count() >= MIN_GLOBAL_CHARS;
        if let Some(n) = note {
            out.note = self.clear_note(n, job.whole, name, &raw, &placeholder, &mut out.remains).await;
            if let (true, Some(id), Some(dir)) = (global, n["id"].as_str(), self.cfg.backup_dir.as_ref()) {
                match clear_backups(dir, id, &encodings, &placeholder) {
                    Ok(n) => out.backups = n,
                    Err(e) => {
                        tracing::warn!(error = %e.kind(), "note backups not cleared");
                        out.remains.push("the dated note backups could not be cleared".into());
                    }
                }
            }
        }
        // The chats: everywhere for a long value; a short one only in the message it came from.
        let only: Option<Vec<String>> = (!global).then(|| {
            job.source.strip_prefix("chat:").map(str::to_owned).into_iter().collect()
        });
        for timeline in [&self.timeline, &st.selftest_timeline] {
            match timeline.scrub(&encodings, &placeholder, only.as_deref()) {
                Ok(n) => out.chat_items += n,
                Err(e) => {
                    tracing::warn!(error = %e, "timeline not cleared");
                    out.remains.push("a stored chat could not be cleared".into());
                }
            }
        }
        if global {
            out.recent_turns += st.recent.scrub(&encodings, &placeholder);
            out.recent_turns += st.telegram_recent.scrub(&encodings, &placeholder);
            for dir in &self.cfg.scrub_dirs {
                match lucina_store::scrub::scrub_tree(dir, &encodings, &placeholder) {
                    Ok(n) => out.files += n,
                    Err(e) => {
                        tracing::warn!(error = %e.kind(), dir = %dir.display(), "a data folder was not cleared");
                        out.remains.push(format!("{} could not be cleared", dir.display()));
                    }
                }
            }
        } else {
            out.remains.push("the value is too short to clear from other stored chats by itself".into());
        }
        out
    }

    /// The value out of the note, then read the note again. `whole`: the text becomes a pointer.
    async fn clear_note(&self, note: &Value, whole: bool, name: &str, raw: &str, placeholder: &str, remains: &mut Vec<String>) -> bool {
        let key = note["id"].as_str().or(note["slug"].as_str()).unwrap_or_default().to_owned();
        let path = format!("/v1/notes/{}", crate::bridges::enc(&key));
        let needle = [raw.to_owned()];
        let mut current = note.clone();
        for _ in 0..3 {
            let mut body = json!({"source": "lucina"});
            if let Some(at) = current["updatedAt"].as_str() {
                body["baseUpdatedAt"] = json!(at);
            }
            if whole {
                let pointer = format!("{placeholder} (moved to Infisical)");
                if current["items"].as_array().is_some() {
                    body["items"] = json!([{"text": pointer}]);
                } else {
                    body["content"] = json!(pointer);
                }
            } else if let Some(items) = current["items"].as_array() {
                let mut items = items.clone();
                for it in &mut items {
                    lucina_store::scrub::scrub_value(&mut it["text"], &needle, placeholder);
                }
                body["items"] = Value::Array(items);
            } else if let Some(text) = current["content"].as_str().and_then(|c| lucina_store::scrub::scrub_str(c, &needle, placeholder)) {
                body["content"] = json!(text);
            }
            if let Some(title) = current["title"].as_str().and_then(|t| lucina_store::scrub::scrub_str(t, &needle, placeholder)) {
                body["title"] = json!(title);
            }
            // A slug made from a first line that held the value: the name goes too.
            if slug_holds_value(current["slug"].as_str().unwrap_or_default(), raw) {
                let id = current["id"].as_str().unwrap_or_default().trim_start_matches("n_");
                body["slug"] = json!(format!("kept-{}", id.chars().take(10).collect::<String>()));
            }
            let r = self.secretary.call(Method::PATCH, &path, Some(&body)).await;
            crate::owner_secrets::wipe_value(&mut body);
            if r.ok {
                // Trust what the store says now, not what it answered.
                let back = self.secretary.call(Method::GET, &path, None).await;
                if back.ok && !back.data.to_string().contains(&raw_json(raw)) && !note_text(&back.data).contains(raw) {
                    return true;
                }
                break;
            }
            if r.status != 409 {
                break;
            }
            // Edited meanwhile: read it again and clear that.
            let again = self.secretary.call(Method::GET, &path, None).await;
            if !again.ok {
                break;
            }
            current = again.data;
        }
        tracing::warn!(note = %key, name = %name, "the note was not cleared");
        remains.push(format!("note {key} still holds it: clearing failed after the save"));
        false
    }

    fn tell_saved(&self, st: &crate::AppState, j: &Job) {
        let cleared = j.cleared.clone().unwrap_or_default();
        let place = format!("{} / {} / {}", j.project, j.environment, j.path);
        let mut text = format!("✅ Saved `{}` in Infisical, {place}.", j.name);
        if j.source.starts_with("note:") {
            text.push_str(if cleared.note { " I took it out of the note." } else { " The note still holds it: clearing failed." });
        }
        if cleared.chat_items > 0 {
            text.push_str(&format!(" Cleared from {} chat message(s).", cleared.chat_items));
        }
        if j.auto {
            text.push_str(" You sent it in a message, so I kept it. The chat shows only a mask. It went through my local model, not the cloud.");
        }
        for r in &cleared.remains {
            text.push_str(&format!(" {r}."));
        }
        let timeline = self.timeline.clone();
        let _ = lucina_item(&timeline, &text, "secret_receipt", None);
        let result = json!({"key": j.name, "environment": j.environment, "path": j.path,
                            "project": {"name": j.project, "slug": j.project, "alias": j.project}});
        st.secret_requests.saved(j.request.as_deref(), &result);
    }

    fn tell_failed(&self, st: &crate::AppState, j: &Job) {
        // A value in a message that the local model could not take: the old card, while it is held.
        if let Some(id) = j.source.strip_prefix("held:") {
            if matches!(j.code.as_deref(), Some("not_a_secret")) {
                return;
            }
            match st.held_secrets.get(id) {
                Some(v) => {
                    let hint = format!("…, {}", v.expose_secret().chars().count());
                    crate::owner_secrets::offer_held(&self.timeline, &[(id.to_owned(), hint)], &j.lang);
                }
                None => {
                    let text = "🔐 I hid a secret from your message but couldn't save it in time, so it's gone: I don't have it. Send it again, or use \"Add secret\" in the app.";
                    let _ = lucina_item(&self.timeline, text, "secret_receipt", None);
                }
            }
            return;
        }
        let why = match j.code.as_deref().unwrap_or("") {
            "local_model_unavailable" => "my local model didn't answer, and this never goes to a cloud model".to_owned(),
            "no_secret" => "my local model found no secret there".to_owned(),
            "not_in_source" => "what my local model picked isn't in the text, so I didn't trust it".to_owned(),
            "exists" => "a secret with that name already exists with another value, and I never overwrite one".to_owned(),
            "not_allowed" => "I may only store new secrets in the places you allowed".to_owned(),
            "readback_mismatch" => "Infisical read back something different. The secret may be there: check it".to_owned(),
            "source_not_found" => "the note or message isn't there".to_owned(),
            other => format!("the step failed ({other})"),
        };
        let text = format!("⚠️ I didn't save `{}`: {why}. Nothing was cleared.", j.name);
        let _ = lucina_item(&self.timeline, &text, "secret_receipt", None);
    }

    /// Where a secret may be: references and why, never the text. `limit`: per kind.
    pub async fn scan(&self, limit: usize) -> Result<Value, ApiError> {
        let mut notes = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let (mut since, mut more) = (String::from("0"), true);
        let mut pages = 0;
        while more && pages < 200 && notes.len() < limit {
            pages += 1;
            let r = self.secretary.call(Method::GET, &format!("/v1/notes/changes?since={since}&limit=200"), None).await;
            if !r.ok {
                return Err(ApiError::new(502, "notes_unavailable", "the notes could not be read"));
            }
            for n in r.data["notes"].as_array().into_iter().flatten() {
                if let Some(why) = sweep::note_suspect(n) {
                    let id = n["id"].as_str().unwrap_or_default().to_owned();
                    if seen.insert(id) && notes.len() < limit {
                        // By id: a slug is made from the first line, so it can hold the secret itself.
                        notes.push(json!({"source": format!("note:{}", n["id"].as_str().unwrap_or_default()),
                                          "title": sweep::safe_title(n), "why": why.why, "whole": why.whole}));
                    }
                }
            }
            since = r.data["cursor"].as_str().unwrap_or("0").to_owned();
            more = r.data["hasMore"].as_bool().unwrap_or(false);
        }
        let mut chats = Vec::new();
        let items = self.timeline.read_all().map_err(|_| ApiError::new(500, "timeline", "the chats could not be read"))?;
        for item in items.iter().rev() {
            if chats.len() >= limit {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&item.raw) else { continue };
            let text = v["text"].as_str().unwrap_or_default();
            if v["role"] == "user" && (!lucina_secrets::labelled::detect(text).is_empty() || !lucina_secrets::redact::detect(text).is_empty()) {
                chats.push(json!({"source": format!("chat:{}", item.id), "at": v["at"], "why": "a value said in words or a token shape"}));
            }
        }
        Ok(json!({"notes": notes, "chats": chats, "note": "References only. Move one with secret_keep."}))
    }
}

/// Part of the value stands in the note's name (slugs are cut at 80 characters and lowercased).
fn slug_holds_value(slug: &str, raw: &str) -> bool {
    let value = lucina_secrets::labelled::as_slug(raw);
    slug.split('-').any(|seg| seg.len() >= 8 && value.contains(seg))
}

/// The value as it stands inside a JSON string.
fn raw_json(raw: &str) -> String {
    let s = serde_json::to_string(raw).unwrap_or_default();
    s.trim_matches('"').to_owned()
}

/// A note's text: its content, or its items for a list note.
fn note_text(n: &Value) -> String {
    let mut out = n["content"].as_str().unwrap_or_default().to_owned();
    if out.is_empty() {
        for it in n["items"].as_array().into_iter().flatten() {
            out.push_str(it["text"].as_str().unwrap_or_default());
            out.push('\n');
        }
    }
    out
}

fn check_verbatim(v: &SecretValue, text: &str) -> Result<(), Stop> {
    if v.is_empty() || !text.contains(v.expose_secret()) {
        return Err(stop("not_in_source"));
    }
    Ok(())
}

fn local_stop(e: local::LocalError) -> Stop {
    match e {
        local::LocalError::Unavailable => wait("local_model_unavailable"),
        local::LocalError::Unparsable => stop("local_model_unclear"),
        local::LocalError::Nothing => stop("no_secret"),
        local::LocalError::NotLocal => stop("local_model_not_local"),
    }
}

/// `<dir>/<day>/<note id>.json` and its trash copy: each path is rewritten by itself.
fn clear_backups(dir: &Path, note_id: &str, needles: &[String], placeholder: &str) -> std::io::Result<usize> {
    if !note_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) || note_id.is_empty() {
        return Ok(0);
    }
    if !dir.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let mut n = 0;
    for day in std::fs::read_dir(dir)? {
        let day = day?.path();
        for file in [day.join(format!("{note_id}.json")), day.join("trash").join(format!("{note_id}.json"))] {
            if file.is_file() && lucina_store::scrub::scrub_file(&file, needles, placeholder)? {
                n += 1;
            }
        }
    }
    Ok(n)
}

/// The value in the owner's message was masked before anything saw it: keep it, or offer the card.
pub fn after_turn(st: &crate::AppState, timeline: &TimelineStore, held: &[(String, String)], masked: &str, lang: &str, session: &str) {
    if held.is_empty() {
        return;
    }
    let keeper = &st.secret_keep;
    if !keeper.cfg.enabled || !keeper.cfg.auto || crate::selftest::is_session(session) {
        crate::owner_secrets::offer_held(timeline, held, lang);
        return;
    }
    let context: String = masked.chars().take(300).collect();
    for (id, _) in held {
        keeper.add(Job {
            id: random_id(),
            source: format!("held:{id}"),
            name: "KEPT_SECRET".to_owned(),
            project: keeper.cfg.default_project.clone(),
            environment: keeper.cfg.default_environment.clone(),
            path: "/".to_owned(),
            what: context.clone(),
            whole: false,
            auto: true,
            request: None,
            state: State::Queued,
            code: None,
            tries: 0,
            created_at: now(),
            retry_at: None,
            cleared: None,
            lang: lang.to_owned(),
        });
    }
}

/// The worker: one job at a time, and the sweep.
pub fn spawn(st: &crate::AppState) -> Option<tokio::task::JoinHandle<()>> {
    if !st.secret_keep.cfg.enabled {
        return None;
    }
    let st = st.clone();
    Some(tokio::spawn(async move {
        // After a restart a held value is gone: say so rather than leave the job waiting.
        st.secret_keep.expire_held(&st);
        let mut last_sweep = tokio::time::Instant::now() - Duration::from_secs(3300);
        loop {
            while st.secret_keep.run_next(&st).await {}
            if let Some(every) = st.secret_keep.cfg.sweep {
                if last_sweep.elapsed() >= every {
                    last_sweep = tokio::time::Instant::now();
                    sweep::run(&st).await;
                }
            }
            let _ = tokio::time::timeout(Duration::from_secs(30), st.secret_keep.wake.notified()).await;
        }
    }))
}

impl Keeper {
    fn expire_held(&self, st: &crate::AppState) {
        let gone: Vec<String> = self.with(|jobs| {
            jobs.iter()
                .filter(|j| j.state == State::Queued && j.source.starts_with("held:") && st.held_secrets.get(&j.source[5..]).is_none())
                .map(|j| j.id.clone())
                .collect()
        });
        for id in gone {
            let done = self.finish(&id, |j| {
                j.state = State::Failed;
                j.code = Some("held_expired".into());
            });
            if let Some(j) = done {
                self.tell_failed(st, &j);
            }
        }
    }

    /// Tests: every job, due now.
    pub fn make_all_due_for_test(&self) {
        self.with(|jobs| jobs.iter_mut().for_each(|j| j.retry_at = None));
    }

    /// Tests: a job has waited this long already.
    pub fn age_all_for_test(&self, secs: i64) {
        self.with(|jobs| jobs.iter_mut().for_each(|j| j.created_at -= secs));
    }
}

#[async_trait::async_trait]
impl lucina_tools::tools::secret_keep::SecretKeeper for Keeper {
    async fn keep(&self, session_id: &str, mut body: Value) -> Result<Value, String> {
        if crate::selftest::is_session(session_id) {
            return Err("self-test sessions don't move secrets".into());
        }
        // A secret with no place goes to the default one, whose names start with KEPT_ or KEEP_.
        if body["project"].as_str().is_none_or(str::is_empty) {
            let name = body["name"].as_str().unwrap_or("SECRET").to_owned();
            if !name.starts_with("KEPT_") && !name.starts_with("KEEP_") {
                body["name"] = json!(format!("KEPT_{name}"));
            }
        }
        let job = self.enqueue(&body).map_err(|e| e.message)?;
        Ok(job.outcome())
    }
    async fn status(&self, job: &str) -> Result<Value, String> {
        self.job(job).map(|j| j.outcome()).ok_or_else(|| "no such job".to_owned())
    }
    async fn scan(&self, session_id: &str) -> Result<Value, String> {
        if crate::selftest::is_session(session_id) {
            return Err("self-test sessions don't look for secrets".into());
        }
        Keeper::scan(self, 20).await.map_err(|e| e.message)
    }
}
