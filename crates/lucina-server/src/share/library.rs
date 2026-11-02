//! Drive documents read like shared files. The `media` tool's `read` hands a
//! library file to [`LibraryReader`], which runs it through the share router
//! and handlers (`extract::read`), shows the model the same section a share
//! turn gets (type, guidance for that type, data, text) and keeps what it
//! read in `attachments.db`, so `attachments_search` finds it later.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use lucina_store::timeline::Attachment;
use lucina_tools::tools::media::{
    excerpt, DocumentReader, DocumentRequest, DocumentText, READ_MAX_CHARS,
};
use serde_json::Value;

use super::handlers::Extracted;
use super::{extract, memory, router, section, Part};
use crate::AppState;

/// The attachment pipeline for the `media` tool.
pub struct LibraryReader(pub AppState);

/// The attachment-memory id of a library item.
fn record_id(item_id: &str) -> String {
    format!("drive-{item_id}")
}

/// A stored reading is still good when the file has not changed since.
fn fresh(record: &memory::Record, modified: Option<&str>) -> bool {
    modified
        .and_then(|m| chrono::DateTime::parse_from_rfc3339(m).ok())
        .is_some_and(|m| m.timestamp_millis() < record.created_at)
}

/// A picture vision could not look at: not a reading to keep, or the next question would get it too.
fn saw_nothing(kind: &str, text: &str) -> bool {
    kind == "image" && text.starts_with("Vision: \n")
}

fn from_record(r: memory::Record) -> Extracted {
    let kind = router::KINDS
        .iter()
        .copied()
        .find(|k| *k == r.kind)
        .unwrap_or("other");
    Extracted {
        kind,
        subtype: r.subtype,
        title: Some(r.name),
        text: r.text,
        summary: r.summary,
        structured: r.structured,
        warnings: Vec::new(),
    }
}

/// The model's view: the share turn's section, with the text cut to the
/// passages that match the question when it is long.
fn render(part: &Part, x: &Extracted, question: Option<&str>) -> DocumentText {
    // Room for the separators, so the section never cuts the excerpt again.
    let cut = excerpt(&x.text, question, READ_MAX_CHARS - 32);
    let shown = Extracted {
        text: cut.text,
        ..x.clone()
    };
    let mut budget = usize::MAX;
    let mut content = section(1, part, &shown, &mut budget);
    if cut.truncated {
        content.push_str(if cut.matched {
            "Note: the text above is only the passages that match the question; ask with other words for other parts.\n"
        } else {
            "Note: the text above is the beginning only; a specific question gets the passages about it.\n"
        });
    }
    DocumentText {
        kind: x.kind.into(),
        content: content.trim_start().to_owned(),
        truncated: cut.truncated,
    }
}

#[async_trait::async_trait]
impl DocumentReader for LibraryReader {
    async fn local_path(&self, item_id: &str) -> Option<PathBuf> {
        let base = self.0.cfg.share.media_url.trim_end_matches('/');
        let r = reqwest::Client::new()
            .get(format!("{base}/internal/v1/items/{item_id}/path"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success())?;
        let path = PathBuf::from(r.json::<Value>().await.ok()?["path"].as_str()?);
        let meta = tokio::fs::metadata(&path).await.ok()?;
        meta.is_file().then_some(path)
    }

    async fn read(&self, req: DocumentRequest<'_>) -> Result<DocumentText, String> {
        let st = &self.0;
        let bytes = tokio::fs::metadata(req.path).await.ok().map(|m| m.len());
        let mut part = Part::new(Attachment {
            kind: "file".into(),
            name: Some(req.name.to_owned()),
            mime: Some(req.mime.to_owned()).filter(|m| !m.is_empty()),
            media_id: Some(req.item_id.to_owned()),
            bytes,
            ..Attachment::default()
        });
        let id = record_id(req.item_id);
        let mem = st.share.memory.clone();
        let cached = {
            let (mem, id) = (mem.clone(), id.clone());
            tokio::task::spawn_blocking(move || mem.get(&id))
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
        }
        .filter(|r| fresh(r, req.modified) && !saw_nothing(&r.kind, &r.text));
        if let Some(r) = cached {
            return Ok(render(&part, &from_record(r), req.question));
        }
        part.path = Some(req.path.to_owned());
        let x = extract::read(st, &st.cfg.share.dir, &mut part, true, None, "en", false)
            .await
            .map_err(|e| e.to_string())?;
        let record = memory::Record {
            id,
            share_id: "drive".into(),
            media_id: Some(req.item_id.to_owned()),
            item_id: None,
            name: req.name.to_owned(),
            kind: x.kind.into(),
            subtype: x.subtype.clone(),
            created_at: Utc::now().timestamp_millis(),
            text: x.text.clone(),
            summary: x.summary.clone(),
            structured: x.structured.clone(),
        };
        if saw_nothing(x.kind, &x.text) {
            return Ok(render(&part, &x, req.question));
        }
        let item = req.item_id.to_owned();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || mem.put(&record)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(%item, error = %e, "attachment memory write failed (drive read)")
                }
                Err(e) => tracing::warn!(%item, error = %e, "attachment memory write panicked"),
            }
        });
        Ok(render(&part, &x, req.question))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(dir: &std::path::Path) -> AppState {
        let mut cfg = crate::Config::for_data_dir(dir);
        cfg.share.dir = dir.join("share");
        AppState::build(cfg, None).unwrap()
    }

    #[test]
    fn a_failed_look_at_a_picture_is_not_a_reading() {
        assert!(saw_nothing("image", "Vision: \nOCR text:\ngarbled"));
        assert!(!saw_nothing("image", "Vision: A cat.\nOCR text:\n"));
        assert!(!saw_nothing("pdf", "Vision: \n"));
    }

    #[tokio::test]
    async fn a_drive_file_is_read_by_its_handler_with_that_types_guidance_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let st = st(dir.path());
        let file = dir.path().join("orig");
        let csv = "date,amount\n2026-10-01,12.50\n2026-10-02,7.50\n";
        std::fs::write(&file, csv).unwrap();
        let reader = LibraryReader(st.clone());
        let doc = reader
            .read(DocumentRequest {
                item_id: "itm_csv",
                path: &file,
                name: "spese.csv",
                mime: "text/csv",
                modified: Some("2026-10-02T10:00:00Z"),
                question: Some("total"),
            })
            .await
            .unwrap();
        assert_eq!(doc.kind, "sheet");
        assert!(!doc.truncated);
        assert!(
            doc.content
                .starts_with("### 1. spese.csv — sheet/csv, text/csv"),
            "{}",
            doc.content
        );
        assert!(doc.content.contains("How to handle it: Spreadsheet or CSV"));
        assert!(doc.content.contains("Data: "), "{}", doc.content);
        // Stored for attachments_search (written in the background).
        let mut stored = None;
        for _ in 0..50 {
            stored = st.share.memory.get("drive-itm_csv").unwrap();
            if stored.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let stored = stored.expect("remembered");
        assert_eq!(
            (stored.kind.as_str(), stored.share_id.as_str()),
            ("sheet", "drive")
        );
        assert_eq!(stored.media_id.as_deref(), Some("itm_csv"));
        // Unchanged file: the stored reading answers (even with the file gone).
        std::fs::remove_file(&file).unwrap();
        let again = reader
            .read(DocumentRequest {
                item_id: "itm_csv",
                path: &file,
                name: "spese.csv",
                mime: "text/csv",
                modified: Some("2026-10-02T10:00:00Z"),
                question: None,
            })
            .await
            .unwrap();
        assert!(again
            .content
            .contains("How to handle it: Spreadsheet or CSV"));
    }

    #[test]
    fn long_text_keeps_the_passages_about_the_question() {
        let part = Part::new(Attachment {
            kind: "file".into(),
            name: Some("L05.pptx".into()),
            ..Attachment::default()
        });
        let mut x = Extracted::new("slides");
        for i in 1..=200 {
            x.text.push_str(&format!(
                "\n## Slide {i}\nAcquisition and imaging, part {i}. "
            ));
            if i == 150 {
                x.text
                    .push_str("File carving finds files by header and footer signatures. ");
            }
            x.text.push_str(&"hashing and chain of custody ".repeat(4));
        }
        let doc = render(&part, &x, Some("file carving"));
        assert!(doc.truncated);
        assert!(doc.content.contains("How to handle it: Slides"));
        assert!(doc.content.contains("## Slide 150"));
        assert!(doc
            .content
            .contains("only the passages that match the question"));
        assert!(
            !doc.content.contains("(beginning; the rest is stored"),
            "not cut twice"
        );
    }

    #[test]
    fn a_stored_reading_is_reused_only_for_an_unchanged_file() {
        let r = memory::Record {
            created_at: chrono::DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
                .unwrap()
                .timestamp_millis(),
            kind: "pdf".into(),
            ..memory::Record::default()
        };
        assert!(fresh(&r, Some("2026-10-19T08:00:00Z")));
        assert!(!fresh(&r, Some("2026-10-21T08:00:00Z")));
        assert!(!fresh(&r, None));
        assert_eq!(from_record(r).kind, "pdf");
    }
}
