//! Self-tests and diagnostics stay out of the owner's chat.
//!
//! A chat turn is a self-test when the request says so: `"conversation": "selftest"` (or
//! `"diagnostic"`), a `sessionId` starting with `selftest` or `diag`, or a message starting with
//! `[selftest]` / `[diag]`, `"test": true`, or a test `source` (`probe…`, `test…`: agents' probes and
//! test runs on his devices; [`is_test_source`]). It then runs in its own conversation:
//!
//! - session [`SESSION`] (its own recent turns and pending confirmations);
//! - timeline `selftest-timeline.jsonl` next to the owner's: never `timeline.jsonl`, so never
//!   the owner's chat, its SSE stream or a push (`GET /timeline?conversation=selftest` reads it);
//! - no Honcho memory write.
//!
//! Side effects the turn itself asks for (a reminder, a calendar event, a note) still happen:
//! a self-test should not ask for them.

use serde_json::Value;

/// The conversation name a caller sends.
pub const CONVERSATION: &str = "selftest";
/// The session every self-test turn runs in.
pub const SESSION: &str = "secretary_selftest";
/// Its timeline file, next to the owner's `timeline.jsonl`.
pub const TIMELINE_FILE: &str = "selftest-timeline.jsonl";

fn diagnostic_name(s: &str) -> bool {
    let s = s.trim().to_ascii_lowercase();
    s.starts_with("selftest") || s.starts_with("self-test") || s.starts_with("diag")
}

/// A client `source` that is test traffic, never the owner speaking.
pub fn is_test_source(source: &str) -> bool {
    let s = source.trim().to_ascii_lowercase();
    s.starts_with("probe") || s.starts_with("test")
}

/// The request asks for a self-test or diagnostic turn.
pub fn requested(body: &Value, session: &str, message: &str) -> bool {
    let conversation = body["conversation"].as_str().unwrap_or("");
    let lead = message.trim_start().to_ascii_lowercase();
    body["test"] == Value::Bool(true)
        || body["source"].as_str().is_some_and(is_test_source)
        || diagnostic_name(conversation)
        || diagnostic_name(session)
        || lead.starts_with("[selftest]")
        || lead.starts_with("[self-test]")
        || lead.starts_with("[diag")
}

/// A turn of this session is a self-test (it never writes the owner's timeline or memory).
pub fn is_session(session: &str) -> bool {
    session == SESSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_counts_as_a_self_test() {
        let none = json!({});
        assert!(requested(&json!({"conversation": "selftest"}), "", "ciao"));
        assert!(requested(
            &json!({"conversation": "Diagnostic"}),
            "",
            "ciao"
        ));
        assert!(requested(&none, "selftest-hack-night", "ciao"));
        assert!(requested(&none, "diag_42", "ciao"));
        assert!(requested(
            &none,
            "",
            "  [selftest] dove si tiene la Hack Night?"
        ));
        assert!(requested(&none, "", "[DIAG] ping"));
        assert!(!requested(&none, "", "ciao"));
        assert!(!requested(&none, "app", "come va il selftest di ieri?"));
        assert!(!requested(
            &json!({"conversation": "main"}),
            "ios-1",
            "ciao"
        ));
        assert!(requested(&json!({"source": "probe"}), "", "ciao"));
        assert!(requested(&json!({"source": "test-juno"}), "", "ciao"));
        assert!(requested(&json!({"test": true}), "", "ciao"));
        assert!(!requested(&json!({"source": "fortuna-chat"}), "", "ciao"));
        assert!(!requested(&json!({"test": false}), "", "ciao"));
        assert!(is_session(SESSION));
        assert!(!is_session("secretary_app"));
    }
}
