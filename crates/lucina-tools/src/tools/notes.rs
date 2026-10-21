//! `notes`: markdown notes under `<data dir>/notes/` (atomic 0600 writes).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

const MAX_NOTE_CHARS: usize = 20_000;

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Read,
    Create,
    Append,
    Search,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// Note title (read, create, append).
    title: Option<String>,
    /// Markdown text (create, append).
    content: Option<String>,
    /// Text to look for (search).
    query: Option<String>,
}

/// Lowercase ASCII slug, `-` separated, at most 80 characters.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().to_lowercase().chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'æ' => 'a',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let s: String = out.trim_matches('-').chars().take(80).collect();
    s.trim_end_matches('-').to_owned()
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.env.data_dir.join("notes")
}

fn path_for(ctx: &Ctx, title: &str) -> Result<PathBuf, ToolError> {
    let s = slug(title);
    if s.is_empty() {
        return Err(ToolError::new("title is required"));
    }
    Ok(dir(ctx).join(format!("{s}.md")))
}

fn list(ctx: &Ctx) -> Vec<(String, std::time::SystemTime, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir(ctx)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let title = text
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("# "))
                .map(str::to_owned)
                .unwrap_or_else(|| p.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            out.push((title, mtime, text));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

pub struct NotesTool;

#[async_trait::async_trait]
impl Tool for NotesTool {
    fn name(&self) -> &'static str {
        "notes"
    }
    fn description(&self) -> &'static str {
        "The owner's private markdown notes: list, read, create, append or search notes by title/text."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "notes {} {}",
            args["action"].as_str().unwrap_or(""),
            args["title"].as_str().or(args["query"].as_str()).unwrap_or("")
        )
        .trim()
        .to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let req = |v: &Option<String>, what: &str| {
            v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).ok_or_else(|| ToolError::new(format!("{what} is required")))
        };
        match a.action {
            Action::List => Ok(json!({"notes": list(ctx).into_iter().take(100).map(|(t, m, _)| json!({
                "title": t,
                "modified": chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })).collect::<Vec<_>>()})),
            Action::Read => {
                let title = req(&a.title, "title")?;
                let p = path_for(ctx, &title)?;
                let text = std::fs::read_to_string(&p).map_err(|_| ToolError::new(format!("no note titled {title:?}")))?;
                Ok(json!({"title": title, "content": text.chars().take(8000).collect::<String>()}))
            }
            Action::Create | Action::Append => {
                let title = req(&a.title, "title")?;
                let content = req(&a.content, "content")?;
                let p = path_for(ctx, &title)?;
                let existing = std::fs::read_to_string(&p).ok();
                let text = match (a.action, existing) {
                    (Action::Create, Some(_)) => return Err(ToolError::new(format!("a note titled {title:?} exists; use append"))),
                    (Action::Create, None) => format!("# {title}\n\n{content}\n"),
                    (_, Some(old)) => format!("{}\n{content}\n", old.trim_end()),
                    (_, None) => format!("# {title}\n\n{content}\n"),
                };
                if text.chars().count() > MAX_NOTE_CHARS {
                    return Err(ToolError::new("note too long"));
                }
                lucina_store::write_atomic(&p, text.as_bytes()).map_err(|e| ToolError::new(format!("write failed: {e}")))?;
                ctx.push_effect(Effect::Note { title: title.clone() });
                Ok(json!({"saved": true, "title": title, "chars": text.chars().count()}))
            }
            Action::Search => {
                let q = req(&a.query, "query")?.to_lowercase();
                let hits: Vec<Value> = list(ctx)
                    .into_iter()
                    .filter_map(|(t, _, text)| {
                        let lines: Vec<String> = text.lines().filter(|l| l.to_lowercase().contains(&q)).take(5).map(str::to_owned).collect();
                        (t.to_lowercase().contains(&q) || !lines.is_empty()).then(|| json!({"title": t, "lines": lines}))
                    })
                    .take(20)
                    .collect();
                Ok(json!({"query": q, "results": hits}))
            }
        }
    }
}
