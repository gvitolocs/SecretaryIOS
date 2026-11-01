//! One live prompter connection.
//!
//! Client → server:
//! - text `{"type":"start","language":"auto|en|it|da","vocabulary":["Lucina","Jev"],"useJev":true}`;
//! - binary: 16 kHz mono signed 16-bit little-endian PCM, any chunk size;
//! - text `{"type":"points","section":"…","candidates":[{"id","text","kind"}]}` whenever
//!   the open points change (current section, missed row, next section);
//! - text `{"type":"stop"}`.
//!
//! Server → client:
//! - `{"type":"status","state":"loading|ready","engine":"gpu|cpu"}`;
//! - `{"type":"transcript","final":"…"|null,"partial":"…","startMs","audioMs","sttMs","language"}`;
//! - `{"type":"verdicts","phase":"local|jev","verdicts":[…],"audioMs","sttMs","judgeMs",
//!   "jev":{asked,ms,inputTokens,error},"jevCalls","jevFailed"}`;
//! - `{"type":"error","message":"…"}`.
//!
//! `audioMs` is the end of the audio the message is about, counted from the
//! first sample; the client knows when it sent that sample, so it shows the
//! real speech-to-tick latency.
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use lucina_prompter::{Candidate, JudgeRequest, JudgeResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::engine::{Mode, SttOut};
use super::segmenter::{Job, JobKind, Segmenter, RATE};
use crate::App;

/// A take longer than this is closed (the app reconnects for the next one).
const MAX_SESSION: Duration = Duration::from_secs(3 * 3600);
/// Text of the window the judge reads, and of the speech before it.
const WINDOW_CHARS: usize = 600;
const EARLIER_CHARS: usize = 1200;
/// Language detection needs this much speech before it is trusted for the take.
const LOCK_LANGUAGE_MS: u64 = 1500;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Control {
    #[serde(rename_all = "camelCase")]
    Start {
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        vocabulary: Vec<String>,
        #[serde(default = "yes")]
        use_jev: bool,
    },
    Points {
        #[serde(default)]
        section: String,
        #[serde(default)]
        candidates: Vec<Candidate>,
    },
    Stop,
}

fn yes() -> bool {
    true
}

/// `english` (whisper-server's verbose_json) → `en`, for the three languages he speaks.
fn lang_code(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => Some("en"),
        "it" | "italian" => Some("it"),
        "da" | "danish" => Some("da"),
        _ => None,
    }
}

/// Words that steer whisper's spelling (names, jargon), from what the app sends.
fn prompt_of(vocabulary: &[String]) -> String {
    let mut out = String::new();
    for w in vocabulary
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .take(40)
    {
        if out.len() + w.len() + 2 > 220 {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(w);
    }
    out
}

fn final_derailed(fin: &str, partial: &str) -> bool {
    let words = |s: &str| s.split_whitespace().count();
    words(partial) >= 6 && words(fin) * 2 < words(partial)
}

fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    s.chars().skip(n - max).collect()
}

struct SttDone {
    job: Job,
    out: Result<SttOut, String>,
}

struct JudgeDone {
    audio_ms: u64,
    stt_ms: u64,
    started: Instant,
    out: Result<JudgeResponse, String>,
}

struct Session {
    seg: Segmenter,
    stt_busy: bool,
    ready: bool,
    language: String,
    language_locked: bool,
    prompt: String,
    use_jev: bool,
    finals: Vec<String>,
    partial: String,
    section: String,
    candidates: Vec<Candidate>,
    judged: String,
    judge_busy: bool,
    pending: Option<(JudgeRequest, u64, u64)>,
    jev_calls: u64,
    jev_failed: u64,
}

impl Session {
    fn new() -> Self {
        Self {
            seg: Segmenter::new(),
            stt_busy: false,
            ready: false,
            language: "auto".into(),
            language_locked: false,
            prompt: String::new(),
            use_jev: true,
            finals: vec![],
            partial: String::new(),
            section: String::new(),
            candidates: vec![],
            judged: String::new(),
            judge_busy: false,
            pending: None,
            jev_calls: 0,
            jev_failed: 0,
        }
    }

    /// The judge's request for what was just heard, or None when nothing new.
    fn request(&mut self) -> Option<JudgeRequest> {
        if self.candidates.is_empty() {
            return None;
        }
        let last = self.finals.last().map(String::as_str).unwrap_or("");
        let window = tail(format!("{last} {}", self.partial).trim(), WINDOW_CHARS);
        let key = format!(
            "{window}\u{1}{}",
            self.candidates
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        if window.is_empty() || key == self.judged {
            return None;
        }
        self.judged = key;
        let before = self.finals.len().saturating_sub(1);
        let earlier = tail(&self.finals[..before].join(" "), EARLIER_CHARS);
        Some(JudgeRequest {
            window,
            earlier,
            section: self.section.clone(),
            candidates: self.candidates.clone(),
            use_jev: self.use_jev,
        })
    }
}

fn verdicts_msg(
    phase: &str,
    r: &JudgeResponse,
    audio_ms: u64,
    stt_ms: u64,
    judge_ms: u64,
    s: &Session,
) -> String {
    json!({
        "type": "verdicts",
        "phase": phase,
        "verdicts": r.verdicts,
        "audioMs": audio_ms,
        "sttMs": stt_ms,
        "judgeMs": judge_ms,
        "jev": r.jev,
        "jevCalls": s.jev_calls,
        "jevFailed": s.jev_failed,
    })
    .to_string()
}

pub async fn run(socket: WebSocket, app: Arc<App>) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let send = |v: Value| {
        let _ = out_tx.send(v.to_string());
    };
    let (stt_tx, mut stt_rx) = mpsc::unbounded_channel::<SttDone>();
    let (judge_tx, mut judge_rx) = mpsc::unbounded_channel::<JudgeDone>();
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<Result<Mode, String>>();
    let live = &app.prompter;
    let mut s = Session::new();
    let mut acquired = false;
    let deadline = tokio::time::sleep(MAX_SESSION);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let spawn_judge = |req: JudgeRequest, audio_ms: u64, stt_ms: u64| {
        let judge = live.judge.clone();
        let tx = judge_tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let out = judge.judge(&req).await;
            let _ = tx.send(JudgeDone {
                audio_ms,
                stt_ms,
                started,
                out,
            });
        });
    };

    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Binary(b) => {
                        if b.len() % 2 != 0 {
                            send(json!({"type": "error", "message": "audio must be 16-bit samples"}));
                            continue;
                        }
                        let pcm: Vec<i16> = b.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
                        s.seg.push(&pcm);
                    }
                    Message::Text(t) => match serde_json::from_str::<Control>(&t) {
                        Ok(Control::Start { language, vocabulary, use_jev }) => {
                            if let Some(l) = language.as_deref().and_then(lang_code) {
                                s.language = l.into();
                                s.language_locked = true;
                            }
                            s.prompt = prompt_of(&vocabulary);
                            s.use_jev = use_jev;
                            if !acquired {
                                acquired = true;
                                send(json!({"type": "status", "state": "loading"}));
                                let engine = live.engine.clone();
                                let tx = ready_tx.clone();
                                tokio::spawn(async move { let _ = tx.send(engine.acquire().await); });
                            }
                        }
                        Ok(Control::Points { section, candidates }) => {
                            s.section = section;
                            s.candidates = candidates;
                        }
                        Ok(Control::Stop) => break,
                        Err(e) => send(json!({"type": "error", "message": format!("bad message: {e}")})),
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            Some(r) = ready_rx.recv() => match r {
                Ok(mode) => {
                    s.ready = true;
                    send(json!({"type": "status", "state": "ready", "engine": mode}));
                }
                Err(e) => {
                    // acquire() already gave its slot back.
                    acquired = false;
                    send(json!({"type": "error", "message": e}));
                    break;
                }
            },
            Some(done) = stt_rx.recv() => {
                s.stt_busy = false;
                let SttDone { job, out } = done;
                let out = match out {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::warn!(error = %e, "prompter: speech-to-text failed for one window");
                        send(json!({"type": "error", "message": format!("speech-to-text: {e}")}));
                        continue;
                    }
                };
                let fin = job.kind == JobKind::Final;
                if !s.language_locked && fin && job.end_ms - job.start_ms >= LOCK_LANGUAGE_MS {
                    if let Some(l) = lang_code(&out.language) {
                        s.language = l.into();
                        s.language_locked = true;
                    }
                }
                let mut out = out;
                if fin {
                    // A final far shorter than the partial it replaces is whisper derailing
                    // (it happens on the closing pass): keep what the partial heard.
                    if final_derailed(&out.text, &s.partial) {
                        tracing::info!(final_text = %out.text, partial = %s.partial, "prompter: kept the partial over a derailed final");
                        out.text = std::mem::take(&mut s.partial);
                    }
                    s.partial.clear();
                    if !out.text.is_empty() {
                        s.finals.push(out.text.clone());
                    }
                } else {
                    s.partial = out.text.clone();
                }
                send(json!({
                    "type": "transcript",
                    "final": if fin && !out.text.is_empty() { Some(out.text.as_str()) } else { None },
                    "partial": s.partial,
                    "startMs": job.start_ms,
                    "audioMs": job.end_ms,
                    "sttMs": out.ms,
                    "language": lang_code(&out.language).unwrap_or(&s.language),
                }));
                if let Some(req) = s.request() {
                    let t = Instant::now();
                    let local = lucina_prompter::judge(&req, None).await;
                    send(serde_json::from_str(&verdicts_msg("local", &local, job.end_ms, out.ms, t.elapsed().as_millis() as u64, &s)).unwrap_or_default());
                    if req.use_jev {
                        if s.judge_busy {
                            s.pending = Some((req, job.end_ms, out.ms));
                        } else {
                            s.judge_busy = true;
                            spawn_judge(req, job.end_ms, out.ms);
                        }
                    }
                }
            }
            Some(done) = judge_rx.recv() => {
                s.judge_busy = false;
                let judge_ms = done.started.elapsed().as_millis() as u64;
                match done.out {
                    Ok(r) => {
                        if r.jev.asked > 0 {
                            if r.jev.error.is_none() { s.jev_calls += 1 } else { s.jev_failed += 1 }
                        }
                        send(serde_json::from_str(&verdicts_msg("jev", &r, done.audio_ms, done.stt_ms, judge_ms, &s)).unwrap_or_default());
                    }
                    Err(e) => {
                        s.jev_failed += 1;
                        tracing::warn!(error = %e, "prompter: judge unreachable, the matcher decides");
                        send(json!({"type": "judge", "error": e, "jevFailed": s.jev_failed}));
                    }
                }
                if let Some((req, a, st)) = s.pending.take() {
                    s.judge_busy = true;
                    spawn_judge(req, a, st);
                }
            }
            _ = tick.tick() => {
                if s.ready && !s.stt_busy {
                    if let Some(job) = s.seg.next_job() {
                        s.stt_busy = true;
                        let engine = live.engine.clone();
                        let tx = stt_tx.clone();
                        let (lang, prompt) = (s.language.clone(), s.prompt.clone());
                        tokio::spawn(async move {
                            let out = engine.transcribe(&job.audio, &lang, &prompt).await;
                            let _ = tx.send(SttDone { job, out });
                        });
                    }
                }
            }
            _ = &mut deadline => {
                send(json!({"type": "error", "message": "take too long; start a new one"}));
                break;
            }
        }
    }
    if acquired {
        // A start still loading answers on ready_rx; wait for it so the slot is given back once.
        if !s.ready {
            if let Some(Ok(_)) = ready_rx.recv().await {
                live.engine.release().await;
            }
        } else {
            live.engine.release().await;
        }
    }
    drop(out_tx);
    let _ = writer.await;
    tracing::info!(
        audio_s = s.seg.audio_ms() / 1000,
        finals = s.finals.len(),
        jev_calls = s.jev_calls,
        rate = RATE,
        "prompter: take ended"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_prompt() {
        assert_eq!(lang_code("english"), Some("en"));
        assert_eq!(lang_code(" Danish"), Some("da"));
        assert_eq!(lang_code("german"), None);
        let p = prompt_of(&["Lucina".into(), " Jev ".into(), "".into(), "Whisper".into()]);
        assert_eq!(p, "Lucina, Jev, Whisper");
        let long: Vec<String> = (0..100).map(|i| format!("Word{i}")).collect();
        assert!(prompt_of(&long).len() <= 220);
    }

    #[test]
    fn a_derailed_final_loses_to_the_partial() {
        let partial = "so the audio never leaves my machines then Jev decides which of my points";
        assert!(final_derailed("You're a good girl, Jev.", partial));
        assert!(!final_derailed(
            "So the audio never leaves my machines. Then Jev decides which of my points I covered.",
            partial
        ));
        assert!(
            !final_derailed("Hi.", "Hi there"),
            "short partials are not judged"
        );
    }

    #[test]
    fn requests_only_for_new_speech() {
        let mut s = Session::new();
        s.finals.push("hello there".into());
        assert!(s.request().is_none(), "no open points, no request");
        s.candidates = vec![Candidate {
            id: "a".into(),
            text: "Say hello".into(),
            kind: Default::default(),
        }];
        let r = s.request().unwrap();
        assert_eq!(r.window, "hello there");
        assert!(s.request().is_none(), "same words, same points");
        s.partial = "and welcome".into();
        assert_eq!(s.request().unwrap().window, "hello there and welcome");
        s.finals.push("hello there and welcome everyone".into());
        s.partial.clear();
        let r = s.request().unwrap();
        assert_eq!(r.earlier, "hello there");
        assert_eq!(r.window, "hello there and welcome everyone");
    }
}
