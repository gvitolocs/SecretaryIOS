//! Chat with a notebook: each question retrieves the notebook's best passages (hybrid
//! search over its sources, insights and notes), the model answers from them only, and
//! every claim cites a passage: `[source:<chunk id>]`, `[insight:<id>]` or `[note:<slug>]`
//! (Open Notebook's `[type:id]` convention; `[note:<slug>]` is how Lucina cites notes
//! everywhere). Citations are checked against the passages actually given: a made-up or
//! mangled id is dropped, and the message carries the resolved list.

use std::collections::HashMap;

use regex::Regex;

use crate::llm::{Ask, Delta, Llm, Model, Turn};
use crate::search::{excerpt, Hit};
use crate::store::{Chunk, Citation, Message, Store};
use crate::Error;

pub const PASSAGES: usize = 10;
const HISTORY: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    /// What the model cites: `source:s…-3`, `insight:i…`, `note:<slug>`.
    pub reference: String,
    pub chunk: Chunk,
    pub title: String,
}

/// The citation reference for a passage.
pub fn reference(chunk: &Chunk) -> String {
    if let Some(slug) = chunk.owner.strip_prefix("note:") {
        format!("note:{slug}")
    } else if chunk.owner.starts_with('i') {
        format!("insight:{}", chunk.owner)
    } else {
        format!("source:{}", chunk.id)
    }
}

pub fn passages(store: &Store, hits: &[Hit]) -> Result<Vec<Passage>, Error> {
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    for h in hits {
        let title = match titles.get(&h.chunk.owner) {
            Some(t) => t.clone(),
            None => {
                let t = store.owner_title(&h.chunk.owner)?;
                titles.insert(h.chunk.owner.clone(), t.clone());
                t
            }
        };
        out.push(Passage { reference: reference(&h.chunk), chunk: h.chunk.clone(), title });
    }
    Ok(out)
}

fn where_in(c: &Chunk) -> String {
    match (c.page, c.start_s) {
        (Some(p), _) => format!(" page=\"{p}\""),
        (_, Some(s)) => format!(" at=\"{}\"", clock(s)),
        _ => String::new(),
    }
}

pub fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn system_prompt(notebook: &str, description: &str, passages: &[Passage]) -> String {
    let mut p = format!(
        "You are Lucina, Giuseppe's assistant, helping him study and work with his notebook \"{notebook}\".{}\n\n\
         # HOW TO ANSWER\n\
         - Answer from the passages below. They come from the notebook's sources, insights and notes.\n\
         - Cite every statement you take from a passage by writing its id in square brackets right after it, \
           for example [source:s0a9be3c112-14] or [note:lecture-notes]. Use one bracket per id; to cite two \
           passages write [source:a-1][source:b-2].\n\
         - Copy ids exactly as they appear in `id=\"…\"`. Never invent, shorten or change an id, and never cite \
           anything that is not listed below.\n\
         - If the passages don't answer the question, say so plainly. You may add general knowledge only when it \
           clearly helps, and then say that it is not from his sources (and cite nothing for it).\n\
         - Answer in the language of the question. Be clear and complete but not padded; use Markdown (lists, \
           bold, tables) when it helps, and $…$ / $$…$$ for math.\n\n",
        if description.trim().is_empty() { String::new() } else { format!(" It is about: {}.", description.trim()) }
    );
    if passages.is_empty() {
        p.push_str("# PASSAGES\n(none: the notebook has no passages matching this question yet)\n");
    } else {
        p.push_str("# PASSAGES\n");
        for x in passages {
            p.push_str(&format!(
                "<passage id=\"{}\" title=\"{}\"{}>\n{}\n</passage>\n",
                x.reference,
                x.title.replace('"', "'"),
                where_in(&x.chunk),
                x.chunk.text.trim()
            ));
        }
    }
    p
}

/// What to search for: the question, plus the previous question when this one is a short
/// follow-up ("and the second one?").
pub fn retrieval_query(question: &str, history: &[Message]) -> String {
    let words = question.split_whitespace().count();
    if words < 8 {
        if let Some(prev) = history.iter().rev().find(|m| m.role == "user") {
            return format!("{} {}", prev.text, question);
        }
    }
    question.to_owned()
}

fn cite_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[((?:source|insight|note|s[0-9a-f]{10})[^\[\]\n]{0,400})\]").expect("static regex"))
}

/// Keeps only citations of `passages`, one id per bracket, and lists them (first use first).
pub fn resolve_citations(text: &str, passages: &[Passage], query: &str) -> (String, Vec<Citation>) {
    let by_ref: HashMap<&str, &Passage> = passages.iter().map(|p| (p.reference.as_str(), p)).collect();
    let mut cited: Vec<&Passage> = Vec::new();
    let out = cite_re().replace_all(text, |caps: &regex::Captures| {
        let inner = &caps[1];
        let mut kind = "";
        let mut kept = String::new();
        for item in inner.split([',', ';']) {
            let item = item.trim();
            let full = if let Some((k, _)) = item.split_once(':') {
                kind = match k.trim() {
                    "source" => "source",
                    "insight" => "insight",
                    "note" => "note",
                    _ => "",
                };
                item.replace(' ', "")
            } else if item.starts_with('s') && item.contains('-') {
                format!("source:{item}")
            } else if !kind.is_empty() {
                format!("{kind}:{item}")
            } else {
                continue;
            };
            if let Some(p) = by_ref.get(full.as_str()) {
                kept.push_str(&format!("[{full}]"));
                if !cited.iter().any(|c| c.reference == p.reference) {
                    cited.push(p);
                }
            }
        }
        kept
    });
    // A dropped citation can leave "word ." or doubled spaces behind.
    let out = out.replace(" .", ".").replace(" ,", ",").replace("  ", " ");
    let citations = cited
        .into_iter()
        .map(|p| {
            let (kind, rest) = p.reference.split_once(':').unwrap_or(("source", ""));
            Citation {
                reference: p.reference.clone(),
                kind: kind.to_owned(),
                source_id: (kind == "source").then(|| p.chunk.owner.clone()),
                chunk: (kind == "source").then_some(p.chunk.ordinal),
                slug: (kind == "note").then(|| rest.to_owned()),
                insight_id: (kind == "insight").then(|| p.chunk.owner.clone()),
                title: p.title.clone(),
                excerpt: excerpt(&p.chunk.text, query, 240),
                page: p.chunk.page,
                start_s: p.chunk.start_s,
            }
        })
        .collect();
    (out.trim().to_owned(), citations)
}

pub struct Answer {
    pub text: String,
    pub citations: Vec<Citation>,
    pub model: String,
}

/// One turn: the model's answer to `question` from `passages`, given the chat so far.
#[allow(clippy::too_many_arguments)]
pub async fn answer(
    llm: &dyn Llm,
    model: Model,
    notebook: &str,
    description: &str,
    history: &[Message],
    passages: &[Passage],
    question: &str,
    on_delta: Option<Delta>,
) -> anyhow::Result<Answer> {
    let mut turns = vec![Turn::system(system_prompt(notebook, description, passages))];
    let start = history.len().saturating_sub(HISTORY);
    for m in &history[start..] {
        turns.push(if m.role == "user" { Turn::user(&m.text) } else { Turn::assistant(&m.text) });
    }
    turns.push(Turn::user(question));
    let (text, model_name) = llm
        .complete(
            Ask {
                model,
                turns,
                max_tokens: Some(3000),
                temperature: Some(0.2),
                purpose: "notebook_chat",
                ..Default::default()
            },
            on_delta,
        )
        .await?;
    let (text, citations) = resolve_citations(&text, passages, question);
    if text.is_empty() {
        anyhow::bail!("the model returned nothing");
    }
    Ok(Answer { text, citations, model: model_name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::fake::Scripted;

    fn passage(owner: &str, ordinal: i64, text: &str) -> Passage {
        let chunk = Chunk {
            id: format!("{owner}-{ordinal}"),
            owner: owner.into(),
            ordinal,
            text: text.into(),
            page: Some(3),
            start_s: None,
        };
        Passage { reference: reference(&chunk), title: "RM Class 2".into(), chunk }
    }

    #[test]
    fn references() {
        assert_eq!(passage("s0a9be3c112", 4, "x").reference, "source:s0a9be3c112-4");
        assert_eq!(passage("note:lecture-1", 0, "x").reference, "note:lecture-1");
        assert_eq!(passage("i0000000001", 2, "x").reference, "insight:i0000000001");
    }

    #[test]
    fn citations_are_checked_split_and_listed() {
        let ps = vec![
            passage("s0a9be3c112", 4, "Snooping trusts uplinks"),
            passage("s0a9be3c112", 5, "Option 82 adds relay info"),
            passage("note:lecture-1", 0, "My own notes"),
        ];
        let raw = "Uplinks are trusted [source:s0a9be3c112-4, source:s0a9be3c112-5]. \
                   Made up [source:s0a9be3c112-99]. Bare id [s0a9be3c112-5]. Notes say so [note:lecture-1]. \
                   Again [source:s0a9be3c112-4; 5].";
        let (text, cites) = resolve_citations(raw, &ps, "uplinks");
        assert!(text.contains("trusted [source:s0a9be3c112-4][source:s0a9be3c112-5]."), "{text}");
        assert!(text.contains("Made up."), "{text}");
        assert!(text.contains("Bare id [source:s0a9be3c112-5]."), "{text}");
        assert!(text.contains("[note:lecture-1]"), "{text}");
        assert!(!text.contains("-99"));
        assert_eq!(cites.len(), 3);
        assert_eq!(cites[0].reference, "source:s0a9be3c112-4");
        assert_eq!(cites[0].source_id.as_deref(), Some("s0a9be3c112"));
        assert_eq!(cites[0].chunk, Some(4));
        assert_eq!(cites[0].page, Some(3));
        assert_eq!(cites[2].kind, "note");
        assert_eq!(cites[2].slug.as_deref(), Some("lecture-1"));
    }

    #[test]
    fn markdown_links_and_other_brackets_survive() {
        let (t, c) = resolve_citations("See [the docs](https://x) and [1].", &[], "");
        assert_eq!(t, "See [the docs](https://x) and [1].");
        assert!(c.is_empty());
    }

    #[test]
    fn follow_ups_carry_the_previous_question() {
        let h = vec![Message {
            id: "m1".into(),
            role: "user".into(),
            text: "What is DHCP snooping?".into(),
            citations: vec![],
            model: None,
            created_at: "x".into(),
        }];
        assert_eq!(retrieval_query("and option 82?", &h), "What is DHCP snooping? and option 82?");
        assert_eq!(retrieval_query("Explain the three research methods introduced in class two", &h), "Explain the three research methods introduced in class two");
        assert_eq!(clock(75.0), "1:15");
        assert_eq!(clock(3725.0), "1:02:05");
    }

    #[tokio::test]
    async fn answer_uses_passages_and_history() {
        let llm = Scripted::new(&["Uplinks are trusted [source:s0a9be3c112-4] [source:bogus-1]."]);
        let ps = vec![passage("s0a9be3c112", 4, "Snooping trusts uplinks")];
        let a = answer(&llm, Model::Deepseek, "CNS", "", &[], &ps, "Which ports are trusted?", None).await.unwrap();
        assert_eq!(a.text, "Uplinks are trusted [source:s0a9be3c112-4].");
        assert_eq!(a.citations.len(), 1);
        let asked = llm.asked.lock().unwrap();
        assert!(asked[0].turns[0].text.contains("id=\"source:s0a9be3c112-4\""));
        assert!(asked[0].turns[0].text.contains("page=\"3\""));
    }
}
