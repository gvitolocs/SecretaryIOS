//! Swipe to reply: the owner answers one earlier timeline item (`replyTo` on
//! `/api/secretary/chat` and `/v1/share`). The quoted item (its text, what
//! Lucina read from its attachments, a voice note's transcript) becomes a
//! block of the turn, marked as the message being replied to
//! (`planner::TURN_REPLY`). An unknown or unreadable id is ignored.

use chrono::{DateTime, Utc};
use lucina_memory::{Scope, TurnSurface};
use lucina_store::TimelineStore;
use serde_json::Value;

use crate::share::memory::Record;
use crate::AppState;

/// Characters of the whole block.
pub const MAX_BLOCK_CHARS: usize = 4_000;
/// Characters of the quoted text.
const MAX_TEXT_CHARS: usize = 2_500;
/// Characters of one attachment's summary or transcript.
const MAX_PART_CHARS: usize = 1_500;
const TRAILER: &str = "\nThe owner's new message below is about that message.";

/// `replyTo`: 1..=64 bytes of `[A-Za-z0-9_-]` (a timeline item id).
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The block for a turn of `session` replying to `id` ("" when the turn may not
/// read the owner's timeline, or the item is unknown or unreadable). The item
/// is looked up in the timeline the turn writes (`AppState::timeline_for`): a
/// self-test quotes its own conversation, never the owner's.
pub async fn block(st: &AppState, surface: &TurnSurface, session: &str, id: &str) -> String {
    if !surface.may_read(&Scope::OwnerPersonal) {
        surface.log_denied("read", &Scope::OwnerPersonal, "reply_to");
        return String::new();
    }
    let st2 = st.clone();
    let timeline = st.timeline_for(session);
    let id2 = id.to_owned();
    let out = tokio::task::spawn_blocking(move || build(&st2, &timeline, &id2))
        .await
        .unwrap_or_default();
    if out.is_empty() {
        tracing::debug!(reply_to = %id, "replyTo names no readable timeline item; ignored");
    }
    out
}

fn build(st: &AppState, timeline: &TimelineStore, id: &str) -> String {
    let item = match timeline.get(id) {
        Ok(Some(i)) => i,
        Ok(None) => return String::new(),
        Err(e) => {
            tracing::debug!(reply_to = %id, error = %e, "replyTo lookup failed");
            return String::new();
        }
    };
    let Ok(v) = serde_json::from_str::<Value>(&item.raw) else {
        return String::new();
    };
    let from = if v["role"] == "lucina" {
        st.cfg.persona.clone()
    } else {
        "the owner".to_owned()
    };
    let when = v["ts"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| v["ts"].as_i64().and_then(DateTime::from_timestamp_millis))
        .map(|t| {
            t.with_timezone(&st.cfg.time_zone)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "earlier".into());
    let text = clip(v["text"].as_str().unwrap_or("").trim(), MAX_TEXT_CHARS);
    let mut out =
        format!("The owner is replying to this earlier message (from {from}, {when}):\n«{text}»");
    let attachments = v["attachments"].as_array().cloned().unwrap_or_default();
    if !attachments.is_empty() {
        let share_id = v["meta"]["shareId"].as_str();
        let records = st.share.memory.for_item(id, share_id).unwrap_or_else(|e| {
            tracing::debug!(reply_to = %id, error = %e, "replyTo attachment memory unreadable");
            Vec::new()
        });
        // Lucina's answer to a voice note keeps the words she heard.
        let mut heard: Option<Option<String>> = None;
        out.push_str("\nIts attachments:");
        for a in &attachments {
            let kind = a["kind"].as_str().unwrap_or("file");
            let name = a["name"].as_str().unwrap_or("").trim();
            out.push_str(&format!("\n- {kind}"));
            if !name.is_empty() {
                out.push_str(&format!(" \"{name}\""));
            }
            if kind == "voice" {
                let transcript = a["transcript"]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        let voice = |r: &&Record| r.subtype.as_deref() == Some("voice");
                        let media = a["mediaId"].as_str();
                        records
                            .iter()
                            .filter(voice)
                            .find(|r| media.is_some() && r.media_id.as_deref() == media)
                            .or_else(|| records.iter().find(voice))
                            .map(|r| r.text.trim().to_owned())
                            .filter(|s| !s.is_empty())
                    })
                    .or_else(|| {
                        heard
                            .get_or_insert_with(|| voice_transcript(timeline, id))
                            .clone()
                    });
                match transcript {
                    Some(t) => out.push_str(&format!(
                        ", voice note transcript: «{}»",
                        clip(&t, MAX_PART_CHARS)
                    )),
                    None => out.push_str(" (voice note, no transcript)"),
                }
                continue;
            }
            let media = a["mediaId"].as_str();
            let record = records.iter().find(|r| {
                (media.is_some() && r.media_id.as_deref() == media)
                    || (!name.is_empty() && r.name == name)
            });
            if let Some(r) = record {
                let body = r
                    .summary
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(r.text.trim());
                if !body.is_empty() {
                    out.push_str(&format!(", read from it: {}", clip(body, MAX_PART_CHARS)));
                }
            }
        }
        // Records not matched to an entry above (a renamed title, a long text part).
        for r in &records {
            let shown = attachments.iter().any(|a| {
                (a["mediaId"].as_str().is_some() && r.media_id.as_deref() == a["mediaId"].as_str())
                    || a["name"].as_str().map(str::trim) == Some(r.name.as_str())
                    || (a["kind"] == "voice" && r.subtype.as_deref() == Some("voice"))
            });
            if shown {
                continue;
            }
            let body = r
                .summary
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(r.text.trim());
            if !body.is_empty() {
                out.push_str(&format!(
                    "\n- {} \"{}\", read from it: {}",
                    r.kind,
                    r.name,
                    clip(body, MAX_PART_CHARS)
                ));
            }
        }
    }
    let mut out = clip(&out, MAX_BLOCK_CHARS - TRAILER.chars().count());
    out.push_str(TRAILER);
    out
}

/// The transcript on Lucina's reply to a voice note (`meta.transcript` of the
/// Lucina item whose `replyTo` is `id`).
fn voice_transcript(timeline: &TimelineStore, id: &str) -> Option<String> {
    let items = timeline.read_all().ok()?;
    items
        .iter()
        .rev()
        .filter(|i| i.raw.contains(id))
        .find_map(|i| {
            let v: Value = serde_json::from_str(&i.raw).ok()?;
            if v["role"] != "lucina" || v["replyTo"] != id {
                return None;
            }
            v["meta"]["transcript"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_clipping() {
        assert!(valid_id("01K83ZK4Q6X1V9W2D3E4F5G6H7"));
        assert!(valid_id("tl_2-x"));
        for bad in ["", "a/b", "a.b", "a b", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("àèìòù", 3).chars().count(), 3);
    }
}
