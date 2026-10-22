//! Capability hints (`src/lucina-capability-hints.js`) and the IT/EN reply
//! language detector (`src/reply-language.js`): non-casual chat replies get
//! a "💡 Posso anche / I can also" block when the message looks actionable.

use unicode_normalization::UnicodeNormalization;

use crate::heuristics::*;
use crate::re;

const ITALIAN: &str = r"\b(che|non|per|con|sono|sei|siamo|hai|hanno|abbiamo|ciao|grazie|prego|anche|questo|questa|quello|quella|della|dello|delle|degli|dei|nel|nella|alla|allo|agli|una|uno|gli|piu|puoi|posso|possiamo|fatto|fare|essere|avere|quando|dove|perche|cosa|tutto|tutti|bene|adesso|oggi|domani|ieri|sempre|mai|molto|ancora|allora|invece|pero|quindi|scrivi|scrivere|manda|mandare|dimmi|cerca|cercare|trova|trovare|mostra|voglio|vorrei|mio|mia|miei|mie|tuo|tua|nostro|vostro)\b";
const ENGLISH: &str = r"\b(the|and|you|your|yours|my|mine|that|this|these|those|with|have|has|had|can|could|would|should|what|when|where|why|which|who|how|there|here|about|from|for|not|but|they|them|their|is|are|was|were|will|want|need|help|thanks|thank|okay|just|also|been|being|does|did|doesn|don|please|search|find|show|send|give|make|get|today|tomorrow|yesterday|something|anything|everything|because|after|before|then|than|very|really|good|great|sorry|travel|i'?m|it'?s|let'?s)\b";

/// `detectReplyLanguage`: `None` without enough signal.
pub fn detect_language(value: &str) -> Option<&'static str> {
    let lower = value.to_lowercase();
    let text: String = re!(r"https?:\/\/\S+")
        .replace_all(&lower, " ")
        .nfc()
        .collect();
    if lucina_store::timeutil::utf16_len(text.trim()) < 3 {
        return None;
    }
    let folded: String = text
        .nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect();
    let mut it = re!(ITALIAN, "i").find_all(&folded).len();
    let en = re!(ENGLISH, "i").find_all(&folded).len();
    if re!(r"[àèéìòù]", "i").test(&text) {
        it += 1;
    }
    match it.cmp(&en) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some("it"),
        std::cmp::Ordering::Less => Some("en"),
    }
}

/// `resolveOutputLanguage(reply, user, "it")`.
pub fn output_language(reply: &str, user: &str) -> &'static str {
    detect_language(reply)
        .or_else(|| detect_language(user))
        .unwrap_or("it")
}

fn hint(id: &str, lang: &str) -> &'static str {
    let en = lang == "en";
    match id {
        "reminder" => {
            if en {
                "Telegram reminders — e.g. \"remind me at 21:30 to …\""
            } else {
                "Promemoria Telegram — es. «avvisami alle 21:30 di …»"
            }
        }
        "list-reminders" => {
            if en {
                "List reminders — \"what reminders do I have?\""
            } else {
                "Elenco promemoria — «quali promemoria ho?»"
            }
        }
        "sheet" => {
            if en {
                "Update Google Sheets (weight/receipts) — send a scale/receipt photo or say \"log 82 kg\""
            } else {
                "Aggiorno Google Sheet (peso/scontrino) — manda foto bilancia/scontrino o «registra 82 kg»"
            }
        }
        "graph" => {
            if en {
                "Chart from a Drive sheet — \"chart net worth over the last few months\""
            } else {
                "Grafico da foglio Drive — «grafico patrimonio ultimi mesi»"
            }
        }
        "image" => {
            if en {
                "ComfyUI image — \"generate an image of …\""
            } else {
                "Immagine ComfyUI — «genera un'immagine di …»"
            }
        }
        "harness" => {
            if en {
                "Code tasks via DeepSeek Harness + Qwen on nezopt — \"fix Hermes …\" / \"check the deploy\""
            } else {
                "Task codice con DeepSeek Harness + Qwen su nezopt — «fix Hermes …» / «verifica il deploy»"
            }
        }
        "poko" => {
            if en {
                "Poko on Oracle — \"have Poko post the news\" / Bot Hospital scout"
            } else {
                "Poko su Oracle — «fai postare news a Poko» / Bot Hospital scout"
            }
        }
        _ => {
            if en {
                "Tell me what to do: reminders, sheets, images, code, or Poko news"
            } else {
                "Dimmi cosa fare: promemoria, foglio, immagine, codice, o Poko news"
            }
        }
    }
}

fn suggestion_ids(user_text: &str, plan_action: &str) -> Vec<&'static str> {
    let text = user_text.to_lowercase();
    let mut out: Vec<&'static str> = Vec::new();
    let mut add = |id: &'static str| {
        if !out.contains(&id) && out.len() < 3 {
            out.push(id);
        }
    };
    if !plan_action.is_empty() && plan_action != "reply" {
        return Vec::new();
    }
    if (looks_like_reminder_request(&text)
        || re!(r"(remind|ricordami|avvisami|promemoria)", "i").test(&text))
        && plan_action != "schedule_reminder"
        && !looks_like_reminder_list(&text)
    {
        add("reminder");
    }
    if looks_like_reminder_list(&text) && plan_action != "list_reminders" {
        add("list-reminders");
    }
    if looks_like_spreadsheet_write(&text) {
        add("sheet");
    }
    if re!(r"(graph|grafico|chart)", "i").test(&text) {
        add("graph");
    }
    if re!(r"(image|photo|immagine|genera)", "i").test(&text) {
        add("image");
    }
    if re!(
        r"(cursor|codex|harness|mimir|commit|deploy|fix|repo|hermes)",
        "i"
    )
    .test(&text)
    {
        add("harness");
    }
    if re!(r"(poko|pokoin|news)", "i").test(&text) {
        add("poko");
    }
    let empty = out.is_empty();
    if empty && looks_like_actionable(&text) {
        out.push("menu");
    }
    out
}

/// `appendCapabilityHints(reply, userText, {action: "reply"})`.
pub fn append_capability_hints(reply: &str, user_text: &str, enabled: bool) -> String {
    let base = reply.trim();
    if base.is_empty() || !enabled {
        return base.to_owned();
    }
    if base.contains("💡 Posso anche:") || base.contains("💡 I can also:") {
        return base.to_owned();
    }
    if looks_like_vulnerable_support(user_text) {
        return base.to_owned();
    }
    let ids = suggestion_ids(user_text, "reply");
    if ids.is_empty() {
        return base.to_owned();
    }
    let lang = if output_language(base, user_text) == "en" {
        "en"
    } else {
        "it"
    };
    let heading = if lang == "en" {
        "💡 I can also:"
    } else {
        "💡 Posso anche:"
    };
    let lines: Vec<String> = ids
        .iter()
        .map(|id| format!("• {}", hint(id, lang)))
        .collect();
    let block = format!("\n\n—\n{heading}\n{}", lines.join("\n"));
    let max = 3900;
    let (b, k) = (
        lucina_store::timeutil::utf16_len(base),
        lucina_store::timeutil::utf16_len(&block),
    );
    if b + k <= max {
        format!("{base}{block}")
    } else {
        format!(
            "{}…{block}",
            base.chars()
                .take(max.saturating_sub(k + 1))
                .collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_hints() {
        assert_eq!(detect_language("Ciao, cosa fai oggi?"), Some("it"));
        assert_eq!(detect_language("What are you doing today?"), Some("en"));
        assert_eq!(detect_language("ok"), None);
        let out = append_capability_hints(
            "Certo, posso aiutarti con questo.",
            "puoi aiutarmi a fare un grafico del patrimonio?",
            true,
        );
        assert!(out.contains("💡 Posso anche:"), "{out}");
        assert!(out.contains("Grafico da foglio Drive"));
        assert_eq!(append_capability_hints("Ok.", "ciao", true), "Ok.");
        assert_eq!(
            append_capability_hints("Ok.", "puoi fare un grafico?", false),
            "Ok."
        );
    }
}
