//! Presence questions (`src/discord-presence-context.js`): "sono online su
//! discord?", "cosa sto ascoltando" get today's online minutes, or an
//! explicit "unknown" when the presence watcher is down.

use crate::re;

/// `isPresenceQuestion`.
pub fn is_presence_question(text: &str) -> bool {
    re!(r"\bdiscord\b", "i").test(text)
        || re!(
            r"\b(?:cosa|che cosa|che)\s+(?:sto|stavo)\s+(?:ascoltando|sentendo|giocando)\b",
            "i"
        )
        .test(text)
        || re!(r"\ba\s+(?:cosa|che)\s+sto\s+giocando\b", "i").test(text)
        || re!(r"\b(?:che|quale)\s+canzone\b", "i").test(text)
        || re!(r"\bspotify\b", "i").test(text)
        || re!(r"\bwhat\s+am\s+i\s+(?:listening|playing)\b", "i").test(text)
        || re!(
            r"\b(?:sono|am\s+i|is\s+giuseppe)\s+(?:online|connesso|on)\b",
            "i"
        )
        .test(text)
}

pub const UNKNOWN_LINE: &str = "Discord: presence unknown (the presence watcher is not running or its data is older than 10 minutes).";

/// The second line for a presence question: today's online time.
pub fn online_today_line(minutes: i64) -> String {
    let h = minutes / 60;
    let today = if h > 0 {
        format!("{h} h {} min", minutes % 60)
    } else {
        format!("{minutes} min")
    };
    format!("Online on Discord today: {today}. (Live from Lucina's Discord presence watcher; invisible looks exactly like offline, so never claim to know he is invisible.)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions() {
        assert!(is_presence_question("sono online su discord?"));
        assert!(is_presence_question("cosa sto ascoltando?"));
        assert!(is_presence_question("What am I listening to"));
        assert!(!is_presence_question("cosa c'è da mangiare"));
        assert_eq!(
            online_today_line(135).split('.').next().unwrap(),
            "Online on Discord today: 2 h 15 min"
        );
    }
}
