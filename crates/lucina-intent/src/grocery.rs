//! `src/grocery/intent.js`: it/en/da grocery intents (purchases, pantry,
//! shopping list, offers, prices, spending) and the explicit store context.

use chrono::{DateTime, Duration, Utc};
use lucina_grocery::products::{resolve_canonical_product, Product};

use crate::heuristics::{explicit_recall, looks_like_reminder_request, looks_like_travel_info};
use crate::jsre::{group, Re};
use crate::re;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub id: String,
    pub chain_slug: String,
    pub display_name: String,
}

const STORES: &[(&str, &str, &str)] = &[
    ("foetex", "Føtex", "foetex"),
    ("føtex", "Føtex", "foetex"),
    ("netto", "Netto", "netto"),
    ("bilka", "Bilka", "bilka"),
    ("spar", "SPAR", "spar"),
    ("rema 1000", "REMA 1000", "rema"),
    ("rema", "REMA 1000", "rema"),
    ("lidl", "Lidl", "lidl"),
    ("meny", "MENY", "meny"),
    ("365discount", "365discount", "coop-365discount"),
    ("superbrugsen", "SuperBrugsen", "superbrugsen"),
    ("kvickly", "Kvickly", "kvickly"),
];

pub fn store_from_text(text: &str) -> Option<Store> {
    let lower = text.to_lowercase();
    STORES.iter().find_map(|(needle, name, slug)| {
        Re::try_new(&format!(r"\b{needle}\b"), "i")
            .ok()
            .filter(|r| r.test(&lower))
            .map(|_| Store {
                id: format!("chain:{slug}"),
                chain_slug: (*slug).into(),
                display_name: (*name).into(),
            })
    })
}

pub fn clean_product_text(text: &str) -> String {
    let t = re!(r"\b(?:at|from|in|på|fra|hos)\s+(?:føtex|foetex|netto|bilka|spar|rema(?:\s*1000)?|lidl|meny|365discount|superbrugsen|kvickly)\b.*$", "i").replace(text, "").into_owned();
    let t = re!(
        r"\b(?:yesterday|today|last night|i går|igår|ieri|oggi|stasera)\b",
        "i"
    )
    .replace_all(&t, "");
    let t = re!(
        r"^(?:to\s+)?(?:restock|stock\s+up(?:\s+on)?|buy|get|pick\s+up)\s+",
        "i"
    )
    .replace(&t, "")
    .into_owned();
    let t = re!(
        r"^(?:il|lo|la|i|gli|le|un|uno|una|del|dello|della|dei|degli|delle|the|a|an|some)\s+",
        "i"
    )
    .replace(&t, "")
    .into_owned();
    let t = re!(r"[.!?]+$").replace(&t, "").into_owned();
    t.trim().to_owned()
}

/// `extractQuantity`: `(quantity, unit)`.
pub fn extract_quantity(text: &str) -> (Option<f64>, Option<String>) {
    let Some(c) = re!(
        r"\b(\d+(?:[.,]\d+)?)\s*(kg|g|grams?|gram|pcs?|pieces?|pack(?:s)?)\b",
        "i"
    )
    .exec(text) else {
        return (None, None);
    };
    let q = group(&c, 1).replace(',', ".").parse::<f64>().ok();
    let unit = group(&c, 2).to_lowercase();
    let unit = if re!(r"^grams?$").test(&unit) {
        "g".to_owned()
    } else if re!(r"^pieces?$").test(&unit) {
        "pcs".to_owned()
    } else {
        unit
    };
    (q, Some(unit))
}

pub fn purchase_date(text: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    if re!(r"\b(yesterday|i går|igår|ieri)\b", "i").test(&text.to_lowercase()) {
        now - Duration::days(1)
    } else {
        now
    }
}

fn product_after(text: &str, expr: &Re) -> String {
    expr.exec(text)
        .map(|c| clean_product_text(group(&c, 1)))
        .unwrap_or_default()
}

const OFFER_FILLER: &str = "any anything some good great nice interesting cheap best current currently today this week weekly what whats which are is there theres the a an on for at in of me my do you have got see find show tell sale offer offers deal deals discount discounts bargain bargains qualche qualcosa ci ce c è e sono cè buona buone buon buoni bella belle interessante interessanti oggi settimana questa questo della delle degli dei del di da su sul sulla sulle sui sugli per al alla in un una uno le la lo il i gli hai vedi trovi dimmi mostra cosa quali quale che offerta offerte promo promozione promozioni sconto sconti volantino volantini supermercato spesa er der nogen gode godt noget på i dag denne uge ugens hvad har tilbud tilbuddet tilbuddene tilbudsavis tilbudsavisen near nearby close home around here anywhere everywhere store stores shop shops supermarket supermarkets vicino vicini casa qui intorno zona ovunque dappertutto qualsiasi negozio negozi supermercati nær nærheden hjemme overalt alle butik butikker supermarked supermarkeder";

pub fn offer_product_from_text(text: &str) -> String {
    let cleaned = clean_product_text(text).to_lowercase();
    let cleaned = re!(r"[’']").replace_all(&cleaned, " ");
    let filler: Vec<&str> = OFFER_FILLER.split_whitespace().collect();
    re!(r"[^\p{L}\p{N}]+", "u")
        .replace_all(&cleaned, "\u{0}")
        .split('\u{0}')
        .filter(|w| !w.is_empty() && !filler.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroceryParse {
    pub intent: &'static str,
    pub raw_text: String,
    pub store: Option<Store>,
    pub occurred_at: DateTime<Utc>,
    pub raw_product: String,
    pub product: Option<Product>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// "Cosa ho comprato ieri?", "what did we finish?": a question about the
/// past, never a purchase or pantry event.
fn asks_about_past(value: &str) -> bool {
    re!(r"^\s*(?:lucina[,!]?\s+)?(?:cosa|cos['’]|che|quando|dove|perch[eé]|quanto|quanti|quante|chi|what|when|where|why|how|did|which|hvad|hvornår|hvor|hvorfor|hvilke|hvem)(?![\p{L}\p{N}_])", "iu").test(value)
}

const NOT_GROCERY_WORDS: &[&str] = &[
    "treno",
    "aereo",
    "volo",
    "bus",
    "autobus",
    "taxi",
    "metro",
    "traghetto",
    "tram",
    "train",
    "plane",
    "flight",
    "ferry",
    "cab",
    "uber",
    "toget",
    "tog",
    "flyet",
    "fly",
    "bussen",
    "freddo",
    "raffreddore",
    "influenza",
    "febbre",
    "covid",
    "cold",
    "flu",
    "fever",
    "forkølelse",
    "decisione",
    "decision",
    "beslutning",
    "appuntamento",
    "appointment",
    "job",
    "lavoro",
    "work",
    "arbejde",
    "voto",
    "grade",
    "multa",
    "fine",
    "chiamata",
    "call",
    "chance",
    "occasione",
    "time",
    "tempo",
    "tid",
    "sole",
    "sun",
    "pausa",
    "break",
    "vacanz[ae]",
    "vacation",
    "holiday",
    "ferie",
    "casa",
    "house",
    "home",
    "hus",
    "appartamento",
    "apartment",
    "lejlighed",
    "parole",
    "words",
    "film",
    "movie",
    "serie",
    "series",
    "project",
    "progetto",
    "projekt",
    "compiti",
    "homework",
    "esame",
    "exam",
    "eksamen",
    "partita",
    "game",
    "libro",
    "book",
    "bog",
    "episodio",
    "episode",
    "corso",
    "course",
    "riunione",
    "meeting",
    "patente",
    "license",
    "laurea",
    "degree",
    "premio",
    "prize",
    "lezione",
    "lesson",
    "coraggio",
    "courage",
    "paura",
    "fear",
    "sonno",
    "sleep",
    "idea",
    "consiglio",
    "advice",
    "aiuto",
    "help",
    "posto",
    "place",
    "seat",
    "bici",
    "bike",
    "cykel",
    r"too\s+much",
    r"a\s+lot",
    "troppo",
    "niente",
    "nulla",
    "nothing",
    r"di\s+\p{L}+",
];

/// `notGroceryObject`: the object of "ho preso", "I got", "siamo senza",
/// "ci serve" is plainly not a grocery ("il treno", "a new job", "parole").
pub fn not_grocery_object(raw_product: &str) -> bool {
    re!(
        &format!(
            r"^(?:(?:new|nuov[oaie]|another|altr[oaie]|un['’]altra|big|good|bad|real|first|prim[oa])\s+)?(?:{})(?![\p{{L}}\p{{N}}_])",
            NOT_GROCERY_WORDS.join("|")
        ),
        "iu"
    )
    .test(raw_product.trim())
}

/// "Lista dei film", "lista dei promemoria": not the shopping list.
const OTHER_LIST: &str = r"(?!\s+(?:dei|del|della|delle|degli|dell['’]|di)\s+(?!spesa\b))";

/// `parseGroceryIntent`.
pub fn parse_grocery_intent(text: &str) -> Option<GroceryParse> {
    parse_grocery_intent_at(text, Utc::now())
}

pub fn parse_grocery_intent_at(text: &str, now: DateTime<Utc>) -> Option<GroceryParse> {
    let value = text.trim();
    let lower = value.to_lowercase();
    if explicit_recall(value) && !looks_like_reminder_request(value) {
        return None;
    }
    let base = |intent: &'static str| GroceryParse {
        intent,
        raw_text: value.to_owned(),
        store: store_from_text(value),
        occurred_at: purchase_date(value, now),
        raw_product: String::new(),
        product: None,
        quantity: None,
        unit: None,
    };
    let with_product = |intent: &'static str, raw: String, qty: bool| {
        let mut g = base(intent);
        if qty {
            let (q, u) = extract_quantity(&raw);
            g.quantity = q;
            g.unit = u;
        }
        g.product = Some(resolve_canonical_product(&raw));
        g.raw_product = raw;
        g
    };

    let past_question = asks_about_past(value);
    if !past_question && re!(r"\b(i (?:bought|purchased|got|picked up)|jeg købte|ho comprato|abbiamo comprato|ho preso|abbiamo preso)\b", "i").test(value) {
        let raw = product_after(value, re!(r"(?:i\s+(?:bought|purchased|got|picked up)|jeg\s+købte|ho\s+comprato|abbiamo\s+comprato|ho\s+preso|abbiamo\s+preso)\s+(.+)", "i"));
        if !not_grocery_object(&raw) {
            return Some(with_product("grocery.purchase", raw, true));
        }
    }
    if !past_question && re!(r"\b(we finished|we'?re out of|we are out of|finished the|brugte det sidste|løb tør for|(?:ho|abbiamo|è|e')\s+(?:quasi\s+)?finit[oaie]|siamo senza|sono senza|è finito|e' finito|finito(?:\s+il|\s+la)?)\b", "i").test(value) {
        let raw = product_after(value, re!(r"(?:we\s+(?:finished|are\s+out\s+of)|we(?:['’])?re\s+out\s+of|finished\s+the|brugte\s+det\s+sidste|løb\s+tør\s+for|(?:ho|abbiamo|è|e')\s+(?:quasi\s+)?finit[oaie]|siamo\s+senza|sono\s+senza|è\s+finito|e'\s+finito|finito)\s+(?:il\s+|la\s+|lo\s+|i\s+|gli\s+|le\s+)?(.+)", "i"));
        if !not_grocery_object(&raw) {
            return Some(with_product("grocery.finished", raw, true));
        }
    }
    if !past_question
        && re!(
            r"\b(we (?:used|ate|consumed)|i (?:used|ate|consumed)|spiste|brugte)\b",
            "i"
        )
        .test(value)
    {
        let raw = product_after(
            value,
            re!(
                r"(?:(?:we|i)\s+(?:used|ate|consumed)|spiste|brugte)\s+(.+)",
                "i"
            ),
        );
        if !not_grocery_object(&raw) {
            return Some(with_product("grocery.consume", raw, true));
        }
    }
    if re!(r"^(?:(?:show|read|check|open|see|what's on|whats on|what is on)\s+)?(?:my\s+|the\s+)?(?:shopping|grocery)\s+list\b", "i").test(value)
        || re!(r"\b(?:show|read|check|what's on|whats on|what is on)\b.*\b(?:shopping|grocery)\s+list\b", "i").test(value)
        || re!(r"\bhvad står på (?:min )?indkøbsliste\b", "i").test(value)
        || re!(r"^indkøbsliste\s*[?.!]*$", "i").test(value)
        || re!(r"^(?:(?:mostra|dimmi|leggi|controlla|cosa c'è|cosa c\'è|che c'è|che c\'è)\s+(?:su\s+|nella\s+|sulla\s+)?)?(?:la\s+|mia\s+)?lista(?:\s+della\s+spesa)?\s*[?.!]*$", "i").test(value)
        || re!(&format!(r"(?:\b(?:mostra|dimmi)\b|\b(?:cosa|che)\s+c(?:['’]\s*|\s+)(?:è|e['’]))(?![a-zà-ÿ]).*\blista{OTHER_LIST}(?:\s+della\s+spesa)?\b"), "i").test(value)
        // "cosa ho in lista?", "what's on my list", "hvad står på listen".
        || re!(&format!(r"(?:^|[^\p{{L}}])(?:cosa|cos['’]|che)\s*(?:ho|abbiamo)\s+(?:in|nella|sulla|su)\s+(?:mia\s+|la\s+)?lista{OTHER_LIST}(?:\s+della\s+spesa)?(?![\p{{L}}])"), "iu").test(value)
        || re!(r"\bwhat(?:['’]s|\s+is|\s+do\s+(?:i|we)\s+(?:have|need))\s+on\s+(?:my|the|our)\s+(?:shopping\s+|grocery\s+)?list\b", "i").test(value)
        || re!(r"(?:^|[^\p{L}])hvad\s+(?:står|har\s+(?:jeg|vi)|er\s+der)\s+på\s+(?:min\s+|vores\s+)?(?:indkøbsliste(?:n)?|listen)(?![\p{L}])", "iu").test(value)
    {
        return Some(base("grocery.shopping_list.query"));
    }
    if re!(r"\b(add|put)\b.*\b(shopping list|grocery list)\b", "i").test(value)
        || re!(r"\btilføj\b.*\b(indkøbslisten?|listen)\b", "i").test(value)
        || re!(
            r"\b(add|put)\s+.+(?:\s+on(?:to)?\s+(?:the\s+|my\s+)?list)\b",
            "i"
        )
        .test(value)
        || re!(
            r"\b(?:aggiungi|metti)\b.*\b(?:lista(?:\s+della\s+spesa)?|lista)\b",
            "i"
        )
        .test(value)
    {
        let raw = product_after(
            value,
            re!(
                r"(?:add|put|tilføj|aggiungi|metti)\s+(.+?)(?:\s+(?:to|on|onto|på|til|alla|nella|in|su)\s+(?:my\s+|the\s+|la\s+|mia\s+|min\s+)?(?:(?:shopping|grocery)\s*list|indkøbslisten?|listen|lista(?:\s+della\s+spesa)?|list))?$",
                "i"
            ),
        );
        return Some(with_product("grocery.shopping_list.add", raw, false));
    }
    if re!(
        r"\b(remove|take off|strike|togli|rimuovi)\b.*\b(shopping list|grocery list|list|lista)\b",
        "i"
    )
    .test(value)
    {
        let raw = product_after(
            value,
            re!(
                r"(?:remove|take off|strike|togli|rimuovi)\s+(.+?)(?:\s+(?:from|off|dalla|dalla\s+mia|dalla\s+la)?\s*(?:my\s+|the\s+|our\s+|mia\s+)?(?:shopping|grocery)?\s*lista?(?:\s+della\s+spesa)?)?$",
                "i"
            ),
        );
        return Some(with_product("grocery.shopping_list.remove", raw, false));
    }
    if re!(r"\b(?:restock|stock up on|stock up)\b", "i").test(value) {
        let raw = product_after(
            value,
            re!(r"(?:restock|stock up on|stock up(?:\s+on)?)\s+(.+)", "i"),
        );
        if !raw.is_empty() && word_count(&raw) <= 8 {
            return Some(with_product("grocery.shopping_list.add", raw, false));
        }
    }
    if re!(r"\b(?:(?:i|we)\s+need(?:\s+to\s+buy)?|need\s+to\s+buy|(?:i|we)\s+should\s+buy|remember\s+to\s+buy|don'?t\s+forget\s+to\s+buy|we\s+are\s+out\s+of|we'?re\s+missing|vi\s+mangler|jeg\s+skal\s+bruge|skal\s+købe|mi\s+serve|ci\s+serve|mi\s+servono|ci\s+servono|devo\s+comprare|dobbiamo\s+comprare|bisogna\s+comprare|ricordati\s+di\s+comprare|compra|prendi)\b", "i").test(value)
        && !re!(r"\b(help|advice|you|sleep|break|vacation|holiday|aiuto|consiglio)\b", "i").test(value)
    {
        let raw = product_after(value, re!(r"(?:(?:i|we)\s+need(?:\s+to\s+buy)?|need\s+to\s+buy|(?:i|we)\s+should\s+buy|remember\s+to\s+buy|don'?t\s+forget\s+to\s+buy|we\s+are\s+out\s+of|we'?re\s+missing|vi\s+mangler|jeg\s+skal\s+bruge|skal\s+købe|mi\s+serve|ci\s+serve|mi\s+servono|ci\s+servono|devo\s+comprare|dobbiamo\s+comprare|bisogna\s+comprare|ricordati\s+di\s+comprare|compra|prendi)\s+(.+)", "i"));
        if !raw.is_empty() && word_count(&raw) <= 8 && !not_grocery_object(&raw) {
            return Some(with_product("grocery.shopping_list.add", raw, false));
        }
    }
    if let Some(c) = re!(r"^.{0,20}?\b(?:where|dove|hvor)\b(?:\s+(?:can|could|to|posso|possiamo|kan|i|we|jeg|vi|man)\b)*\s*(?:buy|get|find|comprare|compro|prendere|trovare|købe)\b\s*(?:the\s+|a\s+|an\s+|il\s+|la\s+|lo\s+|l'|i\s+|gli\s+|le\s+|en\s+|et\s+|noen\s+)?(.+)$", "i").exec(value) {
        let raw = re!(r"[?.!]+$").replace(group(&c, 1), "").trim().to_owned();
        if !raw.is_empty() {
            return Some(with_product("grocery.offer.search", raw, false));
        }
    }
    if re!(r"(?:^|[^\p{L}])(?:on offer|on sale|offers|deals|deal (?:on|for)|discounts|bargains|tilbud(?:det|dene|savis(?:en)?)?|offert[ae]|in offerta|in sconto|promozion[ei]|sconti|volantin[oi])(?![\p{L}])", "iu").test(value) {
        let raw = offer_product_from_text(value);
        let product = (!raw.is_empty()).then(|| resolve_canonical_product(&raw));
        let plausible = raw.is_empty()
            || product.as_ref().is_some_and(|p| p.confidence >= 0.86)
            || word_count(&raw) <= 3;
        if plausible {
            let mut g = base("grocery.offer.search");
            g.raw_product = raw;
            g.product = product;
            return Some(g);
        }
    }
    if re!(
        r"\b(?:spend|spent|spending|expense|expenses|groceries|udgifter|forbrug)\b",
        "i"
    )
    .test(value)
        || re!(r"\b(?:ho|abbiamo|hanno)\s+speso\b", "i").test(value)
    {
        return Some(base("grocery.expense.query"));
    }
    // "What do I have tomorrow?" is a calendar question: the pantry needs a place.
    if re!(
        r"\b(pantry|pantries|inventory|what do (?:i|we) have (?:at home|in (?:the |my |our )?(?:pantry|fridge|freezer|kitchen|cupboard)))\b",
        "i"
    )
    .test(value)
        || re!(r"(?:^|[^\p{L}])(?:cosa|cos['’]|che)\s*(?:c['’]?\s*è|c\s+è|ho|abbiamo)\s+in\s+(?:frigo(?:rifero)?|dispensa|freezer|congelatore)(?![\p{L}])", "iu").test(value)
        || re!(r"(?:^|[^\p{L}])hvad\s+(?:har\s+(?:vi|jeg)|er\s+der)\s+i\s+(?:køleskabet|fryseren|skabet)(?![\p{L}])", "iu").test(value)
    {
        return Some(base("grocery.pantries.query"));
    }
    // "Costa" is also a coast: only "quanto costa", "costa il …".
    let price_words = re!(r"\b(price|cost|costs|prices|prisen|koster|prezzo)\b", "i").test(value)
        || re!(
            r"\b(?:quanto|che)\s+cost(?:a|ano)\b|\bcost(?:a|ano)\s+(?:il|la|lo|i|gli|le|un|una|uno)\b",
            "i"
        )
        .test(value)
        || re!(r"\bhow\s+much\s+(?:is|are|for)\b", "i").test(value);
    if price_words && !looks_like_travel_info(value) {
        let extracted = [
            re!(r"(?:price|cost|costs)\s+(?:of|for)\s+(.+)", "i"),
            re!(
                r"quanto\s+(?:costa|costano)\s+(?:il\s+|la\s+|lo\s+|l'|i\s+|gli\s+|le\s+)?(.+)",
                "i"
            ),
            re!(
                r"(?:prezzo|costo)\s+(?:del|della|di)\s+(?:il\s+|la\s+|lo\s+)?(.+)",
                "i"
            ),
            re!(r"how\s+much\s+(?:is|are|for)\s+(?:the\s+|a\s+)?(.+)", "i"),
        ]
        .iter()
        .find_map(|r| {
            r.exec(value)
                .map(|c| group(&c, 1).to_owned())
                .filter(|s| !s.is_empty())
        });
        let raw = extracted
            .unwrap_or_else(|| value.to_owned())
            .trim()
            .to_owned();
        // A percentage or a sum is not a grocery price.
        if !re!(r"\d\s*%|^[\d\s.,+\-*/×x()=?]+$").test(&raw) {
            return Some(with_product("grocery.price.query", raw, false));
        }
    }
    // Analytics questions need a grocery word ("the most beautiful city" is chat).
    if re!(r"\b(average|analytics|most|savings|increased)\b", "i").test(&lower)
        && (re!(r"(?<![\p{L}\p{N}_])(?:grocer(?:y|ies)|shopping|supermarkets?|spesa|supermercat[oi]|dagligvarer|indkøb\p{L}*|food|cibo|buy|bought|purchases?|compr\p{L}*|købt|køber|spend|spent|items?|products?|prodott[oi]|varer)(?![\p{L}\p{N}_])", "iu").test(value)
            || store_from_text(value).is_some())
    {
        return Some(base("grocery.analytics.query"));
    }
    None
}

pub const STORE_AUTO: f64 = 0.9;
pub const STORE_ASK: f64 = 0.6;

#[derive(Debug, Clone, PartialEq)]
pub struct StoreResolution {
    pub store: Option<Store>,
    pub confidence: f64,
    pub source: String,
    pub candidates: Vec<Store>,
}

/// `resolveStoreContext`. `persisted`: `(store, confidence)` from the DB.
pub fn resolve_store_context(
    explicit: Option<&Store>,
    recent_context: &str,
    persisted: Option<(Store, f64)>,
) -> StoreResolution {
    if let Some(s) = explicit {
        return StoreResolution {
            store: Some(s.clone()),
            confidence: 1.0,
            source: "message".into(),
            candidates: vec![s.clone()],
        };
    }
    let mut candidates: Vec<(Store, f64)> = Vec::new();
    for line in recent_context.split('\n').rev() {
        let Some(store) = store_from_text(line) else {
            continue;
        };
        if candidates.iter().any(|(s, _)| s.id == store.id) {
            continue;
        }
        let confidence =
            if re!(r"(?:going to|at|i['’]?m in|i am in|på vej til|er i)", "i").test(line) {
                0.94
            } else {
                0.82
            };
        candidates.push((store, confidence));
    }
    if candidates.len() > 1 {
        let (top, conf) = candidates[0].clone();
        return StoreResolution {
            store: Some(top),
            confidence: conf.min(0.72),
            source: "recent_chat_ambiguous".into(),
            candidates: candidates.into_iter().map(|(s, _)| s).collect(),
        };
    }
    if let Some((store, conf)) = candidates.pop() {
        return StoreResolution {
            store: Some(store.clone()),
            confidence: conf,
            source: "recent_chat".into(),
            candidates: vec![store],
        };
    }
    if let Some((store, conf)) = persisted {
        return StoreResolution {
            store: Some(store.clone()),
            confidence: conf,
            source: "stored_context".into(),
            candidates: vec![store],
        };
    }
    StoreResolution {
        store: None,
        confidence: 0.0,
        source: "none".into(),
        candidates: Vec::new(),
    }
}

/// `decideStoreAction`: `None` = auto, else the question to ask.
pub fn decide_store_action(r: &StoreResolution) -> Option<String> {
    let ambiguous = r.candidates.len() > 1 || r.source == "recent_chat_ambiguous";
    match &r.store {
        None => Some("Which supermarket was that from?".into()),
        Some(s) if r.confidence < STORE_ASK => Some(format!("Was this from {}?", s.display_name)),
        Some(_) if r.confidence < STORE_AUTO && ambiguous => {
            let names: Vec<&str> = r
                .candidates
                .iter()
                .take(2)
                .map(|c| c.display_name.as_str())
                .collect();
            Some(format!("Was this from {}?", names.join(" or ")))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(t: &str) -> Option<&'static str> {
        parse_grocery_intent(t).map(|g| g.intent)
    }

    #[test]
    fn it_en_da_intents() {
        let g = parse_grocery_intent("aggiungi il latte alla lista della spesa").unwrap();
        assert_eq!(g.intent, "grocery.shopping_list.add");
        assert_eq!(g.raw_product, "latte");
        assert_eq!(g.product.unwrap().id, "milk");
        assert_eq!(
            parse_grocery_intent("add eggs to my shopping list")
                .unwrap()
                .raw_product,
            "eggs"
        );
        // Node 15d9629: "indkøbslisten" and "til" parse too.
        assert_eq!(
            intent("tilføj mælk til indkøbslisten"),
            Some("grocery.shopping_list.add")
        );
        assert_eq!(
            parse_grocery_intent("tilføj mælk til indkøbslisten")
                .unwrap()
                .raw_product,
            "mælk"
        );
        assert_eq!(
            parse_grocery_intent("add milk to my shopping list")
                .unwrap()
                .raw_product,
            "milk"
        );
        assert_eq!(
            intent("tilføj mælk til listen"),
            Some("grocery.shopping_list.add")
        );
        assert_eq!(
            intent("cosa c'è nella lista della spesa?"),
            Some("grocery.shopping_list.query")
        );
        assert_eq!(
            intent("cosa c è nella lista"),
            Some("grocery.shopping_list.query")
        );
        assert_eq!(
            intent("hvad står på min indkøbsliste"),
            Some("grocery.shopping_list.query")
        );
        assert_eq!(
            intent("what's on my shopping list?"),
            Some("grocery.shopping_list.query")
        );
        let r = parse_grocery_intent("togli il pane dalla lista").unwrap();
        assert_eq!(
            (r.intent, r.raw_product.as_str()),
            ("grocery.shopping_list.remove", "pane")
        );
        let p = parse_grocery_intent("ho comprato 2 kg di riso da Netto").unwrap();
        assert_eq!(p.intent, "grocery.purchase");
        assert_eq!(p.store.unwrap().display_name, "Netto");
        assert_eq!((p.quantity, p.unit.as_deref()), (Some(2.0), Some("kg")));
        assert_eq!(intent("abbiamo finito il latte"), Some("grocery.finished"));
        assert_eq!(
            intent("mi servono le uova"),
            Some("grocery.shopping_list.add")
        );
        assert_eq!(
            intent("qualche offerta interessante?"),
            Some("grocery.offer.search")
        );
        assert_eq!(
            intent("quanto ho speso questo mese?"),
            Some("grocery.expense.query")
        );
        assert_eq!(intent("ti ricordi quando abbiamo preso il pane?"), None);
        assert_eq!(intent("ciao come stai"), None);
        assert_eq!(intent("I need help with my essay"), None);
    }

    #[test]
    fn store_context() {
        let netto = store_from_text("sono da Netto").unwrap();
        let r = resolve_store_context(None, "user: I'm going to Føtex\nuser: then Netto", None);
        assert_eq!(r.source, "recent_chat_ambiguous");
        assert_eq!(
            decide_store_action(&r).unwrap(),
            "Was this from Netto or Føtex?"
        );
        let r = resolve_store_context(Some(&netto), "", None);
        assert!(decide_store_action(&r).is_none());
        assert_eq!(
            decide_store_action(&resolve_store_context(None, "", None)).unwrap(),
            "Which supermarket was that from?"
        );
    }
}
