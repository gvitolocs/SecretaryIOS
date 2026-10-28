//! Lucina's computer use: `src/computer-use.js` of the retired Node Lucina,
//! ported to Rust.
//!
//! The CUA stack itself is unchanged and stays outside this crate: the Cua
//! Spaces daemon (`lucina-cua.service`, socket
//! `Lucina/data/cua-runtime/cua.sock`) and the two containers it drives,
//! `lucina-workspace` (Linux desktop, `local:lucina-workspace`) and
//! `lucina-android` (`direct:127.0.0.1:32792`). This crate is the transport:
//!
//! - [`parse_computer_result`]: Hermes' stream-json → receipts. A model saying
//!   "done" proves nothing; only observed tool results do.
//! - [`Client`]: spawns `LUCINA_COMPUTER_USE_LAUNCHER`
//!   (`run-hermes-computer.sh`) with a minimal environment, a detached
//!   process group, a 200 s deadline and cancellation through
//!   `--stop-gateway`.
//! - [`Sandbox`]: the stdio MCP proxy (`cua-mcp.sh` / `android-cua-mcp.sh`)
//!   for the direct sandbox tools, so an outside agent can run a command or
//!   click in the shared desktop without going through the model.
//!
//! Everything runs inside the containers: this crate never executes a host
//! command, and the launcher refuses to start unless the Hermes profile is
//! pinned to `local:lucina-workspace`.

mod client;
pub mod control;
mod sandbox;
pub mod viewer;

pub use client::{
    is_computer_use_ready, Cancel, Client, ComputerUseConfig, DEFAULT_LAUNCHER, MAX_PROMPT_CHARS,
    TASK_TIMEOUT,
};
pub use control::{control, run_task, shared_client, Control, SandboxId, Takeover, OWNER_LEASE};
pub use sandbox::{
    guest_path, image_of, text_of, Sandbox, ANDROID_SANDBOX_SHELL, ANDROID_SPACE,
    WORKSPACE_SANDBOX_SHELL, WORKSPACE_SPACE,
};
pub use viewer::{ViewMode, ViewTicket, ViewerConfig};

use serde_json::{json, Value};

/// This crate's version, announced to the Cua MCP proxy.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Space tools the Cua daemon's MCP exposes (`cua-mcp.sh` permissions).
pub const SPACE_TOOLS: [&str; 8] = [
    "list_spaces",
    "space_bash",
    "space_write",
    "list_tools",
    "call_tool",
    "stream_endpoint",
    "list_space_windows",
    "stream_space_window",
];

/// Hermes' own toolsets: not MCP, so they are receipts as they are.
pub const BACKEND_TOOLS: [&str; 7] = [
    "terminal",
    "process",
    "process_manage",
    "read_file",
    "write_file",
    "patch",
    "search_files",
];

/// `mcp__phone__*` names Hermes may report for the owner's phone bridge.
pub const PHONE_TOOLS: [&str; 7] = [
    "phone_info",
    "phone_screenshot",
    "phone_ui",
    "phone_shell",
    "phone_input",
    "phone_otp",
    "phone_call",
];

/// Discovery tools: a successful call proves nothing was done in the
/// sandbox, so they never count as an observed action.
pub const LIST_ONLY_TOOLS: [&str; 3] = ["list_spaces", "list_tools", "stream_endpoint"];

/// Why a computer-use request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The event stream was not one JSON object per line.
    #[error("Hermes returned an invalid computer-use event stream.")]
    InvalidStream,
    /// The computer-use model ran out of quota: no completion was confirmed.
    #[error("The computer-use model quota is exhausted; no task completion was confirmed.")]
    QuotaExhausted,
    /// No `result` event with `exit_code: 0` and a text.
    #[error("Hermes computer-use request failed; no completion was confirmed.")]
    Failed,
    /// `LUCINA_COMPUTER_USE_ENABLED` is off or the launcher is missing.
    #[error("Lucina's sandbox tools are not enabled.")]
    NotEnabled,
    /// A previous cancellation never reached the gateway.
    #[error("Lucina's computer-use gateway needs recovery after an unconfirmed cancellation.")]
    RecoveryRequired,
    /// One task at a time: the desktop is single.
    #[error("Lucina's desktop is busy with another task.")]
    Busy,
    /// Empty, or over [`MAX_PROMPT_CHARS`].
    #[error("Computer-use request is empty or too long.")]
    BadRequest,
    /// The workspace permits Chromium only.
    #[error("Lucina's computer uses Chromium only: work in its open Chromium window with get_window_state, click and type_text.")]
    BrowserPolicy,
    /// The workspace has one Chromium; no isolated or second browser.
    #[error("Lucina's computer has one Chromium, already open and signed in: work in that window with get_window_state, click and type_text; never launch an isolated profile or a second browser.")]
    SecondBrowser,
    /// Cancelled by the caller (the gateway was stopped).
    #[error("Computer-use request cancelled.")]
    Cancelled,
    /// The task did not finish inside the deadline.
    #[error("Computer-use deadline exceeded.")]
    Deadline,
    /// More than 2 MiB of stream-json.
    #[error("Computer-use output limit exceeded.")]
    OutputLimit,
    /// The launcher could not be started.
    #[error("Native Hermes computer-use runtime unavailable.")]
    LauncherUnavailable,
    /// The process exited non-zero with a stream that parsed.
    #[error("Native Hermes could not complete the computer-use request.")]
    NotCompleted,
    /// The cancellation could not be confirmed: the desktop stays locked.
    #[error("Computer task stopped locally, but agent cancellation is unconfirmed; desktop execution is locked until recovery.")]
    UnconfirmedCancel,
    /// `--stop-gateway` did not exit 0 within 15 s.
    #[error("Gateway stop failed.")]
    GatewayStopFailed,
    /// The Cua stdio MCP proxy failed (daemon down, MCP error, timeout).
    #[error("Lucina's sandbox daemon is unavailable: {0}")]
    Sandbox(String),
    /// The owner took over this sandbox from the apps' live view.
    #[error(
        "The owner is driving this computer from the live view; try again once it is given back."
    )]
    OwnerDriving,
    /// An owner-only action without the owner's takeover lease.
    #[error("Take over the computer first.")]
    NotDriving,
    /// The owner drove without real input for [`control::OWNER_IDLE`]: the desktop went back to Lucina.
    #[error("You were idle, so Lucina has the computer again.")]
    OwnerIdle,
    /// A live-view ticket could not be minted.
    #[error("Live view unavailable: {0}")]
    Viewer(String),
}

impl Error {
    /// A short stable code for logs and API errors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidStream => "invalid_stream",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Failed => "no_completion",
            Self::NotEnabled => "not_enabled",
            Self::RecoveryRequired => "recovery_required",
            Self::Busy => "busy",
            Self::BadRequest | Self::BrowserPolicy | Self::SecondBrowser => "bad_request",
            Self::Cancelled => "cancelled",
            Self::Deadline => "deadline_exceeded",
            Self::OutputLimit => "output_limit",
            Self::LauncherUnavailable => "launcher_unavailable",
            Self::NotCompleted => "not_completed",
            Self::UnconfirmedCancel => "unconfirmed_cancel",
            Self::GatewayStopFailed => "gateway_stop_failed",
            Self::Sandbox(_) => "sandbox_unavailable",
            Self::OwnerDriving => "owner_driving",
            Self::NotDriving => "not_driving",
            Self::OwnerIdle => "owner_idle",
            Self::Viewer(_) => "viewer_unavailable",
        }
    }
}

/// How much of a task was actually observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Every receipt succeeded and at least one did something.
    Completed,
    /// Some receipts failed.
    Partial,
    /// Only discovery ran, or nothing succeeded.
    Unverified,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Unverified => "unverified",
        }
    }
}

/// A parsed computer-use turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerResult {
    pub status: Status,
    /// What Lucina says to the owner (never the raw log).
    pub reply: String,
    /// Hermes' session id, when it reported one.
    pub session_id: Option<String>,
    /// The receipts that did something, by Hermes' original tool name.
    pub executed_tools: Vec<String>,
    /// How many receipts failed.
    pub tool_errors: usize,
}

impl ComputerResult {
    /// The `data` body of `POST /v1/computer/task` and the MCP tool result.
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "status": self.status.as_str(),
            "reply": self.reply,
            "executedTools": self.executed_tools,
            "toolErrors": self.tool_errors,
        });
        if let Some(id) = &self.session_id {
            v["sessionId"] = json!(id);
        }
        v
    }
}

/// JavaScript's `a || b`: the first truthy value.
fn or(a: Option<&Value>, b: Option<&Value>) -> Value {
    match a {
        Some(v) if js_truthy(v) => v.clone(),
        _ => match b {
            Some(v) if js_truthy(v) => v.clone(),
            _ => Value::Null,
        },
    }
}

/// JavaScript `String(a || b || '')`.
fn str_or_empty(a: Option<&Value>, b: Option<&Value>) -> String {
    let v = or(a, b);
    if js_truthy(&v) {
        js_string(&v)
    } else {
        String::new()
    }
}

/// JavaScript truthiness of a JSON value.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JavaScript `String(value)`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `cuaTool(name)`: the canonical name of a receipt, or `None` when the
/// event is not a Cua call at all.
///
/// Android receipts come back under a second MCP server name and are folded
/// onto the Cua one; Hermes' own toolsets and the phone bridge are already
/// canonical.
pub fn cua_tool(name: &str) -> Option<String> {
    let name = match name.strip_prefix("mcp__android__") {
        Some(rest) => format!("mcp__cua__{rest}"),
        None => name.to_owned(),
    };
    if BACKEND_TOOLS.contains(&name.as_str()) {
        return Some(name);
    }
    if let Some(rest) = name.strip_prefix("mcp__phone__") {
        if PHONE_TOOLS.contains(&rest) {
            return Some(name);
        }
    }
    SPACE_TOOLS
        .iter()
        .find(|t| name == format!("mcp_cua_{t}") || name == format!("mcp__cua__{t}"))
        .map(|t| (*t).to_owned())
}

/// `/[exit (?!0])\d+\]/`: Hermes reports a tool's exit code in its output.
pub fn claims_failed_exit(s: &str) -> bool {
    let mut from = 0;
    while let Some(i) = s[from..].find("[exit ") {
        let rest = &s[from + i + "[exit ".len()..];
        if !rest.starts_with("0]") {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && rest.as_bytes().get(digits) == Some(&b']') {
                return true;
            }
        }
        from += i + 1;
        if from >= s.len() {
            break;
        }
    }
    false
}

/// `/quota|rate.limit|RESOURCE_EXHAUSTED/i`, without a regex dependency.
pub fn quota_exhausted(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("quota")
        || lower.contains("resource_exhausted")
        || lower.contains("resource exhausted")
        || rate_limit(&lower)
}

fn rate_limit(lower: &str) -> bool {
    let chars: Vec<char> = lower.chars().collect();
    let rate: Vec<char> = "rate".chars().collect();
    let limit: Vec<char> = "limit".chars().collect();
    let n = chars.len();
    (0..n).any(|i| {
        i + 4 < n
            && chars[i..i + 4] == rate[..]
            && i + 5 + limit.len() <= n
            && chars[i + 5..i + 5 + limit.len()] == limit[..]
    })
}

/// Did this receipt actually do something?
fn succeeded(event: &Value) -> bool {
    if event.get("is_error").is_some_and(js_truthy) {
        return false;
    }
    let output = str_or_empty(event.get("output"), None);
    if claims_failed_exit(&output) {
        return false;
    }
    // Hermes wraps MCP evidence as untrusted data. Inspect the payload
    // without treating its text as instructions, including nested driver
    // errors.
    let mut payload = strip_untrusted_open(&output);
    payload = strip_untrusted_close(&payload);
    for _ in 0..3 {
        let Ok(value) = serde_json::from_str::<Value>(&payload) else {
            break;
        };
        let bad = value.get("error").is_some_and(js_truthy)
            || value.get("isError").is_some_and(js_truthy)
            || value.get("not_found").is_some_and(js_truthy)
            || matches!(value.get("verified"), Some(Value::Bool(false)))
            || matches!(value.get("exit_code"), Some(Value::Number(n)) if n.as_f64() != Some(0.0));
        if bad {
            return false;
        }
        match value.get("result") {
            Some(Value::String(s)) => payload = s.clone(),
            _ => break,
        }
    }
    true
}

/// `output.replace(/^<untrusted_tool_result[^>]*>[\s\S]*?\n\n/, '')`.
fn strip_untrusted_open(s: &str) -> String {
    let Some(open) = s.strip_prefix('<') else {
        return s.to_owned();
    };
    if !open.starts_with("untrusted_tool_result") {
        return s.to_owned();
    }
    // `[^>]*>` then `[\s\S]*?` then `\n\n`, matched lazily: the first "\n\n"
    // after the tag ends the header.
    let Some(gt) = s.find('>') else {
        return s.to_owned();
    };
    match s[gt + 1..].find("\n\n") {
        Some(nl) => s[gt + 1 + nl + 2..].to_owned(),
        None => s.to_owned(),
    }
}

/// `output.replace(/\n<\/untrusted_tool_result>\s*$/, '')`.
fn strip_untrusted_close(s: &str) -> String {
    const CLOSE: &str = "\n</untrusted_tool_result>";
    let Some(i) = s.rfind(CLOSE) else {
        return s.to_owned();
    };
    if s[i + CLOSE.len()..].trim().is_empty() {
        s[..i].to_owned()
    } else {
        s.to_owned()
    }
}

/// `parseComputerResult(output)`: Hermes' stream-json → [`ComputerResult`].
pub fn parse_computer_result(output: &str) -> Result<ComputerResult, Error> {
    let mut events: Vec<Value> = Vec::new();
    for line in output.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => events.push(v),
            Err(_) => return Err(Error::InvalidStream),
        }
    }
    let result = events
        .iter()
        .rev()
        .find(|e| e.get("type") == Some(&Value::String("result".into())));
    if let Some(result) = result {
        let text = str_or_empty(result.get("error"), result.get("text"));
        let zero =
            matches!(result.get("exit_code"), Some(Value::Number(n)) if n.as_f64() == Some(0.0));
        if !zero && quota_exhausted(&text) {
            return Err(Error::QuotaExhausted);
        }
    }
    let Some(result) = result else {
        return Err(Error::Failed);
    };
    let text = result.get("text").cloned().unwrap_or(Value::Null);
    let zero = matches!(result.get("exit_code"), Some(Value::Number(n)) if n.as_f64() == Some(0.0));
    if !zero || !js_truthy(&text) {
        return Err(Error::Failed);
    }
    let receipts: Vec<&Value> = events
        .iter()
        .filter(|e| e.get("type") == Some(&Value::String("tool_result".into())))
        .filter(|e| receipt_name(e).and_then(cua_tool).is_some())
        .collect();
    let successful: Vec<&Value> = receipts.iter().copied().filter(|e| succeeded(e)).collect();
    let mut executed_tools: Vec<String> = Vec::new();
    for e in &successful {
        let Some(name) = receipt_name(e) else {
            continue;
        };
        let Some(canonical) = cua_tool(name) else {
            continue;
        };
        if !LIST_ONLY_TOOLS.contains(&canonical.as_str()) {
            executed_tools.push(name.to_owned());
        }
    }
    let text = js_string(&text);
    let (status, reply) = if executed_tools.is_empty() {
        (
            Status::Unverified,
            format!("No sandbox action was confirmed. {text}"),
        )
    } else if successful.len() != receipts.len() {
        (
            Status::Partial,
            format!("Some tool calls failed; the task is only partially verified.\n\n{text}"),
        )
    } else {
        (Status::Completed, text)
    };
    Ok(ComputerResult {
        status,
        reply,
        session_id: result
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        executed_tools,
        tool_errors: receipts.len() - successful.len(),
    })
}

/// `event.name || ''`: receipts whose name is not a string cannot be Cua.
fn receipt_name(event: &Value) -> Option<&str> {
    event.get("name").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    fn receipt(name: &str, output: &str) -> Value {
        json!({"type": "tool_result", "name": name, "output": output, "is_error": false})
    }

    fn ok_receipt() -> Value {
        receipt("terminal", r#"{"output":"sandbox-result","exit_code":0}"#)
    }

    fn final_event() -> Value {
        json!({"type": "result", "exit_code": 0, "text": "Verified sandbox-result.", "session_id": "test-session"})
    }

    #[test]
    fn discovery_alone_does_not_prove_execution() {
        let only = parse_computer_result(&stream(&[final_event()])).unwrap();
        assert_eq!(only.status, Status::Unverified);
        assert_eq!(
            only.reply,
            "No sandbox action was confirmed. Verified sandbox-result."
        );
        let listed = parse_computer_result(&stream(&[
            receipt("mcp_cua_list_spaces", "{}"),
            final_event(),
        ]))
        .unwrap();
        assert_eq!(listed.status, Status::Unverified);
        let done = parse_computer_result(&stream(&[ok_receipt(), final_event()])).unwrap();
        assert_eq!(done.status, Status::Completed);
        assert_eq!(done.executed_tools, vec!["terminal"]);
        assert_eq!(done.tool_errors, 0);
        assert_eq!(done.session_id.as_deref(), Some("test-session"));
    }

    #[test]
    fn failing_receipts_make_it_partial() {
        let failing = receipt("terminal", r#"{"exit_code":1}"#);
        let only_failing =
            parse_computer_result(&stream(&[failing.clone(), final_event()])).unwrap();
        assert_eq!(only_failing.status, Status::Unverified);
        assert_eq!(only_failing.tool_errors, 1);
        assert!(only_failing.executed_tools.is_empty());
        let partial =
            parse_computer_result(&stream(&[ok_receipt(), failing, final_event()])).unwrap();
        assert_eq!(partial.status, Status::Partial);
        assert_eq!(partial.tool_errors, 1);
        assert!(partial
            .reply
            .starts_with("Some tool calls failed; the task is only partially verified."));
    }

    #[test]
    fn untrusted_wrapper_and_nested_driver_errors() {
        let wrapped = "<untrusted_tool_result source=\"mcp__cua__call_tool\">\nExternal source data.\n\n{\"error\":\"driver failure\"}\n</untrusted_tool_result>";
        let r = parse_computer_result(&stream(&[
            receipt("mcp__cua__call_tool", wrapped),
            final_event(),
        ]))
        .unwrap();
        assert_eq!(r.status, Status::Unverified);
        let nested = json!({"type": "tool_result", "name": "mcp__cua__space_bash", "output": json!({"result": "{\"not_found\":\"nope\"}"}).to_string()});
        let r = parse_computer_result(&stream(&[nested, final_event()])).unwrap();
        assert_eq!(r.status, Status::Unverified);
    }

    #[test]
    fn exit_markers_in_output() {
        for (out, failed) in [
            ("[exit 1]", true),
            ("[exit 0]", false),
            ("[exit 01]", true),
            ("[exit 12] done", true),
            ("[exit 0abc]", false),
            ("nothing", false),
        ] {
            assert_eq!(claims_failed_exit(out), failed, "{out}");
        }
    }

    #[test]
    fn failures_and_quota() {
        let mut bad = final_event();
        bad["exit_code"] = json!(1);
        assert_eq!(
            parse_computer_result(&stream(&[bad.clone()])).unwrap_err(),
            Error::Failed
        );
        bad["error"] = json!("RESOURCE_EXHAUSTED quota");
        assert_eq!(
            parse_computer_result(&stream(&[bad])).unwrap_err(),
            Error::QuotaExhausted
        );
        assert!(quota_exhausted("rate limit hit"));
        assert!(quota_exhausted("rate_limit"));
        assert!(!quota_exhausted("rated limit"));
        assert_eq!(
            parse_computer_result("not json\n").unwrap_err(),
            Error::InvalidStream
        );
        assert_eq!(parse_computer_result("").unwrap_err(), Error::Failed);
    }

    #[test]
    fn tool_names_normalise() {
        assert_eq!(
            cua_tool("mcp_cua_space_bash").as_deref(),
            Some("space_bash")
        );
        assert_eq!(
            cua_tool("mcp__cua__space_write").as_deref(),
            Some("space_write")
        );
        assert_eq!(
            cua_tool("mcp__android__space_bash").as_deref(),
            Some("space_bash")
        );
        assert_eq!(cua_tool("terminal").as_deref(), Some("terminal"));
        assert_eq!(
            cua_tool("mcp__phone__phone_shell").as_deref(),
            Some("mcp__phone__phone_shell")
        );
        assert_eq!(cua_tool("mcp__phone__phone_home"), None);
        assert_eq!(cua_tool("web_search"), None);
        assert_eq!(cua_tool("delegate"), None);
    }

    #[test]
    fn android_receipts_are_observed_actions() {
        let r = json!({"type": "tool_result", "name": "mcp__android__space_bash", "output": "{\"exit_code\":0}"});
        let out = parse_computer_result(&stream(&[r, final_event()])).unwrap();
        assert_eq!(out.status, Status::Completed);
        assert_eq!(out.executed_tools, vec!["mcp__android__space_bash"]);
    }
}
