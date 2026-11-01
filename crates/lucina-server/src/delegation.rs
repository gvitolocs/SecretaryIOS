//! Delegation in the server (`LUCINA_DELEGATE=true`, policy in
//! `LUCINA_DELEGATE_CONFIG`, crate `lucina-delegate`): the sink (timeline
//! cards, notes, app notifications), the helpers both planners use, and the
//! API (`/api/secretary/delegations`, `/api/secretary/delegate` for the
//! proactive engine).

use std::sync::Arc;
use std::time::Duration;

use lucina_delegate::engine::{chat_result, Card, Sink};
use lucina_delegate::policy::Refusal;
use lucina_delegate::types::{clip, guess_lang};
use lucina_delegate::{
    DelegateConfig, Delegator, JobRecord, JobStatus, Kind, Origin, Request, StartOutcome,
};
use lucina_store::timeline::{Meta, NewItem, TimelineStore, CHANNELS};
use reqwest::Method;
use serde_json::{json, Value};

use crate::bridges::Secretary;

fn channel_of(c: &str) -> String {
    if CHANNELS.contains(&c) {
        c.to_owned()
    } else {
        "other".to_owned()
    }
}

/// Timeline, notes (`/v1/notes`) and notifications (`/v1/notify`).
pub struct ServerSink {
    pub timeline: Arc<TimelineStore>,
    pub secretary: Secretary,
}

#[async_trait::async_trait]
impl Sink for ServerSink {
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<String, String> {
        let mut content = content.to_owned();
        lucina_secrets::redactor().redact_string(&mut content);
        let body =
            |t: &str| json!({"title": t, "content": content, "source": "lucina", "tags": tags});
        let mut r = self
            .secretary
            .call(Method::POST, "/v1/notes", Some(&body(title)))
            .await;
        if !r.ok && r.code == "slug_taken" {
            let t = format!("{title} · {}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
            r = self
                .secretary
                .call(Method::POST, "/v1/notes", Some(&body(&t)))
                .await;
        }
        if r.ok {
            r.data["slug"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "no slug".to_owned())
        } else {
            Err(format!("notes: {}", r.code))
        }
    }

    async fn notify(&self, title: &str, body: &str) {
        let mut payload = json!({
            "category": "lucina_message",
            "title": clip(title, 120),
            "body": clip(body, 480),
            "route": "/",
        });
        lucina_secrets::redactor().redact_json(&mut payload);
        crate::bridges::notify(&self.secretary, &payload).await;
    }

    fn card(&self, card: &Card) {
        let j = &card.job;
        let meta = Meta {
            job_id: Some(j.id.clone()),
            tool: Some(j.agent.as_str().to_owned()),
            ok: Some(j.status != JobStatus::Error),
            slug: j.note_slug.clone(),
            ..Meta::kind("delegation")
        };
        let meta = match &card.rich {
            Some(m) => meta.merge(Meta::rich(m)),
            None => meta,
        };
        let mut text = card.plain.clone();
        lucina_secrets::redactor().redact_string(&mut text);
        self.timeline.record(NewItem {
            channel: channel_of(&j.channel),
            role: "lucina".into(),
            text,
            meta: Some(meta),
            ..Default::default()
        });
    }

    fn message(&self, channel: &str, text: &str) {
        self.timeline.record(NewItem {
            channel: channel_of(channel),
            role: "lucina".into(),
            text: text.to_owned(),
            meta: Some(Meta::kind("delegation")),
            ..Default::default()
        });
    }
}

/// The delegator from the config (`None` when `LUCINA_DELEGATE` is off or
/// the policy file is invalid).
pub fn build(
    cfg: &crate::Config,
    model: Arc<dyn lucina_llm::ChatModel>,
    http: reqwest::Client,
    secrets: Option<Arc<lucina_secrets::SecretStore>>,
    timeline: Arc<TimelineStore>,
) -> Option<Arc<Delegator>> {
    if !cfg.delegate_enabled {
        return None;
    }
    let dc = match DelegateConfig::load(cfg.delegate_config.as_deref(), &cfg.data_dir) {
        Ok(c) if c.enabled => c,
        Ok(_) => {
            tracing::info!("delegation: disabled in the policy file");
            return None;
        }
        Err(e) => {
            tracing::error!(error = %e, "delegation: invalid policy file; delegation stays off");
            return None;
        }
    };
    // Long agent calls: no client-wide timeout (each call sets its own).
    let agent_http = reqwest::Client::builder()
        .user_agent(concat!(
            "lucina-rs/",
            env!("CARGO_PKG_VERSION"),
            " (delegation)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or(http.clone());
    let agents = lucina_delegate::agents::real(&dc, agent_http, secrets);
    let sink = Arc::new(ServerSink {
        timeline,
        secretary: Secretary {
            http,
            url: cfg.secretary_url.clone(),
            token_file: cfg.secretary_service_token_file.clone(),
        },
    });
    tracing::info!(
        sandbox = %dc.sandbox_root.display(),
        total_cap = dc.budget.total_calls_per_day,
        "delegation enabled (self-judged, hard limits on)"
    );
    Some(Arc::new(Delegator::new(
        dc,
        agents,
        Some(model),
        sink,
        cfg.time_zone,
    )))
}

/// What a chat turn's delegation came to.
pub struct Ran {
    pub lang: &'static str,
    pub job: Option<JobRecord>,
    pub refusal: Option<Refusal>,
    /// The job finished within the turn's wait.
    pub finished: bool,
}

/// Start it and wait a little for a quick answer.
pub async fn run(d: &Arc<Delegator>, req: Request) -> Ran {
    let lang = req
        .lang
        .as_deref()
        .map(|l| match l {
            "it" => "it",
            "da" => "da",
            _ => "en",
        })
        .unwrap_or_else(|| guess_lang(&req.task));
    match d.start(req).await {
        StartOutcome::Refused(r) => Ran {
            lang,
            job: None,
            refusal: Some(r),
            finished: true,
        },
        StartOutcome::Skipped(j) => Ran {
            lang,
            job: Some(j),
            refusal: None,
            finished: true,
        },
        StartOutcome::Started(mut s) => match s.wait(d.inline_wait()).await {
            Some(j) => Ran {
                lang,
                job: Some(j),
                refusal: None,
                finished: true,
            },
            None => Ran {
                lang,
                job: Some(s.job.clone()),
                refusal: None,
                finished: false,
            },
        },
    }
}

/// The answer a finished job gives the reply writer.
pub fn answer_text(job: &JobRecord) -> String {
    // With a note the summary is enough; without one, the whole answer.
    let mut s = match (&job.note_slug, &job.summary) {
        (Some(_), Some(summary)) => summary.clone(),
        _ => job
            .result
            .clone()
            .map(|r| clip(&r, 6000))
            .unwrap_or_default(),
    };
    if let Some(slug) = &job.note_slug {
        s.push_str(&format!(
            "\n\n(The full answer is saved in the Secretary app under Notes: {slug}.)"
        ));
    }
    s
}

/// The reply without a model call (running, skipped, refused, failed, and
/// the fallback for done).
pub fn plain_reply(r: &Ran) -> String {
    let it = r.lang == "it";
    if let Some(x) = &r.refusal {
        return x.message(r.lang);
    }
    let Some(j) = &r.job else {
        return String::new();
    };
    let a = j.agent.label();
    let why = j
        .judgement
        .as_ref()
        .map(|g| g.why.clone())
        .unwrap_or_default();
    match (j.status, r.finished) {
        (JobStatus::Skipped, _) => {
            if it {
                format!("Ci ho pensato e ho deciso di non chiederlo a {a}: {why}")
            } else {
                format!("I thought about it and decided not to ask {a}: {why}")
            }
        }
        (JobStatus::Done, true) => {
            let answer = answer_text(j);
            if it {
                format!("{a} dice: {answer}")
            } else {
                format!("{a} says: {answer}")
            }
        }
        (JobStatus::Error, true) => {
            let e = j.error.clone().unwrap_or_default();
            if it {
                format!("{a} non è riuscito a rispondere ({e}).")
            } else {
                format!("{a} could not answer ({e}).")
            }
        }
        _ => {
            if it {
                format!("Ho chiesto a {a}: ti scrivo qui appena risponde.")
            } else {
                format!("I asked {a}: I will post the answer here as soon as it replies.")
            }
        }
    }
}

/// `[delegation grok research: done 12s]` for memory and the rich composer.
pub fn action_result(r: &Ran) -> String {
    match (&r.refusal, &r.job) {
        (Some(x), _) => format!("[delegation refused: {}]", x.code()),
        (None, Some(j)) => format!(
            "[delegation {} {}: {} {}s]",
            j.agent.as_str(),
            j.kind.as_str(),
            if r.finished {
                j.status.as_str()
            } else {
                "running"
            },
            j.duration_ms / 1000
        ),
        _ => "[delegation]".into(),
    }
}

/// The timeline meta of the turn's reply.
pub fn meta(r: &Ran) -> Option<Meta> {
    let j = r.job.as_ref()?;
    Some(Meta {
        job_id: Some(j.id.clone()),
        tool: Some(j.agent.as_str().to_owned()),
        ..Meta::kind("delegation")
    })
}

// ---------------------------------------------------------------- API

/// `GET /api/secretary/delegations`.
pub async fn api_status(d: Option<&Arc<Delegator>>) -> Value {
    match d {
        Some(d) => d.status().await,
        None => json!({"enabled": false}),
    }
}

/// `POST /api/secretary/delegate`: the proactive engine's door. Read-only
/// research only, same budget; `dryRun` plans without running anything.
pub async fn api_delegate(
    d: &Arc<Delegator>,
    body: &Value,
) -> Result<Value, (u16, &'static str, String)> {
    if body["origin"].as_str() != Some("proactive") {
        return Err((
            403,
            "forbidden",
            "only the proactive engine uses this endpoint".into(),
        ));
    }
    let task = body["task"].as_str().unwrap_or("").trim().to_owned();
    if task.is_empty() || task.chars().count() > 4000 {
        return Err((400, "bad_request", "task: 1 to 4000 characters".into()));
    }
    let kind = body["kind"]
        .as_str()
        .and_then(Kind::parse)
        .unwrap_or(Kind::Research);
    let req = Request {
        to: None,
        kind,
        task,
        workspace: None,
        origin: Origin::Proactive,
        depth: 0,
        caller: Some("lucina-proactive".into()),
        session_id: "proactive".into(),
        channel: "other".into(),
        lang: body["lang"].as_str().map(str::to_owned),
        resume_job: None,
    };
    if body["dryRun"].as_bool().unwrap_or(true) {
        return Ok(match d.plan(&req).await {
            Ok((agent, judgement)) => {
                json!({"dryRun": true, "status": "planned", "agent": agent.label(), "judgement": judgement})
            }
            Err(r) => {
                json!({"dryRun": true, "status": "refused", "code": r.code(), "reason": r.message("en")})
            }
        });
    }
    let r = run(d, req).await;
    Ok(match (&r.refusal, &r.job) {
        (Some(x), _) => json!({"status": "refused", "code": x.code(), "reason": x.message("en")}),
        (None, Some(j)) => {
            let mut v = chat_result(j);
            if !r.finished {
                v["status"] = json!("running");
            }
            v
        }
        _ => json!({"status": "error"}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_delegate::types::{AgentId, Decision, Judgement, Risk};

    fn job(status: JobStatus) -> JobRecord {
        JobRecord {
            id: "1a2b3c4d-x".into(),
            agent: AgentId::Grok,
            backend: "mock".into(),
            kind: Kind::Research,
            origin: Origin::Owner,
            status,
            task: "t".into(),
            lang: "it".into(),
            session_id: "s".into(),
            channel: "web".into(),
            judgement: Some(Judgement {
                goal: String::new(),
                risk: Risk::Low,
                reversible: true,
                why: "non serve.".into(),
                decision: Decision::Skip,
                revised_task: None,
                source: "model".into(),
            }),
            created_at: String::new(),
            finished_at: None,
            duration_ms: 12_000,
            result: Some("Rust 1.95 è uscito.".into()),
            summary: None,
            note_slug: Some("grok-rust".into()),
            error: Some("timeout".into()),
            session: None,
            turns: 1,
            sandbox: None,
            branch: None,
            repo: None,
            undo: None,
        }
    }

    #[test]
    fn replies() {
        let r = |s, finished| Ran {
            lang: "it",
            job: Some(job(s)),
            refusal: None,
            finished,
        };
        assert!(
            plain_reply(&r(JobStatus::Done, true)).starts_with("Grok dice: Rust 1.95 è uscito.")
        );
        assert!(plain_reply(&r(JobStatus::Done, true)).contains("Notes: grok-rust"));
        assert_eq!(
            plain_reply(&r(JobStatus::Running, false)),
            "Ho chiesto a Grok: ti scrivo qui appena risponde."
        );
        assert!(
            plain_reply(&r(JobStatus::Skipped, true)).contains("non chiederlo a Grok: non serve.")
        );
        assert!(plain_reply(&r(JobStatus::Error, true)).contains("(timeout)"));
        assert_eq!(
            action_result(&r(JobStatus::Running, false)),
            "[delegation grok research: running 12s]"
        );
        let refused = Ran {
            lang: "en",
            job: None,
            refusal: Some(Refusal::McpOrigin),
            finished: true,
        };
        assert!(plain_reply(&refused).contains("Lucina MCP"));
        assert_eq!(action_result(&refused), "[delegation refused: mcp_origin]");
        assert_eq!(channel_of("proactive"), "other");
    }
}
