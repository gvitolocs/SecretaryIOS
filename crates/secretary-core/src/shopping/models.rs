//! Domain models. Deliberately our own names/shapes — the proxy translates
//! to the upstream wire format, the core never sees it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub user_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LucinaList {
    pub uuid: String,
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LucinaItem {
    /// Stable id — the dedup key for the sync queue (see sync.rs).
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub section: Option<String>,
}

impl LucinaItem {
    pub fn new(name: impl Into<String>, spec: impl Into<String>) -> Self {
        Self {
            uuid: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            spec: spec.into(),
            checked: false,
            section: None,
        }
    }
}

/// A committed item mutation. `op_id` makes it idempotent: replaying an op
/// after a retry/timeout must not duplicate its effect (the property the TLC
/// model in `formal/` proves for this queue).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemOp {
    pub op_id: String,
    pub item_uuid: String,
    pub kind: OpKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OpKind {
    Add,
    Check,
    Uncheck,
    Delete,
}

/// Fold an op into local state. Idempotent by construction: applying the
/// same op twice leaves state unchanged (Add is a no-op if present, Delete
/// a no-op if absent, Check/Uncheck absolute).
pub fn apply_op(items: &mut Vec<LucinaItem>, op: &ItemOp) {
    let pos = items.iter().position(|i| i.uuid == op.item_uuid);
    match (op.kind, pos) {
        (OpKind::Add, None) => {
            items.push(LucinaItem {
                uuid: op.item_uuid.clone(),
                name: String::new(),
                spec: String::new(),
                checked: false,
                section: None,
            });
        }
        (OpKind::Add, Some(_)) => {}
        (OpKind::Check, Some(i)) => items[i].checked = true,
        (OpKind::Uncheck, Some(i)) => items[i].checked = false,
        // Check/Uncheck of an item a Delete already removed: no-op.
        (OpKind::Check, None) | (OpKind::Uncheck, None) => {}
        (OpKind::Delete, Some(i)) => {
            items.swap_remove(i);
        }
        (OpKind::Delete, None) => {}
    }
}
