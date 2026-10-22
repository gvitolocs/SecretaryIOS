//! The `model3d` block of Lucina message format v1 (`docs/lucina-message-format.md`):
//! a 3D model from the gallery, shown in the chat as an inline viewer (web),
//! a preview card (iOS) or a turntable video (Telegram, Discord).
//!
//! ```json
//! {"type": "model3d", "id": "mdl_0123456789abcdef", "title": "Knight",
//!  "prompt": "a knight in silver armor", "animations": ["Casual Walk", "Wave One Hand"],
//!  "credits": 41, "formats": ["glb", "fbx", "usdz"], "poster": true, "video": true, "rigged": true}
//! ```
//!
//! Clients never take a URL from the block: they build
//! `/v1/models/{id}/files/model.glb` (and `thumb.png`, `preview.mp4`) on their
//! own origin, behind the app's auth, so a block cannot point anywhere else.
//!
//! The rich-reply validator (`crate::rich::normalize_block`) calls
//! [`normalize`] for `model3d` blocks and [`plain_text`] for the fallback.

use serde_json::{json, Map, Value};

pub const FORMAT: &str = "v1";
pub const BLOCK_TYPE: &str = "model3d";
const MAX_TITLE: usize = 120;
const MAX_PROMPT: usize = 400;
const MAX_ANIMATIONS: usize = 10;
const MAX_ANIMATION_NAME: usize = 60;
pub const FORMATS: &[&str] = &["glb", "fbx", "usdz", "obj", "stl", "3mf"];

fn text(v: &Value, max: usize) -> Option<String> {
    let s: String = v
        .as_str()?
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() {
        return None;
    }
    if s.chars().count() <= max {
        Some(s)
    } else {
        Some(s.chars().take(max - 1).collect::<String>() + "…")
    }
}

/// `mdl_` + 8-40 lowercase letters/digits (secretary-server's model ids).
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("mdl_").is_some_and(|r| {
        (8..=40).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// A valid, normalized `model3d` block, or `None`.
pub fn normalize(raw: &Value) -> Option<Value> {
    if raw.get("type")?.as_str()? != BLOCK_TYPE {
        return None;
    }
    let id = raw.get("id")?.as_str()?;
    if !valid_id(id) {
        return None;
    }
    let mut b = Map::new();
    b.insert("type".into(), json!(BLOCK_TYPE));
    b.insert("id".into(), json!(id));
    b.insert(
        "title".into(),
        json!(text(&raw["title"], MAX_TITLE).unwrap_or_else(|| "3D model".into())),
    );
    if let Some(p) = text(&raw["prompt"], MAX_PROMPT) {
        b.insert("prompt".into(), json!(p));
    }
    let anims: Vec<String> = raw["animations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| text(n, MAX_ANIMATION_NAME))
                .take(MAX_ANIMATIONS)
                .collect()
        })
        .unwrap_or_default();
    if !anims.is_empty() {
        b.insert("animations".into(), json!(anims));
    }
    if let Some(c) = raw["credits"].as_u64().filter(|c| *c <= 10_000) {
        b.insert("credits".into(), json!(c));
    }
    let mut formats: Vec<&str> = Vec::new();
    for f in raw["formats"].as_array().into_iter().flatten() {
        if let Some(f) = f.as_str().and_then(|f| FORMATS.iter().find(|x| **x == f)) {
            if !formats.contains(f) {
                formats.push(f);
            }
        }
    }
    if !formats.is_empty() {
        b.insert("formats".into(), json!(formats));
    }
    for flag in ["poster", "video", "rigged"] {
        if raw[flag] == Value::Bool(true) {
            b.insert(flag.into(), json!(true));
        }
    }
    Some(Value::Object(b))
}

/// The `model3d` blocks of a list (at most 4).
pub fn normalize_blocks(blocks: &[Value]) -> Vec<Value> {
    blocks.iter().filter_map(normalize).take(4).collect()
}

/// Yes/no confirm buttons (the pending-action flow), at most 4.
pub fn normalize_actions(actions: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for a in actions {
        if a["kind"] != "confirm" {
            continue;
        }
        let answer = a["payload"]["answer"].as_str().unwrap_or("");
        if answer != "yes" && answer != "no" {
            continue;
        }
        let id = a["id"].as_str().unwrap_or("");
        let id_ok = !id.is_empty()
            && id.len() <= 48
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b));
        let label = text(&a["label"], 40);
        let reply = text(&a["payload"]["text"], 40);
        let (Some(label), Some(reply), true) = (label, reply, id_ok) else {
            continue;
        };
        if out.iter().any(|x| x["id"] == id) {
            continue;
        }
        let mut v = json!({"id": id, "label": label, "kind": "confirm", "payload": {"answer": answer, "text": reply}});
        if let Some(e) =
            text(&a["emoji"], 8).filter(|e| !e.chars().any(|c| c.is_ascii_alphanumeric()))
        {
            v["emoji"] = json!(e);
        }
        out.push(v);
        if out.len() == 4 {
            break;
        }
    }
    out
}

/// Yes/no buttons for a pending action (`sì`/`no` in Italian, else yes/no).
pub fn confirm_actions(lang_it: bool) -> Vec<Value> {
    let (yes, no, yes_label, no_label) = if lang_it {
        ("sì", "no", "Sì, procedi", "No")
    } else {
        ("yes", "no", "Yes, go ahead", "No")
    };
    vec![
        json!({"id": "confirm-yes", "label": yes_label, "emoji": "✅", "kind": "confirm", "payload": {"answer": "yes", "text": yes}}),
        json!({"id": "confirm-no", "label": no_label, "emoji": "✖️", "kind": "confirm", "payload": {"answer": "no", "text": no}}),
    ]
}

/// Plain-text fallback: `🧊 **Knight** — 3D model · Casual Walk, Wave One Hand · 41 credits`.
pub fn plain_text(block: &Value) -> String {
    let mut parts = vec![format!(
        "🧊 **{}** — 3D model",
        block["title"].as_str().unwrap_or("3D model")
    )];
    if let Some(a) = block["animations"].as_array().filter(|a| !a.is_empty()) {
        parts.push(
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(c) = block["credits"].as_u64() {
        parts.push(format!("{c} credits"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_are_validated() {
        let b = normalize(&json!({
            "type": "model3d", "id": "mdl_0123456789abcdef", "title": "  Knight\n", "prompt": "a knight",
            "animations": ["Casual Walk", 3, ""], "credits": 41, "formats": ["glb", "exe", "glb", "usdz"],
            "poster": true, "video": "yes", "url": "https://evil.example/x.glb"
        }))
        .unwrap();
        assert_eq!(
            b,
            json!({"type": "model3d", "id": "mdl_0123456789abcdef", "title": "Knight", "prompt": "a knight",
                   "animations": ["Casual Walk"], "credits": 41, "formats": ["glb", "usdz"], "poster": true})
        );
        assert!(normalize(&json!({"type": "model3d", "id": "../../etc"})).is_none());
        assert!(normalize(&json!({"type": "card", "id": "mdl_0123456789abcdef"})).is_none());
        assert_eq!(
            normalize_blocks(&[json!({"type": "card"}), b.clone()]),
            vec![b.clone()]
        );
        assert_eq!(
            plain_text(&b),
            "🧊 **Knight** — 3D model · Casual Walk · 41 credits"
        );
    }

    #[test]
    fn confirm_buttons() {
        let a = confirm_actions(true);
        assert_eq!(normalize_actions(&a), a);
        let bad = vec![
            json!({"id": "x", "label": "Go", "kind": "route", "payload": {"path": "/"}}),
            json!({"id": "y", "label": "Go", "kind": "confirm", "payload": {"answer": "maybe", "text": "?"}}),
            json!({"id": "bad id!", "label": "Go", "kind": "confirm", "payload": {"answer": "yes", "text": "sì"}}),
        ];
        assert!(normalize_actions(&bad).is_empty());
    }
}
