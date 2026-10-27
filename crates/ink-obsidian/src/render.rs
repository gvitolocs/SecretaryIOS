//! Generated book notes (opt-in). Rendering and write planning are pure; no
//! code path writes to a vault. Only the region between the lucina-ink markers
//! of a note we generated is ever replaced; anything else needs approval.
use crate::parse::split_frontmatter;
use ink_library::model::{Book, Highlight, HighlightKind};

pub const BEGIN: &str = "<!-- lucina-ink:begin -->";
pub const END: &str = "<!-- lucina-ink:end -->";
pub const GENERATED_BY: &str = "lucina-ink";

/// `Books/<Title>.md`, with characters Obsidian rejects in names replaced.
pub fn book_note_path(book: &Book) -> String {
    let name: String = book
        .title
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']'
                )
            {
                ' '
            } else {
                c
            }
        })
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = name.trim_start_matches('.').chars().take(120).collect();
    let name = name.trim();
    format!(
        "Books/{}.md",
        if name.is_empty() {
            book.id.as_str()
        } else {
            name
        }
    )
}

/// A YAML double-quoted scalar (JSON strings are valid YAML).
fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}
/// Keep generated text from opening or closing HTML comments (and markers).
fn inert(s: &str) -> String {
    s.replace("<!--", "&lt;!--").replace("-->", "--&gt;")
}
fn location(h: &Highlight) -> String {
    let mut parts = Vec::new();
    if let Some(l) = &h.location {
        parts.push(if l.start == l.end {
            format!("Kindle location {}", l.start)
        } else {
            format!("Kindle location {}–{}", l.start, l.end)
        });
    }
    if let Some(p) = &h.page {
        parts.push(format!("page {}", inert(p)));
    }
    if parts.is_empty() {
        "location unknown".into()
    } else {
        parts.join(" · ")
    }
}

pub fn render_book_note(book: &Book, highlights: &[Highlight]) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("lucina_book_id: {}\n", book.id));
    out.push_str(&format!("title: {}\n", quoted(&book.title)));
    if book.authors.is_empty() {
        out.push_str("authors: []\n");
    } else {
        out.push_str("authors:\n");
        for a in &book.authors {
            out.push_str(&format!("  - {}\n", quoted(a)));
        }
    }
    let status = serde_json::to_value(book.status).unwrap_or_default();
    out.push_str(&format!(
        "status: {}\n",
        status.as_str().unwrap_or("unknown")
    ));
    let mut sources: Vec<&str> = book.sources.iter().map(|s| s.kind.as_str()).collect();
    sources.sort();
    sources.dedup();
    if sources.is_empty() {
        out.push_str("source: []\n");
    } else {
        out.push_str("source:\n");
        for s in sources {
            out.push_str(&format!("  - {}\n", quoted(s)));
        }
    }
    if let Some(p) = &book.progress {
        if let Some(pct) = p.percent {
            out.push_str(&format!("progress: {pct}\n"));
        }
        if let Some(l) = p.location {
            out.push_str(&format!("progress_location: {l}\n"));
        }
    }
    out.push_str(&format!("generated_by: {GENERATED_BY}\n---\n\n"));
    out.push_str(&format!("# {}\n\n", inert(&book.title)));
    out.push_str(BEGIN);
    out.push_str("\n## Highlights\n\n");
    let mut hs: Vec<&Highlight> = highlights
        .iter()
        .filter(|h| h.kind != HighlightKind::Bookmark)
        .collect();
    hs.sort_by_key(|h| {
        (
            h.location.as_ref().map(|l| l.start).unwrap_or(u32::MAX),
            h.id.clone(),
        )
    });
    if hs.is_empty() {
        out.push_str("_No highlights yet._\n");
    }
    for h in hs {
        if h.kind == HighlightKind::Note {
            out.push_str(&format!(
                "- Note ({}): {}\n\n",
                location(h),
                inert(&h.text)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            continue;
        }
        for l in inert(&h.text).lines() {
            out.push_str(&format!("> {l}\n"));
        }
        out.push_str(&format!("> — {}\n", location(h)));
        if let Some(n) = &h.note {
            out.push_str(&format!(
                "\nNote: {}\n",
                inert(n).split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
        out.push('\n');
    }
    out.push_str(END);
    out.push('\n');
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritePlan {
    /// The file does not exist: write `new` as is.
    Create(String),
    /// Our generated file: only the marked region changed.
    Update(String),
    Unchanged,
    /// Owner-authored or unexpected content; never written without approval.
    NeedsApproval(String),
}

fn region(s: &str) -> Option<(usize, usize)> {
    if s.matches(BEGIN).count() != 1 || s.matches(END).count() != 1 {
        return None;
    }
    let b = s.find(BEGIN)?;
    let e = s.find(END)?;
    (b < e).then_some((b, e + END.len()))
}
fn generated_id(s: &str) -> Option<String> {
    let fm = split_frontmatter(s).0?;
    (fm.scalar("generated_by") == Some(GENERATED_BY))
        .then(|| fm.scalar("lucina_book_id").unwrap_or("").to_owned())
}

pub fn plan_write(existing: Option<&str>, new: &str) -> WritePlan {
    let Some((nb, ne)) = region(new) else {
        return WritePlan::NeedsApproval("new content lacks one generated region".into());
    };
    let Some(old) = existing else {
        return WritePlan::Create(new.to_owned());
    };
    let Some(old_id) = generated_id(old) else {
        return WritePlan::NeedsApproval("existing note was not generated by lucina-ink".into());
    };
    if generated_id(new).is_some_and(|id| id != old_id) {
        return WritePlan::NeedsApproval("existing note belongs to another book".into());
    }
    let Some((ob, oe)) = region(old) else {
        return WritePlan::NeedsApproval("existing note has no single generated region".into());
    };
    let merged = format!("{}{}{}", &old[..ob], &new[nb..ne], &old[oe..]);
    if merged == old {
        WritePlan::Unchanged
    } else {
        WritePlan::Update(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_library::model::*;

    fn book() -> Book {
        serde_json::from_value(serde_json::json!({
            "id": "bk_0123456789abcdef", "title": "A/B: \"Quoted\" Book", "authors": ["Ann Writer"],
            "isbn": null, "doi": null, "asin": null, "fingerprints": [], "formats": [],
            "sources": [{"kind": "kindle-usb", "device": null}, {"kind": "kindle-clippings", "device": null}],
            "drm": "unknown", "owned": true, "onKindle": null, "removedFromKindleAt": null,
            "mentionedOnly": false, "status": "reading", "statusSource": "owner",
            "progress": {"percent": 42.0, "location": 1200, "page": null, "source": "owner", "at": "x"},
            "lastOpenedAt": null, "tags": [], "briefs": [], "relatedNotes": [], "memoryRefs": [],
            "createdAt": "x", "updatedAt": "x"
        }))
        .unwrap()
    }
    fn hl(id: &str, text: &str, start: u32) -> Highlight {
        Highlight {
            id: id.into(),
            book_id: "bk_0123456789abcdef".into(),
            kind: HighlightKind::Highlight,
            text: text.into(),
            note: None,
            location: Some(LocRange {
                start,
                end: start + 2,
            }),
            page: Some("12".into()),
            added_at: None,
            added_at_raw: String::new(),
            source: "kindle-clippings".into(),
            provenance: Provenance {
                file: "documents/My Clippings.txt".into(),
                entry_index: 0,
                device: None,
            },
            dedup_key: String::new(),
        }
    }
    #[test]
    fn renders_frontmatter_and_highlights() {
        let b = book();
        assert_eq!(book_note_path(&b), "Books/A B Quoted Book.md");
        let note = render_book_note(
            &b,
            &[
                hl("hl_2", "Second", 300),
                hl("hl_1", "First <!-- lucina-ink:end -->", 170),
            ],
        );
        let fm = split_frontmatter(&note).0.unwrap();
        assert_eq!(fm.scalar("lucina_book_id"), Some("bk_0123456789abcdef"));
        assert_eq!(fm.scalar("title"), Some("A/B: \"Quoted\" Book"));
        assert_eq!(fm.get("authors").unwrap().values(), vec!["Ann Writer"]);
        assert_eq!(fm.scalar("status"), Some("reading"));
        assert_eq!(
            fm.get("source").unwrap().values(),
            vec!["kindle-clippings", "kindle-usb"]
        );
        assert_eq!(fm.scalar("progress"), Some("42"));
        assert_eq!(fm.scalar("generated_by"), Some("lucina-ink"));
        assert!(note.find("First").unwrap() < note.find("Second").unwrap());
        assert!(note.contains("> — Kindle location 170–172 · page 12"));
        assert_eq!(note.matches(END).count(), 1);
        assert!(region(&note).is_some());
    }
    #[test]
    fn write_plans_never_overwrite_owner_content() {
        let b = book();
        let v1 = render_book_note(&b, &[hl("hl_1", "First", 170)]);
        assert_eq!(plan_write(None, &v1), WritePlan::Create(v1.clone()));
        assert_eq!(plan_write(Some(&v1), &v1), WritePlan::Unchanged);
        let owned = format!("{v1}\n## My thoughts\nKeep me.\n").replace("# A/B", "# Renamed");
        let v2 = render_book_note(&b, &[hl("hl_1", "First", 170), hl("hl_2", "Second", 300)]);
        let WritePlan::Update(merged) = plan_write(Some(&owned), &v2) else {
            panic!("expected update")
        };
        assert!(
            merged.contains("Second")
                && merged.contains("Keep me.")
                && merged.contains("# Renamed")
        );
        for existing in [
            "# My own note about this book\n".to_owned(),
            v1.replace("generated_by: lucina-ink", "generated_by: me"),
            v1.replace(END, ""),
            format!("{v1}{BEGIN}"),
            v1.replace("bk_0123456789abcdef", "bk_ffffffffffffffff"),
        ] {
            assert!(
                matches!(
                    plan_write(Some(&existing), &v2),
                    WritePlan::NeedsApproval(_)
                ),
                "{existing}"
            );
        }
        assert!(matches!(
            plan_write(None, "no markers"),
            WritePlan::NeedsApproval(_)
        ));
    }
}
