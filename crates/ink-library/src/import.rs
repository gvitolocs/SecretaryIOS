//! Pure, additive import merge. Existing identities and owner edits are durable.
use crate::{
    clippings::{self, ClippingEntry},
    model::*,
    search::{author_key, book_key, title_key},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScannedBook {
    #[serde(default)]
    pub kind: BookKind,
    pub path: String,
    pub size_bytes: u64,
    pub fingerprint: String,
    pub format: Format,
    pub metadata: Metadata,
    #[serde(default)]
    pub sidecar_present: bool,
    pub sidecar_extensions: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportBatch {
    pub source: String,
    pub device: Option<String>,
    pub scanned_at: String,
    pub books: Vec<ScannedBook>,
    pub clippings: Vec<ClippingEntry>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub books_added: usize,
    pub books_updated: usize,
    pub books_unchanged: usize,
    pub highlights_added: usize,
    pub highlights_skipped: usize,
    pub removed_from_kindle: usize,
    pub conflicts: Vec<String>,
    pub warnings: Vec<String>,
}
pub fn stable_id(prefix: &str, key: &str) -> String {
    format!(
        "{prefix}{}",
        &format!("{:x}", Sha256::digest(key.as_bytes()))[..16]
    )
}
fn isbn(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}
fn identity(s: &ScannedBook) -> String {
    if !s.fingerprint.is_empty() {
        format!("sha256:{}", s.fingerprint)
    } else if let Some(i) = &s.metadata.isbn {
        format!("isbn:{}", isbn(i))
    } else if let Some(a) = &s.metadata.asin {
        format!("asin:{}", a.to_uppercase())
    } else {
        format!(
            "title:{}",
            book_key(
                s.metadata.title.as_deref().unwrap_or("Untitled"),
                &s.metadata.authors
            )
        )
    }
}
/// A re-scan refreshes when the file was last seen on the Kindle; that alone is not a change, so it
/// neither bumps `updatedAt` nor counts the book as updated (the new time is still kept).
fn changed(old: &Book, new: &Book) -> bool {
    match (&old.on_kindle, &new.on_kindle) {
        (Some(o), Some(n)) if o.last_seen_at != n.last_seen_at => {
            let mut same_seen = new.clone();
            if let Some(p) = same_seen.on_kindle.as_mut() {
                p.last_seen_at = o.last_seen_at.clone();
            }
            *old != same_seen
        }
        _ => old != new,
    }
}

fn new_book(id: String, title: String, authors: Vec<String>, now: &str) -> Book {
    Book {
        kind: BookKind::Book,
        id,
        title,
        authors,
        isbn: None,
        doi: None,
        asin: None,
        fingerprints: vec![],
        formats: vec![],
        sources: vec![],
        drm: DrmState::Unknown,
        owned: false,
        on_kindle: None,
        removed_from_kindle_at: None,
        mentioned_only: false,
        status: ReadingStatus::Unknown,
        status_source: None,
        progress: None,
        last_opened_at: None,
        tags: vec![],
        briefs: vec![],
        related_notes: vec![],
        memory_refs: vec![],
        created_at: now.into(),
        updated_at: now.into(),
    }
}
fn add<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
}
fn metadata_value(
    existing: &mut Option<String>,
    incoming: &Option<String>,
    field: &str,
    id: &str,
    o: &mut ImportOutcome,
) {
    if let Some(v) = incoming {
        match existing {
            Some(old) if old != v => o
                .conflicts
                .push(format!("{id}: conflicting {field}; kept existing")),
            None => *existing = Some(v.clone()),
            _ => {}
        }
    }
}
#[derive(Default)]
struct IdentityIndex {
    fingerprints: BTreeMap<String, String>,
    isbns: BTreeMap<String, String>,
    asins: BTreeMap<String, String>,
    titles: BTreeMap<String, BTreeSet<String>>,
}
impl IdentityIndex {
    fn insert(&mut self, b: &Book) {
        for f in &b.fingerprints {
            self.fingerprints
                .entry(f.clone())
                .or_insert_with(|| b.id.clone());
        }
        if let Some(i) = &b.isbn {
            self.isbns.entry(isbn(i)).or_insert_with(|| b.id.clone());
        }
        if let Some(a) = &b.asin {
            self.asins
                .entry(a.to_uppercase())
                .or_insert_with(|| b.id.clone());
        }
        self.titles
            .entry(title_key(&b.title))
            .or_default()
            .insert(b.id.clone());
    }
}
/// Only an unambiguous author match or a unique title candidate is usable.
fn title_match(
    index: &IdentityIndex,
    state: &LibraryState,
    title: &str,
    authors: &[String],
    stub_only: bool,
) -> Option<String> {
    let key = title_key(title);
    if key.is_empty() {
        return None;
    }
    let candidates: Vec<_> = index
        .titles
        .get(&key)?
        .iter()
        .filter_map(|id| state.books.get(id))
        .filter(|b| !stub_only || b.fingerprints.is_empty())
        .collect();
    let author = author_key(authors);
    let exact: Vec<_> = candidates
        .iter()
        .filter(|b| author_key(&b.authors) == author)
        .collect();
    if exact.len() == 1 {
        Some(exact[0].id.clone())
    } else if exact.is_empty() && candidates.len() == 1 {
        Some(candidates[0].id.clone())
    } else {
        None
    }
}
fn not_older(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a >= b,
        _ => a >= b,
    }
}
pub fn merge(
    existing: &LibraryState,
    batch: &ImportBatch,
    now: &str,
) -> (LibraryState, ImportOutcome) {
    let mut state = existing.clone();
    let mut outcome = ImportOutcome::default();
    let mut seen = BTreeSet::new();
    let mut index = IdentityIndex::default();
    for b in state.books.values() {
        index.insert(b);
    }
    // Sort incoming files so conflicts within a batch are deterministic.
    let mut files: Vec<_> = batch.books.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path).then(a.fingerprint.cmp(&b.fingerprint)));
    for s in files {
        let title = s.metadata.title.as_deref().unwrap_or("Untitled");
        let found = index
            .fingerprints
            .get(&s.fingerprint)
            .or_else(|| {
                s.metadata
                    .isbn
                    .as_ref()
                    .and_then(|i| index.isbns.get(&isbn(i)))
            })
            .or_else(|| {
                s.metadata
                    .asin
                    .as_ref()
                    .and_then(|a| index.asins.get(&a.to_uppercase()))
            })
            .cloned()
            .or_else(|| title_match(&index, &state, title, &s.metadata.authors, true));
        let id = found.unwrap_or_else(|| stable_id("bk_", &identity(s)));
        seen.insert(id.clone());
        let b = state
            .books
            .entry(id.clone())
            .or_insert_with(|| new_book(id.clone(), title.into(), s.metadata.authors.clone(), now));
        b.kind = s.kind;
        metadata_value(&mut b.isbn, &s.metadata.isbn, "ISBN", &id, &mut outcome);
        metadata_value(&mut b.asin, &s.metadata.asin, "ASIN", &id, &mut outcome);
        if b.authors.is_empty() {
            b.authors = s.metadata.authors.clone();
        }
        if !s.fingerprint.is_empty() {
            add(&mut b.fingerprints, s.fingerprint.clone());
        }
        add(&mut b.formats, s.format);
        add(
            &mut b.sources,
            Source {
                kind: batch.source.clone(),
                device: batch.device.clone(),
            },
        );
        if b.drm == DrmState::Unknown {
            b.drm = s.metadata.drm;
        } else if s.metadata.drm != DrmState::Unknown && b.drm != s.metadata.drm {
            outcome
                .conflicts
                .push(format!("{id}: conflicting DRM state; kept existing"));
        }
        b.owned = true;
        b.mentioned_only = false;
        if batch.source == "kindle-usb" {
            let presence = KindlePresence {
                path: s.path.clone(),
                size_bytes: s.size_bytes,
                last_seen_at: batch.scanned_at.clone(),
            };
            // Old saved batches cannot move lastSeen backwards.
            if b.on_kindle
                .as_ref()
                .is_none_or(|p| not_older(&presence.last_seen_at, &p.last_seen_at))
                && b.removed_from_kindle_at
                    .as_ref()
                    .is_none_or(|t| not_older(&presence.last_seen_at, t))
            {
                b.on_kindle = Some(presence);
                b.removed_from_kindle_at = None;
            }
        }
        index.insert(b);
    }
    // Lossy title keys are never evidence that two files are the same book.
    for ids in index.titles.values() {
        let files: Vec<_> = ids
            .iter()
            .filter(|id| {
                state
                    .books
                    .get(*id)
                    .is_some_and(|b| !b.fingerprints.is_empty())
            })
            .collect();
        if files.len() > 1 {
            for id in files {
                outcome.conflicts.push(format!(
                    "{id}: title key collision between distinct files; kept separate"
                ));
            }
        }
    }
    let mut clips = batch.clippings.clone();
    clippings::attach_notes(&mut clips);
    clips.sort_by_key(|e| clippings::dedup_key(&e.title, &e.authors, &e.highlight));
    let mut highlight_index: BTreeMap<String, BTreeMap<String, usize>> = state
        .highlights
        .iter()
        .map(|(id, hs)| {
            (
                id.clone(),
                hs.iter()
                    .enumerate()
                    .map(|(i, h)| (h.dedup_key.clone(), i))
                    .collect(),
            )
        })
        .collect();
    for e in clips {
        let key = book_key(&e.title, &e.authors);
        let id = title_match(&index, &state, &e.title, &e.authors, false)
            .unwrap_or_else(|| stable_id("bk_", &format!("title:{key}")));
        let b = state
            .books
            .entry(id.clone())
            .or_insert_with(|| new_book(id.clone(), e.title.clone(), e.authors.clone(), now));
        // KFX files expose no author; the clippings name one for the same book.
        if b.authors.is_empty() && !e.authors.is_empty() {
            b.authors = e.authors.clone();
        }
        add(
            &mut b.sources,
            Source {
                kind: "kindle-clippings".into(),
                device: batch.device.clone(),
            },
        );
        index.insert(b);
        let mut h = e.highlight;
        h.book_id = id.clone();
        h.dedup_key = clippings::dedup_key(&e.title, &e.authors, &h);
        h.id = stable_id("hl_", &h.dedup_key);
        let highlights = state.highlights.entry(id).or_default();
        let positions = highlight_index.entry(h.book_id.clone()).or_default();
        if let Some(old) = positions
            .get(&h.dedup_key)
            .and_then(|i| highlights.get_mut(*i))
        {
            if old.note.is_none() {
                old.note = h.note;
            }
            outcome.highlights_skipped += 1;
        } else {
            positions.insert(h.dedup_key.clone(), highlights.len());
            highlights.push(h);
            outcome.highlights_added += 1;
        }
    }
    // Also attach notes imported separately from their highlights.
    for hs in state.highlights.values_mut() {
        let mut notes: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
        for h in hs.iter().filter(|h| h.kind == HighlightKind::Note) {
            if let Some(l) = &h.location {
                notes.entry(l.end).or_default().insert(h.text.clone());
            }
        }
        for h in hs.iter_mut() {
            if h.kind == HighlightKind::Highlight {
                if let Some(n) = h.location.as_ref().and_then(|l| notes.get(&l.end)) {
                    h.note = Some(n.iter().cloned().collect::<Vec<_>>().join("\n"));
                }
            }
        }
        hs.sort_by(|a, b| a.id.cmp(&b.id));
    }
    if batch.source == "kindle-usb" {
        for b in state.books.values_mut() {
            if !seen.contains(&b.id)
                && b.on_kindle
                    .as_ref()
                    .is_some_and(|p| not_older(&batch.scanned_at, &p.last_seen_at))
            {
                b.on_kindle = None;
                b.removed_from_kindle_at = Some(batch.scanned_at.clone());
                outcome.removed_from_kindle += 1;
            }
        }
    }
    for b in state.books.values_mut() {
        match existing.books.get(&b.id) {
            None => outcome.books_added += 1,
            Some(old) => {
                if changed(old, b) || existing.highlights.get(&b.id) != state.highlights.get(&b.id)
                {
                    b.updated_at = now.into();
                    outcome.books_updated += 1;
                } else {
                    outcome.books_unchanged += 1;
                }
            }
        }
    }
    outcome.conflicts.sort();
    outcome.conflicts.dedup();
    (state, outcome)
}
