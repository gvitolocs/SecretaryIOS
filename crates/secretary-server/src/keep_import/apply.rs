//! Apply a scanned Takeout to the note store: attachments first (one file
//! in memory at a time), then each note through `NoteStore::import_note`,
//! which decides new / updated / unchanged / kept / skipped. In a dry run
//! nothing is written, but the report still counts what would be.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::report::SAMPLE_LEN;
use super::takeout::{decide_mime, AttachmentRef, ParsedNote};
use super::{ImportReport, ReportError, SampleNote, Takeout};
use crate::notes::{
    ext_for_mime, Annotation, Attachment, Caller, ImportOutcome, ImportedNote, NoteStore,
};

fn api_msg(e: crate::error::ApiError) -> String {
    format!("{} ({})", e.message, e.code)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn base_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// Read one attachment, decide its type and store it (a dry run only
/// describes it). Counts the file as new when no note of this import and no
/// stored file has these bytes yet.
fn materialize(
    t: &mut Takeout,
    store: &NoteStore,
    a: &AttachmentRef,
    created_at: &str,
    dry_run: bool,
    seen_shas: &mut HashSet<String>,
    rep: &mut ImportReport,
) -> Result<Attachment, String> {
    let bytes = t.read_attachment(a)?;
    let mime =
        decide_mime(&a.declared_mime, &bytes).map_err(|e| format!("{}: {e}", a.file_path))?;
    let ext =
        ext_for_mime(mime).ok_or_else(|| format!("{}: no extension for {mime}", a.file_path))?;
    let sha = hex(&Sha256::digest(&bytes));
    let is_new = !seen_shas.contains(&sha) && !store.has_attachment(&sha, ext);
    seen_shas.insert(sha.clone());
    let name = base_name(&a.file_path);
    let att = if dry_run {
        Attachment {
            file: format!("{sha}.{ext}"),
            id: sha,
            mimetype: mime.to_owned(),
            size: bytes.len() as u64,
            name: Some(name.to_owned()),
            created_at: created_at.to_owned(),
        }
    } else {
        store
            .put_attachment(&bytes, mime, Some(name))
            .map_err(|e| format!("{}: {}", a.file_path, api_msg(e)))?
    };
    if is_new {
        rep.attachments.new += 1;
        rep.attachments.bytes += bytes.len() as u64;
    }
    Ok(att)
}

fn imported(n: &ParsedNote, attachments: Vec<Attachment>) -> ImportedNote {
    ImportedNote {
        key: n.key.clone(),
        edited_usec: n.edited_usec,
        title: n.title.clone(),
        content: n.content.clone(),
        items: n.items.clone(),
        color: n.color.clone(),
        pinned: n.pinned,
        archived: n.archived,
        trashed: n.trashed,
        labels: n.labels.clone(),
        attachments,
        annotations: n
            .annotations
            .iter()
            .map(|a| Annotation {
                url: a.url.clone(),
                title: a.title.clone(),
                description: a.description.clone(),
                source: a.source.clone(),
            })
            .collect(),
        collaborators: n.collaborators.clone(),
        created_at: n.created_at.clone(),
        updated_at: n.updated_at.clone(),
    }
}

/// Import (or, with `dry_run`, only check) Takeout zips into `store`.
/// `Err` only when a zip cannot be opened or the label registry cannot be
/// read; everything per note ends up in the report's `errors`.
pub fn import_takeout(
    store: &NoteStore,
    paths: &[PathBuf],
    dry_run: bool,
    caller: Caller,
) -> Result<ImportReport, String> {
    let mut t = Takeout::open(paths)?;
    let scan = t.scan();
    let mut rep = ImportReport {
        dry_run,
        files: scan.files.clone(),
        errors: scan.errors.clone(),
        ..Default::default()
    };
    rep.notes.total = scan.notes.len();

    // Labels: Labels.txt first, then the notes', each once.
    let mut label_names: Vec<String> = Vec::new();
    let mut seen_labels = HashSet::new();
    for name in scan
        .label_names
        .iter()
        .chain(scan.notes.iter().flat_map(|n| n.labels.iter()))
    {
        if seen_labels.insert(name.to_lowercase()) {
            label_names.push(name.clone());
        }
    }
    rep.labels.total = label_names.len();
    rep.labels.new = store
        .ensure_labels(&label_names, dry_run)
        .map_err(api_msg)?;

    let mut seen_shas = HashSet::new();
    let mut cache: HashMap<usize, Result<Attachment, String>> = HashMap::new();
    for pn in &scan.notes {
        rep.notes.trashed += usize::from(pn.trashed);
        rep.notes.archived += usize::from(pn.archived);
        rep.notes.pinned += usize::from(pn.pinned);
        rep.notes.lists += usize::from(pn.items.is_some());

        let mut atts: Vec<Attachment> = Vec::new();
        for a in &pn.attachments {
            let Some(e) = a.entry else {
                rep.attachments.missing.push(a.file_path.clone());
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(slot) = cache.entry(e) {
                slot.insert(materialize(
                    &mut t,
                    store,
                    a,
                    &pn.created_at,
                    dry_run,
                    &mut seen_shas,
                    &mut rep,
                ));
            }
            match &cache[&e] {
                Ok(att) => {
                    rep.attachments.total += 1;
                    atts.push(att.clone());
                }
                Err(msg) => rep.errors.push(ReportError {
                    file: pn.file.clone(),
                    error: format!("attachment {msg}"),
                }),
            }
        }

        let nothing_left = pn.title.is_empty()
            && pn.content.trim().is_empty()
            && pn.items.as_ref().is_none_or(Vec::is_empty)
            && atts.is_empty();
        if nothing_left {
            rep.errors.push(ReportError {
                file: pn.file.clone(),
                error: "nothing left to import after its attachments failed, skipped".into(),
            });
            continue;
        }

        match store.import_note(imported(pn, atts), dry_run, caller) {
            Ok(o) => match o {
                ImportOutcome::New => {
                    rep.notes.new += 1;
                    if rep.sample.len() < SAMPLE_LEN {
                        rep.sample.push(SampleNote {
                            title: pn.title.clone(),
                            labels: pn.labels.clone(),
                            color: pn.color.clone(),
                            kind: pn.kind().to_owned(),
                        });
                    }
                }
                ImportOutcome::Updated => rep.notes.updated += 1,
                ImportOutcome::Unchanged => rep.notes.unchanged += 1,
                ImportOutcome::KeptLocalEdits => rep.notes.kept_local_edits += 1,
                ImportOutcome::SkippedDeleted => rep.notes.skipped_deleted += 1,
            },
            Err(e) => rep.errors.push(ReportError {
                file: pn.file.clone(),
                error: api_msg(e),
            }),
        }
    }
    Ok(rep)
}
