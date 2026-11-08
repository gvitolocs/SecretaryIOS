//! YouTube links: video ids, caption tracks and transcripts, read from pages
//! and timedtext bodies the orchestration fetched. No network I/O here.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::Extracted;

/// Seconds between `[mm:ss]` markers in a transcript.
const MARKER_EVERY_S: f64 = 60.0;

/// The 11-character video id of any common YouTube URL shape.
pub fn video_id(url: &str) -> Option<String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url)
        .or_else(|_| reqwest::Url::parse(&format!("https://{url}")))
        .ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let mut segs = parsed.path_segments()?.filter(|s| !s.is_empty());
    let candidate = match host {
        "youtu.be" => segs.next().map(str::to_string),
        "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            match segs.next() {
                Some("watch") => parsed
                    .query_pairs()
                    .find(|(k, _)| k == "v")
                    .map(|(_, v)| v.into_owned()),
                Some("shorts" | "embed" | "live" | "v" | "e") => segs.next().map(str::to_string),
                _ => None,
            }
        }
        _ => None,
    }?;
    is_video_id(&candidate).then_some(candidate)
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One caption track listed by a watch page.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub lang: String,
    pub name: String,
    /// Speech-recognised (`kind == "asr"`).
    pub auto: bool,
    pub url: String,
}

/// The caption tracks in a watch page's `ytInitialPlayerResponse`.
pub fn caption_tracks(watch_html: &str) -> Vec<Track> {
    let captions = player_response(watch_html)
        .and_then(|pr| pr.get("captions").cloned())
        .or_else(|| json_after(watch_html, "\"captions\":"));
    let Some(list) = captions
        .as_ref()
        .and_then(|c| c.pointer("/playerCaptionsTracklistRenderer/captionTracks"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|t| {
            let base = t.get("baseUrl")?.as_str()?;
            let url = base.replace("&amp;", "&");
            let url = if url.starts_with('/') {
                format!("https://www.youtube.com{url}")
            } else {
                url
            };
            let name = t
                .pointer("/name/simpleText")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let runs = t.pointer("/name/runs")?.as_array()?;
                    Some(
                        runs.iter()
                            .filter_map(|r| r.get("text").and_then(Value::as_str))
                            .collect::<String>(),
                    )
                })
                .unwrap_or_default();
            Some(Track {
                lang: t
                    .get("languageCode")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name,
                auto: t.get("kind").and_then(Value::as_str) == Some("asr"),
                url,
            })
        })
        .collect()
}

/// The best track for the owner: manual then automatic in `lang`, then
/// manual then automatic English, then whatever comes first.
pub fn pick_track<'a>(tracks: &'a [Track], lang: &str) -> Option<&'a Track> {
    let primary = |l: &str| {
        l.split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let owner = primary(lang);
    let find = |want: &str, auto: bool| {
        tracks
            .iter()
            .find(|t| t.auto == auto && !want.is_empty() && primary(&t.lang) == want)
    };
    find(&owner, false)
        .or_else(|| find(&owner, true))
        .or_else(|| find("en", false))
        .or_else(|| find("en", true))
        .or_else(|| tracks.first())
}

/// The track URL asking for the json3 format unless a format is already set.
pub fn timedtext_url(track: &Track) -> String {
    let has_fmt = track
        .url
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|kv| kv.starts_with("fmt=")));
    if has_fmt {
        track.url.clone()
    } else if track.url.contains('?') {
        format!("{}&fmt=json3", track.url)
    } else {
        format!("{}?fmt=json3", track.url)
    }
}

/// Plain transcript text from a json3 or XML timedtext body, with a
/// `[mm:ss]` marker starting a new line about every minute.
pub fn parse_timedtext(body: &str) -> String {
    let body = body.trim_start_matches('\u{feff}').trim_start();
    let cues = if body.starts_with('{') {
        json3_cues(body)
    } else {
        xml_cues(body)
    };
    let mut out = String::new();
    let mut next_marker = 0.0;
    for (start, text) in cues {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        if start >= next_marker {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[{}] ", timestamp(start)));
            while next_marker <= start {
                next_marker += MARKER_EVERY_S;
            }
        } else {
            out.push(' ');
        }
        out.push_str(&text);
    }
    out
}

fn timestamp(seconds: f64) -> String {
    // Whole seconds of a transcript position; negative or NaN become 0.
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// `events[].segs[].utf8` with `tStartMs`.
fn json3_cues(body: &str) -> Vec<(f64, String)> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let Some(events) = v.get("events").and_then(Value::as_array) else {
        return Vec::new();
    };
    events
        .iter()
        .filter_map(|e| {
            let segs = e.get("segs")?.as_array()?;
            let text: String = segs
                .iter()
                .filter_map(|s| s.get("utf8").and_then(Value::as_str))
                .collect();
            let start = e.get("tStartMs").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
            Some((start, text))
        })
        .collect()
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    // The patterns are literals in this file and covered by the tests.
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

/// `<text start="1.2" dur="3">…</text>` (format 1) or `<p t="1200">…</p>` (srv3).
fn xml_cues(body: &str) -> Vec<(f64, String)> {
    static TEXT: OnceLock<Regex> = OnceLock::new();
    static P: OnceLock<Regex> = OnceLock::new();
    static START: OnceLock<Regex> = OnceLock::new();
    static T: OnceLock<Regex> = OnceLock::new();
    let text_re = regex(&TEXT, r"(?s)<text\b([^>]*)>(.*?)</text>");
    let start_re = regex(&START, r#"\bstart="([0-9.]+)""#);
    let mut cues: Vec<(f64, String)> = text_re
        .captures_iter(body)
        .map(|c| {
            let start = start_re
                .captures(&c[1])
                .and_then(|s| s[1].parse::<f64>().ok())
                .unwrap_or(0.0);
            (start, clean_markup(&c[2]))
        })
        .collect();
    if cues.is_empty() {
        let p_re = regex(&P, r"(?s)<p\b([^>]*)>(.*?)</p>");
        let t_re = regex(&T, r#"\bt="([0-9]+)""#);
        cues = p_re
            .captures_iter(body)
            .map(|c| {
                let start = t_re
                    .captures(&c[1])
                    .and_then(|s| s[1].parse::<f64>().ok())
                    .unwrap_or(0.0)
                    / 1000.0;
                (start, clean_markup(&c[2]))
            })
            .collect();
    }
    cues
}

/// Strips tags and decodes entities, including double-escaped ones
/// (`&amp;quot;`), which timedtext XML often carries.
fn clean_markup(s: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let mut cur = s.to_string();
    for _ in 0..3 {
        let decoded = decode_entities(&cur);
        if decoded == cur {
            break;
        }
        cur = decoded;
    }
    regex(&TAG, r"<[^>]{0,200}>")
        .replace_all(&cur, "")
        .into_owned()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let decoded = after.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &after[..end];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => {
                    let num = name.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => num.parse().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `(title, channel, description)` from `videoDetails`, else the meta tags.
pub fn page_meta(watch_html: &str) -> (Option<String>, Option<String>, Option<String>) {
    let details = player_response(watch_html).and_then(|pr| pr.get("videoDetails").cloned());
    let field = |key: &str| {
        details
            .as_ref()
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let title = field("title")
        .or_else(|| meta_content(watch_html, "og:title"))
        .or_else(|| meta_content(watch_html, "title"));
    let channel = field("author")
        .or_else(|| link_name(watch_html))
        .or_else(|| meta_content(watch_html, "author"));
    let description = field("shortDescription")
        .or_else(|| meta_content(watch_html, "og:description"))
        .or_else(|| meta_content(watch_html, "description"));
    (title, channel, description)
}

/// `content` of `<meta property|name="key" content="…">`, in either order.
fn meta_content(html: &str, key: &str) -> Option<String> {
    static META: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();
    let meta_re = regex(&META, r"(?is)<meta\b[^>]*>");
    let attr_re = regex(
        &ATTR,
        r#"(?is)\b(property|name|itemprop|content)\s*=\s*"([^"]*)""#,
    );
    meta_re.find_iter(html).find_map(|m| {
        let mut is_key = false;
        let mut content = None;
        for c in attr_re.captures_iter(m.as_str()) {
            if c[1].eq_ignore_ascii_case("content") {
                content = Some(c[2].to_string());
            } else if c[2].eq_ignore_ascii_case(key) {
                is_key = true;
            }
        }
        let content = decode_entities(content?.trim());
        (is_key && !content.is_empty()).then_some(content)
    })
}

/// The channel in `<span itemprop="author"><link itemprop="name" content="…">`.
fn link_name(html: &str) -> Option<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let re = regex(
        &LINK,
        r#"(?is)itemprop="author".{0,400}?<link\s+itemprop="name"\s+content="([^"]+)""#,
    );
    re.captures(html).map(|c| decode_entities(c[1].trim()))
}

/// The JSON object assigned to `ytInitialPlayerResponse`.
fn player_response(html: &str) -> Option<Value> {
    let marker = "ytInitialPlayerResponse";
    let mut from = 0;
    while let Some(pos) = html[from..].find(marker) {
        let after = from + pos + marker.len();
        let rest = html[after..].trim_start_matches(['"', '\'', ']', ' ', '\t', '\n', '\r']);
        if let Some(r) = rest.strip_prefix(['=', ':']) {
            let r = r.trim_start();
            if r.starts_with('{') {
                if let Some(obj) = balanced_object(r) {
                    if let Ok(v) = serde_json::from_str::<Value>(obj) {
                        return Some(v);
                    }
                }
            }
        }
        from = after;
    }
    None
}

/// The first parseable JSON object right after `key`.
fn json_after(html: &str, key: &str) -> Option<Value> {
    let mut from = 0;
    while let Some(pos) = html[from..].find(key) {
        let after = from + pos + key.len();
        let rest = html[after..].trim_start();
        if rest.starts_with('{') {
            if let Some(v) = balanced_object(rest).and_then(|o| serde_json::from_str(o).ok()) {
                return Some(v);
            }
        }
        from = after;
    }
    None
}

/// The `{…}` prefix of `s` (which starts with `{`), skipping braces in strings.
fn balanced_object(s: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, b) in s.bytes().enumerate() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The shared-link result for a YouTube video. The caller adds
/// `structured.video_id` (from [`video_id`]) when it has one.
pub fn to_extracted(
    title: Option<String>,
    channel: Option<String>,
    description: Option<String>,
    transcript: Option<String>,
) -> Extracted {
    let mut out = Extracted::new("link");
    out.subtype = Some("youtube".into());
    let transcript = transcript.filter(|t| !t.trim().is_empty());
    let mut text = Vec::new();
    if let Some(t) = &title {
        text.push(format!("Title: {t}"));
    }
    if let Some(c) = &channel {
        text.push(format!("Channel: {c}"));
    }
    if let Some(d) = description.as_deref().filter(|d| !d.trim().is_empty()) {
        text.push(format!("\nDescription:\n{}", d.trim()));
    }
    match &transcript {
        Some(t) => text.push(format!("\nTranscript:\n{}", t.trim())),
        None => out
            .warnings
            .push("No transcript was available for this video.".into()),
    }
    out.structured = serde_json::json!({
        "channel": channel,
        "has_transcript": transcript.is_some(),
    });
    out.title = title;
    out.text = text.join("\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCH: &str = include_str!("../../../tests/fixtures/attachments/youtube-watch.html");
    const JSON3: &str =
        include_str!("../../../tests/fixtures/attachments/youtube-timedtext.json3.json");
    const XML: &str = include_str!("../../../tests/fixtures/attachments/youtube-timedtext.xml");

    #[test]
    fn video_ids_from_every_url_shape() {
        let id = Some("dQw4w9WgXcQ".to_string());
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=42s",
            "youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc123",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVM",
            "https://www.youtube.com/embed/dQw4w9WgXcQ?start=10",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=shared",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(video_id(url), id, "{url}");
        }
        for bad in [
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/channel/UC1234567890",
            "https://vimeo.com/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXc!",
            "not a url",
        ] {
            assert_eq!(video_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn caption_tracks_from_watch_page() {
        let tracks = caption_tracks(WATCH);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].lang, "en");
        assert_eq!(tracks[0].name, "English");
        assert!(!tracks[0].auto);
        assert!(tracks[0]
            .url
            .starts_with("https://www.youtube.com/api/timedtext?v=aBcDeFgHiJ0&ei="));
        assert!(tracks[0].url.contains("&lang=en"));
        assert!(!tracks[0].url.contains("\\u0026"));
        assert_eq!(tracks[1].lang, "it");
        assert_eq!(tracks[1].name, "Italiano (generati automaticamente)");
        assert!(tracks[1].auto);
    }

    #[test]
    fn captions_key_fallback_and_missing_captions() {
        let html = r#"<script>var x = {"captions":{"playerCaptionsTracklistRenderer":{"captionTracks":[{"baseUrl":"/api/timedtext?v=x&amp;lang=da","name":{"runs":[{"text":"Dansk"}]},"languageCode":"da"}]}}};</script>"#;
        let tracks = caption_tracks(html);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].name, "Dansk");
        assert_eq!(
            tracks[0].url,
            "https://www.youtube.com/api/timedtext?v=x&lang=da"
        );
        assert!(caption_tracks("<html>no player here</html>").is_empty());
    }

    #[test]
    fn picks_the_owner_track() {
        let t = |lang: &str, auto: bool| Track {
            lang: lang.into(),
            name: lang.into(),
            auto,
            url: format!("https://x/{lang}/{auto}"),
        };
        let tracks = vec![
            t("de", false),
            t("en", true),
            t("it", true),
            t("en", false),
            t("it", false),
        ];
        assert_eq!(pick_track(&tracks, "it-IT"), Some(&tracks[4]));
        assert_eq!(pick_track(&tracks[..3], "it"), Some(&tracks[2]));
        assert_eq!(pick_track(&tracks, "da"), Some(&tracks[3]));
        assert_eq!(pick_track(&tracks[..2], "da"), Some(&tracks[1]));
        assert_eq!(pick_track(&tracks[..1], "da"), Some(&tracks[0]));
        assert_eq!(pick_track(&[], "da"), None);
        let from_page = caption_tracks(WATCH);
        assert_eq!(
            pick_track(&from_page, "it").map(|t| t.lang.as_str()),
            Some("it")
        );
        assert_eq!(
            pick_track(&from_page, "da").map(|t| t.lang.as_str()),
            Some("en")
        );
    }

    #[test]
    fn timedtext_url_adds_json3_once() {
        let mut track = Track {
            lang: "en".into(),
            name: "English".into(),
            auto: false,
            url: "https://www.youtube.com/api/timedtext?v=x&lang=en".into(),
        };
        assert_eq!(
            timedtext_url(&track),
            "https://www.youtube.com/api/timedtext?v=x&lang=en&fmt=json3"
        );
        track.url.push_str("&fmt=srv3");
        assert_eq!(timedtext_url(&track), track.url);
        track.url = "https://example.com/tt".into();
        assert_eq!(timedtext_url(&track), "https://example.com/tt?fmt=json3");
    }

    #[test]
    fn parses_json3_timedtext() {
        let text = parse_timedtext(JSON3);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "[00:00] Welcome back to the channel. Today we're making fresh pasta by hand."
        );
        assert!(lines[1].starts_with("[01:02] You need 100 grams of flour per egg."));
        assert!(lines[2].starts_with("[02:05] Rest the dough"));
        assert!(text.contains("& salt"));
    }

    #[test]
    fn parses_xml_timedtext() {
        let text = parse_timedtext(XML);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[0],
            "[00:00] Ciao a tutti! Oggi parliamo di \"pasta fresca\" & ragù. Don't worry, it's easy."
        );
        assert_eq!(lines[1], "[01:01] Mettete la farina sul tavolo.");
        assert_eq!(
            parse_timedtext(
                r#"<timedtext format="3"><body><p t="61500" d="2000">Uno <s>due</s></p></body></timedtext>"#
            ),
            "[01:01] Uno due"
        );
        assert_eq!(timestamp(3725.0), "1:02:05");
        assert_eq!(parse_timedtext(""), "");
    }

    #[test]
    fn page_meta_from_video_details_and_og_tags() {
        let (title, channel, description) = page_meta(WATCH);
        assert_eq!(
            title.as_deref(),
            Some("Fresh pasta by hand: the only recipe you need")
        );
        assert_eq!(channel.as_deref(), Some("Nonna's Kitchen"));
        assert!(description
            .as_deref()
            .is_some_and(|d| d.starts_with("Flour, eggs & patience.\nIngredients")));

        let html = r#"<meta content="Og &amp; title" property="og:title"><meta property="og:description" content="Desc"><span itemprop="author" itemscope itemtype="http://schema.org/Person"><link itemprop="url" href="https://www.youtube.com/@x"><link itemprop="name" content="Chan"></span>"#;
        assert_eq!(
            page_meta(html),
            (
                Some("Og & title".into()),
                Some("Chan".into()),
                Some("Desc".into())
            )
        );
        assert_eq!(page_meta("<html></html>"), (None, None, None));
    }

    #[test]
    fn builds_the_extracted_link() {
        let (title, channel, description) = page_meta(WATCH);
        let transcript = parse_timedtext(JSON3);
        let x = to_extracted(title, channel, description, Some(transcript));
        assert_eq!(x.kind, "link");
        assert_eq!(x.subtype.as_deref(), Some("youtube"));
        assert_eq!(x.structured["channel"], "Nonna's Kitchen");
        assert_eq!(x.structured["has_transcript"], true);
        assert!(x.text.starts_with(
            "Title: Fresh pasta by hand: the only recipe you need\nChannel: Nonna's Kitchen\n\nDescription:\nFlour, eggs"
        ));
        assert!(x.text.contains("\n\nTranscript:\n[00:00] Welcome back"));
        assert!(x.warnings.is_empty());

        let bare = to_extracted(Some("T".into()), None, None, Some("  ".into()));
        assert_eq!(bare.structured["has_transcript"], false);
        assert_eq!(bare.structured["channel"], Value::Null);
        assert_eq!(bare.text, "Title: T");
        assert_eq!(bare.warnings.len(), 1);
    }
}
