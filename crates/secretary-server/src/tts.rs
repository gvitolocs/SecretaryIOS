//! `POST /v1/tts {text, voice?, lang?, speed?}` → `audio/mpeg`, proxied to
//! Kokoro's OpenAI-compatible API (`{SECRETARY_TTS_URL}/v1/audio/speech`)
//! and streamed back without buffering.
//!
//! Voice: an explicit `voice` (from the `SECRETARY_TTS_VOICES` whitelist)
//! wins; else `lang` (`it` → `if_sara`, `en` → `af_heart`); else a small
//! Italian stopword heuristic picks between the two.

use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::json;

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody};
use crate::AppState;

pub const DEFAULT_URL: &str = "http://100.107.164.33:8880";
pub const DEFAULT_VOICES: &str = "if_sara,af_heart,af_bella,bf_emma";
pub const ITALIAN_VOICE: &str = "if_sara";
pub const ENGLISH_VOICE: &str = "af_heart";
pub const MAX_CHARS: usize = 1200;
pub const PER_MINUTE: u32 = 30;
pub const TIMEOUT: Duration = Duration::from_secs(30);

const ITALIAN_STOPWORDS: &[&str] = &[
    "che", "non", "per", "sono", "ciao", "il", "la", "è", "un", "una", "di", "ti", "mi", "come",
    "oggi",
];

/// Does `text` look Italian? At least two stopword hits, or one in a text
/// of at most three words.
pub fn looks_italian(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let hits = words
        .iter()
        .filter(|w| ITALIAN_STOPWORDS.contains(w))
        .count();
    hits >= 2 || (hits == 1 && words.len() <= 3)
}

/// The voice for a request, or a `bad_request` message.
pub fn pick_voice(
    voices: &[String],
    voice: Option<&str>,
    lang: Option<&str>,
    text: &str,
) -> Result<String, String> {
    if let Some(v) = voice {
        return if voices.iter().any(|x| x == v) {
            Ok(v.to_owned())
        } else {
            Err(format!("voice: one of {}", voices.join(", ")))
        };
    }
    match lang.map(|l| l.split(['-', '_']).next().unwrap_or("").to_lowercase()) {
        Some(l) if l == "it" => Ok(ITALIAN_VOICE.into()),
        Some(l) if l == "en" => Ok(ENGLISH_VOICE.into()),
        Some(_) => Err("lang: it or en".into()),
        None if looks_italian(text) => Ok(ITALIAN_VOICE.into()),
        None => Ok(ENGLISH_VOICE.into()),
    }
}

#[derive(Deserialize)]
struct TtsRequest {
    text: String,
    voice: Option<String>,
    lang: Option<String>,
    speed: Option<f64>,
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

fn unavailable(status: StatusCode, message: &str) -> ApiError {
    ApiError::new(status, "tts_unavailable", message)
}

/// Upstream client and settings.
pub struct Tts {
    pub url: String,
    pub voices: Vec<String>,
    pub limiter: crate::ratelimit::RateLimiter,
    client: reqwest::Client,
}

impl Tts {
    pub fn new(url: &str, voices: Vec<String>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            url: url.trim_end_matches('/').to_owned(),
            voices,
            limiter: crate::ratelimit::RateLimiter::new(PER_MINUTE),
            client: reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(timeout.min(Duration::from_secs(5)))
                .build()?,
        })
    }
}

async fn speak(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(req): JsonBody<TtsRequest>,
) -> Result<Response, ApiError> {
    let tts = &st.tts;
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > MAX_CHARS {
        return Err(bad(format!("text: 1–{MAX_CHARS} characters")));
    }
    let speed = req.speed.unwrap_or(1.0);
    if !(0.5..=2.0).contains(&speed) {
        return Err(bad("speed: 0.5–2.0"));
    }
    let voice =
        pick_voice(&tts.voices, req.voice.as_deref(), req.lang.as_deref(), text).map_err(bad)?;
    if let Err(wait) = tts.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let upstream = tts
        .client
        .post(format!("{}/v1/audio/speech", tts.url))
        .json(&json!({"model": "kokoro", "input": text, "voice": voice,
                      "response_format": "mp3", "speed": speed}))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                unavailable(StatusCode::GATEWAY_TIMEOUT, "the speech service timed out")
            } else {
                unavailable(StatusCode::BAD_GATEWAY, "the speech service is unreachable")
            }
        })?;
    if !upstream.status().is_success() {
        tracing::warn!(status = upstream.status().as_u16(), "tts upstream error");
        return Err(unavailable(
            StatusCode::BAD_GATEWAY,
            "the speech service failed",
        ));
    }
    let mut resp = Body::from_stream(upstream.bytes_stream()).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    if let Ok(v) = HeaderValue::from_str(&voice) {
        resp.headers_mut().insert("x-tts-voice", v);
    }
    Ok(resp)
}

/// Behind `require_device` (so cookie callers need the CSRF header).
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/tts", post(speak))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voices() -> Vec<String> {
        DEFAULT_VOICES.split(',').map(str::to_owned).collect()
    }

    #[test]
    fn italian_heuristic() {
        assert!(looks_italian("Ciao Giuseppe, come stai oggi?"));
        assert!(looks_italian("Non è vero"));
        assert!(looks_italian("Ciao!"));
        assert!(looks_italian("Ho comprato il latte e la frutta"));
        assert!(!looks_italian("Hello, how are you today?"));
        assert!(!looks_italian("Come here and see the new workout plan"));
        assert!(!looks_italian(""));
    }

    #[test]
    fn voice_selection_order() {
        let v = voices();
        assert_eq!(
            pick_voice(&v, Some("bf_emma"), Some("it"), "ciao").unwrap(),
            "bf_emma"
        );
        assert!(pick_voice(&v, Some("zz_nope"), None, "hi").is_err());
        assert_eq!(
            pick_voice(&v, None, Some("it"), "hello there").unwrap(),
            "if_sara"
        );
        assert_eq!(
            pick_voice(&v, None, Some("en-US"), "ciao come stai").unwrap(),
            "af_heart"
        );
        assert!(pick_voice(&v, None, Some("fr"), "bonjour").is_err());
        assert_eq!(
            pick_voice(&v, None, None, "Ciao, come stai?").unwrap(),
            "if_sara"
        );
        assert_eq!(
            pick_voice(&v, None, None, "Good morning").unwrap(),
            "af_heart"
        );
    }
}
