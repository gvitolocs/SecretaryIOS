//! The tool loop: at most `max_rounds` model calls (6), a total deadline,
//! tool errors returned to the model as `{"error": ...}`, confirmation-gated
//! tools turned into pending actions instead of running.

use std::time::Duration;

use lucina_llm::{ChatModel, ChatRequest, LlmError, Message, Role, ToolCall};
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::pending::{PendingAction, PendingStore};
use crate::{is_denied_name, Ctx, Origin, Registry};

pub const MAX_ROUNDS: usize = 6;
const MAX_TOOL_OUTPUT_CHARS: usize = 8000;
const PER_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_rounds: usize,
    pub deadline: Duration,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    /// Characters of one tool result the model sees (longer ones are cut).
    pub max_tool_output_chars: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_rounds: MAX_ROUNDS,
            deadline: Duration::from_secs(90),
            model: None,
            temperature: Some(0.4),
            max_tool_output_chars: MAX_TOOL_OUTPUT_CHARS,
        }
    }
}

/// One tool call and what came of it.
#[derive(Debug, Clone)]
pub struct ToolRecord {
    pub call_id: String,
    pub name: String,
    pub args: Value,
    pub ok: bool,
    /// What the model saw.
    pub output: Value,
    pub summary: String,
    /// Set when the call was deferred for confirmation (not executed).
    pub pending: Option<PendingAction>,
    pub ms: u64,
}

impl ToolRecord {
    pub fn executed(&self) -> bool {
        self.pending.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The model answered with text.
    Answered,
    /// Rounds used up; the last call ran without tools.
    MaxRounds,
    /// The total deadline passed.
    Deadline,
}

#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub text: String,
    pub rounds: usize,
    pub records: Vec<ToolRecord>,
    pub stop: Stop,
    pub messages: Vec<Message>,
}

fn clip_output(v: &Value, max: usize) -> String {
    let s = v.to_string();
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        json!({"truncated": true, "partial": cut}).to_string()
    }
}

/// Execute one validated call now (no confirmation check): the path for a
/// confirmed pending action, and the inner step of [`run`].
pub async fn execute(
    registry: &Registry,
    name: &str,
    args: Value,
    ctx: &Ctx,
    timeout: Duration,
) -> (bool, Value, String) {
    let Some(tool) = registry.get(name) else {
        return (
            false,
            json!({"error": format!("unknown tool {name}")}),
            name.to_owned(),
        );
    };
    let summary = tool.summarize(&args);
    if is_denied_name(name) || tool.moves_money() {
        return (
            false,
            json!({"error": "refused: Lucina never buys anything or moves money"}),
            summary,
        );
    }
    if let Err(e) = registry.validate(name, &args) {
        return (false, json!({"error": e}), summary);
    }
    let (ok, mut output) =
        match tokio::time::timeout(timeout.min(PER_TOOL_TIMEOUT), tool.call(args, ctx)).await {
            Ok(Ok(v)) => (true, v),
            Ok(Err(e)) => (false, json!({"error": e.0})),
            Err(_) => (false, json!({"error": "tool timed out"})),
        };
    // A tool result goes to the model, the timeline and Honcho: whatever a
    // tool or an upstream echoed back, no secret value leaves this point.
    lucina_secrets::redactor().redact_json(&mut output);
    (ok, output, summary)
}

/// What a model's tool call becomes in the turn that proposes it (Verus
/// kernel `propose` in `verification/verus/confirmation_gate.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallGate {
    /// Needs the owner's yes: stored as a pending action, not run.
    Defer,
    /// Goes to [`execute`] (which still refuses unknown, denied and invalid calls).
    Execute,
    /// Needs the owner's yes but the turn came through Lucina MCP: refused.
    RefuseMcp,
}

pub fn call_gate(registry: &Registry, name: &str, args: &Value, origin: Origin) -> CallGate {
    match registry.get(name) {
        Some(tool)
            if tool.needs_confirmation(args)
                && !is_denied_name(name)
                && !tool.moves_money()
                && registry.validate(name, args).is_ok() =>
        {
            if origin == Origin::Mcp {
                CallGate::RefuseMcp
            } else {
                CallGate::Defer
            }
        }
        _ => CallGate::Execute,
    }
}

async fn handle_call(
    registry: &Registry,
    call: &ToolCall,
    ctx: &Ctx,
    pending: &PendingStore,
    remaining: Duration,
) -> ToolRecord {
    let started = std::time::Instant::now();
    let name = call.function.name.clone();
    let raw = call.function.arguments.trim();
    let args: Value = if raw.is_empty() {
        json!({})
    } else {
        match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                return ToolRecord {
                    call_id: call.id.clone(),
                    name: name.clone(),
                    args: Value::Null,
                    ok: false,
                    output: json!({"error": format!("arguments are not valid JSON: {e}")}),
                    summary: name,
                    pending: None,
                    ms: 0,
                }
            }
        }
    };
    let gate = call_gate(registry, &name, &args, ctx.origin);
    if gate == CallGate::RefuseMcp {
        let summary = registry
            .get(&name)
            .map(|t| t.summarize(&args))
            .unwrap_or_else(|| name.clone());
        return ToolRecord {
            call_id: call.id.clone(),
            name,
            args,
            ok: false,
            output: json!({"error": "refused: this needs the owner's own confirmation, so it does not run from Lucina MCP. Ask the owner to do it in the Secretary app."}),
            summary,
            pending: None,
            ms: started.elapsed().as_millis() as u64,
        };
    }
    if let Some(tool) = registry.get(&name) {
        if gate == CallGate::Defer {
            let summary = tool.summarize(&args);
            let action = pending.create(&ctx.session_id, &name, args.clone(), &summary, ctx.now);
            return ToolRecord {
                call_id: call.id.clone(),
                name,
                args,
                ok: true,
                output: json!({
                    "status": "awaiting_confirmation",
                    "pendingActionId": action.id,
                    "expiresAt": action.expires_at,
                    "action": summary,
                    "instruction": "Not done yet. Ask the owner to confirm with an explicit yes in their next message; it expires in 10 minutes. Do not claim it is done."
                }),
                summary: action.summary.clone(),
                pending: Some(action),
                ms: started.elapsed().as_millis() as u64,
            };
        }
    }
    let (ok, output, summary) = execute(registry, &name, args.clone(), ctx, remaining).await;
    ToolRecord {
        call_id: call.id.clone(),
        name,
        args,
        ok,
        output,
        summary,
        pending: None,
        ms: started.elapsed().as_millis() as u64,
    }
}

/// Run the loop. `on_executed` sees every executed (not deferred) tool call
/// right after it finished (the server appends it to the timeline).
pub async fn run(
    model: &dyn ChatModel,
    registry: &Registry,
    mut messages: Vec<Message>,
    ctx: &Ctx,
    pending: &PendingStore,
    cfg: &AgentConfig,
    on_executed: &(dyn Fn(&ToolRecord) + Send + Sync),
) -> Result<AgentOutcome, LlmError> {
    let deadline = Instant::now() + cfg.deadline;
    let mut records = Vec::new();
    let specs = registry.specs();
    for round in 0..cfg.max_rounds.max(1) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(AgentOutcome {
                text: String::new(),
                rounds: round,
                records,
                stop: Stop::Deadline,
                messages,
            });
        }
        let last = round + 1 == cfg.max_rounds.max(1);
        let request = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: if specs.is_empty() {
                Vec::new()
            } else {
                specs.clone()
            },
            tool_choice: (!specs.is_empty()).then(|| {
                if last {
                    "none".to_owned()
                } else {
                    "auto".to_owned()
                }
            }),
            temperature: cfg.temperature,
            max_tokens: None,
            purpose: "agent",
            json_object: false,
        };
        let response = match tokio::time::timeout(remaining, model.complete(request)).await {
            Ok(r) => r?,
            Err(_) => {
                return Ok(AgentOutcome {
                    text: String::new(),
                    rounds: round + 1,
                    records,
                    stop: Stop::Deadline,
                    messages,
                })
            }
        };
        let calls = response.tool_calls().to_vec();
        if calls.is_empty() || last {
            let text = response.message.text().trim().to_owned();
            messages.push(response.message);
            let stop = if last && !calls.is_empty() {
                Stop::MaxRounds
            } else {
                Stop::Answered
            };
            return Ok(AgentOutcome {
                text,
                rounds: round + 1,
                records,
                stop,
                messages,
            });
        }
        messages.push(Message {
            role: Role::Assistant,
            content: response.message.content.clone(),
            tool_calls: Some(calls.clone()),
            tool_call_id: None,
            reasoning_content: response.message.reasoning_content.clone(),
        });
        for call in &calls {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let record = handle_call(registry, call, ctx, pending, remaining).await;
            if record.executed() {
                on_executed(&record);
            }
            tracing::info!(tool = %record.name, ok = record.ok, deferred = record.pending.is_some(), ms = record.ms, "tool call");
            messages.push(Message::tool_result(
                call.id.clone(),
                clip_output(&record.output, cfg.max_tool_output_chars),
            ));
            records.push(record);
        }
    }
    Ok(AgentOutcome {
        text: String::new(),
        rounds: cfg.max_rounds,
        records,
        stop: Stop::MaxRounds,
        messages,
    })
}

/// One call through the confirmation and MCP gates (tests of other modules).
#[cfg(test)]
pub(crate) async fn handle_call_for_test(
    registry: &Registry,
    call: &ToolCall,
    ctx: &Ctx,
    pending: &PendingStore,
) -> ToolRecord {
    handle_call(registry, call, ctx, pending, Duration::from_secs(30)).await
}
