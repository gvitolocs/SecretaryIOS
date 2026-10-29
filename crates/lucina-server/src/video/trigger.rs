//! When a chat turn is a video report request: a YouTube link (typed, pasted, or shared from
//! the iPhone) and the owner asking for a transcript, a report or a summary.

use std::sync::OnceLock;

use regex::Regex;

/// The 11-character id of a YouTube video URL (watch, shorts, youtu.be, live, embed).
pub fn video_id(url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?ix)^(?:https?://)?(?:(?:www|m|music)\.)?
              (?:youtube\.com/(?:watch\?(?:[^\s\#]*&)?v=|shorts/|live/|embed/|v/)|youtu\.be/)
              ([A-Za-z0-9_-]{11})(?:[^A-Za-z0-9_-]|$)",
        )
        .expect("video id regex")
    });
    re.captures(url.trim()).map(|c| c[1].to_owned())
}

/// The first YouTube video in `text`: (the URL as written, its id). A share turn carries the
/// link as `"video_id":"…"` data instead of the URL.
pub fn find(text: &str) -> Option<(String, String)> {
    static URL: OnceLock<Regex> = OnceLock::new();
    static DATA: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:https?://)?(?:(?:www|m|music)\.)?(?:youtube\.com|youtu\.be)/[^\s<>"')\]]+"#,
        )
        .expect("url regex")
    });
    for m in url.find_iter(text) {
        if let Some(id) = video_id(m.as_str()) {
            return Some((m.as_str().to_owned(), id));
        }
    }
    let data = DATA.get_or_init(|| {
        Regex::new(r#""video_id"\s*:\s*"([A-Za-z0-9_-]{11})""#).expect("data regex")
    });
    data.captures(text).map(|c| {
        let id = c[1].to_owned();
        (format!("https://www.youtube.com/watch?v={id}"), id)
    })
}

/// What the owner wrote. A share turn's message is the owner's note plus the shared
/// content (whose description may well say "summary"): only the note counts.
pub fn owner_request(message: &str) -> String {
    const NOTE: &str = "Owner's question or note: \"";
    if let Some(i) = message.find(NOTE) {
        let rest = &message[i + NOTE.len()..];
        let end = rest
            .find("\"\n")
            .or_else(|| rest.rfind('"'))
            .unwrap_or(rest.len());
        return rest[..end].to_owned();
    }
    if let Some(i) = message.find("<Shared content>") {
        return message[..i].trim().to_owned();
    }
    message.to_owned()
}

/// Does the owner ask for a transcript, a report or a summary (en, it, da)?
pub fn wants_report(request: &str) -> bool {
    let r = request.to_lowercase();
    const WORDS: &[&str] = &[
        "transcri",
        "trascri",
        "transskri",
        "report",
        "rapport",
        "summar",
        "riassu",
        "sintesi",
        "resum",
        "opsummer",
        "sammenfat",
        "resumé",
        "visual inspection",
        "analy",
        "break it down",
        "key points",
        "punti chiave",
    ];
    WORDS.iter().any(|w| r.contains(w))
}

/// "again", "redo": make a new report even when there is one.
pub fn wants_redo(request: &str) -> bool {
    let r = request.to_lowercase();
    [
        "again",
        "redo",
        "re-do",
        "rerun",
        "re-run",
        "di nuovo",
        "rifai",
        "igen",
        "from scratch",
    ]
    .iter()
    .any(|w| r.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_every_url_shape() {
        for (u, id) in [
            (
                "https://youtube.com/shorts/FBs--2nSPLQ?is=32SzuKgHtzUd-_pI",
                "FBs--2nSPLQ",
            ),
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
                "dQw4w9WgXcQ",
            ),
            (
                "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
                "dQw4w9WgXcQ",
            ),
            ("youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            ("https://m.youtube.com/live/dQw4w9WgXcQ?si=x", "dQw4w9WgXcQ"),
            (
                "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
                "dQw4w9WgXcQ",
            ),
        ] {
            assert_eq!(video_id(u).as_deref(), Some(id), "{u}");
        }
        assert_eq!(
            video_id("https://www.youtube.com/playlist?list=PL3940CA200C541175"),
            None
        );
        assert_eq!(video_id("https://youtube.com/shorts/tooShort"), None);
    }

    #[test]
    fn finds_the_link_in_text_or_share_data() {
        let (u, id) =
            find("Transcribe this https://youtube.com/shorts/FBs--2nSPLQ?is=32Sz please").unwrap();
        assert_eq!(
            (u.as_str(), id.as_str()),
            (
                "https://youtube.com/shorts/FBs--2nSPLQ?is=32Sz",
                "FBs--2nSPLQ"
            )
        );
        let share = "Owner's question or note: \"Transcribe this video\"\n<Shared content>\nData: {\"channel\":\"Arjay McCandless\",\"has_transcript\":false,\"video_id\":\"FBs--2nSPLQ\"}";
        assert_eq!(find(share).unwrap().1, "FBs--2nSPLQ");
        assert!(find("no video here https://example.com").is_none());
    }

    #[test]
    fn only_the_owners_words_decide() {
        let msg = "Owner's question or note: \"Transcribe this video and visual inspection with awkward terms then do a report\"\nDevice language: da\n\n<Shared content>\nDescription: a summary of …";
        let req = owner_request(msg);
        assert_eq!(
            req,
            "Transcribe this video and visual inspection with awkward terms then do a report"
        );
        assert!(wants_report(&req));
        let msg = "Owner's question or note: \"Is this funny?\"\n<Shared content>\nDescription: summary report";
        assert!(!wants_report(&owner_request(msg)));
        assert!(wants_report(
            "riassumi questo video https://youtu.be/dQw4w9WgXcQ"
        ));
        assert!(wants_report("Kan du lave et resumé?"));
        assert!(!wants_report(
            "remind me to watch https://youtu.be/dQw4w9WgXcQ tonight"
        ));
        assert!(wants_redo("do the report again"));
        assert!(!wants_redo("do a report"));
    }
}
