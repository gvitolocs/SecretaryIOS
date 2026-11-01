//! `POST /v1/memos/stt`: one short clip of speech in, its text out.
//!
//! Used by secretary-server's `POST /v1/voice` (the LucinaVoice speaker), so
//! Lucina needs no cloud key to understand it. It runs on the prompter's
//! whisper-server (small model, GPU lease first, CPU if there is none), the
//! same engine as the live prompter, so nothing new is loaded or configured.
//!
//! - Body: `Content-Type: audio/wav` (16 kHz mono 16-bit PCM) or
//!   `audio/x-s16le;rate=16000` (the same samples, raw little-endian); at most
//!   [`MAX_BODY`]. Another type or format is 415, under [`MIN_SECONDS`] of audio 422
//!   `no_speech`.
//! - Query: `lang` only: `auto` (default), `it`, `en` or `da`.
//! - `200 {text, language, ms, mode}`: `language` is the code whisper heard
//!   (`it`, `en`, `da`; whisper's own name for any other), `mode` is `gpu` or
//!   `cpu`. Silence is a 200 with `text: ""`; the caller decides what that means.
//! - 503 `stt_unavailable` when the engine does not start (no model, no
//!   whisper-server), 502 `stt_failed` when a started engine fails on the clip.
//! - Same auth as every `/v1/memos` route (Caddy's gateway key, or the service token).
//!
//! The engine is joined as a session for the length of the request and left
//! again on every path, including a client that hangs up (the work runs in its
//! own task, and [`Lease`] releases on drop), so the reaper can stop an idle server.
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

use crate::api::{ApiError, R};
use crate::prompter::engine::{Engine, Mode};
use crate::prompter::segmenter::RATE;
use crate::App;

const RATE_HZ: u32 = RATE as u32;

/// Largest clip accepted (about 65 s of 16 kHz PCM).
pub const MAX_BODY: usize = 2 * 1024 * 1024;
/// Shorter clips are a button tap, not speech.
pub const MIN_SECONDS: f64 = 0.3;

pub fn routes(app: Arc<App>) -> Router<Arc<App>> {
    Router::new()
        .route(
            "/v1/memos/stt",
            post(stt).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route_layer(axum::middleware::from_fn_with_state(app, crate::api::auth))
}

fn unsupported() -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "expected audio/wav (16 kHz mono 16-bit) or audio/x-s16le;rate=16000",
    )
}

fn no_speech() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "no_speech",
        "no speech was heard",
    )
}

/// The `lang` query: `auto` (absent), `it`, `en` or `da`; no other parameter.
fn language(q: &HashMap<String, String>) -> Result<String, ApiError> {
    if let Some(k) = q.keys().find(|k| k.as_str() != "lang") {
        return Err(ApiError::bad(format!("unknown query parameter {k}")));
    }
    match q.get("lang").map(|l| l.trim().to_ascii_lowercase()) {
        None => Ok("auto".into()),
        Some(l) if matches!(l.as_str(), "auto" | "it" | "en" | "da") => Ok(l),
        Some(_) => Err(ApiError::bad("lang: auto, it, en or da")),
    }
}

/// 16 kHz mono 16-bit samples of a WAV file. A `data` size of 0 or past the end
/// of the file (a streamed WAV) means "to the end".
fn wav_samples(b: &[u8]) -> Result<Vec<i16>, ApiError> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(ApiError::bad("not a WAV file"));
    }
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut i = 12usize;
    while i.saturating_add(8) <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let body = i + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            format = u16::from_le_bytes([b[body], b[body + 1]]);
            channels = u16::from_le_bytes([b[body + 2], b[body + 3]]);
            rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
            bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
        } else if id == b"data" {
            if format != 1 || channels != 1 || rate != RATE_HZ || bits != 16 {
                return Err(unsupported());
            }
            let avail = b.len() - body;
            let n = if len == 0 || len > avail { avail } else { len };
            return Ok(samples(&b[body..body + n]));
        }
        i = match body.checked_add(len).and_then(|e| e.checked_add(len & 1)) {
            Some(next) => next,
            None => break,
        };
    }
    Err(ApiError::bad("WAV without a data chunk"))
}

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect()
}

/// The 16 kHz samples of the body, by `Content-Type`.
fn decode(headers: &HeaderMap, body: &[u8]) -> Result<Vec<i16>, ApiError> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unsupported)?;
    let mut parts = ct.split(';').map(str::trim);
    let media = parts.next().unwrap_or("").to_ascii_lowercase();
    let rate = parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("rate")
            .then(|| v.trim().trim_matches('"').to_owned())
    });
    let pcm = match media.as_str() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => wav_samples(body)?,
        "audio/x-s16le" if rate.as_deref().is_none_or(|r| r == "16000") => samples(body),
        _ => return Err(unsupported()),
    };
    if (pcm.len() as f64) / f64::from(RATE_HZ) < MIN_SECONDS {
        return Err(no_speech());
    }
    Ok(pcm)
}

/// A joined engine session; leaves it when dropped, on any path.
pub struct Lease(Arc<Engine>);

impl Lease {
    /// Join the engine (starting it if needed). `Err` = it did not start, and
    /// nothing is joined.
    pub async fn acquire(engine: &Arc<Engine>) -> Result<(Self, Mode), String> {
        let mode = engine.acquire().await?;
        Ok((Self(engine.clone()), mode))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let engine = self.0.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move { engine.release().await });
            }
            Err(_) => tracing::error!("stt: no runtime to leave the engine session"),
        }
    }
}

async fn stt(
    State(app): State<Arc<App>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> R {
    let lang = language(&q)?;
    let pcm = decode(&headers, &body)?;
    let engine = app.prompter.engine.clone();
    // Its own task: a client that hangs up must not cancel the join half way.
    let job = tokio::spawn(async move {
        let (_lease, mode) = Lease::acquire(&engine).await.map_err(|e| {
            tracing::warn!(error = %e, "stt: engine did not start");
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "stt_unavailable", e)
        })?;
        let out = engine
            .transcribe(&pcm, &lang, "Lucina")
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "stt: transcription failed");
                ApiError::new(StatusCode::BAD_GATEWAY, "stt_failed", e)
            })?;
        Ok::<_, ApiError>((mode, lang, out))
    });
    let (mode, lang, out) = job.await.map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "speech-to-text task failed",
        )
    })??;
    let language = crate::prompter::session::lang_code(&out.language)
        .map(str::to_owned)
        .or_else(|| (!out.language.is_empty()).then(|| out.language.to_ascii_lowercase()))
        .unwrap_or(if lang == "auto" { String::new() } else { lang });
    Ok(Json(json!({
        "text": out.text,
        "language": language,
        "ms": out.ms,
        "mode": mode,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ct(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_str(v).unwrap());
        h
    }

    fn pcm_bytes(n: usize) -> Vec<u8> {
        (0..n as i16).flat_map(|s| s.to_le_bytes()).collect()
    }

    fn wav(rate: u32, channels: u16, pcm: &[u8]) -> Vec<u8> {
        let mut w = b"RIFF".to_vec();
        w.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
        w.extend_from_slice(&(2 * channels).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        w.extend_from_slice(pcm);
        w
    }

    #[test]
    fn raw_pcm_and_wav_decode_to_the_same_samples() {
        let raw = pcm_bytes(8000);
        let a = decode(&ct("audio/x-s16le;rate=16000"), &raw).unwrap();
        let b = decode(&ct("audio/x-s16le"), &raw).unwrap();
        let c = decode(&ct("Audio/WAV"), &wav(16_000, 1, &raw)).unwrap();
        assert_eq!(a.len(), 8000);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a[3], 3);
        // A streamed WAV: data size 0 or too big means "to the end".
        let mut streamed = wav(16_000, 1, &raw);
        streamed[40..44].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&ct("audio/wav"), &streamed).unwrap(), a);
        streamed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&ct("audio/wav"), &streamed).unwrap(), a);
    }

    #[test]
    fn other_media_and_formats_are_415() {
        let raw = pcm_bytes(8000);
        for v in [
            "application/json",
            "audio/mpeg",
            "audio/x-s16le;rate=24000",
            "",
        ] {
            let e = decode(&ct(v), &raw).unwrap_err();
            assert_eq!(e.0, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");
        }
        assert_eq!(
            decode(&HeaderMap::new(), &raw).unwrap_err().0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        for w in [wav(44_100, 1, &raw), wav(16_000, 2, &raw)] {
            let e = decode(&ct("audio/wav"), &w).unwrap_err();
            assert_eq!(e.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        // Not a WAV at all, or one without audio, is a bad request.
        assert_eq!(
            decode(&ct("audio/wav"), &raw).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            decode(&ct("audio/wav"), b"RIFF\0\0\0\0WAVEfmt ")
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn under_a_third_of_a_second_is_no_speech() {
        for n in [0, 1, 4799] {
            let e = decode(&ct("audio/x-s16le;rate=16000"), &pcm_bytes(n)).unwrap_err();
            assert_eq!((e.0, e.1), (StatusCode::UNPROCESSABLE_ENTITY, "no_speech"));
        }
        assert!(decode(&ct("audio/x-s16le;rate=16000"), &pcm_bytes(4800)).is_ok());
        // An odd trailing byte is dropped, not an error.
        let mut odd = pcm_bytes(8000);
        odd.push(9);
        assert_eq!(
            decode(&ct("audio/x-s16le;rate=16000"), &odd).unwrap().len(),
            8000
        );
    }

    #[test]
    fn only_lang_is_accepted_in_the_query() {
        let q = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(language(&q(&[])).unwrap(), "auto");
        assert_eq!(language(&q(&[("lang", "IT")])).unwrap(), "it");
        assert_eq!(language(&q(&[("lang", "auto")])).unwrap(), "auto");
        for bad in [
            q(&[("lang", "fr")]),
            q(&[("lang", "")]),
            q(&[("x", "1")]),
            q(&[("lang", "en"), ("prompt", "hi")]),
        ] {
            assert_eq!(language(&bad).unwrap_err().0, StatusCode::BAD_REQUEST);
        }
    }
}
