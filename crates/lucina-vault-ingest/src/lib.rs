//! Lucina's Obsidian ingest: the vault keeps itself current at about zero cost.
//!
//! Items come from three sources ([`sources`]): every ops record, every
//! first-parent commit or merge on the followed branches of the Lucina repos,
//! and Claude session wrap-up reports. They are redacted and queued durably
//! ([`queue`], dedup key per item). A worker turns each one into Zettelkasten
//! notes in the vault folder (`Permanent/` claims, an `Events/` digest, the
//! project's `MOC/` page and `00 Lucina hub.md`, [`notes`]), following the
//! owner's guidelines (`guidelines/zettelkasten.md`, `templates/`).
//!
//! The writing model is the local Qwen through the gateway, used only while
//! nobody else needs it ([`gate`]): a busy, switching, leased or unloaded GPU
//! means "wait and keep queueing", never a preemption and never another
//! model. A manual run ([`RunOpts::cold`]) or a queue older than
//! `cold_after` may load an idle model.
//!
//! Replays and restarts never duplicate notes: keys are deduplicated at
//! enqueue, the model's answer is stored before any write, and every line the
//! worker adds carries the item's `ingest:` marker.

pub mod gate;
pub mod model;
pub mod notes;
pub mod plan;
pub mod queue;
pub mod redact;
pub mod sources;
pub mod templates;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use lucina_ops::Ops;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use gate::{Gate, Verdict};
use model::{ModelError, Qwen};
use plan::Plan;
use queue::{Depth, Item, Queue, Slot, State};
use sources::GitRepo;
use templates::Templates;

pub const DEFAULT_QWEN_URL: &str = "http://127.0.0.1:11436";
/// The dense Qwen 3.8 27B (the gateway's `qwen` provider).
pub const DEFAULT_MODEL: &str = "qwen3.8:27b-q4-72k";
pub const DEFAULT_GIT: &str =
    "lucina-rs=/home/nez/Projects/Lucina/SecretaryIOS@origin/main#lucina;\
lucinaos=/home/nez/Projects/Lucina/lucinaos@usb-dev-loop#lucinaos";
const STATE_FILE: &str = "state.json";

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("vault-ingest config: {0}")]
    Config(String),
    #[error(transparent)]
    Vault(#[from] lucina_ops::VaultError),
    #[error(transparent)]
    Ops(#[from] lucina_ops::OpsError),
    #[error("vault-ingest: {0}")]
    Io(String),
}

/// At most `max` characters, on a character boundary, with `…` when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestConfig {
    /// `queue.jsonl`, `state.json`, the locks.
    pub state_dir: PathBuf,
    /// The gateway (loopback only).
    pub qwen_url: String,
    pub key_file: Option<PathBuf>,
    pub model: String,
    /// `qcode-run`'s lock (`~/.cache/qcode/lock`).
    pub qcode_lock: Option<PathBuf>,
    /// The gateway's `last-used` stamp (`~/.local/state/llama-server-qwen/last-used`).
    pub last_used: Option<PathBuf>,
    /// Qwen must have been idle this long before the worker asks it.
    pub quiet: std::time::Duration,
    pub git: Vec<GitRepo>,
    /// Items waiting this long may load an unloaded model; `None`: never on
    /// their own (only a manual run).
    pub cold_after: Option<Duration>,
    /// On first start, ops records this far back are queued.
    pub backfill: Duration,
    /// On first sight of a repo, this many first-parent commits are queued.
    pub backfill_commits: usize,
    /// Items one run may send to the model.
    pub batch: usize,
    pub time_zone: Tz,
    pub templates_dir: Option<PathBuf>,
}

/// Whether a URL is plain HTTP on a loopback address.
pub fn is_loopback_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = match host.strip_prefix('[') {
        Some(h) => h.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

impl IngestConfig {
    pub fn new(state_dir: impl Into<PathBuf>, time_zone: Tz) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            state_dir: state_dir.into(),
            qwen_url: DEFAULT_QWEN_URL.into(),
            key_file: home
                .as_ref()
                .map(|h| h.join(".config/llama-server-api.key")),
            model: DEFAULT_MODEL.into(),
            qcode_lock: home.as_ref().map(|h| h.join(".cache/qcode/lock")),
            last_used: home
                .as_ref()
                .map(|h| h.join(".local/state/llama-server-qwen/last-used")),
            quiet: std::time::Duration::from_secs(120),
            git: Vec::new(),
            cold_after: Some(Duration::hours(6)),
            backfill: Duration::days(2),
            backfill_commits: 10,
            batch: 6,
            time_zone,
            templates_dir: None,
        }
    }

    /// `LUCINA_VAULT_INGEST_*` over [`IngestConfig::new`]; `default_state`
    /// is `<ops state>/ingest`.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        default_state: &Path,
        time_zone: Tz,
    ) -> Result<Self, IngestError> {
        let v = |k: &str| {
            get(&format!("LUCINA_VAULT_INGEST_{k}"))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        };
        let num = |k: &str, max: i64| -> Result<Option<i64>, IngestError> {
            v(k).map(|s| {
                s.parse::<i64>()
                    .ok()
                    .filter(|n| (0..=max).contains(n))
                    .ok_or_else(|| {
                        IngestError::Config(format!("LUCINA_VAULT_INGEST_{k} must be 0..={max}"))
                    })
            })
            .transpose()
        };
        let mut c = Self::new(
            v("STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_state.to_owned()),
            time_zone,
        );
        if let Some(u) = v("QWEN_URL") {
            c.qwen_url = u.trim_end_matches('/').to_owned();
        }
        if let Some(k) = v("QWEN_KEY_FILE") {
            c.key_file = Some(PathBuf::from(k));
        }
        if let Some(m) = v("MODEL") {
            c.model = m;
        }
        if let Some(l) = v("QCODE_LOCK") {
            c.qcode_lock = Some(PathBuf::from(l));
        }
        if let Some(l) = v("LAST_USED_FILE") {
            c.last_used = Some(PathBuf::from(l));
        }
        if let Some(s) = num("QUIET_SECS", 3600)? {
            c.quiet = std::time::Duration::from_secs(s as u64);
        }
        c.git = GitRepo::parse_list(&v("GIT").unwrap_or_else(|| DEFAULT_GIT.into()))
            .map_err(IngestError::Config)?;
        if let Some(h) = num("COLD_AFTER_HOURS", 24 * 30)? {
            c.cold_after = (h > 0).then(|| Duration::hours(h));
        }
        if let Some(d) = num("BACKFILL_DAYS", 365)? {
            c.backfill = Duration::days(d);
        }
        if let Some(n) = num("BACKFILL_COMMITS", 500)? {
            c.backfill_commits = n as usize;
        }
        if let Some(n) = num("BATCH", 100)? {
            c.batch = (n as usize).max(1);
        }
        c.templates_dir = v("TEMPLATES").map(PathBuf::from);
        c.check()?;
        Ok(c)
    }

    /// The one rule that keeps this free: the local gateway or nothing.
    pub fn check(&self) -> Result<(), IngestError> {
        if !is_loopback_url(&self.qwen_url) {
            return Err(IngestError::Config(format!(
                "the Qwen URL must be the local gateway (http on loopback), not {}",
                self.qwen_url
            )));
        }
        Ok(())
    }
}

/// What a run may do.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOpts {
    /// Ignore the backoff (`next_try`); the gate still decides.
    pub force: bool,
    /// May load an unloaded model (a manual "ingest now").
    pub cold: bool,
    /// Items for the model this run (default: the configured batch).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub at: Option<DateTime<Utc>>,
    /// `ran` | `waiting` (gate or backoff) | `idle` (nothing due) | `already_running`
    pub outcome: String,
    pub ingested: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Why the run stopped short (the gate's reason, a busy gateway).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_try: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    pub ops: usize,
    pub git: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Persisted {
    /// Ops records made from here on are ingested.
    ops_since: Option<DateTime<Utc>>,
    /// Last commit seen per repo name.
    git: BTreeMap<String, String>,
    next_try: Option<DateTime<Utc>>,
    last_run: Option<RunReport>,
    last_scan: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub name: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub refname: String,
    pub project: String,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub queue: Depth,
    pub oldest_pending: Option<DateTime<Utc>>,
    /// The worker's next attempt (backoff after a "wait").
    pub next_try: Option<DateTime<Utc>>,
    pub last_run: Option<RunReport>,
    pub last_scan: Option<DateTime<Utc>>,
    pub running: bool,
    pub model: String,
    pub cold_after_hours: Option<i64>,
    pub ops_since: Option<DateTime<Utc>>,
    pub repos: Vec<RepoStatus>,
    /// Items that gave up (`retry` re-arms them), at most 20.
    pub failed: Vec<FailedItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedItem {
    pub key: String,
    pub title: String,
    pub error: String,
}

pub struct Ingest {
    cfg: IngestConfig,
    ops: Arc<Ops>,
    queue: Queue,
    templates: Templates,
    gate: Gate,
    qwen: Qwen,
    running: AtomicBool,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ingest")
            .field("state_dir", &self.cfg.state_dir)
            .field("vault", &self.ops.vault().root())
            .finish()
    }
}

struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Ingest {
    pub fn open(cfg: IngestConfig, ops: Arc<Ops>) -> Result<Self, IngestError> {
        cfg.check()?;
        let queue = Queue::open(&cfg.state_dir)?;
        let templates = Templates::load(cfg.templates_dir.as_deref());
        let gate = Gate::new(&cfg.qwen_url, cfg.key_file.clone(), cfg.qcode_lock.clone())
            .with_quiet(cfg.last_used.clone(), cfg.quiet);
        let qwen = Qwen::new(&cfg.qwen_url, cfg.key_file.clone(), &cfg.model);
        Ok(Self {
            cfg,
            ops,
            queue,
            templates,
            gate,
            qwen,
            running: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &IngestConfig {
        &self.cfg
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    fn state_path(&self) -> PathBuf {
        self.cfg.state_dir.join(STATE_FILE)
    }

    fn load(&self) -> Persisted {
        match std::fs::read(self.state_path()) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "vault-ingest: state.json unreadable; starting over");
                Persisted::default()
            }),
            Err(_) => Persisted::default(),
        }
    }

    fn save(&self, p: &Persisted) -> Result<(), IngestError> {
        let tmp = self.cfg.state_dir.join(format!(".{STATE_FILE}.tmp"));
        let body = serde_json::to_vec_pretty(p).map_err(|e| IngestError::Io(e.to_string()))?;
        std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, self.state_path()))
            .map_err(|e| IngestError::Io(format!("state.json: {e}")))
    }

    /// Queue what is new in the sources (cheap: no model).
    pub async fn scan(&self, now: DateTime<Utc>) -> Result<ScanReport, IngestError> {
        let mut st = self.load();
        let mut report = ScanReport::default();
        let since = *st.ops_since.get_or_insert(now - self.cfg.backfill);
        let records = self.ops.records()?;
        report.ops = self
            .queue
            .enqueue(sources::ops_items(&records, since), now)?;
        for repo in &self.cfg.git {
            match self
                .scan_repo(repo, st.git.get(&repo.name).map(String::as_str), now)
                .await
            {
                Ok((n, head)) => {
                    report.git.insert(repo.name.clone(), n);
                    st.git.insert(repo.name.clone(), head);
                }
                Err(e) => report.errors.push(format!("{}: {e}", repo.name)),
            }
        }
        st.last_scan = Some(now);
        self.save(&st)?;
        Ok(report)
    }

    /// Queue a repo's new commits; returns how many and the new cursor.
    async fn scan_repo(
        &self,
        repo: &GitRepo,
        cursor: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(usize, String), String> {
        let head = repo.head().await?;
        let shas = repo
            .new_commits(cursor, &head, self.cfg.backfill_commits)
            .await?;
        let mut items = Vec::new();
        let mut last = cursor.map(str::to_owned);
        for sha in &shas {
            items.push(repo.item(sha, &self.cfg.time_zone).await?);
            last = Some(sha.clone());
        }
        let n = self.queue.enqueue(items, now).map_err(|e| e.to_string())?;
        // Capped scans resume from the last commit taken, not from the head.
        Ok((n, last.unwrap_or(head)))
    }

    /// Queue one Claude session report (`{id?, title, project, summary, at?}`).
    pub fn enqueue_session(
        &self,
        report: &Value,
        now: DateTime<Utc>,
    ) -> Result<(String, bool), IngestError> {
        let local = now.with_timezone(&self.cfg.time_zone).fixed_offset();
        let item = sources::session_item(report, local).map_err(IngestError::Config)?;
        let key = item.key.clone();
        let added = self.queue.enqueue(vec![item], now)? > 0;
        Ok((key, added))
    }

    /// Re-arm items that gave up (all of them with `keys` empty).
    pub fn retry(
        &self,
        keys: &[String],
        any: bool,
        now: DateTime<Utc>,
    ) -> Result<usize, IngestError> {
        let keys = if keys.is_empty() {
            self.queue.failed_keys()?
        } else {
            keys.to_vec()
        };
        let n = self.queue.retry(&keys, any, now)?;
        if n > 0 {
            let mut st = self.load();
            st.next_try = None;
            self.save(&st)?;
        }
        Ok(n)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> Result<Status, IngestError> {
        let st = self.load();
        let (queue, oldest_pending) = self.queue.depth()?;
        let slots = self.queue.slots()?;
        let failed = slots
            .iter()
            .filter_map(|(k, s)| match &s.state {
                State::Failed { last_error, .. } => Some(FailedItem {
                    key: k.clone(),
                    title: s.item.as_ref().map(|i| i.title.clone()).unwrap_or_default(),
                    error: last_error.clone(),
                }),
                _ => None,
            })
            .take(20)
            .collect();
        Ok(Status {
            queue,
            oldest_pending,
            next_try: st.next_try,
            last_run: st.last_run,
            last_scan: st.last_scan,
            running: self.running(),
            model: self.cfg.model.clone(),
            cold_after_hours: self.cfg.cold_after.map(|d| d.num_hours()),
            ops_since: st.ops_since,
            repos: self
                .cfg
                .git
                .iter()
                .map(|r| RepoStatus {
                    name: r.name.clone(),
                    path: r.path.clone(),
                    refname: r.refname.clone(),
                    project: r.project.clone(),
                    cursor: st.git.get(&r.name).cloned(),
                })
                .collect(),
            failed,
        })
    }

    /// The server's tick: scan, then run when the backoff allows.
    pub async fn tick(&self) -> Result<RunReport, IngestError> {
        let now = Utc::now();
        if let Err(e) = self.scan(now).await {
            tracing::warn!(error = %e, "vault-ingest: scan failed");
        }
        self.run(RunOpts::default(), Utc::now()).await
    }

    /// Turn due items into notes while the model is free.
    pub async fn run(&self, opts: RunOpts, now: DateTime<Utc>) -> Result<RunReport, IngestError> {
        let mut report = RunReport {
            at: Some(now),
            ..Default::default()
        };
        if self.running.swap(true, Ordering::SeqCst) {
            report.outcome = "already_running".into();
            return Ok(report);
        }
        let _running = Running(&self.running);
        let Some(_lock) = self.queue.try_run_lock()? else {
            report.outcome = "already_running".into();
            return Ok(report);
        };
        if let Err(e) = self.queue.compact_if_big() {
            tracing::warn!(error = %e, "vault-ingest: compaction failed");
        }
        let mut st = self.load();
        if !opts.force {
            if let Some(t) = st.next_try.filter(|t| *t > now) {
                report.outcome = "waiting".into();
                report.next_try = Some(t);
                report.waiting_for = st.last_run.as_ref().and_then(|r| r.waiting_for.clone());
                return Ok(report);
            }
        }
        let mut due: Vec<(String, Slot)> = self
            .queue
            .ordered()?
            .into_iter()
            .filter(|(_, s)| s.due(now) && s.item.is_some())
            .collect();
        // Items with a stored plan need no model: they are written first.
        due.sort_by_key(|(_, s)| s.plan.is_none());
        if due.is_empty() {
            report.outcome = "idle".into();
            st.next_try = None;
            st.last_run = Some(report.clone());
            self.save(&st)?;
            return Ok(report);
        }
        let oldest = due.iter().map(|(_, s)| s.enqueued_at).min().unwrap_or(now);
        let mut allow_cold = opts.cold || self.cfg.cold_after.is_some_and(|d| oldest <= now - d);
        let budget = opts.limit.unwrap_or(self.cfg.batch).max(1);
        let mut asked = 0usize;
        let today = now.with_timezone(&self.cfg.time_zone).date_naive();
        let writer = notes::Writer {
            vault: self.ops.vault(),
            templates: &self.templates,
            today,
        };
        report.outcome = "ran".into();
        for (key, slot) in due {
            let Some(item) = slot.item.as_ref() else {
                continue;
            };
            let plan = match slot.plan {
                Some(p) => p,
                None => {
                    if asked >= budget {
                        break;
                    }
                    let verdict = self.gate.check(allow_cold).await;
                    let cold = match verdict {
                        Verdict::Go { cold } => cold,
                        Verdict::Wait { reason, retry_in } => {
                            let t =
                                now + Duration::from_std(retry_in).unwrap_or(Duration::minutes(10));
                            report.waiting_for = Some(reason);
                            report.next_try = Some(t);
                            break;
                        }
                    };
                    asked += 1;
                    match self.plan_item(&writer, item, cold).await {
                        Ok(p) => {
                            // Loaded now: later items need no cold start.
                            allow_cold = false;
                            self.queue.planned(&key, &p, Utc::now())?;
                            p
                        }
                        Err(ModelError::Busy(why)) => {
                            report.waiting_for = Some(why);
                            report.next_try = Some(now + Duration::minutes(5));
                            break;
                        }
                        Err(ModelError::Failed(why)) => {
                            let n = self.queue.failed(&key, &why, Utc::now())?;
                            tracing::warn!(key = %key, attempts = n, error = %why, "vault-ingest: item failed");
                            report.failed += 1;
                            report.errors.push(format!("{key}: {why}"));
                            continue;
                        }
                    }
                }
            };
            if plan.skip {
                self.queue.skipped(&key, &plan.skip_reason, Utc::now())?;
                report.skipped += 1;
                continue;
            }
            match writer.write(item, &plan) {
                Ok(pages) => {
                    self.queue.done(&key, pages.clone(), Utc::now())?;
                    report.ingested += 1;
                    for p in pages {
                        if !report.notes.contains(&p) {
                            report.notes.push(p);
                        }
                    }
                }
                Err(e) => {
                    let why = e.to_string();
                    self.queue.failed(&key, &why, Utc::now())?;
                    report.failed += 1;
                    report.errors.push(format!("{key}: {why}"));
                }
            }
        }
        if report.ingested + report.skipped + report.failed == 0 && report.waiting_for.is_some() {
            report.outcome = "waiting".into();
        }
        st.next_try = report.next_try;
        st.last_run = Some(report.clone());
        self.save(&st)?;
        tracing::info!(
            outcome = %report.outcome,
            ingested = report.ingested,
            skipped = report.skipped,
            failed = report.failed,
            waiting_for = report.waiting_for.as_deref().unwrap_or(""),
            "vault-ingest: run"
        );
        Ok(report)
    }

    async fn plan_item(
        &self,
        writer: &notes::Writer<'_>,
        item: &Item,
        cold: bool,
    ) -> Result<Plan, ModelError> {
        let existing = writer
            .claim_index()
            .map_err(|e| ModelError::Failed(e.to_string()))?;
        let related = plan::related_titles(&existing, item, plan::PROMPT_TITLES);
        let user = plan::prompt(item, &related);
        let answer = self
            .qwen
            .complete(&self.templates.guidelines, &user, cold)
            .await?;
        Plan::from_answer(&answer, item, &existing).map_err(ModelError::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_local_gateway() {
        for ok in [
            "http://127.0.0.1:11436",
            "http://localhost:11436/",
            "http://[::1]:11436",
            "http://127.0.0.1",
        ] {
            assert!(is_loopback_url(ok), "{ok}");
        }
        for bad in [
            "https://api.deepseek.com",
            "http://api.deepseek.com",
            "http://127.0.0.1.evil.com:80",
            "http://10.0.0.5:11436",
            "https://127.0.0.1:11436",
            "127.0.0.1:11436",
        ] {
            assert!(!is_loopback_url(bad), "{bad}");
        }
        let get = |k: &str| {
            (k == "LUCINA_VAULT_INGEST_QWEN_URL").then(|| "https://api.deepseek.com/v1".to_owned())
        };
        assert!(matches!(
            IngestConfig::from_lookup(get, Path::new("/s"), chrono_tz::Europe::Rome),
            Err(IngestError::Config(_))
        ));
    }

    #[test]
    fn config_from_env() {
        let env: std::collections::HashMap<&str, &str> = [
            ("LUCINA_VAULT_INGEST_GIT", "a=/r@main#lucina"),
            ("LUCINA_VAULT_INGEST_COLD_AFTER_HOURS", "0"),
            ("LUCINA_VAULT_INGEST_BATCH", "3"),
        ]
        .into();
        let c = IngestConfig::from_lookup(
            |k| env.get(k).map(|v| v.to_string()),
            Path::new("/s/ingest"),
            chrono_tz::Europe::Rome,
        )
        .unwrap();
        assert_eq!(c.state_dir, PathBuf::from("/s/ingest"));
        assert_eq!(c.git.len(), 1);
        assert_eq!(c.cold_after, None);
        assert_eq!(c.batch, 3);
        assert_eq!(c.qwen_url, DEFAULT_QWEN_URL);
        let d =
            IngestConfig::from_lookup(|_| None, Path::new("/s"), chrono_tz::Europe::Rome).unwrap();
        assert_eq!(d.git.len(), 2, "default repos");
        assert_eq!(d.cold_after, Some(Duration::hours(6)));
        let bad: std::collections::HashMap<&str, &str> =
            [("LUCINA_VAULT_INGEST_BATCH", "-1")].into();
        assert!(IngestConfig::from_lookup(
            |k| bad.get(k).map(|v| v.to_string()),
            Path::new("/s"),
            chrono_tz::Europe::Rome
        )
        .is_err());
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
