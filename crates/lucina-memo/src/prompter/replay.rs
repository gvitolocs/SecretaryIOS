//! Replays a recorded take against a live prompter socket, like the app would:
//! streams a 16 kHz WAV in real time, keeps the open points up to date with the
//! app's rules (done at 0.75, a next-section point at 0.85 moves the take on
//! and turns the open points into missed ones), and reports every tick with its
//! latency. The end-to-end test (`tests/prompter_e2e.rs`) uses it.
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use lucina_prompter::{Candidate, Kind, DONE_P};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use super::segmenter::RATE;

/// A next-section point needs this much to move the take on (the app's rule).
pub const NEXT_DONE_P: f64 = 0.85;

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub points: Vec<(String, String)>,
}

/// `## Section` headings and `- point` lines; ids `s<i>p<j>` like the app.
pub fn parse_script(md: &str) -> Vec<Section> {
    let mut out: Vec<Section> = vec![];
    for line in md.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(h) = line.strip_prefix('#') {
            out.push(Section {
                title: h.trim_start_matches('#').trim().into(),
                points: vec![],
            });
            continue;
        }
        let text = line.trim_start_matches(['-', '*', '•']).trim();
        if out.is_empty() {
            out.push(Section {
                title: "Points".into(),
                points: vec![],
            });
        }
        let s = out.len() - 1;
        let id = format!("s{s}p{}", out[s].points.len());
        out[s].points.push((id, text.into()));
    }
    out
}

#[derive(Debug, Clone)]
pub struct Tick {
    pub id: String,
    pub audio_ms: u64,
    pub latency_ms: u64,
    pub phase: String,
    pub p: f64,
}

#[derive(Debug, Default)]
pub struct Report {
    pub ticks: Vec<Tick>,
    pub missed: Vec<String>,
    pub transcript: Vec<String>,
    pub engine: String,
    pub jev_calls: u64,
    pub jev_failed: u64,
}

struct State {
    sections: Vec<Section>,
    current: usize,
    done: Vec<String>,
    missed: Vec<String>,
}

impl State {
    fn text(&self, id: &str) -> String {
        self.sections
            .iter()
            .flat_map(|s| &s.points)
            .find(|(i, _)| i == id)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    }
    fn candidates(&self) -> Vec<Candidate> {
        let open = |s: &Section, kind: Kind| -> Vec<Candidate> {
            s.points
                .iter()
                .filter(|(id, _)| !self.done.contains(id))
                .map(|(id, text)| Candidate {
                    id: id.clone(),
                    text: text.clone(),
                    kind,
                })
                .collect()
        };
        let mut c = open(&self.sections[self.current], Kind::Current);
        c.extend(
            self.missed
                .iter()
                .filter(|id| !self.done.contains(id))
                .map(|id| Candidate {
                    id: id.clone(),
                    text: self.text(id),
                    kind: Kind::Missed,
                }),
        );
        if let Some(next) = self.sections.get(self.current + 1) {
            c.extend(open(next, Kind::Next));
        }
        c
    }
    fn points_msg(&self) -> String {
        json!({"type": "points", "section": self.sections[self.current].title, "candidates": self.candidates()}).to_string()
    }
    /// Apply verdicts; true when the open points changed.
    fn apply(&mut self, verdicts: &[Value]) -> Vec<(String, f64)> {
        let mut ticked = vec![];
        let next: Vec<String> = self
            .sections
            .get(self.current + 1)
            .map(|s| s.points.iter().map(|(i, _)| i.clone()).collect())
            .unwrap_or_default();
        for v in verdicts {
            let (Some(id), Some(p)) = (v["id"].as_str(), v["p"].as_f64()) else {
                continue;
            };
            if self.done.iter().any(|d| d == id) {
                continue;
            }
            let need = if next.iter().any(|n| n == id) {
                NEXT_DONE_P
            } else {
                DONE_P
            };
            if p >= need {
                self.done.push(id.into());
                self.missed.retain(|m| m != id);
                ticked.push((id.to_owned(), p));
            }
        }
        if ticked.iter().any(|(id, _)| next.contains(id)) {
            let open: Vec<String> = self.sections[self.current]
                .points
                .iter()
                .map(|(i, _)| i.clone())
                .filter(|i| !self.done.contains(i))
                .collect();
            self.missed.extend(open);
            self.current += 1;
        }
        ticked
    }
}

/// 16 kHz mono 16-bit PCM samples of a WAV file (the `data` chunk).
pub fn read_wav(bytes: &[u8]) -> anyhow::Result<Vec<i16>> {
    anyhow::ensure!(
        bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "not a WAV file"
    );
    let mut i = 12;
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into()?) as usize;
        let body = &bytes[i + 8..(i + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into()?);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            anyhow::ensure!(
                rate == RATE as u32 && channels == 1 && bits == 16,
                "need 16 kHz mono 16-bit, got {rate} Hz {channels} ch {bits} bit"
            );
            return Ok(body
                .as_chunks::<2>().0.iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect());
        }
        i += 8 + len + (len & 1);
    }
    anyhow::bail!("WAV without a data chunk")
}

/// Stream `pcm` to `url` (ws:// or wss://) at real-time speed and report the ticks.
pub async fn replay(
    url: &str,
    bearer: Option<&str>,
    script: &str,
    pcm: &[i16],
    use_jev: bool,
    vocabulary: &[String],
) -> anyhow::Result<Report> {
    let mut req = url.into_client_request()?;
    if let Some(t) = bearer {
        req.headers_mut()
            .insert("authorization", format!("Bearer {t}").parse()?);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req).await?;
    let (mut tx, mut rx) = ws.split();
    let mut st = State {
        sections: parse_script(script),
        current: 0,
        done: vec![],
        missed: vec![],
    };
    anyhow::ensure!(!st.sections.is_empty(), "the script has no points");
    let mut report = Report::default();
    tx.send(Message::text(
        json!({"type": "start", "language": "auto", "vocabulary": vocabulary, "useJev": use_jev})
            .to_string(),
    ))
    .await?;
    tx.send(Message::text(st.points_msg())).await?;
    // Wait for the engine before the clock starts, like the app (which shows "loading").
    loop {
        let m = tokio::time::timeout(Duration::from_secs(60), rx.next())
            .await?
            .ok_or_else(|| anyhow::anyhow!("closed while loading"))??;
        let v: Value = serde_json::from_str(m.to_text().unwrap_or("{}")).unwrap_or_default();
        match v["type"].as_str() {
            Some("status") if v["state"] == "ready" => {
                report.engine = v["engine"].as_str().unwrap_or_default().into();
                break;
            }
            Some("error") => anyhow::bail!("server: {}", v["message"]),
            _ => {}
        }
    }
    let chunk = RATE / 10; // 100 ms, like the app's worklet
    let t0 = Instant::now();
    let mut sent = 0usize;
    // The tail: 2.5 s of silence so the last words are closed as a final.
    let silence = vec![0i16; RATE * 5 / 2];
    let all: Vec<i16> = pcm.iter().copied().chain(silence).collect();
    let end_wait = Duration::from_secs(6);
    let mut finished_at: Option<Instant> = None;
    loop {
        let due =
            t0 + Duration::from_millis((sent / RATE * 1000 + (sent % RATE) * 1000 / RATE) as u64);
        tokio::select! {
            _ = tokio::time::sleep_until(due.into()), if sent < all.len() => {
                let n = chunk.min(all.len() - sent);
                let bytes: Vec<u8> = all[sent..sent + n].iter().flat_map(|s| s.to_le_bytes()).collect();
                tx.send(Message::binary(bytes)).await?;
                sent += n;
                if sent == all.len() { finished_at = Some(Instant::now()); }
            }
            _ = tokio::time::sleep_until((finished_at.unwrap_or_else(Instant::now) + end_wait).into()), if finished_at.is_some() => break,
            m = rx.next() => {
                let Some(m) = m else { break };
                let m = m?;
                let Ok(text) = m.to_text() else { continue };
                let v: Value = serde_json::from_str(text).unwrap_or_default();
                match v["type"].as_str() {
                    Some("transcript") => {
                        if let Some(f) = v["final"].as_str() { report.transcript.push(f.into()); }
                    }
                    Some("verdicts") => {
                        report.jev_calls = v["jevCalls"].as_u64().unwrap_or(report.jev_calls);
                        report.jev_failed = v["jevFailed"].as_u64().unwrap_or(report.jev_failed);
                        let audio_ms = v["audioMs"].as_u64().unwrap_or(0);
                        let latency_ms = t0.elapsed().as_millis().saturating_sub(audio_ms as u128) as u64;
                        let ticked = st.apply(v["verdicts"].as_array().map(Vec::as_slice).unwrap_or(&[]));
                        if !ticked.is_empty() {
                            for (id, p) in ticked {
                                report.ticks.push(Tick { id, audio_ms, latency_ms, phase: v["phase"].as_str().unwrap_or("").into(), p });
                            }
                            if st.current >= st.sections.len() { st.current = st.sections.len() - 1; }
                            tx.send(Message::text(st.points_msg())).await?;
                        }
                    }
                    Some("judge") => report.jev_failed = v["jevFailed"].as_u64().unwrap_or(report.jev_failed),
                    Some("error") => tracing::warn!(message = %v["message"], "prompter replay: server error"),
                    _ => {}
                }
            }
        }
    }
    let _ = tx.send(Message::text(r#"{"type":"stop"}"#)).await;
    // Open points of the last section count as missed too.
    let open: Vec<String> = st.sections[st.current]
        .points
        .iter()
        .map(|(i, _)| i.clone())
        .filter(|i| !st.done.contains(i))
        .collect();
    report.missed = st
        .missed
        .iter()
        .filter(|m| !st.done.contains(m))
        .cloned()
        .chain(open)
        .collect();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_ids_follow_sections() {
        let s = parse_script("## Intro\n- One\n- Two\n\n# Next\n* Three\nloose line\n");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].points[1], ("s0p1".into(), "Two".into()));
        assert_eq!(s[1].points[1], ("s1p1".into(), "loose line".into()));
        assert_eq!(parse_script("- a")[0].title, "Points");
    }

    #[test]
    fn moving_on_turns_open_points_into_missed_ones() {
        let mut st = State {
            sections: parse_script("## A\n- a0\n- a1\n## B\n- b0\n- b1"),
            current: 0,
            done: vec![],
            missed: vec![],
        };
        st.apply(&[json!({"id": "s0p0", "p": 0.9})]);
        assert_eq!(st.done, ["s0p0"]);
        // A next-section point at 0.8 is not enough to move on.
        st.apply(&[json!({"id": "s1p0", "p": 0.8})]);
        assert_eq!(st.current, 0);
        st.apply(&[json!({"id": "s1p0", "p": 0.9})]);
        assert_eq!(st.current, 1);
        assert_eq!(st.missed, ["s0p1"]);
        let c = st.candidates();
        assert!(c.iter().any(|c| c.id == "s0p1" && c.kind == Kind::Missed));
        // Saying a missed point later still ticks it and clears it from the row.
        st.apply(&[json!({"id": "s0p1", "p": 0.8})]);
        assert!(st.missed.is_empty());
    }

    #[test]
    fn reads_16k_mono_wav() {
        let w = super::super::engine::wav(&[5, -5]);
        let pcm = read_wav(&w).unwrap();
        assert_eq!(&pcm[..2], &[5, -5]);
        assert!(read_wav(b"nope").is_err());
    }
}
