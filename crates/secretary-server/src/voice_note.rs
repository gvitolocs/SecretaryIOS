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
//! - **Watcher.** By event, never by fast polling (each `GET <share url>/<shareId>` goes through
//!   Caddy's forward_auth and counts against the device's request budget): Lucina's timeline stream
//!   is opened first with the server's own credentials, ONE status read catches an answer written
//!   before that, then the item that replies to the note's `userItemId` is the answer; at the
//!   deadline ([`WATCH_FOR`]) one last status read. A dropped stream is reopened once, then the
//!   status is read every [`FALLBACK_EVERY`]. An answer sends `reply` and `done` with the
//!   `shareId`; status `error` (or the end of the time) sends `error lucina_unavailable`. Nothing of the audio, the text or the credentials is logged.

use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use axum::http::{header, HeaderMap, StatusCode};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::notify::ingest::SseParser;
use crate::AppState;

/// Caddy's loopback site on p1: forward_auth against secretary-server, then lucina-rs.
pub const DEFAULT_SHARE_URL: &str = "http://127.0.0.1:8801/v1/share";
/// How often the share's status is read when there is no timeline stream to follow.
pub const FALLBACK_EVERY: Duration = Duration::from_secs(5);
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

/// What one read of the share's status says.
enum Status {
    /// Lucina answered: her text and timeline id.
    Done(String, Value),
    /// The share ended in `error`.
    Failed,
    /// Not yet (or not readable).
    Pending,
}

/// ONE `GET <share url>/<id>` through Caddy (it counts against the device's request budget, so the
/// watcher makes few of them).
async fn read_status(st: &AppState, creds: &HeaderMap, url: &str) -> Status {
    let req = st.voice.share_client().get(url).timeout(POLL_TIMEOUT);
    let Ok(resp) = crate::voice::with_creds(req, creds).send().await else {
        return Status::Pending;
    };
    if !resp.status().is_success() {
        return Status::Pending;
    }
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    match v["status"].as_str() {
        Some("done") => Status::Done(
            v["reply"].as_str().unwrap_or("").to_owned(),
            v["lucinaItemId"].as_str().map_or(Value::Null, |s| json!(s)),
        ),
        Some("error") => Status::Failed,
        _ => Status::Pending,
    }
}

/// Lucina's timeline stream, opened with the server's own credentials (no device budget).
struct Feed {
    parser: SseParser,
    body: futures_util::stream::BoxStream<'static, reqwest::Result<axum::body::Bytes>>,
    last: Option<String>,
}

impl Feed {
    async fn open(st: &AppState, last: Option<&str>) -> Option<Self> {
        let resp = st.lucina().ok()?.timeline_stream(last).await.ok()?;
        Some(Self {
            parser: SseParser::default(),
            body: resp.bytes_stream().boxed(),
            last: last.map(str::to_owned),
        })
    }

    /// The next item that answers `user_item_id` (role `lucina`, `replyTo` that id): its text and
    /// id. `None` when the stream ended or failed.
    async fn next_answer(&mut self, user_item_id: &str) -> Option<(String, Value)> {
        loop {
            let chunk = self.body.next().await?.ok()?;
            for ev in self.parser.push(&chunk) {
                if ev.id.is_some() {
                    self.last = ev.id.clone();
                }
                if ev.event != "item" {
                    continue;
                }
                let Ok(item) = serde_json::from_str::<Value>(&ev.data) else {
                    continue;
                };
                let reply_to = match &item["replyTo"] {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => continue,
                };
                if item["role"] == "lucina" && reply_to == user_item_id {
                    let id = match &item["id"] {
                        Value::String(s) => json!(s),
                        Value::Number(n) => json!(n.to_string()),
                        _ => Value::Null,
                    };
                    return Some((item["text"].as_str().unwrap_or("").to_owned(), id));
                }
            }
        }
    }
}

/// What the watcher knows of the note, to report with.
struct Report {
    tx: mpsc::Sender<Message>,
    share_id: String,
    t0: Instant,
    stt_ms: Option<u64>,
    note_ms: u64,
    device: String,
}

impl Report {
    fn ms_since(&self) -> u64 {
        u64::try_from(self.t0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    async fn answered(&self, reply_text: &str, item_id: Value) {
        let reply_ms = self.ms_since();
        let reply = json!({
            "type": "reply",
            "shareId": self.share_id,
            "text": reply_text,
            "itemId": item_id,
        });
        let mut ms = json!({"note": self.note_ms, "reply": reply_ms});
        if let Some(s) = self.stt_ms {
            ms["stt"] = json!(s);
        }
        ms["total"] = json!(self.ms_since());
        tracing::info!(device = %self.device, note_ms = self.note_ms, reply_ms, "voice note answered");
        if self.tx.send(text(&reply)).await.is_err() {
            return;
        }
        let _ = self
            .tx
            .send(text(&json!({"type": "done", "shareId": self.share_id, "ms": ms})))
            .await;
    }

    async fn failed(&self, message: &str) {
        tracing::warn!(device = %self.device, "voice note: no answer");
        let _ = self
            .tx
            .send(text(&json!({
                "type": "error",
                "code": "lucina_unavailable",
                "shareId": self.share_id,
                "message": message,
            })))
            .await;
    }

    /// Act on a status read; `true` when the note is settled.
    async fn settle(&self, s: Status) -> bool {
        match s {
            Status::Done(t, id) => {
                self.answered(&t, id).await;
                true
            }
            Status::Failed => {
                self.failed("Lucina could not answer the note").await;
                true
            }
            Status::Pending => false,
        }
    }
}

/// Follow the share until Lucina has answered, then send `reply` and `done` (or `error`). It
/// follows by event, not by polling: Lucina's timeline stream is opened first, then ONE status
/// read catches an answer written before it opened, and the answer is the item that replies to
/// `user_item_id`. At the deadline one last status read. A stream that drops is reopened once with
/// its last event id; after that the status is read every [`FALLBACK_EVERY`]. `ms` are measured
/// from the end of the recording: `stt` (if the server transcribed) and `note` (the share was
/// accepted). Runs as a task owned by the connection.
#[allow(clippy::too_many_arguments)]
pub async fn watch(
    st: AppState,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    share_id: String,
    user_item_id: String,
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
    let report = Report {
        tx,
        share_id,
        t0,
        stt_ms,
        note_ms,
        device,
    };
    // 1. The stream first (an answer after this point is not missed), 2. one status read.
    let mut feed = if user_item_id.is_empty() {
        None
    } else {
        Feed::open(&st, None).await
    };
    if report.settle(read_status(&st, &creds, &url).await).await {
        return;
    }
    let mut reopened = feed.is_none();
    // 3. Wait.
    loop {
        let now = Instant::now();
        if now >= deadline {
            // 4. One last status read, then give up.
            if !report.settle(read_status(&st, &creds, &url).await).await {
                report
                    .failed("no answer to the note could be followed; it is in the chat")
                    .await;
            }
            return;
        }
        let left = deadline - now;
        match feed.as_mut() {
            Some(f) => match tokio::time::timeout(left, f.next_answer(&user_item_id)).await {
                Err(_) => {} // the deadline; the next turn of the loop ends it
                Ok(Some((t, id))) => {
                    report.answered(&t, id).await;
                    return;
                }
                Ok(None) => {
                    let last = f.last.clone();
                    feed = if reopened {
                        None
                    } else {
                        reopened = true;
                        Feed::open(&st, last.as_deref()).await
                    };
                }
            },
            None => {
                tokio::time::sleep(FALLBACK_EVERY.min(left)).await;
                if report.settle(read_status(&st, &creds, &url).await).await {
                    return;
                }
            }
        }
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
