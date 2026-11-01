//! `POST /v1/voice`: one push-to-talk turn for LucinaVoice, the ESP32
//! speaker. Audio in, Lucina's spoken answer out.
//!
//! ```text
//! mic ─► transcribe (Gemini) ─► Lucina chat ─► Kokoro (raw PCM) ─► speaker
//! ```
//!
//! **Request.** The body is the recording, sent with `Transfer-Encoding:
//! chunked` while the button is held (or with a `Content-Length`), at most
//! [`MAX_BODY`] (1 MiB, about 32 s at 16 kHz):
//!
//! - `Content-Type: audio/x-s16le;rate=16000`: raw little-endian signed
//!   16-bit mono PCM at 16 kHz; wrapped into a WAV header here;
//! - `Content-Type: audio/wav`: a PCM WAV file, sent to the transcriber as is.
//!
//! Optional query `lang=it|en` picks the Kokoro voice (and the language of
//! the fixed phrases below); without it the voice follows the language of the
//! reply text, as in [`crate::tts`].
//!
//! **Steps.**
//! 1. Under [`MIN_SECONDS`] of audio, or a transcript that is empty:
//!    `422 no_speech`. Transcription goes **local first**: the audio is
//!    posted to `SECRETARY_VOICE_STT_URL` (default [`DEFAULT_STT_URL`], Caddy's
//!    loopback site on p1, which checks the device token and proxies to
//!    lucina-memo's whisper.cpp on nezopt), with the caller's own
//!    `Authorization: Bearer …` header unchanged (a cookie caller: its `Cookie`
//!    plus `X-Secretary-CSRF: 1`), `lang` passed through, 45 s at most. Raw PCM
//!    goes as `audio/x-s16le;rate=16000`, a WAV file as `audio/wav`. If that
//!    fails (no connection, timeout, 5xx incl. `stt_unavailable`, any other
//!    error status) the turn falls back to Gemini, the transcriber behind
//!    `/v1/calls/transcribe` (`SECRETARY_GEMINI_KEY_FILE`,
//!    `SECRETARY_TRANSCRIPTION_MODEL`; a plain transcript without speaker
//!    labels), and is `503 transcription_unavailable` when that is not set up
//!    either. An empty local transcript is `422 no_speech` with no Gemini retry.
//! 2. The transcript goes to Lucina like `POST /v1/chat`: `sessionId:
//!    "lucinavoice"`, `channel: "web"`, a fresh `clientMessageId`
//!    (`voice-<ms>-<rand>`) and the calling device. A reply Lucina marks
//!    `sensitive` is never spoken or sent in a header: the device hears a
//!    fixed "I'll answer that in the app." instead. An empty reply is
//!    replaced by a fixed short apology.
//! 3. The spoken text (at most [`crate::tts::MAX_CHARS`] characters) is
//!    synthesized by Kokoro as raw PCM and streamed back as it is produced.
//!
//! **Response.** `200`, `Content-Type: audio/x-s16le;rate=24000` (see
//! [`crate::tts::PCM_RATE`]): raw little-endian signed 16-bit mono samples,
//! streamed chunked; the chunk boundaries do not respect sample boundaries.
//! `X-Lucina-Transcript` and `X-Lucina-Reply` carry what was heard and the
//! text that is spoken, UTF-8 percent-encoded after cutting each at 1000
//! bytes (on a character boundary). `X-Tts-Voice` names the voice.
//!
//! **Limits.** [`PER_MINUTE`] turns per device per minute (its own bucket,
//! `429 rate_limited`); one turn at a time per device (`429 voice_busy`).
//!
//! | status | code | |
//! |---|---|---|
//! | 400 | `bad_request` / `invalid_audio` | bad `lang`; a WAV the server cannot read |
//! | 413 | `payload_too_large` | over 1 MiB |
//! | 415 | `unsupported_media_type` | not `audio/wav` or `audio/x-s16le;rate=16000` |
//! | 422 | `no_speech` | too short, or nothing was said |
//! | 429 | `rate_limited`, `voice_busy`, `transcription_provider_refused` | |
//! | 502/504 | `transcription_failed`, `lucina_*`, `tts_unavailable` | an upstream failed |
//! | 503 | `transcription_unavailable`, `lucina_not_configured` | not configured |

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequest, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use base64::Engine;
use secretary_core::models::ChatRequest;
use serde::Deserialize;

use crate::auth::Identity;
use crate::error::ApiError;
use crate::tts::{self, Format};
use crate::AppState;

/// Longest recording accepted.
pub const MAX_BODY: usize = 1024 * 1024;
/// Shorter recordings are a button tap, not speech.
pub const MIN_SECONDS: f64 = 0.3;
/// Sample rate of the raw PCM the device sends.
pub const INPUT_RATE: u32 = 16_000;
/// Voice turns per device per minute.
pub const PER_MINUTE: u32 = 20;
/// Transcription deadline for a voice turn (a recording is up to 32 s).
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(45);
/// `X-Lucina-*` header text is cut at this many bytes before encoding.
const HEADER_TEXT_BYTES: usize = 1000;
/// Local speech-to-text: Caddy's loopback site on p1 (forward_auth against
/// secretary-server, then lucina-memo's `POST /v1/memos/stt`).
pub const DEFAULT_STT_URL: &str = "http://127.0.0.1:8801/v1/memos/stt";
/// Local speech-to-text deadline.
const STT_TIMEOUT: Duration = Duration::from_secs(45);
/// Lucina's chat session for the speaker.
const SESSION_ID: &str = "lucinavoice";

/// Spoken instead of a sensitive reply: (Italian, English).
const SENSITIVE_PHRASE: (&str, &str) = ("Ti rispondo nell'app.", "I'll answer that in the app.");
/// Spoken when Lucina's reply is empty: (Italian, English).
const EMPTY_PHRASE: (&str, &str) = (
    "Scusa, non ho una risposta.",
    "Sorry, I don't have an answer.",
);

/// Rate limit and the one-turn-at-a-time guard.
pub struct Voice {
    pub limiter: crate::ratelimit::RateLimiter,
    busy: Mutex<HashSet<String>>,
    /// The local speech-to-text endpoint, if any.
    stt_url: Option<String>,
    stt: reqwest::Client,
}

impl Voice {
    pub fn new(stt_url: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            limiter: crate::ratelimit::RateLimiter::new(PER_MINUTE),
            busy: Mutex::new(HashSet::new()),
            stt_url,
            stt: reqwest::Client::builder()
                .timeout(STT_TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    /// Ask the local engine. `Ok(text)` (maybe empty) when it answered 200,
    /// `Err(reason)` when the turn should fall back (the reason is logged,
    /// never the audio or the credentials).
    async fn local_stt(
        &self,
        headers: &HeaderMap,
        audio: Bytes,
        content_type: &'static str,
        lang: Option<&str>,
    ) -> Result<String, String> {
        let base = self
            .stt_url
            .as_deref()
            .ok_or("local speech-to-text is off")?;
        let url = match lang.and_then(|l| l.split(['-', '_']).next()) {
            Some(l) if !l.is_empty() => format!("{base}?lang={}", l.to_ascii_lowercase()),
            _ => base.to_owned(),
        };
        let mut req = self
            .stt
            .post(url)
            .header(header::CONTENT_TYPE, content_type)
            .body(audio);
        // The caller's own credentials: the gateway checks them like it does for the app.
        if let Some(auth) = headers.get(header::AUTHORIZATION) {
            req = req.header(header::AUTHORIZATION, auth.clone());
        } else if let Some(cookie) = headers.get(header::COOKIE) {
            req = req
                .header(header::COOKIE, cookie.clone())
                .header("x-secretary-csrf", "1");
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "timed out".to_owned()
            } else {
                "unreachable".to_owned()
            }
        })?;
        let status = resp.status();
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            return Ok(String::new()); // no speech, as far as the engine can tell
        }
        if !status.is_success() {
            return Err(format!("answered {status}"));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| "unreadable reply".to_owned())?;
        v["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "reply without text".to_owned())
    }

    /// Claim the device's turn; `None` while it has one running.
    fn begin<'a>(&'a self, device: &str) -> Option<Turn<'a>> {
        let mut busy = self.busy.lock().unwrap_or_else(|p| p.into_inner());
        busy.insert(device.to_owned()).then(|| Turn {
            voice: self,
            device: device.to_owned(),
        })
    }
}

/// A running turn; frees the device's slot when dropped.
struct Turn<'a> {
    voice: &'a Voice,
    device: String,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.voice
            .busy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.device);
    }
}

#[derive(Deserialize)]
struct VoiceQuery {
    lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    Wav,
    Pcm16k,
}

fn bad_media() -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "expected Content-Type: audio/wav or audio/x-s16le;rate=16000",
    )
}

fn no_speech() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "no_speech",
        "no speech was heard",
    )
}

fn invalid_audio(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_audio", message)
}

/// The body kind named by `Content-Type`.
fn input_kind(headers: &HeaderMap) -> Result<Input, ApiError> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(bad_media)?;
    let mut parts = ct.split(';').map(str::trim);
    let media = parts.next().unwrap_or("").to_ascii_lowercase();
    let rate = parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("rate")
            .then(|| v.trim().trim_matches('"').to_owned())
    });
    match media.as_str() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => Ok(Input::Wav),
        "audio/x-s16le" if rate.as_deref().is_none_or(|r| r == "16000") => Ok(Input::Pcm16k),
        _ => Err(bad_media()),
    }
}

/// A 44-byte WAV header for `data_len` bytes of mono 16-bit PCM at `rate`.
fn wav_header(data_len: usize, rate: u32) -> Vec<u8> {
    let data_len = u32::try_from(data_len).unwrap_or(u32::MAX - 36);
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // mono
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    h.extend_from_slice(&2u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// Seconds of audio in a WAV file, or `None` if it is not one. A `data`
/// size of 0 or past the end of the file (a streamed WAV) means "to the end".
fn wav_seconds(b: &[u8]) -> Option<f64> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u32_at = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            b.get(i..i.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let mut pos = 12usize;
    let mut byte_rate = None;
    while pos.checked_add(8)? <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32_at(pos + 4)? as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if size < 16 {
                return None;
            }
            byte_rate = Some(u32_at(body + 8)?);
        } else if id == b"data" {
            let avail = b.len() - body;
            let n = if size == 0 || size > avail {
                avail
            } else {
                size
            };
            let rate = byte_rate.filter(|r| *r > 0)?;
            return Some(n as f64 / f64::from(rate));
        }
        pos = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// `s` cut at [`HEADER_TEXT_BYTES`] on a character boundary, then
/// percent-encoded (everything but `A-Za-z0-9-_.~`).
fn header_text(s: &str) -> HeaderValue {
    let mut end = s.len().min(HEADER_TEXT_BYTES);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end * 2);
    for b in s[..end].bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    HeaderValue::from_str(&out).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The reply as it is read aloud: markdown marks dropped, at most
/// [`tts::MAX_CHARS`] characters, cut between sentences where possible.
fn speakable(reply: &str) -> String {
    let plain: String = reply
        .lines()
        .map(|l| l.trim_start_matches('#').trim())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_'))
        .collect();
    let plain = plain.trim();
    if plain.chars().count() <= tts::MAX_CHARS {
        return plain.to_owned();
    }
    let mut out = String::new();
    for part in tts::chunks(plain, tts::CHUNK_CHARS) {
        if out.chars().count() + part.chars().count() + 1 > tts::MAX_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&part);
    }
    out
}

/// `fresh clientMessageId`: `voice-<ms>-<8 hex>`.
fn client_message_id() -> Result<String, ApiError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut r = [0u8; 4];
    getrandom::fill(&mut r).map_err(|_| ApiError::internal("random source failed"))?;
    Ok(format!(
        "voice-{ms}-{}",
        r.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

async fn voice(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    req: Request,
) -> Result<Response, ApiError> {
    let kind = input_kind(req.headers())?;
    let Query(VoiceQuery { lang }) = Query::<VoiceQuery>::try_from_uri(req.uri())
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", e.body_text()))?;
    // Validates `lang` (it, en, it-IT, ...).
    let tts = &st.tts;
    tts::pick_voice(&tts.voices, None, lang.as_deref(), "")
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", m))?;
    if let Err(wait) = st.voice.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let _turn = st.voice.begin(&ident.device_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "voice_busy",
            "the previous voice turn is still running",
        )
    })?;

    // The recording: the device streams it while the button is held.
    let caller = req.headers().clone();
    let body = Bytes::from_request(req, &st).await?;
    if body.is_empty() {
        return Err(no_speech());
    }
    let (wav, seconds) = match kind {
        Input::Wav => {
            let seconds = wav_seconds(&body).ok_or_else(|| invalid_audio("not a PCM WAV file"))?;
            (body.to_vec(), seconds)
        }
        Input::Pcm16k => {
            let pcm = &body[..body.len() & !1]; // whole samples only
            let mut wav = wav_header(pcm.len(), INPUT_RATE);
            wav.extend_from_slice(pcm);
            (wav, pcm.len() as f64 / f64::from(INPUT_RATE * 2))
        }
    };
    if seconds < MIN_SECONDS {
        return Err(no_speech());
    }

    let (audio, content_type) = match kind {
        Input::Wav => (body.clone(), "audio/wav"),
        Input::Pcm16k => (body.slice(..body.len() & !1), "audio/x-s16le;rate=16000"),
    };
    let heard = match st
        .voice
        .local_stt(&caller, audio, content_type, lang.as_deref())
        .await
    {
        Ok(text) => text,
        Err(why) => {
            tracing::warn!(%why, "local speech-to-text failed; falling back to Gemini");
            crate::calls::transcribe_inline(
                "audio/wav",
                &base64::engine::general_purpose::STANDARD.encode(&wav),
                crate::calls::Options {
                    diarization: false,
                    timeout: TRANSCRIBE_TIMEOUT,
                },
            )
            .await?
            .text
        }
    }
    .trim()
    .to_owned();
    if heard.is_empty() {
        return Err(no_speech());
    }

    let chat = ChatRequest {
        message: heard.clone(),
        session_id: Some(SESSION_ID.into()),
        client_message_id: Some(client_message_id()?),
        channel: Some("web".into()),
        device: Some(ident.device_id.clone()),
        device_name: Some(ident.device_name.clone()),
        reply_to: None,
    };
    let reply = st.lucina()?.chat(&chat).await?;
    let sensitive = reply.extra.get("sensitive").and_then(|v| v.as_bool()) == Some(true);

    // What is spoken, and the language it is in when `lang` did not say.
    let italian = match lang.as_deref() {
        Some(l) => l.to_ascii_lowercase().starts_with("it"),
        None => tts::looks_italian(&heard),
    };
    let phrase = |p: (&'static str, &'static str)| (if italian { p.0 } else { p.1 }).to_owned();
    let spoken = if sensitive {
        phrase(SENSITIVE_PHRASE)
    } else {
        Some(speakable(&reply.reply))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| phrase(EMPTY_PHRASE))
    };
    let voice_name = tts::pick_voice(&tts.voices, None, lang.as_deref(), &spoken)
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", m))?;
    let stream = tts
        .synthesize(&spoken, &voice_name, 1.0, Format::Pcm)
        .await?;

    let mut resp = Body::from_stream(stream).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("audio/x-s16le;rate={}", tts::PCM_RATE))
            .unwrap_or_else(|_| HeaderValue::from_static("audio/x-s16le")),
    );
    h.insert("x-lucina-transcript", header_text(&heard));
    h.insert("x-lucina-reply", header_text(&spoken));
    if let Ok(v) = HeaderValue::from_str(&voice_name) {
        h.insert("x-tts-voice", v);
    }
    Ok(resp)
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/voice",
        post(voice).layer(DefaultBodyLimit::max(MAX_BODY)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ct(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn content_types() {
        assert_eq!(input_kind(&ct("audio/wav")).unwrap(), Input::Wav);
        assert_eq!(input_kind(&ct("Audio/X-WAV")).unwrap(), Input::Wav);
        for v in [
            "audio/x-s16le;rate=16000",
            "audio/x-s16le; rate=16000",
            "audio/x-s16le",
        ] {
            assert_eq!(input_kind(&ct(v)).unwrap(), Input::Pcm16k, "{v}");
        }
        for v in [
            "audio/x-s16le;rate=44100",
            "audio/mpeg",
            "application/json",
            "",
        ] {
            assert!(input_kind(&ct(v)).is_err(), "{v}");
        }
        assert!(input_kind(&HeaderMap::new()).is_err());
    }

    #[test]
    fn wav_header_round_trips() {
        let mut wav = wav_header(32_000, INPUT_RATE);
        assert_eq!(wav.len(), 44);
        wav.extend(std::iter::repeat_n(0u8, 32_000));
        assert!((wav_seconds(&wav).unwrap() - 1.0).abs() < 1e-9);
        // A streamed WAV with a zero or oversized data size runs to the end.
        wav[40..44].copy_from_slice(&0u32.to_le_bytes());
        assert!((wav_seconds(&wav).unwrap() - 1.0).abs() < 1e-9);
        wav[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!((wav_seconds(&wav).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn wav_parser_rejects_junk() {
        assert!(wav_seconds(b"").is_none());
        assert!(wav_seconds(b"RIFFxxxxWAVE").is_none());
        assert!(wav_seconds(b"not a wav file at all").is_none());
        // data before fmt: no rate known
        let mut b = b"RIFF\0\0\0\0WAVEdata".to_vec();
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        assert!(wav_seconds(&b).is_none());
        // a chunk size that overflows the walk
        let mut b = b"RIFF\0\0\0\0WAVEjunk".to_vec();
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(wav_seconds(&b).is_none());
    }

    #[test]
    fn header_text_is_cut_on_a_char_boundary_then_encoded() {
        assert_eq!(header_text("Ciao, come stai?"), "Ciao%2C%20come%20stai%3F");
        assert_eq!(header_text("perché"), "perch%C3%A9");
        // 999 ASCII bytes then a 2-byte char: the cut at 1000 would split it.
        let s = format!("{}é", "a".repeat(999));
        assert_eq!(header_text(&s).to_str().unwrap(), "a".repeat(999));
        let s = "a".repeat(2000);
        assert_eq!(header_text(&s).len(), 1000);
        assert_eq!(header_text("a\nb"), "a%0Ab");
    }

    #[test]
    fn spoken_text_is_plain_and_bounded() {
        assert_eq!(
            speakable("## Meteo\n**Oggi** sole `ok`"),
            "Meteo\nOggi sole ok"
        );
        let long = "Questa è una frase. ".repeat(200);
        let s = speakable(&long);
        assert!(s.chars().count() <= tts::MAX_CHARS);
        assert!(s.ends_with('.'));
        assert_eq!(speakable("  \n "), "");
    }

    #[test]
    fn one_turn_per_device() {
        let v = Voice::new(None).unwrap();
        let a = v.begin("dev_a").unwrap();
        assert!(v.begin("dev_a").is_none());
        assert!(v.begin("dev_b").is_some());
        drop(a);
        assert!(v.begin("dev_a").is_some());
    }
}
