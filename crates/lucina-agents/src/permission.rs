//! Permission requests and responses (Paseo `AgentPermissionRequestPayloadSchema`,
//! `AgentPermissionResponseSchema`). Questions (AskUserQuestion-style) use the same
//! channel with `kind: "question"`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::timeline::ToolCallDetail;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub kind: PermissionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ToolCallDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<Map<String, Value>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<PermissionAction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionKind {
    Tool,
    Plan,
    Question,
    Mode,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionAction {
    pub id: String,
    pub label: String,
    pub behavior: Behavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<ActionVariant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<ActionIntent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Behavior {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionVariant {
    Primary,
    Secondary,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionIntent {
    Implement,
    ImplementResume,
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "behavior",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum PermissionResponse {
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_action_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_permissions: Option<Vec<Map<String, Value>>>,
    },
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_action_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
    },
}

impl PermissionResponse {
    /// The deny Paseo sends for every pending request when a turn fails
    /// ("Turn failed") or is interrupted ("Interrupted").
    pub fn auto_deny(message: &str) -> Self {
        PermissionResponse::Deny {
            selected_action_id: None,
            message: Some(message.to_string()),
            interrupt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_and_responses_round_trip() {
        let req = json!({"id":"p1","provider":"mock","name":"ExitPlanMode","kind":"plan","title":"Plan",
            "input":{"plan":"x"},"detail":{"type":"plan","text":"x"},
            "actions":[{"id":"impl","label":"Implement","behavior":"allow","variant":"primary","intent":"implement"}]});
        let parsed: PermissionRequest = serde_json::from_value(req.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), req);

        for resp in [
            json!({"behavior":"allow","selectedActionId":"impl","updatedInput":{"a":1}}),
            json!({"behavior":"deny","message":"Interrupted"}),
            json!({"behavior":"deny","interrupt":true}),
        ] {
            let parsed: PermissionResponse = serde_json::from_value(resp.clone()).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), resp);
        }
        assert_eq!(
            serde_json::to_value(PermissionResponse::auto_deny("Turn failed")).unwrap(),
            json!({"behavior":"deny","message":"Turn failed"})
        );
    }
}
