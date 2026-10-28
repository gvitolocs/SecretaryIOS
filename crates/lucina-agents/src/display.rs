//! How a tool call reads in a timeline or activity log: a display name and a
//! short summary (Paseo `tool-call-display.js`, `tool-name-normalization.js`,
//! `paseo-tool-presentation/{index,catalog,agent-message,summary-text}.js`,
//! `path-utils.js` `stripCwdPrefix`).

use serde_json::{Map, Number, Value};

use crate::jsstr;
use crate::timeline::{AgentMessage, AgentMessageEvent, ToolCallDetail};

/// A display name and an optional summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub display_name: String,
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool names

pub fn normalize_tool_name(name: &str) -> String {
    jsstr::trim(name).to_lowercase()
}

fn tool_tokens(name: &str) -> Vec<String> {
    let normalized = normalize_tool_name(name);
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in normalized.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_speak_tool_name(name: &str) -> bool {
    tool_tokens(name).last().map(String::as_str) == Some("speak")
}

fn is_likely_namespaced_tool_name(name: &str) -> bool {
    let normalized = normalize_tool_name(name);
    if normalized.contains(['.', ':', '/']) {
        return true;
    }
    if !normalized.contains("__") {
        return false;
    }
    let segments: Vec<&str> = normalized.split("__").filter(|s| !s.is_empty()).collect();
    segments.len() >= 3 || (segments.len() == 2 && segments[1].contains('_'))
}

/// Paseo `isLikelyExternalToolName`: MCP-style namespaced names, and `speak`.
pub fn is_likely_external_tool_name(name: &str) -> bool {
    let normalized = normalize_tool_name(name);
    if normalized.is_empty() {
        return false;
    }
    is_speak_tool_name(&normalized) || is_likely_namespaced_tool_name(&normalized)
}

fn is_paseo_namespace(segment: &str) -> bool {
    segment == "paseo" || segment.starts_with("paseo_")
}

/// Paseo `getPaseoToolLeafName` guarded by `isPaseoToolName`: `create_agent` for
/// `mcp__paseo__create_agent` or `paseo.create_agent`; `None` otherwise.
pub fn paseo_tool_leaf_name(name: &str) -> Option<String> {
    let normalized = normalize_tool_name(name);
    if is_speak_tool_name(&normalized) {
        return None;
    }
    if normalized.contains("__") {
        let segments: Vec<&str> = normalized.split("__").filter(|s| !s.is_empty()).collect();
        if segments.len() >= 3 && segments[0] == "mcp" && is_paseo_namespace(segments[1]) {
            return Some(segments[2..].join("__"));
        }
        return None;
    }
    if normalized.contains('.') {
        let mut parts = normalized.split('.');
        let first = parts.next().unwrap_or_default();
        if is_paseo_namespace(first) {
            return Some(parts.collect::<Vec<_>>().join("."));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Summaries

/// Paseo `MAX_SUMMARY_CHARS`.
pub const MAX_SUMMARY_CHARS: usize = 120;
const SCAN_WINDOW_CHARS: usize = MAX_SUMMARY_CHARS * 4;

/// Paseo `toSummaryText`: whitespace collapsed, at most 120 UTF-16 units, an
/// ellipsis when cut.
pub fn to_summary_text(value: &str) -> Option<String> {
    let is_partial = jsstr::len(value) > SCAN_WINDOW_CHARS;
    let window = jsstr::slice(value, 0, SCAN_WINDOW_CHARS);
    let collapsed = jsstr::collapse_spaces(&window);
    let collapsed = jsstr::trim(&collapsed);
    if collapsed.is_empty() {
        return None;
    }
    if jsstr::len(collapsed) <= MAX_SUMMARY_CHARS && !is_partial {
        return Some(collapsed.to_string());
    }
    let kept = jsstr::slice_whole(collapsed, 0, MAX_SUMMARY_CHARS - 1);
    Some(format!("{}…", jsstr::trim_end(&kept)))
}

/// `String(n)` for a JSON number: integral floats print without `.0`.
pub fn js_number_string(n: &Number) -> String {
    if let Some(f) = n.as_f64() {
        if !n.is_i64() && !n.is_u64() && f.fract() == 0.0 && f.abs() < 1e21 {
            return format!("{}", f as i64);
        }
    }
    n.to_string()
}

/// Paseo `stripCwdPrefix`.
pub fn strip_cwd_prefix(file_path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return file_path.to_string();
    };
    if file_path.is_empty() {
        return file_path.to_string();
    }
    let normalized_cwd = cwd.replace('\\', "/");
    let normalized_cwd = normalized_cwd.trim_end_matches('/');
    let normalized_path = file_path.replace('\\', "/");
    let prefix = format!("{normalized_cwd}/");
    if let Some(rest) = normalized_path.strip_prefix(&prefix) {
        return rest.to_string();
    }
    if normalized_path == normalized_cwd {
        return ".".to_string();
    }
    file_path.to_string()
}

fn read_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.clone().filter(|s| !s.is_empty())
}

/// First letter up, as `.replace(/^./, c => c.toUpperCase())`.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c != '\n' && c != '\r' && c != '\u{2028}' && c != '\u{2029}' => {
            c.to_uppercase().chain(chars).collect()
        }
        _ => s.to_string(),
    }
}

fn split_on_name_punctuation(s: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in s.chars() {
        if matches!(c, '.' | '_' | '-') {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// Paseo `humanizeToolName`.
pub fn humanize_tool_name(name: &str) -> String {
    let trimmed = jsstr::trim(name);
    if trimmed.is_empty() {
        return name.to_string();
    }
    if trimmed.contains([':', '.', '/']) || trimmed.contains("__") {
        return trimmed.to_string();
    }
    let spaced = split_on_name_punctuation(trimmed);
    let joined = spaced
        .split(' ')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    capitalize(&joined)
}

fn humanize_leaf_name(leaf: &str) -> String {
    let spaced = split_on_name_punctuation(leaf);
    capitalize(&jsstr::trim(&spaced).to_lowercase())
}

// ---------------------------------------------------------------------------
// Agent messages

/// Paseo `buildAgentMessageDisplay`.
pub fn agent_message_display(message: &AgentMessage) -> Display {
    let display_name = match message.event {
        AgentMessageEvent::Message => "Message received",
        AgentMessageEvent::Finished => "Agent finished",
        AgentMessageEvent::Errored => "Agent errored",
        AgentMessageEvent::PermissionRequired => "Agent needs permission",
        AgentMessageEvent::Closed => "Agent closed",
    };
    let who = message.sender.title.as_deref().unwrap_or(&message.sender.id);
    Display { display_name: display_name.to_string(), summary: to_summary_text(who) }
}

// ---------------------------------------------------------------------------
// The Paseo tool catalog (`paseo-tool-presentation/catalog.js`)

enum Context {
    None,
    Field(&'static [&'static str]),
    TargetAgent,
    FieldOrTargetAgent(&'static str),
    Workspace,
    Viewport,
}

fn catalog(leaf: &str) -> Option<(&'static str, Context)> {
    use Context::*;
    Some(match leaf {
        "create_agent" => ("Create agent", Field(&["title", "initialPrompt"])),
        "send_agent_prompt" => ("Send message", Field(&["prompt"])),
        "get_agent_status" => ("Check agent", TargetAgent),
        "get_agent_activity" => ("Read agent activity", TargetAgent),
        "list_agents" => ("List agents", None),
        "update_agent" => ("Update agent", FieldOrTargetAgent("name")),
        "set_agent_mode" => ("Set agent mode", Field(&["modeId"])),
        "cancel_agent" => ("Cancel agent", TargetAgent),
        "archive_agent" => ("Archive agent", TargetAgent),
        "kill_agent" => ("Kill agent", TargetAgent),
        "list_pending_permissions" => ("List permissions", None),
        "respond_to_permission" => ("Respond to permission", TargetAgent),
        "create_workspace" => ("Create workspace", Workspace),
        "list_hosts" => ("List hosts", None),
        "list_workspaces" => ("List workspaces", None),
        "rename_workspace" => ("Rename workspace", Field(&["title"])),
        "archive_workspace" => ("Archive workspace", Field(&["workspaceId"])),
        "list_workspace_scripts" => ("List scripts", None),
        "start_workspace_script" => ("Start script", Field(&["scriptName"])),
        "stop_workspace_script" => ("Stop script", Field(&["scriptName"])),
        "create_terminal" => ("Create terminal", Field(&["name", "command"])),
        "list_terminals" => ("List terminals", None),
        "capture_terminal" => ("Read terminal", Field(&["terminalId"])),
        "send_terminal_keys" => ("Send keys", Field(&["keys"])),
        "kill_terminal" => ("Kill terminal", Field(&["terminalId"])),
        "create_schedule" => ("Create schedule", Field(&["name", "cron"])),
        "create_heartbeat" => ("Create heartbeat", Field(&["name", "cron"])),
        "update_schedule" => ("Update schedule", Field(&["name", "id"])),
        "list_schedules" => ("List schedules", None),
        "inspect_schedule" => ("Inspect schedule", Field(&["id"])),
        "schedule_logs" => ("Read schedule logs", Field(&["id"])),
        "run_schedule_once" => ("Run schedule", Field(&["id"])),
        "pause_schedule" => ("Pause schedule", Field(&["id"])),
        "resume_schedule" => ("Resume schedule", Field(&["id"])),
        "delete_schedule" => ("Delete schedule", Field(&["id"])),
        "delete_heartbeat" => ("Delete heartbeat", Field(&["id"])),
        "list_providers" => ("List providers", None),
        "list_models" => ("List models", Field(&["provider"])),
        "list_profiles" => ("List profiles", None),
        "inspect_provider" => ("Inspect provider", Field(&["provider"])),
        "browser_list_tabs" => ("List browser tabs", None),
        "browser_new_tab" => ("Open browser tab", Field(&["url"])),
        "browser_close_tab" => ("Close browser tab", Field(&["browserId"])),
        "browser_navigate" => ("Navigate", Field(&["url"])),
        "browser_back" => ("Go back", None),
        "browser_forward" => ("Go forward", None),
        "browser_reload" => ("Reload page", None),
        "browser_snapshot" => ("Snapshot page", None),
        "browser_screenshot" => ("Take screenshot", None),
        "browser_click" => ("Click element", Field(&["ref"])),
        "browser_hover" => ("Hover element", Field(&["ref"])),
        "browser_drag" => ("Drag element", Field(&["sourceRef"])),
        "browser_fill" => ("Fill field", Field(&["ref"])),
        "browser_select" => ("Select option", Field(&["value", "ref"])),
        "browser_type" => ("Type text", Field(&["text"])),
        "browser_keypress" => ("Press key", Field(&["key"])),
        "browser_upload" => ("Upload files", Field(&["filePaths"])),
        "browser_wait" => ("Wait for page", Field(&["text", "url"])),
        "browser_scroll" => ("Scroll page", None),
        "browser_resize" => ("Resize browser", Viewport),
        "browser_logs" => ("Read browser logs", None),
        "browser_evaluate" => ("Run JavaScript", Field(&["function"])),
        _ => return Option::None,
    })
}

fn is_present(v: &Value) -> bool {
    match v {
        Value::String(s) => !jsstr::trim(s).is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Number(_) => true,
        _ => false,
    }
}

fn base_name(v: Option<&Value>) -> Option<Value> {
    let Some(Value::String(s)) = v else { return None };
    s.split(['\\', '/'])
        .rfind(|seg| !seg.is_empty())
        .map(|seg| Value::String(seg.to_string()))
}

fn context_value(ctx: &Context, input: &Map<String, Value>, target: Option<&str>) -> Option<Value> {
    let target_value = || target.map(|t| Value::String(t.to_string()));
    match ctx {
        Context::None => None,
        Context::Field(keys) => keys.iter().filter_map(|k| input.get(*k)).find(|v| is_present(v)).cloned(),
        Context::TargetAgent => target_value(),
        Context::FieldOrTargetAgent(key) => match input.get(*key) {
            Some(v) if is_present(v) => Some(v.clone()),
            _ => target_value(),
        },
        Context::Workspace => [
            input.get("title").cloned(),
            input.get("branchName").cloned(),
            input.get("branch").cloned(),
            base_name(input.get("path")),
        ]
        .into_iter()
        .flatten()
        .find(is_present),
        Context::Viewport => match (input.get("width"), input.get("height")) {
            (Some(Value::Number(w)), Some(Value::Number(h))) => Some(Value::String(format!(
                "{}×{}",
                js_number_string(w),
                js_number_string(h)
            ))),
            _ => None,
        },
    }
}

fn format_context(v: Option<Value>) -> Option<String> {
    match v? {
        Value::String(s) => to_summary_text(&s),
        Value::Number(n) => Some(js_number_string(&n)),
        Value::Array(items) => {
            let head = match items.first() {
                Some(Value::String(s)) => to_summary_text(s)?,
                _ => return None,
            };
            Some(if items.len() > 1 { format!("{head} +{}", items.len() - 1) } else { head })
        }
        _ => None,
    }
}

/// Paseo `buildPaseoToolDisplay`: `None` for tools outside the Paseo namespace.
pub fn paseo_tool_display(
    tool_name: &str,
    detail: &ToolCallDetail,
    target_agent_title: Option<&str>,
) -> Option<Display> {
    let leaf = paseo_tool_leaf_name(tool_name)?;
    let Some((label, ctx)) = catalog(&leaf) else {
        return Some(Display { display_name: humanize_leaf_name(&leaf), summary: None });
    };
    let empty = Map::new();
    let input = match detail {
        ToolCallDetail::Unknown { input: Value::Object(map), .. } => map,
        _ => &empty,
    };
    let target = target_agent_title
        .map(str::to_string)
        .or_else(|| read_string(input.get("agentId")));
    let summary = format_context(context_value(&ctx, input, target.as_deref()));
    Some(Display { display_name: label.to_string(), summary })
}

// ---------------------------------------------------------------------------
// The display model

/// What [`tool_call_display`] reads from a tool call.
pub struct ToolCallDisplayInput<'a> {
    pub name: &'a str,
    pub detail: &'a ToolCallDetail,
    pub metadata: Option<&'a Map<String, Value>>,
    pub agent_message: Option<&'a AgentMessage>,
    pub cwd: Option<&'a str>,
    pub target_agent_title: Option<&'a str>,
}

fn canonical_detail_display(detail: &ToolCallDetail, cwd: Option<&str>) -> (Option<String>, Option<String>) {
    let some = |s: &str| Some(s.to_string());
    match detail {
        ToolCallDetail::Shell { command, .. } => (some("Shell"), Some(command.clone())),
        ToolCallDetail::Read { file_path, .. } => (some("Read"), Some(strip_cwd_prefix(file_path, cwd))),
        ToolCallDetail::Edit { file_path, .. } => (some("Edit"), Some(strip_cwd_prefix(file_path, cwd))),
        ToolCallDetail::Write { file_path, .. } => (some("Write"), Some(strip_cwd_prefix(file_path, cwd))),
        ToolCallDetail::Search(s) => (some("Search"), Some(s.query.clone())),
        ToolCallDetail::Fetch { url, .. } => (some("Fetch"), Some(url.clone())),
        ToolCallDetail::WorktreeSetup { branch_name, .. } => (some("Worktree setup"), Some(branch_name.clone())),
        ToolCallDetail::SubAgent { sub_agent_type, description, .. } => (
            Some(non_empty(sub_agent_type).unwrap_or_else(|| "Task".to_string())),
            non_empty(description),
        ),
        ToolCallDetail::PlainText { label, .. } => (None, label.clone()),
        ToolCallDetail::Plan { .. } => (some("Plan"), None),
        ToolCallDetail::Unknown { .. } => (None, None),
    }
}

fn unknown_detail_override(input: &ToolCallDisplayInput<'_>) -> (Option<String>, Option<String>) {
    let lower = normalize_tool_name(input.name);
    let is_unknown = matches!(input.detail, ToolCallDetail::Unknown { .. });
    if is_unknown && lower == "task" {
        let summary = input.metadata.and_then(|m| read_string(m.get("subAgentActivity")));
        return (Some("Task".to_string()), summary);
    }
    if is_unknown && lower == "thinking" {
        return (Some("Thinking".to_string()), None);
    }
    if lower == "terminal" {
        let summary = match input.detail {
            ToolCallDetail::PlainText { label, .. } => non_empty(label),
            _ => None,
        };
        return (Some("Terminal".to_string()), summary);
    }
    (None, None)
}

/// Paseo `buildToolCallDisplayModel` (without `errorText`, which no Lucina text
/// output uses yet).
pub fn tool_call_display(input: &ToolCallDisplayInput<'_>) -> Display {
    let paseo = match input.agent_message {
        Some(message) => Some(agent_message_display(message)),
        None => paseo_tool_display(input.name, input.detail, input.target_agent_title),
    };
    if let Some(d) = paseo {
        return Display { display_name: d.display_name, summary: d.summary.filter(|s| !s.is_empty()) };
    }
    let (canon_name, canon_summary) = canonical_detail_display(input.detail, input.cwd);
    let (over_name, over_summary) = unknown_detail_override(input);
    let display_name = over_name
        .or(canon_name)
        .unwrap_or_else(|| humanize_tool_name(input.name));
    let summary = over_summary.or(canon_summary).filter(|s| !s.is_empty());
    Display { display_name, summary }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names() {
        assert_eq!(humanize_tool_name("read_file"), "Read file");
        assert_eq!(humanize_tool_name("mcp__x__y"), "mcp__x__y");
        assert_eq!(humanize_tool_name("WebSearch"), "Websearch");
        assert!(is_likely_external_tool_name("mcp__github__create_issue"));
        assert!(is_likely_external_tool_name("speak"));
        assert!(!is_likely_external_tool_name("Bash"));
        assert_eq!(paseo_tool_leaf_name("mcp__paseo__create_agent").as_deref(), Some("create_agent"));
        assert_eq!(paseo_tool_leaf_name("paseo.list_agents").as_deref(), Some("list_agents"));
        assert_eq!(paseo_tool_leaf_name("mcp__other__x"), None);
    }

    #[test]
    fn summaries() {
        assert_eq!(to_summary_text("  a \n b "), Some("a b".to_string()));
        let long = "x".repeat(130);
        assert_eq!(to_summary_text(&long), Some(format!("{}…", "x".repeat(119))));
        assert_eq!(strip_cwd_prefix("/w/src/a.rs", Some("/w/")), "src/a.rs");
        assert_eq!(strip_cwd_prefix("/w", Some("/w")), ".");
        assert_eq!(js_number_string(&Number::from_f64(3.0).unwrap()), "3");
    }

    #[test]
    fn paseo_catalog() {
        let detail: ToolCallDetail = serde_json::from_value(json!({"type":"unknown",
            "input":{"title":"","initialPrompt":"Fix the bug"},"output":null})).unwrap();
        let d = paseo_tool_display("mcp__paseo__create_agent", &detail, None).unwrap();
        assert_eq!(d, Display { display_name: "Create agent".into(), summary: Some("Fix the bug".into()) });
        let d = paseo_tool_display("mcp__paseo__brand_new_tool", &detail, None).unwrap();
        assert_eq!(d.display_name, "Brand new tool");
    }
}
