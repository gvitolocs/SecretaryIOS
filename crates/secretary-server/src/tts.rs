//! `POST /v1/tts {text, voice?, lang?, speed?}` → `audio/mpeg`, proxied to
//! Kokoro's OpenAI-compatible API (`{SECRETARY_TTS_URL}/v1/audio/speech`)
//! and streamed back without buffering.
//!
//! - Voice: an explicit `voice` (from the `SECRETARY_TTS_VOICES` whitelist)
//!   wins; else `lang` (`it` → `if_sara`, `en` → `af_heart`); else a language
//!   score (Italian stopwords + accented letters vs English stopwords; a tie
//!   or no signal is Italian, Lucina mostly speaks Italian). The voice is
//!   always sent: Kokoro would otherwise pick its own default.
//! - Texts over [`CHUNK_CHARS`] are split on sentence boundaries and
//!   synthesized one chunk after the other into the same MP3 stream (MP3
//!   frames concatenate), so playback starts after the first sentence.
//! - [`Tts::synthesize`] is the same pipeline for `POST /v1/voice`
//!   ([`crate::voice`]), which asks Kokoro for raw PCM ([`Format::Pcm`]:
//!   signed 16-bit little-endian mono at [`PCM_RATE`] Hz, checked against
//!   Kokoro-FastAPI v0.9.0) instead of MP3; raw samples concatenate too.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use futures_util::stream::{self, BoxStream, StreamExt};
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
/// Longer texts are synthesized sentence by sentence in chunks of at most this.
pub const CHUNK_CHARS: usize = 300;
pub const PER_MINUTE: u32 = 30;
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Sample rate of Kokoro's `pcm` output (mono, signed 16-bit little-endian).
pub const PCM_RATE: u32 = 24_000;

const ITALIAN_STOPWORDS: &[&str] = &[
    "che", "non", "per", "sono", "ciao", "il", "la", "è", "un", "una", "di", "ti", "mi", "come",
    "oggi", "e", "del", "della", "con", "sei", "ho", "hai", "grazie", "anche", "questo", "lo",
    "gli", "le", "ma", "se", "cosa", "allora", "bene",
];
const ENGLISH_STOPWORDS: &[&str] = &[
    "the", "and", "you", "is", "are", "to", "of", "what", "how", "this", "that", "with", "have",
    "i", "my", "your", "be", "it's", "i'm", "today", "hello", "hi", "good", "morning", "thanks",
    "it", "in", "on", "for", "do", "was", "will", "not", "can",
];

/// Italian unless English clearly scores higher. Score: Italian stopwords +
/// accented letters vs English stopwords; a tie or no signal is Italian.
pub fn looks_italian(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let accents = lower.chars().filter(|c| "àèéìíòóùú".contains(*c)).count();
    let it = words
        .iter()
        .filter(|w| ITALIAN_STOPWORDS.contains(w))
        .count()
        + accents;
    let en = words
        .iter()
        .filter(|w| ENGLISH_STOPWORDS.contains(w))
        .count();
    it >= en
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

/// Split `text` into chunks of at most `max` characters, preferring sentence
/// ends (`.`, `!`, `?`, `…`, newline), then commas/semicolons, then spaces.
pub fn chunks(text: &str, max: usize) -> Vec<String> {
    fn pieces(text: &str, ends: &[char]) -> Vec<String> {
        let mut out = vec![];
        let mut cur = String::new();
        for c in text.chars() {
            cur.push(c);
            if ends.contains(&c) {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }
    fn split(text: &str, max: usize, levels: &[&[char]]) -> Vec<String> {
        if text.chars().count() <= max {
            return vec![text.to_owned()];
        }
        let Some((ends, rest)) = levels.split_first() else {
            // no boundary at all: hard cut
            let chars: Vec<char> = text.chars().collect();
            return chars.chunks(max).map(|c| c.iter().collect()).collect();
        };
        let mut out: Vec<String> = vec![];
        let mut cur = String::new();
        for piece in pieces(text, ends) {
            for part in split(&piece, max, rest) {
                if cur.chars().count() + part.chars().count() > max && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push_str(&part);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }
    split(
        text,
        max,
        &[&['.', '!', '?', '…', '\n'], &[',', ';', ':'], &[' ']],
    )
    .into_iter()
    .map(|c| c.trim().to_owned())
    .filter(|c| !c.is_empty())
    .collect()
}

#[derive(Deserialize)]
struct TtsRequest {
    text: String,
    voice: Option<String>,
    lang: Option<String>,
    speed: Option<f64>,
}

/// What Kokoro is asked to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Mp3,
    /// Raw signed 16-bit little-endian mono samples at [`PCM_RATE`] Hz.
    Pcm,
}

impl Format {
    fn response_format(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Pcm => "pcm",
        }
    }
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

#[derive(Clone)]
struct Upstream {
    client: reqwest::Client,
    url: String,
    voice: String,
    speed: f64,
    format: Format,
}

impl Upstream {
    async fn speech(self, input: String) -> reqwest::Result<reqwest::Response> {
        self.client
            .post(format!("{}/v1/audio/speech", self.url))
            .json(
                &json!({"model": "kokoro", "input": input, "voice": self.voice,
                          "response_format": self.format.response_format(),
                          "speed": self.speed}),
            )
            .send()
            .await?
            .error_for_status()
    }
}

impl Tts {
    /// Synthesize `text` with `voice`, chunk by chunk, into one byte stream.
    /// The first chunk decides the result (so an unreachable Kokoro is a
    /// 502/504 here); later chunk failures end the stream with an error.
    pub(crate) async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        speed: f64,
        format: Format,
    ) -> Result<BoxStream<'static, reqwest::Result<Bytes>>, ApiError> {
        let upstream = Upstream {
            client: self.client.clone(),
            url: self.url.clone(),
            voice: voice.to_owned(),
            speed,
            format,
        };
        let mut parts = chunks(text, CHUNK_CHARS).into_iter();
        let first_text = parts.next().unwrap_or_else(|| text.to_owned());
        let first = upstream.clone().speech(first_text).await.map_err(|e| {
            if e.is_timeout() {
                unavailable(StatusCode::GATEWAY_TIMEOUT, "the speech service timed out")
            } else {
                if let Some(status) = e.status() {
                    tracing::warn!(status = status.as_u16(), "tts upstream error");
                }
                unavailable(StatusCode::BAD_GATEWAY, "the speech service failed")
            }
        })?;
        let rest = stream::iter(parts.collect::<Vec<_>>())
            .then(move |chunk| upstream.clone().speech(chunk))
            .flat_map(|r| match r {
                Ok(resp) => resp.bytes_stream().boxed(),
                Err(e) => {
                    tracing::warn!(error = %e, "tts upstream failed mid-stream");
                    stream::once(async move { Err(e) }).boxed()
                }
            });
        Ok(first.bytes_stream().chain(rest).boxed())
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
    let stream = tts.synthesize(text, &voice, speed, Format::Mp3).await?;
    let mut resp = Body::from_stream(stream).into_response();
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
    fn language_score_ties_go_to_italian() {
        assert!(looks_italian("Ciao Giuseppe, come stai oggi?"));
        assert!(looks_italian("Non è vero"));
        assert!(looks_italian("Perché?"));
        assert!(looks_italian("Ho comprato il latte e la frutta"));
        assert!(looks_italian("Giuseppe"));
        assert!(looks_italian(""));
        assert!(!looks_italian("Hello, how are you today?"));
        assert!(!looks_italian("Come here and see the new workout plan"));
        assert!(!looks_italian("I'm fine, thanks"));
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
        assert_eq!(pick_voice(&v, None, None, "Va bene").unwrap(), "if_sara");
    }

    #[test]
    fn long_texts_split_on_sentences() {
        assert_eq!(chunks("Ciao. Come stai?", 300), vec!["Ciao. Come stai?"]);
        let s = "Questa è una frase di prova abbastanza lunga. ".repeat(12);
        let parts = chunks(&s, 300);
        assert!(parts.len() >= 2);
        assert!(parts
            .iter()
            .all(|p| p.chars().count() <= 300 && p.ends_with('.')));
        assert_eq!(parts.join(" "), s.trim());
        // a single very long sentence falls back to commas, then spaces
        let long = format!("{}.", "parola ".repeat(100).trim());
        let parts = chunks(&long, 300);
        assert!(parts.len() >= 3 && parts.iter().all(|p| p.chars().count() <= 300));
        assert_eq!(parts.join(" "), long);
        let blob = "x".repeat(700);
        assert_eq!(chunks(&blob, 300).concat(), blob);
    }
}
