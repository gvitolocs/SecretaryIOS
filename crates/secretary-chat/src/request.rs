//! The JSON entry point: one request in, one response out (`secretary_chat_request` in `secretary-ffi`).
//!
//! | Request | Response |
//! |---|---|
//! | `{"op":"merge","current":[Key],"incoming":[Key]}` | `{"ids":[..],"replaced":[{"local":..,"server":..}],"reply_to_updates":[{"id":..,"reply_to":..}]}` |
//! | `{"op":"sync_next","plan":null,"cursor":"c0","outcome":{"kind":"started"}}` (the first call), then `{"op":"sync_next","plan":<plan>,"outcome":{..}}` | `{"plan":<plan>,"action":{"kind":"fetch","after":"c0","limit":200}}`, `{"kind":"apply","reset":false}`, `{"kind":"done"}` or `{"kind":"abort"}` |
//! | `{"op":"older","ids":[..],"stored":null,"page_len":60,"page_size":60}` | `{"older_cursor":"tl_3","has_older":true}` |
//! | anything else, or invalid JSON | `{"error":".."}` |
//!
//! A [`Key`](crate::merge::Key) is `{"id","ts_us","role":"user"|"lucina","client_message_id"?,"reply_to"?,"share_id"?}`.
//! A sync `outcome` is `{"kind":"started"}`, `{"kind":"page","cursor":"c1"|null,"has_more":true}`,
//! `{"kind":"applied"}`, `{"kind":"gone410"}` or `{"kind":"failed"}`. The `plan` is opaque: pass back what the
//! last response returned.
use crate::{
    merge::{self, Key},
    page,
    sync::{Outcome, SyncPlan},
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Merge {
        current: Vec<Key>,
        incoming: Vec<Key>,
    },
    SyncNext {
        #[serde(default)]
        plan: Option<SyncPlan>,
        /// Only read on the first call (`plan` null): the store's last cursor.
        #[serde(default)]
        cursor: Option<String>,
        outcome: Outcome,
    },
    Older {
        ids: Vec<String>,
        #[serde(default)]
        stored: Option<String>,
        page_len: usize,
        page_size: usize,
    },
}

fn run(request: Request) -> Result<Value, String> {
    match request {
        Request::Merge { current, incoming } => {
            serde_json::to_value(merge::merge(&current, &incoming)).map_err(|e| e.to_string())
        }
        Request::SyncNext {
            plan,
            cursor,
            outcome,
        } => {
            let mut plan = plan.unwrap_or_else(|| SyncPlan::new(cursor));
            let action = plan.next(outcome);
            Ok(json!({ "plan": plan, "action": action }))
        }
        Request::Older {
            ids,
            stored,
            page_len,
            page_size,
        } => Ok(json!({
            "older_cursor": page::older_cursor(ids.iter().map(String::as_str), stored.as_deref()),
            "has_older": page::has_older(page_len, page_size, stored.as_deref()),
        })),
    }
}

/// Answers one JSON request with one JSON response. Never panics on bad input.
pub fn handle(json: &str) -> String {
    let result = serde_json::from_str::<Request>(json)
        .map_err(|e| e.to_string())
        .and_then(run);
    match result {
        Ok(value) => value.to_string(),
        Err(error) => json!({ "error": error }).to_string(),
    }
}
