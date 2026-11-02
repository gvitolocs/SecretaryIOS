//! The tasks bridge's parser and texts (`src/tasks-bridge.js`, Node
//! `6282f40`): "cosa devo fare oggi?", "aggiungi task …", "fatto lo
//! shadowing", "sposta le attività in ritardo a domani", in it/en/da.
//! The HTTP side (secretary-server `/v1/tasks/*`) is in the server's
//! `bridges` module.

use serde_json::Value;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use crate::jsre::{js_trim, Re};
use crate::notes::words_src;
use crate::re;

/// What a tasks turn asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    List,
    Add,
    Done,
    Overdue,
    Delete,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::List => "list",
            Kind::Add => "add",
            Kind::Done => "done",
            Kind::Overdue => "overdue",
            Kind::Delete => "delete",
        }
    }
}

/// `parseTasksIntent` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksIntent {
    pub kind: Kind,
    pub lang: &'static str,
    /// `add`: the quick-add text.
    pub text: Option<String>,
    /// `done`: the words naming the task.
    pub what: Option<String>,
    /// `overdue`: the target day as typed ("domani"), if any.
    pub date: Option<String>,
}

const TASK_WORD: &str = r"task|tasks|attivit[aà]|compit[io]|to-?do|todoist|opgaver?|gøremål";

struct Patterns {
    list: Vec<Re>,
    add: Vec<Re>,
    done: Vec<Re>,
    delete: Vec<Re>,
    overdue: Vec<Re>,
}

fn w(src: &str) -> Re {
    Re::new(&words_src(src), "iu")
}

fn patterns() -> &'static Patterns {
    static P: std::sync::OnceLock<Patterns> = std::sync::OnceLock::new();
    P.get_or_init(|| Patterns {
        list: vec![
            w(r"(?:cosa|cos['’]|che\s+cosa|che)\s+(?:devo|dovrei|mi\s+resta\s+da|resta\s+da)\s+fare"),
            w(&format!(r"(?:quali|che)\s+(?:sono\s+)?(?:i\s+miei\s+|le\s+mie\s+)?(?:{TASK_WORD})")),
            w(&format!(r"(?:i\s+miei|le\s+mie|mostrami\s+(?:i|le)|elenca\s+(?:i|le))\s+(?:{TASK_WORD})")),
            w(r"what\s+(?:do\s+i\s+(?:need|have)\s+to|should\s+i|is\s+left\s+to|have\s+i\s+got\s+to)\s+do"),
            w(&format!(r"(?:my|show\s+(?:me\s+)?my|list\s+my|what\s+are\s+my)\s+(?:{TASK_WORD})")),
            w(r"hvad\s+(?:skal|mangler)\s+jeg\s+(?:at\s+)?(?:nå|gøre|lave)"),
            w(&format!(r"(?:mine|vis\s+(?:mig\s+)?mine)\s+(?:{TASK_WORD})")),
        ],
        add: vec![
            w(r"(?:aggiungi|aggiungere|crea|metti|segna)(?:mi)?\s+(?:un[ao]?\s+|una\s+nuova\s+|un\s+nuovo\s+)?(?:task|attivit[aà]|compito|to-?do)(?:\s+(?:in|a|su)\s+todoist)?\s*[:,-]?\s+(?<text>.+)"),
            w(r"(?:aggiungi|metti)\s+(?:alle?\s+(?:mie\s+)?(?:attivit[aà]|task)|su\s+todoist|in\s+todoist)\s*[:,-]?\s+(?<text>.+)"),
            w(r"(?:add|create|make)\s+(?:a\s+|an\s+|new\s+|a\s+new\s+)?(?:task|to-?do)(?:\s+(?:to|in)\s+todoist)?\s*[:,-]?\s+(?<text>.+)"),
            w(r"(?:add|put)\s+(?:to|on)\s+my\s+(?:tasks|to-?do(?:\s+list)?|todoist)\s*[:,-]?\s+(?<text>.+)"),
            w(r"(?:tilføj|opret|lav)\s+(?:en\s+(?:ny\s+)?)?(?:opgave|to-?do)(?:\s+i\s+todoist)?\s*[:,-]?\s+(?<text>.+)"),
        ],
        done: vec![
            w(r"(?:ho\s+)?(?:fatto|finito|completato|terminato)\s+(?:(?:lo|la|il|l['’]|i|gli|le|un|una|con)\s*)?(?<what>.+)"),
            w(r"(?:segna|metti)\s+(?<what>.+?)\s+come\s+(?:fatt[oa]|completat[oa])"),
            w(r"(?:i\s+)?(?:did|finished|completed|have\s+done)\s+(?:(?:my|the)\s+)?(?<what>.+)"),
            w(r"done\s+with\s+(?:(?:my|the)\s+)?(?<what>.+)"),
            w(r"mark\s+(?<what>.+?)\s+(?:as\s+)?(?:done|complete[d]?)"),
            w(r"(?:jeg\s+har\s+)?(?:lavet|gjort|klaret)\s+(?:min\s+|mit\s+|mine\s+)?(?<what>.+)"),
            w(r"færdig\s+med\s+(?:min\s+|mit\s+|mine\s+)?(?<what>.+)"),
        ],
        delete: vec![
            w(r"(?:cancella|elimina|rimuovi)\s+(?:il\s+|la\s+|l['’])?(?:task|attivit[aà]|compito)"),
            w(r"(?:delete|remove)\s+(?:the\s+|my\s+)?(?:task|to-?do)"),
            w(r"(?:slet|fjern)\s+(?:opgaven|en\s+opgave|opgave)"),
        ],
        overdue: vec![
            w(r"(?:sposta|riprogramma|rimanda)\s+(?:le\s+|i\s+|tutte\s+le\s+|tutti\s+i\s+)?(?:attivit[aà]|task|compiti)\s+(?:in\s+ritardo|scadut[ei])(?:\s+(?:a|per|ad)\s+(?<date>oggi|domani|lunedì|lunedi|la\s+prossima\s+settimana))?"),
            w(r"(?:reschedule|move)\s+(?:my\s+|the\s+|all\s+)?overdue(?:\s+tasks)?(?:\s+to\s+(?<date>today|tomorrow|next\s+week|monday))?"),
            w(r"(?:flyt|udskyd)\s+(?:mine\s+|de\s+)?forfaldne(?:\s+opgaver)?(?:\s+til\s+(?<date>i\s+dag|i\s+morgen|næste\s+uge|mandag))?"),
        ],
    })
}

/// `guessLang`: `"it" | "en" | "da"`.
pub fn guess_lang(text: &str) -> &'static str {
    // Function-word scoring first (`lang::detect`): one stray word ("mine", "lo") must not decide.
    // Owner rule (2026-11-02): the answer is English or Danish, never Italian.
    if let Some(l) = crate::lang::detect(text) {
        return l.reply().code();
    }
    if re!(
        r"[æøå]|\b(hvad|jeg|opgave|opgaver|lavet|gjort|tilføj|mine)\b",
        "i"
    )
    .test(text)
    {
        return "da";
    }
    "en"
}

fn named(re: &Re, t: &str, name: &str) -> Option<Option<String>> {
    let c = re.exec(t)?;
    Some(c.name(name).map(|m| m.as_str().trim().to_owned()))
}

/// `parseTasksIntent`.
pub fn parse_tasks_intent(text: &str) -> Option<TasksIntent> {
    let t0 = js_trim(text);
    let t = js_trim(t0.trim_end_matches(['?', '!', '.']));
    if t.is_empty() || lucina_store::timeutil::utf16_len(t) > 600 {
        return None;
    }
    let lang = guess_lang(t);
    let p = patterns();
    let base = |kind| TasksIntent {
        kind,
        lang,
        text: None,
        what: None,
        date: None,
    };
    if p.delete.iter().any(|re| re.test(t)) {
        return Some(base(Kind::Delete));
    }
    for re in &p.add {
        if let Some(Some(x)) = named(re, t, "text") {
            if !x.is_empty() {
                return Some(TasksIntent {
                    text: Some(x),
                    ..base(Kind::Add)
                });
            }
        }
    }
    for re in &p.overdue {
        if let Some(d) = named(re, t, "date") {
            return Some(TasksIntent {
                date: d.filter(|d| !d.is_empty()),
                ..base(Kind::Overdue)
            });
        }
    }
    if p.list.iter().any(|re| re.test(t)) {
        return Some(base(Kind::List));
    }
    for re in &p.done {
        if let Some(Some(x)) = named(re, t, "what") {
            if !x.is_empty() && x.split_whitespace().count() <= 6 {
                return Some(TasksIntent {
                    what: Some(x),
                    ..base(Kind::Done)
                });
            }
        }
    }
    None
}

/// Owner turns about the day or tasks get the tasks context line
/// (`TASK_TOPIC` in `src/lucina-turn-context.js`).
pub fn is_task_topic(prompt: &str) -> bool {
    re!(r"\b(task|tasks|todo|to-do|todoist|attivit|compit|da fare|devo fare|oggi|giornata|today|my day|to do|opgave|i dag|nå i dag|routine|shadowing|anki|duolingo)", "iu").test(prompt)
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect()
}

const STOP: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "di", "del", "della", "the", "my", "a", "an", "of", "min",
    "minuti", "minutes", "mit", "en", "et", "og", "e", "and", "oggi", "today", "dag",
];

fn walk<'a>(t: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(t);
    for c in t["children"].as_array().into_iter().flatten() {
        walk(c, out);
    }
}

fn groups(view: &Value) -> impl Iterator<Item = &Value> {
    view["list"]["groups"].as_array().into_iter().flatten()
}

fn group_tasks(g: &Value) -> impl Iterator<Item = &Value> {
    g["tasks"].as_array().into_iter().flatten()
}

/// `matchTasks`: open tasks of a state view whose title contains every word
/// of `what` (folded, stop words dropped).
pub fn match_tasks<'a>(view: &'a Value, what: &str) -> Vec<&'a Value> {
    let folded = fold(what);
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|x| x.chars().count() > 1 && !STOP.contains(x))
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut all = Vec::new();
    for g in groups(view) {
        for t in group_tasks(g) {
            walk(t, &mut all);
        }
    }
    let mut seen: Vec<String> = Vec::new();
    all.into_iter()
        .filter(|t| {
            let c = fold(t["content"].as_str().unwrap_or(""));
            let id = js(&t["id"]);
            if seen.contains(&id) || !words.iter().all(|x| c.contains(x)) {
                return false;
            }
            seen.push(id);
            true
        })
        .collect()
}

/// A JSON value as JS template text (`${v}`).
pub fn js(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.to_string(),
        },
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        _ => "undefined".into(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn pick(lang: &str, it: &str, en: &str, da: &str) -> String {
    match lang {
        "it" => it,
        "da" => da,
        _ => en,
    }
    .to_owned()
}

fn due_text(t: &Value, lang: &str) -> String {
    let d = &t["due"];
    if !truthy(d) {
        return String::new();
    }
    match d["rel_days"].as_f64() {
        Some(rel) if rel < 0.0 => {
            let n = js(&serde_json::json!(-rel));
            let n = n.strip_suffix(".0").unwrap_or(&n).to_owned();
            pick(
                lang,
                &format!("in ritardo di {n} g"),
                &format!("{n}d overdue"),
                &format!("{n} d. forsinket"),
            )
        }
        _ if truthy(&d["time"]) => js(&d["time"]),
        _ => String::new(),
    }
}

/// `taskLines`: "- 15 min shadowing (09:00) ↻" for today's answer.
pub fn task_lines(view: &Value, lang: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for g in groups(view) {
        for t in group_tasks(g) {
            let extra = [
                due_text(t, lang),
                if t["due"]["is_recurring"].as_bool() == Some(true) {
                    "↻".into()
                } else {
                    String::new()
                },
                if t["priority"].as_f64() == Some(1.0) {
                    "p1".into()
                } else {
                    String::new()
                },
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
            let content = js(&t["content"]);
            lines.push(if extra.is_empty() {
                format!("- {content}")
            } else {
                format!("- {content} ({extra})")
            });
        }
    }
    lines
}

/// `todayAnswer`: today's tasks, the daily goal, today's events.
pub fn today_answer(view: &Value, lang: &str, events: Option<&[String]>) -> String {
    let open: usize = groups(view)
        .map(|g| g["tasks"].as_array().map_or(0, Vec::len))
        .sum();
    let head = if open == 0 {
        pick(
            lang,
            "Per oggi non hai attività aperte.",
            "You have no open tasks for today.",
            "Du har ingen åbne opgaver i dag.",
        )
    } else {
        pick(
            lang,
            &format!("Oggi hai {open} attività:"),
            &format!(
                "You have {open} {} today:",
                if open == 1 { "task" } else { "tasks" }
            ),
            &format!(
                "Du har {open} {} i dag:",
                if open == 1 { "opgave" } else { "opgaver" }
            ),
        )
    };
    let mut parts = vec![head];
    parts.extend(task_lines(view, lang));
    let goal = &view["goal"];
    if truthy(&goal["daily_goal"]) {
        let (c, g) = (js(&goal["completed_today"]), js(&goal["daily_goal"]));
        parts.push(pick(
            lang,
            &format!("Obiettivo: {c}/{g} fatte."),
            &format!("Goal: {c}/{g} done."),
            &format!("Mål: {c}/{g} klaret."),
        ));
    }
    if let Some(evs) = events.filter(|e| !e.is_empty()) {
        parts.push(pick(
            lang,
            "In calendario:",
            "On your calendar:",
            "I kalenderen:",
        ));
        parts.extend(evs.iter().take(8).map(|e| format!("- {e}")));
    }
    match view["sync"]["status"].as_str() {
        Some("reconnect") => parts.push(pick(
            lang,
            "(Todoist va ricollegato in Secretary → Link.)",
            "(Todoist needs reconnecting in Secretary → Links.)",
            "(Todoist skal forbindes igen i Secretary → Links.)",
        )),
        Some("disconnected") => parts.push(pick(
            lang,
            "(Todoist non è ancora collegato: lo colleghi in Secretary → Link.)",
            "(Todoist is not connected yet: connect it in Secretary → Links.)",
            "(Todoist er ikke forbundet endnu: forbind den i Secretary → Links.)",
        )),
        _ => {}
    }
    parts.join("\n")
}

pub fn unavailable(lang: &str) -> String {
    pick(
        lang,
        "Non riesco a leggere le tue attività adesso.",
        "I can't reach your tasks right now.",
        "Jeg kan ikke nå dine opgaver lige nu.",
    )
}

pub fn delete_refused(lang: &str) -> String {
    pick(
        lang,
        "Le attività le elimini tu nell’app Secretary (Attività) o in Todoist: io non cancello nulla.",
        "You delete tasks yourself in the Secretary app (Tasks) or in Todoist; I never delete them.",
        "Opgaver sletter du selv i Secretary-appen (Opgaver) eller i Todoist; jeg sletter aldrig noget.",
    )
}

/// The reply after a quick add (`parsed` from `/v1/tasks/command`).
pub fn added(lang: &str, parsed: &Value, typed: &str) -> String {
    let content = if truthy(&parsed["content"]) {
        js(&parsed["content"])
    } else {
        typed.to_owned()
    };
    let when = if truthy(&parsed["due_string"]) {
        let d = js(&parsed["due_string"]);
        pick(
            lang,
            &format!(", per {d}"),
            &format!(", due {d}"),
            &format!(", {d}"),
        )
    } else {
        String::new()
    };
    pick(
        lang,
        &format!("Aggiunta: «{content}»{when}. La trovi anche in Todoist tra poco."),
        &format!("Added \"{content}\"{when}. It shows up in Todoist shortly."),
        &format!("Tilføjet: \"{content}\"{when}. Den dukker op i Todoist om lidt."),
    )
}

/// The `reschedule_overdue` date for a typed day (default today).
pub fn overdue_date(date: Option<&str>) -> String {
    let Some(d) = date else {
        return "today".into();
    };
    let l = d.to_lowercase();
    match l.as_str() {
        "oggi" | "i dag" => "today",
        "domani" | "i morgen" => "tomorrow",
        "næste uge" | "la prossima settimana" => "next week",
        "lunedì" | "lunedi" | "mandag" => "monday",
        _ => return l,
    }
    .into()
}

pub fn overdue_moved(lang: &str) -> String {
    pick(
        lang,
        "Fatto: ho spostato le attività in ritardo.",
        "Done: I moved your overdue tasks.",
        "Klaret: jeg har flyttet dine forfaldne opgaver.",
    )
}

pub fn which_one(lang: &str, hits: &[&Value]) -> String {
    let names = |open: &str, close: &str| {
        hits.iter()
            .take(5)
            .map(|t| format!("{open}{}{close}", js(&t["content"])))
            .collect::<Vec<_>>()
            .join(", ")
    };
    pick(
        lang,
        &format!("Quale? {}", names("«", "»")),
        &format!("Which one? {}", names("\"", "\"")),
        &format!("Hvilken? {}", names("\"", "\"")),
    )
}

/// The reply after completing `task`; `goal` from the command's view.
pub fn completed(lang: &str, task: &Value, goal: &Value) -> String {
    let tail = if truthy(&goal["daily_goal"]) {
        let (c, g) = (js(&goal["completed_today"]), js(&goal["daily_goal"]));
        pick(
            lang,
            &format!(" ({c}/{g} oggi)"),
            &format!(" ({c}/{g} today)"),
            &format!(" ({c}/{g} i dag)"),
        )
    } else {
        String::new()
    };
    let again = if task["due"]["is_recurring"].as_bool() == Some(true) {
        pick(
            lang,
            " Torna alla prossima ripetizione.",
            " It comes back at its next repeat.",
            " Den kommer igen ved næste gentagelse.",
        )
    } else {
        String::new()
    };
    let c = js(&task["content"]);
    pick(
        lang,
        &format!("Bravo! Segnato «{c}» come fatto{tail}.{again}"),
        &format!("Nice! Marked \"{c}\" done{tail}.{again}"),
        &format!("Godt! \"{c}\" er markeret som klaret{tail}.{again}"),
    )
}

/// `buildTasksContext` from `/v1/tasks/signals` ("" when not connected).
pub fn context_line(s: &Value) -> String {
    if s["sync_status"].as_str() == Some("disconnected") {
        return "Giuseppe's tasks (Todoist): not connected yet (he connects Todoist in Secretary → Links). Do not invent tasks.".into();
    }
    let name = |t: &Value| {
        let late = t["days_overdue"].as_f64().unwrap_or(0.0);
        if late > 0.0 {
            format!("{} ({}d late)", js(&t["content"]), js(&t["days_overdue"]))
        } else if truthy(&t["time"]) {
            format!("{} {}", js(&t["time"]), js(&t["content"]))
        } else {
            js(&t["content"])
        }
    };
    let list = |k: &str| -> Vec<String> {
        s[k].as_array()
            .into_iter()
            .flatten()
            .take(10)
            .map(name)
            .collect()
    };
    let (today, overdue) = (list("today_tasks"), list("overdue"));
    let mut line = format!(
        "Giuseppe's tasks (Todoist) today {}: {}/{} done",
        js(&s["today"]),
        js(&s["completed_today"]),
        js(&s["daily_goal"])
    );
    if today.is_empty() {
        line.push_str("; nothing left for today");
    } else {
        line.push_str(&format!("; to do: {}", today.join("; ")));
    }
    if !overdue.is_empty() {
        line.push_str(&format!("; overdue: {}", overdue.join("; ")));
    }
    format!("{line}. Use these when he asks what to do; never invent tasks.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind(t: &str) -> Option<&'static str> {
        parse_tasks_intent(t).map(|i| i.kind.as_str())
    }

    #[test]
    fn parses_like_node() {
        let l = parse_tasks_intent("cosa devo fare oggi?").unwrap();
        assert_eq!((l.kind, l.lang), (Kind::List, "en"));
        assert_eq!(kind("what do I need to do today?"), Some("list"));
        let d = parse_tasks_intent("hvad skal jeg nå i dag?").unwrap();
        assert_eq!((d.kind, d.lang), (Kind::List, "da"));
        let a = parse_tasks_intent("aggiungi task chiamare Marco domani alle 9").unwrap();
        assert_eq!(a.kind, Kind::Add);
        assert_eq!(a.text.as_deref(), Some("chiamare Marco domani alle 9"));
        assert_eq!(
            parse_tasks_intent("add a task: buy stamps #Errands")
                .unwrap()
                .text
                .as_deref(),
            Some("buy stamps #Errands")
        );
        assert_eq!(
            parse_tasks_intent("tilføj en opgave ring til mor")
                .unwrap()
                .text
                .as_deref(),
            Some("ring til mor")
        );
        let f = parse_tasks_intent("fatto lo shadowing").unwrap();
        assert_eq!((f.kind, f.what.as_deref()), (Kind::Done, Some("shadowing")));
        assert_eq!(
            parse_tasks_intent("done with anki")
                .unwrap()
                .what
                .as_deref(),
            Some("anki")
        );
        let o = parse_tasks_intent("sposta le attività in ritardo a domani").unwrap();
        assert_eq!((o.kind, o.date.as_deref()), (Kind::Overdue, Some("domani")));
        assert_eq!(
            parse_tasks_intent("reschedule overdue tasks").unwrap().date,
            None
        );
        assert_eq!(kind("elimina il task della palestra"), Some("delete"));
        assert_eq!(kind("ciao Lucina"), None);
        assert_eq!(kind("aggiungi il latte alla lista della spesa"), None);
        // Too many words for a task name: not a completion.
        assert_eq!(
            kind("ho finito di leggere quel libro lunghissimo che mi hai consigliato"),
            None
        );
    }

    #[test]
    fn matching_and_answers() {
        let view = json!({"list": {"groups": [{"tasks": [
            {"id": "1", "content": "15 min Shadowing", "due": {"rel_days": 0, "time": "09:00", "is_recurring": true}},
            {"id": "2", "content": "Anki", "due": {"rel_days": -2}, "priority": 1,
             "children": [{"id": "3", "content": "Anki deck città"}]}
        ]}]}, "goal": {"daily_goal": 5, "completed_today": 1}, "sync": {"status": "ok"}});
        assert_eq!(match_tasks(&view, "lo shadowing").len(), 1);
        assert_eq!(match_tasks(&view, "anki").len(), 2);
        assert_eq!(match_tasks(&view, "citta").len(), 1);
        assert!(match_tasks(&view, "la").is_empty());
        assert_eq!(
            today_answer(&view, "it", Some(&["09:30–10:30 Dentista".into()])),
            "Oggi hai 2 attività:\n- 15 min Shadowing (09:00 ↻)\n- Anki (in ritardo di 2 g p1)\nObiettivo: 1/5 fatte.\nIn calendario:\n- 09:30–10:30 Dentista"
        );
        let empty = json!({"list": {"groups": [{"tasks": []}]}, "goal": {"daily_goal": 5, "completed_today": 0}, "sync": {"status": "disconnected"}});
        assert_eq!(
            today_answer(&empty, "en", None),
            "You have no open tasks for today.\nGoal: 0/5 done.\n(Todoist is not connected yet: connect it in Secretary → Links.)"
        );
        assert_eq!(overdue_date(Some("Domani")), "tomorrow");
        assert_eq!(overdue_date(None), "today");
        assert_eq!(
            added(
                "it",
                &json!({"content": "Chiamare Marco", "due_string": "domani alle 9"}),
                "x"
            ),
            "Aggiunta: «Chiamare Marco», per domani alle 9. La trovi anche in Todoist tra poco."
        );
        assert_eq!(
            completed(
                "en",
                &view["list"]["groups"][0]["tasks"][0],
                &json!({"daily_goal": 5, "completed_today": 2})
            ),
            "Nice! Marked \"15 min Shadowing\" done (2/5 today). It comes back at its next repeat."
        );
        assert!(is_task_topic("cosa devo fare oggi?"));
        assert!(!is_task_topic("ciao"));
        let s = json!({"today": "2026-10-22", "completed_today": 1, "daily_goal": 5, "sync_status": "ok",
            "today_tasks": [{"content": "Anki", "time": "09:00"}], "overdue": [{"content": "Tax", "days_overdue": 3}]});
        assert_eq!(context_line(&s), "Giuseppe's tasks (Todoist) today 2026-10-22: 1/5 done; to do: 09:00 Anki; overdue: Tax (3d late). Use these when he asks what to do; never invent tasks.");
    }
}
