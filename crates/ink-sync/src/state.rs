//! Server-side state and the pure, idempotent, commutative `apply`.
//!
//! Conflict rules (deterministic, documented in `docs/server-api.md`, Library
//! sync):
//!
//! - **Idempotency.** An op id already applied (per-device watermark) is a
//!   no-op (`duplicate`). Every op is applied at most once, whatever its
//!   verdict (a rejected op is recorded too, so it is acked and never retried).
//! - **Progress.** Last writer wins by [`Stamp`] `(at, device, counter)`, a
//!   total order. An owner edit (`PATCH`) sets a floor: device progress stamped
//!   at or before it loses.
//! - **Status.** Last writer wins by `(status == completed, stamp)`: among
//!   device ops, `completed` is sticky (a device never moves a book back from
//!   completed). An owner edit sets a floor like progress; an owner
//!   `completed` freezes the status against every device op (`protected`).
//!   Only the owner moves a book back from completed.
//! - **Highlights.** A grow-only set keyed by `bookId|kind|anchor|text` (the
//!   dedup key). The earliest stamp names the creation time. Notes are a
//!   last-writer-wins register per highlight id, fed by `addHighlight` with a
//!   note and by `editNote`.
//! - **Deletions** are tombstones: recorded, never applied silently. Only an
//!   owner confirmation hides the highlight (and keeps it hidden: a confirmed
//!   deletion is a remove-wins set).
//! - **Sessions.** Start is a min register, end a max register, by stamp.
//! - **Conflicts** (an op that lost, was protected or was rejected) are
//!   reported per request and kept in a bounded audit log. The log is
//!   diagnostics: convergence ([`SyncState::content`]) covers everything else.
//!
//! Every register is a join (max/min/union) over a total order, so `apply`
//! commutes for ops with distinct ids, and a server's content is a function of
//! the set of ops it applied (plus the owner's edits).
use crate::op::{Op, OpId, OpKind, Stamp};
use crate::watermark::Watermark;
use ink_library::import::stable_id;
use ink_library::model::{
    Highlight, HighlightKind, LibraryState, Progress, Provenance, ReadingSession, ReadingStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Audit log bound (oldest conflicts drop first).
pub const MAX_CONFLICT_LOG: usize = 1000;
/// `source` of device-created highlights.
pub const DEVICE_SOURCE: &str = "device-sync";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lww<T> {
    pub stamp: Stamp,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressValue {
    pub anchor: Option<String>,
    pub percent: Option<f32>,
    pub location: Option<u32>,
    pub at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookRegs {
    #[serde(default)]
    pub progress: Option<Lww<ProgressValue>>,
    /// Owner progress edit time (ms): device progress at or before it loses.
    #[serde(default)]
    pub progress_floor: Option<i64>,
    #[serde(default)]
    pub status: Option<Lww<ReadingStatus>>,
    #[serde(default)]
    pub status_floor: Option<i64>,
    /// The owner set `completed`: device status ops never apply.
    #[serde(default)]
    pub owner_completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightReg {
    pub book_id: String,
    pub kind: HighlightKind,
    pub text: String,
    pub anchor: String,
    pub dedup_key: String,
    /// The earliest op that added it (min register).
    pub created: Stamp,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteReg {
    pub book_id: String,
    pub note: Lww<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPoint {
    pub book_id: String,
    pub at: String,
    pub location: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReg {
    /// Earliest start (min register).
    pub start: Option<Lww<SessionPoint>>,
    /// Latest end (max register).
    pub end: Option<Lww<SessionPoint>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    pub op_id: OpId,
    pub book_id: String,
    pub highlight_id: String,
    pub at: String,
    /// Set only by the owner; until then nothing is hidden.
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// Fresh op; it changed (or joined into) the content.
    Applied,
    /// Already applied (same op id): nothing changed.
    Duplicate,
    /// Lost to a newer value (or to an owner edit after it).
    Superseded,
    /// Blocked by the owner-status rule.
    Protected,
    /// A deletion request, recorded as an unconfirmed tombstone.
    Recorded,
    /// Invalid payload; recorded as applied so it is not retried.
    Rejected,
    /// Not recorded (the device's out-of-order window is full): not acked,
    /// the client retries it.
    Deferred,
}

impl Verdict {
    pub fn acked(self) -> bool {
        self != Verdict::Deferred
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub op_id: OpId,
    pub book_id: String,
    pub op_type: String,
    /// `superseded | protected | rejected | deleted`
    pub kind: String,
    /// The op whose value won, when one did.
    pub winner: Option<OpId>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub verdict: Verdict,
    pub conflict: Option<Conflict>,
}

/// The server's sync state, stored next to the library (`library/sync.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    /// Number of ops recorded (fresh ops of any verdict but `deferred`).
    #[serde(default)]
    pub clock: u64,
    #[serde(default)]
    pub devices: BTreeMap<String, Watermark>,
    #[serde(default)]
    pub books: BTreeMap<String, BookRegs>,
    /// Device-created highlights by highlight id.
    #[serde(default)]
    pub highlights: BTreeMap<String, HighlightReg>,
    /// Notes by highlight id (device or Kindle highlights).
    #[serde(default)]
    pub notes: BTreeMap<String, NoteReg>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionReg>,
    /// Deletion requests by op id.
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
    /// Highlight ids whose deletion the owner confirmed (remove wins).
    #[serde(default)]
    pub deleted: BTreeSet<String>,
    /// Audit log, newest last, bounded by [`MAX_CONFLICT_LOG`].
    #[serde(default)]
    pub conflicts: VecDeque<Conflict>,
}

/// Everything but the audit log: what convergence compares.
#[derive(Debug, PartialEq)]
pub struct Content<'a> {
    pub clock: u64,
    pub devices: &'a BTreeMap<String, Watermark>,
    pub books: &'a BTreeMap<String, BookRegs>,
    pub highlights: &'a BTreeMap<String, HighlightReg>,
    pub notes: &'a BTreeMap<String, NoteReg>,
    pub sessions: &'a BTreeMap<String, SessionReg>,
    pub tombstones: &'a BTreeMap<String, Tombstone>,
    pub deleted: &'a BTreeSet<String>,
}

impl SyncState {
    pub fn content(&self) -> Content<'_> {
        Content {
            clock: self.clock,
            devices: &self.devices,
            books: &self.books,
            highlights: &self.highlights,
            notes: &self.notes,
            sessions: &self.sessions,
            tombstones: &self.tombstones,
            deleted: &self.deleted,
        }
    }

    pub fn is_applied(&self, id: &OpId) -> bool {
        self.devices
            .get(&id.device)
            .is_some_and(|w| w.contains(id.counter))
    }

    pub fn pending_tombstones(&self) -> impl Iterator<Item = &Tombstone> {
        self.tombstones.values().filter(|t| !t.confirmed)
    }
}

/// The dedup key of a device highlight. Kindle clippings use title/author
/// keys (`ink_library::clippings::dedup_key`); the `device|` prefix keeps the
/// two key spaces apart.
pub fn highlight_key(book_id: &str, kind: HighlightKind, anchor: &str, text: &str) -> String {
    let kind = match kind {
        HighlightKind::Highlight => "highlight",
        HighlightKind::Note => "note",
        HighlightKind::Bookmark => "bookmark",
    };
    format!(
        "device|{book_id}|{kind}|{anchor}|{}",
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

pub fn highlight_id(key: &str) -> String {
    stable_id("hl_", key)
}

/// Functional form: `apply(state, op) -> (state, outcome)`.
pub fn apply(mut state: SyncState, op: &Op) -> (SyncState, Outcome) {
    let o = apply_mut(&mut state, op);
    (state, o)
}

fn conflict(op: &Op, kind: &str, winner: Option<OpId>, detail: impl Into<String>) -> Conflict {
    Conflict {
        op_id: op.op_id.clone(),
        book_id: op.kind.book_id().to_owned(),
        op_type: op.kind.name().into(),
        kind: kind.into(),
        winner,
        detail: detail.into(),
    }
}

fn outcome(verdict: Verdict, conflict: Option<Conflict>) -> Outcome {
    Outcome { verdict, conflict }
}

pub fn apply_mut(s: &mut SyncState, op: &Op) -> Outcome {
    // The op-id gate: record first, then apply exactly once.
    let wm = s.devices.entry(op.op_id.device.clone()).or_default();
    if wm.contains(op.op_id.counter) {
        return outcome(Verdict::Duplicate, None);
    }
    if wm.insert(op.op_id.counter).is_err() {
        return outcome(Verdict::Deferred, None);
    }
    s.clock += 1;
    let o = match op.validate() {
        Err(e) => outcome(Verdict::Rejected, Some(conflict(op, "rejected", None, e))),
        Ok(at_ms) => {
            let stamp = Stamp {
                at_ms,
                device: op.op_id.device.clone(),
                counter: op.op_id.counter,
            };
            effect(s, op, stamp)
        }
    };
    if let Some(c) = &o.conflict {
        s.conflicts.push_back(c.clone());
        while s.conflicts.len() > MAX_CONFLICT_LOG {
            s.conflicts.pop_front();
        }
    }
    o
}

/// Status order: `completed` first, then the stamp.
fn status_key(r: &Lww<ReadingStatus>) -> (bool, &Stamp) {
    (r.value == ReadingStatus::Completed, &r.stamp)
}

fn effect(s: &mut SyncState, op: &Op, stamp: Stamp) -> Outcome {
    let book = op.kind.book_id().to_owned();
    match &op.kind {
        OpKind::SetProgress {
            anchor,
            percent,
            location,
            ..
        } => {
            let regs = s.books.entry(book).or_default();
            if regs.progress_floor.is_some_and(|f| stamp.at_ms <= f) {
                return outcome(
                    Verdict::Superseded,
                    Some(conflict(
                        op,
                        "superseded",
                        None,
                        "older than the owner's progress edit",
                    )),
                );
            }
            let new = Lww {
                stamp,
                value: ProgressValue {
                    anchor: anchor.clone(),
                    percent: *percent,
                    location: *location,
                    at: op.at.clone(),
                },
            };
            lww_progress(&mut regs.progress, new, op)
        }
        OpKind::SetStatus { status, .. } => {
            let regs = s.books.entry(book).or_default();
            if regs.owner_completed {
                return outcome(
                    Verdict::Protected,
                    Some(conflict(
                        op,
                        "protected",
                        None,
                        "the owner marked this book completed",
                    )),
                );
            }
            if regs.status_floor.is_some_and(|f| stamp.at_ms <= f) {
                return outcome(
                    Verdict::Superseded,
                    Some(conflict(
                        op,
                        "superseded",
                        None,
                        "older than the owner's status edit",
                    )),
                );
            }
            let new = Lww {
                stamp,
                value: *status,
            };
            // A device value from before the owner's edit no longer counts.
            let floor = regs.status_floor;
            let live = regs
                .status
                .as_ref()
                .filter(|c| floor.is_none_or(|f| c.stamp.at_ms > f));
            match live {
                Some(cur) if status_key(cur) > status_key(&new) => {
                    let winner = cur.stamp.op_id();
                    if cur.value == ReadingStatus::Completed
                        && new.value != ReadingStatus::Completed
                    {
                        outcome(
                            Verdict::Protected,
                            Some(conflict(
                                op,
                                "protected",
                                Some(winner),
                                "completed is only reverted by the owner",
                            )),
                        )
                    } else {
                        let c = (cur.stamp.device != new.stamp.device).then(|| {
                            conflict(op, "superseded", Some(winner), "a newer status won")
                        });
                        outcome(Verdict::Superseded, c)
                    }
                }
                cur => {
                    let c = cur
                        .as_ref()
                        .filter(|c| c.stamp.device != new.stamp.device && c.value != new.value)
                        .map(|c| Conflict {
                            op_id: c.stamp.op_id(),
                            winner: Some(op.op_id.clone()),
                            ..conflict(op, "superseded", None, "replaced by a newer status")
                        });
                    regs.status = Some(new);
                    outcome(Verdict::Applied, c)
                }
            }
        }
        OpKind::AddHighlight {
            kind,
            text,
            anchor,
            note,
            ..
        } => {
            let key = highlight_key(&book, *kind, anchor, text);
            let id = highlight_id(&key);
            let reg = HighlightReg {
                book_id: book,
                kind: *kind,
                text: text.clone(),
                anchor: anchor.clone(),
                dedup_key: key,
                created: stamp.clone(),
                created_at: op.at.clone(),
            };
            match s.highlights.get_mut(&id) {
                Some(cur) if cur.created <= stamp => {}
                Some(cur) => *cur = reg,
                None => {
                    s.highlights.insert(id.clone(), reg);
                }
            }
            if note.is_some() {
                note_join(s, &id, op, stamp, note.clone());
            }
            let c = s
                .deleted
                .contains(&id)
                .then(|| conflict(op, "deleted", None, "the owner deleted this highlight"));
            outcome(Verdict::Applied, c)
        }
        OpKind::EditNote {
            highlight_id, note, ..
        } => {
            let o = note_join(s, highlight_id, op, stamp, note.clone());
            if o.verdict == Verdict::Applied && s.deleted.contains(highlight_id) {
                return outcome(
                    Verdict::Applied,
                    Some(conflict(
                        op,
                        "deleted",
                        None,
                        "the owner deleted this highlight",
                    )),
                );
            }
            o
        }
        OpKind::DeleteHighlight { highlight_id, .. } => {
            s.tombstones.insert(
                op.op_id.to_string(),
                Tombstone {
                    op_id: op.op_id.clone(),
                    book_id: book,
                    highlight_id: highlight_id.clone(),
                    at: op.at.clone(),
                    confirmed: false,
                },
            );
            outcome(Verdict::Recorded, None)
        }
        OpKind::StartSession {
            session_id,
            location,
            ..
        } => {
            let reg = s.sessions.entry(session_id.clone()).or_default();
            let new = Lww {
                stamp,
                value: SessionPoint {
                    book_id: book,
                    at: op.at.clone(),
                    location: *location,
                },
            };
            if reg.start.as_ref().is_none_or(|cur| new.stamp < cur.stamp) {
                reg.start = Some(new);
                outcome(Verdict::Applied, None)
            } else {
                outcome(Verdict::Superseded, None)
            }
        }
        OpKind::EndSession {
            session_id,
            location,
            ..
        } => {
            let reg = s.sessions.entry(session_id.clone()).or_default();
            let new = Lww {
                stamp,
                value: SessionPoint {
                    book_id: book,
                    at: op.at.clone(),
                    location: *location,
                },
            };
            if reg.end.as_ref().is_none_or(|cur| new.stamp > cur.stamp) {
                reg.end = Some(new);
                outcome(Verdict::Applied, None)
            } else {
                outcome(Verdict::Superseded, None)
            }
        }
    }
}

fn lww_progress(
    slot: &mut Option<Lww<ProgressValue>>,
    new: Lww<ProgressValue>,
    op: &Op,
) -> Outcome {
    match slot {
        Some(cur) if cur.stamp > new.stamp => {
            let c = (cur.stamp.device != new.stamp.device).then(|| {
                conflict(
                    op,
                    "superseded",
                    Some(cur.stamp.op_id()),
                    "a newer position won",
                )
            });
            outcome(Verdict::Superseded, c)
        }
        _ => {
            let c = slot
                .as_ref()
                .filter(|c| c.stamp.device != new.stamp.device)
                .map(|c| Conflict {
                    op_id: c.stamp.op_id(),
                    winner: Some(op.op_id.clone()),
                    ..conflict(op, "superseded", None, "replaced by a newer position")
                });
            *slot = Some(new);
            outcome(Verdict::Applied, c)
        }
    }
}

fn note_join(
    s: &mut SyncState,
    highlight_id: &str,
    op: &Op,
    stamp: Stamp,
    note: Option<String>,
) -> Outcome {
    let book = op.kind.book_id().to_owned();
    let new = NoteReg {
        book_id: book,
        note: Lww { stamp, value: note },
    };
    match s.notes.get_mut(highlight_id) {
        Some(cur) if cur.note.stamp > new.note.stamp => {
            let c = (cur.note.stamp.device != new.note.stamp.device).then(|| {
                conflict(
                    op,
                    "superseded",
                    Some(cur.note.stamp.op_id()),
                    "a newer note edit won",
                )
            });
            outcome(Verdict::Superseded, c)
        }
        Some(cur) => {
            *cur = new;
            outcome(Verdict::Applied, None)
        }
        None => {
            s.notes.insert(highlight_id.to_owned(), new);
            outcome(Verdict::Applied, None)
        }
    }
}

// ---- owner actions (not ops: they come from authenticated owner edits) -----------

/// The owner set the status (`PATCH`): device status ops at or before `at_ms`
/// lose; `completed` freezes the status against every device op.
pub fn owner_set_status(s: &mut SyncState, book_id: &str, status: ReadingStatus, at_ms: i64) {
    let r = s.books.entry(book_id.to_owned()).or_default();
    r.owner_completed = status == ReadingStatus::Completed;
    r.status_floor = Some(r.status_floor.map_or(at_ms, |f| f.max(at_ms)));
}

/// The owner set the progress (`PATCH`): device progress at or before `at_ms` loses.
pub fn owner_set_progress(s: &mut SyncState, book_id: &str, at_ms: i64) {
    let r = s.books.entry(book_id.to_owned()).or_default();
    r.progress_floor = Some(r.progress_floor.map_or(at_ms, |f| f.max(at_ms)));
}

/// The owner confirms a deletion request; returns the tombstone.
pub fn confirm_tombstone(s: &mut SyncState, op_id: &str) -> Option<Tombstone> {
    let t = s.tombstones.get_mut(op_id)?;
    t.confirmed = true;
    let t = t.clone();
    s.deleted.insert(t.highlight_id.clone());
    Some(t)
}

// ---- projection into the library records ------------------------------------------

fn device_source(st: &Stamp) -> String {
    format!("device:{}", st.device)
}

/// Writes the registers into `lib` for the books in `only` (every book when
/// `None`). Books missing from the library keep their registers until they
/// appear (an import), so ops on them are never lost. Sets `updatedAt` to
/// `now` on every book it changes; returns whether anything changed.
pub fn project(
    s: &SyncState,
    lib: &mut LibraryState,
    only: Option<&BTreeSet<String>>,
    now: &str,
) -> bool {
    let wanted = |lib: &LibraryState, b: &str| {
        only.is_none_or(|o| o.contains(b)) && lib.books.contains_key(b)
    };
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for (id, regs) in &s.books {
        if !wanted(lib, id) {
            continue;
        }
        let Some(book) = lib.books.get_mut(id) else {
            continue;
        };
        if let Some(p) = &regs.progress {
            if regs.progress_floor.is_none_or(|f| p.stamp.at_ms > f) {
                let next = Progress {
                    percent: p.value.percent,
                    location: p.value.location,
                    page: None,
                    source: device_source(&p.stamp),
                    at: p.value.at.clone(),
                    anchor: p.value.anchor.clone(),
                };
                if book.progress.as_ref() != Some(&next) {
                    book.progress = Some(next);
                    touched.insert(id.clone());
                }
            }
        }
        if let Some(st) = &regs.status {
            if !regs.owner_completed && regs.status_floor.is_none_or(|f| st.stamp.at_ms > f) {
                let src = Some(device_source(&st.stamp));
                if book.status != st.value || book.status_source != src {
                    book.status = st.value;
                    book.status_source = src;
                    touched.insert(id.clone());
                }
            }
        }
    }
    for (id, h) in &s.highlights {
        if !wanted(lib, &h.book_id) || s.deleted.contains(id) {
            continue;
        }
        let next = Highlight {
            id: id.clone(),
            book_id: h.book_id.clone(),
            kind: h.kind,
            text: h.text.clone(),
            note: None,
            location: None,
            page: None,
            added_at: Some(h.created_at.clone()),
            added_at_raw: h.created_at.clone(),
            source: DEVICE_SOURCE.into(),
            provenance: Provenance {
                file: format!("sync/{}", h.created.device),
                entry_index: h.created.counter as usize,
                device: Some(h.created.device.clone()),
            },
            dedup_key: h.dedup_key.clone(),
        };
        let hs = lib.highlights.entry(h.book_id.clone()).or_default();
        match hs.iter_mut().find(|x| x.id == *id) {
            Some(cur) => {
                let note = cur.note.clone();
                let next = Highlight { note, ..next };
                if *cur != next {
                    *cur = next;
                    touched.insert(h.book_id.clone());
                }
            }
            None => {
                hs.push(next);
                hs.sort_by(|a, b| a.id.cmp(&b.id));
                touched.insert(h.book_id.clone());
            }
        }
    }
    for (id, n) in &s.notes {
        if !wanted(lib, &n.book_id) {
            continue;
        }
        if let Some(h) = lib
            .highlights
            .get_mut(&n.book_id)
            .and_then(|hs| hs.iter_mut().find(|h| h.id == *id))
        {
            if h.note != n.note.value {
                h.note = n.note.value.clone();
                touched.insert(n.book_id.clone());
            }
        }
    }
    for t in s.tombstones.values().filter(|t| t.confirmed) {
        if !wanted(lib, &t.book_id) {
            continue;
        }
        if let Some(hs) = lib.highlights.get_mut(&t.book_id) {
            let before = hs.len();
            hs.retain(|h| h.id != t.highlight_id);
            if hs.len() != before {
                touched.insert(t.book_id.clone());
            }
        }
    }
    let mut sessions_changed = false;
    for (id, reg) in &s.sessions {
        let Some(start) = &reg.start else { continue };
        if !wanted(lib, &start.value.book_id) {
            continue;
        }
        let next = ReadingSession {
            id: id.clone(),
            book_id: start.value.book_id.clone(),
            started_at: start.value.at.clone(),
            ended_at: reg.end.as_ref().map(|e| e.value.at.clone()),
            start_location: start.value.location,
            end_location: reg.end.as_ref().and_then(|e| e.value.location),
            source: device_source(&start.stamp),
        };
        match lib.sessions.iter_mut().find(|x| x.id == *id) {
            Some(cur) if *cur == next => {}
            Some(cur) => {
                *cur = next;
                sessions_changed = true;
            }
            None => {
                lib.sessions.push(next);
                sessions_changed = true;
            }
        }
    }
    if sessions_changed {
        lib.sessions
            .sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
    }
    for id in &touched {
        if let Some(b) = lib.books.get_mut(id) {
            b.updated_at = now.into();
        }
    }
    !touched.is_empty() || sessions_changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOK: &str = "bk_0123456789abcdef";

    fn op(dev: &str, n: u64, at_s: i64, kind: OpKind) -> Op {
        let at = chrono::DateTime::from_timestamp(1_790_000_000 + at_s, 0)
            .unwrap()
            .to_rfc3339();
        Op {
            op_id: OpId::new(dev, n),
            at,
            kind,
        }
    }
    fn progress(dev: &str, n: u64, at: i64, pct: f32) -> Op {
        op(
            dev,
            n,
            at,
            OpKind::SetProgress {
                book_id: BOOK.into(),
                anchor: Some(format!("a{pct}")),
                percent: Some(pct),
                location: None,
            },
        )
    }
    fn status(dev: &str, n: u64, at: i64, s: ReadingStatus) -> Op {
        op(
            dev,
            n,
            at,
            OpKind::SetStatus {
                book_id: BOOK.into(),
                status: s,
            },
        )
    }
    fn run(ops: &[&Op]) -> SyncState {
        let mut s = SyncState::default();
        for o in ops {
            apply_mut(&mut s, o);
        }
        s
    }

    #[test]
    fn replay_is_a_noop() {
        let p = progress("a", 1, 10, 5.0);
        let (s1, o1) = apply(SyncState::default(), &p);
        assert_eq!(o1.verdict, Verdict::Applied);
        let (s2, o2) = apply(s1.clone(), &p);
        assert_eq!(o2.verdict, Verdict::Duplicate);
        assert_eq!(s1, s2);
    }

    #[test]
    fn progress_lww_commutes_and_reports_the_conflict() {
        let old = progress("a", 1, 10, 5.0);
        let new = progress("b", 1, 20, 50.0);
        let s1 = run(&[&old, &new]);
        let s2 = run(&[&new, &old]);
        assert_eq!(s1.content(), s2.content());
        assert_eq!(
            s1.books[BOOK].progress.as_ref().unwrap().value.percent,
            Some(50.0)
        );
        assert_eq!(s1.conflicts.len(), 1);
        assert_eq!(s2.conflicts.len(), 1);
        assert_eq!(s1.conflicts[0].op_id, old.op_id);
        assert_eq!(s2.conflicts[0].op_id, old.op_id);
    }

    #[test]
    fn device_completed_is_sticky_but_owner_can_revert() {
        let done = status("a", 1, 10, ReadingStatus::Completed);
        let reread = status("b", 1, 20, ReadingStatus::Reading);
        for order in [[&done, &reread], [&reread, &done]] {
            let s = run(&order);
            assert_eq!(
                s.books[BOOK].status.as_ref().unwrap().value,
                ReadingStatus::Completed
            );
        }
        let mut s = run(&[&done]);
        owner_set_status(&mut s, BOOK, ReadingStatus::Reading, 1_790_000_030_000);
        // An op older than the owner's edit loses; a newer one applies.
        let stale = status("b", 2, 25, ReadingStatus::Completed);
        assert_eq!(apply_mut(&mut s, &stale).verdict, Verdict::Superseded);
        let fresh = status("b", 3, 40, ReadingStatus::Abandoned);
        assert_eq!(apply_mut(&mut s, &fresh).verdict, Verdict::Applied);
    }

    #[test]
    fn owner_completed_is_never_reverted_by_devices() {
        let mut s = SyncState::default();
        owner_set_status(&mut s, BOOK, ReadingStatus::Completed, 1_790_000_000_000);
        let o = apply_mut(&mut s, &status("a", 1, 999, ReadingStatus::Reading));
        assert_eq!(o.verdict, Verdict::Protected);
        assert_eq!(o.conflict.unwrap().kind, "protected");
        assert!(s.books[BOOK].status.is_none());
    }

    #[test]
    fn highlights_grow_and_deletes_need_the_owner() {
        let add = |dev: &str, n, at, note: Option<&str>| {
            op(
                dev,
                n,
                at,
                OpKind::AddHighlight {
                    book_id: BOOK.into(),
                    kind: HighlightKind::Highlight,
                    text: "A  thought".into(),
                    anchor: "loc:10".into(),
                    note: note.map(Into::into),
                },
            )
        };
        let a = add("a", 1, 30, Some("first"));
        let b = add("b", 1, 10, None);
        let s1 = run(&[&a, &b]);
        let s2 = run(&[&b, &a]);
        assert_eq!(s1.content(), s2.content());
        assert_eq!(s1.highlights.len(), 1);
        let (id, reg) = s1.highlights.iter().next().unwrap();
        assert_eq!(reg.created.device, "b");
        assert_eq!(s1.notes[id].note.value.as_deref(), Some("first"));
        let del = op(
            "a",
            2,
            40,
            OpKind::DeleteHighlight {
                book_id: BOOK.into(),
                highlight_id: id.clone(),
            },
        );
        let mut s = s1.clone();
        assert_eq!(apply_mut(&mut s, &del).verdict, Verdict::Recorded);
        assert_eq!(s.highlights.len(), 1);
        assert_eq!(s.pending_tombstones().count(), 1);
        let t = confirm_tombstone(&mut s, "a:2").unwrap();
        assert_eq!(&t.highlight_id, id);
        assert!(s.deleted.contains(id));
        // A late re-add of the deleted highlight is reported, never shown.
        let o = apply_mut(&mut s, &add("c", 1, 50, None));
        assert_eq!(o.conflict.unwrap().kind, "deleted");
    }

    #[test]
    fn sessions_join_in_any_order() {
        let start = op(
            "a",
            1,
            10,
            OpKind::StartSession {
                book_id: BOOK.into(),
                session_id: "s1".into(),
                location: Some(5),
            },
        );
        let end = op(
            "a",
            2,
            70,
            OpKind::EndSession {
                book_id: BOOK.into(),
                session_id: "s1".into(),
                location: Some(9),
            },
        );
        assert_eq!(
            run(&[&start, &end]).content(),
            run(&[&end, &start]).content()
        );
    }

    #[test]
    fn rejected_ops_are_recorded_once() {
        let mut bad = progress("a", 1, 1, 5.0);
        bad.at = "not a time".into();
        let mut s = SyncState::default();
        assert_eq!(apply_mut(&mut s, &bad).verdict, Verdict::Rejected);
        assert_eq!(apply_mut(&mut s, &bad).verdict, Verdict::Duplicate);
        assert_eq!(s.clock, 1);
        assert_eq!(s.conflicts.len(), 1);
    }
}
