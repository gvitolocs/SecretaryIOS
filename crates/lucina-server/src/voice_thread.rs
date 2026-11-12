//! The two sides of the "Lucina Voice" thread (docs/lucinavoice-protocol.md section 7).
//!
//! The speaker's thread is its own session (`lucinavoice`) and the app's chats have theirs, so neither one's recent
//! turns hold the other's. Each side therefore reads the other's latest lines as one more context block:
//!
//! - an app turn gets the speaker's last lines, from the speaker session's recent turns
//!   ([`speaker_block`]): "what did I ask you on the speaker?" has an answer in the app;
//! - a spoken turn gets the app's last lines, from the timeline ([`app_block`]): the app's session ids differ per
//!   client, the timeline is the record they all share.
//!
//! Both only look back [`WINDOW`], show at most [`LINES`] lines and clip each to [`CLIP`] characters.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use lucina_store::timeline::ConversationFilter;
use lucina_store::timeutil::parse_js_date;
use serde_json::Value;

use crate::AppState;

/// How many lines of the other side.
pub const LINES: usize = 6;
/// How far back the other side's lines count.
pub const WINDOW: Duration = Duration::hours(12);
/// Characters kept of each line.
pub const CLIP: usize = 300;

/// The recent-turn key of the speaker's session.
pub fn speaker_key(st: &AppState) -> String {
    lucina_memory::secretary_session_id(crate::chat::VOICE_SESSION, &st.cfg.default_session)
}

/// What the turn reads of the other side: `voice` is whether the turn is the speaker's own.
pub fn other_side_block(st: &AppState, voice: bool, now: DateTime<Utc>) -> String {
    if voice {
        app_block(st, now)
    } else {
        speaker_block(st, now)
    }
}

fn line(at: DateTime<Utc>, tz: Tz, who: &str, text: &str) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text.chars().take(CLIP).collect();
    format!(
        "[{} {}] {who}: {text}",
        at.with_timezone(&tz).format("%d/%m %H:%M"),
        tz.name()
    )
}

/// The speaker's last lines for an app turn.
pub fn speaker_block(st: &AppState, now: DateTime<Utc>) -> String {
    let tz = st.cfg.time_zone;
    let mut lines: Vec<String> = st
        .recent
        .turns(&speaker_key(st))
        .iter()
        .filter_map(|t| {
            let at = parse_js_date(t.at.as_deref()?)?;
            (now - at <= WINDOW).then(|| {
                let who = if t.role == "assistant" {
                    "Lucina"
                } else {
                    "Giuseppe"
                };
                line(at, tz, who, &t.text)
            })
        })
        .collect();
    if lines.len() > LINES {
        lines.drain(..lines.len() - LINES);
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "Latest turns on Giuseppe's LucinaVoice speaker (the \"Lucina Voice\" thread, most recent last; context only, they are not part of this chat). When he asks what he said or asked on the speaker, use these:\n{}",
        lines.join("\n")
    )
}

/// The app's last lines for a spoken turn.
pub fn app_block(st: &AppState, now: DateTime<Utc>) -> String {
    let tz = st.cfg.time_zone;
    let Ok(page) = st.timeline.page(None, None, 60, &ConversationFilter::All) else {
        return String::new();
    };
    // Newest first in the page.
    let mut lines: Vec<String> = Vec::new();
    for item in &page.items {
        let Ok(v) = serde_json::from_str::<Value>(&item.raw) else {
            continue;
        };
        let channel = v["channel"].as_str().unwrap_or("");
        let meta = &v["meta"];
        if !matches!(channel, "web" | "ios" | "dsi" | "mac")
            || meta["via"] == "mcp"
            || meta["kind"].is_string()
            || meta["tool"].is_string()
        {
            continue;
        }
        let who = match v["role"].as_str() {
            Some("user") => "Giuseppe",
            Some("lucina") => "Lucina",
            _ => continue,
        };
        let (Some(text), Some(at)) = (
            v["text"].as_str().filter(|t| !t.trim().is_empty()),
            v["ts"].as_str().and_then(parse_js_date),
        ) else {
            continue;
        };
        if now - at > WINDOW {
            break;
        }
        lines.push(line(at, tz, who, text));
        if lines.len() == LINES {
            break;
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    lines.reverse();
    format!(
        "Latest turns in Giuseppe's Secretary app chat (most recent last; context only, they are not part of this spoken conversation):\n{}",
        lines.join("\n")
    )
}
