//! Lucina MCP turns (`src/secretary-api.js`, "MCP turns"): ChatGPT, Grok or
//! Claude Code asking on the owner's behalf reach `/chat` through
//! secretary-server with session `mcp-<caller>[-…]` and a
//! `[via Lucina MCP, from <caller>]` prefix. Only allowlisted work runs.

use crate::jsre::group;
use crate::re;

/// `computer_task` is deliberately not here: a guarded MCP caller (chat scope) must not drive Lucina's
/// computer through `ask_lucina`; callers with the `computer` scope use the `computer_*` tools. A
/// trusted MCP turn (`LUCINA_MCP_TRUSTED=true`, live) is not guarded at all, so the mail-on-her-computer
/// route (`computer_route`) runs for it like for the owner.
///
/// Planner actions an MCP turn may run (`notes` is the notes bridge,
/// `tasks` the tasks bridge: read, add, complete; never delete; `game` the status of Lucina's Pokémon
/// run, never starting or pausing it).
pub const ALLOWED_ACTIONS: &[&str] = &[
    "reply",
    "list_reminders",
    "schedule_reminder",
    "notes",
    "tasks",
    "game",
];

/// Grocery intents an MCP turn may run.
pub const ALLOWED_GROCERY_INTENTS: &[&str] = &[
    "grocery.shopping_list.add",
    "grocery.purchase",
    "grocery.shopping_list.query",
    "grocery.offer.search",
    "grocery.deals.cheapest",
    "grocery.deals.lunch",
    "grocery.price.query",
    "grocery.expense.query",
    "grocery.pantries.query",
    "grocery.analytics.query",
];

/// Room the prefix may add on top of the 12 000-character message limit.
pub const PREFIX_ROOM: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTurn {
    pub caller: String,
    /// The message without the prefix.
    pub text: String,
}

/// `mcpTurnInfo({sessionId, message})`: `None` for a normal app turn.
/// `session_id` is the requested id (before the `secretary_` prefix).
pub fn turn_info(session_id: &str, message: &str) -> Option<McpTurn> {
    let prefix = re!(
        r"^\s*\[via Lucina MCP(?:,\s*from\s+([^\]]{1,64}))?[^\]]*\]\s*",
        "i"
    )
    .exec(message);
    let session = re!(r"^(?:secretary_)?mcp-([A-Za-z0-9_]+)?", "i").exec(session_id);
    if prefix.is_none() && session.is_none() {
        return None;
    }
    let from_prefix = prefix
        .as_ref()
        .map(|c| group(c, 1))
        .filter(|s| !s.is_empty());
    let from_session = session
        .as_ref()
        .map(|c| group(c, 1))
        .filter(|s| !s.is_empty());
    let caller: String = from_prefix
        .or(from_session)
        .unwrap_or("unknown")
        .trim()
        .chars()
        .take(64)
        .collect();
    let caller = if caller.is_empty() {
        "unknown".to_owned()
    } else {
        caller
    };
    let stripped = match &prefix {
        Some(c) => message[c.get(0).map(|m| m.end()).unwrap_or(0)..].trim(),
        None => message.trim(),
    };
    let text = if stripped.is_empty() {
        message.trim()
    } else {
        stripped
    };
    Some(McpTurn {
        caller,
        text: text.to_owned(),
    })
}

/// The note in every prompt of an MCP turn.
pub fn note(m: &McpTurn) -> String {
    format!(
        "This request comes from {} acting for the owner through Lucina MCP; Giuseppe is not typing it himself.",
        m.caller
    )
}

/// The note for an MCP turn when MCP callers are trusted (`LUCINA_MCP_TRUSTED`).
pub fn note_trusted(m: &McpTurn) -> String {
    format!(
        "This request comes from {} acting for the owner through Lucina MCP with the owner's token; Giuseppe is not typing it himself, but he authorized this client: act on it as you would on his own request (your own judgement, the hard limits still hold).",
        m.caller
    )
}

/// The note `source` for an MCP caller ("chatgpt", "claude-code", "grok").
pub fn note_source(caller: &str) -> String {
    let lower = caller.to_lowercase();
    let mut s = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            if dash {
                s.push('-');
                dash = false;
            }
            s.push(c);
        } else {
            dash = true;
        }
    }
    let s = s.trim_matches('-');
    let s: String = s.chars().take(32).collect();
    if s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        s
    } else {
        "lucina".into()
    }
}

/// The reply when an MCP turn asks for non-allowlisted work.
pub fn refusal(persona: &str, blocked: &str) -> String {
    format!("{persona}: I did not do that. \"{blocked}\" needs Giuseppe's own confirmation, so it does not run from Lucina MCP. Ask me in the Secretary app or on Telegram.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_session_and_sources() {
        let m = turn_info(
            "mcp-chatgpt-abc",
            "[via Lucina MCP, from chatgpt] aggiungi il latte",
        )
        .unwrap();
        assert_eq!(
            m,
            McpTurn {
                caller: "chatgpt".into(),
                text: "aggiungi il latte".into()
            }
        );
        let m = turn_info("app", "[via Lucina MCP, from Claude Code] post the news").unwrap();
        assert_eq!(m.caller, "Claude Code");
        assert_eq!(
            turn_info("mcp-grok", "ciao").unwrap(),
            McpTurn {
                caller: "grok".into(),
                text: "ciao".into()
            }
        );
        assert!(turn_info("ios-1234", "ciao").is_none());
        assert_eq!(note_source("Claude Code"), "claude-code");
        assert_eq!(note_source("!!"), "lucina");
        assert!(refusal("Lucina", "social_post_task")
            .contains("\"social_post_task\" needs Giuseppe's own confirmation"));
    }

    #[test]
    fn computer_work_is_for_trusted_turns_or_the_computer_tools() {
        assert!(!ALLOWED_ACTIONS.contains(&"computer_task"));
        // The 07:32 turn: the route exists, the guard decides whether this caller may use it.
        let m = turn_info(
            "mcp-www",
            "[via Lucina MCP, from www] Quick check only: has Giuseppe already SENT an email this morning? If you can see Outlook Sent, say yes/no.",
        )
        .unwrap();
        assert_eq!(m.caller, "www");
        assert!(crate::computer_route::wants_computer(&m.text));
        assert!(note_trusted(&m).contains("act on it as you would on his own request"));
    }
}
