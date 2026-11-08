//! The old share path (docs/share.md): POST /v1/share, then poll GET /v1/share/{id}.
//!
//! This measures the "before" number: the recording is uploaded whole at the
//! end of speech, and the old clients poll every 2 s.

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::pace;
use crate::turn::TurnResult;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct ShareConfig {
    /// e.g. `https://getsecretary.tech`
    pub base_url: String,
    pub token: String,
    /// The WAV to upload (as-is, filename `voice-probe.wav`).
    pub wav: Vec<u8>,
    /// Its duration in ms (for `meta`).
    pub duration_ms: u64,
    pub lang: Option<String>,
    pub source: Option<String>,
}

/// One share turn. t_rec_end is the moment the request starts (the recording
/// would have ended then).
pub async fn run(cfg: &ShareConfig) -> Result<TurnResult> {
    let client = reqwest::Client::new();
    let share_id = Uuid::new_v4().to_string();
    let meta = serde_json::json!({
        "files": [{ "index": 0, "voice": true, "durationMs": cfg.duration_ms }]
    });

    let mut form = reqwest::multipart::Form::new();
    form = form.text("shareId", share_id.clone());
    form = form.text("intent", "ask");
    if let Some(lang) = &cfg.lang {
        form = form.text("lang", lang.clone());
    }
    if let Some(source) = &cfg.source {
        form = form.text("source", source.clone());
    }
    form = form.text("meta", meta.to_string());
    let file = reqwest::multipart::Part::bytes(cfg.wav.clone())
        .file_name("voice-probe.wav")
        .mime_str("audio/wav")?;
    form = form.part("file", file);

    // The recording ends here.
    let t_rec_end = Instant::now();
    let url = format!("{}/v1/share", cfg.base_url.trim_end_matches('/'));
    let up_bytes = cfg.wav.len() as u64;

    let mut down_bytes = 0u64;
    let mut t = BTreeMap::new();
    let mut reply: Option<String> = None;
    let mut error: Option<String> = None;

    let resp = match client
        .post(&url)
        .bearer_auth(&cfg.token)
        .multipart(form)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            t.insert("accepted".into(), t_rec_end.elapsed().as_millis() as u64);
            bail!("POST {url}: {e}");
        }
    };
    let accepted = t_rec_end.elapsed().as_millis() as u64;
    t.insert("accepted".into(), accepted);
    let status = resp.status();
    let body: Vec<u8> = resp.bytes().await?.to_vec();
    down_bytes += body.len() as u64;
    if !status.is_success() {
        error = Some(format!("http_{status}"));
        return Ok(result(cfg, up_bytes, down_bytes, t, reply, error));
    }

    // Poll every 250 ms until done or error (old clients poll every 2 s).
    let get_url = format!("{}/v1/share/{share_id}", cfg.base_url.trim_end_matches('/'));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            error = Some("timeout".into());
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        let resp = match client.get(&get_url).bearer_auth(&cfg.token).send().await {
            Ok(r) => r,
            Err(e) => {
                bail!("GET {get_url}: {e}");
            }
        };
        let status = resp.status();
        let body: Vec<u8> = resp.bytes().await?.to_vec();
        down_bytes += body.len() as u64;
        if !status.is_success() {
            bail!("GET {get_url}: http_{status}");
        }
        let v: Value = serde_json::from_slice(&body)?;
        match v.get("status").and_then(|s| s.as_str()) {
            Some("done") => {
                let reply_ms = t_rec_end.elapsed().as_millis() as u64;
                t.insert("reply".into(), reply_ms);
                t.insert(
                    "reply_at_2s_poll".into(),
                    pace::reply_at_2s_poll(reply_ms),
                );
                reply = v.get("reply").and_then(|t| t.as_str()).map(|s| s.to_string());
                break;
            }
            Some("error") => {
                t.insert("error".into(), t_rec_end.elapsed().as_millis() as u64);
                error = Some(
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("share_error")
                        .to_string(),
                );
                break;
            }
            _ => {} // processing (or a stage)
        }
    }

    Ok(result(cfg, up_bytes, down_bytes, t, reply, error))
}

fn result(
    cfg: &ShareConfig,
    up_bytes: u64,
    down_bytes: u64,
    t: BTreeMap<String, u64>,
    reply: Option<String>,
    error: Option<String>,
) -> TurnResult {
    TurnResult {
        mode: "share",
        purpose: None,
        codec: None,
        connect_ms: None,
        speech_ms: cfg.duration_ms,
        t,
        server_ms: None,
        up_bytes,
        down_bytes,
        transcript_chars: 0,
        reply_chars: reply.as_ref().map(|r| r.chars().count() as u32).unwrap_or(0),
        error,
        server_section10: None,
        transcript: None,
        reply,
        audio: Vec::new(),
        audio_rate: 0,
    }
}
