//! The few strings the server writes itself (push titles, digests, its own
//! notices), in English, Italian and Danish. Clients title inbox entries
//! by category in their own language; these are for pushes, which the
//! service worker shows as they arrive.

use super::model::Category;

/// `en` unless `it` or `da`.
pub fn lang(l: Option<&str>) -> &'static str {
    match l {
        Some("it") => "it",
        Some("da") => "da",
        _ => "en",
    }
}

pub fn title(c: Category, l: &str) -> &'static str {
    match (c, l) {
        (Category::LucinaMessage, _) => "Lucina",
        (Category::Reminder, "it") => "Promemoria",
        (Category::Reminder, "da") => "Påmindelse",
        (Category::Reminder, _) => "Reminder",
        (Category::Calendar, "it") => "Tra poco",
        (Category::Calendar, "da") => "Snart",
        (Category::Calendar, _) => "Coming up",
        (Category::DeviceConfirm, "it") => "Lucina aspetta una conferma",
        (Category::DeviceConfirm, "da") => "Lucina venter på din bekræftelse",
        (Category::DeviceConfirm, _) => "Lucina needs your OK",
        (Category::Shopping, "it") => "Lista della spesa",
        (Category::Shopping, "da") => "Indkøbsliste",
        (Category::Shopping, _) => "Shopping list",
        (Category::Presence, "it") => "Stato di Lucina",
        (Category::Presence, "da") => "Lucinas status",
        (Category::Presence, _) => "Lucina's status",
        (Category::System, _) => "Secretary",
    }
}

/// Title when several Lucina messages are unread.
pub fn lucina_burst(n: usize, l: &str) -> String {
    match l {
        "it" => format!("{n} messaggi da Lucina"),
        "da" => format!("{n} beskeder fra Lucina"),
        _ => format!("{n} messages from Lucina"),
    }
}

/// Body when previews are off.
pub fn hidden_body(l: &str) -> &'static str {
    match l {
        "it" => "Apri Secretary per leggerlo.",
        "da" => "Åbn Secretary for at læse den.",
        _ => "Open Secretary to read it.",
    }
}

pub fn digest_title(l: &str) -> &'static str {
    match l {
        "it" => "Mentre non c'eri",
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
        "it" => "Lucina non è raggiungibile.",
        "da" => "Lucina kan ikke nås.",
        _ => "Lucina is not reachable.",
    }
}

pub fn lucina_online(l: &str) -> &'static str {
    match l {
        "it" => "Lucina è di nuovo online.",
        "da" => "Lucina er online igen.",
        _ => "Lucina is back online.",
    }
}

pub fn new_device(name: &str, l: &str) -> String {
    match l {
        "it" => format!("Nuovo dispositivo collegato: {name}"),
        "da" => format!("Ny enhed logget ind: {name}"),
        _ => format!("New device signed in: {name}"),
    }
}

pub fn test_body(l: &str) -> &'static str {
    match l {
        "it" => "Le notifiche funzionano.",
        "da" => "Notifikationer virker.",
        _ => "Notifications work.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(lang(Some("it")), "it");
        assert_eq!(lang(Some("fr")), "en");
        assert_eq!(lang(None), "en");
        assert_eq!(title(Category::Reminder, "da"), "Påmindelse");
        assert_eq!(lucina_burst(3, "it"), "3 messaggi da Lucina");
        assert_eq!(
            digest_body(
                &[(Category::LucinaMessage, 3), (Category::Reminder, 1)],
                "en"
            ),
            "Lucina: 3 · Reminder: 1"
        );
    }
}
