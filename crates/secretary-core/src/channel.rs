//! Which app a chat turn came from. The channel names the owner's client: `web`, `ios`, `dsi`
//! (the Nintendo DSi's Fortuna shell) or `mac` (the Lucina app on his Mac). `voice` is the
//! LucinaVoice speakers' own thread and is set server-side only.
//!
//! The Fortuna firmware installed before these channels existed still sends `web`, so the label it
//! claims is corrected from what else the request says about itself. Shared by the gateway and
//! Lucina so both come to the same answer.

/// The channels a client may name for itself.
pub const APP_CHANNELS: &[&str] = &["web", "ios", "dsi", "mac"];

/// Every channel a chat turn may carry. `voice` is never claimed by a client.
pub const CHAT_CHANNELS: &[&str] = &["web", "ios", "dsi", "mac", "voice"];

/// The `source` label Fortuna's chat sends.
pub const FORTUNA_SOURCE: &str = "fortuna-chat";
/// The session id prefix of a Fortuna device (`fortuna-<deviceId>`).
pub const FORTUNA_SESSION_PREFIX: &str = "fortuna-";
/// The User-Agent prefix of the Fortuna shell (`Fortuna/1.62 (Nintendo DSi)`).
pub const FORTUNA_UA_PREFIX: &str = "Fortuna/";

/// The channel of a turn. An explicit `dsi`, `mac` or `ios` wins. A claimed `web` (or none) becomes
/// `dsi` when the request says it is Fortuna: `source` is `fortuna-chat`, the session id starts with
/// `fortuna-`, or the User-Agent starts with `Fortuna/`. Nothing else is guessed: only an explicit
/// `mac` gives `mac`. Any other claimed value (`voice`, unknown) is returned as it is.
pub fn client_channel(
    claimed: &str,
    source: Option<&str>,
    session: Option<&str>,
    user_agent: Option<&str>,
) -> String {
    let claimed = claimed.trim();
    if !(claimed.is_empty() || claimed == "web") {
        return claimed.to_owned();
    }
    let fortuna = source.is_some_and(|s| s == FORTUNA_SOURCE)
        || session.is_some_and(|s| s.starts_with(FORTUNA_SESSION_PREFIX))
        || user_agent.is_some_and(|u| u.trim_start().starts_with(FORTUNA_UA_PREFIX));
    if fortuna { "dsi" } else { "web" }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_channel_table() {
        let ua = Some("Fortuna/1.62 (Nintendo DSi)");
        // (claimed, source, session, user agent) -> channel
        let rows: &[(&str, Option<&str>, Option<&str>, Option<&str>, &str)] = &[
            ("web", Some("fortuna-chat"), None, None, "dsi"),
            ("web", None, Some("fortuna-abc123"), None, "dsi"),
            ("web", None, None, ua, "dsi"),
            ("", Some("fortuna-chat"), None, None, "dsi"),
            ("web", None, None, Some("Mozilla/5.0"), "web"),
            ("web", Some("juno-chat"), Some("juno-x"), None, "web"),
            ("web", None, None, None, "web"),
            ("", None, None, None, "web"),
            ("mac", None, None, None, "mac"),
            ("mac", Some("fortuna-chat"), None, None, "mac"),
            ("dsi", None, None, None, "dsi"),
            ("ios", Some("fortuna-chat"), Some("fortuna-1"), ua, "ios"),
            ("ios", None, None, None, "ios"),
            ("voice", None, Some("fortuna-1"), None, "voice"),
        ];
        for (claimed, source, session, agent, want) in rows {
            assert_eq!(
                client_channel(claimed, *source, *session, *agent),
                *want,
                "{claimed:?} {source:?} {session:?} {agent:?}"
            );
        }
    }

    #[test]
    fn client_channel_never_guesses_the_mac() {
        assert_eq!(
            client_channel("web", None, Some("mac-1"), Some("Lucina/1 (Macintosh)")),
            "web"
        );
    }
}
