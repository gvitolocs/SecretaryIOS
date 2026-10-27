//! Seed notes for `secretary-server notes import`.

use serde::Deserialize;
use serde_json::Value;

use super::model::Note;
use super::model::{CreateNote, UpdateNote};
use super::store::{Caller, NoteStore};
use crate::error::ApiError;

/// A note to import: a `.json` file (`{slug, title, content, tags?, source?,
/// pinned?}`) or a `.md` file with a front matter block:
///
/// ```text
/// ---
/// slug: craftit-meeting
/// title: CRAFT-IT4SD — meeting with Marianne & Jonas
/// tags: work, aarhus
/// source: chatgpt
/// pinned: true
/// ---
/// # Markdown content…
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    pub slug: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "owner")]
    pub source: String,
    #[serde(default)]
    pub pinned: bool,
}

fn owner() -> String {
    "owner".into()
}

/// Parse a seed file's text (`md` = front matter + markdown).
pub fn parse_seed(text: &str, md: bool) -> anyhow::Result<Seed> {
    if !md {
        return Ok(serde_json::from_str(text)?);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("a .md seed starts with a '---' front matter block"))?;
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("the front matter block has no closing '---'"))?;
    let (head, body) = (&rest[..end], &rest[end + 5..]);
    let mut v = serde_json::Map::new();
    for line in head.lines().filter(|l| !l.trim().is_empty()) {
        let (k, val) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("front matter line without ':': {line:?}"))?;
        let (k, val) = (k.trim(), val.trim());
        let value = match k {
            "tags" => Value::Array(
                val.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| Value::String(t.to_owned()))
                    .collect(),
            ),
            "pinned" => Value::Bool(val == "true"),
            _ => Value::String(val.to_owned()),
        };
        v.insert(k.to_owned(), value);
    }
    v.insert(
        "content".into(),
        Value::String(body.trim_start_matches('\n').to_owned()),
    );
    Ok(serde_json::from_value(Value::Object(v))?)
}

impl NoteStore {
    /// Import (CLI): create the note, or with `replace` update the one with
    /// the same slug. Returns `(note, created)`; `None` = skipped (exists).
    pub fn import(&self, seed: Seed, replace: bool) -> Result<Option<(Note, bool)>, ApiError> {
        if let Ok(existing) = self.get(&seed.slug) {
            if !replace {
                return Ok(None);
            }
            let n = self.update(
                &existing.id,
                UpdateNote {
                    title: Some(seed.title),
                    content: Some(seed.content),
                    tags: Some(seed.tags),
                    pinned: Some(seed.pinned),
                    ..UpdateNote::default()
                },
            )?;
            return Ok(Some((n, false)));
        }
        let n = self.create(
            CreateNote {
                title: seed.title,
                content: seed.content,
                slug: Some(seed.slug),
                tags: seed.tags,
                source: Some(seed.source),
                pinned: seed.pinned,
                ..CreateNote::default()
            },
            Caller("owner"),
        )?;
        Ok(Some((n, true)))
    }
}
