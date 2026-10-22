//! The Meshy service: plans, the budget, and confirmed plans run as
//! background jobs (the Secretary job store) that report on the timeline,
//! in the app (`/v1/notify`) and on Telegram.
//!
//! A job runs the plan's steps in order. Each step creates one Meshy task
//! and polls it (`Retry-After`, else `LUCINA_MESHY_POLL_MS`) until it ends or
//! `LUCINA_MESHY_STEP_TIMEOUT_MS` passes. A task is **never created twice**:
//! a failed or timed-out poll keeps the task id and the job reports it. The
//! result (GLB, thumbnail, extra formats, the turntable MP4) goes to the
//! gallery on secretary-server; the job ends with a `model3d` block.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use chrono_tz::Tz;
use lucina_store::jobs::Job;
use lucina_store::timeline::{Meta, NewItem};
use lucina_store::{model3d, JobStore, TimelineStore};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::api::{MeshyApi, Task, TaskKind, MAX_DOWNLOAD_BYTES};
use crate::budget::{Ledger, LimitError, Limits};
use crate::error::MeshyError;
use crate::gallery::{self, Gallery};
use crate::key::KeySource;
use crate::plan::{self, Action, Args, InputRef, Plan, Source, StepKind};
use crate::preview::{self, PreviewConfig};
use crate::telegram::TelegramBridge;

pub const JOB_ACTION: &str = "meshy";
const MAX_UPLOAD_IMAGE: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MeshyConfig {
    /// `LUCINA_MESHY=true`.
    pub enabled: bool,
    /// `MESHY_API_BASE` (default `https://api.meshy.ai/openapi`).
    pub api_base: String,
    /// `MESHY_API_KEY_FILE` (0600).
    pub key_file: Option<PathBuf>,
    pub limits: Limits,
    /// Wait between polls when Meshy sends no `Retry-After` (default 5 s).
    pub poll: Duration,
    /// Shortest wait between polls (default 1 s; tests use less).
    pub poll_min: Duration,
    /// Give up polling one step after this (default 20 min).
    pub step_timeout: Duration,
    /// Background jobs at once (default 2; Meshy Pro queues 10 tasks).
    pub max_jobs: usize,
    /// `<data>/lucina-rs` (ledger, work files).
    pub state_dir: PathBuf,
    /// `upload:<file>` images (`LUCINA_MESHY_UPLOADS_DIR`, default `<data>/uploads`).
    pub uploads_dir: PathBuf,
    pub preview: PreviewConfig,
    pub telegram_url: Option<String>,
    pub telegram_token_file: Option<PathBuf>,
    pub secretary_url: Option<String>,
    pub secretary_token_file: Option<PathBuf>,
    /// Notify the apps when a job ends (`LUCINA_MESHY_NOTIFY`, default true).
    pub notify: bool,
    pub time_zone: Tz,
}

impl MeshyConfig {
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
        data_dir: &std::path::Path,
        time_zone: Tz,
        secretary_url: Option<String>,
        secretary_token_file: Option<PathBuf>,
    ) -> Self {
        let flag = |k: &str, d: bool| {
            get(k)
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(d)
        };
        let num = |k: &str, d: u64| get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        let mut preview = PreviewConfig {
            enabled: flag("LUCINA_MESHY_PREVIEW", true),
            ..PreviewConfig::default()
        };
        if let Some(s) = get("LUCINA_MESHY_PREVIEW_SCRIPT") {
            preview.script = PathBuf::from(s);
        }
        if let Some(n) = get("LUCINA_MESHY_PREVIEW_NODE") {
            preview.node = PathBuf::from(n);
        }
        for (var, env) in [
            ("LUCINA_MESHY_PREVIEW_THREE", "MESHY_PREVIEW_THREE"),
            (
                "LUCINA_MESHY_PREVIEW_PLAYWRIGHT",
                "MESHY_PREVIEW_PLAYWRIGHT",
            ),
            ("LUCINA_MESHY_PREVIEW_CHROME", "MESHY_PREVIEW_CHROME"),
            ("LUCINA_MESHY_FFMPEG", "FFMPEG_PATH"),
        ] {
            if let Some(v) = get(var) {
                preview.env.push((env.into(), v));
            }
        }
        preview.seconds = (num("LUCINA_MESHY_PREVIEW_SECONDS", 5) as f32).clamp(4.0, 6.0);
        Self {
            enabled: flag("LUCINA_MESHY", false),
            api_base: get("MESHY_API_BASE").unwrap_or_else(|| crate::api::DEFAULT_BASE.into()),
            key_file: get("MESHY_API_KEY_FILE").map(PathBuf::from),
            limits: Limits::new(
                num("LUCINA_MESHY_DAILY_CREDITS", 60) as u32,
                num("LUCINA_MESHY_MAX_PLAN_CREDITS", 60) as u32,
            ),
            poll: Duration::from_millis(num("LUCINA_MESHY_POLL_MS", 5000).max(1)),
            poll_min: Duration::from_millis(num("LUCINA_MESHY_POLL_MIN_MS", 1000).max(1)),
            step_timeout: Duration::from_millis(
                num("LUCINA_MESHY_STEP_TIMEOUT_MS", 20 * 60_000).max(10),
            ),
            max_jobs: num("LUCINA_MESHY_MAX_JOBS", 2).clamp(1, 8) as usize,
            state_dir: data_dir.join("lucina-rs"),
            uploads_dir: get("LUCINA_MESHY_UPLOADS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("uploads")),
            preview,
            telegram_url: get("LUCINA_MESHY_TELEGRAM_URL"),
            telegram_token_file: get("LUCINA_MESHY_TELEGRAM_TOKEN_FILE").map(PathBuf::from),
            secretary_url,
            secretary_token_file,
            notify: flag("LUCINA_MESHY_NOTIFY", true),
            time_zone,
        }
    }
}

/// Where a confirmed plan came from (for the reports).
#[derive(Debug, Clone, Default)]
pub struct Origin {
    /// `web` | `ios` | `telegram`.
    pub channel: String,
    pub session_id: String,
    pub reply_to: Option<String>,
    /// `it` (default) or `en` for the job's own messages.
    pub lang: String,
}

impl Origin {
    fn it(&self) -> bool {
        self.lang != "en"
    }
}

pub struct MeshyService {
    pub cfg: MeshyConfig,
    pub api: MeshyApi,
    pub ledger: Ledger,
    pub gallery: Gallery,
    pub telegram: TelegramBridge,
    pub key: Arc<dyn KeySource>,
    jobs: Arc<JobStore>,
    timeline: Arc<TimelineStore>,
    http: reqwest::Client,
    slots: Semaphore,
}

#[derive(Debug, Clone)]
struct StepDone {
    step: StepKind,
    task: Task,
    credits: u32,
}

/// Why a job stopped.
#[derive(Debug)]
struct Failure {
    step: Option<StepKind>,
    task_id: Option<String>,
    error: String,
    code: &'static str,
}

impl MeshyService {
    pub fn new(
        cfg: MeshyConfig,
        key: Arc<dyn KeySource>,
        jobs: Arc<JobStore>,
        timeline: Arc<TimelineStore>,
    ) -> Self {
        let api = MeshyApi::new(&cfg.api_base, key.clone());
        let ledger = Ledger::file(cfg.state_dir.join("meshy-ledger.json"), cfg.time_zone);
        let gallery = Gallery::new(cfg.secretary_url.clone(), cfg.secretary_token_file.clone());
        let telegram =
            TelegramBridge::new(cfg.telegram_url.clone(), cfg.telegram_token_file.clone());
        let slots = Semaphore::new(cfg.max_jobs);
        Self {
            cfg,
            api,
            ledger,
            gallery,
            telegram,
            key,
            jobs,
            timeline,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client"),
            slots,
        }
    }

    /// For `/health` (no secrets).
    pub fn health(&self) -> Value {
        json!({
            "enabled": self.cfg.enabled,
            "apiBase": self.cfg.api_base,
            "key": self.key.describe(),
            "budget": self.budget(),
            "gallery": self.gallery.configured(),
            "telegram": self.telegram.configured(),
            "preview": self.cfg.preview.enabled,
        })
    }

    pub fn budget(&self) -> Value {
        let spent = self.ledger.spent_today(Utc::now());
        json!({
            "dailyBudget": self.cfg.limits.daily_budget,
            "maxPerPlan": self.cfg.limits.max_plan,
            "spentToday": spent,
            "leftToday": self.cfg.limits.daily_budget.saturating_sub(spent),
            "hardCapPerDay": crate::budget::HARD_CAP_PER_DAY,
        })
    }

    /// A plan without network calls (post-processing inputs unresolved:
    /// a placeholder input stands in, the cost is the same).
    pub fn estimate(&self, args: &Args) -> Result<Plan, String> {
        let placeholder = InputRef {
            model_id: args.model_id.clone(),
            task_id: "pending".into(),
            kind: TaskKind::TextTo3d,
            rig_task_id: None,
            title: None,
            prompt: None,
        };
        plan::build(args, Some(&placeholder))
    }

    /// The real plan (resolves `model_id` through the gallery).
    pub async fn plan(&self, args: &Args) -> Result<Plan, String> {
        let action = args.planned_action()?;
        let needs_input = matches!(
            action,
            Action::Remesh | Action::Texture | Action::Rig | Action::Animate | Action::Export
        );
        if !needs_input {
            return plan::build(args, None);
        }
        let id = args
            .model_id
            .as_deref()
            .ok_or("model_id: which gallery model (mdl_…) to work on")?;
        let input = self.input_for(id).await?;
        plan::build(args, Some(&input))
    }

    async fn input_for(&self, model_id: &str) -> Result<InputRef, String> {
        let m = self
            .gallery
            .get(model_id)
            .await
            .map_err(|e| e.to_string())?;
        let task_id = m["meshy"]["taskId"]
            .as_str()
            .ok_or("that gallery model has no Meshy task (it cannot be post-processed)")?;
        let kind = m["meshy"]["taskKind"]
            .as_str()
            .and_then(TaskKind::parse)
            .ok_or("that gallery model has an unknown Meshy task kind")?;
        Ok(InputRef {
            model_id: Some(model_id.to_owned()),
            task_id: task_id.to_owned(),
            kind,
            rig_task_id: m["meshy"]["rigTaskId"].as_str().map(str::to_owned),
            title: m["title"].as_str().map(str::to_owned),
            prompt: m["prompt"].as_str().map(str::to_owned),
        })
    }

    pub fn check(&self, plan: &Plan) -> Result<(), LimitError> {
        self.ledger.check(self.cfg.limits, plan.total, Utc::now())
    }

    pub async fn list(&self, limit: u32) -> Result<Vec<Value>, String> {
        let models = self.gallery.list(limit).await.map_err(|e| e.to_string())?;
        Ok(models.iter().map(gallery::summary).collect())
    }

    /// A Meshy job's status (`status` action).
    pub fn job(&self, id: &str) -> Option<Value> {
        let job = self.jobs.find(id).ok().flatten()?;
        if job.action != JOB_ACTION {
            return None;
        }
        let mut v = job.public();
        for k in ["progress", "modelId", "plan", "credits"] {
            if let Some(x) = job.extra.get(k) {
                v[k] = x.clone();
            }
        }
        Some(v)
    }

    /// Reserve the credits and start the job in the background.
    pub async fn start(self: &Arc<Self>, plan: Plan, origin: Origin) -> Result<Job, String> {
        if !self.cfg.enabled {
            return Err("Meshy is off (LUCINA_MESHY=false)".into());
        }
        self.key.api_key().await.map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        self.ledger
            .reserve(self.cfg.limits, &id, plan.total, Utc::now())
            .map_err(|e| e.to_string())?;
        let now = lucina_store::timeutil::now_iso();
        let mut extra = serde_json::Map::new();
        extra.insert("plan".into(), plan.to_json());
        extra.insert("credits".into(), json!({"estimate": plan.total}));
        let job = Job {
            id: id.clone(),
            action: JOB_ACTION.into(),
            status: "queued".into(),
            result: None,
            error: None,
            image_url: None,
            session_id: origin.session_id.clone(),
            client_message_id: None,
            created_at: now.clone(),
            started_at: None,
            finished_at: None,
            updated_at: now,
            timeline_item_id: None,
            extra,
        };
        let job = self.jobs.insert(job).map_err(|e| {
            self.ledger.settle(&id, 0);
            format!("could not record the job: {e}")
        })?;
        tracing::info!(job_id = %id, credits = plan.total, steps = plan.steps.len(), "meshy job queued");
        let me = self.clone();
        tokio::spawn(async move {
            let _slot = me.slots.acquire().await;
            me.run(id, plan, origin).await;
        });
        Ok(job)
    }

    fn progress(
        &self,
        job_id: &str,
        index: usize,
        of: usize,
        step: StepKind,
        task_id: Option<&str>,
        percent: u8,
    ) {
        let _ = self.jobs.update(job_id, |j| {
            j.status = "running".into();
            if j.started_at.is_none() {
                j.started_at = Some(lucina_store::timeutil::now_iso());
            }
            j.extra.insert(
                "progress".into(),
                json!({"step": step.as_str(), "index": index + 1, "of": of, "taskId": task_id, "percent": percent}),
            );
        });
    }

    /// Wait for a task to end. Transient errors wait and poll again; the task
    /// is never re-created.
    async fn poll(
        &self,
        job_id: &str,
        kind: TaskKind,
        id: &str,
        index: usize,
        of: usize,
        step: StepKind,
    ) -> Result<Task, MeshyError> {
        let started = tokio::time::Instant::now();
        let mut last_percent = 255u8;
        loop {
            let wait = match self.api.get(kind, id).await {
                Ok((task, ra)) => {
                    if task.status.terminal() {
                        return Ok(task);
                    }
                    if task.progress != last_percent {
                        last_percent = task.progress;
                        self.progress(job_id, index, of, step, Some(id), task.progress);
                    }
                    ra.map(Duration::from_secs).unwrap_or(self.cfg.poll)
                }
                Err(MeshyError::RateLimited { retry_after }) => Duration::from_secs(retry_after),
                Err(e) if e.transient() => {
                    tracing::warn!(
                        job_id,
                        task_id = id,
                        code = e.code(),
                        "meshy poll failed; retrying"
                    );
                    self.cfg.poll
                }
                Err(e) => return Err(e),
            };
            let wait = wait.clamp(self.cfg.poll_min, Duration::from_secs(30));
            if started.elapsed() + wait > self.cfg.step_timeout {
                return Err(MeshyError::Timeout {
                    task_id: id.to_owned(),
                    after_s: started.elapsed().as_secs(),
                });
            }
            tokio::time::sleep(wait).await;
        }
    }

    fn image_ref(&self, r: &str) -> Result<String, String> {
        let Some(name) = r.strip_prefix("upload:") else {
            return Ok(r.to_owned());
        };
        let path = self.cfg.uploads_dir.join(name);
        let meta = std::fs::metadata(&path).map_err(|_| format!("upload {name} not found"))?;
        if !meta.is_file() || meta.len() > MAX_UPLOAD_IMAGE {
            return Err(format!("upload {name}: not a file of at most 10 MB"));
        }
        let bytes = std::fs::read(&path).map_err(|_| format!("upload {name} unreadable"))?;
        let mime = image_mime(&bytes)
            .ok_or_else(|| format!("upload {name}: not a PNG, JPEG or WebP image"))?;
        Ok(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    fn body(&self, step: StepKind, plan: &Plan, done: &[StepDone]) -> Result<Value, String> {
        let s = &plan.spec;
        let extra_formats: Vec<&String> = s.formats.iter().filter(|f| *f != "glb").collect();
        let last = |kinds: &[StepKind]| {
            done.iter()
                .rev()
                .find(|d| kinds.contains(&d.step))
                .map(|d| d.task.id.clone())
        };
        let input = match &s.source {
            Source::Input(i) => Some(i),
            _ => None,
        };
        // The newest model-producing task (or the gallery input).
        let model_task = last(&[
            StepKind::Refine,
            StepKind::Preview,
            StepKind::ImageTo3d,
            StepKind::MultiImageTo3d,
            StepKind::Remesh,
            StepKind::Retexture,
        ])
        .or_else(|| input.map(|i| i.task_id.clone()));
        let mut b = json!({});
        let geometry = |b: &mut Value| {
            if let Some(t) = &s.topology {
                b["topology"] = json!(t);
            }
            if let Some(p) = s.target_polycount {
                b["target_polycount"] = json!(p);
                b["should_remesh"] = json!(true);
            }
            if s.smart_topology {
                b["model_type"] = json!("smart-topology");
            }
            if s.rig {
                b["pose_mode"] = json!("a-pose");
            }
        };
        match step {
            StepKind::Preview => {
                let Source::Text { prompt } = &s.source else {
                    return Err("preview without a prompt".into());
                };
                b = json!({"mode": "preview", "prompt": prompt, "ai_model": s.ai_model, "name": s.title});
                geometry(&mut b);
                if s.texture.is_none() && !extra_formats.is_empty() {
                    b["target_formats"] = json!(s.formats);
                }
            }
            StepKind::Refine => {
                let preview = last(&[StepKind::Preview]).ok_or("refine without a preview")?;
                b = json!({"mode": "refine", "preview_task_id": preview, "enable_pbr": s.pbr,
                           "texture_resolution": s.texture.map(|t| t.as_str()).unwrap_or("2k")});
                if let Some(t) = &s.texture_prompt {
                    b["texture_prompt"] = json!(t);
                }
                if !extra_formats.is_empty() {
                    b["target_formats"] = json!(s.formats);
                }
            }
            StepKind::ImageTo3d | StepKind::MultiImageTo3d => {
                let Source::Images { urls } = &s.source else {
                    return Err("image step without images".into());
                };
                let urls: Vec<String> = urls
                    .iter()
                    .map(|u| self.image_ref(u))
                    .collect::<Result<_, _>>()?;
                b = if step == StepKind::ImageTo3d {
                    json!({"image_url": urls[0]})
                } else {
                    json!({"image_urls": urls})
                };
                b["ai_model"] = json!(s.ai_model);
                b["name"] = json!(s.title);
                b["should_texture"] = json!(s.texture.is_some());
                b["enable_pbr"] = json!(s.pbr);
                if let Some(t) = s.texture {
                    b["texture_resolution"] = json!(t.as_str());
                }
                if let Some(t) = &s.texture_prompt {
                    b["texture_prompt"] = json!(t);
                }
                geometry(&mut b);
                if !extra_formats.is_empty() {
                    b["target_formats"] = json!(s.formats);
                }
            }
            StepKind::Remesh => {
                b = json!({"input_task_id": model_task.ok_or("remesh without a model")?, "target_formats": s.formats});
                if let Some(t) = &s.topology {
                    b["topology"] = json!(t);
                }
                if let Some(p) = s.target_polycount {
                    b["target_polycount"] = json!(p);
                }
            }
            StepKind::Retexture => {
                let style = s
                    .texture_prompt
                    .clone()
                    .or_else(|| match &s.source {
                        Source::Text { prompt } => Some(prompt.clone()),
                        _ => None,
                    })
                    .ok_or("texture without a style prompt")?;
                b = json!({"input_task_id": model_task.ok_or("texture without a model")?, "text_style_prompt": style,
                           "enable_pbr": s.pbr, "texture_resolution": s.texture.map(|t| t.as_str()).unwrap_or("2k")});
            }
            StepKind::Rig => {
                b = json!({"input_task_id": model_task.ok_or("rig without a model")?, "height_meters": s.height_m});
            }
            StepKind::Animate => {
                let rig = last(&[StepKind::Rig])
                    .or_else(|| input.and_then(|i| i.rig_task_id.clone()))
                    .ok_or("animate without a rig")?;
                let ids: Vec<i64> = s.clips.iter().map(|c| c.id).collect();
                b = if ids.len() == 1 {
                    json!({"rig_task_id": rig, "action_id": ids[0]})
                } else {
                    json!({"rig_task_id": rig, "action_ids": ids})
                };
            }
            StepKind::Convert => {
                b = json!({"input_task_id": model_task.ok_or("export without a model")?, "target_formats": extra_formats});
            }
        }
        Ok(b)
    }

    async fn run(self: &Arc<Self>, job_id: String, plan: Plan, origin: Origin) {
        let of = plan.steps.len();
        let mut done: Vec<StepDone> = Vec::new();
        let mut failure: Option<Failure> = None;
        for (i, step) in plan.steps.iter().enumerate() {
            self.progress(&job_id, i, of, step.kind, None, 0);
            let body = match self.body(step.kind, &plan, &done) {
                Ok(b) => b,
                Err(e) => {
                    failure = Some(Failure {
                        step: Some(step.kind),
                        task_id: None,
                        error: e,
                        code: "plan",
                    });
                    break;
                }
            };
            let kind = step.kind.task_kind();
            let task_id = match self.api.create(kind, &body).await {
                Ok(id) => id,
                Err(e) => {
                    let mut error = e.to_string();
                    if e.create_outcome_unknown() {
                        error.push_str(" The request may still have created a task at Meshy; it was not resent (check meshy.ai before trying again).");
                    }
                    failure = Some(Failure {
                        step: Some(step.kind),
                        task_id: None,
                        error,
                        code: e.code(),
                    });
                    break;
                }
            };
            tracing::info!(job_id = %job_id, step = step.kind.as_str(), task_id = %task_id, "meshy task created");
            self.progress(&job_id, i, of, step.kind, Some(&task_id), 0);
            match self.poll(&job_id, kind, &task_id, i, of, step.kind).await {
                Ok(task) if task.status == crate::api::Status::Succeeded => {
                    let credits = task.consumed_credits.unwrap_or(step.credits);
                    done.push(StepDone {
                        step: step.kind,
                        task,
                        credits,
                    });
                }
                Ok(task) => {
                    failure = Some(Failure {
                        step: Some(step.kind),
                        task_id: Some(task_id.clone()),
                        error: format!(
                            "Meshy task {} {}: {}",
                            task_id,
                            task.status.as_str(),
                            task.error.unwrap_or_else(|| "no reason given".into())
                        ),
                        code: "task_failed",
                    });
                    break;
                }
                Err(e) => {
                    failure = Some(Failure {
                        step: Some(step.kind),
                        task_id: Some(task_id.clone()),
                        error: e.to_string(),
                        code: e.code(),
                    });
                    break;
                }
            }
        }
        let consumed: u32 = done.iter().map(|d| d.credits).sum();
        self.ledger.settle(&job_id, consumed);
        if failure.is_none() {
            match self.finish(&job_id, &plan, &done, consumed, &origin).await {
                Ok(()) => {}
                Err(e) => {
                    failure = Some(Failure {
                        step: None,
                        task_id: None,
                        error: e,
                        code: "delivery",
                    })
                }
            }
        }
        if let Some(f) = failure {
            self.fail(&job_id, &plan, &done, consumed, f, &origin).await;
        }
        self.jobs.finish(&job_id);
    }

    async fn finish(
        &self,
        job_id: &str,
        plan: &Plan,
        done: &[StepDone],
        consumed: u32,
        origin: &Origin,
    ) -> Result<(), String> {
        let s = &plan.spec;
        let last = done.last().ok_or("no step ran")?;
        let gen = done.iter().rev().find(|d| {
            matches!(
                d.step,
                StepKind::Refine
                    | StepKind::Preview
                    | StepKind::ImageTo3d
                    | StepKind::MultiImageTo3d
                    | StepKind::Remesh
                    | StepKind::Retexture
            )
        });
        let rig = done.iter().rev().find(|d| d.step == StepKind::Rig);
        let anim = done.iter().rev().find(|d| d.step == StepKind::Animate);
        let input = match &s.source {
            Source::Input(i) => Some(i.clone()),
            _ => None,
        };
        let work = self.cfg.state_dir.join("meshy-work").join(job_id);
        std::fs::create_dir_all(&work).map_err(|e| format!("work dir: {}", e.kind()))?;
        let dl = |e: MeshyError| format!("download failed: {e}");

        // Export only: new formats onto the existing gallery model.
        if s.action == Action::Export {
            let model_id = input
                .as_ref()
                .and_then(|i| i.model_id.clone())
                .ok_or("export without a gallery model")?;
            let mut added = Vec::new();
            for f in s.formats.iter().filter(|f| *f != "glb") {
                if let Some(url) = last.task.format_url(f) {
                    let bytes = self
                        .api
                        .download(url, MAX_DOWNLOAD_BYTES)
                        .await
                        .map_err(dl)?;
                    self.gallery
                        .put_file(&model_id, &format!("model.{f}"), bytes)
                        .await
                        .map_err(|e| e.to_string())?;
                    added.push(f.to_uppercase());
                }
            }
            let model = self
                .gallery
                .get(&model_id)
                .await
                .map_err(|e| e.to_string())?;
            let text = if origin.it() {
                format!(
                    "Ho esportato «{}» in {} ({} crediti).",
                    s.title,
                    added.join(", "),
                    consumed
                )
            } else {
                format!(
                    "Exported \"{}\" as {} ({} credits).",
                    s.title,
                    added.join(", "),
                    consumed
                )
            };
            self.deliver(job_id, plan, &model, consumed, None, text, origin)
                .await;
            let _ = std::fs::remove_dir_all(&work);
            return Ok(());
        }

        // The model: clips > rig (with its free walk) > generation.
        let (glb_url, model_task) = if let Some(a) = anim {
            (a.task.glb_url().map(str::to_owned), a)
        } else if let Some(r) = rig {
            let walk = r
                .task
                .basic_animations()
                .into_iter()
                .find(|(n, _)| *n == "Walking")
                .map(|(_, u)| u);
            (walk.or_else(|| r.task.glb_url().map(str::to_owned)), r)
        } else {
            (last.task.glb_url().map(str::to_owned), last)
        };
        let glb_url = glb_url.ok_or("Meshy returned no GLB")?;
        let glb = self
            .api
            .download(&glb_url, MAX_DOWNLOAD_BYTES)
            .await
            .map_err(dl)?;
        if glb.len() < 20 || &glb[..4] != b"glTF" {
            return Err("Meshy's GLB is not a binary glTF".into());
        }
        let mut animations: Vec<String> = s.clips.iter().map(|c| c.name.clone()).collect();
        if anim.is_none() && rig.is_some() {
            animations.push("Walking".into());
        }
        let steps: Vec<Value> = done
            .iter()
            .map(|d| json!({"step": d.step.as_str(), "taskId": d.task.id, "taskKind": d.task.kind.as_str(), "credits": d.credits}))
            .collect();
        let formats: Vec<String> = s.formats.clone();
        let meta = json!({
            "title": s.title,
            "prompt": match &s.source { Source::Text { prompt } => Some(prompt.clone()), Source::Input(i) => i.prompt.clone(), Source::Images { .. } => None },
            "source": "meshy",
            "jobId": job_id,
            "credits": consumed,
            "animations": animations,
            "clips": s.clips.iter().map(|c| json!({"actionId": c.id, "name": c.name, "inPlace": c.in_place})).collect::<Vec<_>>(),
            "formats": formats,
            "parent": input.as_ref().and_then(|i| i.model_id.clone()),
            "meshy": {
                "taskId": gen.map(|g| g.task.id.clone()).or_else(|| input.as_ref().map(|i| i.task_id.clone())),
                "taskKind": gen.map(|g| g.task.kind.as_str()).or_else(|| input.as_ref().map(|i| i.kind.as_str())),
                "rigTaskId": rig.map(|r| r.task.id.clone()).or_else(|| input.as_ref().and_then(|i| i.rig_task_id.clone())),
                "finalTaskId": model_task.task.id,
                "aiModel": s.ai_model,
                "steps": steps,
            },
        });
        let model = self
            .gallery
            .create(&meta)
            .await
            .map_err(|e| format!("gallery: {e}"))?;
        let model_id = model["id"]
            .as_str()
            .filter(|i| gallery::valid_model_id(i))
            .ok_or("gallery returned no model id")?
            .to_owned();
        self.gallery
            .put_file(&model_id, "model.glb", glb.clone())
            .await
            .map_err(|e| format!("gallery: {e}"))?;
        // Thumbnail from the generation task (rig/animation tasks have none).
        if let Some(url) = gen.and_then(|g| g.task.thumbnail_url().map(str::to_owned)) {
            match self.api.download(&url, 10 * 1024 * 1024).await {
                Ok(bytes) => {
                    if let Some(ext) = image_mime(&bytes)
                        .map(|m| m.trim_start_matches("image/").replace("jpeg", "jpg"))
                    {
                        if let Err(e) = self
                            .gallery
                            .put_file(&model_id, &format!("thumb.{ext}"), bytes)
                            .await
                        {
                            tracing::warn!(job_id, error = %e, "meshy thumbnail upload failed");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(job_id, code = e.code(), "meshy thumbnail download failed")
                }
            }
        }
        // Extra formats Meshy made at generation (free).
        for f in s.formats.iter().filter(|f| *f != "glb") {
            let url = model_task
                .task
                .format_url(f)
                .or_else(|| gen.and_then(|g| g.task.format_url(f)));
            if let Some(url) = url {
                match self.api.download(url, MAX_DOWNLOAD_BYTES).await {
                    Ok(bytes) => {
                        if let Err(e) = self
                            .gallery
                            .put_file(&model_id, &format!("model.{f}"), bytes)
                            .await
                        {
                            tracing::warn!(job_id, format = %f, error = %e, "meshy format upload failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(job_id, format = %f, code = e.code(), "meshy format download failed")
                    }
                }
            }
        }
        // Turntable video for Telegram/Discord (CPU).
        let mut video: Option<Vec<u8>> = None;
        if self.cfg.preview.enabled {
            let glb_path = work.join("model.glb");
            let mp4_path = work.join("preview.mp4");
            if std::fs::write(&glb_path, &glb).is_ok() {
                match preview::render(&self.cfg.preview, &glb_path, &mp4_path, &s.title).await {
                    Ok(info) => {
                        tracing::info!(
                            job_id,
                            bytes = info.bytes,
                            render_s = info.render_s,
                            "meshy preview rendered"
                        );
                        if let Ok(bytes) = std::fs::read(&info.path) {
                            if self
                                .gallery
                                .put_file(&model_id, "preview.mp4", bytes.clone())
                                .await
                                .is_ok()
                            {
                                video = Some(bytes);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(job_id, error = %e, "meshy preview failed"),
                }
            }
        }
        let model = self.gallery.get(&model_id).await.unwrap_or(model);
        let names = if animations.is_empty() {
            String::new()
        } else {
            format!(" — {}", animations.join(", "))
        };
        let text = if origin.it() {
            format!(
                "Il tuo modello 3D «{}» è pronto{} ({} crediti).",
                s.title, names, consumed
            )
        } else {
            format!(
                "Your 3D model \"{}\" is ready{} ({} credits).",
                s.title, names, consumed
            )
        };
        self.deliver(job_id, plan, &model, consumed, video, text, origin)
            .await;
        let _ = std::fs::remove_dir_all(&work);
        Ok(())
    }

    /// The `model3d` block for a gallery model.
    pub fn block(model: &Value) -> Option<Value> {
        let files: Vec<&str> = model["files"]
            .as_array()
            .map(|f| f.iter().filter_map(|x| x["name"].as_str()).collect())
            .unwrap_or_default();
        model3d::normalize(&json!({
            "type": model3d::BLOCK_TYPE,
            "id": model["id"],
            "title": model["title"],
            "prompt": model["prompt"],
            "animations": model["animations"],
            "credits": model["credits"],
            "formats": model["formats"],
            "poster": files.iter().any(|f| f.starts_with("thumb.")),
            "video": files.contains(&"preview.mp4"),
            "rigged": model["meshy"]["rigTaskId"].is_string(),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn deliver(
        &self,
        job_id: &str,
        plan: &Plan,
        model: &Value,
        consumed: u32,
        video: Option<Vec<u8>>,
        text: String,
        origin: &Origin,
    ) {
        let block = Self::block(model);
        let model_id = model["id"].as_str().unwrap_or("").to_owned();
        let item = self.timeline.record(NewItem {
            channel: timeline_channel(&origin.channel),
            role: "lucina".into(),
            // The sentence names the model, its clips and the credits; the block is the widget.
            text: text.clone(),
            reply_to: origin.reply_to.clone(),
            meta: Some(Meta {
                job_id: Some(job_id.to_owned()),
                kind: Some("meshy".into()),
                format: block.as_ref().map(|_| model3d::FORMAT.to_owned()),
                bubbles: block.as_ref().map(|_| vec![text.clone()]),
                lang: Some(if origin.it() { "it" } else { "en" }.into()),
                blocks: block.clone().map(|b| vec![b]),
                ..Meta::default()
            }),
            ..Default::default()
        });
        let item_id = item.as_ref().map(|i| i.id.clone());
        tracing::info!(job_id, model_id = %model_id, credits = consumed, "meshy job done");
        self.notify(&text, job_id, item_id.as_deref()).await;
        if self.telegram.configured() {
            let r = match &video {
                Some(v) => self.telegram.send_preview(&model_id, &text, v).await,
                None => self.telegram.send_text(&text).await,
            };
            if let Err(e) = r {
                tracing::warn!(job_id, error = %e, "meshy telegram delivery failed");
            }
        }
        // Done last: whoever polls the job sees everything delivered.
        let _ = self.jobs.update(job_id, |j| {
            j.status = "done".into();
            j.result = Some(text.clone());
            j.timeline_item_id = item.map(|i| i.id);
            j.finished_at = Some(lucina_store::timeutil::now_iso());
            j.extra.insert("modelId".into(), json!(model_id));
            j.extra.insert(
                "credits".into(),
                json!({"estimate": plan.total, "consumed": consumed}),
            );
            j.extra.remove("progress");
        });
    }

    async fn fail(
        &self,
        job_id: &str,
        plan: &Plan,
        done: &[StepDone],
        consumed: u32,
        f: Failure,
        origin: &Origin,
    ) {
        let step = f.step.map(|s| s.as_str()).unwrap_or("delivery");
        let kept = f
            .task_id
            .as_deref()
            .map(|t| format!(" Meshy task {t}."))
            .unwrap_or_default();
        let text = if origin.it() {
            format!("Meshy: «{}» non è riuscito al passo {step}: {}{kept} Crediti spesi: {consumed} di {}.", plan.spec.title, f.error, plan.total)
        } else {
            format!("Meshy: \"{}\" failed at the {step} step: {}{kept} Credits spent: {consumed} of {}.", plan.spec.title, f.error, plan.total)
        };
        tracing::warn!(
            job_id,
            step,
            code = f.code,
            credits = consumed,
            "meshy job failed"
        );
        let item = self.timeline.record(NewItem {
            channel: timeline_channel(&origin.channel),
            role: "lucina".into(),
            text: text.clone(),
            reply_to: origin.reply_to.clone(),
            meta: Some(Meta {
                job_id: Some(job_id.to_owned()),
                kind: Some("meshy".into()),
                ..Meta::default()
            }),
            ..Default::default()
        });
        let item_id = item.as_ref().map(|i| i.id.clone());
        let steps: Vec<Value> = done
            .iter()
            .map(|d| json!({"step": d.step.as_str(), "taskId": d.task.id, "credits": d.credits}))
            .collect();
        self.notify(&text, job_id, item_id.as_deref()).await;
        if self.telegram.configured() {
            let _ = self.telegram.send_text(&text).await;
        }
        let _ = self.jobs.update(job_id, |j| {
            j.status = "error".into();
            j.error = Some(f.error.chars().take(500).collect());
            j.timeline_item_id = item.map(|i| i.id);
            j.finished_at = Some(lucina_store::timeutil::now_iso());
            j.extra.insert("errorCode".into(), json!(f.code));
            j.extra.insert("failedStep".into(), json!(step));
            j.extra.insert("doneSteps".into(), json!(steps));
            j.extra.insert(
                "credits".into(),
                json!({"estimate": plan.total, "consumed": consumed}),
            );
        });
    }

    /// `POST /v1/notify` on secretary-server (the owner's own devices).
    /// `timeline_id` makes it one notification with secretary-server's own
    /// timeline ingester (both are keyed `tl:<id>`).
    async fn notify(&self, body: &str, job_id: &str, timeline_id: Option<&str>) {
        if !self.cfg.notify {
            return;
        }
        let (Some(url), Some(token)) = (
            self.cfg
                .secretary_url
                .as_deref()
                .map(|u| u.trim_end_matches('/')),
            self.cfg
                .secretary_token_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.trim().to_owned())
                .filter(|t| t.starts_with("svc_")),
        ) else {
            return;
        };
        let body: String = body.chars().take(500).collect();
        let r = self
            .http
            .post(format!("{url}/v1/notify"))
            .bearer_auth(token)
            .json(&{
                let mut b = json!({"category": "lucina_message", "title": "Lucina · 3D", "body": body, "route": "/", "dedupKey": format!("lucina:meshy:{job_id}")});
                if let Some(t) = timeline_id {
                    b["timelineId"] = json!(t);
                }
                b
            })
            .send()
            .await;
        match r {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => tracing::warn!(
                job_id,
                status = resp.status().as_u16(),
                "meshy notify refused"
            ),
            Err(_) => tracing::warn!(job_id, "meshy notify: secretary-server unreachable"),
        }
    }
}

fn timeline_channel(c: &str) -> String {
    match c {
        "web" | "ios" | "telegram" | "discord" => c.to_owned(),
        _ => "web".to_owned(),
    }
}

/// `image/png`, `image/jpeg` or `image/webp` from the magic bytes.
pub fn image_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
