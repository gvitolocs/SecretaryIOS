//! The delegation engine: hard limits → agent choice → budget → Lucina's
//! self-review → sandbox → the job in the background → card, note,
//! notification, audit.
//!
//! A backend that fails with "out of credits", "not logged in" or a rate limit is remembered as down
//! ([`crate::state::Down`]); while it is, the job goes to Mistral (Lucina's own model) for non-code
//! work, and the card says so. A running job can be cancelled ([`Delegator::cancel`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_llm::{ChatModel, ChatRequest, Message};
use lucina_store::rich::RichMessage;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::agents::{Agent, AgentJob};
use crate::audit::AuditLog;
use crate::card;
use crate::config::{DelegateConfig, Isolation};
use crate::intent::{Switch, SwitchIntent};
use crate::judge;
use crate::policy::{self, Refusal};
use crate::sandbox::{self, Workspace};
use crate::state::{outage, Down, PauseNotice, StateStore};
use crate::types::{
    answer_lang, clip, clip_sentences, guess_lang, now_iso, AgentId, Decision, Fallback, JobRecord,
    JobStatus, Judgement, Kind, Request,
};

const KEEP_JOBS: usize = 100;
const RESULT_CHARS: usize = 8000;
/// The error of a job the owner stopped.
pub const CANCELLED: &str = "cancelled by Giuseppe";

/// The clock the engine reads (tests move it forward).
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// A rendered card for the timeline.
#[derive(Debug, Clone)]
pub struct Card {
    pub job: JobRecord,
    pub plain: String,
    pub rich: Option<RichMessage>,
}

/// Where delegation reports (the server: timeline, notes, notifications).
#[async_trait::async_trait]
pub trait Sink: Send + Sync {
    /// Save a long answer as a note; the slug.
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<String, String>;
    /// An app notification to the owner.
    async fn notify(&self, title: &str, body: &str);
    /// A session whose jobs never alert the owner (a self-test): its cards stay in its own chat.
    fn quiet(&self, _session: &str) -> bool {
        false
    }
    /// A delegation card on the timeline (the job's channel).
    fn card(&self, card: &Card);
    /// A plain Lucina message on the timeline.
    fn message(&self, channel: &str, text: &str);
}

/// A sink that drops everything.
pub struct NullSink;

#[async_trait::async_trait]
impl Sink for NullSink {
    async fn save_note(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
        Err("no notes".into())
    }
    async fn notify(&self, _: &str, _: &str) {}
    fn card(&self, _: &Card) {}
    fn message(&self, _: &str, _: &str) {}
}

/// A started job; `wait` returns it once finished (or `None` on timeout).
pub struct Started {
    pub job: JobRecord,
    rx: watch::Receiver<Option<JobRecord>>,
}

impl Started {
    pub async fn wait(&mut self, d: Duration) -> Option<JobRecord> {
        let r = tokio::time::timeout(d, self.rx.wait_for(|j| j.is_some())).await;
        match r {
            Ok(Ok(v)) => (*v).clone(),
            _ => None,
        }
    }
}

pub enum StartOutcome {
    Started(Started),
    /// Lucina's self-review decided not to (the card says why).
    Skipped(JobRecord),
    Refused(Refusal),
}

/// One job per agent at a time: released when dropped.
struct Claim {
    running: Arc<Mutex<HashSet<AgentId>>>,
    agent: AgentId,
}

fn claim(running: &Arc<Mutex<HashSet<AgentId>>>, agent: AgentId) -> Option<Claim> {
    running
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(agent)
        .then(|| Claim {
            running: running.clone(),
            agent,
        })
}

/// A job in the background. `cancel` aborts it and waits for the task to be gone: its claim is
/// dropped and its processes (`kill_on_drop`, the sandbox's process-group guard) are killed.
struct RunningTask {
    handle: tokio::task::JoinHandle<()>,
    /// The agent doing the work (Mistral when it stands in).
    runner: AgentId,
    started: Instant,
    sandbox: std::path::PathBuf,
    tx: Arc<watch::Sender<Option<JobRecord>>>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.agent);
    }
}

pub struct Delegator {
    cfg: DelegateConfig,
    agents: Vec<Arc<dyn Agent>>,
    state: StateStore,
    judge_model: Option<Arc<dyn ChatModel>>,
    sink: Arc<dyn Sink>,
    running: Arc<Mutex<HashSet<AgentId>>>,
    jobs: Mutex<Vec<JobRecord>>,
    tasks: Mutex<HashMap<String, RunningTask>>,
    audit: AuditLog,
    tz: Tz,
    clock: Clock,
}

fn redacted(s: &str) -> String {
    lucina_secrets::redactor().redact(s).into_owned()
}

impl Delegator {
    pub fn new(
        cfg: DelegateConfig,
        agents: Vec<Arc<dyn Agent>>,
        judge_model: Option<Arc<dyn ChatModel>>,
        sink: Arc<dyn Sink>,
        tz: Tz,
    ) -> Self {
        let jobs = cfg
            .jobs_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Vec<JobRecord>>(&b).ok())
            .map(|mut l| {
                // A job that was running when Lucina stopped did not finish.
                for j in l.iter_mut().filter(|j| j.status == JobStatus::Running) {
                    j.status = JobStatus::Error;
                    j.error = Some("interrupted (Lucina restarted)".into());
                }
                l
            })
            .unwrap_or_default();
        Self {
            state: StateStore::new(cfg.state_path.clone(), tz),
            audit: AuditLog::new(cfg.audit_path.clone()),
            cfg,
            agents,
            judge_model,
            sink,
            running: Arc::new(Mutex::new(HashSet::new())),
            jobs: Mutex::new(jobs),
            tasks: Mutex::new(HashMap::new()),
            tz,
            clock: Arc::new(Utc::now),
        }
    }

    /// Another clock (tests and replays: an outage that has run out).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// `HH:MM` in the owner's time zone.
    fn hhmm(&self, t: DateTime<Utc>) -> String {
        t.with_timezone(&self.tz).format("%H:%M").to_string()
    }

    pub fn config(&self) -> &DelegateConfig {
        &self.cfg
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn inline_wait(&self) -> Duration {
        Duration::from_secs(self.cfg.inline_wait_secs)
    }

    // ------------------------------------------------------------ jobs

    fn store(&self, job: &JobRecord) {
        let mut l = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        match l.iter_mut().find(|j| j.id == job.id) {
            Some(j) => *j = job.clone(),
            None => l.push(job.clone()),
        }
        let skip = l.len().saturating_sub(KEEP_JOBS);
        if skip > 0 {
            l.drain(..skip);
        }
        if let Some(p) = &self.cfg.jobs_path {
            if let Ok(text) = serde_json::to_vec_pretty(&*l) {
                if let Err(e) = lucina_store::write_atomic(p, &text) {
                    tracing::warn!(error = %e, "delegate jobs write failed");
                }
            }
        }
    }

    /// A job by id or unique id prefix (6+ characters).
    pub fn job(&self, id: &str) -> Option<JobRecord> {
        let id = id.trim().to_lowercase();
        if id.len() < 6 {
            return None;
        }
        let l = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let hits: Vec<&JobRecord> = l.iter().filter(|j| j.id.starts_with(&id)).collect();
        (hits.len() == 1).then(|| hits[0].clone())
    }

    pub fn recent(&self, n: usize) -> Vec<JobRecord> {
        let l = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        l.iter().rev().take(n).cloned().collect()
    }

    fn post(&self, job: &JobRecord) {
        let (plain, rich) = card::render(job);
        self.sink.card(&Card {
            job: job.clone(),
            plain,
            rich,
        });
    }

    fn audit(&self, job: &JobRecord, outcome: &str) {
        let (usage, total) = self.state.usage(job.answered_by(), self.now());
        self.audit
            .write(AuditLog::entry(job, outcome, usage, total));
    }

    fn audit_refusal(&self, req: &Request, agent: Option<AgentId>, r: &Refusal) {
        let mut line = json!({
            "ts": now_iso(),
            "agent": agent.map(AgentId::as_str),
            "kind": req.kind.as_str(),
            "origin": req.origin.as_str(),
            "outcome": format!("refused:{}", r.code()),
        });
        if let Some(c) = &req.caller {
            line["caller"] = json!(clip(c, 64));
        }
        self.audit.write(line);
    }

    async fn announce_pause(&self, n: &PauseNotice, lang: &str, channel: &str) {
        let text = card::pause_text(lang, &n.why);
        tracing::warn!(scope = %n.scope, why = %n.why, "delegation paused by a daily cap");
        self.sink.message(channel, &text);
        self.sink.notify("Lucina", &text).await;
    }

    // ------------------------------------------------------------ choice

    /// Whether `agent` can take a `kind` job now: its backend, or why not.
    async fn usable(
        &self,
        agent: &Arc<dyn Agent>,
        kind: Kind,
        now: DateTime<Utc>,
    ) -> Result<String, Refusal> {
        let id = agent.id();
        if !agent.supports(kind) {
            return Err(Refusal::Unsupported(id, kind));
        }
        self.state.check(id, now)?;
        if self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
        {
            return Err(Refusal::Busy(id));
        }
        let backend = agent
            .backend()
            .await
            .map_err(|why| Refusal::Unavailable(id, why))?;
        if let Some(d) = self.state.down(id, now).filter(|d| d.backend == backend) {
            return Err(Refusal::Unavailable(
                id,
                format!(
                    "{} {} since {} ({})",
                    d.backend,
                    d.state,
                    self.hhmm(d.since),
                    d.reason
                ),
            ));
        }
        Ok(backend)
    }

    fn agent(&self, id: AgentId) -> Option<&Arc<dyn Agent>> {
        self.agents.iter().find(|a| a.id() == id)
    }

    /// Mistral, when it may stand in for `from` on a non-code job.
    async fn stand_in(
        &self,
        req: &Request,
        from: AgentId,
        why: &Refusal,
        now: DateTime<Utc>,
    ) -> Option<(Arc<dyn Agent>, Fallback)> {
        if req.kind == Kind::Code
            || from == AgentId::Mistral
            || policy::is_caller(req, AgentId::Mistral)
        {
            return None;
        }
        let m = self.agent(AgentId::Mistral)?;
        self.usable(m, req.kind, now).await.ok()?;
        let down = self.state.down(from, now);
        let reason = match (why, &down) {
            (Refusal::Busy(_), _) => "busy with another job".to_owned(),
            (_, Some(d)) => format!("{} since {}", d.state, self.hhmm(d.since)),
            (Refusal::Unavailable(_, w), None) => format!("unavailable ({})", clip(w, 120)),
            _ => "unavailable".to_owned(),
        };
        let from_backend = match (down, self.agent(from)) {
            (Some(d), _) => d.backend,
            (None, Some(a)) => a.backend().await.unwrap_or_default(),
            (None, None) => String::new(),
        };
        Some((
            m.clone(),
            Fallback {
                from,
                from_backend,
                reason,
            },
        ))
    }

    /// The agent to run, and the fallback when Mistral stands in for the one asked: the named agent,
    /// or every preferred one, was unreachable or busy on a non-code job.
    async fn choose(&self, req: &Request) -> Result<(Arc<dyn Agent>, Option<Fallback>), Refusal> {
        let now = self.now();
        // Auto choice never picks the agent that is calling Lucina (`hard_limits` refuses a named one).
        let order: Vec<AgentId> = match req.to {
            Some(a) => vec![a],
            None => policy::preference(req.kind, &req.task)
                .into_iter()
                .filter(|a| !policy::is_caller(req, *a))
                .collect(),
        };
        self.state.check_global(now)?;
        let stands_in = |e: &Refusal| matches!(e, Refusal::Unavailable(..) | Refusal::Busy(_));
        let mut errs: Vec<(AgentId, Refusal)> = Vec::new();
        for &id in &order {
            let Some(agent) = self.agent(id) else {
                if req.to.is_some() {
                    errs.push((id, Refusal::Unavailable(id, "not configured".into())));
                }
                continue;
            };
            match self.usable(agent, req.kind, now).await {
                Ok(_) => return Ok((agent.clone(), None)),
                Err(Refusal::Stopped) => return Err(Refusal::Stopped),
                Err(e) => {
                    let named = req.to.is_some();
                    errs.push((id, e));
                    if named {
                        break;
                    }
                }
            }
        }
        // Mistral stands in only when nothing else could run because of outages or busy agents.
        let all_out = !errs.is_empty()
            && errs.iter().any(|(_, e)| stands_in(e))
            && errs.iter().all(|(_, e)| {
                stands_in(e) || matches!(e, Refusal::AgentOff(_) | Refusal::Unsupported(..))
            });
        if all_out {
            let (from, why) = errs
                .iter()
                .find(|(_, e)| stands_in(e))
                .map(|(a, e)| (*a, e.clone()))
                .expect("one stands in");
            if let Some((m, fb)) = self.stand_in(req, from, &why, now).await {
                tracing::info!(from = from.as_str(), reason = %fb.reason, "delegation: Mistral stands in");
                return Ok((m, Some(fb)));
            }
        }
        let mut errs: Vec<Refusal> = errs.into_iter().map(|(_, e)| e).collect();
        if req.to.is_some() {
            if let Some(e) = errs.pop() {
                return Err(e);
            }
        }
        match errs.len() {
            0 => Err(Refusal::NoAgent("none configured".into())),
            1 => Err(errs.remove(0)),
            _ if errs.iter().all(|e| e.code() == errs[0].code()) => Err(errs.remove(0)),
            _ => Err(Refusal::NoAgent(
                errs.iter()
                    .map(|e| e.message("en"))
                    .collect::<Vec<_>>()
                    .join(" "),
            )),
        }
    }

    fn sandbox_note(&self, req: &Request) -> String {
        match (req.kind, &req.workspace) {
            (Kind::Code, Some(w)) => format!(
                "a bubblewrap sandbox with a clone of {w} on branch lucina/<job>; never main, never pushed or deployed"
            ),
            (Kind::Code, None) => "a fresh, isolated sandbox directory".into(),
            _ => "read-only: the agent only answers".into(),
        }
    }

    async fn self_review(&self, req: &Request, agent: AgentId, lang: &str) -> Judgement {
        match (&self.judge_model, self.cfg.judge) {
            (Some(m), true) => {
                judge::judge(
                    m.as_ref(),
                    req,
                    agent,
                    lang,
                    &self.sandbox_note(req),
                    Duration::from_secs(self.cfg.judge_timeout_secs),
                )
                .await
            }
            _ => judge::fallback(req.kind, lang),
        }
    }

    /// The agent and Lucina's judgement, without running anything (the
    /// proactive engine's dry run).
    pub async fn plan(&self, req: &Request) -> Result<(AgentId, Judgement), Refusal> {
        if !self.cfg.enabled {
            return Err(Refusal::Disabled);
        }
        policy::hard_limits(req, None, self.cfg.max_depth)?;
        if let Some(t) = req.to {
            policy::hard_limits(req, Some(t), self.cfg.max_depth)?;
        }
        let (agent, _) = self.choose(req).await?;
        policy::hard_limits(req, Some(agent.id()), self.cfg.max_depth)?;
        let lang =
            crate::types::answer_lang(req.lang.as_deref().unwrap_or_else(|| guess_lang(&req.task)))
                .to_owned();
        Ok((agent.id(), self.self_review(req, agent.id(), &lang).await))
    }

    // ------------------------------------------------------------ start

    /// Start a delegation. Never asks the owner: hard limits, budget and
    /// Lucina's own judgement decide.
    pub async fn start(self: &Arc<Self>, req: Request) -> StartOutcome {
        let lang =
            crate::types::answer_lang(req.lang.as_deref().unwrap_or_else(|| guess_lang(&req.task)))
                .to_owned();
        let refuse = |agent: Option<AgentId>, r: Refusal| {
            tracing::info!(
                agent = agent.map(AgentId::as_str),
                kind = req.kind.as_str(),
                origin = req.origin.as_str(),
                code = r.code(),
                "delegation refused"
            );
            self.audit_refusal(&req, agent, &r);
            StartOutcome::Refused(r)
        };
        if !self.cfg.enabled {
            return refuse(None, Refusal::Disabled);
        }
        if let Err(r) = policy::hard_limits(&req, None, self.cfg.max_depth) {
            return refuse(None, r);
        }
        let repo = match (req.kind, &req.workspace) {
            (Kind::Code, _) if self.cfg.isolation == Isolation::None => {
                return refuse(
                    None,
                    Refusal::Workspace("code runs only in the bubblewrap sandbox".into()),
                )
            }
            (Kind::Code, Some(w)) => match sandbox::check_repo(&self.cfg, w) {
                Ok(p) => Some(p),
                Err(e) => return refuse(None, Refusal::Workspace(e)),
            },
            (_, Some(_)) => {
                return refuse(
                    None,
                    Refusal::Workspace("only code tasks take a workspace".into()),
                )
            }
            _ => None,
        };
        if let Some(t) = req.to {
            if let Err(r) = policy::hard_limits(&req, Some(t), self.cfg.max_depth) {
                return refuse(Some(t), r);
            }
        }
        let (agent, fallback) = match self.choose(&req).await {
            Ok(a) => a,
            Err(r) => return refuse(req.to, r),
        };
        // `id` does the work; the job keeps the agent asked for when Mistral stands in.
        let id = agent.id();
        let asked = fallback.as_ref().map_or(id, |f| f.from);
        if let Err(r) = policy::hard_limits(&req, Some(id), self.cfg.max_depth) {
            return refuse(Some(id), r);
        }
        // One job per agent: claim it now (released when the job ends).
        let Some(claim) = claim(&self.running, id) else {
            return refuse(Some(id), Refusal::Busy(id));
        };
        let now = self.now();
        if let Err((r, notice)) = self.state.reserve(id, &self.cfg.budget, now) {
            if let Some(n) = notice {
                self.announce_pause(&n, &lang, &req.channel).await;
            }
            return refuse(Some(id), r);
        }

        // Lucina judges herself.
        let judgement = self.self_review(&req, id, &lang).await;
        let job_id = uuid::Uuid::new_v4().to_string();
        let mut task = req.task.clone();
        let mut job = JobRecord {
            id: job_id.clone(),
            agent: asked,
            answered_by: Some(id),
            fallback,
            backend: agent.backend().await.unwrap_or_default(),
            kind: req.kind,
            origin: req.origin,
            caller: req.caller.clone(),
            status: JobStatus::Running,
            task: clip(&redacted(&req.task), 160),
            lang: lang.clone(),
            session_id: req.session_id.clone(),
            channel: req.channel.clone(),
            judgement: Some(judgement.clone()),
            created_at: now_iso(),
            finished_at: None,
            duration_ms: 0,
            result: None,
            summary: None,
            note_slug: None,
            error: None,
            session: None,
            turns: 0,
            sandbox: None,
            branch: None,
            repo: None,
            undo: None,
        };
        let skip = |mut job: JobRecord, extra: Option<&str>| {
            self.state.release(id, self.now());
            job.status = JobStatus::Skipped;
            job.finished_at = Some(now_iso());
            if let (Some(e), Some(j)) = (extra, job.judgement.as_mut()) {
                j.why = format!("{} {e}", j.why).trim().to_owned();
            }
            self.store(&job);
            self.post(&job);
            self.audit(&job, "skipped");
            tracing::info!(job = %job.id, agent = id.as_str(), "delegation skipped by self-review");
            StartOutcome::Skipped(job)
        };
        match judgement.decision {
            Decision::Skip => return skip(job, None),
            Decision::Revise => {
                let revised = judgement.revised_task.clone().unwrap_or_default();
                let again = Request {
                    task: revised.clone(),
                    ..req.clone()
                };
                if policy::hard_limits(&again, Some(id), self.cfg.max_depth).is_err() {
                    return skip(job, Some("(the revised task broke a hard limit)"));
                }
                task = revised;
                job.task = clip(&redacted(&task), 160);
            }
            Decision::Act => {}
        }

        // The sandbox.
        let ws = match sandbox::prepare(&self.cfg, &job_id, repo.as_deref()).await {
            Ok(ws) => ws,
            Err(e) => {
                self.state.release(id, self.now());
                job.status = JobStatus::Error;
                job.error = Some(e.clone());
                job.finished_at = Some(now_iso());
                self.store(&job);
                self.post(&job);
                self.audit(&job, "error");
                return refuse(Some(id), Refusal::Workspace(e));
            }
        };
        job.sandbox = Some(ws.dir.to_string_lossy().into_owned());
        job.branch = ws.branch.clone();
        job.repo = ws.repo.as_ref().map(|r| r.to_string_lossy().into_owned());

        let (clean_task, _) = policy::sanitize_outbound(&task);
        let prompt = policy::build_prompt(
            job.short_id(),
            id,
            req.kind,
            &clean_task,
            &lang,
            Some(&ws.workdir.to_string_lossy()),
            ws.branch.as_deref(),
        );
        // Once more over the whole prompt: nothing secret leaves.
        let (prompt, _) = policy::sanitize_outbound(&prompt);
        let resume_session = req
            .resume_job
            .as_deref()
            .and_then(|j| self.job(j))
            .filter(|j| j.answered_by() == id)
            .and_then(|j| j.session);
        let ajob = AgentJob {
            job_id: job_id.clone(),
            kind: req.kind,
            prompt,
            workdir: ws.workdir.clone(),
            writable: ws.dir.clone(),
            timeout: self.cfg.timeout(id, req.kind),
            max_turns: self
                .cfg
                .claude
                .max_turns_per_job
                .min(self.state.turns_left(id, &self.cfg.budget, now))
                .max(1),
            resume_session,
            isolation: self.cfg.isolation,
        };
        self.store(&job);
        tracing::info!(job = %job.id, agent = id.as_str(), backend = %job.backend, kind = req.kind.as_str(), decision = judgement.decision.as_str(), risk = judgement.risk.as_str(), "delegation started");

        let (tx, rx) = watch::channel(None);
        let tx = Arc::new(tx);
        let me = Arc::clone(self);
        let started_job = job.clone();
        let sandbox_dir = ws.dir.clone();
        let task_tx = tx.clone();
        // Registered under the lock the task takes to leave the map: it cannot leave before it is in.
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let handle = tokio::spawn(async move {
            let _claim = claim;
            let done = me.run_job(agent, ajob, ws, job).await;
            me.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&done.id);
            let _ = task_tx.send(Some(done));
        });
        tasks.insert(
            job_id,
            RunningTask {
                handle,
                runner: id,
                started: Instant::now(),
                sandbox: sandbox_dir,
                tx,
            },
        );
        drop(tasks);
        StartOutcome::Started(Started {
            job: started_job,
            rx,
        })
    }

    async fn run_job(
        &self,
        agent: Arc<dyn Agent>,
        ajob: AgentJob,
        ws: Workspace,
        mut job: JobRecord,
    ) -> JobRecord {
        let t0 = Instant::now();
        let inline = self.inline_wait();
        let result = {
            let run = agent.run(&ajob);
            tokio::pin!(run);
            tokio::select! {
                r = &mut run => r,
                _ = tokio::time::sleep(inline) => {
                    // Taking a while: show it on the timeline.
                    self.post(&job);
                    run.await
                }
            }
        };
        let runner = agent.id();
        let ms = t0.elapsed().as_millis() as u64;
        let turns = result.as_ref().map(|r| r.turns).unwrap_or(0);
        let now = self.now();
        self.state.record(runner, ms, turns, now);
        let result = match result {
            Ok(reply) => {
                self.state.clear_down(runner, now);
                Ok(reply)
            }
            Err(e) => match self.note_outage(runner, &job.backend, &e) {
                // It never answered: Mistral answers instead (never for code).
                Some(state) => match self.fallback_run(&ajob, &mut job, runner, state).await {
                    Some(r) => r,
                    None => Err(e),
                },
                None => Err(e),
            },
        };
        job.duration_ms = t0.elapsed().as_millis() as u64;
        job.turns = result.as_ref().map(|r| r.turns).unwrap_or(turns);
        let lang = job.lang.clone();
        let answered = job.answered_by();
        match result {
            Ok(reply) => {
                let text = redacted(&reply.text);
                job.backend = reply.backend.clone();
                job.session = reply.session.clone();
                if job.kind == Kind::Code {
                    match sandbox::publish_branch(&ws).await {
                        Ok(Some(undo)) => job.undo = Some(undo),
                        Ok(None) => {
                            if ws.repo.is_some() {
                                // No commits: nothing left in the repo.
                                job.branch = None;
                            }
                            job.undo = Some(format!("rm -rf {}", ws.dir.display()));
                        }
                        Err(e) => {
                            job.undo = Some(format!("rm -rf {}", ws.dir.display()));
                            tracing::warn!(job = %job.id, error = %e, "delegate: branch not published");
                        }
                    }
                } else {
                    let _ = std::fs::remove_dir_all(&ws.dir);
                }
                let long = text.chars().count() > self.cfg.note_threshold_chars;
                // A self-test leaves nothing in his notes.
                if (long || job.kind == Kind::Code) && !self.sink.quiet(&job.session_id) {
                    let title = format!("{}: {}", answered.label(), clip(&job.task, 60));
                    let head = format!(
                        "> {} · {} · {} · {}\n> {}\n\n",
                        answered.label(),
                        job.backend,
                        job.kind.as_str(),
                        card::duration(job.duration_ms),
                        job.task
                    );
                    let tags = vec![
                        "lucina".to_owned(),
                        "delegation".to_owned(),
                        answered.as_str().to_owned(),
                    ];
                    match self
                        .sink
                        .save_note(&title, &format!("{head}{text}"), &tags)
                        .await
                    {
                        Ok(slug) => job.note_slug = Some(slug),
                        Err(e) => {
                            tracing::warn!(job = %job.id, error = %e, "delegate: note not saved")
                        }
                    }
                }
                if long {
                    job.summary = self.summarize(&text, answered, &lang).await;
                }
                job.result = Some(clip(&text, RESULT_CHARS));
                job.status = JobStatus::Done;
            }
            Err(e) => {
                if job.kind != Kind::Code {
                    let _ = std::fs::remove_dir_all(&ws.dir);
                } else {
                    job.undo = Some(format!("rm -rf {}", ws.dir.display()));
                }
                job.error = Some(clip(&redacted(&e), 300));
                job.status = JobStatus::Error;
            }
        }
        job.finished_at = Some(now_iso());
        self.store(&job);
        self.post(&job);
        if job.caller.is_none() && !self.sink.quiet(&job.session_id) && job.duration_ms > inline.as_millis() as u64 {
            let instead = job
                .fallback
                .as_ref()
                .map(|f| format!(" ({} was {})", f.from.label(), f.reason))
                .unwrap_or_default();
            let body = match job.status {
                JobStatus::Done => format!(
                    "{}{instead} {}: {}",
                    answered.label(),
                    if lang == "it" {
                        "ha risposto"
                    } else {
                        "answered"
                    },
                    clip(
                        job.summary
                            .as_deref()
                            .or(job.result.as_deref())
                            .unwrap_or(""),
                        300
                    )
                ),
                _ => format!(
                    "{}{instead} {}: {}",
                    answered.label(),
                    if lang == "it" {
                        "non è riuscito"
                    } else {
                        "failed"
                    },
                    job.error.as_deref().unwrap_or("")
                ),
            };
            self.sink.notify("Lucina", &body).await;
        }
        self.audit(&job, job.status.as_str());
        tracing::info!(job = %job.id, agent = job.agent.as_str(), answered_by = answered.as_str(), status = job.status.as_str(), ms = job.duration_ms, turns = job.turns, "delegation finished");
        job
    }

    /// A run failed with an out-of-credits, signed-out or rate-limit error: remember the backend as
    /// down (the state word), so the next request does not try it again until then.
    fn note_outage(&self, agent: AgentId, backend: &str, error: &str) -> Option<&'static str> {
        let (state, hold) = outage(error)?;
        let now = self.now();
        let down = Down {
            backend: backend.to_owned(),
            since: now,
            until: now + hold,
            state: state.to_owned(),
            reason: clip(&redacted(error), 160),
        };
        tracing::warn!(agent = agent.as_str(), backend, state, until = %down.until, "delegate: backend down");
        self.state.mark_down(agent, down, now);
        Some(state)
    }

    /// The same job once on Mistral, when it may stand in (not code, not Mistral itself, free, in
    /// budget). `None`: no stand-in; the original error stays.
    async fn fallback_run(
        &self,
        ajob: &AgentJob,
        job: &mut JobRecord,
        from: AgentId,
        state: &str,
    ) -> Option<Result<crate::agents::AgentReply, String>> {
        if job.kind == Kind::Code || from == AgentId::Mistral || job.fallback.is_some() {
            return None;
        }
        let m = self.agent(AgentId::Mistral)?.clone();
        let now = self.now();
        self.usable(&m, job.kind, now).await.ok()?;
        let _claim = claim(&self.running, AgentId::Mistral)?;
        if let Err((r, notice)) = self.state.reserve(AgentId::Mistral, &self.cfg.budget, now) {
            if let Some(n) = notice {
                self.announce_pause(&n, &job.lang, &job.channel).await;
            }
            tracing::info!(code = r.code(), "delegate: Mistral cannot stand in");
            return None;
        }
        let mut mjob = ajob.clone();
        mjob.timeout = self.cfg.timeout(AgentId::Mistral, job.kind);
        let t0 = Instant::now();
        let r = m.run(&mjob).await;
        let now = self.now();
        self.state.record(
            AgentId::Mistral,
            t0.elapsed().as_millis() as u64,
            r.as_ref().map_or(0, |r| r.turns),
            now,
        );
        match r {
            Ok(reply) => {
                self.state.clear_down(AgentId::Mistral, now);
                job.fallback = Some(Fallback {
                    from,
                    from_backend: job.backend.clone(),
                    reason: state.to_owned(),
                });
                job.answered_by = Some(AgentId::Mistral);
                tracing::info!(job = %job.id, from = from.as_str(), state, "delegation: Mistral answered instead");
                Some(Ok(reply))
            }
            Err(e) => {
                let backend = m.backend().await.unwrap_or_default();
                self.note_outage(AgentId::Mistral, &backend, &e);
                tracing::warn!(job = %job.id, error = %clip(&redacted(&e), 200), "delegate: Mistral could not stand in");
                None
            }
        }
    }

    async fn summarize(&self, text: &str, agent: AgentId, lang: &str) -> Option<String> {
        let model = self.judge_model.as_ref()?;
        let language = match lang {
            "it" => "Italian",
            "da" => "Danish",
            _ => "English",
        };
        let req = ChatRequest {
            messages: vec![
                Message::system(format!("Summarize the answer {} gave for Giuseppe in 2-3 short sentences in {language}. Keep the key facts, numbers and names. No preamble.", agent.label())),
                Message::user(text.chars().take(12_000).collect::<String>()),
            ],
            temperature: Some(0.2),
            max_tokens: Some(1500),
            purpose: "delegate-summary",
            ..ChatRequest::default()
        };
        match tokio::time::timeout(Duration::from_secs(25), model.complete(req)).await {
            // A summary cut by the token limit ends with its last full sentence.
            Ok(Ok(r)) => {
                let s = r.message.text().trim().to_owned();
                let s = if s.ends_with(['.', '!', '?', ')', '»', '"']) {
                    s
                } else {
                    clip_sentences(&s, s.chars().count().saturating_sub(1).max(1))
                };
                Some(s).filter(|s| !s.is_empty())
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------ owner

    /// The owner's switches: the reply text.
    pub fn switch(&self, s: &SwitchIntent) -> String {
        let now = self.now();
        // Never Italian (owner rule 2026-11-02): an Italian intent is answered in English.
        let l = crate::types::answer_lang(s.lang);
        let pick = |it: String, en: String| if l == "it" { it } else { en };
        match s.switch {
            Switch::Off(Some(a)) => {
                self.state.set_enabled(Some(a), false, now);
                pick(
                    format!(
                        "Ok, non uso più {} finché non mi dici \"puoi usare {}\".",
                        a.label(),
                        a.label()
                    ),
                    format!(
                        "Ok, I will not use {} until you say \"you can use {} again\".",
                        a.label(),
                        a.label()
                    ),
                )
            }
            Switch::Off(None) => {
                self.state.set_enabled(None, false, now);
                pick(
                    "Ok, smetto di delegare ad altri agenti. Dimmi \"riprendi a delegare\" quando vuoi.".into(),
                    "Ok, I stop delegating to other agents. Say \"resume delegating\" when you want.".into(),
                )
            }
            Switch::On(Some(a)) => {
                self.state.set_enabled(Some(a), true, now);
                pick(
                    format!("Ok, posso di nuovo usare {}.", a.label()),
                    format!("Ok, I can use {} again.", a.label()),
                )
            }
            Switch::On(None) => {
                self.state.set_enabled(None, true, now);
                pick(
                    "Ok, riprendo a delegare quando serve.".into(),
                    "Ok, I delegate again when it helps.".into(),
                )
            }
            Switch::Status => {
                let snap = self.state.snapshot(&self.cfg.budget, now);
                let line = |a: &AgentId| {
                    let v = &snap["agents"][a.as_str()];
                    let health = v["health"].as_str().filter(|h| *h != "ok");
                    format!(
                        "{} {}: {}/{}{}",
                        if v["enabled"] == true && health.is_none() {
                            "✅"
                        } else {
                            "⛔"
                        },
                        a.label(),
                        v["calls"],
                        v["callsCap"],
                        health.map(|h| format!(" ({h})")).unwrap_or_default()
                    )
                };
                let agents = AgentId::ALL
                    .iter()
                    .filter(|a| self.agent(**a).is_some())
                    .map(line)
                    .collect::<Vec<_>>()
                    .join(" · ");
                let stopped = snap["stopped"] == true;
                pick(
                    format!(
                        "Deleghe oggi: {}/{}{}. {agents}",
                        snap["totalCalls"],
                        snap["totalCallsCap"],
                        if stopped {
                            " (ferme: mi hai detto di non delegare)"
                        } else {
                            ""
                        }
                    ),
                    format!(
                        "Delegations today: {}/{}{}. {agents}",
                        snap["totalCalls"],
                        snap["totalCallsCap"],
                        if stopped {
                            " (stopped: you told me not to delegate)"
                        } else {
                            ""
                        }
                    ),
                )
            }
        }
    }

    /// "annulla delega <id>": remove what a job left (its sandbox, its branch).
    pub async fn undo(&self, id: &str, lang: &str) -> String {
        // Never Italian (owner rule 2026-11-02).
        let it = crate::types::answer_lang(lang) == "it";
        let Some(mut job) = self.job(id) else {
            return if it {
                format!("Non trovo una delega {id}.")
            } else {
                format!("I cannot find delegation {id}.")
            };
        };
        if job.undo.is_none() {
            return if it {
                "Quella delega non ha lasciato niente da annullare.".into()
            } else {
                "That delegation left nothing to undo.".into()
            };
        }
        match sandbox::undo(
            &self.cfg,
            job.sandbox.as_deref(),
            job.repo.as_deref(),
            job.branch.as_deref(),
        )
        .await
        {
            Ok(done) => {
                job.undo = None;
                self.store(&job);
                self.audit(&job, "undone");
                if it {
                    format!("Fatto ↩️ ({}).", done.join(", "))
                } else {
                    format!("Done ↩️ ({}).", done.join(", "))
                }
            }
            Err(e) => {
                if it {
                    format!("Non sono riuscita ad annullarla: {e}.")
                } else {
                    format!("I could not undo it: {e}.")
                }
            }
        }
    }

    /// "stop it": abort a running job (its processes die with it), record it as cancelled, free the
    /// agent. `Err` says why not: unknown job, or already finished.
    pub async fn cancel(&self, id_or_prefix: &str, lang: &str) -> Result<JobRecord, String> {
        let da = answer_lang(lang) == "da";
        let Some(job) = self.job(id_or_prefix) else {
            return Err(if da {
                "ingen sådan opgave".into()
            } else {
                "no such job".into()
            });
        };
        let task = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job.id);
        let Some(task) = task else {
            return Err(self.already(&job, da));
        };
        task.handle.abort();
        // Wait until the task is gone: its claim is released and its child processes are killed.
        let ended_on_its_own = task.handle.await.is_ok();
        let current = self.job(&job.id).unwrap_or(job);
        if ended_on_its_own || current.status != JobStatus::Running {
            return Err(self.already(&current, da));
        }
        let mut job = current;
        let now = self.now();
        job.status = JobStatus::Error;
        job.error = Some(CANCELLED.into());
        job.finished_at = Some(now_iso());
        job.duration_ms = task.started.elapsed().as_millis() as u64;
        self.state.record(task.runner, job.duration_ms, 0, now);
        if job.kind == Kind::Code {
            job.undo = Some(format!("rm -rf {}", task.sandbox.display()));
        } else {
            let _ = std::fs::remove_dir_all(&task.sandbox);
        }
        self.store(&job);
        self.post(&job);
        self.audit(&job, "cancelled");
        let _ = task.tx.send(Some(job.clone()));
        tracing::info!(job = %job.id, agent = job.agent.as_str(), ms = job.duration_ms, "delegation cancelled");
        Ok(job)
    }

    /// "already done at 14:02".
    fn already(&self, job: &JobRecord, da: bool) -> String {
        let at = job.finished_at.as_deref().unwrap_or(&job.created_at);
        let at = DateTime::parse_from_rfc3339(at)
            .map(|t| self.hhmm(t.with_timezone(&Utc)))
            .unwrap_or_else(|_| "?".into());
        if da {
            format!("allerede {} kl. {at}", job.status.as_str())
        } else {
            format!("already {} at {at}", job.status.as_str())
        }
    }

    /// `/api/secretary/delegations`: switches, budget, agents, recent jobs.
    pub async fn status(&self) -> Value {
        let now = self.now();
        let budget = self.state.snapshot(&self.cfg.budget, now);
        let mut agents = serde_json::Map::new();
        for a in &self.agents {
            let health = budget["agents"][a.id().as_str()]["health"].clone();
            agents.insert(
                a.id().as_str().into(),
                match a.backend().await {
                    Ok(b) => json!({"backend": b, "ready": health == "ok", "health": health}),
                    Err(e) => json!({"backend": null, "ready": false, "why": e, "health": health}),
                },
            );
        }
        json!({
            "enabled": self.cfg.enabled,
            "maxDepth": self.cfg.max_depth.min(1),
            "isolation": match self.cfg.isolation { Isolation::Bwrap => "bwrap", Isolation::None => "none" },
            "budget": budget,
            "agents": agents,
            "jobs": self.recent(10),
        })
    }
}

/// What a chat turn says about a finished, running, skipped or refused job.
pub fn chat_result(job: &JobRecord) -> Value {
    json!({
        "status": job.status.as_str(),
        "jobId": job.id,
        "agent": job.agent.label(),
        "answeredBy": job.answered_by().label(),
        "fallback": job.fallback.as_ref().map(|f| card::fallback_sentence(f, job.answered_by(), "en")),
        "backend": job.backend,
        "kind": job.kind.as_str(),
        "durationSec": job.duration_ms / 1000,
        "answer": job.summary.clone().or_else(|| job.result.clone().map(|r| clip(&r, 6000))),
        "fullAnswerInNote": job.note_slug,
        "error": job.error,
        "why": job.judgement.as_ref().map(|j| j.why.clone()),
        "undo": job.undo,
    })
}
