//! Lucina's Pokémon Red run from chat: which messages ask about it, and the replies.
//!
//! Port of the Node `src/game-bridge.js` (parseGameIntent, statusReply and the reply texts). Only messages that
//! name Pokémon together with the game are taken, so card talk (Pokoin, prices) never lands here.

use serde_json::Value;

use crate::re;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Status,
    Start,
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameIntent {
    pub kind: Kind,
    pub lang: &'static str,
}

fn game_lang(t: &str) -> &'static str {
    if re!(
        r"[æøå]|\b(hvordan|hvad|spil|spillet|stands|fortsæt|genoptag)\b",
        "iu"
    )
    .test(t)
    {
        return "da";
    }
    if re!(r"\b(come|va|riprendi|ricomincia|metti|pausa|partita|gioco|gioca|giocare|ferma|sospendi|dove|sei)\b", "iu").test(t) {
        return "it";
    }
    crate::tasks::guess_lang(t)
}

/// `parseGameIntent`: `None` when the message is not about Lucina's game.
pub fn parse_game_intent(text: &str) -> Option<GameIntent> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 300 || !re!(r"pok[eé]mon|pokered", "iu").test(t) {
        return None;
    }
    if re!(r"\b(cards?|cart[ae]|kort(?:et)?|tcg|price[sd]?|prezz[oi]|pris(?:er)?|pokoin|booster|set)\b", "iu").test(t) {
        return None;
    }
    let lang = game_lang(t);
    if re!(
        r"\b(pause|stop|halt|ferma(?:ti)?|pausa|sospendi|smetti|stands|stop(?:pe)?|hold pause)\b",
        "iu"
    )
    .test(t)
    {
        return Some(GameIntent {
            kind: Kind::Pause,
            lang,
        });
    }
    if re!(r"\b(start|play|resume|continue|unpause|keep playing|riprendi|ricomincia|gioca|avvia|continua|fai ripartire|spil|fortsæt|genoptag|start(?:e)?)\b", "iu").test(t)
        && !t.contains('?')
    {
        return Some(GameIntent { kind: Kind::Start, lang });
    }
    if re!(r"\b(red|rosso|game|gioco|spil(?:let)?|run|partita|play(?:ing)?|gioca(?:re|ndo)?|spiller|adventure|avventura|eventyr)\b", "iu").test(t)
        || re!(r"\?|\b(how|what|where|status|come|cosa|dove|a che punto|hvordan|hvor|hvad|update|aggiornami|news|level|livello)\b", "iu").test(t)
    {
        return Some(GameIntent { kind: Kind::Status, lang });
    }
    None
}

pub fn off(lang: &str) -> &'static str {
    match lang {
        "it" => "Il mio gioco Pokémon non è attivo in questo momento (il server del gioco non risponde).",
        "da" => "Mit Pokémon-spil kører ikke lige nu (spilserveren svarer ikke).",
        _ => "My Pokémon game is not running right now (the game server does not answer).",
    }
}

/// Reply to a start (`running` true) or pause request; `changed` false when it already was that way.
pub fn control_reply(lang: &str, running: bool, changed: bool) -> &'static str {
    match (lang, running, changed) {
        ("it", true, true) => "Riprendo Pokémon Rosso: sto giocando di nuovo.",
        ("it", true, false) => "Sto già giocando a Pokémon Rosso.",
        ("it", false, true) => "Ho messo in pausa la mia partita a Pokémon Rosso. Dimmi \"riprendi Pokémon\" quando vuoi che continui.",
        ("it", false, false) => "La partita a Pokémon Rosso è già in pausa.",
        ("da", true, true) => "Jeg spiller Pokémon Red igen.",
        ("da", true, false) => "Jeg spiller allerede Pokémon Red.",
        ("da", false, true) => "Mit Pokémon Red-spil er sat på pause. Sig \"spil Pokémon\", når jeg skal fortsætte.",
        ("da", false, false) => "Mit Pokémon Red-spil er allerede på pause.",
        (_, true, true) => "Back to Pokémon Red — I'm playing again.",
        (_, true, false) => "I'm already playing Pokémon Red.",
        (_, false, true) => "Paused my Pokémon Red run. Say \"riprendi Pokémon\" or \"play Pokémon\" when you want me to continue.",
        (_, false, false) => "My Pokémon Red run is already paused.",
    }
}

fn place_name(map: &str) -> String {
    map.to_lowercase()
        .split('_')
        .map(|w| {
            if re!(r"^(b?\d+f)$").test(w) {
                w.to_uppercase()
            } else {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `statusReply`: where Lucina is, her team, badges, objective and last note, from the game's `/state`.
pub fn status_reply(state: &Value, lang: &str) -> String {
    let team = state["party"]
        .as_array()
        .map(|p| {
            p.iter()
                .map(|m| {
                    let name = m["name"].as_str().unwrap_or("");
                    let species = m["species"].as_str().unwrap_or("");
                    let sp = if !species.is_empty() && species != name {
                        format!(" ({species})")
                    } else {
                        String::new()
                    };
                    format!("{name}{sp} Lv{} {}/{}", m["level"], m["hp"], m["max_hp"])
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| match lang {
            "it" => "ancora nessun Pokémon".into(),
            "da" => "ingen Pokémon endnu".into(),
            _ => "no Pokémon yet".into(),
        });
    let note = state["events"]
        .as_array()
        .and_then(|e| {
            e.iter()
                .filter_map(|x| x.as_str())
                .filter(|x| !re!(r"^(Game server restarted|Lucina is playing|Paused by owner|New objective:)", "u").test(x))
                .last()
                .map(String::from)
        })
        .unwrap_or_default();
    let running = state["running"].as_bool().unwrap_or(false);
    let place = place_name(state["map_name"].as_str().unwrap_or(""));
    let battle = state["mode"].as_str() == Some("Battle");
    let badges = state["badges"].as_u64().unwrap_or(0);
    let objective = state["objective"].as_str().unwrap_or("");
    match lang {
        "it" => format!(
            "Pokémon Rosso ({}): sono a {place}{}. Squadra: {team}. Medaglie: {badges}.{}{}",
            if running { "sto giocando" } else { "in pausa" },
            if battle { ", in lotta" } else { "" },
            if objective.is_empty() {
                String::new()
            } else {
                format!(" Obiettivo: {objective}.")
            },
            if note.is_empty() {
                String::new()
            } else {
                format!(" Ultima nota: {note}")
            }
        ),
        "da" => format!(
            "Pokémon Red ({}): jeg er i {place}{}. Hold: {team}. Medaljer: {badges}.{}{}",
            if running { "spiller" } else { "på pause" },
            if battle { ", i en kamp" } else { "" },
            if objective.is_empty() {
                String::new()
            } else {
                format!(" Mål: {objective}.")
            },
            if note.is_empty() {
                String::new()
            } else {
                format!(" Seneste note: {note}")
            }
        ),
        _ => format!(
            "Pokémon Red ({}): I'm in {place}{}. Team: {team}. Badges: {badges}.{}{}",
            if running { "playing" } else { "paused" },
            if battle { ", in a battle" } else { "" },
            if objective.is_empty() {
                String::new()
            } else {
                format!(" Objective: {objective}.")
            },
            if note.is_empty() {
                String::new()
            } else {
                format!(" Last note: {note}")
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_messages_about_the_game() {
        let k = |t: &str| parse_game_intent(t).map(|i| (i.kind, i.lang));
        assert_eq!(
            k("come va la partita a Pokémon?"),
            Some((Kind::Status, "it"))
        );
        assert_eq!(
            k("how is your Pokémon Red game going?"),
            Some((Kind::Status, "en"))
        );
        assert_eq!(
            k("hvordan går Pokémon-spillet?"),
            Some((Kind::Status, "da"))
        );
        assert_eq!(k("metti in pausa Pokémon"), Some((Kind::Pause, "it")));
        assert_eq!(k("pause the Pokémon game").map(|x| x.0), Some(Kind::Pause));
        assert_eq!(k("riprendi a giocare a Pokémon"), Some((Kind::Start, "it")));
        assert_eq!(k("play Pokémon").map(|x| x.0), Some(Kind::Start));
        assert_eq!(k("what is the price of this Pokémon card?"), None);
        assert_eq!(k("quanto vale la carta Pokémon Charizard?"), None);
        assert_eq!(k("add milk to the shopping list"), None);
    }

    #[test]
    fn status_in_the_owner_language() {
        let s = json!({"running": true, "map_name": "MT_MOON_B2F", "mode": "Overworld", "badges": 1, "objective": "Reach Cerulean City",
            "party": [{"name": "SHELLY", "species": "WARTORTLE", "level": 25, "hp": 60, "max_hp": 75}],
            "events": ["Lucina is playing", "Bubble is 4x on Geodude."]});
        assert_eq!(
            status_reply(&s, "it"),
            "Pokémon Rosso (sto giocando): sono a Mt Moon B2F. Squadra: SHELLY (WARTORTLE) Lv25 60/75. Medaglie: 1. Obiettivo: Reach Cerulean City. Ultima nota: Bubble is 4x on Geodude."
        );
    }
}
