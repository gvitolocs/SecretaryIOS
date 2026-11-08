//! Incremental index. A manifest of `path → (mtime, size, sha256)` decides
//! which notes are read and parsed again; links are re-resolved and book
//! references recombined on every run. A library change re-reads every note,
//! because mentions depend on the library.
use crate::{
    books::{finalize, note_refs, BookRef, How, LibraryIndex},
    config::VaultConfig,
    parse::{parse_note, LinkKind, RawLink},
    resolve::{Resolution, Resolver},
    walk::{read_note, walk, Read},
    Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub mtime_ns: u64,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRecord {
    pub manifest: ManifestEntry,
    pub title: String,
    pub tags: Vec<String>,
    pub links: Vec<RawLink>,
    /// References made by the note itself (before cross-note wikilinks).
    pub refs: Vec<BookRef>,
    /// Books this note is about (generated book notes, `title` + author notes).
    pub about: Vec<String>,
}

/// Persistable index state. It holds titles, links and excerpts, so it is
/// as private as the vault and must live outside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexState {
    pub library_fingerprint: String,
    pub notes: BTreeMap<String, NoteRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub notes: usize,
    pub excluded_dirs: usize,
    pub excluded_files: usize,
    pub skipped: usize,
    pub attachments: usize,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Notes read and parsed in this run.
    pub reparsed: usize,
    pub links: usize,
    pub wikilinks: usize,
    pub embeds: usize,
    pub markdown_links: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub ambiguous: usize,
    pub tags: usize,
    pub book_notes: usize,
    pub refs: usize,
    pub refs_by_how: BTreeMap<String, usize>,
    pub books_referenced: usize,
    pub notes_with_refs: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub counts: Counts,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
    pub refs: Vec<BookRef>,
}

fn sha256(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

/// Index from scratch.
pub fn index(cfg: &VaultConfig, lib: &LibraryIndex) -> Result<Report> {
    update(&mut IndexState::default(), cfg, lib)
}

/// Bring `state` up to date with the vault and return the full reference set.
pub fn update(state: &mut IndexState, cfg: &VaultConfig, lib: &LibraryIndex) -> Result<Report> {
    let w = walk(cfg)?;
    let mut r = Report {
        warnings: w.warnings.clone(),
        ..Default::default()
    };
    let lib_changed = state.library_fingerprint != lib.fingerprint;
    let mut next: BTreeMap<String, NoteRecord> = BTreeMap::new();
    for e in &w.notes {
        let old = state.notes.remove(&e.rel);
        if let Some(old) = old.as_ref().filter(|o| {
            !lib_changed && o.manifest.mtime_ns == e.mtime_ns && o.manifest.size == e.size
        }) {
            next.insert(e.rel.clone(), old.clone());
            r.counts.unchanged += 1;
            continue;
        }
        let text = match read_note(&w.root, &e.rel, cfg.max_file_bytes)? {
            Read::Text(t) => t,
            Read::Skipped(why) => {
                r.counts.skipped += 1;
                r.warnings.push(why);
                if old.is_some() {
                    r.removed.push(e.rel.clone());
                }
                continue;
            }
        };
        let manifest = ManifestEntry {
            mtime_ns: e.mtime_ns,
            size: text.len() as u64,
            sha256: sha256(&text),
        };
        match &old {
            Some(o) if o.manifest.sha256 == manifest.sha256 && !lib_changed => {
                let mut o = o.clone();
                o.manifest = manifest;
                next.insert(e.rel.clone(), o);
                r.counts.unchanged += 1;
                continue;
            }
            Some(o) if o.manifest.sha256 == manifest.sha256 => r.counts.unchanged += 1,
            Some(_) => r.changed.push(e.rel.clone()),
            None => r.added.push(e.rel.clone()),
        }
        let note = parse_note(&e.rel, &text);
        let (refs, about) = note_refs(lib, &e.rel, &note);
        r.counts.reparsed += 1;
        next.insert(
            e.rel.clone(),
            NoteRecord {
                manifest,
                title: note.title,
                tags: note.tags,
                links: note.links,
                refs,
                about,
            },
        );
    }
    r.removed
        .extend(std::mem::take(&mut state.notes).into_keys());
    r.removed.sort();
    state.notes = next;
    state.library_fingerprint = lib.fingerprint.clone();

    let resolver = Resolver::new(
        state
            .notes
            .keys()
            .map(String::as_str)
            .chain(w.attachments.iter().map(String::as_str)),
    );
    let mut refs = Vec::new();
    let mut tags = BTreeSet::new();
    let c = &mut r.counts;
    for (path, n) in &state.notes {
        tags.extend(n.tags.iter().cloned());
        refs.extend(n.refs.iter().cloned());
        c.book_notes += usize::from(!n.about.is_empty());
        for l in &n.links {
            c.links += 1;
            match l.kind {
                LinkKind::Wiki => c.wikilinks += 1,
                LinkKind::Embed => c.embeds += 1,
                LinkKind::Markdown => c.markdown_links += 1,
            }
            match resolver.resolve(path, l) {
                Resolution::Resolved(target) => {
                    c.resolved += 1;
                    if target == *path {
                        continue;
                    }
                    // A link to a note about a book refers to that book.
                    for id in state
                        .notes
                        .get(&target)
                        .map(|t| t.about.as_slice())
                        .unwrap_or(&[])
                    {
                        refs.push(BookRef {
                            book_id: id.clone(),
                            note_path: path.clone(),
                            note_title: n.title.clone(),
                            how: How::Wikilink,
                            excerpt: crate::books::excerpt(&l.raw, 0, l.raw.len()),
                            line: l.line,
                        });
                    }
                }
                Resolution::Unresolved => c.unresolved += 1,
                Resolution::Ambiguous(_) => c.ambiguous += 1,
            }
        }
    }
    r.refs = finalize(refs);
    c.notes = state.notes.len();
    c.excluded_dirs = w.excluded_dirs;
    c.excluded_files = w.excluded_files;
    c.skipped += w.skipped;
    c.attachments = w.attachments.len();
    c.added = r.added.len();
    c.changed = r.changed.len();
    c.removed = r.removed.len();
    c.tags = tags.len();
    c.refs = r.refs.len();
    for x in &r.refs {
        *c.refs_by_how.entry(x.how.as_str().to_owned()).or_default() += 1;
    }
    c.books_referenced = r
        .refs
        .iter()
        .map(|x| &x.book_id)
        .collect::<BTreeSet<_>>()
        .len();
    c.notes_with_refs = r
        .refs
        .iter()
        .map(|x| &x.note_path)
        .collect::<BTreeSet<_>>()
        .len();
    Ok(r)
}
