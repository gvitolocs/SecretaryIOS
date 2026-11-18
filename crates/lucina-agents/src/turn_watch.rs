//! Turn watch for ACP agents: when a running turn is dead although the agent never
//! said so.
//!
//! OpenCode (`opencode acp`) on Zen's free tier logs `Free usage limit reached` to its own
//! log and never answers `session/prompt`, so the turn hung as "running" (2026-11-08,
//! agent cc504145) and the quota hand-off could not pass it on. agentd now watches each
//! ACP turn:
//! - **Limit in the agent's log**: a line of the agent's error log for this session
//!   ([`limit_in_log_line`]) ends the turn at once as a quota stop.
//! - **Stall**: no `session/update` for the stall time while no tool call and no permission
//!   request is open ends it as a quota stop too (owner, 2026-11-08). An open tool (a long
//!   build) or a permission waiting for the owner never counts as a stall.
//!
//! Verus model: `verification/verus/acp_turn_watch.rs` (W1-W5); differential test
//! `tests/verus_turn_watch.rs`.

/// What the watch reads about a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchFacts {
    /// Milliseconds since the agent last sent anything for this turn.
    pub idle_ms: u64,
    /// The stall time, in milliseconds; `None` = no stall rule for this agent.
    pub stall_ms: Option<u64>,
    /// Tool calls started and not finished.
    pub open_tools: u32,
    /// Permission requests waiting for an answer.
    pub open_permissions: u32,
    /// The agent's own log reported a usage limit for this session.
    pub log_limit: bool,
}

/// The watch's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep waiting.
    Wait,
    /// The agent logged a usage limit: end the turn as a quota stop.
    Limit,
    /// No output for the stall time with nothing open: end the turn as a quota stop.
    Stalled,
}

/// The verdict for one look at a running turn (W1-W5 in the Verus model).
pub fn verdict(f: &WatchFacts) -> Verdict {
    if f.open_permissions > 0 {
        // The owner may answer hours later; the agent isn't calling a model meanwhile.
        return Verdict::Wait;
    }
    if f.log_limit {
        return Verdict::Limit;
    }
    match f.stall_ms {
        Some(limit) if f.open_tools == 0 && f.idle_ms >= limit => Verdict::Stalled,
        _ => Verdict::Wait,
    }
}

/// Error wordings in an agent's log that mean the account or tier ran out.
const LIMIT_PHRASES: &[&str] = &[
    "free usage limit reached",
    "usage limit",
    "rate limit",
    "too many requests",
    "insufficient balance",
    "insufficient_quota",
    "quota exceeded",
    "exceeded your current quota",
    "payment required",
];

/// The error of one OpenCode log line (`level=ERROR … session.id=<id> … error.error="…"`)
/// when it is a usage limit for `session_id`.
pub fn limit_in_log_line(line: &str, session_id: &str) -> Option<String> {
    if session_id.is_empty() || !line.contains("level=ERROR") {
        return None;
    }
    let key = format!("session.id={session_id}");
    let at = line.find(&key)?;
    // The id must end there (no longer id that starts the same).
    if line[at + key.len()..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let error = match line.find("error.error=\"") {
        Some(i) => {
            let rest = &line[i + "error.error=\"".len()..];
            rest[..rest.find('"').unwrap_or(rest.len())].to_string()
        }
        None => return None,
    };
    let lower = error.to_lowercase();
    LIMIT_PHRASES.iter().any(|p| lower.contains(p)).then_some(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Agent cc504145's line from `~/.local/share/opencode/log/opencode.log`, 2026-11-08.
    const REAL: &str = r#"timestamp=2026-11-08T10:23:36.086Z level=ERROR run=0d6eb771 message="stream error" providerID=opencode modelID=deepseek-v4.1-flash-free session.id=ses_e4550afe4ffec45cR8Hl7cjxEM small=false agent=build mode=primary error.error="AI_APICallError: Free usage limit reached. Free models work in OpenCode (https://opencode.ai); subscribe to OpenCode Go to keep going: https://opencode.ai/go""#;

    #[test]
    fn the_real_zen_limit_line_is_a_limit_for_its_session_only() {
        let e = limit_in_log_line(REAL, "ses_e4550afe4ffec45cR8Hl7cjxEM").unwrap();
        assert!(e.starts_with("AI_APICallError: Free usage limit reached."), "{e}");
        assert_eq!(limit_in_log_line(REAL, "ses_other"), None);
        assert_eq!(limit_in_log_line(REAL, "ses_e4550afe4ffec45cR8Hl7cjx"), None, "a prefix of the id is another session");
        assert_eq!(limit_in_log_line(REAL, ""), None);
        assert_eq!(limit_in_log_line(&REAL.replace("level=ERROR", "level=WARN"), "ses_e4550afe4ffec45cR8Hl7cjxEM"), None);
        let other = REAL.replace("Free usage limit reached", "connection reset");
        assert_eq!(limit_in_log_line(&other, "ses_e4550afe4ffec45cR8Hl7cjxEM"), None, "other errors are left to the stall rule");
    }

    fn facts() -> WatchFacts {
        WatchFacts { idle_ms: 0, stall_ms: Some(1000), open_tools: 0, open_permissions: 0, log_limit: false }
    }

    #[test]
    fn verdicts() {
        assert_eq!(verdict(&facts()), Verdict::Wait);
        assert_eq!(verdict(&WatchFacts { idle_ms: 999, ..facts() }), Verdict::Wait);
        assert_eq!(verdict(&WatchFacts { idle_ms: 1000, ..facts() }), Verdict::Stalled);
        assert_eq!(verdict(&WatchFacts { idle_ms: 5000, open_tools: 1, ..facts() }), Verdict::Wait, "a long tool is not a stall");
        assert_eq!(verdict(&WatchFacts { idle_ms: 5000, open_permissions: 1, ..facts() }), Verdict::Wait, "the owner may answer late");
        assert_eq!(verdict(&WatchFacts { idle_ms: 5000, stall_ms: None, ..facts() }), Verdict::Wait);
        assert_eq!(verdict(&WatchFacts { log_limit: true, ..facts() }), Verdict::Limit);
        assert_eq!(verdict(&WatchFacts { log_limit: true, open_tools: 2, stall_ms: None, ..facts() }), Verdict::Limit);
        assert_eq!(verdict(&WatchFacts { log_limit: true, open_permissions: 1, ..facts() }), Verdict::Wait);
    }
}
