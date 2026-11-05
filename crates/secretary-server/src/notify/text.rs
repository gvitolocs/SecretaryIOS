//! The few strings the server writes itself (push titles, digests, its own
//! notices), in English and Danish (never Italian). Clients title inbox entries
//! by category in their own language; these are for pushes, which the
//! service worker shows as they arrive.

use super::model::Category;

/// `en` unless `da`. Owner rule (2026-11-02): never Italian, even when the client asks for `it`.
pub fn lang(l: Option<&str>) -> &'static str {
    match l {
        Some("da") => "da",
        _ => "en",
    }
}

pub fn title(c: Category, l: &str) -> &'static str {
    match (c, l) {
        (Category::LucinaMessage, _) => "Lucina",
        (Category::Reminder, "da") => "Påmindelse",
        (Category::Reminder, _) => "Reminder",
        (Category::Calendar, "da") => "Snart",
        (Category::Calendar, _) => "Coming up",
        (Category::DeviceConfirm, "da") => "Lucina venter på din bekræftelse",
        (Category::DeviceConfirm, _) => "Lucina needs your OK",
        (Category::Agent, "da") => "Agenter",
        (Category::Agent, _) => "Agents",
        (Category::Shopping, "da") => "Indkøbsliste",
        (Category::Shopping, _) => "Shopping list",
        (Category::Presence, "da") => "Lucinas status",
        (Category::Presence, _) => "Lucina's status",
        (Category::System, _) => "Secretary",
        (Category::Approval, "da") => "Din tilladelse ønskes",
        (Category::Approval, _) => "Approval needed",
    }
}

/// An agent push, by `data.reason` (lucina-agentd sends no title).
pub fn agent_title(reason: Option<&str>, l: &str) -> &'static str {
    match (reason, l) {
        (Some("permission"), "da") => "En agent beder om tilladelse",
        (Some("permission"), _) => "Agent needs permission",
        (Some("error"), "da") => "En agent kræver opmærksomhed",
        (Some("error"), _) => "Agent needs attention",
        (_, "da") => "Agent er færdig",
        _ => "Agent finished",
    }
}

/// Title of a "time to leave" calendar alert.
pub fn leave_title(l: &str) -> &'static str {
    match l {
        "da" => "Tid til at tage afsted",
        _ => "Time to leave",
    }
}

/// Title when several Lucina messages are unread.
pub fn lucina_burst(n: usize, l: &str) -> String {
    match l {
        "da" => format!("{n} beskeder fra Lucina"),
        _ => format!("{n} messages from Lucina"),
    }
}

/// Body when previews are off.
pub fn hidden_body(l: &str) -> &'static str {
    match l {
        "da" => "Åbn Secretary for at læse den.",
        _ => "Open Secretary to read it.",
    }
}

pub fn digest_title(l: &str) -> &'static str {
    match l {
        "da" => "Mens du var væk",
        _ => "While you were away",
    }
}

/// "Lucina: 3 · Reminder: 1"
pub fn digest_body(counts: &[(Category, usize)], l: &str) -> String {
    counts
        .iter()
        .map(|(c, n)| format!("{}: {n}", title(*c, l)))
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn lucina_offline(l: &str) -> &'static str {
    match l {
        "da" => "Lucina kan ikke nås.",
        _ => "Lucina is not reachable.",
    }
}

pub fn lucina_online(l: &str) -> &'static str {
    match l {
        "da" => "Lucina er online igen.",
        _ => "Lucina is back online.",
    }
}

pub fn new_device(name: &str, l: &str) -> String {
    match l {
        "da" => format!("Ny enhed logget ind: {name}"),
        _ => format!("New device signed in: {name}"),
    }
}

pub fn test_body(l: &str) -> &'static str {
    match l {
        "da" => "Notifikationer virker.",
        _ => "Notifications work.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        // Never Italian, even when the client or an old setting says `it`.
        assert_eq!(lang(Some("it")), "en");
        assert_eq!(lang(Some("fr")), "en");
        assert_eq!(lang(None), "en");
        assert_eq!(title(Category::Reminder, "da"), "Påmindelse");
        assert_eq!(lucina_burst(3, "it"), "3 messages from Lucina");
        assert_eq!(lucina_burst(3, "da"), "3 beskeder fra Lucina");
        assert_eq!(
            digest_body(
                &[(Category::LucinaMessage, 3), (Category::Reminder, 1)],
                "en"
            ),
            "Lucina: 3 · Reminder: 1"
        );
    }
}
