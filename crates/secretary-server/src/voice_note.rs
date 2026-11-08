//! Voice notes on the LucinaVoice socket (`docs/lucinavoice-protocol.md` section 10.4): the
//! multipart `POST /v1/share` of the recording, and the watcher that follows the share until
//! Lucina has answered. The turn itself is driven by [`crate::voice_ws`]; the headers and the
//! decoding are in [`crate::voice_frames`].
//!
//! - **Request.** `SECRETARY_VOICE_SHARE_URL` (default [`DEFAULT_SHARE_URL`]: Caddy's loopback
//!   site on p1, which checks the device token and proxies to lucina-rs), with the caller's own
//!   credentials exactly like the speech-to-text call (`Authorization` unchanged; a cookie
//!   caller: its `Cookie` plus `X-Secretary-CSRF: 1`). Fields: `shareId` (the turn id, a UUID),
//!   `intent=ask`, `lang`, `sessionId`, `channel`, `source`, `replyTo`, the `file`, and `meta`
//!   (`{"files":[{"index":0,"voice":true,"durationMs":…,"transcript"?:…}]}`).
//! - **Watcher.** `GET <share url>/<shareId>` every [`WATCH_EVERY`] for at most [`WATCH_FOR`];
//!   `done` sends `reply` and `done` with the `shareId`, `error` (or the end of the time) sends
//!   `error lucina_unavailable`. Nothing of the audio, the text or the credentials is logged.

use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use axum::http::{header, HeaderMap, StatusCode};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::AppState;

/// Caddy's loopback site on p1: forward_auth against secretary-server, then lucina-rs.
pub const DEFAULT_SHARE_URL: &str = "http://127.0.0.1:8801/v1/share";
/// How often the share's status is read.
pub const WATCH_EVERY: Duration = Duration::from_millis(250);
/// How long a note's answer is followed.
pub const WATCH_FOR: Duration = Duration::from_secs(180);
/// Answers followed at once per connection; the oldest is dropped beyond that.
pub const MAX_WATCHED: usize = 4;
/// One status read.
const POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// A UUID v4 in canonical form from 16 random bytes.
pub fn uuid_from(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A fresh turn id (UUID v4, which is a valid `shareId`).
pub fn new_turn_id() -> Result<String, crate::error::ApiError> {
    let mut r = [0u8; 16];
    getrandom::fill(&mut r)
        .map_err(|_| crate::error::ApiError::internal("random source failed"))?;
    Ok(uuid_from(r))
}

/// `voice-<UTC yyyymmddThhmmss>.wav`.
pub fn file_name(now: chrono::DateTime<chrono::Utc>) -> String {
    format!("voice-{}.wav", now.format("%Y%m%dT%H%M%S"))
}

/// A `multipart/form-data` body, built by hand (text fields and one file).
pub struct Multipart {
    boundary: String,
    body: Vec<u8>,
}

impl Multipart {
    pub fn new(boundary: String) -> Self {
        Self {
            boundary,
            body: Vec::new(),
        }
    }

    fn part_head(&mut self, disposition: &str, content_type: Option<&str>) {
        self.body.extend_from_slice(b"--");
        self.body.extend_from_slice(self.boundary.as_bytes());
        self.body
            .extend_from_slice(b"\r\nContent-Disposition: form-data; ");
        self.body.extend_from_slice(disposition.as_bytes());
        if let Some(ct) = content_type {
            self.body.extend_from_slice(b"\r\nContent-Type: ");
            self.body.extend_from_slice(ct.as_bytes());
        }
        self.body.extend_from_slice(b"\r\n\r\n");
    }

    /// A text field. `name` is one of this module's own constants.
    pub fn text(&mut self, name: &str, value: &str) {
        self.part_head(&format!("name=\"{name}\""), None);
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
    }

    /// A file part. `filename` is generated here (no quotes or control characters).
    pub fn file(&mut self, name: &str, filename: &str, content_type: &str, data: &[u8]) {
        self.part_head(
            &format!("name=\"{name}\"; filename=\"{filename}\""),
            Some(content_type),
        );
        self.body.extend_from_slice(data);
        self.body.extend_from_slice(b"\r\n");
    }

    /// The `Content-Type` header value and the body.
    pub fn finish(mut self) -> (String, Vec<u8>) {
        self.body.extend_from_slice(b"--");
        self.body.extend_from_slice(self.boundary.as_bytes());
        self.body.extend_from_slice(b"--\r\n");
        (
            format!("multipart/form-data; boundary={}", self.boundary),
            self.body,
        )
    }
}

/// What a note's share request carries.
pub struct NoteForm<'a> {
    pub share_id: &'a str,
    pub lang: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub channel: &'a str,
    pub source: &'a str,
    pub reply_to: Option<&'a str>,
    pub duration_ms: u64,
    pub transcript: Option<&'a str>,
    pub file_name: &'a str,
    /// The whole WAV file.
    pub wav: &'a [u8],
}

/// The `meta` field: `{"files":[{"index":0,"voice":true,"durationMs":…,"transcript"?:…}]}`.
pub fn meta_json(duration_ms: u64, transcript: Option<&str>) -> String {
    let mut f = json!({"index": 0, "voice": true, "durationMs": duration_ms});
    if let Some(t) = transcript.filter(|t| !t.is_empty()) {
        f["transcript"] = json!(t);
    }
    json!({"files": [f]}).to_string()
}

/// The multipart body of a note's `POST /v1/share`: its `Content-Type` and the bytes.
pub fn share_body(boundary: String, f: &NoteForm) -> (String, Vec<u8>) {
    let mut m = Multipart::new(boundary);
    m.text("shareId", f.share_id);
    m.text("intent", "ask");
    if let Some(l) = f.lang {
        m.text("lang", l);
    }
    if let Some(s) = f.session_id {
        m.text("sessionId", s);
    }
    m.text("channel", f.channel);
    m.text("source", f.source);
    if let Some(r) = f.reply_to {
        m.text("replyTo", r);
    }
    m.text("meta", &meta_json(f.duration_ms, f.transcript));
    m.file("file", f.file_name, "audio/wav", f.wav);
    m.finish()
}

/// A random multipart boundary.
pub fn new_boundary() -> String {
    let mut r = [0u8; 16];
    // A failing random source gives a fixed-looking boundary; the body is still valid unless the
    // audio held it, which random bytes make as unlikely as anything else.
    let _ = getrandom::fill(&mut r);
    format!(
        "----lucinavoice{}",
        r.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// Why a note was not posted.
#[derive(Debug)]
pub enum NoteError {
    /// The share could not be reached.
    Unreachable(&'static str),
    /// The share answered with this status.
    Refused(StatusCode),
}

impl NoteError {
    pub fn message(&self) -> String {
        match self {
            Self::Unreachable(why) => format!("the note could not be posted: {why}"),
            Self::Refused(s) => format!("the note was refused (HTTP {})", s.as_u16()),
        }
    }
}

/// What the share accepted.
pub struct Accepted {
    pub user_item_id: String,
}

/// `POST` the note. The caller has checked that notes are on.
pub async fn post_share(
    st: &AppState,
    creds: &HeaderMap,
    content_type: String,
    body: Vec<u8>,
) -> Result<Accepted, NoteError> {
    let Some(url) = st.voice.share_url() else {
        return Err(NoteError::Unreachable("voice notes are off"));
    };
    let req = st
        .voice
        .share_client()
        .post(url)
        .header(header::CONTENT_TYPE, content_type)
        .body(body);
    let resp = crate::voice::with_creds(req, creds)
        .send()
        .await
        .map_err(|e| {
            NoteError::Unreachable(if e.is_timeout() {
                "timed out"
            } else {
                "unreachable"
            })
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(NoteError::Refused(status));
    }
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    Ok(Accepted {
        user_item_id: v["userItemId"].as_str().unwrap_or("").to_owned(),
    })
}

/// What the watcher reports back to the socket.
fn text(v: &Value) -> Message {
    Message::Text(v.to_string().into())
}

/// Follow the share until Lucina has answered, then send `reply` and `done` (or `error`).
/// `ms` are measured from the end of the recording: `stt` (if the server transcribed) and
/// `note` (the share was accepted). Runs as a task owned by the connection.
#[allow(clippy::too_many_arguments)]
pub async fn watch(
    st: AppState,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    share_id: String,
    t0: Instant,
    stt_ms: Option<u64>,
    note_ms: u64,
    device: String,
) {
    let Some(base) = st.voice.share_url() else {
        return;
    };
    let url = format!("{}/{}", base.trim_end_matches('/'), share_id);
    let deadline = Instant::now() + WATCH_FOR;
    let ms_since = |t: Instant| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
    loop {
        let req = st.voice.share_client().get(&url).timeout(POLL_TIMEOUT);
        let mut stop = false;
        if let Ok(resp) = crate::voice::with_creds(req, &creds).send().await {
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                stop = true;
            } else if status.is_success() {
                let v: Value = resp.json().await.unwrap_or(Value::Null);
                match v["status"].as_str() {
                    Some("done") => {
                        let reply_ms = ms_since(t0);
                        let item = v["lucinaItemId"].as_str().map_or(Value::Null, |s| json!(s));
                        let reply = json!({
                            "type": "reply",
                            "shareId": share_id,
                            "text": v["reply"].as_str().unwrap_or(""),
                            "itemId": item,
                        });
                        let mut ms = json!({"note": note_ms, "reply": reply_ms});
                        if let Some(s) = stt_ms {
                            ms["stt"] = json!(s);
                        }
                        ms["total"] = json!(ms_since(t0));
                        tracing::info!(
                            device = %device,
                            note_ms,
                            reply_ms,
                            "voice note answered"
                        );
                        if tx.send(text(&reply)).await.is_err() {
                            return;
                        }
                        let _ = tx
                            .send(text(
                                &json!({"type": "done", "shareId": share_id, "ms": ms}),
                            ))
                            .await;
                        return;
                    }
                    Some("error") => {
                        tracing::warn!(device = %device, "voice note: lucina ended the share in error");
                        let _ = tx
                            .send(text(&json!({
                                "type": "error",
                                "code": "lucina_unavailable",
                                "shareId": share_id,
                                "message": "Lucina could not answer the note",
                            })))
                            .await;
                        return;
                    }
                    _ => {}
                }
            }
        }
        if stop || Instant::now() >= deadline {
            tracing::warn!(device = %device, stopped = stop, "voice note: no answer followed");
            let _ = tx
                .send(text(&json!({
                    "type": "error",
                    "code": "lucina_unavailable",
                    "shareId": share_id,
                    "message": "no answer to the note could be followed; it is in the chat",
                })))
                .await;
            return;
        }
        tokio::time::sleep(WATCH_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_id_is_a_uuid_v4_and_a_valid_share_id() {
        let id = uuid_from([0xff; 16]);
        assert_eq!(id, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let id = uuid_from([0; 16]);
        assert_eq!(id, "00000000-0000-4000-8000-000000000000");
        let id = new_turn_id().unwrap();
        // lucina-rs: 8..=64 of [A-Za-z0-9-].
        assert_eq!(id.len(), 36);
        assert!(id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        assert_eq!(id.as_bytes()[14], b'4');
        assert_ne!(id, new_turn_id().unwrap());
    }

    #[test]
    fn the_file_name_is_the_utc_time() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-11-08T09:05:03Z").unwrap();
        assert_eq!(file_name(t.to_utc()), "voice-20261108T090503.wav");
    }

    #[test]
    fn the_share_body_is_multipart() {
        let f = NoteForm {
            share_id: "abc",
            lang: Some("it"),
            session_id: None,
            channel: "ios",
            source: "ios-chat",
            reply_to: Some("itm_1"),
            duration_ms: 1500,
            transcript: Some("ciao"),
            file_name: "voice-x.wav",
            wav: &[1, 2, 3, 0xff],
        };
        let (ct, body) = share_body("BND".into(), &f);
        assert_eq!(ct, "multipart/form-data; boundary=BND");
        let text = String::from_utf8_lossy(&body).into_owned();
        assert!(text.starts_with(
            "--BND\r\nContent-Disposition: form-data; name=\"shareId\"\r\n\r\nabc\r\n"
        ));
        assert!(text.contains("name=\"intent\"\r\n\r\nask\r\n"));
        assert!(text.contains("name=\"lang\"\r\n\r\nit\r\n"));
        assert!(!text.contains("sessionId"));
        assert!(text.contains("name=\"replyTo\"\r\n\r\nitm_1\r\n"));
        assert!(text.contains(
            "name=\"file\"; filename=\"voice-x.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        ));
        assert!(text.ends_with("--BND--\r\n"));
        assert!(body.windows(6).any(|w| w == [b'\r', b'\n', 1, 2, 3, 0xff]));
        let meta = meta_json(1500, Some("ciao"));
        let v: Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(
            v,
            json!({"files": [{"index": 0, "voice": true, "durationMs": 1500, "transcript": "ciao"}]})
        );
        let v: Value = serde_json::from_str(&meta_json(7, Some(""))).unwrap();
        assert!(v["files"][0].get("transcript").is_none());
    }
}
