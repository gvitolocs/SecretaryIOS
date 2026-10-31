//! The Whisper transcript (lucina-memo's whisper.cpp path, GPU first) and YouTube's own
//! captions (VTT), both as timed [`Seg`]s.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;

use super::{thermal, Seg, VideoConfig, VideoMeta};

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub segments: Vec<Seg>,
    /// "whisper.cpp large-v3 (GPU)", or empty when Whisper did not run.
    pub engine: String,
    pub language: Option<String>,
}

/// What Whisper is primed with: names and jargon the audio probably contains. whisper.cpp
/// keeps the last ~224 tokens of a prompt, so this stays short.
pub fn whisper_prompt(meta: &VideoMeta) -> String {
    let mut p = format!("{}.", meta.title.trim().trim_end_matches('.'));
    if !meta.channel.is_empty() {
        p.push_str(&format!(" {}.", meta.channel));
    }
    let tags: Vec<&str> = meta
        .tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .take(20)
        .collect();
    if !tags.is_empty() {
        p.push_str(&format!(" {}.", tags.join(", ")));
    }
    let chapters: Vec<&str> = meta
        .chapters
        .iter()
        .map(|c| c.title.trim())
        .take(15)
        .collect();
    if !chapters.is_empty() {
        p.push_str(&format!(" {}.", chapters.join(", ")));
    }
    p.chars().take(600).collect()
}

/// The video's audio through Whisper.
pub async fn whisper(
    cfg: &VideoConfig,
    meta: &VideoMeta,
    video: &Path,
    work: &Path,
) -> anyhow::Result<Transcript> {
    thermal::wait_cool(&cfg.thermal).await;
    let wav = work.join("audio.wav");
    let mut cmd = thermal::command(&cfg.thermal, &cfg.ffmpeg);
    cmd.args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(video)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&wav);
    thermal::output(cmd, Duration::from_secs(30 * 60))
        .await
        .context("ffmpeg (audio)")?;
    let whisper_dir = work.join("whisper");
    tokio::fs::create_dir_all(&whisper_dir).await?;
    let t = lucina_memo::transcribe::transcribe_prompted(
        &cfg.whisper,
        &wav,
        &whisper_dir,
        meta.duration,
        cfg.whisper.gpu_wait,
        &whisper_prompt(meta),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = tokio::fs::remove_file(&wav).await;
    Ok(Transcript {
        segments: t
            .segments
            .into_iter()
            .map(|s| Seg {
                start: s.start,
                end: s.end,
                text: s.text.trim().to_owned(),
            })
            .filter(|s| !s.text.is_empty())
            .collect(),
        engine: t.engine,
        language: Some(t.language).filter(|l| !l.is_empty()),
    })
}

fn vtt_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (
            h.parse::<f64>().ok()?,
            m.parse::<f64>().ok()?,
            s.parse::<f64>().ok()?,
        ),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s.parse::<f64>().ok()?),
        _ => return None,
    };
    Some(h * 3600.0 + m * 60.0 + sec)
}

fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_tag = false;
    for c in line.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A WebVTT file as segments. YouTube's auto captions roll: each cue repeats the previous
/// line before adding the new words, and 10 ms cues hold the text on screen. Only new
/// lines are kept.
pub fn parse_vtt(vtt: &str) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut last_line = String::new();
    let mut lines = vtt.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((a, b)) = line.split_once("-->") else {
            continue;
        };
        let start = vtt_time(a);
        let end = vtt_time(b.split_whitespace().next().unwrap_or(""));
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        let mut text: Vec<String> = Vec::new();
        // A cue ends at an empty line (YouTube's cues may start with a line of one space).
        while let Some(l) = lines.peek() {
            if l.trim_end_matches('\r').is_empty() {
                break;
            }
            let l = strip_tags(lines.next().unwrap_or_default());
            if !l.is_empty() && l != last_line {
                text.push(l.clone());
                last_line = l;
            }
        }
        if end - start < 0.05 || text.is_empty() {
            continue;
        }
        out.push(Seg {
            start,
            end,
            text: text.join(" "),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_auto_captions_keep_each_line_once() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
00:00:00.160 --> 00:00:02.869 align:start position:0%\n \nI<00:00:00.400><c> challenged</c><00:00:00.720><c> Rust</c>\n\n\
00:00:02.869 --> 00:00:02.879 align:start position:0%\nI challenged Rust\n \n\n\
00:00:02.879 --> 00:00:05.000 align:start position:0%\nI challenged Rust\ndevelopers<00:00:03.100><c> to</c><00:00:03.300><c> build</c>\n";
        let segs = parse_vtt(vtt);
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert_eq!(segs[0].text, "I challenged Rust");
        assert_eq!(segs[1].text, "developers to build");
        assert!((segs[1].start - 2.879).abs() < 1e-6);
    }

    #[test]
    fn manual_subtitles_and_entities() {
        let vtt = "WEBVTT\n\n1\n00:01.000 --> 00:03.500\nio_uring &amp; <i>SQLite</i>\n\n2\n01:00:00.000 --> 01:00:01.000\nend\n";
        let segs = parse_vtt(vtt);
        assert_eq!(segs[0].text, "io_uring & SQLite");
        assert!((segs[0].start - 1.0).abs() < 1e-9);
        assert!((segs[1].start - 3600.0).abs() < 1e-9);
    }

    #[test]
    fn prompt_has_title_tags_and_chapters() {
        let meta = VideoMeta {
            title: "I challenged Rust Developers".into(),
            channel: "Arjay McCandless".into(),
            tags: vec!["io_uring".into(), "axum".into()],
            chapters: vec![super::super::Chapter {
                start: 0.0,
                end: 10.0,
                title: "SQLite batching".into(),
            }],
            ..Default::default()
        };
        let p = whisper_prompt(&meta);
        assert_eq!(
            p,
            "I challenged Rust Developers. Arjay McCandless. io_uring, axum. SQLite batching."
        );
    }
}
