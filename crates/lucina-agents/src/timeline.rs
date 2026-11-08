//! Timeline items, as Paseo's `AgentTimelineItemPayloadSchema` (protocol `messages.js`).
//!
//! Every agent has one timeline: user and assistant messages, reasoning, tool
//! calls, task lists, errors, notifications, compactions and plugin items. Rows carry a
//! sequence number that grows by one per item; a [`Cursor`] is `{epoch, seq}`, and
//! a cursor from another epoch is stale. Paseo keeps timelines only in memory and
//! starts a new epoch after a restart; lucina-agentd stores them, so its epoch
//! changes only when a timeline is rebuilt.
//!
//! Numbers stay [`serde_json::Number`] so `1` round-trips as `1`, not `1.0`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// `field: z.x().nullable().optional()`: absent, `null`, or a value.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TimelineItem {
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_message_id: Option<String>,
    },
    AssistantMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
    },
    Reasoning {
        text: String,
    },
    ToolCall(Box<ToolCall>),
    Todo {
        items: Vec<TodoEntry>,
    },
    Error {
        message: String,
    },
    Notification {
        level: NotificationLevel,
        message: String,
    },
    Compaction {
        status: CompactionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<CompactionTrigger>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pre_tokens: Option<Number>,
    },
    Plugin {
        id: String,
        plugin_id: String,
        kind: String,
        version: Number,
        data: Value,
    },
}

impl TimelineItem {
    /// The wire `type` (`user_message`, `tool_call`, ...).
    pub fn kind(&self) -> &'static str {
        match self {
            TimelineItem::UserMessage { .. } => "user_message",
            TimelineItem::AssistantMessage { .. } => "assistant_message",
            TimelineItem::Reasoning { .. } => "reasoning",
            TimelineItem::ToolCall(_) => "tool_call",
            TimelineItem::Todo { .. } => "todo",
            TimelineItem::Error { .. } => "error",
            TimelineItem::Notification { .. } => "notification",
            TimelineItem::Compaction { .. } => "compaction",
            TimelineItem::Plugin { .. } => "plugin",
        }
    }

    /// Paseo `timelineItemIdentity`: tool calls by `callId`, plugin items by
    /// `pluginId/id`; other items have none.
    pub fn identity(&self) -> Option<String> {
        match self {
            TimelineItem::ToolCall(call) => Some(call.call_id.clone()),
            TimelineItem::Plugin { plugin_id, id, .. } => Some(format!("{plugin_id}/{id}")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_message: Option<AgentMessage>,
    pub call_id: String,
    pub name: String,
    pub detail: ToolCallDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    pub status: ToolCallStatus,
    /// `null` unless the status is `failed`.
    #[serde(default)]
    pub error: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

impl ToolCallStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolCallStatus::Running => "running",
            ToolCallStatus::Completed => "completed",
            ToolCallStatus::Failed => "failed",
            ToolCallStatus::Canceled => "canceled",
        }
    }
}

/// A message one agent sends another (protocol `agent-message.js`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub event: AgentMessageEvent,
    pub sender: AgentMessageSender,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMessageEvent {
    Message,
    Finished,
    Errored,
    PermissionRequired,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageSender {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolCallDetail {
    WorktreeSetup {
        worktree_path: String,
        branch_name: String,
        log: String,
        commands: Vec<WorktreeSetupCommand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
    },
    Shell {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        exit_code: Option<Option<Number>>,
    },
    Read {
        file_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<Number>,
    },
    Edit {
        file_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_string: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_string: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unified_diff: Option<String>,
    },
    Write {
        file_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    Search(SearchDetail),
    Fetch {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<Number>,
    },
    SubAgent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub_agent_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        log: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actions: Option<Vec<SubAgentAction>>,
    },
    PlainText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    Plan {
        text: String,
    },
    Unknown {
        #[serde(default)]
        input: Value,
        #[serde(default)]
        output: Value,
    },
}

impl ToolCallDetail {
    pub fn kind(&self) -> &'static str {
        match self {
            ToolCallDetail::WorktreeSetup { .. } => "worktree_setup",
            ToolCallDetail::Shell { .. } => "shell",
            ToolCallDetail::Read { .. } => "read",
            ToolCallDetail::Edit { .. } => "edit",
            ToolCallDetail::Write { .. } => "write",
            ToolCallDetail::Search(_) => "search",
            ToolCallDetail::Fetch { .. } => "fetch",
            ToolCallDetail::SubAgent { .. } => "sub_agent",
            ToolCallDetail::PlainText { .. } => "plain_text",
            ToolCallDetail::Plan { .. } => "plan",
            ToolCallDetail::Unknown { .. } => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchDetail {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<SearchToolName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_results: Option<Vec<WebResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_files: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_matches: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SearchMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchToolName {
    Search,
    Grep,
    Glob,
    WebSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Content,
    FilesWithMatches,
    Count,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebResult {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubAgentAction {
    pub index: Number,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSetupCommand {
    pub index: Number,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub log: String,
    pub status: WorktreeSetupStatus,
    pub exit_code: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<Number>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeSetupStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoEntry {
    pub text: String,
    pub completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TodoStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionStatus {
    Loading,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionTrigger {
    Auto,
    Manual,
}

/// One stored timeline row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRow {
    pub seq: u64,
    pub timestamp: String,
    pub item: TimelineItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// A position in one agent's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(v: Value) {
        let item: TimelineItem = serde_json::from_value(v.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&item).unwrap(), v, "round trip of {v}");
    }

    #[test]
    fn every_item_round_trips() {
        round_trip(json!({"type":"user_message","text":"hi","messageId":"m1","clientMessageId":"c1"}));
        round_trip(json!({"type":"assistant_message","text":"yo"}));
        round_trip(json!({"type":"reasoning","text":"hmm"}));
        round_trip(json!({"type":"todo","items":[{"text":"a","completed":false,"status":"in_progress","activeForm":"Doing a"}]}));
        round_trip(json!({"type":"error","message":"boom"}));
        round_trip(json!({"type":"notification","level":"warning","message":"careful"}));
        round_trip(json!({"type":"compaction","status":"completed","trigger":"auto","preTokens":1200}));
        round_trip(json!({"type":"plugin","id":"p1","pluginId":"demo","kind":"card","version":1,"data":{"a":[1,2]}}));
    }

    #[test]
    fn every_tool_detail_round_trips() {
        let details = [
            json!({"type":"worktree_setup","worktreePath":"/w","branchName":"b","log":"",
                   "commands":[{"index":1,"command":"make","cwd":"/w","log":"ok","status":"completed","exitCode":0,"durationMs":12}],"truncated":false}),
            json!({"type":"shell","command":"ls","cwd":"/w","output":"a\n","exitCode":null}),
            json!({"type":"shell","command":"ls","exitCode":2}),
            json!({"type":"shell","command":"ls"}),
            json!({"type":"read","filePath":"/w/a.rs","offset":10,"limit":20}),
            json!({"type":"edit","filePath":"/w/a.rs","oldString":"a","newString":"b","unifiedDiff":"@@"}),
            json!({"type":"write","filePath":"/w/b.rs","content":"x"}),
            json!({"type":"search","query":"foo","toolName":"web_search","webResults":[{"title":"t","url":"u"}],"numFiles":3,"mode":"files_with_matches"}),
            json!({"type":"fetch","url":"https://x","code":200,"codeText":"OK","bytes":10,"durationMs":1.5}),
            json!({"type":"sub_agent","subAgentType":"Explore","description":"look","log":"done","actions":[]}),
            json!({"type":"plain_text","label":"Terminal","text":"x","icon":"square_terminal"}),
            json!({"type":"plan","text":"1. do"}),
            json!({"type":"unknown","input":null,"output":{"ok":true}}),
        ];
        for detail in details {
            round_trip(json!({"type":"tool_call","callId":"c","name":"n","detail":detail,"status":"running","error":null}));
        }
        round_trip(json!({"type":"tool_call","callId":"c","name":"n","detail":{"type":"plan","text":"p"},
                          "metadata":{"k":1},"status":"failed","error":"denied"}));
        round_trip(json!({"type":"tool_call","agentMessage":{"event":"permission-required","sender":{"id":"a1","title":"Child"},"text":"t"},
                          "callId":"c","name":"n","detail":{"type":"unknown","input":{},"output":null},"status":"completed","error":null}));
    }

    #[test]
    fn identity_and_kind() {
        let call: TimelineItem = serde_json::from_value(json!({"type":"tool_call","callId":"c9","name":"n",
            "detail":{"type":"plan","text":"p"},"status":"running","error":null})).unwrap();
        assert_eq!(call.identity().as_deref(), Some("c9"));
        assert_eq!(call.kind(), "tool_call");
        let plugin: TimelineItem = serde_json::from_value(json!({"type":"plugin","id":"x","pluginId":"p","kind":"k","version":2,"data":null})).unwrap();
        assert_eq!(plugin.identity().as_deref(), Some("p/x"));
        assert_eq!(TimelineItem::Reasoning { text: String::new() }.identity(), None);
    }
}
