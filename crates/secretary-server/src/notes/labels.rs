//! The label registry: Keep labels are the notes' `tags`; `notes/labels.json`
//! remembers their display case ("Fisica 2") and labels without notes.

use std::collections::HashMap;

use axum::http::StatusCode;
use serde::Serialize;

use super::model::*;
use super::store::{Index, LabelEntry, NoteStore};
use crate::error::ApiError;

/// A label as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelOut {
    pub tag: String,
    pub name: String,
    /// Live, non-trashed notes that carry it.
    pub count: usize,
}

fn label_not_found(tag: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "label_not_found",
        format!("no label {tag:?}"),
    )
}

/// Label names from an import: control characters and `,` become spaces,
/// the rest is clipped; what cannot be a label is dropped.
pub(super) fn lenient_labels(names: &[String]) -> Vec<Label> {
    let mut out: Vec<Label> = Vec::new();
    for raw in names {
        let cleaned: String = raw
            .chars()
            .map(|c| if c.is_control() || c == ',' { ' ' } else { c })
            .collect();
        let flat = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        let clipped: String = flat.chars().take(MAX_TAG_CHARS).collect();
        if let Ok(Some(l)) = parse_label(&clipped) {
            if !out.iter().any(|x| x.tag == l.tag) {
                out.push(l);
            }
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

impl Index {
    fn live_counts(&self) -> HashMap<&str, usize> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for n in self.notes.values() {
            for t in &n.tags {
                *counts.entry(t.as_str()).or_default() += 1;
            }
        }
        counts
    }

    /// Registered, or carried by any note (live or in the trash).
    fn label_known(&self, tag: &str) -> bool {
        self.labels.iter().any(|l| l.tag == tag)
            || self
                .notes
                .values()
                .chain(self.trash.values())
                .any(|n| n.tags.iter().any(|t| t == tag))
    }

    fn label_out(&self, tag: &str) -> LabelOut {
        let name = self
            .labels
            .iter()
            .find(|l| l.tag == tag)
            .map_or_else(|| tag.to_owned(), |l| l.name.clone());
        let count = self
            .notes
            .values()
            .filter(|n| n.tags.iter().any(|t| t == tag))
            .count();
        LabelOut {
            tag: tag.to_owned(),
            name,
            count,
        }
    }
}

impl NoteStore {
    /// Registry ∪ tags in use, by display name (case-insensitive).
    pub fn labels(&self) -> Vec<LabelOut> {
        self.with(|_, idx| {
            let counts = idx.live_counts();
            let names = idx.label_names();
            let mut out: Vec<LabelOut> = idx
                .labels
                .iter()
                .map(|l| LabelOut {
                    tag: l.tag.clone(),
                    name: l.name.clone(),
                    count: counts.get(l.tag.as_str()).copied().unwrap_or(0),
                })
                .collect();
            for (tag, count) in &counts {
                if !names.contains_key(*tag) {
                    out.push(LabelOut {
                        tag: (*tag).to_owned(),
                        name: (*tag).to_owned(),
                        count: *count,
                    });
                }
            }
            out.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.tag.cmp(&b.tag))
            });
            out
        })
    }

    /// `POST /v1/notes/labels`: `409 label_exists` when the tag is known.
    pub fn create_label(&self, name: &str) -> Result<LabelOut, ApiError> {
        let label = parse_label(name)?.ok_or_else(|| bad_request("name: must not be empty"))?;
        self.with(|_, idx| {
            if idx.label_known(&label.tag) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "label_exists",
                    format!("a label {:?} exists", label.name),
                ));
            }
            idx.register(std::slice::from_ref(&label));
            Ok(idx.label_out(&label.tag))
        })
    }

    /// `PATCH /v1/notes/labels/{tag}`: a new display case for the same tag,
    /// or a new tag rewritten in every note (merged into the label that
    /// already has that name).
    pub fn rename_label(&self, tag: &str, name: &str) -> Result<LabelOut, ApiError> {
        let old = parse_label(tag)?.ok_or_else(|| label_not_found(tag))?;
        let new = parse_label(name)?.ok_or_else(|| bad_request("name: must not be empty"))?;
        self.with(|st, idx| {
            if !idx.label_known(&old.tag) {
                return Err(label_not_found(&old.tag));
            }
            if new.tag == old.tag {
                match idx.labels.iter_mut().find(|l| l.tag == old.tag) {
                    Some(e) => e.name = new.name.clone(),
                    None => idx.labels.push(LabelEntry {
                        tag: new.tag.clone(),
                        name: new.name.clone(),
                        created_at: rfc3339(now()),
                    }),
                }
                idx.mark_labels_dirty();
                return Ok(idx.label_out(&new.tag));
            }
            // Rewrite the tag everywhere; work out all notes before writing.
            let mut touched: Vec<Note> = Vec::new();
            for n in idx.notes.values().chain(idx.trash.values()) {
                if !n.tags.contains(&old.tag) {
                    continue;
                }
                let mut m = n.clone();
                let mut tags: Vec<String> = Vec::new();
                for t in &n.tags {
                    let t = if *t == old.tag {
                        new.tag.clone()
                    } else {
                        t.clone()
                    };
                    if !tags.contains(&t) {
                        tags.push(t);
                    }
                }
                m.tags = tags;
                touched.push(m);
            }
            for n in touched {
                st.put(idx, n)?;
            }
            let created = idx
                .labels
                .iter()
                .find(|l| l.tag == old.tag)
                .map(|l| l.created_at.clone());
            idx.labels.retain(|l| l.tag != old.tag);
            if !idx.labels.iter().any(|l| l.tag == new.tag) {
                idx.labels.push(LabelEntry {
                    tag: new.tag.clone(),
                    name: new.name.clone(),
                    created_at: created.unwrap_or_else(|| rfc3339(now())),
                });
            }
            idx.mark_labels_dirty();
            Ok(idx.label_out(&new.tag))
        })
    }

    /// `DELETE /v1/notes/labels/{tag}`: out of the registry and every note.
    /// Returns how many notes lost it.
    pub fn delete_label(&self, tag: &str) -> Result<usize, ApiError> {
        let old = parse_label(tag)?.ok_or_else(|| label_not_found(tag))?;
        self.with(|st, idx| {
            if !idx.label_known(&old.tag) {
                return Err(label_not_found(&old.tag));
            }
            let touched: Vec<Note> = idx
                .notes
                .values()
                .chain(idx.trash.values())
                .filter(|n| n.tags.contains(&old.tag))
                .map(|n| {
                    let mut m = n.clone();
                    m.tags.retain(|t| *t != old.tag);
                    m
                })
                .collect();
            let n = touched.len();
            for m in touched {
                st.put(idx, m)?;
            }
            idx.labels.retain(|l| l.tag != old.tag);
            idx.mark_labels_dirty();
            Ok(n)
        })
    }

    /// Register labels (display names); returns the names that were new.
    /// `dry_run` only computes them. Names the importer cannot use as they
    /// are (a `,`, control characters, too long) are cleaned up.
    pub fn ensure_labels(&self, names: &[String], dry_run: bool) -> Result<Vec<String>, ApiError> {
        let labels = lenient_labels(names);
        self.with(|_, idx| {
            let in_use = idx.live_counts();
            let new: Vec<Label> = labels
                .into_iter()
                .filter(|l| {
                    !idx.labels.iter().any(|e| e.tag == l.tag)
                        && !in_use.contains_key(l.tag.as_str())
                })
                .collect();
            if !dry_run {
                idx.register(&new);
            }
            Ok(new.into_iter().map(|l| l.name).collect())
        })
    }
}
