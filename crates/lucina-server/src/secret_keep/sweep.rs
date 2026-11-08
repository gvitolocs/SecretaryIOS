//! The sweep: notes that changed since the last look, checked for secret shapes by deterministic
//! patterns, no model at all. A note that looks like it holds one gets one offer in the owner's
//! chat, with a button that asks Lucina to move it. Names and titles only, never the text.

use std::collections::BTreeMap;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::owner_secrets::{lucina_item, rich_meta};

const FILE: &str = "secret-keep-sweep.json";
/// Notes named in one offer; the rest wait for the next sweep.
const MAX_OFFER: usize = 3;
const OFFER_EVERY: i64 = 24 * 3600;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Sweep {
    cursor: String,
    /// Note id -> the `updatedAt` already offered: not asked twice.
    offered: BTreeMap<String, String>,
    /// When the last offer went out: at most one card a day, however many notes wait.
    #[serde(default)]
    last_offer: i64,
}

/// A title that may be shown or sent to the cloud model. Never the slug: it is made from the first
/// line when there is no title, so it can hold the secret itself.
pub fn safe_title(n: &Value) -> String {
    let title = n["title"].as_str().unwrap_or_default();
    let title = lucina_secrets::labelled::mask(title);
    let title = lucina_secrets::redactor().redact(&title).into_owned();
    if title.is_empty() {
        "untitled".to_owned()
    } else {
        title
    }
}

/// Why a note looks like it holds a secret.
#[derive(Debug, PartialEq, Eq)]
pub struct Suspect {
    pub why: &'static str,
    /// Nothing but the secret: moving the whole text makes sense.
    pub whole: bool,
}

pub fn note_suspect(n: &Value) -> Option<Suspect> {
    let title = n["title"].as_str().unwrap_or_default();
    let mut text = n["content"].as_str().unwrap_or_default().to_owned();
    if text.is_empty() {
        for it in n["items"].as_array().into_iter().flatten() {
            text.push_str(it["text"].as_str().unwrap_or_default());
            text.push('\n');
        }
    }
    let text = text.as_str();
    let moved = text.contains("[secret:");
    let named = lucina_secrets::labelled::title_names_secret(title);
    let short = !text.trim().is_empty() && text.split_whitespace().count() <= 2;
    if lucina_secrets::labelled::slug_holds_token(n["slug"].as_str().unwrap_or_default()) {
        return Some(Suspect {
            why: "its first line looks like a token",
            whole: false,
        });
    }
    if !lucina_secrets::redact::detect(text).is_empty() {
        return Some(Suspect {
            why: "a token shape",
            whole: false,
        });
    }
    if !lucina_secrets::labelled::detect(text).is_empty() {
        return Some(Suspect {
            why: "a value said in words",
            whole: false,
        });
    }
    if named && !moved && !text.trim().is_empty() {
        return Some(Suspect {
            why: "its title names a credential",
            whole: !short,
        });
    }
    None
}

fn load(path: &std::path::Path) -> Sweep {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// One look. A failure waits for the next one.
pub async fn run(st: &crate::AppState) {
    let path = st.cfg.data_dir.join(FILE);
    let mut sweep = load(&path);
    if chrono::Utc::now().timestamp() - sweep.last_offer < OFFER_EVERY {
        return;
    }
    let mut suspects: Vec<(String, String)> = Vec::new();
    let mut more_suspects = 0usize;
    let mut pages = 0;
    loop {
        pages += 1;
        let since = if sweep.cursor.is_empty() {
            "0"
        } else {
            sweep.cursor.as_str()
        };
        let r = st
            .secretary
            .call(
                Method::GET,
                &format!("/v1/notes/changes?since={since}&limit=200"),
                None,
            )
            .await;
        if !r.ok {
            if r.status == 410 {
                // The cursor is too old: from the start next time. What was offered stays offered.
                sweep.cursor.clear();
                save(&path, &sweep);
            }
            tracing::info!(code = %r.code, "secret sweep: notes not read");
            return;
        }
        for n in r.data["notes"].as_array().into_iter().flatten() {
            let (Some(id), Some(at)) = (n["id"].as_str(), n["updatedAt"].as_str()) else {
                continue;
            };
            if note_suspect(n).is_none() || sweep.offered.get(id).is_some_and(|a| a == at) {
                continue;
            }
            if suspects.len() < MAX_OFFER {
                sweep.offered.insert(id.to_owned(), at.to_owned());
                suspects.push((id.to_owned(), safe_title(n)));
            } else {
                more_suspects += 1;
            }
        }
        // Notes past the offer's size aren't marked: they stay behind the cursor by not moving it.
        if more_suspects > 0 {
            break;
        }
        sweep.cursor = r.data["cursor"].as_str().unwrap_or_default().to_owned();
        if !r.data["hasMore"].as_bool().unwrap_or(false) || pages >= 100 {
            break;
        }
    }
    if !suspects.is_empty() {
        offer(st, &suspects, more_suspects);
        sweep.last_offer = chrono::Utc::now().timestamp();
    }
    save(&path, &sweep);
}

fn save(path: &std::path::Path, sweep: &Sweep) {
    if let Ok(bytes) = serde_json::to_vec(sweep) {
        if let Err(e) = lucina_store::atomic::write_atomic(path, &bytes) {
            tracing::warn!(error = %e.kind(), "secret sweep state not written");
        }
    }
}

fn offer(st: &crate::AppState, notes: &[(String, String)], more: usize) {
    let names = notes
        .iter()
        .map(|(_, t)| format!("“{t}”"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!(
        "🔐 These notes look like they hold a secret: {names}. I can move each one to Infisical myself: only my local model reads it, then I take it out of the note."
    );
    if more > 0 {
        text.push_str(&format!(" {more} more after these."));
    }
    let mut actions: Vec<Value> = notes
        .iter()
        .take(MAX_OFFER)
        .enumerate()
        .map(|(i, (id, title))| {
            let short: String = title.chars().take(24).collect();
            json!({"id": format!("keep-note-{}", i + 1), "label": format!("Move {short}"), "emoji": "🔐", "kind": "reply",
                   "payload": {"text": format!("Move the password in note {id} to Infisical")}})
        })
        .collect();
    actions
        .push(json!({"id": "keep-note-no", "label": "Not now", "kind": "dismiss", "payload": {}}));
    let _ = lucina_item(
        &st.timeline,
        &text,
        "secret_offer",
        rich_meta(vec![text.clone()], actions),
    );
    tracing::info!(notes = notes.len(), more, "secret sweep offered");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_counts_as_a_suspect() {
        let note = |title: &str, content: &str| json!({"title": title, "content": content});
        assert_eq!(
            note_suspect(&note(
                "telegram",
                "ghp_abcdefghijklmnopqrstuvwxyz0123456789"
            ))
            .unwrap()
            .why,
            "a token shape"
        );
        assert_eq!(
            note_suspect(&note("wifi", "the password is Cavolo-23"))
                .unwrap()
                .why,
            "a value said in words"
        );
        let s = note_suspect(&note("glm api", "shortvalue")).unwrap();
        assert_eq!((s.why, s.whole), ("its title names a credential", false));
        let s = note_suspect(&note("Password", "gmail abc\nspotify def\nother ghi")).unwrap();
        assert!(s.whole);
        assert!(note_suspect(&note("Spesa", "latte e pane")).is_none());
        assert!(note_suspect(&note("Password", "")).is_none());
        assert!(note_suspect(&note("Password", "[secret:KEEP_X] (moved to Infisical)")).is_none());
        let list = json!({"title": "x", "items": [{"text": "api key: Abc12345"}]});
        assert!(note_suspect(&list).is_some());
    }
}
