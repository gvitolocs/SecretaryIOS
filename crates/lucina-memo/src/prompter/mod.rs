//! Lucina Prompter, the listening half (docs/prompter.md).
//!
//! The owner reads talking points from the Secretary app while he records; the
//! app streams his microphone here and crosses points out as he makes them.
//!
//! ```text
//! web / Secretary Code ──wss /v1/memos/prompter/live──► p1 Caddy (@memos gate) ──► lucina-memo
//!   16 kHz PCM, open points                                 whisper-server (small, gpu-lease)
//!   ◄── transcript, verdicts, latency                       ──► lucina-rs /api/secretary/prompter/judge (Jev)
//! ```
//!
//! - Audio stays on nezopt and is never written to disk; only transcript text
//!   leaves this service, to lucina-rs's judge (Jev on Lucina's own key).
//! - The local matcher answers first (obvious hits in microseconds) and is the
//!   whole judge whenever lucina-rs or Jev is out.
//! - `POST /v1/memos/prompter/draft` drafts sections and points from a topic or a
//!   document; `POST /v1/memos/prompter/summary` summarises a finished take.
pub mod engine;
pub mod replay;
pub mod segmenter;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use lucina_llm::{ChatRequest, Message};
use lucina_prompter::{JudgeRequest, JudgeResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, R};
use crate::App;

/// Settings (environment, see [`engine::EngineConfig`] for the speech-to-text ones).
#[derive(Debug, Clone)]
pub struct PrompterConfig {
    pub engine: engine::EngineConfig,
    /// lucina-rs's judge.
    pub judge_url: String,
    /// The Secretary API bearer for the judge (`SECRETARY_API_TOKEN`).
    pub judge_token: Option<String>,
}

impl PrompterConfig {
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String> + Copy,
        whisper: &crate::transcribe::WhisperConfig,
    ) -> Self {
        Self {
            engine: engine::EngineConfig::from_lookup(get, whisper),
            judge_url: get("LUCINA_PROMPTER_JUDGE_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8790/api/secretary/prompter/judge".into()),
            judge_token: get("SECRETARY_API_TOKEN_FILE")
                .and_then(|p| std::fs::read_to_string(p).ok())
                .or_else(|| get("SECRETARY_API_TOKEN"))
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty()),
        }
    }
}

/// The live part of the service: the engine and the judge client.
pub struct Live {
    pub engine: Arc<engine::Engine>,
    judge: Arc<dyn Judge>,
}

impl Live {
    pub fn new(cfg: &PrompterConfig) -> Self {
        Self {
            engine: engine::Engine::new(cfg.engine.clone()),
            judge: Arc::new(RemoteJudge {
                url: cfg.judge_url.clone(),
                token: cfg.judge_token.clone(),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .expect("reqwest client"),
            }),
        }
    }

    /// A fake judge (tests).
    pub fn with_judge(mut self, j: Arc<dyn Judge>) -> Self {
        self.judge = j;
        self
    }
}

/// Where the Jev verdicts come from: lucina-rs, or a fake in tests.
#[async_trait::async_trait]
pub trait Judge: Send + Sync {
    async fn judge(&self, req: &JudgeRequest) -> Result<JudgeResponse, String>;
}

struct RemoteJudge {
    url: String,
    token: Option<String>,
    http: reqwest::Client,
}

#[async_trait::async_trait]
impl Judge for RemoteJudge {
    async fn judge(&self, req: &JudgeRequest) -> Result<JudgeResponse, String> {
        let token = self
            .token
            .as_deref()
            .ok_or("no SECRETARY_API_TOKEN for lucina-rs")?;
        let r = self
            .http
            .post(&self.url)
            .bearer_auth(token)
            .json(req)
            .send()
            .await
            .map_err(|e| format!("lucina-rs: {}", e.without_url()))?;
        if !r.status().is_success() {
            return Err(format!("lucina-rs answered {}", r.status()));
        }
        let v: Value = r.json().await.map_err(|e| e.without_url().to_string())?;
        serde_json::from_value(v["data"].clone()).map_err(|e| format!("lucina-rs reply: {e}"))
    }
}

pub fn routes(app: Arc<App>) -> Router<Arc<App>> {
    Router::new()
        .route("/v1/memos/prompter/live", get(live))
        .route("/v1/memos/prompter/status", get(status))
        .route("/v1/memos/prompter/draft", post(draft))
        .route("/v1/memos/prompter/summary", post(summary))
        .route_layer(axum::middleware::from_fn_with_state(app, crate::api::auth))
}

async fn live(State(app): State<Arc<App>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| session::run(socket, app))
}

async fn status(State(app): State<Arc<App>>) -> R {
    Ok(Json(json!({"engine": app.prompter.engine.status().await})))
}

const DRAFT_SYSTEM: &str = "You write talking points for a talk or a video the speaker will record. \
Answer with Markdown only: one `## Section` heading per part of the talk, each followed by 2 to 5 \
bullet points (`- point`). Points are short cues of 3 to 12 words that remind the speaker what to \
say, not sentences to read aloud. Use 3 to 8 sections in the order the talk should go. Write in \
the language asked for, or in the language of the topic. No title, no introduction, no closing remarks.";

const SUMMARY_SYSTEM: &str = "You review one recorded take for the speaker, who talked from a list of \
talking points. Using the coverage report and the transcript, say in Markdown: what he covered well, \
what he missed or rushed, whether the order worked, and two or three concrete suggestions for the next \
take. Under 200 words, second person, in English (Danish when the transcript is Danish; never Italian). No heading.";

const MAX_DOC_CHARS: usize = 40_000;

fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftReq {
    #[serde(default)]
    topic: String,
    #[serde(default)]
    document: String,
    #[serde(default)]
    language: String,
}

async fn draft(State(app): State<Arc<App>>, Json(b): Json<DraftReq>) -> R {
    if b.topic.trim().is_empty() && b.document.trim().is_empty() {
        return Err(ApiError::bad("give a topic or a document"));
    }
    let mut user = format!("Topic: {}\n", clip(b.topic.trim(), 2000));
    if !b.language.trim().is_empty() {
        user.push_str(&format!("Language: {}\n", clip(b.language.trim(), 40)));
    }
    if !b.document.trim().is_empty() {
        user.push_str(&format!(
            "\nNotes or document to draw the points from:\n<<<\n{}\n>>>\n",
            clip(&b.document, MAX_DOC_CHARS)
        ));
    }
    let text = complete(&app, "prompter_draft", DRAFT_SYSTEM, user, 1500).await?;
    Ok(Json(json!({"markdown": text})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummaryReq {
    #[serde(default)]
    title: String,
    #[serde(default)]
    script: String,
    #[serde(default)]
    report: String,
    #[serde(default)]
    transcript: String,
}

async fn summary(State(app): State<Arc<App>>, Json(b): Json<SummaryReq>) -> R {
    if b.transcript.trim().is_empty() {
        return Err(ApiError::bad("the take has no transcript"));
    }
    let user = format!(
        "Title: {}\n\nTalking points:\n{}\n\nCoverage report:\n{}\n\nTranscript:\n{}\n",
        clip(&b.title, 200),
        clip(&b.script, 10_000),
        clip(&b.report, 6_000),
        clip(&b.transcript, 30_000)
    );
    let text = complete(&app, "prompter_summary", SUMMARY_SYSTEM, user, 900).await?;
    Ok(Json(json!({"summary": text})))
}

async fn complete(
    app: &App,
    purpose: &'static str,
    system: &str,
    user: String,
    max_tokens: u32,
) -> Result<String, ApiError> {
    let req = ChatRequest {
        messages: vec![Message::system(system), Message::user(user)],
        temperature: Some(0.4),
        max_tokens: Some(max_tokens),
        purpose,
        thinking: Some(false),
        ..Default::default()
    };
    let r = app.llm.complete(req).await.map_err(|e| {
        tracing::warn!(error = %e, purpose, "prompter: Lucina's model failed");
        ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "llm",
            "Lucina could not write this now; try again",
        )
    })?;
    let text = r.message.text().trim().to_owned();
    if text.is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "llm",
            "Lucina's answer was empty; try again",
        ));
    }
    Ok(text)
}
