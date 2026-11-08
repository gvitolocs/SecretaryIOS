//! The voice socket turn (docs/lucinavoice-protocol.md, sections 2 and 10).

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

use crate::pace;
use crate::turn::TurnResult;

/// Defaults for the wait of `listening` and for the read after `end`.
const LISTENING_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const NOTE_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Precomputed audio frames and turn keys.
#[derive(Debug, Clone)]
pub struct WsConfig {
    pub purpose: String, // "command" | "note"
    pub codec: String, // "pcm16" | "ima-adpcm"
    /// IMA-ADPCM only: rate and block (bytes) from the WAV header.
    pub adpcm_rate: Option<u32>,
    pub adpcm_block: Option<u16>,
    pub lang: Option<String>,
    pub thread: Option<String>,
    pub channel: Option<String>,
    pub source: Option<String>,
    /// --text: also send the reply as text.
    pub text: bool,
    /// --no-speak: no speech at all.
    pub no_speak: bool,
    /// Audio bytes per frame; frames are sent at t_start + k * frame_ms.
    pub frame_ms: u64,
    pub realtime: bool,
    /// The whole clip, cut into frames (binary messages).
    pub frames: Vec<Message>,
    /// Duration of the audio (ms).
    pub speech_ms: u64,
}

impl WsConfig {
    /// The `start` message: only the keys the user gave, plus purpose/codec;
    /// `mode` is always `ptt`. IMA-ADPCM adds `rate` and `block`.
    pub fn start_message(&self) -> Value {
        let mut v = json!({ "type": "start", "mode": "ptt" });
        v["purpose"] = json!(self.purpose);
        v["codec"] = json!(self.codec);
        if let Some(r) = self.adpcm_rate {
            v["rate"] = json!(r);
        }
        if let Some(b) = self.adpcm_block {
            v["block"] = json!(b);
        }
        if let Some(lang) = &self.lang {
            v["lang"] = json!(lang);
        }
        if let Some(thread) = &self.thread {
            v["thread"] = json!(thread);
        }
        if let Some(channel) = &self.channel {
            v["channel"] = json!(channel);
        }
        if let Some(source) = &self.source {
            v["source"] = json!(source);
        }
        if self.text {
            v["text"] = json!(true);
        }
        if self.no_speak {
            v["speak"] = json!(false);
        }
        v
    }
}

/// Connect (TCP + TLS + upgrade) with the device token as `Authorization: Bearer`.
/// Returns the elapsed ms and the stream.
pub async fn connect(url: &str, token: &str) -> Result<(u64, WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>)> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
    let t0 = Instant::now();
    let (stream, _resp) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .with_context(|| format!("connecting to {url}"))?
        .context("WebSocket upgrade failed")?;
    Ok((t0.elapsed().as_millis() as u64, stream))
}

/// Run one turn on an already-open socket: `start`, audio frames, `end`,
/// then read until `done` or `error`. t0 is when `end` was written.
/// A socket-level failure (or timeout) is `fatal`: do not run another turn
/// on that socket.
pub async fn turn<S>(ws: &mut WebSocketStream<S>, cfg: &WsConfig) -> Result<(TurnResult, bool)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut r = TurnResult {
        mode: "ws",
        purpose: Some(cfg.purpose.clone()),
        codec: Some(cfg.codec.clone()),
        connect_ms: None,
        speech_ms: cfg.speech_ms,
        t: BTreeMap::new(),
        server_ms: None,
        up_bytes: 0,
        down_bytes: 0,
        transcript_chars: 0,
        reply_chars: 0,
        error: None,
        server_section10: None,
        transcript: None,
        reply: None,
        audio: Vec::new(),
        audio_rate: 0,
    };

    // start
    let start = cfg.start_message();
    let start_text = start.to_string();
    ws.send(Message::text(start_text.clone())).await?;
    r.up_bytes += start_text.len() as u64;

    // wait for listening
    let listening = tokio::time::timeout(LISTENING_TIMEOUT, async {
        loop {
            match ws.next().await.transpose()? {
                Some(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("listening") {
                            return Ok::<_, anyhow::Error>(v.get("codec").is_some());
                        }
                    }
                }
                Some(_) => {}
                None => bail!("socket closed while waiting for listening"),
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timeout waiting for listening"))?
    .with_context(|| "waiting for listening")?;
    r.server_section10 = Some(listening);

    // audio frames, paced: frame k leaves at t_start + k * frame_ms
    let t_start = Instant::now();
    for (k, frame) in cfg.frames.iter().enumerate() {
        if cfg.realtime {
            let deadline = pace::frame_deadline(t_start, cfg.frame_ms, k as u64);
            let now = Instant::now();
            if deadline > now {
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            }
        }
        ws.send(frame.clone()).await?;
        r.up_bytes += message_len(frame) as u64;
    }

    // end; t0 is when it was written
    ws.send(Message::text(r#"{"type":"end"}"#)).await?;
    r.up_bytes += 15;
    let t0 = Instant::now();

    // read until done or error
    let limit = if cfg.purpose == "note" { NOTE_TIMEOUT } else { COMMAND_TIMEOUT };
    let mut note_share: Option<String> = None;
    let mut audio_rate: Option<u32> = None;
    let wait = tokio::time::timeout(limit, async {
        loop {
            match ws.next().await.transpose()? {
                Some(Message::Text(t)) => {
                    r.down_bytes += t.len() as u64;
                    let Ok(v) = serde_json::from_str::<Value>(&t) else {
                        continue; // non-JSON text: ignore
                    };
                    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let now = t0.elapsed().as_millis() as u64;
                    match ty {
                        "transcript" => {
                            r.t.insert("transcript".into(), now);
                            if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                                r.transcript = Some(text.to_string());
                                r.transcript_chars = text.chars().count() as u32;
                            }
                        }
                        "delta" => {
                            r.t.entry("first_delta".into()).or_insert(now);
                        }
                        "answer" => {
                            r.t.entry("answer".into()).or_insert(now);
                        }
                        "audio" => {
                            audio_rate = Some(
                                v.get("rate").and_then(|r| r.as_u64()).unwrap_or(24000) as u32,
                            );
                        }
                        "reply" => {
                            r.t.entry("reply".into()).or_insert(now);
                            if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                                r.reply = Some(text.to_string());
                                r.reply_chars = text.chars().count() as u32;
                            }
                        }
                        "note" => {
                            r.t.entry("note".into()).or_insert(now);
                            note_share = v
                                .get("shareId")
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string());
                        }
                        "done" => {
                            // After `note`, messages carry a shareId; only the
                            // done of this note ends the wait.
                            if let (Some(id), Some(want)) = (
                                v.get("shareId").and_then(|s| s.as_str()),
                                note_share.as_deref(),
                            ) {
                                if id != want {
                                    continue;
                                }
                            }
                            r.t.insert("done".into(), now);
                            if v.get("ms").is_some() {
                                r.server_ms = v.get("ms").cloned();
                            }
                            return Ok(());
                        }
                        "error" => {
                            r.t.insert("error".into(), now);
                            r.error = Some(
                                v.get("code")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("unknown")
                                    .to_string(),
                            );
                            return Ok(());
                        }
                        _ => {} // endpoint, listening again, …
                    }
                }
                Some(Message::Binary(b)) => {
                    r.down_bytes += b.len() as u64;
                    if audio_rate.is_some() {
                        r.t.entry("first_audio".into()).or_insert(t0.elapsed().as_millis() as u64);
                        if b.len() % 2 == 0 {
                            r.audio.extend_from_slice(&b);
                        }
                    }
                }
                // Pings: tungstenite answers them at the protocol level.
                Some(Message::Ping(_)) | Some(Message::Pong(_)) | Some(Message::Close(_)) | Some(Message::Frame(_)) => {}
                None => bail!("socket closed before done or error"),
            }
        }
    })
    .await;
    let fatal = match wait {
        Ok(Ok(())) => false,
        Ok(Err(e)) => {
            r.error = Some(e.to_string());
            true
        }
        Err(_) => {
            r.error = Some("timeout".into());
            true
        }
    };

    r.audio_rate = audio_rate.unwrap_or(0);
    Ok((r, fatal))
}

fn message_len(m: &Message) -> usize {
    match m {
        Message::Text(t) => t.len(),
        Message::Binary(b) => b.len(),
        _ => 0,
    }
}
