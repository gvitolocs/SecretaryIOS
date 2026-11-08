//! `src/avatar-outfit.js`: "mettiti il vestito da cameriera", "put on your
//! gym clothes", "tag træningstøjet på", "torna in automatico", and hair:
//! "fatti la coda", "put your hair in a bun". Replies in it/en/da.

use std::sync::OnceLock;

use crate::jsre::Re;
use crate::notes::words_src;

const WINDOW_CHARS: usize = 48;

fn words(src: &str) -> Re {
    Re::new(&words_src(src), "iu")
}

struct P {
    verbs: Vec<(&'static str, Re)>,
    da_paa: Re,
    auto: Vec<(&'static str, Re)>,
    not_clothes: Re,
    outfits: Vec<(&'static str, Re)>,
    hair_verbs: Vec<(&'static str, Re)>,
    hairs: Vec<(&'static str, Re)>,
}

fn p() -> &'static P {
    static P_: OnceLock<P> = OnceLock::new();
    P_.get_or_init(|| P {
        verbs: vec![
            ("it", words(r"mettiti|indossa|vestiti\s+da|cambiati(?:\s+in)?|cambia(?:\s+(?:il|l'|di))?\s*(?:vestito|abito|outfit|look)(?:\s+(?:in|con))?")),
            ("en", words(r"put\s+on|change\s+into|change\s+your\s+(?:outfit|clothes|look)(?:\s+to)?|switch\s+(?:in)?to|dress\s+(?:up\s+)?(?:as|in)|wear\s+your|get\s+(?:changed\s+)?into")),
            ("da", words(r"tag|tage|skift\s+til|klæd\s+dig\s+(?:ud\s+)?(?:som|i)")),
        ],
        da_paa: words("på"),
        auto: vec![
            ("it", words(r"torna(?:\s+a\s+(?:cambiarti|vestirti))?\s+(?:in\s+)?automatic[oa]|(?:cambiati|vestiti)\s+(?:in\s+)?automatic[oa]|cambiati\s+da\s+sola")),
            ("en", words(r"(?:go|switch)\s+back\s+to\s+automatic|pick\s+(?:your\s+)?(?:outfits?|clothes)\s+(?:automatically|yourself)|change\s+(?:outfits?\s+)?(?:automatically|by\s+yourself)")),
            ("da", words(r"skift\s+(?:tøj\s+)?automatisk|vælg\s+(?:selv\s+)?tøj(?:et)?\s+(?:automatisk|selv)")),
        ],
        not_clothes: words(r"music|musica|musik|playlist|song|canzone|sang|timer|video|film|movie|podcast|radio|lista|list|liste|spesa|indkøbsliste"),
        outfits: vec![
            ("gym", words(r"palestra|tuta|sportiv[oa]|gym|workout|training|sports?(?:\s*(?:clothes|wear))?|træning(?:støj(?:et)?)?|sportstøj(?:et)?")),
            ("chef", words(r"chef|cuoc[ao]|da\s+cucina|kok(?:ke)?(?:tøj(?:et)?)?")),
            ("maid", words(r"cameriera|maid|stuepige(?:tøj(?:et)?)?|tjenestepige(?:tøj(?:et)?)?")),
            // "Mettiti comoda" alone is "make yourself comfortable".
            ("casual", words(r"casual|qualcosa\s+di\s+comodo|(?:vestit[oaie]|abit[oi]|tuta)\s+comod[oaie]|in\s+comodo|hverdag(?:støj(?:et)?)?|afslappet")),
            ("uniform", words(r"divisa|uniforme?n?|solit[oa]|usual|normale?|default|predefinit[oa]|sædvanlige?")),
        ],
        hair_verbs: vec![
            ("it", words(r"fatti|raccogli(?:ti)?(?:\s+i\s+capelli)?|pettinati(?:\s+con)?|acconciati(?:\s+con)?|sciogli(?:ti)?(?=\s+i\s+capelli)|lega(?:ti)?\s+i\s+capelli|metti(?:ti)?\s+i\s+capelli")),
            ("en", words(r"(?:put|wear|tie|do|style)\s+your\s+hair(?:\s+(?:up|back))?(?:\s+(?:in|into|as|with))?|let\s+your\s+hair\s+down|(?:give\s+yourself|do)\s+an?")),
            ("da", words(r"sæt\s+(?:dit\s+)?hår(?:et)?(?:\s+(?:op\s+)?i)?|lav\s+(?:dig\s+)?(?:en|et|to)?|slå\s+håret\s+ud")),
        ],
        hairs: vec![
            ("natural", words(r"(?:capelli\s+)?sciolt[io]|i\s+capelli$|down|your\s+own\s+hair|naturali|ud|løst(?:\s+hår)?|naturligt")),
            ("twintails", words(r"codini|due\s+code|twin\s*tails|pigtails|rottehaler|to\s+hestehaler")),
            ("ponytail", words(r"coda(?:\s+di\s+cavallo)?|ponytail|hestehale")),
            ("bun", words(r"chignon|crocchia|tuppo|bun|top\s*knot|knold|hårknude")),
            ("braid", words(r"treccia|braid|plait|fletning|fletninger")),
        ],
    })
}

/// A look request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookIntent {
    /// `None` with `hair: None`: back to automatic.
    pub outfit: Option<String>,
    pub hair: Option<String>,
    pub lang: &'static str,
}

fn window(message: &str, start: usize, chars: usize) -> &str {
    let rest = &message[start..];
    match rest.char_indices().nth(chars) {
        Some((i, _)) => &rest[..i],
        None => rest,
    }
}

/// `parseOutfitIntent` (falls through to [`parse_hair_intent`]).
pub fn parse_outfit_intent(text: &str) -> Option<LookIntent> {
    let message = text.trim();
    if message.is_empty() || lucina_store::timeutil::utf16_len(message) > 300 {
        return None;
    }
    let p = p();
    for (lang, re) in &p.auto {
        if re.test(message) {
            return Some(LookIntent {
                outfit: None,
                hair: None,
                lang,
            });
        }
    }
    if p.not_clothes.test(message) {
        return None;
    }
    for (lang, re) in &p.verbs {
        let Some((_, end, matched)) = re.find(message) else {
            continue;
        };
        let mut win = window(message, end, WINDOW_CHARS);
        if *lang == "da" {
            let paa = p.da_paa.find(&message[end..]);
            if paa.is_none()
                && !crate::re!(r"^skift", "i").test(matched)
                && !crate::re!(r"^klæd", "i").test(matched)
            {
                continue;
            }
            if let Some((a, _, _)) = paa {
                win = &message[end..end + a];
            }
        }
        for (id, word) in &p.outfits {
            if word.test(win) {
                return Some(LookIntent {
                    outfit: Some((*id).into()),
                    hair: None,
                    lang,
                });
            }
        }
    }
    parse_hair_intent(message)
}

/// `parseHairIntent`.
pub fn parse_hair_intent(text: &str) -> Option<LookIntent> {
    let message = text.trim();
    let p = p();
    if message.is_empty()
        || lucina_store::timeutil::utf16_len(message) > 300
        || p.not_clothes.test(message)
    {
        return None;
    }
    for (lang, re) in &p.hair_verbs {
        let Some((_, end, matched)) = re.find(message) else {
            continue;
        };
        if crate::re!(r"down|sciogli|slå", "i").test(matched) {
            return Some(LookIntent {
                outfit: None,
                hair: Some("natural".into()),
                lang,
            });
        }
        let win = window(message, end, WINDOW_CHARS);
        for (id, word) in &p.hairs {
            if word.test(win) {
                return Some(LookIntent {
                    outfit: None,
                    hair: Some((*id).into()),
                    lang,
                });
            }
        }
    }
    None
}

fn outfit_name(lang: &str, id: &str) -> String {
    let n = match (lang, id) {
        ("it", "gym") => "la tuta da palestra",
        ("it", "chef") => "il vestito da chef",
        ("it", "maid") => "il vestito da cameriera",
        ("it", "casual") => "qualcosa di comodo",
        ("it", "uniform") => "la mia divisa",
        ("en", "gym") => "my gym clothes",
        ("en", "chef") => "my chef outfit",
        ("en", "maid") => "my maid outfit",
        ("en", "casual") => "something casual",
        ("en", "uniform") => "my uniform",
        ("da", "gym") => "træningstøjet",
        ("da", "chef") => "kokketøjet",
        ("da", "maid") => "stuepigetøjet",
        ("da", "casual") => "noget afslappet",
        ("da", "uniform") => "min uniform",
        _ => id,
    };
    n.to_owned()
}

fn hair_name(lang: &str, id: &str) -> String {
    let n = match (lang, id) {
        ("it", "ponytail") => "una coda",
        ("it", "bun") => "uno chignon",
        ("it", "twintails") => "due codini",
        ("it", "braid") => "una treccia",
        ("en", "ponytail") => "a ponytail",
        ("en", "bun") => "a bun",
        ("en", "twintails") => "twin tails",
        ("en", "braid") => "a braid",
        ("da", "ponytail") => "en hestehale",
        ("da", "bun") => "en knold",
        ("da", "twintails") => "rottehaler",
        ("da", "braid") => "en fletning",
        _ => id,
    };
    n.to_owned()
}

/// The `PUT /v1/avatar` body for a look request.
pub fn patch_for(i: &LookIntent) -> serde_json::Value {
    match (&i.hair, &i.outfit) {
        (Some(h), _) => serde_json::json!({"hair": h}),
        (None, None) => serde_json::json!({"outfit": null, "hair": null, "auto": true}),
        (None, Some(o)) => serde_json::json!({"outfit": o}),
    }
}

/// `changeOutfit`'s reply: `Ok(())` when saved, else the server/transport
/// code. Returns `(reply, saved)`.
pub fn reply_for(i: &LookIntent, result: Result<(), &str>) -> (String, bool) {
    let lang = match i.lang {
        "en" | "da" => i.lang,
        _ => "it",
    };
    let r = |it: &str, en: &str, da: &str| match lang {
        "en" => en.to_owned(),
        "da" => da.to_owned(),
        _ => it.to_owned(),
    };
    match result {
        Err("unknown_outfit") => (
            r(
                "Quel vestito non ce l'ho nel guardaroba.",
                "I don't have that outfit in my wardrobe.",
                "Det tøj har jeg ikke i min garderobe.",
            ),
            false,
        ),
        Err("unknown_hairstyle") => (
            r(
                "Quell'acconciatura non ce l'ho ancora.",
                "I don't have that hairstyle yet.",
                "Den frisure har jeg ikke endnu.",
            ),
            false,
        ),
        Err(_) => (
            r(
                "Non riesco a cambiarmi in questo momento. Riprova tra poco.",
                "I can't change right now. Try again in a moment.",
                "Jeg kan ikke skifte lige nu. Prøv igen om lidt.",
            ),
            false,
        ),
        Ok(()) => {
            let text = match (&i.hair, &i.outfit) {
                (Some(h), _) if h == "natural" => r(
                    "Ecco, capelli sciolti come al solito.",
                    "There, my hair is down again.",
                    "Sådan, håret er løst igen.",
                ),
                (Some(h), _) => {
                    let n = hair_name(lang, h);
                    match lang {
                        "en" => format!("Done: {n}. How do I look?"),
                        "da" => format!("Så er det {n}. Hvordan ser jeg ud?"),
                        _ => format!("Fatto: {n}. Come sto?"),
                    }
                }
                (None, None) => r(
                    "Va bene, torno a cambiarmi da sola in base a dove siamo.",
                    "All right, I'll pick my outfit for wherever we are again.",
                    "Fint, så vælger jeg selv tøj efter, hvor vi er.",
                ),
                (None, Some(o)) => {
                    let n = outfit_name(lang, o);
                    match lang {
                        "en" => format!("One moment… there, I put on {n}!"),
                        "da" => format!("Et øjeblik… sådan, jeg har taget {n} på!"),
                        _ => format!("Un attimo… ecco fatto, ho messo {n}!"),
                    }
                }
            };
            (text, true)
        }
    }
}

/// `[look …]` action result for Honcho.
pub fn action_result(i: &LookIntent, result: Result<(), &str>) -> String {
    match result {
        Ok(()) => match (&i.hair, &i.outfit) {
            (Some(h), _) => format!("[look hair {h}]"),
            (None, Some(o)) => format!("[look {o}]"),
            (None, None) => "[look auto]".into(),
        },
        Err(code) => format!("[look failed: {code}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(t: &str) -> Option<(Option<String>, Option<String>, &'static str)> {
        parse_outfit_intent(t).map(|i| (i.outfit, i.hair, i.lang))
    }

    #[test]
    fn looks() {
        assert_eq!(
            o("Mettiti il vestito da cameriera"),
            Some((Some("maid".into()), None, "it"))
        );
        assert_eq!(
            o("put on your gym clothes"),
            Some((Some("gym".into()), None, "en"))
        );
        assert_eq!(
            o("tag træningstøjet på"),
            Some((Some("gym".into()), None, "da"))
        );
        assert_eq!(o("torna in automatico"), Some((None, None, "it")));
        assert_eq!(
            o("fatti la coda"),
            Some((None, Some("ponytail".into()), "it"))
        );
        assert_eq!(
            o("put your hair in a bun"),
            Some((None, Some("bun".into()), "en"))
        );
        assert_eq!(
            o("let your hair down"),
            Some((None, Some("natural".into()), "en"))
        );
        assert_eq!(o("metti il latte nella lista"), None);
        assert_eq!(o("put on some workout music"), None);
        let maid = parse_outfit_intent("Mettiti il vestito da cameriera").unwrap();
        assert_eq!(
            reply_for(&maid, Ok(())).0,
            "Un attimo… ecco fatto, ho messo il vestito da cameriera!"
        );
        assert_eq!(action_result(&maid, Ok(())), "[look maid]");
        assert_eq!(patch_for(&maid), serde_json::json!({"outfit": "maid"}));
    }
}
