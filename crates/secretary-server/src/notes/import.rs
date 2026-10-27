//! The store API of the Google Keep importer: [`ImportedNote`] in,
//! [`ImportOutcome`] out, idempotent on the import key (docs/keep.md).
//!
//! Re-importing a known key:
//!
//! - in the trash here: left alone (`KeptLocalEdits`);
//! - the export's `editedUsec` is not newer: `Unchanged`;
//! - newer, and the note was not edited here since the import (its
//!   `updatedAt` still equals `import.importedAt`, the stamp the import wrote):
//!   `Updated`;
//! - newer, but edited here: `KeptLocalEdits`.
//!
//! A key whose note was deleted forever here is in `import-deleted.json`:
//! `SkippedDeleted`, never resurrected.

use super::labels::lenient_labels;
use super::model::*;
use super::store::{Caller, NoteStore};
use crate::error::ApiError;

/// One Keep note, mapped by the importer.
#[derive(Debug, Clone, Default)]
pub struct ImportedNote {
    /// `gkeep:<createdTimestampUsec>[-n]`.
    pub key: String,
    /// `userEditedTimestampUsec`.
    pub edited_usec: i64,
    pub title: String,
    /// Markdown.
    pub content: String,
    /// `(text, checked)`; `Some` = a list note.
    pub items: Option<Vec<(String, bool)>>,
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    pub trashed: bool,
    /// Display names.
    pub labels: Vec<String>,
    /// From [`NoteStore::put_attachment`].
    pub attachments: Vec<Attachment>,
    pub annotations: Vec<Annotation>,
    pub collaborators: Vec<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    New,
    Updated,
    Unchanged,
    KeptLocalEdits,
    SkippedDeleted,
}

/// Text without control characters (newline and tab stay), line endings as `\n`.
fn clean_body(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn clean_line(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// An RFC 3339 stamp at millisecond precision, or now when it is not one.
fn stamp(s: &str) -> String {
    rfc3339(parse_time(s.trim()).unwrap_or_else(now))
}

/// Overwrite the fields an import owns.
fn fill(note: &mut Note, n: &ImportedNote, labels: &[Label]) -> Result<(), ApiError> {
    note.title = clean_line(&n.title, MAX_TITLE_CHARS);
    note.color = {
        let c = n.color.trim().to_lowercase();
        if COLORS.contains(&c.as_str()) {
            c
        } else {
            "default".to_owned()
        }
    };
    note.pinned = n.pinned;
    note.archived = n.archived;
    note.tags = labels.iter().map(|l| l.tag.clone()).collect();
    let mut seen: Vec<&str> = Vec::new();
    note.attachments = n
        .attachments
        .iter()
        .filter(|a| {
            super::attachments::split_file_name(&a.file).is_some()
                && !seen.contains(&a.id.as_str())
                && {
                    seen.push(a.id.as_str());
                    true
                }
        })
        .take(MAX_ATTACHMENTS)
        .cloned()
        .collect();
    note.annotations = n
        .annotations
        .iter()
        .filter_map(|a| check_annotation(a).ok())
        .take(MAX_ANNOTATIONS)
        .collect();
    note.collaborators = n
        .collaborators
        .iter()
        .filter_map(|c| check_collaborators(std::slice::from_ref(c)).ok()?.pop())
        .take(MAX_COLLABORATORS)
        .collect();
    match &n.items {
        Some(items) => {
            let items: Vec<ChecklistItem> = items
                .iter()
                .take(MAX_ITEMS)
                .map(|(text, checked)| ChecklistItem {
                    id: new_item_id(),
                    text: clean_line(text, MAX_ITEM_CHARS),
                    checked: *checked,
                    indent: 0,
                })
                .collect();
            set_items(note, items)?;
        }
        None => {
            let c = clean_body(&n.content);
            if c.len() > MAX_CONTENT_BYTES {
                return Err(too_large());
            }
            note.content = c;
            note.items = None;
        }
    }
    Ok(())
}

impl NoteStore {
    /// Apply the idempotency rules to one imported note. `dry_run` decides
    /// the outcome and writes nothing (not even labels).
    pub fn import_note(
        &self,
        n: ImportedNote,
        dry_run: bool,
        caller: Caller,
    ) -> Result<ImportOutcome, ApiError> {
        if n.key.trim().is_empty() || n.key.len() > 100 {
            return Err(bad_request("import key: 1 to 100 characters"));
        }
        let labels = lenient_labels(&n.labels);
        self.with(|st, idx| {
            if idx.deleted_keys.contains(&n.key) {
                return Ok(ImportOutcome::SkippedDeleted);
            }
            let known = idx
                .notes
                .values()
                .map(|x| (x, false))
                .chain(idx.trash.values().map(|x| (x, true)))
                .find(|(x, _)| x.import.as_ref().is_some_and(|i| i.key == n.key));
            let updated = stamp(&n.updated_at);
            let outcome = match known {
                Some((_, true)) => return Ok(ImportOutcome::KeptLocalEdits),
                Some((old, false)) => {
                    let info = old
                        .import
                        .as_ref()
                        .ok_or_else(|| ApiError::internal("imported note without import info"))?;
                    if n.edited_usec <= info.edited_usec {
                        return Ok(ImportOutcome::Unchanged);
                    }
                    if old.updated_at != info.imported_at {
                        return Ok(ImportOutcome::KeptLocalEdits);
                    }
                    ImportOutcome::Updated
                }
                None => ImportOutcome::New,
            };
            if dry_run {
                return Ok(outcome);
            }
            let mut note = match known {
                Some((old, _)) => old.clone(),
                None => {
                    if idx.notes.len() >= MAX_NOTES && !n.trashed {
                        return Err(notes_full());
                    }
                    let created = stamp(&n.created_at);
                    let mut note = Note {
                        id: super::model::new_id().map_err(super::store::io_err)?,
                        slug: String::new(),
                        title: String::new(),
                        content: String::new(),
                        tags: Vec::new(),
                        source: "google-keep".into(),
                        pinned: false,
                        position: epoch_ms(&created).unwrap_or_else(now_ms),
                        created_at: created,
                        updated_at: updated.clone(),
                        deleted_at: None,
                        color: "default".into(),
                        archived: false,
                        items: None,
                        attachments: Vec::new(),
                        annotations: Vec::new(),
                        collaborators: Vec::new(),
                        reminder: None,
                        rev: 0,
                        import: None,
                    };
                    fill(&mut note, &n, &labels)?;
                    note.slug = idx.free_slug(
                        &slug_base(&note.title, &note.content, note.items.as_deref()),
                        None,
                    );
                    note
                }
            };
            if outcome == ImportOutcome::Updated {
                fill(&mut note, &n, &labels)?;
            }
            note.updated_at = updated.clone();
            note.import = Some(ImportInfo {
                source: "google-keep".into(),
                key: n.key.clone(),
                edited_usec: n.edited_usec,
                imported_at: updated,
            });
            if n.trashed && note.deleted_at.is_none() {
                note.deleted_at = Some(rfc3339(now()));
            }
            let stored = st.put(idx, note)?;
            idx.register(&labels);
            tracing::debug!(id = %stored.id, by = caller.0, "keep note imported");
            Ok(outcome)
        })
    }
}

impl From<ImportOutcome> for &'static str {
    fn from(o: ImportOutcome) -> Self {
        match o {
            ImportOutcome::New => "new",
            ImportOutcome::Updated => "updated",
            ImportOutcome::Unchanged => "unchanged",
            ImportOutcome::KeptLocalEdits => "keptLocalEdits",
            ImportOutcome::SkippedDeleted => "skippedDeleted",
        }
    }
}
