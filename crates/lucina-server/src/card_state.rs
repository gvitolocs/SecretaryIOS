//! What a timeline item's buttons look like now, the same on every client (flaws "decided decisions
//! still offer their buttons" and "state differs between clients"). Timeline items are append-only,
//! so this is an overlay: `GET /api/secretary/timeline/cards` draws it from the decisions log and from
//! the buttons the owner used, which clients report with `POST /api/secretary/timeline/cards/{item}`.
//! `event: cards` on the timeline stream tells clients to read it again; a secret card's queue
//! changing sends it too.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use lucina_store::ulid::is_timeline_id;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::api::ApiError;
use crate::decide::Decision;

/// Buttons kept; the oldest go first. Clients keep their own copy of what they used.
const MAX_CHOICES: usize = 2000;
const MAX_ACTION_CHARS: usize = 128;

fn channel() -> &'static broadcast::Sender<()> {
    static TX: OnceLock<broadcast::Sender<()>> = OnceLock::new();
    TX.get_or_init(|| broadcast::channel(16).0)
}

/// Something a client draws changed: open streams send `event: cards`.
pub fn changed() {
    let _ = channel().send(());
}

pub fn subscribe() -> broadcast::Receiver<()> {
    channel().subscribe()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Choice {
    action: String,
    at: i64,
}

pub struct Choices {
    path: PathBuf,
    items: Mutex<BTreeMap<String, Choice>>,
}

impl Choices {
    fn open(dir: &Path) -> Self {
        let path = dir.join("card-choices.json");
        let items = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path, items: Mutex::new(items) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Choice>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The first button used on an item stays: two clients agree on which one it was.
    fn choose(&self, item: &str, action: &str) -> std::io::Result<String> {
        let mut items = self.lock();
        if let Some(c) = items.get(item) {
            return Ok(c.action.clone());
        }
        items.insert(item.to_owned(), Choice { action: action.to_owned(), at: Utc::now().timestamp_millis() });
        while items.len() > MAX_CHOICES {
            let oldest = items.iter().min_by_key(|(_, c)| c.at).map(|(k, _)| k.clone());
            let Some(oldest) = oldest else { break };
            items.remove(&oldest);
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&*items).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(action.to_owned())
    }
}

fn choices(dir: &Path) -> Arc<Choices> {
    static ALL: OnceLock<Mutex<HashMap<PathBuf, Arc<Choices>>>> = OnceLock::new();
    let mut m = ALL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    m.entry(dir.to_path_buf()).or_insert_with(|| Arc::new(Choices::open(dir))).clone()
}

/// The card of a decision no longer waiting. The button is the one `ask_owner` made for its option.
fn decision_card(d: &Decision) -> Option<(String, Value)> {
    let item = d.owner_item_id.clone()?;
    if d.status == "pending" {
        return None;
    }
    let mut card = json!({"closed": true, "status": d.status});
    if let Some(i) = d.chosen_index {
        card["action"] = json!(format!("{}-{i}", d.decision_id));
    }
    Some((item, card))
}

/// Every closed card, by item id. A decision's own record outranks a tap.
pub fn cards(dir: &Path) -> Value {
    let mut out = serde_json::Map::new();
    for (item, c) in choices(dir).lock().iter() {
        out.insert(item.clone(), json!({"closed": true, "status": "chosen", "action": c.action}));
    }
    for d in crate::decide::store(dir).list(None, usize::MAX, Utc::now()) {
        if let Some((item, mut card)) = decision_card(&d) {
            if card.get("action").is_none() {
                if let Some(action) = out.get(&item).and_then(|c| c.get("action")).cloned() {
                    card["action"] = action;
                }
            }
            out.insert(item, card);
        }
    }
    json!({"cards": out})
}

/// `POST /timeline/cards/{item}` with `{action}`: the owner used this button.
pub fn choose(dir: &Path, item: &str, body: &Value) -> Result<Value, ApiError> {
    if !is_timeline_id(item) {
        return Err(ApiError::bad_request("malformed item id"));
    }
    let action = body["action"].as_str().unwrap_or("").trim();
    if action.is_empty() || action.chars().count() > MAX_ACTION_CHARS {
        return Err(ApiError::bad_request("action must be a button id"));
    }
    let kept = choices(dir)
        .choose(item, action)
        .map_err(|e| ApiError::new(500, "internal_error", format!("card choices: {e}")))?;
    changed();
    Ok(json!({"closed": true, "status": "chosen", "action": kept}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(status: &str, chosen: Option<usize>) -> Decision {
        serde_json::from_value(json!({
            "decision_id": "dc-0123456789ab", "record_id": "r", "status": status, "decision": null, "chosen_option": null,
            "chosen_index": chosen, "reason": "r", "decided_by": null, "confidence": null, "asked_owner": true,
            "owner_only": false, "question": "q", "options": ["a", "b"], "recommendation": null, "impact": "high",
            "reversible": true, "urgency": "normal", "topic": null, "asking_client": "c", "asking_session": null,
            "created_at": "2026-11-02T00:00:00.000Z", "decided_at": null, "expires_at": null,
            "owner_item_id": "01M6CC55XHF1CBKXG1AA1P5A4B",
        }))
        .unwrap()
    }

    #[test]
    fn a_decided_decision_closes_its_card_on_the_button_it_chose() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::decide::store(dir.path());
        let mut d = decision("pending", None);
        store.put(&d).unwrap();
        assert_eq!(cards(dir.path())["cards"], json!({}), "still waiting: the buttons stay");
        d.status = "decided".into();
        d.chosen_index = Some(2);
        let mut rx = subscribe();
        store.put(&d).unwrap();
        assert!(rx.try_recv().is_ok(), "open streams hear about it");
        let got = &cards(dir.path())["cards"]["01M6CC55XHF1CBKXG1AA1P5A4B"];
        assert_eq!(got, &json!({"closed": true, "status": "decided", "action": "dc-0123456789ab-2"}));
    }

    #[test]
    fn withdrawn_and_expired_decisions_close_their_card_with_no_button() {
        for status in ["withdrawn", "expired"] {
            let (_, card) = decision_card(&decision(status, None)).unwrap();
            assert_eq!(card, json!({"closed": true, "status": status}));
        }
    }

    #[test]
    fn the_first_button_used_stays_for_every_client_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let item = "01M6CC55XHF1CBKXG1AA1P5A4C";
        let first = choose(dir.path(), item, &json!({"action": "yes"})).unwrap();
        assert_eq!(first["action"], "yes");
        let second = choose(dir.path(), item, &json!({"action": "no"})).unwrap();
        assert_eq!(second["action"], "yes");
        let reopened = Choices::open(dir.path());
        assert_eq!(reopened.lock()[item].action, "yes");
        assert_eq!(cards(dir.path())["cards"][item]["status"], "chosen");
    }

    #[test]
    fn bad_input_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(choose(dir.path(), "../x", &json!({"action": "yes"})).is_err());
        assert!(choose(dir.path(), "01M6CC55XHF1CBKXG1AA1P5A4D", &json!({})).is_err());
    }
}
