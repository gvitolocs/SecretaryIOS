//! `src/intent-heuristics.js`: IT/EN/DA heuristics shared by tier 0, the
//! planner and the grocery parser. Patterns are verbatim.

use chrono::Utc;
use lucina_store::parser::{parse_relative, ParseOptions};
use lucina_store::timeutil::utf16_len;

use crate::re;

/// Planner settings the heuristics read (Node `config.planner`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// `LUCINA_FAST_CHAT_ENABLED` (default false; on for lucina-nez).
    pub fast_chat: bool,
    /// `LUCINA_FAST_CHAT_MAX_CHARS` (320).
    pub fast_chat_max_chars: usize,
    /// `LUCINA_AGGRESSIVE_TOOL_ROUTING` (default true).
    pub aggressive_tool_routing: bool,
    /// `LUCINA_INTENT_CONFIDENCE_MIN` (0.72).
    pub confidence_min: f64,
    /// `LUCINA_ORACLE_MONITOR_ENABLED` (false on nezopt).
    pub oracle_monitor: bool,
    /// `LUCINA_INTENT_ROUTER_ENABLED` (default true).
    pub intent_router: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fast_chat: false,
            fast_chat_max_chars: 320,
            aggressive_tool_routing: true,
            confidence_min: 0.72,
            oracle_monitor: false,
            intent_router: true,
        }
    }
}

pub const EXPLICIT_RECALL: &str = r"\b(?:ti ricordi|ti sei ricordat[oa]|ricordi quando|ricordi cosa|ricordi (?:il|la|lo|i|gli|le|di|come|perche'|perché)|ricordati quando|cosa avevamo (?:detto|deciso|visto|fatto|programmato|previsto|organizzato|scelto)|come avevamo (?:detto|deciso|fatto)|avevamo (?:parlato|detto|deciso|visto|fatto|previsto|programmato)(?:\s+di)?|(?:di cui|di che) parlavamo|parlavamo (?:di|ieri|della|del)|l'altra volta|la volta scorsa|due settimane fa|tre settimane fa|un mese fa|qualche (?:settimana|giorno) fa|cosa ti avevo detto|mi avevi detto|can you remember|remind me what|quanto manca all'appuntamento|husker du (?:hva|hvad|what|da)|kan du huske|hva vi (?:ble|blev) enige|hva vi (?:ble|blev) enige|hvad vi (?:ble|blev) enige|hvad havde vi planlagt|remember when|do you remember|what did we (?:decide|say|talk about))\b";

pub fn explicit_recall(text: &str) -> bool {
    re!(EXPLICIT_RECALL, "i").test(text)
}

/// `looksLikeTimerRequest`: the relative-duration parser accepts it.
pub fn looks_like_timer(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let o = ParseOptions {
        tz: chrono_tz::Europe::Rome,
        default_hour: 9,
        default_minute: 0,
        now: Utc::now(),
    };
    parse_relative(text, &o).is_ok()
}

pub fn looks_like_image_spreadsheet_log(text: &str) -> bool {
    let text = text.to_lowercase();
    let scale_photo = re!(r"(bilancia|bathroom scale|body scale|digital scale|peso corporale|weight scale|scala pesapersone)", "i").test(&text)
        && re!(r"\d+[,.]?\d*\s*kg|\bkg\b", "i").test(&text);
    let receipt_photo = re!(
        r"(scontrino|ricevuta|receipt|scontrino fiscale|fiscal receipt|ticket di cassa)",
        "i"
    )
    .test(&text)
        && re!(r"(€|eur|totale|total|importo|amount)", "i").test(&text);
    let caption_log = re!(r"(registra|salva|aggiungi|log|segna|append|metti).{0,40}(foglio|sheet|excel|drive|tabella|weight|peso)", "i").test(&text);
    scale_photo || receipt_photo || caption_log
}

pub fn looks_like_poko_status(text: &str) -> bool {
    let text = text.to_lowercase();
    if !re!(r"\bpoko\b").test(&text) {
        return false;
    }
    re!(r"(what happened|what's wrong|whats wrong|cosa.?è successo|che succede|perch[eé]|why|failed|fallito|down|not posting|non posta|non ha postato|didn.?t post|news\?|status|stato|bravo)", "i").test(&text)
}

pub fn looks_like_poko_news_scout_retry(text: &str) -> bool {
    let text = text.to_lowercase();
    if re!(r"(when i mention|if i mention|mentioning|don.?t just|do not just|remember (?:this|the) conversation|learn from|reread|re-read|read (?:the )?(?:last|previous) message|i meant|what i meant|not asking|stop (?:connecting|routing))", "i").test(&text) {
        return false;
    }
    let retry = re!(
        r"(retry|try again|rerun|run again|run|trigger|restart|start|fix|repair|use|call|invoke|activate|enable|check|help|riprova|ripeti|riesegui|avvia|riavvia|attiva|sistema|ripara|usa|controlla|aiuta)",
        "i"
    );
    if re!(r"(bot hospital|bot-hospital|hospital)", "i").test(&text) {
        return retry.test(&text);
    }
    if re!(r"(news scout|scout run|retry scout)", "i").test(&text) {
        return retry.test(&text);
    }
    if re!(r"\bpoko\b").test(&text)
        && re!(r"(post|publish|pubblica|news|canale|channel|scout)", "i").test(&text)
    {
        return re!(r"(help|retry|fix|force|fai|trigger|run|hospital|didn.?t post|non ha postato|non ha pubblicato|manca il post|fallito|failed|missed)", "i").test(&text);
    }
    if re!(
        r"(lucina|hermes).{0,40}(help|aiuta|fai).{0,40}\bpoko\b",
        "i"
    )
    .test(&text)
    {
        return re!(r"(post|publish|pubblica|news)", "i").test(&text);
    }
    false
}

pub fn looks_like_poko_news_scout_retry_followup(user_text: &str, memory: &str) -> bool {
    let text = user_text.trim().to_lowercase();
    if text.is_empty() {
        return false;
    }
    if !re!(r"^(go again|again|retry|try again|rerun|run again|do it again|fai di nuovo|fallo di nuovo|riprova|riprovaci|ancora)$", "i").test(&text) {
        return false;
    }
    re!(r"(poko|bot hospital|bot-hospital|news scout|pokoin news|news_post_|allowlisted_retry|missing discord|missing telegram|message proof|hypemeter)", "i").test(memory)
}

fn looks_like_manual_social_draft(text: &str) -> bool {
    let text = text.to_lowercase();
    if !re!(r"\b(twitter|x)\b").test(&text) {
        return false;
    }
    if re!(
        r"(publish|send|post|pubblica|invia|manda|posta).{0,40}\b(on|to|su)\s+(twitter|x)\b",
        "i"
    )
    .test(&text)
    {
        return false;
    }
    re!(
        r"(prepare|write|draft|copy|paste|manual|incoll|bozza|prepara|scrivi|testo|qua|qui|here)",
        "i"
    )
    .test(&text)
}

pub fn looks_like_spreadsheet_write(text: &str) -> bool {
    let lower = text.to_lowercase();
    if re!(
        r"(sheet|spreadsheet|google sheet|excel|xlsx|drive|foglio)",
        "i"
    )
    .test(&lower)
    {
        return re!(r"(write|insert|append|log|register|record|save|update|correct|store|add|scrivi|inserisci|aggiungi|registra|salva|aggiorna|correggi|segna)", "i").test(&lower);
    }
    if looks_like_weight_log(&lower)
        && re!(
            r"(registra|salva|log|segna|aggiungi|record|save|append|update|aggiorna|peso|weight)\b",
            "i"
        )
        .test(&lower)
        && re!(r"\d+([.,]\d+)?").test(&lower)
    {
        return true;
    }
    looks_like_image_spreadsheet_log(text)
}

pub fn looks_like_graph(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.trim().is_empty() {
        return false;
    }
    re!(r"(graph|grafico|chart|plot|visualiz|diagramma)", "i").test(&text)
        && re!(r"(crea|create|make|genera|generate|draw|mostra|show|plot|fai|build|from|da|del|della|sheet|foglio|patrimonio|weight|peso)", "i").test(&text)
}

pub fn looks_like_code_project(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.trim().is_empty() {
        return false;
    }
    if re!(r"(cursor|codex|harness|deepseek|\brepo\b|repository|codebase|pull request|\bpr\b)", "i").test(&text)
        && re!(r"(fix|patch|repair|commit|deploy|refactor|implement|inspect|debug|diagnos|run|test|push|merge)", "i").test(&text)
    {
        return true;
    }
    if re!(r"\b(fix|patch|repair|debug|deploy|commit|refactor|implementa|sistema)\b", "i").test(&text)
        && re!(r"(hermes|lucina|pokoin|marketplace|bug|error|fail|crash|build|ci|worker|ollama|nezopt)", "i").test(&text)
    {
        return true;
    }
    re!(r"^(commit|deploy|push|merge)\b.{0,80}$", "i").test(text.trim())
}

pub fn looks_like_social_post(text: &str) -> bool {
    let text = text.to_lowercase();
    if looks_like_poko_news_scout_retry(&text) || looks_like_manual_social_draft(&text) {
        return false;
    }
    if re!(r"(add|include|attach|send|post|publish|aggiungi|includi|allega|manda|invia|pubblica).{0,40}(card )?(image|photo|picture|media|immagine|foto)", "i").test(&text) {
        return true;
    }
    if !re!(
        r"(telegram|channel|canale|news|pokoin news|post|publish|pubblica|pubblicare)",
        "i"
    )
    .test(&text)
    {
        return false;
    }
    re!(
        r"(post|publish|send|create|write|announce|pubblica|pubblicare|manda|invia|crea|scrivi)",
        "i"
    )
    .test(&text)
}

pub fn looks_like_model_status(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    if re!(
        r"(revive|reload|wake|load|metti|riavvia|riporta).{0,40}qwen",
        "i"
    )
    .test(text)
    {
        return false;
    }
    re!(r"\b(which|what|che|quale)\b.{0,40}\bmodel", "i").test(text)
        || re!(r"\bmodel\b.{0,24}(using|running|loaded|attivo|usi|usando)", "i").test(text)
        || re!(r"(which llm|che modello|quale modello|what are you running|sei su qwen|sei su gemma)", "i").test(text)
        || re!(r"\b(are you|you on|running on|using|sei su|stai usando)\b.{0,40}\b(qwen|gemma|lfm|llm|gpu|cpu)\b", "i").test(text)
        || re!(r"\b(qwen|gemma|lfm)\b.{0,24}\b(or|o|vs|versus)\b.{0,24}\b(qwen|gemma|lfm|cpu|gpu)\b", "i").test(text)
}

pub fn looks_like_revive_qwen(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let wants_qwen = re!(r"qwen", "i").test(text);
    let wants_gpu = re!(r"\bgpu\b|vram|7900", "i").test(text);
    let revive = re!(
        r"(revive|reload|wake|load|metti|riavvia|riporta|rimetti|restore|bring back|accendi)",
        "i"
    )
    .test(text);
    wants_qwen && (revive || wants_gpu)
}

pub fn looks_like_oracle_monitor(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty()
        || !re!(
            r"(oracle|instance|server|peer\s*1|peer1|\boracle peer)",
            "i"
        )
        .test(&text)
    {
        return false;
    }
    re!(
        r"(status|usage|load|check|monitor|health|cpu|memoria|\bram\b|quanto|how much|resource)",
        "i"
    )
    .test(&text)
}

const HTTP_URL: &str = r#"\bhttps?:\/\/[^\s<>"')\]]+"#;

fn extract_urls(text: &str) -> Vec<&str> {
    re!(HTTP_URL, "i").find_all(text)
}

fn is_direct_media(url: &str) -> bool {
    re!(
        r"\.(?:avif|gif|heic|jpe?g|m4a|mov|mp3|mp4|png|svg|wav|webm|webp)(?:[?#]|$)",
        "i"
    )
    .test(url)
}

pub fn looks_like_web_crawl(text: &str) -> bool {
    let urls = extract_urls(text);
    if looks_like_site_browse_or_opinion(text) {
        return true;
    }
    if urls.is_empty() {
        return false;
    }
    let stripped = re!(HTTP_URL, "i").replace_all(text, " ");
    let non_url = re!(r"\s+").replace_all(&stripped, " ");
    let non_url = non_url.trim();
    if re!(r"(crawl4ai|crawl|scrape|extract|read|summari[sz]e|riassumi|estrai|leggi|controlla|check|what(?:'s| is) on|che c.?è|page|website|site|url|pagina|sito)", "i").test(text) {
        return true;
    }
    if non_url.is_empty()
        || re!(
            r"^(this|that|this page|that page|quest[ao]|questa pagina|quel sito|link)$",
            "i"
        )
        .test(non_url)
    {
        return !urls.iter().all(|u| is_direct_media(u));
    }
    if re!(r"\b(what|how|why|which|who|when|where|explain|tell me|show me|answer|compare|review|analy[sz]e|reason|decide|should|can|could|does|do|is|are|implement|install|setup|teach|learn|use|integrate|update|fix|make|build|dimmi|spiega|mostra|rispondi|confronta|analizza|ragiona|decidi|dovrei|posso|puoi|implementa|installa|configura|insegna|impara|usa|integra|aggiorna|sistema|fai|crea)\b", "i").test(non_url) {
        return !urls.iter().all(|u| is_direct_media(u));
    }
    re!(r"(use|usa).{0,24}(web|url|site|website|pagina)", "i").test(text)
}

pub fn looks_like_site_browse_or_opinion(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let browse = re!(r"(load|open|browse|check|visit|fetch|pull up|vai su|apri|carica|controlla).{0,40}\b(the\s+)?(site|website|page|url|sito|pagina|link)\b", "i").test(text)
        || re!(r"\b(the\s+)?(site|website|page|sito|pagina)\b.{0,40}\b(opinion|review|feedback|thoughts|analy[sz]e|valuta|opinione|recensione|ux|ui)\b", "i").test(text);
    let opinion = re!(r"\b(your\s+)?(opinion|review|thoughts|take|feedback)\b.{0,40}\b(site|website|page|url|sito|pagina|this|that|it)\b", "i").test(text)
        || re!(r"\bgive me your (opinion|review|take|thoughts)\b", "i").test(text)
        || re!(r"\bwhat do you (think|make)\b.{0,40}\b(this|the|that)?\s*(site|website|page|url|sito|pagina|link)\b", "i").test(text)
        || re!(r"\b(think of|pensa a|pensi di)\b.{0,24}\b(this|the|that|sto|questo)?\s*(site|website|page|sito|pagina)\b", "i").test(text);
    browse || opinion
}

pub fn looks_like_study_focus_reminder(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() {
        return false;
    }
    if re!(r"youtube\.com\/watch|youtu\.be\/", "i").test(&text)
        && re!(r"(ricord|remind|mentre\s*studio|while\s*(i\s*)?stud|focus|concentr|these things|queste cose)", "i").test(&text)
    {
        return true;
    }
    if re!(
        r"(ricordami|remind me).{0,50}(mentre\s*studio|while\s*(i\s*)?stud|during\s*(my\s*)?stud)",
        "i"
    )
    .test(&text)
    {
        return true;
    }
    if re!(
        r"(mentre\s*studio|while\s*(i\s*)?stud).{0,50}(ricord|remind|tip|consigli|focus|lucina)",
        "i"
    )
    .test(&text)
    {
        return true;
    }
    re!(r"fai in modo che lucina mi ricord", "i").test(&text)
        && re!(r"(studio|stud)", "i").test(&text)
}

pub fn looks_like_conversational_advice(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() {
        return false;
    }
    if re!(r"(notebook\s*lm|notebooklm|study\s+tips?|how\s+(?:can|do|should)\s+i\s+study|any\s+tips|study\s+better|improve\s+(?:my\s+)?studying)", "i").test(&text) {
        return true;
    }
    if re!(
        r"(you should know|should know them|as i (?:said|told)|like i (?:said|told))",
        "i"
    )
    .test(&text)
    {
        return true;
    }
    if re!(r"^(hi|hey|hello|yo|sup|ciao|salve|gm|good (morning|evening|night)|thanks|thank you|ty|thx|ok|okay|k|cool|nice|lol|lmao|see you|bye|later|so\??)\b", "i").test(text.trim()) {
        return true;
    }
    re!(r"(how are you|how'?s it going|what are you up to|how was your day|tell me something|what should i (cook|eat|watch)|i feel (a bit |so )?(tired|good|great|ok|okay|lonely|sad|happy))", "i").test(&text)
}

pub fn looks_like_vulnerable_support(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() {
        return false;
    }
    if !re!(r"(porn|masturbat|post[-\s]?nut|urge|craving|relapse|compuls|addict|shame|ashamed|guilt|anxiety|panic|depress|lonel|overwhelmed|mentally overloaded|stress(?:ed)?|burn(?:ed)? out|failed an exam|failed exam|fail(?:ed|ing).{0,30}(exam|test)|overthink.{0,30}sex|self[-\s]?control|suicid|kill myself|hurt myself|self[-\s]?harm)", "i").test(&text) {
        return false;
    }
    re!(
        r"\b(i|i'm|im|me|my|myself|mi|io|sono|ho|ero|sto|stavo)\b",
        "i"
    )
    .test(&text)
        || re!(
            r"(need help|aiuto|non riesco|can't stop|cannot stop|mi vergogno|mi sento|feel|felt)",
            "i"
        )
        .test(&text)
}

pub fn looks_like_education_or_study(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() {
        return false;
    }
    if re!(r"(master(?:s)?|phd|thesis|universit|degree|engineering|computer science|studying|atudi|student|corso di laurea|laurea magistrale)", "i").test(&text) {
        return true;
    }
    re!(r"(denmark|danmark|danimarca|aarhus|copenhagen)", "i").test(&text)
        && re!(
            r"(stud|live|master|universit|degree|engineering|atudi)",
            "i"
        )
        .test(&text)
}

pub fn reminder_list_scope(text: &str) -> &'static str {
    if re!(
        r"(today|oggi|per il giorno|for the day|questa giornata|stasera|this evening|tonight)",
        "i"
    )
    .test(&text.to_lowercase())
    {
        "today"
    } else {
        "all"
    }
}

const ACTIONABLE_VERB: &str = r"(book|buy|get|fetch|search|find|check|look up|cerca|trova|compra|prenota|ordina|send|draft|reply|respond|answer|rispondi|replica|schedule|set|create|make|generate|registra|salva|aggiorna|pubblica|remind|avvisami|ricordami|dimmi|mostra|tell me|show me)";

/// Explicitly asks to hand work to an agent; technical vocabulary alone is insufficient.
pub fn looks_like_delegation_request(text: &str) -> bool {
    if re!(r"\b(?:cursor_task|project work|coding agent task)\b", "i").test(text)
        || re!(r"\b(?:fai fare a|delega a|delegate to)\s+\S+", "i").test(text)
    {
        return true;
    }
    // Keep the request and named runner in the same short clause. Connectors also
    // cover forms such as "run the tests with/on Codex" and "give the fix to Cursor".
    re!(
        r"\b(?:fai fare|chiedi a|usa|use|ask|have|get|let|lancia|avvia|start|launch|delega|delegate|manda a|con|tramite|via|with|run\b[^.!?;\n]{0,60}\b(?:with|on)|give\b[^.!?;\n]{0,60}\bto)\s+[^.!?;\n]{0,60}\b(?:cursor|codex|harness|deepseek harness|dsh|claude code|coding agent|qwen|zcode|opencode)\b",
        "i"
    )
    .test(text)
}

pub fn looks_like_harness_or_log_task(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() {
        return false;
    }
    if looks_like_code_project(&text) {
        return true;
    }
    let harness = re!(r"(harness|deepseek|dsh|mimir)", "i").test(&text);
    let logs = re!(r"\b(?:logs?|logging|logfiles?|journalctl|docker logs|lucina log|hermes-lucina|stack trace|error message)\b", "i").test(&text);
    let inspect = re!(r"(check|inspect|read|look at|guarda|controlla|leggi|analizza|analyze|debug|diagnose|what do you notice|cosa noti|cosa vedi)", "i").test(&text);
    let code_runner = re!(r"(?:execute|run|launch|start|esegui|lancia).{0,28}(?:code|command|script|fix|patch|test|check|codice|comando)", "i").test(&text);
    let pc_fix = re!(r"(?:pc|computer|nezopt|nespc|local machine|macchina).{0,40}(?:fix|patch|repair|make fixes|sistema|ripara|correggi)", "i").test(&text)
        || re!(r"(?:fix|patch|repair|make fixes|sistema|ripara|correggi).{0,40}(?:pc|computer|nezopt|nespc|local machine|macchina)", "i").test(&text);
    if harness && re!(r"(try|use|run|prov|fai|usa|prova|with harness)", "i").test(&text) {
        return true;
    }
    if code_runner || pc_fix || (logs && inspect) || (harness && logs) {
        return true;
    }
    re!(r"project.?work|cursor_task|coding agent", "i").test(&text)
}

pub fn looks_like_actionable(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() || utf16_len(&text) < 8 {
        return false;
    }
    if looks_like_conversational_advice(&text) || looks_like_education_or_study(&text) {
        return false;
    }
    if looks_like_reminder_request(&text) || looks_like_spreadsheet_write(&text) {
        return true;
    }
    if looks_like_vulnerable_support(&text) {
        return false;
    }
    if !re!(ACTIONABLE_VERB, "i").test(&text) {
        return looks_like_travel_info(&text)
            && re!(r"(plan|piano|itinerary|itinerario|when|quando|orari)", "i").test(&text);
    }
    looks_like_web_crawl(&text)
        || looks_like_site_browse_or_opinion(&text)
        || looks_like_graph(&text)
        || looks_like_code_project(&text)
        || re!(r"(image|photo|immagine|foto|comfyui|draw|genera)", "i").test(&text)
        || re!(r"(graph|grafico|chart|plot)", "i").test(&text)
        || re!(r"(sheet|spreadsheet|foglio|excel|drive|peso|weight|scontrino|receipt)", "i").test(&text)
        || looks_like_harness_or_log_task(&text)
        || re!(r"(cursor|codex|commit|deploy|refactor|codebase|\brepo\b|implementa|fix|patch|repair|harness|execute code|run code|make fixes)", "i").test(&text)
        || re!(r"(poko|pokoin|news scout|bot hospital)", "i").test(&text)
}

pub fn looks_like_travel_info(text: &str) -> bool {
    let text = text.to_lowercase();
    let hard = re!(
        r"\b(?:vol[io]|flights?|tren[oi]|trains?|trips?|viagg\w*|airports?|aeroporto|bookings?|bigliett[oi]|itinerar(?:y|ies)|itinerari[oi])\b",
        "i"
    );
    if re!(r"(draw|picture|image|photo|immagine|foto|paint|render|comfyui|genera(?:te)?\s+(an?\s+)?(image|picture|foto|immagine))", "i").test(&text)
        && !hard.test(&text)
    {
        return false;
    }
    if re!(
        r"\b(?:vol[io]|flights?|viagg(?:io|i)|trips?|vacanz[ae]|holidays?|summer)\b",
        "i"
    )
    .test(&text)
        || re!(r"(?<!\breal\s)\bestate\b", "i").test(&text)
    {
        return true;
    }
    if re!(r"\b(?:tren[oi]|trains?|orari|orario|partenz[ae]|arriv[oi]|departures?|arrivals?|itinerar(?:y|ies)|itinerari[oi])\b", "i").test(&text) {
        return true;
    }
    if re!(
        r"\b(?:cph|copenhagen|aarhus|fco|mxp|lin|ory|cdg|rome|roma|milan|milano)\b",
        "i"
    )
    .test(&text)
    {
        return re!(r"\b(?:to|from|verso|da|per|near|around|flights?|vol[io]|tren[oi]|trains?|trips?|viagg\w*|airports?|aeroporto|plan|piano|when|quando|orari|orario|search|cerca)\b", "i").test(&text);
    }
    re!(r"\b(?:aeroporto|airports?|boarding|check-?in|bigliett[oi]|tickets?|prenotazion[ei]|bookings?)\b", "i").test(&text)
}

pub fn looks_like_explicit_web_search(text: &str) -> bool {
    re!(
        r"\bcerca(?:mi)?\b.{0,60}(sito|online|prezzo|web|news|internet)",
        "i"
    )
    .test(text)
        || re!(r"\b(search|look up)\b.{0,40}\b(site|web|price|online)", "i").test(text)
}

pub fn looks_like_generate_image(text: &str) -> bool {
    let text = text.to_lowercase();
    if text.is_empty() || looks_like_spreadsheet_write(&text) || looks_like_graph(&text) {
        return false;
    }
    re!(r"(generate|create|make|draw|paint|render|genera|crea|disegna).{0,40}(image|picture|photo|foto|immagine|drawing|illustration)", "i").test(&text)
        || re!(r"\b(draw|paint|render)\b.{0,48}\b(of|a|an|the|un[oa]?)\b", "i").test(&text)
        || re!(r"\b(comfyui|txt2img|image gen)\b", "i").test(&text)
}

pub fn looks_like_reminder_list(text: &str) -> bool {
    let text = text.to_lowercase();
    if looks_like_travel_info(&text) {
        return false;
    }
    if re!(r"(solo|only|just).{0,20}(viagg|voli|tren|travel|trip)", "i").test(&text)
        || re!(r"(non|not).{0,30}(tutti|all).{0,20}promemoria", "i").test(&text)
    {
        return false;
    }
    // "Cancella i promemoria di oggi", "the reminder you sent was wrong": not a list request.
    if re!(r"(?<![\p{L}\p{N}_])(?:cancel|delete|remove|clear|edit|change|move|snooze|postpone|wrong|cancella|elimina|rimuovi|sposta|modifica|rimanda|sbagliat[oaie]|slet|fjern|ændr|ændre|flyt|udsæt|forkert)(?![\p{L}\p{N}_])", "iu").test(&text) {
        return false;
    }
    if re!(r"(plans?\s+for\s+(the\s+)?day|what(?:'s| is)\s+on\s+(today|oggi)|anything\s+scheduled|cosa\s+ho\s+(in\s+)?programma\s+(oggi|today|stasera)|agenda\s+(di\s+)?oggi|promemoria\s+(per\s+)?oggi|reminders?\s+(for\s+)?today|scheduled\s+(for\s+)?today|have\s+i\s+got\s+anything)", "i").test(&text) {
        return true;
    }
    let at_time = re!(r"(alle|at)\s+\d{1,2}[:.]\d{2}", "i");
    if re!(r"\b(list|show|see|display|get)\b.{0,24}\breminders?\b", "i").test(&text)
        || re!(r"\b(my|the)\s+reminders\b", "i").test(&text)
        || re!(r"\breminders?\s*\??\s*$", "i").test(text.trim())
    {
        return !at_time.test(&text);
    }
    // A reminder noun: "ricordami oggi di …" or "my workout schedule" never list.
    if !re!(r"(promemoria|reminder|påmindelse)", "i").test(&text) {
        return false;
    }
    re!(r"(ci sono|quali|lista|elenco|mostra|vedi|hai|oggi|today|futuro|future|futuri|programmati|pending|scheduled|upcoming|quanti|for the day|list|show|hvilke|vis|har jeg|i dag)", "i").test(&text)
        && !at_time.test(&text)
        && !re!(r"^(?:\d{4}-\d{2}-\d{2}|\d{1,2}\s+[a-z]+\s+\d{4})\s*[—–-]", "i").test(text.trim())
}

pub fn looks_like_reminder_request(text: &str) -> bool {
    if looks_like_reminder_list(text) {
        return false;
    }
    // Events go in the calendar ("Mentor session 6 november at 12:30").
    if crate::calendar::looks_like_event(text) || crate::calendar::looks_like_dated_time(text) {
        return false;
    }
    if re!(r"mi ricordi\s+(cosa|quando|perch|come|se|il motivo)", "i").test(text)
        || re!(r"remind me\s+(what|why|when|whether)\b", "i").test(text)
    {
        return false;
    }
    if looks_like_timer(text) {
        return true;
    }
    if re!(r"(remind me|set a reminder|schedule a reminder|ricordami|avvisami|mi ricordi (alle|di|che devo)|mandami un promemoria|promemoria (per|domani|alle))", "i").test(text) {
        return true;
    }
    if re!(r"\b(?:alle|at)\s+\d{1,2}[:.]\d{2}\b", "i").test(text) {
        // Bare times are useful for notes to self, but timestamps in reports
        // must not schedule reminders (including short reports).
        let trimmed = text.trim();
        let starts_with_time = re!(r"^(?:alle|at)\s+\d{1,2}[:.]\d{2}\b", "i").test(trimmed);
        let report = re!(
            r"(?<![\p{L}\p{N}_])(?:applied|fixed|did|done|was|were|happened|finished|started|restarted|deployed|added|returned|since|ho|abbiamo|hai|è stato|sono stat[oi]|fatto|risolto|sistemato|applicato|finito)(?![\p{L}\p{N}_])",
            "i"
        )
        .test(text);
        return starts_with_time || (utf16_len(text) <= 80 && !report);
    }
    re!(
        r"^(?:\d{4}-\d{2}-\d{2}|\d{1,2}\s+[A-Za-z]+\s+\d{4})\s*[—–-]\s*.+",
        "i"
    )
    .test(text.trim())
}

pub fn looks_like_personal_state_query(text: &str) -> bool {
    let text = text.to_lowercase();
    re!(r"(pesavo|peso|pesata|weight|bilancia|misurazion|quanti kg|quanti chili)", "i").test(&text)
        && re!(r"(quanto|qual|what|how much|come era|l'?ultima volta|prima|attuale|ricordi|remember|mi sono pesat)", "i").test(&text)
}

pub fn looks_like_complex(text: &str, s: &Settings) -> bool {
    if !s.fast_chat || utf16_len(text) > s.fast_chat_max_chars {
        return true;
    }
    if looks_like_conversational_advice(text)
        || looks_like_education_or_study(text)
        || looks_like_vulnerable_support(text)
    {
        return false;
    }
    looks_like_actionable(text)
        || looks_like_site_browse_or_opinion(text)
        || looks_like_spreadsheet_write(text)
        || looks_like_image_spreadsheet_log(text)
        || looks_like_graph(text)
        || looks_like_code_project(text)
        || looks_like_social_post(text)
        || looks_like_model_status(text)
        || looks_like_revive_qwen(text)
        || looks_like_oracle_monitor(text)
        || looks_like_reminder_list(text)
        || looks_like_reminder_request(text)
        || looks_like_manual_social_draft(text)
        || re!(r"(generate|create|draw|render|comfyui|immagine|foto|photo|picture|image)", "i").test(text)
        || re!(r"(graph|chart|plot|grafico|visualiz)", "i").test(text)
        || re!(r"(cursor|codex|commit|deploy|refactor|codebase|repository|\brepo\b|implementa|fix bug)", "i").test(text)
        || re!(r"(discord|canale discord|ruolo|role|server admin)", "i").test(text)
        || re!(r"(sheet|spreadsheet|excel|xlsx|drive|foglio)", "i").test(text)
}

fn looks_like_weight_log(text: &str) -> bool {
    re!(r"(weight|peso|bilancia|scale|kg|chil[oi]|kilo)", "i").test(&text.to_lowercase())
}

pub fn default_spreadsheet_name(text: &str) -> &'static str {
    if looks_like_weight_log(text) {
        "weight_tracking_strong_2026-05-15.xlsx"
    } else {
        "patrimony.xlsx"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_and_lists() {
        assert!(looks_like_reminder_request(
            "ricordami alle 18:30 di chiamare la mamma"
        ));
        assert!(looks_like_reminder_request("Timer 20 minutes"));
        assert!(looks_like_reminder_request(
            "Remind me in 2 hours to stretch"
        ));
        assert!(!looks_like_reminder_request("quali promemoria ho oggi?"));
        assert!(looks_like_reminder_list("quali promemoria ho oggi?"));
        assert!(looks_like_reminder_list("list my reminders"));
        assert!(!looks_like_reminder_list("orari dei treni per Aarhus"));
        assert!(!looks_like_reminder_request(
            "mi ricordi cosa avevamo deciso?"
        ));
        assert_eq!(reminder_list_scope("promemoria per oggi"), "today");
    }

    #[test]
    fn travel_complex_and_recall() {
        assert!(looks_like_travel_info("voli per Roma a dicembre"));
        assert!(!looks_like_travel_info("real estate prices"));
        assert!(looks_like_travel_info("quest'estate vado in Italia"));
        assert!(explicit_recall("ti ricordi cosa avevamo deciso?"));
        let s = Settings {
            fast_chat: true,
            ..Settings::default()
        };
        assert!(!looks_like_complex("che bella giornata oggi", &s));
        assert!(looks_like_complex("ricordami alle 18:00 di uscire", &s));
        assert!(looks_like_complex("genera un'immagine di un gatto", &s));
        assert!(looks_like_code_project("fix the lucina build bug"));
        assert!(looks_like_social_post(
            "pubblica sul canale telegram la news"
        ));
    }
}
