//! What Lucina says to the owner about OTPs, on every surface (Telegram,
//! Secretary app, Rust Lucina). Replies that carry a code are `sensitive`:
//! never stored in the timeline or memory, shown once, gone at `expiresAt`.

use crate::proto::{clean_sender, Delivered};

pub const NOT_PRIVATE: &str = "OTP access requires Giuseppe’s own private app or Telegram chat.";
pub const DISARMED: &str = "OTP listening stopped; pending codes cleared.";
pub const UNAVAILABLE: &str =
    "The internet OTP bridge is unavailable. Check that the S20 FE has internet and Lucina OTP is enabled.";

pub fn status(armed: bool) -> &'static str {
    if armed {
        "The internet OTP bridge is ready and listening."
    } else {
        "The internet OTP bridge is ready. Listening is off."
    }
}

pub fn armed(continuous: bool) -> &'static str {
    if continuous {
        "Background OTP forwarding is enabled for new codes from any provider. Ask me here when you need a code; each code expires after two minutes."
    } else {
        "I’m listening for new OTPs from any provider for 5 minutes. Request a fresh code, then ask me for your OTP here."
    }
}

/// Nothing fresh is waiting (the relay answered 204).
pub fn waiting(continuous: bool) -> &'static str {
    if continuous {
        "No fresh OTP is waiting. Background forwarding is enabled. Ask me here within two minutes of receiving a new code. I cannot read SMS history."
    } else {
        "No fresh OTP is waiting. I’m listening for new codes over the internet for up to 5 minutes. Request one from the service, then ask me for your OTP here. I cannot read earlier SMS."
    }
}

/// The code itself. Only for the owner's private chat, marked sensitive.
pub fn delivered(otp: &Delivered) -> String {
    format!(
        "OTP from {}: {}\nCheck it matches the service you requested. Delivered once; expires shortly.",
        clean_sender(&otp.sender),
        otp.code
    )
}

/// Who is asking, as the transports describe it.
#[derive(Debug, Clone, Default)]
pub struct Asker<'a> {
    /// `"app"` (Secretary) or `"telegram"`.
    pub surface: &'a str,
    /// Secretary: the request came with the owner's session.
    pub owner_authorized: bool,
    /// The message came through the MCP (another agent asking on the owner's behalf).
    pub mcp: bool,
    /// Telegram: configured owner id, sender id, chat id, chat type, sender is a bot.
    pub owner_id: &'a str,
    pub user_id: &'a str,
    pub chat_id: &'a str,
    pub chat_type: &'a str,
    pub is_bot: bool,
}

/// Codes go only to the owner's own private chat: the Secretary app with the
/// owner's session (never via MCP), or a Telegram private chat whose sender
/// and chat are the configured owner.
pub fn private_owner(a: &Asker<'_>) -> bool {
    if a.surface == "app" {
        return a.owner_authorized && !a.mcp;
    }
    a.surface == "telegram"
        && !a.owner_id.is_empty()
        && a.owner_id.bytes().all(|b| b.is_ascii_digit())
        && a.chat_type == "private"
        && a.user_id == a.owner_id
        && a.chat_id == a.owner_id
        && !a.is_bot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_owner_only() {
        let tg = |chat_id, chat_type| Asker {
            surface: "telegram",
            owner_id: "42",
            user_id: "42",
            chat_id,
            chat_type,
            ..Default::default()
        };
        assert!(private_owner(&tg("42", "private")));
        assert!(!private_owner(&tg("-42", "group")));
        assert!(private_owner(&Asker {
            surface: "app",
            owner_authorized: true,
            ..Default::default()
        }));
        assert!(!private_owner(&Asker {
            surface: "app",
            owner_authorized: true,
            mcp: true,
            ..Default::default()
        }));
        assert!(!private_owner(&Asker {
            surface: "mcp",
            owner_authorized: true,
            ..Default::default()
        }));
    }

    #[test]
    fn delivered_text_cleans_sender() {
        let d = Delivered {
            sender: "<Bank>".into(),
            code: "482917".into(),
            received_at: 1,
            expires_at: 2,
            source: Default::default(),
        };
        assert!(delivered(&d).starts_with("OTP from Bank: 482917\n"));
    }
}
