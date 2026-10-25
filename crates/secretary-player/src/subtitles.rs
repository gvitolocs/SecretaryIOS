//! SubRip (.srt) and WebVTT parsing, cue lookup, and SRT → WebVTT conversion.
//!
//! The apps draw subtitles themselves over the video (AVPlayer and ExoPlayer
//! cannot side-load an external file into a progressive stream), and the
//! server serves every subtitle as WebVTT, which browsers and HLS take as is.
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Parses SRT or WebVTT (detected from the `WEBVTT` header). Malformed cues
/// are skipped; the result is sorted by start time.
pub fn parse(text: &str) -> Vec<Cue> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let vtt = text.trim_start().starts_with("WEBVTT");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().map(str::trim_end).skip_while(|l| l.trim().is_empty());
        let mut timing = None;
        // The timing line is the first or (after a cue id / SRT counter) the second line.
        for _ in 0..2 {
            let Some(line) = lines.next() else { break };
            if vtt && (line.starts_with("WEBVTT") || line.starts_with("NOTE") || line.starts_with("STYLE") || line.starts_with("REGION")) {
                break;
            }
            if let Some(t) = parse_timing(line) {
                timing = Some(t);
                break;
            }
        }
        let Some((start_ms, end_ms)) = timing else { continue };
        let body: Vec<String> = lines.map(clean).filter(|l| !l.is_empty()).collect();
        if body.is_empty() || end_ms <= start_ms {
            continue;
        }
        cues.push(Cue { start_ms, end_ms, text: body.join("\n") });
    }
    cues.sort_by_key(|c| (c.start_ms, c.end_ms));
    cues
}

/// `00:01:02,500 --> 00:01:04,000` (SRT) or `01:02.500 --> 01:04.000 align:start` (VTT).
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (a, b) = line.split_once("-->")?;
    let b = b.split_whitespace().next()?;
    Some((parse_time(a.trim())?, parse_time(b)?))
}

fn parse_time(s: &str) -> Option<u64> {
    let s = s.replace(',', ".");
    let (hms, frac) = s.split_once('.').unwrap_or((&s, "0"));
    let parts: Vec<u64> = hms.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let (h, m, sec) = match parts[..] {
        [h, m, s] => (h, m, s),
        [m, s] => (0, m, s),
        _ => return None,
    };
    if m >= 60 || sec >= 60 || frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // "5" = 500 ms, "05" = 50 ms, "005" = 5 ms; extra digits are dropped.
    let ms: u64 = format!("{frac:0<3}")[..3].parse().ok()?;
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

/// Drops markup the overlay does not render: `<i>`, `<font ...>`, `<c.red>`,
/// VTT timestamps, and ASS override blocks like `{\an8}`.
fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut depth = None;
    for ch in line.chars() {
        match (depth, ch) {
            (None, '<') => depth = Some('>'),
            (None, '{') => depth = Some('}'),
            (Some(close), c) if c == close => depth = None,
            (Some(_), _) => {}
            (None, c) => out.push(c),
        }
    }
    decode_entities(out.trim())
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
        .replace("&amp;", "&")
}

/// The cues showing at `t_ms` (overlapping cues are all shown, oldest first).
pub fn active(cues: &[Cue], t_ms: u64) -> Vec<&Cue> {
    // Cues are sorted by start: everything after the first start > t is out.
    let end = cues.partition_point(|c| c.start_ms <= t_ms);
    cues[..end].iter().filter(|c| c.end_ms > t_ms).collect()
}

fn format_time(ms: u64) -> String {
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Serialises cues as WebVTT. `<`, `>` and `&` in text are escaped.
pub fn to_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n");
    for c in cues {
        let text = c.text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        out.push_str(&format!("\n{} --> {}\n{}\n", format_time(c.start_ms), format_time(c.end_ms), text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> there\r\n\r\n2\r\n00:00:02,000 --> 00:00:04,000\r\n{\\an8}Second\r\nline two\r\n\r\n3\r\nbroken --> nope\r\nskip\r\n";

    #[test]
    fn parses_srt() {
        let cues = parse(SRT);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start_ms: 1_000, end_ms: 2_500, text: "Hello there".into() });
        assert_eq!(cues[1].text, "Second\nline two");
    }

    #[test]
    fn parses_vtt_with_ids_settings_and_notes() {
        let vtt = "WEBVTT - film\n\nNOTE a comment\n00:00:09.000 --> 00:00:10.000\n\nintro\n01:02.5 --> 01:04.000 align:start line:0\n<c.yellow>Tom &amp; Jerry</c>\n\n00:01:05.000 --> 00:01:06.000\n<00:01:05.500>karaoke\n";
        let cues = parse(vtt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start_ms: 62_500, end_ms: 64_000, text: "Tom & Jerry".into() });
        assert_eq!(cues[1].text, "karaoke");
    }

    #[test]
    fn active_handles_overlap() {
        let cues = parse(SRT);
        let at = |t| active(&cues, t).iter().map(|c| c.start_ms).collect::<Vec<_>>();
        assert_eq!(at(500), Vec::<u64>::new());
        assert_eq!(at(1_500), vec![1_000]);
        assert_eq!(at(2_200), vec![1_000, 2_000]);
        assert_eq!(at(2_500), vec![2_000]);
        assert_eq!(at(4_000), Vec::<u64>::new());
    }

    #[test]
    fn srt_to_vtt_round_trips() {
        let cues = parse(SRT);
        let vtt = to_vtt(&cues);
        assert!(vtt.starts_with("WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nHello there\n"));
        assert_eq!(parse(&vtt), cues);
        let tricky = vec![Cue { start_ms: 3_723_004, end_ms: 3_724_000, text: "a < b & c".into() }];
        assert!(to_vtt(&tricky).contains("01:02:03.004 --> 01:02:04.000\na &lt; b &amp; c"));
        assert_eq!(parse(&to_vtt(&tricky)), tricky);
    }
}
