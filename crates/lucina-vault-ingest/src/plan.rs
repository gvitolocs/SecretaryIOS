//! The model's answer, checked: what notes an item becomes.
//!
//! The model proposes; this module decides what may reach the vault. Every
//! text is redacted and stripped of markers and wikilinks, a claim title must
//! be one sentence that makes a valid file name, `files` must be among the
//! item's candidate files (no invented paths) and links must name existing
//! claims or claims of the same answer (no dangling links).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::queue::{Item, SourceKind};
use crate::redact::{has_secret, redact};

pub const MAX_CLAIMS: usize = 3;
const MAX_TITLE: usize = 150;
const MIN_TITLE: usize = 8;
const MAX_BODY: usize = 1500;
const MAX_SUMMARY: usize = 1500;
const MAX_UPDATE: usize = 500;
const MAX_FILES: usize = 12;
const MAX_LINKS: usize = 6;
/// Existing claim titles shown to the model.
pub const PROMPT_TITLES: usize = 40;
/// Item text shown to the model.
const PROMPT_TEXT: usize = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub title: String,
    pub body: String,
    /// What is new, when the title names an existing claim.
    #[serde(default)]
    pub update: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub connects_to: Vec<String>,
    #[serde(default)]
    pub enables: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub skip: bool,
    #[serde(default)]
    pub skip_reason: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub claims: Vec<Claim>,
}

/// Characters Obsidian refuses in a note name, plus what would break a link.
const FORBIDDEN: &[char] = &[
    '*', '"', '\\', '/', '<', '>', ':', '|', '?', '#', '^', '[', ']', '`',
];

/// A title that is a valid note name: one line, no forbidden characters, no
/// leading dot or trailing dot/space, at most 150 characters.
pub fn clean_title(s: &str) -> String {
    let one: String = s
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut t = one.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > MAX_TITLE {
        t = t.chars().take(MAX_TITLE).collect::<String>();
        if let Some(i) = t.rfind(' ') {
            t.truncate(i);
        }
    }
    t.trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim()
        .to_owned()
}

/// Lower-case ASCII-folded words: what "the same title" means.
pub fn normalize(s: &str) -> String {
    let folded: String = s
        .to_lowercase()
        .chars()
        .map(lucina_ops::render::fold_char)
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Prose for a note: redacted, no HTML comments (our markers), no wikilink
/// brackets (links are ours to make), clipped.
fn prose(s: &str, max: usize) -> String {
    let s = redact(s)
        .replace("<!--", "")
        .replace("-->", "")
        .replace("[[", "")
        .replace("]]", "")
        .replace("{{", "{ {");
    let s = s
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    crate::clip(&s, max)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The JSON object in a model answer: thinking blocks and code fences off.
pub fn answer_json(text: &str) -> Result<Value, String> {
    let mut t = text.to_owned();
    while let (Some(a), Some(b)) = (t.find("<think>"), t.find("</think>")) {
        if b < a {
            break;
        }
        t.replace_range(a..b + "</think>".len(), "");
    }
    let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) else {
        return Err("the answer holds no JSON object".into());
    };
    if b < a {
        return Err("the answer holds no JSON object".into());
    }
    serde_json::from_str(&t[a..=b]).map_err(|e| format!("the answer is not valid JSON: {e}"))
}

impl Plan {
    /// Check a model answer against the item and the vault's existing claim
    /// titles (`existing`: file stems of `Permanent/`).
    pub fn from_answer(text: &str, item: &Item, existing: &[String]) -> Result<Plan, String> {
        let v = answer_json(text)?;
        if !v.is_object() {
            return Err("the answer is not a JSON object".into());
        }
        let skip = v.get("skip").and_then(Value::as_bool).unwrap_or(false);
        let summary_src = v
            .get("event")
            .map(|e| str_of(e, "summary"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| str_of(&v, "summary"));
        let mut plan = Plan {
            skip,
            skip_reason: prose(str_of(&v, "skip_reason"), 300),
            summary: prose(summary_src, MAX_SUMMARY),
            claims: Vec::new(),
        };
        if skip {
            if plan.skip_reason.is_empty() {
                plan.skip_reason = "not meaningful development knowledge".into();
            }
            return Ok(plan);
        }
        let mut seen = std::collections::HashSet::new();
        let raw: Vec<&Value> = v
            .get("claims")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        for c in raw {
            if plan.claims.len() >= MAX_CLAIMS {
                break;
            }
            // A title that needed redaction is no claim worth a file name.
            let raw = str_of(c, "title");
            if redact(raw) != raw || has_secret(raw) {
                continue;
            }
            let title = clean_title(raw);
            if title.chars().count() < MIN_TITLE {
                continue;
            }
            if !seen.insert(normalize(&title)) {
                continue;
            }
            let body = prose(str_of(c, "body"), MAX_BODY);
            if body.is_empty() {
                continue;
            }
            let mut files: Vec<String> = strings(c.get("files"))
                .into_iter()
                .map(|f| f.trim().to_owned())
                .filter(|f| item.files.contains(f))
                .collect();
            files.dedup();
            files.truncate(MAX_FILES);
            plan.claims.push(Claim {
                title,
                body,
                update: prose(str_of(c, "update"), MAX_UPDATE),
                files,
                connects_to: strings(c.get("connects_to")),
                enables: strings(c.get("enables")),
            });
        }
        // Links: only to an existing claim or a claim of this answer.
        let mut known: HashMap<String, String> =
            existing.iter().map(|t| (normalize(t), t.clone())).collect();
        for c in &plan.claims {
            known
                .entry(normalize(&c.title))
                .or_insert_with(|| c.title.clone());
        }
        // A reused title takes the existing note's exact name.
        for c in &mut plan.claims {
            if let Some(t) = known.get(&normalize(&c.title)) {
                c.title = t.clone();
            }
        }
        for c in &mut plan.claims {
            let own = normalize(&c.title);
            let pick = |links: &[String]| -> Vec<String> {
                let mut out: Vec<String> = Vec::new();
                for l in links {
                    let n = normalize(l);
                    if n == own {
                        continue;
                    }
                    if let Some(t) = known.get(&n) {
                        if !out.contains(t) {
                            out.push(t.clone());
                        }
                    }
                }
                out.truncate(MAX_LINKS);
                out
            };
            c.connects_to = pick(&c.connects_to);
            c.enables = pick(&c.enables);
        }
        if plan.summary.is_empty() && plan.claims.is_empty() {
            return Err("the answer has neither a summary nor a claim".into());
        }
        if plan.summary.is_empty() {
            plan.summary = prose(&item.title, MAX_SUMMARY);
        }
        Ok(plan)
    }
}

/// The existing claim titles most related to the item (shared words).
pub fn related_titles(existing: &[String], item: &Item, n: usize) -> Vec<String> {
    let words: std::collections::HashSet<String> = normalize(&format!(
        "{} {} {}",
        item.project,
        item.title,
        crate::clip(&item.text, 2000)
    ))
    .split(' ')
    .filter(|w| w.len() > 3)
    .map(str::to_owned)
    .collect();
    let mut scored: Vec<(usize, &String)> = existing
        .iter()
        .map(|t| {
            let s = normalize(t)
                .split(' ')
                .filter(|w| words.contains(*w))
                .count();
            (s, t)
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(n).map(|(_, t)| t.clone()).collect()
}

/// The user message: the item, its candidate files and related claims.
pub fn prompt(item: &Item, related: &[String]) -> String {
    let what = match item.source {
        SourceKind::Ops => "an ops record",
        SourceKind::Git => "a git commit on a Lucina repo",
        SourceKind::Session => "a Claude session report",
    };
    let mut s = format!(
        "Item: {what} · project {} · {}\nTitle: {}\n\n{}\n\n",
        item.project,
        item.at.format("%Y-%m-%d %H:%M %:z"),
        item.title,
        crate::clip(&item.text, PROMPT_TEXT)
    );
    s.push_str("Candidate files (the only paths you may name):\n");
    if item.files.is_empty() {
        s.push_str("- (none)\n");
    }
    for f in &item.files {
        s.push_str(&format!("- {f}\n"));
    }
    s.push_str("\nExisting claims (reuse an exact title to update that claim):\n");
    if related.is_empty() {
        s.push_str("- (none yet)\n");
    }
    for t in related {
        s.push_str(&format!("- {t}\n"));
    }
    s.push_str("\nAnswer with the JSON object only.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Backlink;

    fn item() -> Item {
        Item {
            key: "git:lucina-rs:abc".into(),
            source: SourceKind::Git,
            project: "lucina".into(),
            title: "ops: write the store first".into(),
            at: chrono::DateTime::parse_from_rfc3339("2026-11-01T10:00:00+01:00").unwrap(),
            text: "commit text".into(),
            files: vec![
                "/home/nez/Projects/Lucina/SecretaryIOS/crates/lucina-ops/src/lib.rs".into(),
            ],
            backlink: Backlink::Session { id: "x".into() },
        }
    }

    #[test]
    fn titles() {
        assert_eq!(
            clean_title("lucina-ops: writes [[store]] first / then the vault."),
            "lucina-ops writes store first then the vault"
        );
        assert_eq!(clean_title("..hidden"), "hidden");
        assert!(clean_title(&"word ".repeat(80)).chars().count() <= MAX_TITLE);
        assert_eq!(
            normalize("Perché  the Store—first!"),
            "perche the store first"
        );
    }

    #[test]
    fn answer_is_checked() {
        let existing = vec!["Lucina ops record writes the store before the vault".to_owned()];
        let answer = r#"<think>hmm</think>```json
        {"skip": false, "event": {"summary": "The ops store <!-- x --> now comes first. token ghp_abcdefghijklmnopqrstuvwxyz0123456789"},
         "claims": [
          {"title": "lucina ops record writes the STORE before the vault", "body": "Body [[link]].", "update": "Now also fsyncs.",
           "files": ["/home/nez/Projects/Lucina/SecretaryIOS/crates/lucina-ops/src/lib.rs", "/home/nez/src/invented.rs"],
           "connects_to": ["Nonexistent claim", "Second claim is about the vault lock"]},
          {"title": "Second claim is about the vault lock", "body": "B2.", "enables": ["Lucina ops record writes the store before the vault"]},
          {"title": "short", "body": "too short a title"},
          {"title": "Second claim is about the vault lock", "body": "duplicate"},
          {"title": "Third valid claim about something else", "body": "x"},
          {"title": "Fourth valid claim exceeds the maximum", "body": "x"}
         ]}
        ```"#;
        let p = Plan::from_answer(answer, &item(), &existing).unwrap();
        assert!(!p.skip);
        assert!(
            !p.summary.contains("<!--") && !p.summary.contains("ghp_"),
            "{}",
            p.summary
        );
        assert_eq!(p.claims.len(), 3);
        let c = &p.claims[0];
        assert_eq!(c.title, existing[0], "reused title takes the existing name");
        assert_eq!(c.body, "Body link.");
        assert_eq!(c.files.len(), 1, "invented path dropped");
        assert_eq!(c.connects_to, ["Second claim is about the vault lock"]);
        assert_eq!(p.claims[1].enables, [existing[0].clone()]);
    }

    #[test]
    fn skip_and_garbage() {
        let p = Plan::from_answer(r#"{"skip": true}"#, &item(), &[]).unwrap();
        assert!(p.skip && !p.skip_reason.is_empty());
        assert!(Plan::from_answer("no json here", &item(), &[]).is_err());
        assert!(Plan::from_answer(r#"{"claims": []}"#, &item(), &[]).is_err());
        let p = Plan::from_answer(
            r#"{"claims": [{"title": "A claim with a real title here", "body": "b"}]}"#,
            &item(),
            &[],
        )
        .unwrap();
        assert_eq!(
            p.summary, "ops: write the store first",
            "the item title stands in"
        );
    }

    #[test]
    fn secret_titles_are_dropped() {
        let p = Plan::from_answer(
            r#"{"event": {"summary": "s"}, "claims": [{"title": "The Kindle serial G000AB1234567890 is the board", "body": "b"}]}"#,
            &item(),
            &[],
        )
        .unwrap();
        assert!(p.claims.is_empty());
    }

    #[test]
    fn related() {
        let existing = vec![
            "Unrelated pokemon note".to_owned(),
            "The ops store is written first".to_owned(),
        ];
        assert_eq!(
            related_titles(&existing, &item(), 5),
            ["The ops store is written first"]
        );
        let p = prompt(&item(), &existing[1..]);
        assert!(
            p.contains("lucina-ops/src/lib.rs") && p.contains("The ops store is written first")
        );
    }
}
