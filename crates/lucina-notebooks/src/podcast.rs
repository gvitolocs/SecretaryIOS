//! Podcasts from a notebook, after Open Notebook's podcast pipeline (episode and speaker
//! profiles, briefing → outline → dialogue → speech; Copyright (c) 2024 Luis Novo, MIT):
//!
//! 1. context: each source's Dense Summary (else the start of its text) and the notes;
//! 2. outline: JSON segments (name, description, size);
//! 3. script: the dialogue, segment by segment, as JSON lines `{speaker, text}`;
//! 4. voices: kokoro on nezopt, one voice per speaker, one WAV per line (kept, so a restart
//!    resumes where it stopped);
//! 5. mix: ffmpeg joins the lines with short pauses into `podcasts/<id>.mp3`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::llm::{Ask, Llm, Model, Turn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Speaker {
    pub name: String,
    pub voice: String,
    pub backstory: String,
    pub personality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerProfile {
    pub id: String,
    pub name: String,
    pub language: String,
    pub speakers: Vec<Speaker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EpisodeProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The default speaker profile.
    pub speaker_profile: String,
    pub briefing: String,
    pub segments: usize,
}

fn speaker(name: &str, voice: &str, backstory: &str, personality: &str) -> Speaker {
    Speaker { name: name.into(), voice: voice.into(), backstory: backstory.into(), personality: personality.into() }
}

pub fn speaker_profiles() -> Vec<SpeakerProfile> {
    vec![
        SpeakerProfile {
            id: "duo-en".into(),
            name: "Lucina and Marco (English)".into(),
            language: "English".into(),
            speakers: vec![
                speaker("Lucina", "af_heart", "Giuseppe's assistant; has read every source in the notebook closely.", "warm, precise, explains with concrete examples"),
                speaker("Marco", "am_michael", "A curious fellow student who asks what a classmate would ask.", "lively, asks follow-up questions, sums things up"),
            ],
        },
        SpeakerProfile {
            id: "duo-it".into(),
            name: "Lucina e Nicola (italiano)".into(),
            language: "Italian".into(),
            speakers: vec![
                speaker("Lucina", "if_sara", "L'assistente di Giuseppe; ha letto con attenzione tutte le fonti del notebook.", "calorosa, precisa, spiega con esempi concreti"),
                speaker("Nicola", "im_nicola", "Un compagno di corso curioso che fa le domande che farebbe uno studente.", "vivace, fa domande di approfondimento, riassume"),
            ],
        },
        SpeakerProfile {
            id: "solo-en".into(),
            name: "Lucina alone (English)".into(),
            language: "English".into(),
            speakers: vec![speaker("Lucina", "af_heart", "Giuseppe's assistant and tutor.", "clear, structured, encouraging")],
        },
        SpeakerProfile {
            id: "solo-it".into(),
            name: "Lucina da sola (italiano)".into(),
            language: "Italian".into(),
            speakers: vec![speaker("Lucina", "if_sara", "L'assistente e tutor di Giuseppe.", "chiara, ordinata, incoraggiante")],
        },
    ]
}

pub fn episode_profiles() -> Vec<EpisodeProfile> {
    vec![
        EpisodeProfile {
            id: "deep-dive".into(),
            name: "Deep dive".into(),
            description: "Two hosts talk through the notebook's main ideas.".into(),
            speaker_profile: "duo-en".into(),
            briefing: "A lively, accurate conversation that walks through the most important ideas in the sources, \
                       explains them with examples, and connects them. Stay faithful to the sources."
                .into(),
            segments: 4,
        },
        EpisodeProfile {
            id: "exam-review".into(),
            name: "Exam review".into(),
            description: "A study session: key definitions, typical exam questions and their answers.".into(),
            speaker_profile: "duo-en".into(),
            briefing: "An exam-preparation session. Cover the key definitions, the points most likely to be \
                       examined, common mistakes, and two or three practice questions with model answers."
                .into(),
            segments: 4,
        },
        EpisodeProfile {
            id: "tutor".into(),
            name: "Solo tutor".into(),
            description: "One voice explains the notebook like a short lecture.".into(),
            speaker_profile: "solo-en".into(),
            briefing: "A clear mini-lecture that explains the sources step by step, from the basics to the details."
                .into(),
            segments: 3,
        },
    ]
}

pub fn episode_profile(id: &str) -> Option<EpisodeProfile> {
    episode_profiles().into_iter().find(|p| p.id == id)
}

pub fn speaker_profile(id: &str) -> Option<SpeakerProfile> {
    speaker_profiles().into_iter().find(|p| p.id == id)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub name: String,
    pub description: String,
    pub size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub speaker: String,
    pub text: String,
}

#[async_trait]
pub trait Tts: Send + Sync {
    /// WAV audio of `text` in `voice`.
    async fn speak(&self, text: &str, voice: &str) -> anyhow::Result<Vec<u8>>;
}

/// kokoro-fastapi's OpenAI speech endpoint (`/v1/audio/speech`).
pub struct Kokoro {
    pub url: String,
    http: reqwest::Client,
}

impl Kokoro {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into().trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(300)).build().expect("reqwest client"),
        }
    }
}

#[async_trait]
impl Tts for Kokoro {
    async fn speak(&self, text: &str, voice: &str) -> anyhow::Result<Vec<u8>> {
        let r = self
            .http
            .post(format!("{}/v1/audio/speech", self.url))
            .json(&json!({ "model": "kokoro", "input": text, "voice": voice, "response_format": "wav", "speed": 1.0 }))
            .send()
            .await?;
        let status = r.status();
        let body = r.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("kokoro answered {status}: {}", String::from_utf8_lossy(&body).chars().take(200).collect::<String>());
        }
        Ok(body.to_vec())
    }
}

/// The first JSON object in a model's answer (models sometimes wrap it in a code fence).
pub fn json_object(s: &str) -> anyhow::Result<Value> {
    let start = s.find('{').ok_or_else(|| anyhow::anyhow!("no JSON in the answer"))?;
    let end = s.rfind('}').ok_or_else(|| anyhow::anyhow!("no JSON in the answer"))?;
    Ok(serde_json::from_str(&s[start..=end])?)
}

/// Asks for JSON and reads it with `read`; a broken answer is sent back with the error, up to
/// three tries in all (models sometimes leave a quote unescaped or stop mid-object).
pub async fn ask_json<T>(
    llm: &dyn Llm,
    mut ask: Ask,
    read: impl Fn(&Value) -> anyhow::Result<T>,
) -> anyhow::Result<(Value, T)> {
    let mut last = anyhow::anyhow!("no answer");
    for _ in 0..3 {
        let (out, _) = llm.complete(ask.clone(), None).await?;
        match json_object(&out).and_then(|v| read(&v).map(|t| (v, t))) {
            Ok(r) => return Ok(r),
            Err(e) => {
                tracing::warn!(error = %e, head = %out.chars().take(200).collect::<String>(), "notebooks: model JSON unreadable, asking again");
                ask.turns.push(Turn::assistant(out));
                ask.turns.push(Turn::user(format!(
                    "That answer is not valid JSON in the asked shape ({e}). Answer again with only the JSON object."
                )));
                last = e;
            }
        }
    }
    Err(last)
}

fn speakers_block(p: &SpeakerProfile) -> String {
    p.speakers
        .iter()
        .map(|s| format!("- **{}**: {} Personality: {}.", s.name, s.backstory, s.personality))
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn outline(
    llm: &dyn Llm,
    model: Model,
    context: &str,
    briefing: &str,
    profile: &SpeakerProfile,
    segments: usize,
) -> anyhow::Result<(String, Vec<Segment>)> {
    let prompt = format!(
        "You plan podcast episodes. Create an outline for an episode from this briefing and content.\n\n\
         <briefing>\n{briefing}\n</briefing>\n\n<context>\n{context}\n</context>\n\n<speakers>\n{}\n</speakers>\n\n\
         Write everything in {}. Make exactly {segments} segments that cover the briefing, flow logically, start \
         with a short introduction and end with a wrap-up. Each description says what is discussed and which \
         facts from the context to use. Segments are only topic markers: don't reintroduce speakers.\n\n\
         Answer with one JSON object with the keys \"title\" (the episode title) and \"segments\" (a list of \
         {segments} objects with the keys \"name\", \"description\" and \"size\"; size is exactly one of \"short\", \
         \"medium\" or \"long\"). For example: {{\"title\": \"Rogue servers on the LAN\", \"segments\": \
         [{{\"name\": \"Why it matters\", \"description\": \"What a rogue DHCP server can do.\", \"size\": \"medium\"}}]}}. \
         Write your own segments; no placeholders, no ellipses.",
        speakers_block(profile),
        profile.language
    );
    let ask = Ask {
        model,
        turns: vec![Turn::user(prompt)],
        json: model == Model::Deepseek,
        max_tokens: Some(2500),
        temperature: Some(0.6),
        purpose: "notebook_podcast_outline",
    };
    let (v, segs): (Value, Vec<Segment>) = ask_json(llm, ask, |v| Ok(serde_json::from_value(v["segments"].clone())?)).await?;
    if segs.is_empty() {
        anyhow::bail!("the outline has no segments");
    }
    let title = v["title"].as_str().unwrap_or("").trim().to_owned();
    Ok((title, segs))
}

/// Words for a segment of `size`, so the whole episode lasts about `minutes`.
pub fn segment_words(size: &str, segments: &[Segment], minutes: f64) -> usize {
    let weight = |s: &str| match s {
        "short" => 1.0,
        "long" => 2.5,
        _ => 1.7,
    };
    let total: f64 = segments.iter().map(|s| weight(&s.size)).sum::<f64>().max(1.0);
    // ~150 spoken words a minute.
    ((minutes * 150.0) * weight(size) / total).round().max(40.0) as usize
}

#[allow(clippy::too_many_arguments)]
pub async fn script_segment(
    llm: &dyn Llm,
    model: Model,
    context: &str,
    briefing: &str,
    profile: &SpeakerProfile,
    segments: &[Segment],
    index: usize,
    so_far: &[Line],
    minutes: f64,
) -> anyhow::Result<Vec<Line>> {
    let seg = &segments[index];
    let words = segment_words(&seg.size, segments, minutes);
    let names: Vec<&str> = profile.speakers.iter().map(|s| s.name.as_str()).collect();
    let recent: Vec<String> = so_far.iter().rev().take(6).rev().map(|l| format!("{}: {}", l.speaker, l.text)).collect();
    let outline: Vec<String> = segments.iter().enumerate().map(|(i, s)| format!("{}. {} — {}", i + 1, s.name, s.description)).collect();
    let prompt = format!(
        "You write natural podcast dialogue that is faithful to the sources.\n\n<briefing>\n{briefing}\n</briefing>\n\n\
         <context>\n{context}\n</context>\n\n<speakers>\n{}\n</speakers>\n\n<outline>\n{}\n</outline>\n\n\
         <dialogue_so_far>\n{}\n</dialogue_so_far>\n\n\
         Write segment {} (\"{}\": {}) in {} — about {words} words in total. {}\
         Use only these speaker names: {}. Spoken style: no Markdown, no stage directions, no sound effects, \
         spell out symbols and abbreviations the way a person would say them. Continue naturally from the \
         dialogue so far{}.\n\n\
         Answer with one JSON object: {{\"dialogue\": [{{\"speaker\": \"name\", \"text\": \"what they say\"}}]}}.",
        speakers_block(profile),
        outline.join("\n"),
        if recent.is_empty() { "(this is the start)".to_owned() } else { recent.join("\n") },
        index + 1,
        seg.name,
        seg.description,
        profile.language,
        if names.len() > 1 { "Alternate speakers; each turn is 1-4 sentences. " } else { "One speaker: short paragraphs. " },
        names.join(", "),
        if index + 1 == segments.len() { "; this is the last segment, so close the episode" } else { "" },
    );
    let ask = Ask {
        model,
        turns: vec![Turn::user(prompt)],
        json: model == Model::Deepseek,
        max_tokens: Some(4000),
        temperature: Some(0.7),
        purpose: "notebook_podcast_script",
    };
    let (_, lines): (Value, Vec<Line>) = ask_json(llm, ask, |v| Ok(serde_json::from_value(v["dialogue"].clone())?)).await?;
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|l| Line {
            // A name the profile doesn't have goes to the first speaker.
            speaker: if names.contains(&l.speaker.as_str()) { l.speaker } else { names[0].to_owned() },
            text: l.text.trim().to_owned(),
        })
        .filter(|l| !l.text.is_empty())
        .collect();
    if lines.is_empty() {
        anyhow::bail!("segment {} has no dialogue", index + 1);
    }
    Ok(lines)
}

/// Voices every line (skipping lines already in `work`) and mixes `out` (MP3). Returns its
/// duration in seconds.
pub async fn voice_and_mix(
    tts: &dyn Tts,
    ffmpeg: &Path,
    profile: &SpeakerProfile,
    lines: &[Line],
    work: &Path,
    out: &Path,
) -> anyhow::Result<f64> {
    tokio::fs::create_dir_all(work).await?;
    let mut files: Vec<PathBuf> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let f = work.join(format!("line-{i:04}.wav"));
        if tokio::fs::metadata(&f).await.map(|m| m.len() > 44).unwrap_or(false) {
            files.push(f);
            continue;
        }
        let voice = profile
            .speakers
            .iter()
            .find(|s| s.name == l.speaker)
            .unwrap_or(&profile.speakers[0])
            .voice
            .clone();
        let wav = tts.speak(&l.text, &voice).await?;
        let tmp = f.with_extension("wav.part");
        tokio::fs::write(&tmp, &wav).await?;
        tokio::fs::rename(&tmp, &f).await?;
        files.push(f);
    }
    let pause = work.join("pause.wav");
    run(ffmpeg, &[
        "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "anullsrc=r=24000:cl=mono", "-t", "0.35",
        "-c:a", "pcm_s16le", pause.to_str().unwrap_or_default(),
    ])
    .await?;
    let mut list = String::new();
    for (i, f) in files.iter().enumerate() {
        if i > 0 {
            list.push_str(&format!("file '{}'\n", pause.display()));
        }
        list.push_str(&format!("file '{}'\n", f.display().to_string().replace('\'', "'\\''")));
    }
    let list_path = work.join("list.txt");
    tokio::fs::write(&list_path, list).await?;
    let tmp = out.with_extension("mp3.part");
    if let Some(dir) = out.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    // Lines may differ in sample rate: resample everything to one stream.
    run(ffmpeg, &[
        "-nostdin", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i", list_path.to_str().unwrap_or_default(),
        "-ar", "24000", "-ac", "1", "-c:a", "libmp3lame", "-b:a", "96k", "-f", "mp3", tmp.to_str().unwrap_or_default(),
    ])
    .await?;
    tokio::fs::rename(&tmp, out).await?;
    let mut secs = 0.0;
    for f in &files {
        secs += wav_seconds(&tokio::fs::read(f).await?).unwrap_or(0.0);
    }
    Ok(secs + 0.35 * files.len().saturating_sub(1) as f64)
}

async fn run(bin: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = tokio::process::Command::new("nice")
        .arg("-n")
        .arg("10")
        .arg(bin)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>());
    }
    Ok(())
}

/// The length of a PCM WAV (from its `fmt ` and `data` chunks).
pub fn wav_seconds(b: &[u8]) -> Option<f64> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut byte_rate = 0u32;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        if id == b"fmt " && i + 20 <= b.len() {
            byte_rate = u32::from_le_bytes([b[i + 16], b[i + 17], b[i + 18], b[i + 19]]);
        }
        if id == b"data" {
            if byte_rate == 0 {
                return None;
            }
            // Streamed WAVs may say 0xFFFFFFFF: use what is there.
            let size = size.min(b.len() - i - 8);
            return Some(size as f64 / byte_rate as f64);
        }
        i += 8 + size + (size & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::fake::Scripted;

    fn wav(seconds: f64) -> Vec<u8> {
        let rate = 24000u32;
        let n = (seconds * rate as f64) as u32 * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&n.to_le_bytes());
        b.resize(b.len() + n as usize, 0);
        b
    }

    #[test]
    fn profiles_are_consistent() {
        for e in episode_profiles() {
            assert!(speaker_profile(&e.speaker_profile).is_some(), "{}", e.id);
        }
        assert!(speaker_profiles().iter().all(|p| !p.speakers.is_empty()));
    }

    #[test]
    fn wav_length() {
        assert!((wav_seconds(&wav(1.5)).unwrap() - 1.5).abs() < 0.01);
        assert_eq!(wav_seconds(b"nope"), None);
    }

    #[test]
    fn json_in_fences() {
        assert_eq!(json_object("```json\n{\"a\": 1}\n```").unwrap()["a"], 1);
        assert!(json_object("no").is_err());
    }

    #[test]
    fn words_follow_minutes_and_sizes() {
        let segs: Vec<Segment> = ["short", "long", "short"]
            .iter()
            .map(|s| Segment { name: "x".into(), description: "y".into(), size: s.to_string() })
            .collect();
        let total: usize = segs.iter().map(|s| segment_words(&s.size, &segs, 3.0)).sum();
        assert!((400..=500).contains(&total), "{total}");
        assert!(segment_words("long", &segs, 3.0) > segment_words("short", &segs, 3.0));
    }

    #[tokio::test]
    async fn outline_and_script_parse() {
        let llm = Scripted::new(&[
            r#"{"title":"Snooping","segments":[{"name":"Intro","description":"d","size":"short"},{"name":"End","description":"d","size":"short"}]}"#,
            "{\"dialogue\": [{\"speaker\": \"Lucina\", \"text\": \"broken \"quote\"}]}",
            r#"{"dialogue":[{"speaker":"Lucina","text":"Hi."},{"speaker":"Bob","text":"Hello."},{"speaker":"Marco","text":" "}]}"#,
        ]);
        let p = speaker_profile("duo-en").unwrap();
        let (title, segs) = outline(&llm, Model::Deepseek, "ctx", "brief", &p, 2).await.unwrap();
        assert_eq!(title, "Snooping");
        assert_eq!(segs.len(), 2);
        let lines = script_segment(&llm, Model::Deepseek, "ctx", "brief", &p, &segs, 0, &[], 2.0).await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].speaker, "Lucina", "unknown names go to the first speaker");
    }

    struct Silence;
    #[async_trait]
    impl Tts for Silence {
        async fn speak(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<u8>> {
            Ok(wav(0.5))
        }
    }

    #[tokio::test]
    async fn voices_and_mixes_when_ffmpeg_is_there() {
        let ffmpeg = PathBuf::from("/usr/bin/ffmpeg");
        if !ffmpeg.exists() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let p = speaker_profile("duo-en").unwrap();
        let lines = vec![
            Line { speaker: "Lucina".into(), text: "One".into() },
            Line { speaker: "Marco".into(), text: "Two".into() },
        ];
        let out = d.path().join("p.mp3");
        let secs = voice_and_mix(&Silence, &ffmpeg, &p, &lines, &d.path().join("w"), &out).await.unwrap();
        assert!((secs - 1.35).abs() < 0.01, "{secs}");
        assert!(std::fs::metadata(&out).unwrap().len() > 1000);
    }
}
