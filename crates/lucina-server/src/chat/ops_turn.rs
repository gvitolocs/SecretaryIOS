//! The ops record in chat (crate::ops), on the JSON planner path:
//!
//! - "what happened with X", "why did we decide Y", "when did we deploy Z",
//!   "cosa è successo con X" are answered from the record. A question that
//!   finds nothing and names no known project goes on to the normal turn.
//! - The owner stating a decision about a named project ("we decided …",
//!   "abbiamo deciso …", "decision: …") is recorded, then acknowledged.

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::json;

use super::{Finish, Turn};

pub const ACTION: &str = "ops";
/// Projects named in chat before they have records.
const KNOWN_PROJECTS: &[&str] = &[
    "lucina",
    "lucina-agents",
    "pokoin",
    "pokobot",
    "orchestra",
    "harness",
    "hermes",
    "nezopt",
    "p1",
    "pi",
    "gallery",
    "immich",
    "infisical",
    "secretary-web",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub subject: String,
    pub today: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub title: String,
}

fn rx(cell: &'static OnceLock<Vec<Regex>>, patterns: &[&str]) -> &'static [Regex] {
    cell.get_or_init(|| {
        patterns
            .iter()
            .map(|p| Regex::new(&format!("(?i){p}")).expect("ops turn regex"))
            .collect()
    })
}

fn clean_subject(s: &str) -> String {
    s.trim()
        .trim_end_matches(|c: char| c == '?' || c == '.' || c == '!' || c.is_whitespace())
        .trim()
        .chars()
        .take(200)
        .collect()
}

/// A question about what happened on a project.
pub fn parse_question(text: &str) -> Option<Question> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &R,
        &[
            r"\bwhat(?:'s| has| have)?\s+(?:happened|changed|been going on|been done)\s+(?:with|to|on|in|at|for)\s+(?P<s>.+)",
            r"\bwhy did (?:we|i|you) (?:decide|choose|pick|go with|switch to|drop|move to)\s+(?P<s>.+)",
            r"\bwhen did (?:we|i|you) (?:deploy|ship|release|launch|roll out)\s+(?P<s>.+)",
            r"\b(?:any|which|what) incidents?\s+(?:on|with|in|for|at)\s+(?P<s>.+)",
            r"\b(?:history|record|log) of\s+(?P<s>.+)",
            r"\bcos(?:a|') ?(?:è|e'|e)\s+(?:successo|cambiato)\s+(?:con|a|su|in|al|alla|allo|nel|nella|per)\s+(?P<s>.+)",
            r"\bperch[eé] abbiamo (?:deciso|scelto)\s+(?P<s>.+)",
            r"\bquando abbiamo (?:deployato|rilasciato|messo in produzione|pubblicato)\s+(?P<s>.+)",
            r"\b(?:che|quali) incidenti\s+(?:ci sono stati\s+)?(?:su|con|in|per|a)\s+(?P<s>.+)",
        ],
    );
    let t = text.trim();
    let caps = rs.iter().find_map(|r| r.captures(t))?;
    let subject = clean_subject(&caps["s"]);
    if subject.is_empty() {
        return None;
    }
    static TODAY: OnceLock<Regex> = OnceLock::new();
    let today = TODAY
        .get_or_init(|| Regex::new(r"(?i)\b(?:today|oggi|i dag)\b").expect("today regex"))
        .is_match(t);
    Some(Question { subject, today })
}

/// The owner stating a decision (not a question).
pub fn parse_decision(text: &str) -> Option<Decision> {
    let t = text.trim();
    if t.ends_with('?') {
        return None;
    }
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &R,
        &[
            r"^\s*(?:decision|decisione|deciso|decided)\s*:\s*(?P<d>.+)",
            r"\b(?:we|i)(?:'ve| have)? decided (?:to |that |on )?(?P<d>.+)",
            r"\b(?:abbiamo|ho) deciso (?:di |che )?(?P<d>.+)",
            r"\b(?:record|log|note down|write down|save) (?:this|that|the) decision\b",
            r"\b(?:annota|registra|segna|salva) (?:questa|la) decisione\b",
        ],
    );
    let caps = rs.iter().find_map(|r| r.captures(t))?;
    let d = caps.name("d").map(|m| m.as_str()).unwrap_or(t);
    let first = d
        .split_terminator(['\n', '.', '!'])
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or(d);
    let title = clean_subject(first);
    (!title.is_empty()).then_some(Decision { title })
}

/// The project a message names: the longest known slug (or its spaced form)
/// that appears as a whole word.
pub fn find_project(text: &str, known: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let mut all: Vec<&str> = known.iter().map(String::as_str).collect();
    all.extend_from_slice(KNOWN_PROJECTS);
    all.sort_by_key(|p| std::cmp::Reverse(p.len()));
    all.dedup();
    let word = |hay: &str, needle: &str| -> bool {
        hay.match_indices(needle).any(|(i, _)| {
            let before = hay[..i].chars().next_back();
            let after = hay[i + needle.len()..].chars().next();
            let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '-');
            edge(before) && edge(after)
        })
    };
    all.into_iter()
        .find(|p| word(&lower, p) || (p.contains('-') && word(&lower, &p.replace('-', " "))))
        .map(str::to_owned)
}

fn local(at: &str, tz: Tz) -> String {
    DateTime::parse_from_rfc3339(at)
        .map(|d| d.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| at.to_owned())
}

/// The records as a knowledge block for the reply.
pub fn knowledge(items: &[lucina_ops::SearchItem], tz: Tz) -> String {
    let mut out =
        String::from("Ops records (Lucina's record of the owner's projects), best match first:\n");
    for i in items {
        out.push_str(&format!(
            "- {} [{}] {}: {} (page Lucina/{})\n",
            local(&i.at, tz),
            i.kind.as_str(),
            i.project,
            i.title,
            i.paths.project
        ));
        let body: String = i.body.chars().take(600).collect();
        if !body.trim().is_empty() {
            out.push_str(&format!("  {}\n", body.replace('\n', " ")));
        }
        for (label, v) in [("cause", &i.cause), ("fix", &i.fix)] {
            if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty()) {
                out.push_str(&format!("  {label}: {}\n", v.replace('\n', " ")));
            }
        }
        if !i.follow_ups.is_empty() {
            out.push_str(&format!("  open: {}\n", i.follow_ups.join("; ")));
        }
    }
    out
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str, now: DateTime<Utc>) -> Option<Finish> {
    let st = t.st;
    let ops = st.ops.clone()?;
    let tz = st.cfg.time_zone;
    let known = ops.projects().unwrap_or_default();
    if let Some(q) = parse_question(&t.message) {
        let project = find_project(&q.subject, &known);
        let r = ops
            .search(lucina_ops::SearchQuery {
                q: q.subject.clone(),
                since: q.today.then(|| now.with_timezone(&tz).date_naive()),
                limit: Some(8),
                ..Default::default()
            })
            .await
            .ok()?;
        if r.items.is_empty() {
            project.as_ref()?;
            let reply = format!(
                "I have nothing in my record about {} yet. Sessions and agents report there with ops_record.",
                q.subject
            );
            return Some(Finish::reply(reply, ACTION).with("[ops search: 0]", None));
        }
        let extra = [
            t.extra_system.clone(),
            format!("The owner asked about \"{}\".", q.subject),
            "Answer only from the ops records in the knowledge block: what happened, when (local time) and why; cite the dates and the project page (Lucina/Projects/<project>.md).".into(),
            "If the records do not answer the question, say so. Never invent a record.".into(),
        ]
        .join(" ");
        let reply = st
            .planner
            .generate_reply(
                &t.input(memory, t.platform_label()),
                &extra,
                &knowledge(&r.items, tz),
                "reply",
            )
            .await
            .unwrap_or_default();
        let reply = if reply.trim().is_empty() {
            knowledge(&r.items, tz)
        } else {
            reply
        };
        return Some(
            Finish::reply(reply, ACTION).with(format!("[ops search: {}]", r.items.len()), None),
        );
    }
    // Decisions are the owner's own: never an MCP caller or a guarded channel.
    if t.guarded || t.mcp.is_some() {
        return None;
    }
    let d = parse_decision(&t.message)?;
    let project = find_project(&t.message, &known)?;
    let new: lucina_ops::NewRecord = serde_json::from_value(json!({
        "kind": "decision",
        "project": project,
        "title": d.title,
        "body": t.message.chars().take(4000).collect::<String>(),
    }))
    .ok()?;
    let r = match ops.record(new, "lucina").await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "ops: decision not recorded");
            return None;
        }
    };
    let extra = [
        t.extra_system.clone(),
        format!(
            "The owner just stated a decision about the project {project}. It is now in the ops record (Lucina/{}{}).",
            r.paths.project,
            if r.duplicate { ", already recorded before" } else { "" }
        ),
        "Acknowledge it in one or two short sentences, say it is recorded, and add nothing else unless the message asks something.".into(),
    ]
    .join(" ");
    let reply = st
        .planner
        .generate_reply(&t.input(memory, t.platform_label()), &extra, "", "reply")
        .await
        .unwrap_or_default();
    let reply = if reply.trim().is_empty() {
        format!(
            "Recorded the decision for {project} (Lucina/{}).",
            r.paths.project
        )
    } else {
        reply
    };
    Some(Finish::reply(reply, ACTION).with(format!("[ops record: decision {}]", r.id), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions() {
        let q = parse_question("What happened with Lucina Agents today?").unwrap();
        assert_eq!(q.subject, "Lucina Agents today");
        assert!(q.today);
        assert_eq!(
            parse_question("why did we decide to use Syncthing for the vault?")
                .unwrap()
                .subject,
            "to use Syncthing for the vault"
        );
        assert_eq!(
            parse_question("cosa è successo con il Pi?")
                .unwrap()
                .subject,
            "il Pi"
        );
        assert_eq!(
            parse_question("perché abbiamo scelto JPEG XL")
                .unwrap()
                .subject,
            "JPEG XL"
        );
        assert_eq!(
            parse_question("any incidents on nezopt?").unwrap().subject,
            "nezopt"
        );
        assert!(parse_question("what's the weather tomorrow?").is_none());
        assert!(parse_question("add milk to the list").is_none());
    }

    #[test]
    fn decisions() {
        assert_eq!(
            parse_decision("We decided to keep Syncthing for the Lucina vault. It is simpler.")
                .unwrap()
                .title,
            "keep Syncthing for the Lucina vault"
        );
        assert_eq!(
            parse_decision("abbiamo deciso di passare a JPEG XL per la gallery")
                .unwrap()
                .title,
            "passare a JPEG XL per la gallery"
        );
        assert_eq!(
            parse_decision("Decision: pokoin sync moves to events")
                .unwrap()
                .title,
            "pokoin sync moves to events"
        );
        assert!(parse_decision("did we decide to move pokoin?").is_none());
        assert!(parse_decision("buy milk").is_none());
    }

    #[test]
    fn projects() {
        let known = vec!["pokoin".to_owned()];
        assert_eq!(
            find_project("the Lucina Agents phase 5", &known).as_deref(),
            Some("lucina-agents")
        );
        assert_eq!(
            find_project("pokoin sync", &known).as_deref(),
            Some("pokoin")
        );
        assert_eq!(
            find_project("the lucina-agents line", &known).as_deref(),
            Some("lucina-agents")
        );
        assert_eq!(
            find_project("lucina's chat", &known).as_deref(),
            Some("lucina")
        );
        assert_eq!(find_project("pizza tonight", &known), None);
        assert_eq!(find_project("spirit", &known), None);
    }
}
