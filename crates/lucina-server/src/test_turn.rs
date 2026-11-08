//! A voice test device's turns (docs/lucinavoice-protocol.md section 10.8).
//!
//! Agents measure the voice pipeline with a real device token listed in secretary-server's `SECRETARY_VOICE_TEST_DEVICES`.
//! Their turns must reach neither the owner's chat nor his phone nor his memory. secretary-server marks them: a chat turn
//! carries `"test": true` (only its own pipeline sets it, `/v1/chat` clears it), a note's `POST /v1/share` the form field
//! `test=1`. Such a turn:
//!
//! - runs as the owner's own turn (not guarded, routed by Jev, the voice fast path allowed) in the session [`SESSION`];
//! - writes every item with `meta.via = "test"` and `meta.conversation = "voice-test"`: `via` keeps it off his phone
//!   (secretary-server's `notify::ingest::agent_chat` is silent for any `via`), and the store files it under the conversation
//!   `test:voice-test`, never the main conversation or the shared Lucina Voice thread;
//! - is not remembered: no Honcho write, no About-you observation, no attachment memory, and a note's audio is not put in the
//!   library. Its own recent turns (session [`SESSION`]) are kept so that a follow-up in the same test conversation works.

use lucina_store::timeline::Meta;

/// The session id a test turn is sent under (the planner prefixes `secretary_`).
pub const REQUESTED: &str = "voice-test";
/// The session of a test turn.
pub const SESSION: &str = "secretary_voice-test";
/// `meta.conversation` of its items.
pub const CONVERSATION: &str = "voice-test";
/// `meta.via` of its items.
pub const VIA: &str = "test";

/// This session belongs to a test device.
pub fn is_session(session: &str) -> bool {
    session == SESSION
}

/// The meta of every item of a test turn.
pub fn meta() -> Meta {
    Meta {
        via: Some(VIA.into()),
        conversation: Some(CONVERSATION.into()),
        ..Meta::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marks_and_the_session() {
        let m = meta();
        assert_eq!(m.via.as_deref(), Some("test"));
        assert_eq!(m.conversation.as_deref(), Some("voice-test"));
        assert_eq!(
            lucina_memory::secretary_session_id(REQUESTED, "app"),
            SESSION
        );
        assert!(is_session(SESSION));
        assert!(!is_session("secretary_lucinavoice"));
    }
}
