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
//! 2. The transcript goes to Lucina like `POST /v1/chat` but as a stream
//!    (`Accept: text/event-stream`, `voice: true`; see [`crate::voice_pipe`]):
//!    `sessionId: "lucinavoice"`, `channel: "voice"` (the shared "Lucina Voice" thread, protocol
//!    section 7), a fresh `clientMessageId`
//!    (`voice-<ms>-<rand>`) and the calling device. A Lucina that answers in
//!    plain JSON works too. A reply Lucina marks `sensitive` is never spoken:
//!    the device hears a fixed "I'll answer that in the app." instead. An empty
//!    reply is replaced by a fixed short apology.
//! 3. The reply is cut into sentences as it is written; each goes to Kokoro as
//!    raw PCM, in order, the next one requested while the current one streams
//!    ([`crate::voice_pipe`]). At most [`crate::tts::MAX_CHARS`] characters are
//!    spoken. The response body starts with the first audio and carries the
//!    PCM as it is produced.
//!
//! **Response.** `200`, `Content-Type: audio/x-s16le;rate=24000` (see
//! [`crate::tts::PCM_RATE`]): raw little-endian signed 16-bit mono samples,
//! streamed chunked, in frames of at most 8 KiB that keep sample boundaries.
//! `X-Lucina-Transcript` carries what was heard, UTF-8 percent-encoded after
//! cutting at 1000 bytes (on a character boundary). `X-Tts-Voice` names the
//! voice, chosen from `lang`, else from the transcript and the first sentence.
//! **There is no `X-Lucina-Reply` any more:** the reply is not known when the
//! first audio (and with it the headers) goes out. A failure after the body
//! started ends the stream abnormally instead of changing the status.
//!
//! The same pipeline serves the persistent socket, `GET /v1/voice/ws`
//! ([`crate::voice_ws`]), which is what LucinaVoice uses.
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequest, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use base64::Engine;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::auth::Identity;
use crate::error::ApiError;
use crate::tts;
use crate::voice_pipe::{self, Shared, Spec};
use crate::voice_telemetry::{self, TurnInfo};
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
/// A voice note's upload (the loopback share endpoint accepts it at once).
const SHARE_TIMEOUT: Duration = Duration::from_secs(120);
/// A warm-up (or any answered local transcription) this recent means the
/// engine is loaded: `start` on the socket does not warm it again.
pub const WARM_FRESH: Duration = Duration::from_secs(60);
/// What the warm-up sends: 0.5 s of silence at 16 kHz.
const WARM_BYTES: usize = 16_000;

/// The caller's own credentials on a loopback request through Caddy (the gateway checks them like
/// it does for the app): `Authorization` unchanged, else the `Cookie` plus `X-Secretary-CSRF: 1`.
pub(crate) fn with_creds(req: reqwest::RequestBuilder, headers: &HeaderMap) -> reqwest::RequestBuilder {
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        req.header(header::AUTHORIZATION, auth.clone())
    } else if let Some(cookie) = headers.get(header::COOKIE) {
        req.header(header::COOKIE, cookie.clone())
            .header("x-secretary-csrf", "1")
    } else {
        req
    }
}

/// Rate limit and the one-turn-at-a-time guard.
pub struct Voice {
    pub limiter: crate::ratelimit::RateLimiter,
    busy: Mutex<HashSet<String>>,
    /// The local speech-to-text endpoint, if any.
    stt_url: Option<String>,
    stt: reqwest::Client,
    /// Where voice notes are posted (`POST /v1/share`), if notes are on.
    share_url: Option<String>,
    share: reqwest::Client,
    warm: Mutex<Warm>,
    /// Devices whose socket turns are tests (`SECRETARY_VOICE_TEST_DEVICES`, protocol section 10.8).
    test_devices: HashSet<String>,
    /// The Lucina API token: the key of a test share's marker ([`crate::voice_marker`]).
    test_key: Option<Vec<u8>>,
}

/// When the local engine last answered, and whether a warm-up is under way.
#[derive(Default)]
struct Warm {
    last_ok: Option<Instant>,
    in_flight: bool,
}

impl Voice {
    pub fn new(stt_url: Option<String>, share_url: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            limiter: crate::ratelimit::RateLimiter::new(PER_MINUTE),
            busy: Mutex::new(HashSet::new()),
            warm: Mutex::new(Warm::default()),
            test_devices: HashSet::new(),
            test_key: None,
            stt_url,
            stt: reqwest::Client::builder()
                .timeout(STT_TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            share_url,
            share: reqwest::Client::builder()
                .timeout(SHARE_TIMEOUT)
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    /// These device ids' socket turns are tests: Lucina runs them silently, out of the owner's chat and memory.
    #[must_use]
    pub fn with_test_devices(mut self, devices: &[String], key: Option<&str>) -> Self {
        self.test_devices = devices.iter().cloned().collect();
        self.test_key = key.map(|k| k.as_bytes().to_vec());
        self
    }

    /// The `X-Voice-Test` value for a test share `share_id` (`None` without the Lucina token to sign with).
    pub fn test_marker(&self, share_id: &str) -> Option<String> {
        crate::voice_marker::sign(self.test_key.as_deref()?, share_id)
    }

    /// Whether `device`'s socket turns are tests.
    pub fn is_test_device(&self, device: &str) -> bool {
        self.test_devices.contains(device)
    }

    /// The share endpoint voice notes are posted to; `None` = notes are off.
    pub fn share_url(&self) -> Option<&str> {
        self.share_url.as_deref()
    }

    pub fn share_client(&self) -> &reqwest::Client {
        &self.share
    }

    /// Ask the local engine. `Ok(heard)` (its text maybe empty) when it
    /// answered 200, `Err(reason)` when the turn should fall back (the reason
    /// is logged, never the audio or the credentials). The engine's Voice Learn
    /// facts (`corrections`, `ambiguous`) ride along when it sends them.
    async fn local_stt(
        &self,
        headers: &HeaderMap,
        audio: Bytes,
        content_type: &'static str,
        lang: Option<&str>,
    ) -> Result<Heard, String> {
        let base = self
            .stt_url
            .as_deref()
            .ok_or("local speech-to-text is off")?;
        let url = match lang.and_then(|l| l.split(['-', '_']).next()) {
            Some(l) if !l.is_empty() => format!("{base}?lang={}", l.to_ascii_lowercase()),
            _ => base.to_owned(),
        };
        let req = self
            .stt
            .post(url)
            .header(header::CONTENT_TYPE, content_type)
            .body(audio);
        let resp = with_creds(req, headers).send().await.map_err(|e| {
            if e.is_timeout() {
                "timed out".to_owned()
            } else {
                "unreachable".to_owned()
            }
        })?;
        let status = resp.status();
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            self.engine_answered();
            return Ok(Heard::default()); // no speech, as far as the engine can tell
        }
        if !status.is_success() {
            return Err(format!("answered {status}"));
        }
        self.engine_answered();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| "unreadable reply".to_owned())?;
        let text = v["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "reply without text".to_owned())?;
        Ok(Heard {
            text,
            facts: SttFacts::from_json(&v),
        })
    }

    fn engine_answered(&self) {
        self.warm.lock().unwrap_or_else(|p| p.into_inner()).last_ok = Some(Instant::now());
    }

    /// Load the local engine ahead of a turn: post 0.5 s of silence with the
    /// caller's credentials, unless the engine answered in the last
    /// [`WARM_FRESH`] or a warm-up is already running. The result is only
    /// logged.
    pub async fn warm_stt(&self, headers: &HeaderMap, lang: Option<&str>) {
        if self.stt_url.is_none() {
            return;
        }
        {
            let mut w = self.warm.lock().unwrap_or_else(|p| p.into_inner());
            if w.in_flight || w.last_ok.is_some_and(|t| t.elapsed() < WARM_FRESH) {
                return;
            }
            w.in_flight = true;
        }
        let started = Instant::now();
        let r = self
            .local_stt(
                headers,
                Bytes::from(vec![0u8; WARM_BYTES]),
                "audio/x-s16le;rate=16000",
                lang,
            )
            .await;
        self.warm
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .in_flight = false;
        match r {
            Ok(_) => tracing::debug!(
                ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "speech-to-text warmed"
            ),
            Err(why) => tracing::debug!(%why, "speech-to-text warm-up failed"),
        }
    }

    /// Claim the device's turn; `false` while it has one running.
    fn claim(&self, device: &str) -> bool {
        self.busy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(device.to_owned())
    }

    fn release(&self, device: &str) {
        self.busy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(device);
    }
}

/// A running turn of a device; frees the device's slot when dropped.
pub struct Turn {
    st: AppState,
    device: String,
}

impl Turn {
    /// Claim the device's turn; `None` while it has one running (over HTTP or
    /// on a socket).
    pub fn begin(st: &AppState, device: &str) -> Option<Self> {
        st.voice.claim(device).then(|| Self {
            st: st.clone(),
            device: device.to_owned(),
        })
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.st.voice.release(&self.device);
    }
}

/// The recording of a turn.
pub enum Audio {
    /// A PCM WAV file.
    Wav(Bytes),
    /// Raw s16le mono 16 kHz.
    Pcm(Bytes),
}

/// A word the speech-to-text engine replaced by a Voice Learn entry's
/// canonical word (`corrections: [{from, to}]` of lucina-memo's `/v1/memos/stt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub from: String,
    pub to: String,
}

/// A stretch of the transcript that might be one of several vocabulary words, left alone by
/// the engine (`ambiguous: [{heard, candidates}]` of `/v1/memos/stt`). Lucina's `vocab` layer
/// asks Jev which one the speaker meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ambiguity {
    pub heard: String,
    /// The canonical words it might be, closest first.
    pub candidates: Vec<String>,
}

/// What the engine said about Voice Learn for one recording. The counts go to the telemetry
/// (never the words); the ambiguous spans go to Lucina's `voiceMeta`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SttFacts {
    pub corrections: Vec<Correction>,
    /// Matches the engine found too close to call.
    pub ambiguous: Vec<Ambiguity>,
}

/// Most corrections kept per turn.
const MAX_CORRECTIONS: usize = 16;
/// Most ambiguous spans kept per turn, and candidates per span (Lucina's own caps are 3 and 5).
const MAX_AMBIGUOUS: usize = 3;
const MAX_CANDIDATES: usize = 5;

fn clean_word(v: &serde_json::Value) -> String {
    v.as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>'))
        .take(100)
        .collect()
}

impl SttFacts {
    /// `corrections` (`[{from, to}]`) and `ambiguous` (`[{heard, candidates}]`) of the
    /// engine's JSON answer. Untrusted: cleaned and bounded.
    pub fn from_json(v: &serde_json::Value) -> Self {
        let corrections = v["corrections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let (from, to) = (clean_word(&c["from"]), clean_word(&c["to"]));
                (!from.is_empty() && !to.is_empty()).then_some(Correction { from, to })
            })
            .take(MAX_CORRECTIONS)
            .collect();
        let ambiguous = v["ambiguous"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let heard = clean_word(&a["heard"]);
                let candidates: Vec<String> = a["candidates"]
                    .as_array()?
                    .iter()
                    .map(clean_word)
                    .filter(|c| !c.is_empty())
                    .take(MAX_CANDIDATES)
                    .collect();
                (!heard.is_empty() && !candidates.is_empty()).then_some(Ambiguity { heard, candidates })
            })
            .take(MAX_AMBIGUOUS)
            .collect();
        Self {
            corrections,
            ambiguous,
        }
    }

    /// The facts of an in-process correction ([`lucina_vocab::Vocab::correct`]), cleaned and
    /// bounded like the engine's JSON answer.
    pub fn from_corrected(c: &lucina_vocab::Corrected) -> Self {
        let corrections = c
            .corrections
            .iter()
            .map(|(from, to)| Correction {
                from: clean_word(&serde_json::json!(from)),
                to: clean_word(&serde_json::json!(to)),
            })
            .filter(|c| !c.from.is_empty() && !c.to.is_empty())
            .take(MAX_CORRECTIONS)
            .collect();
        let ambiguous = c
            .ambiguous
            .iter()
            .filter_map(|a| {
                let heard = clean_word(&serde_json::json!(a.heard));
                let candidates: Vec<String> = a
                    .candidates
                    .iter()
                    .map(|c| clean_word(&serde_json::json!(c)))
                    .filter(|c| !c.is_empty())
                    .take(MAX_CANDIDATES)
                    .collect();
                (!heard.is_empty() && !candidates.is_empty()).then_some(Ambiguity { heard, candidates })
            })
            .take(MAX_AMBIGUOUS)
            .collect();
        Self {
            corrections,
            ambiguous,
        }
    }

    /// `{heard, candidates}` spans as JSON, for `voiceMeta`.
    pub fn ambiguous_json(&self) -> Vec<serde_json::Value> {
        self.ambiguous
            .iter()
            .map(|a| serde_json::json!({"heard": a.heard, "candidates": a.candidates}))
            .collect()
    }

    /// `{from, to}` pairs as JSON, for the sample answer.
    pub fn corrections_json(&self) -> Vec<serde_json::Value> {
        self.corrections
            .iter()
            .map(|c| serde_json::json!({"from": c.from, "to": c.to}))
            .collect()
    }
}

/// A transcript and the Voice Learn facts that came with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    pub text: String,
    pub facts: SttFacts,
}

/// Speech to text: the local engine first (`caller`'s credentials, `lang`
/// passed through), Gemini when that fails (with a "Vocabulary:" line from
/// Voice Learn, [`crate::voice_learn`], and its text post-corrected with the same
/// vocabulary, so both paths answer text, corrections and ambiguous spans). The text is trimmed; empty means
/// nobody spoke. The error is the HTTP one (`503 transcription_unavailable`
/// when Gemini is not set up either).
pub async fn transcribe(
    st: &AppState,
    caller: &HeaderMap,
    audio: &Audio,
    lang: Option<&str>,
) -> Result<Heard, ApiError> {
    let (body, content_type) = match audio {
        Audio::Wav(b) => (b.clone(), "audio/wav"),
        Audio::Pcm(b) => (b.slice(..b.len() & !1), "audio/x-s16le;rate=16000"),
    };
    let heard = match st.voice.local_stt(caller, body, content_type, lang).await {
        Ok(heard) => heard,
        Err(why) => {
            tracing::warn!(%why, "local speech-to-text failed; falling back to Gemini");
            let wav = match audio {
                Audio::Wav(b) => b.to_vec(),
                Audio::Pcm(b) => {
                    let pcm = &b[..b.len() & !1];
                    let mut wav = wav_header(pcm.len(), INPUT_RATE);
                    wav.extend_from_slice(pcm);
                    wav
                }
            };
            // Only when Gemini can be asked at all: no point fetching the list otherwise.
            let vocab = if crate::calls::gemini_configured() {
                st.voice_vocab.load(st).await
            } else {
                None
            };
            let text = crate::calls::transcribe_inline(
                "audio/wav",
                &base64::engine::general_purpose::STANDARD.encode(&wav),
                crate::calls::Options {
                    diarization: false,
                    timeout: TRANSCRIBE_TIMEOUT,
                },
                vocab.as_ref().and_then(|v| v.line.as_deref()),
            )
            .await?
            .text;
            // The local engine corrects with the vocabulary itself; Gemini's text gets the same
            // post-correction here, so both paths answer {text, corrections, ambiguous}.
            match vocab.filter(|v| !v.vocab.entries.is_empty() && !text.trim().is_empty()) {
                Some(v) => {
                    let fixed = v.vocab.correct(&text);
                    Heard {
                        facts: SttFacts::from_corrected(&fixed),
                        text: fixed.text,
                    }
                }
                None => Heard {
                    text,
                    facts: SttFacts::default(),
                },
            }
        }
    };
    Ok(Heard {
        text: heard.text.trim().to_owned(),
        facts: heard.facts,
    })
}

#[derive(Deserialize)]
struct VoiceQuery {
    lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
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

pub(crate) fn no_speech() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "no_speech",
        "no speech was heard",
    )
}

pub(crate) fn invalid_audio(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_audio", message)
}

/// The body kind named by `Content-Type`.
pub(crate) fn input_kind(headers: &HeaderMap) -> Result<Input, ApiError> {
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
pub(crate) fn wav_seconds(b: &[u8]) -> Option<f64> {
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

/// `fresh clientMessageId`: `voice-<ms>-<8 hex>`.
pub(crate) fn client_message_id() -> Result<String, ApiError> {
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

/// The recording of a request body (whole samples of raw PCM, or a PCM WAV
/// file) and its length in seconds.
pub(crate) fn audio_of(kind: Input, body: Bytes) -> Result<(Audio, f64), ApiError> {
    match kind {
        Input::Wav => {
            let seconds = wav_seconds(&body).ok_or_else(|| invalid_audio("not a PCM WAV file"))?;
            Ok((Audio::Wav(body), seconds))
        }
        Input::Pcm16k => {
            let pcm = body.slice(..body.len() & !1); // whole samples only
            let seconds = pcm.len() as f64 / f64::from(INPUT_RATE * 2);
            Ok((Audio::Pcm(pcm), seconds))
        }
    }
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
    tts::pick_voice(&st.tts.voices, None, lang.as_deref(), "")
        .map_err(|m| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", m))?;
    if let Err(wait) = st.voice.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let turn = Turn::begin(&st, &ident.device_id).ok_or_else(|| {
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
    let up_bytes = body.len() as u64;
    let (audio, seconds) = audio_of(kind, body)?;
    if seconds < MIN_SECONDS {
        return Err(no_speech());
    }

    // End of speech is now: the spans below count from here.
    let shared = Arc::new(Shared::new(Instant::now()));
    let mut info = TurnInfo {
        err: None,
        transcript_chars: 0,
        speech_ms: (seconds * 1000.0) as u64,
        up_bytes,
        ..TurnInfo::default()
    };
    let Heard { text: heard, facts } =
        match transcribe(&st, &caller, &audio, lang.as_deref()).await {
            Ok(h) => h,
            Err(e) => {
                info.err = Some("stt_unavailable".into());
                voice_telemetry::finish_turn(&st, &ident, &shared, info);
                return Err(e);
            }
        };
    shared.set_stt(shared.elapsed_ms());
    info.stt_corrections = facts.corrections.len() as u32;
    info.stt_ambiguous = facts.ambiguous.len() as u32;
    if heard.is_empty() {
        info.err = Some("no_speech".into());
        voice_telemetry::finish_turn(&st, &ident, &shared, info);
        return Err(no_speech());
    }
    info.transcript_chars = heard.chars().count() as u64;

    // The spoken reply, produced by a task that outlives this handler and
    // feeds the response body.
    let (out_tx, mut out_rx) = mpsc::channel::<Bytes>(16);
    let spec = Spec::voice_thread(heard.clone(), lang, ident.clone(), facts);
    let task = tokio::spawn({
        let (st, shared) = (st.clone(), shared.clone());
        async move {
            let _turn = turn; // the device is busy until its audio is made
            let r = voice_pipe::speak_turn(st.clone(), spec, shared.clone(), out_tx).await;
            match &r {
                Err(voice_pipe::PipeError::Closed) => {} // the client went away
                Err(e) => info.err = Some(e.code().into()),
                Ok(_) => {}
            }
            if !matches!(r, Err(voice_pipe::PipeError::Closed)) {
                voice_telemetry::finish_turn(&st, &ident, &shared, info);
            }
            r
        }
    });
    let mut task = AbortOnDrop(task);

    // Headers wait for the first audio, so a failure before it is a status.
    let Some(first) = out_rx.recv().await else {
        return Err(match (&mut task.0).await {
            Ok(Err(e)) => e.into_api(),
            Ok(Ok(_)) => ApiError::internal("no audio was produced"),
            Err(_) => ApiError::internal("voice turn failed"),
        });
    };
    let voice_name = shared.voice.get().cloned();

    let stream = futures_util::stream::unfold(
        (Some(first), out_rx, Some(task)),
        |(first, mut rx, mut task)| async move {
            if let Some(chunk) = first {
                return Some((Ok::<_, std::io::Error>(chunk), (None, rx, task)));
            }
            if let Some(chunk) = rx.recv().await {
                return Some((Ok(chunk), (None, rx, task)));
            }
            // The audio is over: a failure ends the body abnormally.
            let mut task = task.take()?;
            let result = (&mut task.0).await;
            match result {
                Ok(Err(e)) => {
                    tracing::warn!(code = e.code(), "voice turn failed after its audio began");
                    Some((
                        Err(std::io::Error::other("voice turn failed")),
                        (None, rx, None),
                    ))
                }
                _ => None,
            }
        },
    );
    let mut resp = Body::from_stream(stream).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("audio/x-s16le;rate={}", tts::PCM_RATE))
            .unwrap_or_else(|_| HeaderValue::from_static("audio/x-s16le")),
    );
    h.insert("x-lucina-transcript", header_text(&heard));
    if let Some(v) = voice_name.and_then(|v| HeaderValue::from_str(&v).ok()) {
        h.insert("x-tts-voice", v);
    }
    Ok(resp)
}

/// Aborts its task when dropped (the response body went away).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/voice",
            post(voice).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/v1/voice/ws", get(crate::voice_ws::upgrade))
        .merge(crate::voice_telemetry::routes())
        .merge(crate::voice_learn::routes())
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
    fn stt_facts_are_cleaned_and_bounded() {
        let v = serde_json::json!({
            "text": "x",
            "corrections": [
                {"from": "skibi gard", "to": "Skibigårdsvej"},
                {"from": "", "to": "dropped"},
                {"from": "a<b>\n", "to": "ok"},
                "junk"
            ],
            "ambiguous": [
                {"heard": "via", "candidates": ["Skibigårdsvej", "Skibbyvej", "", 7]},
                {"heard": "x"},
                {"heard": "", "candidates": ["y"]},
                {"heard": "mikel", "candidates": ["Mikkel"]}
            ]
        });
        let f = SttFacts::from_json(&v);
        assert_eq!(
            f.corrections,
            vec![
                Correction { from: "skibi gard".into(), to: "Skibigårdsvej".into() },
                Correction { from: "ab".into(), to: "ok".into() },
            ]
        );
        // a span needs its words and at least one candidate
        assert_eq!(
            f.ambiguous,
            vec![
                Ambiguity { heard: "via".into(), candidates: vec!["Skibigårdsvej".into(), "Skibbyvej".into()] },
                Ambiguity { heard: "mikel".into(), candidates: vec!["Mikkel".into()] },
            ]
        );
        assert_eq!(
            f.ambiguous_json()[1],
            serde_json::json!({"heard": "mikel", "candidates": ["Mikkel"]})
        );
        // a count or a flag is not the contract; nothing at all; and a flood
        assert!(SttFacts::from_json(&serde_json::json!({"ambiguous": 3})).ambiguous.is_empty());
        assert!(SttFacts::from_json(&serde_json::json!({"ambiguous": true})).ambiguous.is_empty());
        assert_eq!(SttFacts::from_json(&serde_json::json!({"text": "x"})), SttFacts::default());
        let spans: Vec<_> = (0..10).map(|i| serde_json::json!({"heard": format!("h{i}"), "candidates": ["a", "b", "c", "d", "e", "f", "g"]})).collect();
        let f = SttFacts::from_json(&serde_json::json!({"ambiguous": spans}));
        assert_eq!((f.ambiguous.len(), f.ambiguous[0].candidates.len()), (MAX_AMBIGUOUS, MAX_CANDIDATES));
        let flood: Vec<_> = (0..100).map(|i| serde_json::json!({"from": format!("a{i}"), "to": "b"})).collect();
        let f = SttFacts::from_json(&serde_json::json!({"corrections": flood}));
        assert_eq!(f.corrections.len(), MAX_CORRECTIONS);
        assert_eq!(f.corrections_json()[0], serde_json::json!({"from": "a0", "to": "b"}));
    }

    #[test]
    fn one_turn_per_device() {
        let v = Voice::new(None, None).unwrap();
        assert!(v.claim("dev_a"));
        assert!(!v.claim("dev_a"));
        assert!(v.claim("dev_b"));
        v.release("dev_a");
        assert!(v.claim("dev_a"));
    }
}
