//! Whisper, ZCode and YouTube's captions, window by window. Each source's segments go into
//! fixed windows (by their midpoint); a window with one source keeps it, and windows where
//! the sources disagree go to Gemini, which keeps the better text (or fixes one with the
//! other: a term Whisper misheard that ZCode or the captions spelled right). Without Gemini
//! the order is Whisper, captions, ZCode.

use std::time::Duration;

use serde_json::{json, Value};

use super::gemini::{self, Part};
use super::{MergedSeg, Seg, VideoConfig, VideoMeta};

/// Window length (seconds).
const WINDOW: f64 = 20.0;
/// Windows per Gemini request.
const BATCH: usize = 40;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
    pub whisper: String,
    pub zcode: String,
    pub captions: String,
}

impl Window {
    fn sources(&self) -> usize {
        [&self.whisper, &self.zcode, &self.captions]
            .iter()
            .filter(|t| !t.trim().is_empty())
            .count()
    }

    /// The text without a model: Whisper, then the captions, then ZCode.
    fn fallback(&self) -> MergedSeg {
        let (text, source) = if !self.whisper.trim().is_empty() {
            (&self.whisper, "whisper")
        } else if !self.captions.trim().is_empty() {
            (&self.captions, "captions")
        } else {
            (&self.zcode, "zcode")
        };
        MergedSeg {
            start: self.start,
            end: self.end,
            text: text.trim().to_owned(),
            source: source.into(),
        }
    }
}

/// The three sources in shared windows; empty windows are dropped. A window's bounds are
/// those of the segments in it.
pub fn windows(whisper: &[Seg], zcode: &[Seg], captions: &[Seg]) -> Vec<Window> {
    let last = [whisper, zcode, captions]
        .iter()
        .flat_map(|s| s.iter().map(|s| s.end))
        .fold(0.0_f64, f64::max);
    let n = (last / WINDOW).floor() as usize + 1;
    let mut out: Vec<Window> = (0..n)
        .map(|_| Window {
            start: f64::INFINITY,
            end: 0.0,
            ..Window::default()
        })
        .collect();
    let mut put = |segs: &[Seg], pick: fn(&mut Window) -> &mut String| {
        for s in segs {
            let text = s.text.trim();
            if text.is_empty() {
                continue;
            }
            let mid = (s.start + s.end.max(s.start)) / 2.0;
            let i = ((mid.max(0.0) / WINDOW).floor() as usize).min(n - 1);
            let w = &mut out[i];
            w.start = w.start.min(s.start);
            w.end = w.end.max(s.end);
            let t = pick(w);
            if !t.is_empty() {
                t.push(' ');
            }
            t.push_str(text);
        }
    };
    put(whisper, |w| &mut w.whisper);
    put(zcode, |w| &mut w.zcode);
    put(captions, |w| &mut w.captions);
    out.retain(|w| w.sources() > 0);
    out
}

const SYSTEM: &str = "You merge transcripts of the same video made by different systems: \
Whisper (speech recognition, usually the most complete), ZCode (a vision-language model \
watching the video) and YouTube's captions. For each numbered time window, write the single \
best transcript of what is SAID in that window. Rules: keep the speaker's words and language \
(never translate or summarise); prefer the variant that spells names, technical terms, \
identifiers, numbers and units correctly for the topic (e.g. io_uring, not \"IOU Ring\"; \
SQLite, not \"sequel light\"); you may combine words from different sources; never add \
content none of them has; if only one source has text, keep it. Answer with JSON: \
{\"windows\":[{\"i\":<number>,\"text\":\"…\",\"source\":\"whisper|zcode|captions|both\"}]} \
where source is the source you mostly kept (\"both\" when combined).";

fn request(meta: &VideoMeta, batch: &[(usize, &Window)]) -> String {
    let mut s = format!(
        "Video: {} — {}\nTags: {}\nDescription: {}\n\nWindows:\n",
        meta.title,
        meta.channel,
        meta.tags.join(", "),
        meta.description.chars().take(1500).collect::<String>()
    );
    for (i, w) in batch {
        s.push_str(&format!(
            "\n#{i} [{:.1}–{:.1} s]\nWhisper: {}\nZCode: {}\nCaptions: {}\n",
            w.start,
            w.end,
            w.whisper.trim(),
            w.zcode.trim(),
            w.captions.trim()
        ));
    }
    s
}

/// Gemini's answer for a batch: the merged windows (by number), the rest from
/// [`Window::fallback`].
pub fn apply(windows: &[Window], answer: &Value) -> Vec<MergedSeg> {
    let mut out: Vec<MergedSeg> = windows.iter().map(Window::fallback).collect();
    for item in answer["windows"].as_array().into_iter().flatten() {
        let Some(i) = item["i"].as_u64().or_else(|| {
            item["i"]
                .as_str()
                .and_then(|s| s.trim_start_matches('#').parse().ok())
        }) else {
            continue;
        };
        let (Some(slot), Some(text)) = (out.get_mut(i as usize), item["text"].as_str()) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        slot.text = text.trim().to_owned();
        slot.source = match item["source"].as_str().unwrap_or("both") {
            s @ ("whisper" | "zcode" | "captions" | "both") => s.into(),
            _ => "both".into(),
        };
    }
    out
}

pub async fn merge(
    http: &reqwest::Client,
    cfg: &VideoConfig,
    meta: &VideoMeta,
    whisper: &[Seg],
    zcode: &[Seg],
    captions: &[Seg],
) -> Vec<MergedSeg> {
    let windows = windows(whisper, zcode, captions);
    let disputed: Vec<(usize, &Window)> = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.sources() > 1)
        .collect();
    if disputed.is_empty() {
        return windows.iter().map(Window::fallback).collect();
    }
    let mut answers: Vec<Value> = Vec::new();
    for batch in disputed.chunks(BATCH) {
        let parts = [Part::Text(request(meta, batch))];
        match gemini::generate_json(http, cfg, SYSTEM, &parts, Duration::from_secs(180)).await {
            Ok(v) => answers.extend(v["windows"].as_array().cloned().unwrap_or_default()),
            Err(e) => {
                tracing::warn!(error = %e, "transcript merge: Gemini failed; keeping Whisper there")
            }
        }
    }
    apply(&windows, &json!({ "windows": answers }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, text: &str) -> Seg {
        Seg {
            start,
            end,
            text: text.into(),
        }
    }

    #[test]
    fn segments_share_windows_by_midpoint() {
        let w = windows(
            &[
                seg(0.0, 5.0, "I challenged Rust developers"),
                seg(36.0, 40.0, "IOU Ring"),
            ],
            &[
                seg(1.0, 6.0, "I challenged Rust developers"),
                seg(37.0, 41.0, "io_uring"),
            ],
            &[],
        );
        assert_eq!(w.len(), 2, "the empty middle window is dropped");
        assert_eq!(w[0].whisper, "I challenged Rust developers");
        assert_eq!((w[0].start, w[0].end), (0.0, 6.0));
        assert_eq!(
            (w[1].whisper.as_str(), w[1].zcode.as_str()),
            ("IOU Ring", "io_uring")
        );
    }

    #[test]
    fn the_models_choice_wins_and_the_rest_falls_back() {
        let w = windows(
            &[seg(0.0, 5.0, "a"), seg(36.0, 40.0, "IOU Ring")],
            &[seg(37.0, 41.0, "io_uring")],
            &[seg(70.0, 72.0, "only captions")],
        );
        let merged = apply(
            &w,
            &json!({"windows":[{"i":1,"text":"io_uring","source":"zcode"},{"i":"#9","text":"x"}]}),
        );
        assert_eq!(merged.len(), 3);
        assert_eq!(
            (merged[0].text.as_str(), merged[0].source.as_str()),
            ("a", "whisper")
        );
        assert_eq!(
            (merged[1].text.as_str(), merged[1].source.as_str()),
            ("io_uring", "zcode")
        );
        assert_eq!(merged[2].source, "captions");
    }

    #[test]
    fn nothing_to_merge_without_sources() {
        assert!(windows(&[], &[], &[]).is_empty());
    }
}
