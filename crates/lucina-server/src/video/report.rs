//! The report: a model (Gemini, then Lucina's DeepSeek) writes the summary, the key points,
//! the glossary, the code shown on screen and the visual notes as JSON; the Markdown around
//! it is laid out here, with timestamps that open the video at that point. It is saved as a
//! Keep note labelled "YouTube" (with the merged transcript) and summarised in the chat.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::archive::Entry;
use super::gemini::{self, Part};
use super::transcript::Transcript;
use super::{Frame, MergedSeg, Seg, Skipped, VideoConfig, VideoMeta, ZcodeResult};
use crate::AppState;

pub struct Input<'a> {
    pub meta: &'a VideoMeta,
    pub request: &'a str,
    pub merged: &'a [MergedSeg],
    pub frames: &'a [Frame],
    pub zcode: &'a ZcodeResult,
    pub skipped: &'a [Skipped],
    pub folder: &'a Path,
    pub whisper_engine: &'a str,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timed {
    #[serde(default)]
    pub t: Option<f64>,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Term {
    pub term: String,
    #[serde(default)]
    pub meaning: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Code {
    #[serde(default)]
    pub t: Option<f64>,
    #[serde(default)]
    pub language: String,
    pub text: String,
}

/// What the model writes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub key_points: Vec<Timed>,
    #[serde(default)]
    pub glossary: Vec<Term>,
    #[serde(default)]
    pub code: Vec<Code>,
    #[serde(default)]
    pub visual_notes: Vec<Timed>,
    /// Which model wrote it ("" = no model: the transcript only).
    #[serde(skip)]
    pub model: String,
}

pub struct Saved {
    pub slug: String,
}

pub fn hms(sec: f64) -> String {
    let s = sec.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `[m:ss](…&t=Ns)`: opens the video there.
fn stamp(meta: &VideoMeta, t: f64) -> String {
    format!(
        "[{}](https://www.youtube.com/watch?v={}&t={}s)",
        hms(t),
        meta.id,
        t.max(0.0).floor() as u64
    )
}

/// The note store's slugs are lower-case letters, digits and single dashes: `FBs--2nSPLQ`
/// becomes `youtube-fbs-2nsplq`.
pub fn note_slug(id: &str) -> String {
    let mut s = String::from("youtube");
    for part in id
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
    {
        s.push('-');
        s.push_str(part);
    }
    s
}

/// The answer when this video already has a report.
pub fn already_done(e: &Entry) -> (String, String) {
    let slug = e.note_slug.clone().unwrap_or_default();
    (
        format!(
            "I already made a report for “{}”: [note:{slug}]. Say “again” if you want a new one.",
            e.title
        ),
        format!("existing report {slug}"),
    )
}

/// The transcripts next to the video: each source as JSON, the merged one as JSON and Markdown.
pub fn write_transcripts(
    folder: &Path,
    meta: &VideoMeta,
    whisper: &Transcript,
    zcode: &ZcodeResult,
    captions: &[Seg],
    merged: &[MergedSeg],
) -> anyhow::Result<()> {
    let put = |name: &str, v: &Value| -> anyhow::Result<()> {
        lucina_store::write_atomic(
            &folder.join(name),
            serde_json::to_string_pretty(v)?.as_bytes(),
        )
        .with_context(|| format!("writing {name}"))
    };
    if !whisper.segments.is_empty() {
        put(
            "transcript.whisper.json",
            &json!({"engine": whisper.engine, "language": whisper.language, "segments": whisper.segments}),
        )?;
    }
    if !zcode.transcript.is_empty() {
        put(
            "transcript.zcode.json",
            &json!({"segments": zcode.transcript}),
        )?;
    }
    if !captions.is_empty() {
        put("transcript.captions.json", &json!({"segments": captions}))?;
    }
    put("transcript.json", &json!({"segments": merged}))?;
    let mut md = format!("# {} — transcript\n\n{}\n\n", meta.title, meta.webpage_url);
    for s in merged {
        md.push_str(&format!("[{}] {}  \n", hms(s.start), s.text));
    }
    lucina_store::write_atomic(&folder.join("transcript.md"), md.as_bytes())
        .context("writing transcript.md")
}

const SYSTEM: &str = "You write a report on a video for its owner, a software engineer. You \
get the video's details, the owner's request, a timestamped transcript (merged from Whisper, \
ZCode and captions), timestamped keyframe notes (vision model + OCR) and ZCode's own visual \
notes. Write in English, or in Danish when the owner's request is in Danish (never Italian), but keep every \
technical term, identifier, command, number and unit exactly as the video says or shows it. \
Do not invent anything that is not in the material. Answer with JSON: \
{\"summary\":\"3-6 sentences\",\
\"key_points\":[{\"t\":<seconds>,\"text\":\"…\"}],\
\"glossary\":[{\"term\":\"…\",\"meaning\":\"one or two sentences, for this video's context\"}],\
\"code\":[{\"t\":<seconds>,\"language\":\"rust|bash|…\",\"text\":\"verbatim code or command shown on screen\"}],\
\"visual_notes\":[{\"t\":<seconds>,\"text\":\"what is shown that the speech does not say\"}]}. \
Key points: the substance, in order, each with the second it starts. Glossary: every \
unusual or technical term (the \"awkward\" ones), with what it means here. Code: only what \
is actually on screen; [] if none.";

fn material(input: &Input<'_>) -> String {
    let m = input.meta;
    let mut s = format!(
        "Owner's request: {}\n\nVideo: {}\nChannel: {}\nURL: {}\nDuration: {}\nUploaded: {}\nTags: {}\nChapters: {}\nDescription:\n{}\n\n",
        input.request,
        m.title,
        m.channel,
        m.webpage_url,
        hms(m.duration),
        m.upload_date.as_deref().unwrap_or("?"),
        m.tags.join(", "),
        m.chapters
            .iter()
            .map(|c| format!("{} {}", hms(c.start), c.title))
            .collect::<Vec<_>>()
            .join("; "),
        m.description.chars().take(3000).collect::<String>(),
    );
    s.push_str("Transcript:\n");
    for seg in input.merged {
        s.push_str(&format!("[{:.0}s] {}\n", seg.start, seg.text));
    }
    s.push_str("\nKeyframes:\n");
    for f in input.frames {
        s.push_str(&format!("[{:.0}s] ({}) {}", f.t, f.kind, f.description));
        if !f.ocr.is_empty() {
            s.push_str(&format!(
                " | OCR: {}",
                f.ocr.chars().take(400).collect::<String>()
            ));
        }
        if let Some(c) = &f.code {
            s.push_str(&format!(" | code: {c}"));
        }
        s.push('\n');
    }
    if !input.zcode.visual.is_empty() || !input.zcode.summary.is_empty() {
        s.push_str("\nZCode's notes:\n");
        for v in &input.zcode.visual {
            s.push_str(&format!(
                "[{:.0}s] {} {}\n",
                v.t, v.description, v.on_screen_text
            ));
        }
        if !input.zcode.terms.is_empty() {
            s.push_str(&format!("Terms: {}\n", input.zcode.terms.join(", ")));
        }
        if !input.zcode.summary.is_empty() {
            s.push_str(&format!("Summary: {}\n", input.zcode.summary));
        }
    }
    // A 3-hour talk: keep the request well inside the models' context.
    s.chars().take(400_000).collect()
}

fn parse(v: Value, model: &str) -> anyhow::Result<Report> {
    let mut r: Report = serde_json::from_value(v).context("report JSON has the wrong shape")?;
    anyhow::ensure!(!r.summary.trim().is_empty(), "the report has no summary");
    r.model = model.to_owned();
    Ok(r)
}

/// Gemini, then Lucina's own model; with neither, a report with the transcript only.
pub async fn write(st: &AppState, cfg: &VideoConfig, input: &Input<'_>) -> anyhow::Result<Report> {
    let text = material(input);
    let parts = [Part::Text(text.clone())];
    let gemini_err = match gemini::generate_json(
        &st.tool_env.http,
        cfg,
        SYSTEM,
        &parts,
        Duration::from_secs(300),
    )
    .await
    .and_then(|v| parse(v, &cfg.gemini_model))
    {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    tracing::warn!(error = %gemini_err, "video report: Gemini failed; asking Lucina's model");
    let req = lucina_llm::ChatRequest {
        messages: vec![
            lucina_llm::Message::system(SYSTEM),
            lucina_llm::Message::user(text.chars().take(200_000).collect::<String>()),
        ],
        temperature: Some(0.2),
        max_tokens: Some(6000),
        purpose: "youtube_report",
        json_object: true,
        ..Default::default()
    };
    match st.model.complete(req).await {
        Ok(resp) => {
            let model = resp.model.clone();
            if let Ok(r) = gemini::parse_json(resp.message.text()).and_then(|v| parse(v, &model)) {
                return Ok(r);
            }
        }
        Err(e) => tracing::warn!(error = %e, "video report: Lucina's model failed too"),
    }
    Ok(Report {
        summary: format!(
            "I could not get a model to write the report ({gemini_err:#}); here is the transcript."
        ),
        ..Report::default()
    })
}

/// The note's Markdown.
pub fn markdown(input: &Input<'_>, rep: &Report) -> String {
    let m = input.meta;
    let mut md = format!(
        "**Video:** [{}]({}) · {} · {}{}\n**Archive:** `{}`\n\n## Summary\n\n{}\n",
        m.title,
        m.webpage_url,
        m.channel,
        hms(m.duration),
        m.upload_date
            .as_deref()
            .filter(|d| d.len() == 8)
            .map(|d| format!(" · uploaded {}-{}-{}", &d[..4], &d[4..6], &d[6..]))
            .unwrap_or_default(),
        input.folder.display(),
        rep.summary.trim(),
    );
    if !rep.key_points.is_empty() {
        md.push_str("\n## Key points\n\n");
        for p in &rep.key_points {
            match p.t {
                Some(t) => md.push_str(&format!("- {} {}\n", stamp(m, t), p.text.trim())),
                None => md.push_str(&format!("- {}\n", p.text.trim())),
            }
        }
    }
    if !rep.glossary.is_empty() {
        md.push_str("\n## Technical terms\n\n");
        for g in &rep.glossary {
            md.push_str(&format!("- **{}**: {}\n", g.term.trim(), g.meaning.trim()));
        }
    }
    if !rep.code.is_empty() {
        md.push_str("\n## Code and commands on screen\n");
        for c in &rep.code {
            let at =
                c.t.map(|t| format!(" (at {})", stamp(m, t)))
                    .unwrap_or_default();
            md.push_str(&format!(
                "\n{at}\n```{}\n{}\n```\n",
                c.language.trim(),
                c.text.trim_end()
            ));
        }
    }
    if !rep.visual_notes.is_empty() {
        md.push_str("\n## Visual notes\n\n");
        for v in &rep.visual_notes {
            match v.t {
                Some(t) => md.push_str(&format!("- {} {}\n", stamp(m, t), v.text.trim())),
                None => md.push_str(&format!("- {}\n", v.text.trim())),
            }
        }
    }
    if !input.merged.is_empty() {
        md.push_str("\n## Transcript\n\n");
        let mut chars = 0;
        for s in input.merged {
            let line = format!("{} {}  \n", stamp(m, s.start), s.text);
            chars += line.len();
            // The note store takes 256 KB; the whole transcript stays in the archive.
            if chars > 180_000 {
                md.push_str("\n… (the full transcript is `transcript.md` in the archive folder)\n");
                break;
            }
            md.push_str(&line);
        }
    }
    md.push_str("\n## How this was made\n\n");
    let from = |src: &str| input.merged.iter().filter(|s| s.source == src).count();
    md.push_str(&format!(
        "- Transcript: {} windows (Whisper {}, ZCode {}, captions {}, combined {}); Whisper engine: {}\n",
        input.merged.len(),
        from("whisper"),
        from("zcode"),
        from("captions"),
        from("both"),
        if input.whisper_engine.is_empty() { "did not run" } else { input.whisper_engine },
    ));
    md.push_str(&format!(
        "- Keyframes: {} (scene changes + samples, OCR, vision)\n- ZCode: {}\n- Report written by: {}\n",
        input.frames.len(),
        if input.zcode.transcript.is_empty() && input.zcode.visual.is_empty() {
            "no result".to_owned()
        } else {
            format!(
                "{} transcript segments, {} visual notes{}",
                input.zcode.transcript.len(),
                input.zcode.visual.len(),
                input.zcode.session.as_deref().map(|s| format!(" (session {s})")).unwrap_or_default()
            )
        },
        if rep.model.is_empty() { "no model" } else { &rep.model },
    ));
    for s in input.skipped {
        md.push_str(&format!("- Skipped {}: {}\n", s.step, s.reason));
    }
    md
}

/// Saves the report as a Keep note labelled "YouTube" (a new slug when the video already has
/// a note, e.g. on "again").
pub async fn save_note(st: &AppState, input: &Input<'_>, rep: &Report) -> anyhow::Result<Saved> {
    use lucina_tools::Tool;
    let content = markdown(input, rep);
    lucina_store::write_atomic(&input.folder.join("report.md"), content.as_bytes())
        .context("writing report.md")?;
    let ctx = lucina_tools::Ctx::new(st.tool_env.clone(), "app", "youtube-report")
        .with_origin(lucina_tools::Origin::Owner);
    let base = note_slug(&input.meta.id);
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M").to_string();
    for slug in [base.clone(), format!("{base}-{stamp}")] {
        let args = json!({
            "action": "create",
            "slug": slug,
            "title": format!("{} (YouTube)", input.meta.title),
            "content": content,
            "labels": ["YouTube"],
        });
        match tokio::time::timeout(
            Duration::from_secs(60),
            lucina_tools::tools::notes::NotesTool.call(args, &ctx),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(Saved { slug }),
            Ok(Err(e)) if e.to_string().contains("exists") => continue,
            Ok(Err(e)) => anyhow::bail!("{e}"),
            Err(_) => anyhow::bail!("the note store did not answer in 60 s"),
        }
    }
    anyhow::bail!("a note for this video exists under both {base} and {base}-{stamp}")
}

/// The chat answer (and the job's result line).
pub fn chat_reply(input: &Input<'_>, rep: &Report, saved: &Saved) -> (String, String) {
    let m = input.meta;
    let mut text = format!(
        "📺 **{}** ({}, {})\n\n{}\n",
        m.title,
        m.channel,
        hms(m.duration),
        rep.summary.trim()
    );
    if !rep.key_points.is_empty() {
        text.push('\n');
        for p in rep.key_points.iter().take(6) {
            let at = p.t.map(|t| format!("{} ", hms(t))).unwrap_or_default();
            text.push_str(&format!("• {at}{}\n", p.text.trim()));
        }
    }
    if !rep.glossary.is_empty() {
        let terms: Vec<&str> = rep
            .glossary
            .iter()
            .take(12)
            .map(|g| g.term.trim())
            .collect();
        text.push_str(&format!("\nTerms explained: {}.\n", terms.join(", ")));
    }
    if !input.skipped.is_empty() {
        let steps: Vec<&str> = input.skipped.iter().map(|s| s.step.as_str()).collect();
        text.push_str(&format!(
            "\n(Not done: {}; the note says why.)\n",
            steps.join(", ")
        ));
    }
    text.push_str(&format!(
        "\nFull report, transcript and frames: [note:{}]",
        saved.slug
    ));
    let result = format!("youtube report {} saved as note {}", m.id, saved.slug);
    (text, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> VideoMeta {
        VideoMeta {
            id: "FBs--2nSPLQ".into(),
            title: "I challenged Rust Developers".into(),
            channel: "Arjay McCandless".into(),
            upload_date: Some("20261015".into()),
            duration: 98.0,
            webpage_url: "https://www.youtube.com/shorts/FBs--2nSPLQ".into(),
            ..Default::default()
        }
    }

    #[test]
    fn times_and_links() {
        assert_eq!(hms(0.4), "0:00");
        assert_eq!(hms(98.2), "1:38");
        assert_eq!(hms(3723.0), "1:02:03");
        assert_eq!(
            stamp(&meta(), 36.7),
            "[0:37](https://www.youtube.com/watch?v=FBs--2nSPLQ&t=36s)"
        );
        assert_eq!(note_slug("FBs--2nSPLQ"), "youtube-fbs-2nsplq");
        assert_eq!(note_slug("_a-B_c"), "youtube-a-b-c");
    }

    #[test]
    fn the_model_answer_becomes_the_note() {
        let rep = parse(
            json!({"summary":"An io_uring server reached 64k users.",
                   "key_points":[{"t":36,"text":"io_uring"},{"text":"no time"}],
                   "glossary":[{"term":"io_uring","meaning":"Linux async I/O with shared rings"}],
                   "code":[], "visual_notes":[{"t":"bad","text":"x"}]}),
            "gemini-3.8-flash",
        );
        // A non-numeric time is a shape error: the caller falls back to the next model.
        assert!(rep.is_err());
        let rep = parse(
            json!({"summary":"An io_uring server reached 64k users.",
                   "key_points":[{"t":36,"text":"io_uring"},{"text":"no time"}],
                   "glossary":[{"term":"io_uring","meaning":"Linux async I/O with shared rings"}],
                   "code":[{"t":50,"language":"rust","text":"fn main() {}"}]}),
            "gemini-3.8-flash",
        )
        .unwrap();
        let merged = vec![MergedSeg {
            start: 0.0,
            end: 5.0,
            text: "I challenged Rust developers".into(),
            source: "both".into(),
        }];
        let m = meta();
        let skipped = vec![Skipped {
            step: "ZCode".into(),
            reason: "quota".into(),
        }];
        let zc = ZcodeResult::default();
        let input = Input {
            meta: &m,
            request: "report",
            merged: &merged,
            frames: &[],
            zcode: &zc,
            skipped: &skipped,
            folder: Path::new(
                "/mybook/lucina-videos/2026/2026-10-28 I challenged Rust Developers [FBs--2nSPLQ]",
            ),
            whisper_engine: "whisper.cpp large-v3 (GPU)",
        };
        let md = markdown(&input, &rep);
        assert!(md.contains("· uploaded 2026-10-15"), "{md}");
        assert!(md.contains("- [0:36](https://www.youtube.com/watch?v=FBs--2nSPLQ&t=36s) io_uring"));
        assert!(md.contains("- no time\n"));
        assert!(md.contains("- **io_uring**: Linux async I/O"));
        assert!(md.contains("```rust\nfn main() {}\n```"));
        assert!(md.contains("Whisper 0, ZCode 0, captions 0, combined 1"));
        assert!(md.contains("- Skipped ZCode: quota"));
        let (chat, result) = chat_reply(
            &input,
            &rep,
            &Saved {
                slug: "youtube-fbs--2nsplq".into(),
            },
        );
        assert!(chat.ends_with("[note:youtube-fbs--2nsplq]"), "{chat}");
        assert!(chat.contains("• 0:36 io_uring") && chat.contains("Not done: ZCode"));
        assert_eq!(
            result,
            "youtube report FBs--2nSPLQ saved as note youtube-fbs--2nsplq"
        );
    }
}
