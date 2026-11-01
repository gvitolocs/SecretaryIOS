//! The delegation engine: hard limits → agent choice → budget → Lucina's
//! self-review → sandbox → the job in the background → card, note,
//! notification, audit.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
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
use crate::state::{PauseNotice, StateStore};
use crate::types::{
    clip, clip_sentences, guess_lang, now_iso, AgentId, Decision, JobRecord, JobStatus, Judgement,
    Kind, Request,
};

const KEEP_JOBS: usize = 100;
const RESULT_CHARS: usize = 8000;

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
    audit: AuditLog,
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
        }
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
        let (usage, total) = self.state.usage(job.agent, Utc::now());
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

    async fn choose(&self, req: &Request) -> Result<Arc<dyn Agent>, Refusal> {
        let now = Utc::now();
        // Auto choice never picks the agent that is calling Lucina (`hard_limits` refuses a named one).
        let order = match req.to {
            Some(a) => vec![a],
            None => policy::preference(req.kind, &req.task)
                .into_iter()
                .filter(|a| !policy::is_caller(req, *a))
                .collect(),
        };
        self.state.check_global(now)?;
        let mut errs: Vec<Refusal> = Vec::new();
        for id in order {
            let Some(agent) = self.agents.iter().find(|a| a.id() == id) else {
                if req.to.is_some() {
                    return Err(Refusal::Unavailable(id, "not configured".into()));
                }
                continue;
            };
            let check = async {
                if !agent.supports(req.kind) {
                    return Err(Refusal::Unsupported(id, req.kind));
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
                agent
                    .backend()
                    .await
                    .map_err(|why| Refusal::Unavailable(id, why))?;
                Ok(())
            }
            .await;
            match check {
                Ok(()) => return Ok(agent.clone()),
                Err(Refusal::Stopped) => return Err(Refusal::Stopped),
                Err(e) if req.to.is_some() => return Err(e),
                Err(e) => errs.push(e),
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
        let agent = self.choose(req).await?;
        policy::hard_limits(req, Some(agent.id()), self.cfg.max_depth)?;
        let lang = req
            .lang
            .clone()
            .unwrap_or_else(|| guess_lang(&req.task).to_owned());
        Ok((agent.id(), self.self_review(req, agent.id(), &lang).await))
    }

    // ------------------------------------------------------------ start

    /// Start a delegation. Never asks the owner: hard limits, budget and
    /// Lucina's own judgement decide.
    pub async fn start(self: &Arc<Self>, req: Request) -> StartOutcome {
        let lang = req
            .lang
            .clone()
            .unwrap_or_else(|| guess_lang(&req.task).to_owned());
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
        let agent = match self.choose(&req).await {
            Ok(a) => a,
            Err(r) => return refuse(req.to, r),
        };
        let id = agent.id();
        if let Err(r) = policy::hard_limits(&req, Some(id), self.cfg.max_depth) {
            return refuse(Some(id), r);
        }
        // One job per agent: claim it now (released when the job ends).
        let claim = {
            let mut run = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if !run.insert(id) {
                drop(run);
                return refuse(Some(id), Refusal::Busy(id));
            }
            Claim {
                running: self.running.clone(),
                agent: id,
            }
        };
        let now = Utc::now();
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
            agent: id,
            backend: agent.backend().await.unwrap_or_default(),
            kind: req.kind,
            origin: req.origin,
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
            self.state.release(id, Utc::now());
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
                self.state.release(id, Utc::now());
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
            .filter(|j| j.agent == id)
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
        let me = Arc::clone(self);
        let started_job = job.clone();
        tokio::spawn(async move {
            let _claim = claim;
            let done = me.run_job(agent, ajob, ws, job).await;
            let _ = tx.send(Some(done));
        });
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
        job.duration_ms = t0.elapsed().as_millis() as u64;
        let turns = result.as_ref().map(|r| r.turns).unwrap_or(0);
        self.state
            .record(job.agent, job.duration_ms, turns, Utc::now());
        job.turns = turns;
        let lang = job.lang.clone();
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
                if long || job.kind == Kind::Code {
                    let title = format!("{}: {}", job.agent.label(), clip(&job.task, 60));
                    let head = format!(
                        "> {} · {} · {} · {}\n> {}\n\n",
                        job.agent.label(),
                        job.backend,
                        job.kind.as_str(),
                        card::duration(job.duration_ms),
                        job.task
                    );
                    let tags = vec![
                        "lucina".to_owned(),
                        "delegation".to_owned(),
                        job.agent.as_str().to_owned(),
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
                    job.summary = self.summarize(&text, job.agent, &lang).await;
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
        if job.duration_ms > inline.as_millis() as u64 {
            let body = match job.status {
                JobStatus::Done => format!(
                    "{} {}: {}",
                    job.agent.label(),
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
                    "{} {}: {}",
                    job.agent.label(),
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
        tracing::info!(job = %job.id, agent = job.agent.as_str(), status = job.status.as_str(), ms = job.duration_ms, turns = job.turns, "delegation finished");
        job
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
        let now = Utc::now();
        let l = s.lang;
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
                let line = |a: AgentId| {
                    let v = &snap["agents"][a.as_str()];
                    format!(
                        "{} {}: {}/{}",
                        if v["enabled"] == true { "✅" } else { "⛔" },
                        a.label(),
                        v["calls"],
                        v["callsCap"]
                    )
                };
                let agents = AgentId::ALL.map(line).join(" · ");
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
        let it = lang == "it";
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

    /// `/api/secretary/delegations`: switches, budget, agents, recent jobs.
    pub async fn status(&self) -> Value {
        let mut agents = serde_json::Map::new();
        for a in &self.agents {
            agents.insert(
                a.id().as_str().into(),
                match a.backend().await {
                    Ok(b) => json!({"backend": b, "ready": true}),
                    Err(e) => json!({"backend": null, "ready": false, "why": e}),
                },
            );
        }
        json!({
            "enabled": self.cfg.enabled,
            "maxDepth": self.cfg.max_depth.min(1),
            "isolation": match self.cfg.isolation { Isolation::Bwrap => "bwrap", Isolation::None => "none" },
            "budget": self.state.snapshot(&self.cfg.budget, Utc::now()),
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
