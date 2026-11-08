//! Explicitly uploaded call recordings -> transcript. No SIM audio capture or shared memory.
use crate::{
    error::{ApiError, JsonBody},
    AppState,
};
use axum::http::StatusCode;
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::LazyLock, time::Duration};
static ADMISSION: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(1));
const MAX_AUDIO: usize = 8 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Recording {
    audio_base64: String,
    mime_type: String,
    consent: bool,
}
fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "transcription_unavailable",
        "Recording transcription is not configured.",
    )
}
async fn capabilities() -> Json<Value> {
    Json(
        json!({"simAudioCapture":false,"recordingRequired":true,"configured":std::env::var_os("SECRETARY_GEMINI_KEY_FILE").is_some(),"maxAudioBytes":MAX_AUDIO,"provider":"Gemini"}),
    )
}
fn allowed_mime(s: &str) -> bool {
    matches!(
        s,
        "audio/wav"
            | "audio/mp3"
            | "audio/mpeg"
            | "audio/aiff"
            | "audio/aac"
            | "audio/ogg"
            | "audio/flac"
            | "audio/m4a"
            | "audio/opus"
            | "audio/webm"
    )
}
async fn transcribe(JsonBody(v): JsonBody<Recording>) -> Result<Json<Value>, ApiError> {
    if !v.consent {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "consent_required",
            "Choose a recording and confirm sending it to Gemini for transcription.",
        ));
    }
    if !allowed_mime(&v.mime_type) || v.audio_base64.len() > MAX_AUDIO.div_ceil(3) * 4 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_audio",
            "Use a supported audio recording up to 8 MiB.",
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&v.audio_base64)
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_audio",
                "Invalid recording.",
            )
        })?;
    if bytes.is_empty() || bytes.len() > MAX_AUDIO {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_audio",
            "Use a supported audio recording up to 8 MiB.",
        ));
    }
    let _permit = ADMISSION.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "transcription_busy",
            "Another recording is being transcribed. Try again shortly.",
        )
    })?;
    let t = transcribe_inline(&v.mime_type, &v.audio_base64, Options::DIARIZED, None).await?;
    if t.text.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "empty_transcript",
            "No speech transcript was returned.",
        ));
    }
    Ok(Json(
        json!({"transcript":t.text,"partial":t.partial,"model":t.model,"source":"uploaded_recording"}),
    ))
}

/// Whether the Gemini transcriber is set up at all (a key file is named). A cheap
/// check, so callers do not prepare a request that cannot be sent.
pub(crate) fn gemini_configured() -> bool {
    std::env::var_os("SECRETARY_GEMINI_KEY_FILE").is_some()
}

/// How [`transcribe_inline`] asks the provider.
#[derive(Clone, Copy)]
pub(crate) struct Options {
    /// Speaker labels in the transcript.
    pub diarization: bool,
    pub timeout: Duration,
}

impl Options {
    /// Recordings: speaker labels, up to 100 s.
    pub const DIARIZED: Self = Self {
        diarization: true,
        timeout: Duration::from_secs(100),
    };
}

/// What the provider heard. `text` is empty when it heard no speech.
pub(crate) struct Transcript {
    pub text: String,
    /// The provider stopped before the end of the audio.
    pub partial: bool,
    pub model: String,
}

/// Provider origin; `SECRETARY_TRANSCRIPTION_URL` points tests at a mock.
const DEFAULT_PROVIDER_URL: &str = "https://generativelanguage.googleapis.com";

/// Transcribe one inline audio clip (`data_base64` of `mime_type`) with
/// Gemini: `SECRETARY_GEMINI_KEY_FILE` holds the key,
/// `SECRETARY_TRANSCRIPTION_MODEL` names the model. Shared by
/// `/v1/calls/transcribe` and `/v1/voice`; 503 `transcription_unavailable`
/// when it is not configured. `vocabulary` (Voice Learn, [`crate::voice_learn`])
/// is a "Vocabulary: …" line sent as a text part after the audio.
pub(crate) async fn transcribe_inline(
    mime_type: &str,
    data_base64: &str,
    opts: Options,
    vocabulary: Option<&str>,
) -> Result<Transcript, ApiError> {
    let path = std::env::var_os("SECRETARY_GEMINI_KEY_FILE").ok_or_else(unavailable)?;
    let key = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| unavailable())?;
    if key.trim().is_empty() {
        return Err(unavailable());
    }
    let model =
        std::env::var("SECRETARY_TRANSCRIPTION_MODEL").unwrap_or("gemini-3.5-transcribe".into());
    if !model.starts_with("gemini-")
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err(unavailable());
    }
    let base = std::env::var("SECRETARY_TRANSCRIPTION_URL").unwrap_or(DEFAULT_PROVIDER_URL.into());
    let client = reqwest::Client::builder()
        .timeout(opts.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable())?;
    let mut parts = vec![json!({"inlineData":{"mimeType":mime_type,"data":data_base64}})];
    if let Some(line) = vocabulary.filter(|l| !l.trim().is_empty()) {
        parts.push(json!({"text": line}));
    }
    let mut response=client.post(format!("{}/v1beta/models/{model}:generateContent", base.trim_end_matches('/')))
  .header("x-goog-api-key",key.trim()).json(&json!({
   "contents":[{"role":"user","parts":parts}],
   "generationConfig":{"audioTranscriptionConfig":{"diarization":opts.diarization},"maxOutputTokens":8192}
  })).send().await.map_err(|_|ApiError::new(StatusCode::BAD_GATEWAY,"transcription_failed","The transcription provider could not be reached."))?;
    if !response.status().is_success() {
        return Err(ApiError::new(if response.status().as_u16()==429{StatusCode::TOO_MANY_REQUESTS}else{StatusCode::BAD_GATEWAY},"transcription_provider_refused","The configured transcription model refused the request or reached its quota. No transcript was produced."));
    }
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "transcription_failed",
            "The transcript response was interrupted.",
        )
    })? {
        if raw.len() + chunk.len() > 512 * 1024 {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "transcript_limit",
                "The transcript response exceeded its limit.",
            ));
        }
        raw.extend_from_slice(&chunk);
    }
    let result: Value = serde_json::from_slice(&raw).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "invalid_transcript",
            "The provider returned an invalid transcript.",
        )
    })?;
    let text = result["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let partial = result["candidates"][0]["finishReason"]
        .as_str()
        .is_some_and(|s| s != "STOP");
    Ok(Transcript {
        text,
        partial,
        model,
    })
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calls/capabilities", get(capabilities))
        .route(
            "/v1/calls/transcribe",
            post(transcribe).layer(DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_audio_formats() {
        assert!(allowed_mime("audio/m4a"));
        assert!(!allowed_mime("application/pdf"));
        assert!(!allowed_mime("text/html"));
    }
}
