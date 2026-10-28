//! The owner's mail on Lucina's own computer: a deterministic route that does
//! not depend on the planner LLM.
//!
//! Lucina has no email API (the Gmail integration was removed on 2026-09-20),
//! but her isolated Cua Linux sandbox (`lucina-workspace`) has one Chromium
//! window with the owner's Outlook (givi@au.dk) and Google signed in. A
//! message that names a mail app or mailbox together with her computer, or
//! that asks her to check, explore, open or read the owner's Outlook or Gmail,
//! goes straight to the `computer_task` action (read-only by default).
//!
//! Speech-to-text slips are corrected by context first ("Explode outlook" is
//! "Explore Outlook"); the task carries the corrected text.

use serde_json::{json, Value};

use crate::re;

/// What the task is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Look only: nothing is sent, replied to, forwarded, deleted, moved or changed.
    ReadOnly,
    /// The owner also asked for something that writes (send, reply, forward,
    /// delete…): the task may only prepare it and must ask for his explicit OK.
    NeedsConfirmation,
}

/// The mail account the request is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Outlook,
    Gmail,
}

impl App {
    pub fn label(self) -> &'static str {
        match self {
            App::Outlook => "Outlook",
            App::Gmail => "Gmail",
        }
    }
}

/// Appended to the corrected request (the Outlook account and the rules of the sandbox).
pub const READ_ONLY_SUFFIX: &str = "Use the Outlook (givi@au.dk) tab already open and signed in in your one Chromium window, through accessibility tools (get_window_state, click, type_text, press_key). Read only: do not send, reply, forward, delete, move, archive or change anything.";

/// The same for a request that names Gmail (Google account) and not Outlook.
pub const READ_ONLY_SUFFIX_GMAIL: &str = "Use the Gmail (Google account) tab already open and signed in in your one Chromium window, through accessibility tools (get_window_state, click, type_text, press_key). Read only: do not send, reply, forward, delete, move, archive or change anything.";

/// For a request that asks for something that writes: no send path exists, so it stays a draft.
pub const CONFIRM_SUFFIX: &str = "Use the Outlook (givi@au.dk) tab already open and signed in in your one Chromium window, through accessibility tools (get_window_state, click, type_text, press_key). Do not send, reply, forward, delete, move, archive or change anything now. If the request asks for one of these, only prepare it: describe exactly what you would do (or leave the text as an unsent draft), tell the owner, and ask for his explicit OK in your answer. Never click Send.";

/// Added when the question is about mail already sent.
pub const SENT_HINT: &str = "For a question about sent mail, open the Sent folder (Sent Items, Posta inviata) first, and answer briefly with what you actually saw (yes or no).";

const REMINDER: &str = r"\b(?:remind me|set a reminder|reminders?|ricordami|promemoria|avvisami|mi ricordi|minde mig|p[åa]mind\w*)\b";

/// Mail apps and mailboxes that mean the owner's mail whatever else is said.
const MAIL_STRONG: &str = r"\b(?:out[\s-]?look|g[\s-]?mail|google mail|inbox|mailbox|posta (?:in arrivo|inviata|elettronica)|sent (?:items|folder|mail)|outlook sent|indbakke)\b";

/// Words that are mail only when her computer is named too ("posta" is also the post office).
const MAIL_WEAK: &str = r"\b(?:e-?mails?|mails?|posta|post\s?kasse)\b";

/// Her own computer.
const COMPUTER: &str = r"(?:\bsandbox\b|\blucina-workspace\b|\bchromium\b|\b(?:your|ur)\s+(?:own\s+)?(?:computer|pc|desktop|browser|machine|workspace|laptop)\b|\b(?:in|on|from|via|using|with|inside)\s+(?:the\s+)?(?:computer|desktop|browser)\s+(?:of\s+)?(?:yours|lucina)\b|\b(?:tuo|tua|tuoi|tue)\s+(?:computer|pc|desktop|browser|sandbox|macchina)\b|\b(?:nel|sul|dal|col)\s+tuo\s+(?:computer|pc|desktop|browser)\b|\b(?:din|dit)\s+(?:egen\s+)?(?:computer|pc|browser|sandbox)\b)";

/// Asking to look at the mailbox.
const LOOK: &str = r"\b(?:check\w*|explor\w*|open\w*|read\w*|look\w*|see|seen|peek\w*|brows\w*|scan\w*|view\w*|show\w*|find\w*|search\w*|list\w*|go to|visit\w*|review\w*|summari[sz]\w*|tell me|replied|answered|received|arrived|controlla\w*|guarda\w*|apri\w*|leggi\w*|legg\w*|esplora\w*|vedi|vedere|vedi\w*|cerca\w*|mostra\w*|dai un.occhiata|ricevut\w*|risposto|arrivat\w*|tjek\w*|[åa]bn\w*|kig\w*|l[æa]s\w*|find\w*)\b";

/// Something that writes to the mailbox.
const WRITE: &str = r"\b(?:send|reply|respond|forward|delete|archive|trash|move|mark as|invia\w*|inviare|manda\w*|spedisci|rispondi|rispondere|inoltra\w*|cancella\w*|elimina\w*|archivia\w*|sposta\w*|svar\w*|slet\w*|videresend\w*)\b";

/// "has Giuseppe already sent an email …", "ho già mandato la mail a …".
const OWNER_SENT: &str = r"(?:\b(?:has|have|had|did|does)\s+(?:giuseppe|givi|he|i|we|the owner)\s+(?:already\s+|just\s+|really\s+|actually\s+|yet\s+)?(?:sent|send|written|emailed|mailed|replied)\b|\bho\s+(?:gi[àa]\s+)?(?:mandato|inviato|spedito|scritto)\s+(?:una\s+|l'|la\s+|quella\s+|quel\s+|un\s+)?(?:e-?mail|mail|posta)\b|\bgiuseppe\s+ha\s+(?:gi[àa]\s+)?(?:mandato|inviato|spedito|scritto)\b)";

/// "my inbox", "la mia posta", "my emails": the owner's own mail.
const MY_MAIL: &str = r"\b(?:my|giuseppe'?s|the owner'?s|la mia|le mie|mia|mie|miei|min|mine)\s+(?:new\s+|latest\s+|unread\s+|recent\s+)?(?:e-?mails?|mails?|posta|inbox|indbakke)\b";

/// The speech-to-text slips seen on the owner's phone, corrected by context.
pub fn fix_stt(text: &str) -> String {
    let mut out = text.to_owned();
    // "Explode / exploit / explorer Outlook" is "Explore Outlook".
    let verb = re!(
        r"\b(?:explode|exploded|exploit|exploits|explorer|exploring|explores|expo|explorar|explora)\b(?=\s+(?:the\s+|my\s+|giuseppe'?s\s+)?(?:out[\s-]?look|g[\s-]?mail|google\s+mail|inbox|mailbox))",
        "i"
    );
    if let Some((a, b, found)) = verb.find(&out) {
        let rep = if found.chars().next().is_some_and(char::is_uppercase) {
            "Explore"
        } else {
            "explore"
        };
        out = format!("{}{}{}", &out[..a], rep, &out[b..]);
    }
    // "out look" is "Outlook" when the sentence is about mail or her computer.
    if re!(
        r"\b(?:sandbox|computer|browser|chromium|inbox|e-?mails?|sent|check|open|explore|your|tuo|tua|nel|sul)\b",
        "i"
    )
    .test(&out)
    {
        out = re!(r"\bout[\s-]?look\b", "i").replace_all(&out, "Outlook");
        out = re!(r"\bg[\s-]?mail\b", "i").replace_all(&out, "Gmail");
    }
    out
}

fn app_of(text: &str) -> App {
    if re!(r"\b(?:g[\s-]?mail|google mail)\b", "i").test(text)
        && !re!(r"\bout[\s-]?look\b", "i").test(text)
    {
        App::Gmail
    } else {
        App::Outlook
    }
}

/// Does this message belong on her computer, and with what limits? `None`
/// when it is not about the owner's mail on her computer. The text may be the
/// raw speech-to-text; it is corrected first.
pub fn classify(text: &str) -> Option<Mode> {
    let t = fix_stt(text);
    let t = t.as_str();
    if re!(REMINDER, "i").test(t) {
        return None;
    }
    let strong = re!(MAIL_STRONG, "i").test(t);
    let weak = re!(MAIL_WEAK, "i").test(t);
    let computer = re!(COMPUTER, "i").test(t);
    let routed = (computer && (strong || weak))
        || (strong && re!(LOOK, "i").test(t))
        || (re!(OWNER_SENT, "i").test(t) && (strong || weak))
        || (re!(MY_MAIL, "i").test(t) && re!(LOOK, "i").test(t));
    if !routed {
        return None;
    }
    Some(if re!(WRITE, "i").test(t) {
        Mode::NeedsConfirmation
    } else {
        Mode::ReadOnly
    })
}

/// True when [`classify`] routes the message to her computer.
pub fn wants_computer(text: &str) -> bool {
    classify(text).is_some()
}

/// The task for the computer-use engine: the corrected request plus the rules.
pub fn task_text(text: &str, mode: Mode) -> String {
    let fixed = fix_stt(text);
    let fixed = fixed.trim();
    let app = app_of(fixed);
    let suffix = match (mode, app) {
        (Mode::NeedsConfirmation, _) => CONFIRM_SUFFIX,
        (Mode::ReadOnly, App::Outlook) => READ_ONLY_SUFFIX,
        (Mode::ReadOnly, App::Gmail) => READ_ONLY_SUFFIX_GMAIL,
    };
    let sent = re!(r"\b(?:sent|send|sending)\b|posta inviata|inviat|mandat|spedit", "i").test(fixed)
        && mode == Mode::ReadOnly;
    if sent {
        format!("{fixed}\n\n{suffix} {SENT_HINT}")
    } else {
        format!("{fixed}\n\n{suffix}")
    }
}

/// The acknowledgement, in the owner's language (`en`, `it`, `da`).
pub fn ack(lang: &str, app: App, mode: Mode) -> String {
    let a = app.label();
    match (lang, mode) {
        ("it", Mode::ReadOnly) => {
            format!("Ok, apro {a} sul mio computer e do un'occhiata — solo lettura.")
        }
        ("it", Mode::NeedsConfirmation) => format!(
            "Ok, apro {a} sul mio computer: preparo al massimo una bozza, non invio nulla senza il tuo ok."
        ),
        ("da", Mode::ReadOnly) => {
            format!("Ok, jeg åbner {a} på min computer og kigger — kun læsning.")
        }
        ("da", Mode::NeedsConfirmation) => format!(
            "Ok, jeg åbner {a} på min computer: jeg laver højst et udkast og sender intet uden dit ok."
        ),
        (_, Mode::ReadOnly) => {
            format!("Ok, I'm opening {a} on my computer and having a look — read-only.")
        }
        (_, Mode::NeedsConfirmation) => format!(
            "Ok, I'm opening {a} on my computer: at most I'll prepare a draft, nothing is sent without your OK."
        ),
    }
}

/// The owner's language (`en`, `it`, `da`): the words of these requests first, then the detectors.
fn lang_of(text: &str) -> &'static str {
    if re!(r"\b(?:controlla\w*|guarda\w*|apri\w*|leggi|esplora\w*|vedi|dai un.occhiata|nel tuo|sul tuo|il tuo|la tua|mia posta|ho gi[àa]|posta inviata|posta in arrivo)\b", "i").test(text) {
        return "it";
    }
    if re!(r"\b(?:tjek\w*|[åa]bn\w*|kig\w*|din computer|min indbakke|indbakke|har giuseppe)\b|[æøå]", "i").test(text) {
        return "da";
    }
    crate::lang::detect(text)
        .map(|l| l.code())
        .unwrap_or_else(|| crate::tasks::guess_lang(text))
}

/// The `computer_task` plan for a message, or `None` when it is not routed.
pub fn plan(text: &str) -> Option<Value> {
    let mode = classify(text)?;
    let fixed = fix_stt(text);
    let lang = lang_of(&fixed);
    Some(json!({
        "action": "computer_task",
        "task": task_text(text, mode),
        "ack": ack(lang, app_of(&fixed), mode),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10:59, ios: speech-to-text for "Explore Outlook in your sandbox".
    const EXPLODE: &str = "Explode outlook in your sandbox";
    /// 07:32, mcp:www.
    const MCP_CHECK: &str = "Quick check only: has Giuseppe already SENT an email this morning (28 Oct 2026) from givi@au.dk to Marianne/Arkaitz about CRAFT self-assessment / \"Ecosystem self assessment-tool for replication\"? If you can see Outlook Sent or notes, say yes/no. If you cannot check, say cannot check. One short answer.";

    #[test]
    fn stt_slips_are_corrected_by_context() {
        assert_eq!(fix_stt(EXPLODE), "Explore Outlook in your sandbox");
        assert_eq!(
            fix_stt("exploit my inbox in your browser"),
            "explore my inbox in your browser"
        );
        assert_eq!(
            fix_stt("Explorer the out look in your sandbox"),
            "Explore the Outlook in your sandbox"
        );
        assert_eq!(fix_stt("check g mail on your computer"), "check Gmail on your computer");
        // Not about mail: left alone.
        assert_eq!(
            fix_stt("the exploit in that game is fun"),
            "the exploit in that game is fun"
        );
        assert_eq!(
            fix_stt("the economic out look is bad"),
            "the economic out look is bad"
        );
    }

    #[test]
    fn the_real_turns_route_to_the_computer() {
        assert_eq!(classify(EXPLODE), Some(Mode::ReadOnly));
        assert_eq!(classify(MCP_CHECK), Some(Mode::ReadOnly));
        for t in [
            "Explore Outlook in your sandbox",
            "check Outlook",
            "explore my outlook",
            "can you see Outlook Sent",
            "has Giuseppe sent an email to Marianne?",
            "did I send an email to Arkaitz today?",
            "ho già mandato la mail a Marianne?",
            "controlla Outlook nel tuo browser",
            "apri la mia posta nel tuo computer",
            "guarda la posta inviata in outlook",
            "open Gmail in your own browser",
            "look at the inbox in your sandbox",
            "kig i min indbakke",
            "tjek Outlook på din computer",
        ] {
            assert!(wants_computer(t), "{t}");
        }
    }

    #[test]
    fn other_messages_stay_where_they_were() {
        for t in [
            "have I sent the package to Marco yet?",
            "ho già mandato il pacco?",
            "Remind me to check Outlook at 5",
            "ricordami di controllare outlook domani",
            "I love Gmail's design",
            "write a good email to my boss",
            "vado alla posta a ritirare il pacco",
            "Laundry first, then the post office parcel, then shopping and home for Teresa",
            "open the sandbox",
            "what time is the meeting",
            "did you send the screenshot of the sandbox",
            "the economic outlook is bad",
        ] {
            assert_eq!(classify(t), None, "{t}");
        }
    }

    #[test]
    fn writes_are_never_read_only() {
        assert_eq!(
            classify("send an email to Marianne from Outlook in your sandbox"),
            Some(Mode::NeedsConfirmation)
        );
        assert_eq!(
            classify("rispondi a Marianne su Outlook"),
            None,
            "no look word and no computer: the planner decides"
        );
        assert_eq!(
            classify("check Outlook and reply to Marianne"),
            Some(Mode::NeedsConfirmation)
        );
        let t = task_text("check Outlook and reply to Marianne", Mode::NeedsConfirmation);
        assert!(t.contains("ask for his explicit OK"));
        assert!(t.contains("Never click Send."));
        assert!(!t.contains("Read only"));
    }

    #[test]
    fn plan_carries_the_corrected_task_and_the_ack() {
        let p = plan(EXPLODE).unwrap();
        assert_eq!(p["action"], "computer_task");
        let task = p["task"].as_str().unwrap();
        assert!(task.starts_with("Explore Outlook in your sandbox\n\n"));
        assert!(task.contains(READ_ONLY_SUFFIX));
        assert!(!task.to_lowercase().contains("explode"));
        assert_eq!(
            p["ack"],
            "Ok, I'm opening Outlook on my computer and having a look — read-only."
        );

        let p = plan(MCP_CHECK).unwrap();
        let task = p["task"].as_str().unwrap();
        assert!(task.starts_with("Quick check only: has Giuseppe already SENT an email"));
        assert!(task.contains(READ_ONLY_SUFFIX));
        assert!(task.contains("open the Sent folder"));

        let p = plan("controlla Outlook nel tuo browser").unwrap();
        assert_eq!(
            p["ack"],
            "Ok, apro Outlook sul mio computer e do un'occhiata — solo lettura."
        );
        let p = plan("tjek Outlook på din computer").unwrap();
        assert!(p["ack"].as_str().unwrap().starts_with("Ok, jeg åbner Outlook"));
        let p = plan("open Gmail in your own browser").unwrap();
        assert!(p["task"].as_str().unwrap().contains(READ_ONLY_SUFFIX_GMAIL));
        assert!(plan("ciao").is_none());
    }
}
