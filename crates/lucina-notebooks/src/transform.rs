//! Transformations: prompt templates run on one source, each making an insight (a summary,
//! the key ideas, a table of contents…). The built-ins are Open Notebook's six defaults
//! (Copyright (c) 2024 Luis Novo, MIT; see `LICENSE-open-notebook`), lightly edited.
//! "Dense Summary" is applied to every new source: it is what a source contributes when the
//! notebook keeps it in "summary" context.

use crate::llm::{Ask, Llm, Model, Turn};
use crate::store::Transformation;

pub const DENSE_SUMMARY: &str = "t-dense-summary";

fn builtin(id: &str, name: &str, description: &str, prompt: &str, apply_default: bool) -> Transformation {
    Transformation {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        prompt: prompt.trim().into(),
        apply_default,
        builtin: true,
    }
}

pub fn builtins() -> Vec<Transformation> {
    vec![
        builtin(
            DENSE_SUMMARY,
            "Dense Summary",
            "A rich, compact summary of the whole source",
            r#"
# MISSION
You write Sparse Priming Representations (SPR): the input rendered as a distilled list of succinct statements,
assertions, associations, concepts, analogies and metaphors, capturing as much as possible in as few words as
possible. The reader is another language model that will answer questions from it, so keep every fact, number,
name, definition and step that matters. Use complete sentences.
"#,
            true,
        ),
        builtin(
            "t-simple-summary",
            "Simple Summary",
            "A short summary of the content",
            r#"
# SYSTEM ROLE
You summarise content into a short, information-rich summary.

# TASK
Write a summary that:
- captures the core concepts and key information;
- uses clear, direct language;
- is at most about 200 words.
"#,
            false,
        ),
        builtin(
            "t-key-insights",
            "Key Insights",
            "The important insights and actionable items",
            r#"
# IDENTITY and PURPOSE
You extract surprising, powerful and useful insights from text.

# STEPS
- Collect 20 to 50 of the most surprising, insightful or useful ideas from the input (internally).
- From those, pick the 10 to 25 most powerful ones: higher-level, more abstract and wise.

# OUTPUT INSTRUCTIONS
- Output a section called INSIGHTS only, as a bulleted list (not numbered).
- Each bullet is about 15 words; don't start bullets with the same words.
- No warnings or notes.
"#,
            false,
        ),
        builtin(
            "t-paper-analysis",
            "Paper Analysis",
            "Analyses a technical or scientific paper",
            r#"
# IDENTITY and PURPOSE
You are an insightful, analytical reader of academic papers. You extract the core contributions, methods,
findings, implications and limitations, and judge how clear and well supported the arguments are.

# OUTPUT INSTRUCTIONS
Structure the output exactly as:
- **PURPOSE**: the main research question or goal (1-2 sentences).
- **CONTRIBUTION**: 2-3 bullets on what the paper adds to the field.
- **METHODS**: 1-3 bullets on how the study was done (design, data, sample, instruments).
- **KEY FINDINGS**: 2-3 bullets with the critical results.
- **IMPLICATIONS**: 2-3 bullets on why the findings matter.
- **LIMITATIONS**: 1-2 bullets on limitations or future work.
Bullets are 15-20 words, varied openings, clear language, no disclaimers or opinions.
"#,
            false,
        ),
        builtin(
            "t-reflection-questions",
            "Reflection Questions",
            "Questions that help explore the document further",
            r#"
# IDENTITY and PURPOSE
You turn a text into thought-provoking reflection questions that help someone study and question it: its
assumptions, implications, connections to other ideas, and how it could be applied or tested.

# OUTPUT INSTRUCTIONS
- A section labelled REFLECTIONS with 5 to 8 bullets (not numbered).
- Each bullet is one question of 15-30 words.
- Vary the phrasing; no warnings or notes.
"#,
            false,
        ),
        builtin(
            "t-table-of-contents",
            "Table of Contents",
            "The topics the document covers, in order",
            r#"
# SYSTEM ROLE
You read documents and write a Table of Contents that helps the reader see what the document covers.

# TASK
List every major topic and transition in the order it appears, each with a one-line description of what is
covered. Use page numbers when the input shows them ("[p. 7]").
"#,
            false,
        ),
    ]
}

/// How much of a source a transformation reads (characters). DeepSeek takes far more, but
/// the local model has an 80k-token window and long inputs are slow.
pub fn input_budget(model: Model) -> usize {
    match model {
        Model::Deepseek => 300_000,
        Model::Qwen => 120_000,
    }
}

/// Runs `t` on a source's text. Returns the insight's text.
pub async fn run(llm: &dyn Llm, model: Model, t: &Transformation, title: &str, text: &str) -> anyhow::Result<String> {
    let budget = input_budget(model);
    let mut input: String = text.chars().take(budget).collect();
    if text.chars().count() > budget {
        input.push_str("\n\n[… the rest of the source is cut off here]");
    }
    let system = format!(
        "{}\n\n# FORMAT\nWrite Markdown. Write math as $…$ or $$…$$. Answer in English, or in Danish when the input is Danish; never in Italian, unless the \
         instructions explicitly ask for another language. Only the requested output, no preamble.",
        t.prompt
    );
    let user = format!("# INPUT\nTitle: {title}\n\n{input}");
    let (out, _) = llm
        .complete(
            Ask {
                model,
                turns: vec![Turn::system(system), Turn::user(user)],
                max_tokens: Some(4000),
                temperature: Some(0.3),
                purpose: "notebook_transformation",
                ..Default::default()
            },
            None,
        )
        .await?;
    let out = out.trim().to_owned();
    if out.is_empty() {
        anyhow::bail!("the model returned nothing");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::fake::Scripted;

    #[test]
    fn six_builtins_one_default() {
        let b = builtins();
        assert_eq!(b.len(), 6);
        assert_eq!(b.iter().filter(|t| t.apply_default).count(), 1);
        assert!(b.iter().all(|t| t.id.starts_with("t-") && t.builtin && !t.prompt.is_empty()));
    }

    #[tokio::test]
    async fn run_sends_prompt_and_input() {
        let llm = Scripted::new(&["- short"]);
        let t = &builtins()[1];
        let out = run(&llm, Model::Deepseek, t, "RM 2", "Body text").await.unwrap();
        assert_eq!(out, "- short");
        let asked = llm.asked.lock().unwrap();
        assert!(asked[0].turns[0].text.contains("Simple") || asked[0].turns[0].text.contains("summary"));
        assert!(asked[0].turns[1].text.contains("Body text"));
    }
}
