//! The import report: what was found, what was (or, in a dry run, would be)
//! written, and what went wrong. JSON is camelCase (the endpoint and
//! `--json`); [`ImportReport::render_text`] is the CLI's human form.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileCounts {
    /// Note JSON files found.
    pub json: usize,
    /// `.html` copies of notes (ignored).
    pub html: usize,
    /// Distinct files referenced by notes as attachments.
    pub attachments: usize,
    /// Everything else (other products, unreferenced files).
    pub ignored: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NoteCounts {
    pub total: usize,
    pub new: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub kept_local_edits: usize,
    pub skipped_deleted: usize,
    pub trashed: usize,
    pub archived: usize,
    pub pinned: usize,
    pub lists: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelCounts {
    /// Distinct labels in the export (`Labels.txt` plus the notes').
    pub total: usize,
    /// Display names that did not exist yet.
    pub new: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AttachmentCounts {
    /// Attachment references that resolved to a usable file.
    pub total: usize,
    /// Distinct files not stored yet (a dry run counts them too).
    pub new: usize,
    /// Paths named by a note but absent from the zips.
    pub missing: Vec<String>,
    /// Bytes of the new files.
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportError {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleNote {
    pub title: String,
    pub labels: Vec<String>,
    pub color: String,
    /// `list`, `image`, `audio`, `link` or `text`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportReport {
    pub dry_run: bool,
    pub files: FileCounts,
    pub notes: NoteCounts,
    pub labels: LabelCounts,
    pub attachments: AttachmentCounts,
    pub errors: Vec<ReportError>,
    /// The first 20 new notes.
    pub sample: Vec<SampleNote>,
}

/// Size of [`ImportReport::sample`].
pub const SAMPLE_LEN: usize = 20;

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

impl ImportReport {
    /// Multi-line text for the terminal.
    pub fn render_text(&self) -> String {
        let mut s = String::new();
        let n = &self.notes;
        let _ = writeln!(
            s,
            "{}",
            if self.dry_run {
                "Google Keep import (dry run: nothing written)"
            } else {
                "Google Keep import"
            }
        );
        let _ = writeln!(
            s,
            "files:       {} notes (json), {} html ignored, {} attachments, {} other ignored",
            self.files.json, self.files.html, self.files.attachments, self.files.ignored
        );
        let _ = writeln!(
            s,
            "notes:       {} total: {} new, {} updated, {} unchanged, {} kept (edited here), {} skipped (deleted here)",
            n.total, n.new, n.updated, n.unchanged, n.kept_local_edits, n.skipped_deleted
        );
        let _ = writeln!(
            s,
            "             {} lists, {} pinned, {} archived, {} trashed",
            n.lists, n.pinned, n.archived, n.trashed
        );
        let _ = writeln!(
            s,
            "labels:      {} total, {} new{}",
            self.labels.total,
            self.labels.new.len(),
            if self.labels.new.is_empty() {
                String::new()
            } else {
                format!(": {}", self.labels.new.join(", "))
            }
        );
        let a = &self.attachments;
        let _ = writeln!(
            s,
            "attachments: {} total, {} new ({}), {} missing",
            a.total,
            a.new,
            human_bytes(a.bytes),
            a.missing.len()
        );
        for m in &a.missing {
            let _ = writeln!(s, "  missing: {m}");
        }
        if !self.sample.is_empty() {
            let _ = writeln!(s, "first new notes:");
            for x in &self.sample {
                let title = if x.title.is_empty() {
                    "(untitled)"
                } else {
                    &x.title
                };
                let labels = if x.labels.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", x.labels.join(", "))
                };
                let _ = writeln!(s, "  {} · {} · {title}{labels}", x.kind, x.color);
            }
        }
        if self.errors.is_empty() {
            let _ = writeln!(s, "errors:      none");
        } else {
            let _ = writeln!(s, "errors:      {}", self.errors.len());
            for e in &self.errors {
                let _ = writeln!(s, "  {}: {}", e.file, e.error);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_camel_case_and_round_trips() {
        let r = ImportReport {
            dry_run: true,
            notes: NoteCounts {
                kept_local_edits: 2,
                skipped_deleted: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["dryRun"], true);
        assert_eq!(v["notes"]["keptLocalEdits"], 2);
        assert_eq!(v["notes"]["skippedDeleted"], 1);
        assert!(v["files"]["json"].is_number());
        assert!(v["attachments"]["missing"].is_array());
        assert_eq!(serde_json::from_value::<ImportReport>(v).unwrap(), r);
        assert!(r.render_text().contains("dry run"));
    }
}
