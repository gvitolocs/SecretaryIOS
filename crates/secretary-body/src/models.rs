use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::stats::{Point, Stats};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    /// Display unit only (`kg` or `lb`); stored weights are always grams.
    pub unit: String,
    #[serde(default)]
    pub goal_grams: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub weight_grams: u64,
    pub measured_ms: u64,
    pub note: String,
    pub created_ms: u64,
    pub updated_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: Settings,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub receipts: BTreeMap<String, String>,
}

impl State {
    pub fn initial() -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            settings: Settings {
                unit: "kg".into(),
                goal_grams: None,
            },
            entries: vec![],
            receipts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    #[serde(default)]
    pub op_id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default = "utc")]
    pub timezone: String,
    pub command: Command,
}
fn utc() -> String {
    "UTC".into()
}

/// Optional command fields are skipped when absent, so a command's
/// serialization (and so its receipt fingerprint) depends only on what the
/// client sent.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Snapshot,
    ParseWeight {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    LogEntry {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_grams: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        measured_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    EditEntry {
        entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_grams: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        measured_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    DeleteEntry {
        entry_id: String,
    },
    UpdateSettings {
        unit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_grams: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal: Option<String>,
    },
}

impl Command {
    pub fn is_query(&self) -> bool {
        matches!(self, Self::Snapshot | Self::ParseWeight { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct View {
    pub revision: u64,
    pub settings: Settings,
    /// Newest first.
    pub entries: Vec<Entry>,
    pub stats: Stats,
    /// Oldest first, with the 7-day average at each entry.
    pub series: Vec<Point>,
    pub labels: BTreeMap<String, String>,
    pub stat_labels: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub replayed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<View>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_grams: Option<u64>,
}

/// What Lucina sees (`GET /v1/insights/summary` → `body`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyInsights {
    pub unit: String,
    pub stats: Stats,
    pub stat_labels: BTreeMap<String, String>,
}
