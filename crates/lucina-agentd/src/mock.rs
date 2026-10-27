//! The development providers, ported from Paseo 0.13.2
//! (`server/agent/providers/mock-load-test-agent.js`, `mock-slow-provider.js`), so
//! the same scenario can run through both daemons in the differential tests.
//!
//! `mock` streams a scripted, realistic agent turn (markdown, reasoning, read /
//! grep / edit / bash tool calls) for the model's duration, and reacts to prompt
//! triggers ("emit synthetic turn failure", "emit synthetic plan approval",
//! "emit synthetic questions", ...). `mock-slow` can't start sessions.
//!
//! Not ported yet (parity row P10): the stress and large-payload triggers, the
//! steering-replay shapes, prompt rejections and rewind.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use lucina_agents::jsstr;
use lucina_agents::permission::{
    ActionIntent, ActionVariant, Behavior, PermissionAction, PermissionKind, PermissionRequest,
};
use lucina_agents::timeline::{SearchDetail, SearchMode, SearchToolName, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::types::{AgentMode, Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem, Usage};
use serde_json::{json, Map, Value};
use tokio::task::JoinHandle;

use crate::provider::{EventSink, ModelInfo, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

pub const MOCK_ID: &str = "mock";
pub const MOCK_SLOW_ID: &str = "mock-slow";
pub const DEFAULT_MODEL: &str = "five-minute-stream";
const DEFAULT_MODE: &str = "load-test";
const DEFAULT_DURATION_MS: u64 = 5 * 60 * 1000;
const DEFAULT_INTERVAL_MS: u64 = 40;
/// Paseo `MOCK_LOAD_TEST_HANDLED_COMMAND`.
pub const HANDLED_COMMAND: &str = "/mock handled-command";

#[derive(Debug, Clone, Copy)]
struct Burst {
    min_tokens: u32,
    max_tokens: u32,
    gap_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct Profile {
    duration_ms: u64,
    interval_ms: u64,
    burst: Option<Burst>,
}

struct Model {
    id: &'static str,
    label: &'static str,
    selectable: bool,
    default: bool,
    thinking_default: Option<&'static str>,
    duration_ms: u64,
    interval_ms: u64,
    burst: Option<Burst>,
}

const MODELS: &[Model] = &[
    Model { id: "five-minute-stream", label: "Five minute stream", selectable: true, default: true, thinking_default: Some("low"), duration_ms: DEFAULT_DURATION_MS, interval_ms: 40, burst: None },
    Model { id: "thirty-minute-stream", label: "Thirty minute stream", selectable: true, default: false, thinking_default: None, duration_ms: 30 * 60 * 1000, interval_ms: 50, burst: None },
    Model { id: "legacy-five-minute-stream", label: "Legacy five minute stream", selectable: false, default: false, thinking_default: Some("low"), duration_ms: DEFAULT_DURATION_MS, interval_ms: 40, burst: None },
    Model { id: "one-minute-stream", label: "One minute stream", selectable: true, default: false, thinking_default: None, duration_ms: 60_000, interval_ms: 40, burst: None },
    Model { id: "bursty-stream", label: "Bursty stream", selectable: true, default: false, thinking_default: None, duration_ms: 60_000, interval_ms: 0, burst: Some(Burst { min_tokens: 1, max_tokens: 40, gap_ms: 90 }) },
    Model { id: "ten-second-stream", label: "Ten second stream", selectable: true, default: false, thinking_default: Some("low"), duration_ms: 10_000, interval_ms: 5, burst: None },
    Model { id: "max-only-thinking-stream", label: "Max-only thinking stream", selectable: true, default: false, thinking_default: Some("max"), duration_ms: 10_000, interval_ms: 5, burst: None },
    Model { id: "e2e-fast-stream", label: "E2E fast stream", selectable: false, default: false, thinking_default: None, duration_ms: 2000, interval_ms: 20, burst: None },
];

/// Paseo `resolveModelProfile`: an unknown id gets the first model.
fn profile(model_id: &str) -> Profile {
    let m = MODELS.iter().find(|m| m.id == model_id).unwrap_or(&MODELS[0]);
    Profile { duration_ms: m.duration_ms, interval_ms: m.interval_ms, burst: m.burst }
}

/// Paseo `nextBurstSize` (a deterministic hash, JS 32-bit semantics).
fn next_burst_size(b: &Burst, sequence: u64) -> u32 {
    let mut h = (((sequence + 1) * 2_654_435_761) % (1u64 << 32)) as u32;
    h ^= h >> 15;
    h = h.wrapping_mul(2_246_822_519);
    h ^= h >> 13;
    let span = b.max_tokens - b.min_tokens + 1;
    b.min_tokens + h % span
}

/// Paseo `tokenize`: words up to 5 UTF-16 units stay whole (with their leading
/// whitespace); longer ones are cut into 4-unit pieces.
pub fn tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ws_start = i;
        while i < chars.len() && jsstr::is_js_space(chars[i]) {
            i += 1;
        }
        let ws: String = chars[ws_start..i].iter().collect();
        if i == chars.len() {
            tokens.push(ws);
            break;
        }
        let w_start = i;
        while i < chars.len() && !jsstr::is_js_space(chars[i]) {
            i += 1;
        }
        let w: String = chars[w_start..i].iter().collect();
        let len = jsstr::len(&w);
        if len <= 5 {
            tokens.push(ws + &w);
            continue;
        }
        tokens.push(ws + &jsstr::slice(&w, 0, 4));
        let mut j = 4;
        while j < len {
            tokens.push(jsstr::slice(&w, j, j + 4));
            j += 4;
        }
    }
    tokens
}

fn intro_paragraph(cycle: u64) -> String {
    format!("## Cycle {cycle}\n\nI'll take a look at the scroll anchor behavior you described. Let me start by walking through how the conversation list currently handles streaming updates and where the auto-scroll logic actually lives. My instinct is that the anchor is supposed to pin to the bottom only when the user is already there, but I want to confirm that against the code rather than guess. This kind of behavior is usually a thin layout effect over a ref, and the bugs tend to come from event ordering rather than the math itself, so the first useful step is to read the relevant files.")
}

const REASONING: &str = "Need to find the scroll container, the layout effect that watches for new messages, and any gesture handler that might fight with programmatic scrolling. Probably a ref on the FlatList plus a near-bottom threshold.";

const MID_PARAGRAPH: &str = "Now I have a clearer picture. The auto-scroll uses a ref on the FlatList and tracks whether the user has scrolled away from the bottom by comparing the offset against the content size. There are a few subtle issues worth flagging before we change anything:\n\n- The threshold for 'near the bottom' is hardcoded at 80px, which feels too tight on dense content where headers and tool calls take up a lot of vertical space.\n- We rely on `onContentSizeChange` to detect new content, but that fires after layout, not as the streaming delta arrives, so we end up scrolling one frame late on fast streams.\n- The gesture handler does not pause scroll-to-bottom while the user is actively dragging, which means a drag in progress can be visually overridden mid-frame.\n- Coalescing happens upstream, so the FlatList sees fewer updates than the wire — but each batch can still cause a relayout.\n\nLet me make a small adjustment to the threshold and add a flag for active gestures, then run a quick command to confirm the order of events when streaming is fast.";

const CLOSING_PARAGRAPH: &str = "The change should keep scroll-to-bottom working when the user is at the bottom while not yanking the viewport when they are reading earlier messages. The bash output confirms the `userIsAtBottom` flag flips correctly during a simulated streaming burst, and the gesture flag suppresses scroll while a drag is active. If you want, I can follow up with a regression test that drives the FlatList with synthetic deltas at high frequency to lock in the behavior. For now, I'll stop here so you can take a look.";

fn edit_diff(file: &str) -> String {
    [
        format!("diff --git a/{file} b/{file}"),
        format!("--- a/{file}"),
        format!("+++ b/{file}"),
        "@@ -42,7 +42,9 @@".into(),
        "   const ref = useRef<FlatList>(null);".into(),
        "   const [userIsAtBottom, setUserIsAtBottom] = useState(true);".into(),
        "-  const NEAR_BOTTOM_PX = 80;".into(),
        "+  const NEAR_BOTTOM_PX = 160;".into(),
        "+  const isDraggingRef = useRef(false);".into(),
        String::new(),
        "-  if (userIsAtBottom) ref.current?.scrollToEnd({ animated: true });".into(),
        "+  if (userIsAtBottom && !isDraggingRef.current) {".into(),
        "+    ref.current?.scrollToEnd({ animated: true });".into(),
        "+  }".into(),
    ]
    .join("\n")
}

#[derive(Debug, Clone)]
enum CycleEvent {
    Assistant(String),
    Reasoning(String),
    Tool { call_id: String, name: &'static str, detail: Box<ToolCallDetail>, running: bool },
    Usage,
}

fn search_detail(file_paths: Option<Vec<String>>, num_files: Option<u64>, num_matches: Option<u64>) -> ToolCallDetail {
    ToolCallDetail::Search(SearchDetail {
        query: "scrollToEnd".into(),
        tool_name: Some(SearchToolName::Grep),
        content: None,
        file_paths,
        web_results: None,
        annotations: None,
        num_files: num_files.map(Into::into),
        num_matches: num_matches.map(Into::into),
        duration_ms: None,
        duration_seconds: None,
        truncated: None,
        mode: Some(SearchMode::FilesWithMatches),
    })
}

/// Paseo `buildCycleQueue`.
fn cycle_queue(turn_id: &str, cycle: u64) -> Vec<CycleEvent> {
    let mut q: Vec<CycleEvent> = tokenize(&intro_paragraph(cycle)).into_iter().map(CycleEvent::Assistant).collect();
    q.extend(tokenize(REASONING).into_iter().map(CycleEvent::Reasoning));
    let read_file = "packages/app/src/components/conversation-list.tsx";
    let read_id = format!("{turn_id}:read:{cycle}");
    q.push(CycleEvent::Tool {
        call_id: read_id.clone(),
        name: "read",
        detail: Box::new(ToolCallDetail::Read { file_path: read_file.into(), content: None, offset: None, limit: None }),
        running: true,
    });
    q.push(CycleEvent::Tool {
        call_id: read_id,
        name: "read",
        detail: Box::new(ToolCallDetail::Read {
            file_path: read_file.into(),
            content: Some("export function ConversationList() {\n  const ref = useRef<FlatList>(null);\n  // ...\n}".into()),
            offset: None,
            limit: None,
        }),
        running: false,
    });
    let grep_id = format!("{turn_id}:grep:{cycle}");
    q.push(CycleEvent::Tool { call_id: grep_id.clone(), name: "grep", detail: Box::new(search_detail(None, None, None)), running: true });
    q.push(CycleEvent::Tool {
        call_id: grep_id,
        name: "grep",
        detail: Box::new(search_detail(
            Some(vec![
                "packages/app/src/components/conversation-list.tsx".into(),
                "packages/app/src/hooks/use-scroll-anchor.ts".into(),
            ]),
            Some(2),
            Some(5),
        )),
        running: false,
    });
    q.extend(tokenize(MID_PARAGRAPH).into_iter().map(CycleEvent::Assistant));
    let edit_file = "packages/app/src/hooks/use-scroll-anchor.ts";
    let edit = ToolCallDetail::Edit {
        file_path: edit_file.into(),
        old_string: Some("const NEAR_BOTTOM_PX = 80;".into()),
        new_string: Some("const NEAR_BOTTOM_PX = 160;".into()),
        unified_diff: Some(edit_diff(edit_file)),
    };
    let edit_id = format!("{turn_id}:edit:{cycle}");
    q.push(CycleEvent::Tool { call_id: edit_id.clone(), name: "edit", detail: Box::new(edit.clone()), running: true });
    q.push(CycleEvent::Tool { call_id: edit_id, name: "edit", detail: Box::new(edit), running: false });
    let shell = ToolCallDetail::Shell {
        command: "node scripts/simulate-stream-burst.mjs".into(),
        cwd: Some("/tmp/paseo-mock-load".into()),
        output: Some("[burst] tick 1 userIsAtBottom=true\n[burst] tick 2 userIsAtBottom=true\n[burst] drag-start isDragging=true\n[burst] tick 3 suppressed\n[burst] drag-end isDragging=false\n".into()),
        exit_code: Some(Some(0.into())),
    };
    let shell_id = format!("{turn_id}:bash:{cycle}");
    q.push(CycleEvent::Tool { call_id: shell_id.clone(), name: "bash", detail: Box::new(shell.clone()), running: true });
    q.push(CycleEvent::Tool { call_id: shell_id, name: "bash", detail: Box::new(shell), running: false });
    q.extend(tokenize(CLOSING_PARAGRAPH).into_iter().map(CycleEvent::Assistant));
    q.push(CycleEvent::Usage);
    q
}

/// Paseo `buildBurstyStreamQueue`.
fn bursty_queue(cycle: u64) -> Vec<CycleEvent> {
    let text = [intro_paragraph(cycle), MID_PARAGRAPH.to_string(), CLOSING_PARAGRAPH.to_string()].join("\n\n");
    tokenize(&text).into_iter().map(CycleEvent::Assistant).collect()
}

fn tool_item(call_id: &str, name: &str, detail: ToolCallDetail, status: ToolCallStatus) -> TimelineItem {
    TimelineItem::ToolCall(Box::new(ToolCall {
        agent_message: None,
        call_id: call_id.to_string(),
        name: name.to_string(),
        detail,
        metadata: None,
        status,
        error: Value::Null,
    }))
}

// ---------------------------------------------------------------------------
// Prompt triggers (the regexes of mock-load-test-agent.js, matched by hand)

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `\s+` between words: collapse runs of whitespace for keyword matching.
fn words(s: &str) -> String {
    jsstr::collapse_spaces(&lower(s))
}

fn has_phrase(prompt: &str, phrase_variants: &[&str]) -> bool {
    let w = words(prompt);
    phrase_variants.iter().any(|p| w.contains(p))
}

fn turn_failure(prompt: &str) -> bool {
    has_phrase(prompt, &["emit synthetic turn failure", "emit a synthetic turn failure"])
}

fn plan_approval(prompt: &str) -> bool {
    has_phrase(prompt, &["emit synthetic plan approval", "emit a synthetic plan approval"])
}

fn questions(prompt: &str) -> Option<Value> {
    let w = words(prompt);
    let hit = ["emit synthetic question", "emit a synthetic question"].iter().any(|p| w.contains(p));
    if !hit {
        return None;
    }
    let free = ["free-write", "free write", "freewrite", "freeform", "text-only", "text only", "textonly"]
        .iter()
        .any(|p| w.contains(p));
    if free {
        return Some(json!([
            {"question":"What is the GitHub private repo URL to push to?","header":"repoUrl","options":[],"multiSelect":false,"placeholder":"git@github.com:user/repo.git"},
            {"question":"What should the first commit message be?","header":"commitMessage","options":[],"multiSelect":false,"placeholder":"Initial commit"}
        ]));
    }
    let mut preview = vec![
        "Ship it dark behind the new-surface flag, then turn it on for internal users before everyone else.".to_string(),
        String::new(),
        "if (flags.enabled(\"new-surface\")) {".to_string(),
        "  render(<NewSurface />);".to_string(),
        "}".to_string(),
    ];
    preview.extend((1..=40).map(|i| format!("// rollout step {i}")));
    preview.push("// LAST PREVIEW LINE".into());
    Some(json!([
        {"question":"Which surface should this apply to?","header":"surface","options":[{"label":"App"},{"label":"Desktop"}],"multiSelect":false},
        {"question":"Which rollout should we use?","header":"rollout","options":[{"label":"Immediately"},{"label":"Behind feature flag","preview":preview.join("\n")}],"multiSelect":false},
        {"question":"What success criteria should we use?","header":"success","options":[],"multiSelect":false,"placeholder":"Describe success..."}
    ]))
}

/// `delay synthetic user message by (\d+)ms`, capped at 2000.
fn user_message_delay_ms(prompt: &str) -> u64 {
    number_after(prompt, "delay synthetic user message by ", "ms").map(|n| n.min(2000)).unwrap_or(0)
}

/// `emit (\d+) assistant messages before synthetic user message`, capped at 500.
fn assistant_messages_before_user(prompt: &str) -> Option<u64> {
    let w = lower(prompt);
    let marker = " assistant messages before synthetic user message";
    let end = w.find(marker)?;
    let head = &w[..end];
    let start = head.rfind("emit ")? + "emit ".len();
    let n: u64 = head[start..].parse().ok()?;
    (n > 0).then_some(n.min(500))
}

fn number_after(prompt: &str, prefix: &str, suffix: &str) -> Option<u64> {
    let w = lower(prompt);
    let at = w.find(prefix)? + prefix.len();
    let digits: String = w[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !w[at + digits.len()..].starts_with(suffix) {
        return None;
    }
    digits.parse().ok()
}

fn withhold_user_message(prompt: &str) -> bool {
    lower(prompt).contains("withhold synthetic user message until interrupted")
}

fn user_message_before_acceptance(prompt: &str) -> bool {
    lower(prompt).contains("emit synthetic user message before accepting turn")
}

/// `^emit settled assistant image markdown:\s*(!\[...\](...))\s*$`.
fn settled_image_markdown(prompt: &str) -> Option<String> {
    let trimmed = jsstr::trim(prompt);
    let prefix = "emit settled assistant image markdown:";
    if trimmed.len() < prefix.len() || !trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return None;
    }
    let rest = jsstr::trim(&trimmed[prefix.len()..]);
    let ok = rest.starts_with("![") && rest.ends_with(')') && rest.contains("](");
    ok.then(|| rest.to_string())
}

/// Paseo `parseStructuredBranchNamePrompt`: the deterministic title/branch answer.
fn structured_branch_name(prompt: &str) -> Option<Value> {
    let has = prompt.contains("Generate a title and a git branch name for a coding agent")
        && (prompt.contains("Return JSON only with fields 'title' and 'branch'.")
            || prompt.contains("\"title\"")
            || prompt.contains("\"branch\""));
    let schema = prompt.contains("You must respond with JSON only that matches this JSON Schema")
        && prompt.contains("\"title\"")
        && prompt.contains("\"branch\"");
    if !has && !schema {
        return None;
    }
    let between = |open: &str, close: &str| {
        let s = prompt.find(open)? + open.len();
        let e = prompt[s..].find(close)? + s;
        Some(jsstr::trim(&prompt[s..e]).to_string())
    };
    let seed = between("<user-prompt>\n", "\n</user-prompt>")
        .or_else(|| between("<attachments>\n", "\n</attachments>"))
        .unwrap_or_default();
    let first = seed
        .split('\n')
        .find(|l| !jsstr::trim(l).is_empty())
        .map(|l| jsstr::trim(l).to_string())
        .unwrap_or_else(|| "Mock task".into());
    let unquoted = first.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let title = jsstr::slice(&jsstr::collapse_spaces(unquoted), 0, 80);
    let title = jsstr::trim(&title).to_string();
    let mut branch = String::new();
    let mut dash = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            branch.push(c);
            dash = false;
        } else if !dash {
            branch.push('-');
            dash = true;
        }
    }
    let branch = branch.trim_matches('-');
    let branch: String = branch.chars().take(100).collect();
    let branch = if branch.is_empty() { "mock-task".to_string() } else { branch };
    Some(json!({"title": if title.is_empty() { "Mock task".to_string() } else { title }, "branch": branch}))
}

// ---------------------------------------------------------------------------
// Providers

pub struct MockProvider;

fn modes() -> Vec<AgentMode> {
    vec![
        AgentMode {
            id: "load-test".into(),
            label: "Load Test".into(),
            description: Some("Streams repeated markdown, reasoning, and tool calls for app stress testing".into()),
            icon: Some("ShieldOff".into()),
            color_tier: Some("dangerous".into()),
        },
        AgentMode {
            id: "approval-test".into(),
            label: "Approval Test".into(),
            description: Some("Alternate development-only permission mode for preference tests".into()),
            icon: Some("ShieldCheck".into()),
            color_tier: Some("safe".into()),
        },
    ]
}

fn capabilities() -> Capabilities {
    Capabilities {
        supports_streaming: true,
        supports_session_persistence: true,
        supports_session_listing: Some(true),
        supports_dynamic_modes: false,
        supports_mcp_servers: false,
        supports_reasoning_stream: true,
        supports_tool_invocations: true,
        supports_rewind_conversation: true,
        supports_rewind_files: true,
        supports_rewind_both: true,
        extra: Default::default(),
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: MOCK_ID.into(),
            label: "Mock Load Test".into(),
            default_mode_id: Some(DEFAULT_MODE.into()),
            modes: modes(),
            models: MODELS
                .iter()
                .map(|m| ModelInfo {
                    id: m.id.into(),
                    label: m.label.into(),
                    description: None,
                    is_default: m.default,
                    is_selectable: m.selectable,
                    default_thinking_option_id: m.thinking_default.map(Into::into),
                })
                .collect(),
            capabilities: capabilities(),
            echoes_user_message: true,
        }
    }

    async fn create_session(&self, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        Ok(Arc::new(MockSession::new(uuid::Uuid::new_v4().to_string(), config, events)))
    }

    async fn resume_session(
        &self,
        handle: &PersistenceHandle,
        mut config: SessionConfig,
        events: EventSink,
    ) -> Result<Arc<dyn Session>, ProviderError> {
        if let Some(meta) = &handle.metadata {
            if config.model.is_none() {
                config.model = meta.get("model").and_then(Value::as_str).map(str::to_string);
            }
            if config.mode_id.is_none() {
                config.mode_id = meta.get("modeId").and_then(Value::as_str).map(str::to_string);
            }
        }
        Ok(Arc::new(MockSession::new(handle.session_id.clone(), config, events)))
    }
}

/// `mock-slow`: its catalog never loads and it can't start sessions.
pub struct MockSlowProvider;

#[async_trait]
impl Provider for MockSlowProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: MOCK_SLOW_ID.into(),
            label: "Mock Slow Provider".into(),
            default_mode_id: Some("default".into()),
            modes: vec![AgentMode {
                id: "default".into(),
                label: "Default".into(),
                description: Some("Dev-only mode for the mock slow provider".into()),
                icon: Some("ShieldOff".into()),
                color_tier: Some("dangerous".into()),
            }],
            models: Vec::new(),
            capabilities: Capabilities::default(),
            echoes_user_message: false,
        }
    }

    async fn create_session(&self, _config: SessionConfig, _events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        Err(ProviderError::new("Mock slow provider is dev-only; sessions are not supported."))
    }

    async fn resume_session(
        &self,
        _handle: &PersistenceHandle,
        _config: SessionConfig,
        _events: EventSink,
    ) -> Result<Arc<dyn Session>, ProviderError> {
        Err(ProviderError::new("Mock slow provider is dev-only; sessions are not supported."))
    }
}

// ---------------------------------------------------------------------------
// Session

struct Turn {
    id: String,
    task: Option<JoinHandle<()>>,
    started: bool,
    cycle: u64,
    emitted_tokens: u64,
}

struct Inner {
    model: String,
    mode: String,
    turn: Option<Turn>,
    pending: Vec<PermissionRequest>,
    responses: Vec<String>,
    response: Option<String>,
    streaming_response: Option<String>,
    streaming_interval_ms: u64,
}

pub struct MockSession {
    id: String,
    events: EventSink,
    inner: Arc<Mutex<Inner>>,
}

fn feature_str(fv: &Map<String, Value>, key: &str) -> Option<String> {
    fv.get(key).and_then(Value::as_str).map(str::to_string)
}

impl MockSession {
    fn new(id: String, config: SessionConfig, events: EventSink) -> Self {
        let fv = &config.feature_values;
        let responses = fv
            .get("mockAssistantResponses")
            .and_then(Value::as_array)
            .filter(|a| a.iter().all(Value::is_string))
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let streaming_interval_ms = fv
            .get("mockStreamingAssistantIntervalMs")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 1.0)
            .map(|n| n.min(1000.0) as u64)
            .unwrap_or(DEFAULT_INTERVAL_MS);
        MockSession {
            id,
            events,
            inner: Arc::new(Mutex::new(Inner {
                model: config.model.unwrap_or_else(|| DEFAULT_MODEL.into()),
                mode: config.mode_id.unwrap_or_else(|| DEFAULT_MODE.into()),
                turn: None,
                pending: Vec::new(),
                responses,
                response: feature_str(fv, "mockAssistantResponse"),
                streaming_response: feature_str(fv, "mockStreamingAssistantResponse"),
                streaming_interval_ms,
            })),
        }
    }
}

/// What one turn does once it is accepted (Paseo `scheduleTurn`'s order).
enum Script {
    Failure,
    Streaming(String, u64),
    Settled(String),
    PlanApproval,
    Questions(Value),
    Cycle(Profile),
}

/// The handle a turn task uses: emits only while its turn is still the active one.
#[derive(Clone)]
struct TurnCtx {
    inner: Arc<Mutex<Inner>>,
    events: EventSink,
    turn_id: String,
    message_id: String,
}

impl TurnCtx {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn active(&self) -> bool {
        self.lock().turn.as_ref().is_some_and(|t| t.id == self.turn_id)
    }

    fn emit(&self, ev: StreamEvent) {
        let _ = self.events.send(ev);
    }

    fn timeline(&self, item: TimelineItem) {
        self.emit(StreamEvent::Timeline { provider: MOCK_ID.into(), item, turn_id: Some(self.turn_id.clone()) });
    }

    fn started(&self) {
        let first = {
            let mut g = self.lock();
            match g.turn.as_mut() {
                Some(t) if t.id == self.turn_id && !t.started => {
                    t.started = true;
                    true
                }
                _ => false,
            }
        };
        if first {
            self.emit(StreamEvent::TurnStarted { provider: MOCK_ID.into(), turn_id: Some(self.turn_id.clone()) });
        }
    }

    fn user_message(&self, prompt: &str, client_message_id: Option<String>) {
        if !self.active() {
            return;
        }
        self.started();
        self.timeline(TimelineItem::UserMessage {
            text: prompt.to_string(),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            client_message_id,
        });
    }

    fn usage(&self) -> Usage {
        let g = self.lock();
        let (cycle, tokens) = g.turn.as_ref().map(|t| (t.cycle, t.emitted_tokens)).unwrap_or((0, 0));
        Usage {
            input_tokens: Some((cycle * 32).into()),
            output_tokens: Some(tokens.into()),
            context_window_used_tokens: Some((tokens * 2).into()),
            context_window_max_tokens: Some(128_000.into()),
            ..Usage::default()
        }
    }

    /// Clear the turn (if it's still ours) and run `then` with it cleared.
    fn end(&self, then: impl FnOnce(&TurnCtx)) -> bool {
        let ours = {
            let mut g = self.lock();
            if g.turn.as_ref().is_some_and(|t| t.id == self.turn_id) {
                g.turn = None;
                true
            } else {
                false
            }
        };
        if ours {
            then(self);
        }
        ours
    }

    /// Paseo `finishTurnWithText`.
    fn finish_with_usage(&self) {
        let usage = self.usage();
        self.end(|c| {
            c.emit(StreamEvent::TurnCompleted { provider: MOCK_ID.into(), turn_id: Some(c.turn_id.clone()), usage: Some(usage) })
        });
    }

    fn count_token(&self) {
        if let Some(t) = self.lock().turn.as_mut() {
            t.emitted_tokens += 1;
        }
    }

    fn assistant(&self, text: String) {
        self.timeline(TimelineItem::AssistantMessage { text, message_id: Some(self.message_id.clone()) });
    }
}

async fn run_script(ctx: TurnCtx, script: Script) {
    match script {
        Script::Failure => {
            if !ctx.active() {
                return;
            }
            ctx.started();
            ctx.end(|c| {
                c.emit(StreamEvent::TurnFailed {
                    provider: MOCK_ID.into(),
                    turn_id: Some(c.turn_id.clone()),
                    error: "Requested mock provider failure".into(),
                    code: None,
                    diagnostic: None,
                })
            });
        }
        Script::Settled(text) => {
            if !ctx.active() {
                return;
            }
            ctx.started();
            ctx.assistant(text);
            ctx.end(|c| c.emit(StreamEvent::TurnCompleted { provider: MOCK_ID.into(), turn_id: Some(c.turn_id.clone()), usage: None }));
        }
        Script::Streaming(text, interval) => {
            for token in tokenize(&text) {
                if !ctx.active() {
                    return;
                }
                ctx.started();
                ctx.count_token();
                ctx.assistant(token);
                tokio::time::sleep(Duration::from_millis(interval)).await;
            }
            if ctx.active() {
                ctx.finish_with_usage();
            }
        }
        Script::PlanApproval => {
            if !ctx.active() {
                return;
            }
            ctx.started();
            let request = PermissionRequest {
                id: format!("mock-plan-{}", ctx.turn_id),
                provider: MOCK_ID.into(),
                name: "MockPlanApproval".into(),
                kind: PermissionKind::Plan,
                title: Some("Plan".into()),
                description: Some("Review the proposed plan before implementation starts.".into()),
                input: Some(json_map(json!({"plan": "1. Add the (c) README note.\n2. Run --name=\"my repo\".\n3. Verify ---buzz in the diff."}))),
                detail: None,
                suggestions: None,
                actions: Some(vec![
                    PermissionAction { id: "implement".into(), label: "Implement".into(), behavior: Behavior::Allow, variant: Some(ActionVariant::Primary), intent: Some(ActionIntent::Implement) },
                    PermissionAction { id: "dismiss".into(), label: "Dismiss".into(), behavior: Behavior::Deny, variant: Some(ActionVariant::Secondary), intent: Some(ActionIntent::Dismiss) },
                ]),
                metadata: Some(json_map(json!({"source": "mock_plan_approval"}))),
            };
            ctx.lock().pending.push(request.clone());
            ctx.emit(StreamEvent::PermissionRequested { provider: MOCK_ID.into(), request: Box::new(request) });
        }
        Script::Questions(questions) => {
            if !ctx.active() {
                return;
            }
            ctx.started();
            let request = PermissionRequest {
                id: format!("mock-questions-{}", ctx.turn_id),
                provider: MOCK_ID.into(),
                name: "MockQuestions".into(),
                kind: PermissionKind::Question,
                title: Some("Questions".into()),
                description: None,
                input: Some(json_map(json!({"questions": questions}))),
                detail: None,
                suggestions: None,
                actions: None,
                metadata: Some(json_map(json!({"source": "mock_questions"}))),
            };
            ctx.lock().pending.push(request.clone());
            ctx.emit(StreamEvent::PermissionRequested { provider: MOCK_ID.into(), request: Box::new(request) });
        }
        Script::Cycle(profile) => run_cycles(ctx, profile).await,
    }
}

fn json_map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Paseo `tick` / `dispatchCycleEvent` / `finishTurn`.
async fn run_cycles(ctx: TurnCtx, profile: Profile) {
    let started_at = Instant::now();
    let mut queue: std::collections::VecDeque<CycleEvent> = Default::default();
    let mut burst_index = 0u64;
    loop {
        if !ctx.active() {
            return;
        }
        ctx.started();
        if started_at.elapsed() >= Duration::from_millis(profile.duration_ms) {
            ctx.assistant("\n\n_(end of synthetic stream)_\n".into());
            ctx.finish_with_usage();
            return;
        }
        let this_tick = profile.burst.map(|b| next_burst_size(&b, burst_index)).unwrap_or(1);
        burst_index += 1;
        for _ in 0..this_tick {
            if queue.is_empty() {
                let cycle = {
                    let mut g = ctx.lock();
                    match g.turn.as_mut() {
                        Some(t) => {
                            t.cycle += 1;
                            t.cycle
                        }
                        None => return,
                    }
                };
                queue = if profile.burst.is_some() { bursty_queue(cycle) } else { cycle_queue(&ctx.turn_id, cycle) }.into();
            }
            let Some(ev) = queue.pop_front() else { break };
            match ev {
                CycleEvent::Assistant(text) => {
                    ctx.count_token();
                    ctx.assistant(text);
                }
                CycleEvent::Reasoning(text) => {
                    ctx.count_token();
                    ctx.timeline(TimelineItem::Reasoning { text });
                }
                CycleEvent::Tool { call_id, name, detail, running } => {
                    let status = if running { ToolCallStatus::Running } else { ToolCallStatus::Completed };
                    ctx.timeline(tool_item(&call_id, name, *detail, status));
                }
                CycleEvent::Usage => {
                    let usage = ctx.usage();
                    ctx.emit(StreamEvent::UsageUpdated { provider: MOCK_ID.into(), turn_id: Some(ctx.turn_id.clone()), usage });
                }
            }
        }
        let wait = profile.burst.map(|b| b.gap_ms).unwrap_or(profile.interval_ms);
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}

#[async_trait]
impl Session for MockSession {
    fn session_id(&self) -> String {
        self.id.clone()
    }

    async fn start_turn(&self, prompt: &str, client_message_id: Option<String>) -> Result<String, ProviderError> {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let script = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if g.turn.is_some() {
                return Err(ProviderError::new("Mock load-test provider already has an active turn"));
            }
            g.turn = Some(Turn { id: turn_id.clone(), task: None, started: false, cycle: 0, emitted_tokens: 0 });
            if turn_failure(prompt) {
                Script::Failure
            } else if let Some(text) = g.streaming_response.clone() {
                Script::Streaming(text, g.streaming_interval_ms)
            } else if !g.responses.is_empty() || g.response.is_some() {
                let text = if g.responses.is_empty() { g.response.clone().unwrap_or_default() } else { g.responses.remove(0) };
                Script::Settled(text)
            } else if let Some(answer) = structured_branch_name(prompt) {
                Script::Settled(answer.to_string())
            } else if let Some(md) = settled_image_markdown(prompt) {
                Script::Settled(md)
            } else if plan_approval(prompt) {
                Script::PlanApproval
            } else if let Some(q) = questions(prompt) {
                Script::Questions(q)
            } else {
                Script::Cycle(profile(&g.model))
            }
        };
        let ctx = TurnCtx {
            inner: self.inner.clone(),
            events: self.events.clone(),
            turn_id: turn_id.clone(),
            message_id: uuid::Uuid::new_v4().to_string(),
        };
        let prompt = prompt.to_string();
        let task = tokio::spawn(async move {
            if user_message_before_acceptance(&prompt) {
                ctx.user_message(&prompt, client_message_id);
                run_script(ctx, script).await;
                return;
            }
            if withhold_user_message(&prompt) {
                return;
            }
            if let Some(n) = assistant_messages_before_user(&prompt) {
                tokio::task::yield_now().await;
                if !ctx.active() {
                    return;
                }
                ctx.started();
                for i in 0..n {
                    ctx.timeline(TimelineItem::AssistantMessage {
                        text: format!("Synthetic pre-echo message {}", i + 1),
                        message_id: Some(format!("{}-{}", ctx.message_id, i + 1)),
                    });
                    tokio::task::yield_now().await;
                    if !ctx.active() {
                        return;
                    }
                }
                ctx.user_message(&prompt, client_message_id);
                tokio::task::yield_now().await;
                if ctx.active() {
                    ctx.finish_with_usage();
                }
                return;
            }
            let delay = user_message_delay_ms(&prompt);
            tokio::task::yield_now().await;
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            ctx.user_message(&prompt, client_message_id);
            run_script(ctx, script).await;
        });
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match g.turn.as_mut() {
            Some(t) if t.id == turn_id => t.task = Some(task),
            _ => {}
        }
        Ok(turn_id)
    }

    async fn steer(&self, _prompt: &str, expected_turn_id: &str) -> Result<bool, ProviderError> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Ok(g.turn.as_ref().is_some_and(|t| t.id == expected_turn_id))
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        let turn = self.inner.lock().unwrap_or_else(|p| p.into_inner()).turn.take();
        if let Some(turn) = turn {
            if let Some(task) = turn.task {
                task.abort();
            }
            let _ = self.events.send(StreamEvent::TurnCanceled {
                provider: MOCK_ID.into(),
                turn_id: Some(turn.id),
                reason: "Interrupted".into(),
            });
        }
        Ok(())
    }

    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError> {
        let (request, turn_id) = {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let Some(i) = g.pending.iter().position(|p| p.id == request_id) else {
                return Ok(());
            };
            let request = g.pending.remove(i);
            (request, g.turn.as_ref().map(|t| t.id.clone()))
        };
        let _ = self.events.send(StreamEvent::PermissionResolved {
            provider: MOCK_ID.into(),
            request_id: request_id.to_string(),
            resolution: response,
        });
        if let Some(turn_id) = turn_id {
            let ctx = TurnCtx { inner: self.inner.clone(), events: self.events.clone(), turn_id, message_id: String::new() };
            let _ = request.kind;
            ctx.finish_with_usage();
        }
        Ok(())
    }

    fn out_of_band(&self, prompt: &str) -> Option<Vec<StreamEvent>> {
        (prompt == HANDLED_COMMAND).then(|| {
            vec![StreamEvent::Timeline {
                provider: MOCK_ID.into(),
                item: TimelineItem::AssistantMessage { text: "Mock command handled".into(), message_id: None },
                turn_id: None,
            }]
        })
    }

    async fn close(&self) {
        let _ = self.interrupt().await;
    }

    fn runtime_info(&self) -> RuntimeInfo {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        RuntimeInfo {
            provider: MOCK_ID.into(),
            session_id: Some(self.id.clone()),
            model: Some(g.model.clone()),
            thinking_option_id: None,
            mode_id: Some(g.mode.clone()),
            extra: None,
        }
    }

    fn persistence(&self) -> PersistenceHandle {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        PersistenceHandle {
            provider: MOCK_ID.into(),
            session_id: self.id.clone(),
            native_handle: None,
            metadata: Some(json_map(json!({"model": g.model, "modeId": g.mode}))),
        }
    }

    fn current_mode(&self) -> Option<String> {
        Some(self.inner.lock().unwrap_or_else(|p| p.into_inner()).mode.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_like_paseo() {
        assert_eq!(tokenize("Hello wonderful world  "), vec!["Hello", " wond", "erfu", "l", " world", "  "]);
        assert_eq!(tokenize("## Cycle 1"), vec!["##", " Cycle", " 1"]);
        assert_eq!(tokenize("").len(), 0);
        let joined: String = tokenize(MID_PARAGRAPH).concat();
        assert_eq!(joined, MID_PARAGRAPH);
    }

    #[test]
    fn burst_sizes_in_range_and_deterministic() {
        let b = Burst { min_tokens: 1, max_tokens: 40, gap_ms: 90 };
        let sizes: Vec<u32> = (0..50).map(|i| next_burst_size(&b, i)).collect();
        assert!(sizes.iter().all(|s| (1..=40).contains(s)));
        assert_eq!(sizes, (0..50).map(|i| next_burst_size(&b, i)).collect::<Vec<_>>());
    }

    #[test]
    fn triggers() {
        assert!(turn_failure("please EMIT a synthetic  turn failure now"));
        assert!(plan_approval("emit synthetic plan approval"));
        assert!(questions("emit synthetic questions").is_some());
        assert_eq!(questions("emit synthetic question free-write").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(user_message_delay_ms("delay synthetic user message by 9000ms"), 2000);
        assert_eq!(assistant_messages_before_user("emit 3 assistant messages before synthetic user message"), Some(3));
        assert!(settled_image_markdown("emit settled assistant image markdown: ![a](b.png)").is_some());
        let answer = structured_branch_name(
            "Generate a title and a git branch name for a coding agent\nReturn JSON only with fields 'title' and 'branch'.\n<user-prompt>\n\"Fix the Login bug!\"\n</user-prompt>",
        )
        .unwrap();
        assert_eq!(answer, json!({"title": "Fix the Login bug!", "branch": "fix-the-login-bug"}));
    }
}
