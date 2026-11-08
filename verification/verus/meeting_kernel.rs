// Meetings pipeline kernel (lucina-memo meetings, docs/meetings.md).
//
// Production (branch lucina-rs / meetings-local): crates/lucina-memo/src/meetings/kernel.rs has
// the same function bodies; the store (meetings/store.rs: every status change is checked with
// status_allowed, every PUT goes through chunk, complete through may_complete), the worker
// (ready_to_finish, send_once), the voice book (diarize/profiles.rs: best_match, should_ask)
// and the timeline (meetings/timeline.rs: speaker_for) use them. Differential test:
// crates/lucina-memo/tests/verus_meeting_kernel.rs runs the production functions and the
// copies below over every small input.
//
// Proven for all inputs:
//   MT1 status_allowed: `uploading` is left only for `queued` and never re-entered; `done` and
//       `failed` are entered only from a pipeline step and left only for `queued`.
//   MT2 chunk: bytes are appended only at the stored offset, never past the declared size, and
//       only to a track that is not whole; so the stored file is always a prefix of the
//       client's file (lemma_prefix_after_appends over any sequence of requests).
//   MT3 may_complete: `complete` only with every declared track whole and verified, at least one.
//   MT4 ready_to_finish: `done` only with every output.
//   MT5 best_match: the result reaches the threshold, is the highest score, and the first of
//       equal ones; None only when nothing reaches the threshold.
//   MT6 should_ask: never before two recordings, never twice (lemma over any number of runs).
//   MT7 speaker_for: the word's speaker overlaps it, and most (first of equals).
//   MT8 send_once: at most one send per revision across retries and crashes (marker first).
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Status {
    Uploading,
    Queued,
    Transcribing,
    Diarizing,
    Analyzing,
    Publishing,
    Done,
    Failed,
}

// ---------------------------------------------------------------- MT1

pub open spec fn pipeline(s: Status) -> bool {
    s == Status::Queued || s == Status::Transcribing || s == Status::Diarizing || s
        == Status::Analyzing || s == Status::Publishing
}

pub fn is_pipeline(s: Status) -> (r: bool)
    ensures
        r == pipeline(s),
{
    match s {
        Status::Queued | Status::Transcribing | Status::Diarizing | Status::Analyzing
        | Status::Publishing => true,
        _ => false,
    }
}

pub fn status_allowed(from: Status, to: Status) -> (r: bool)
    ensures
        from == to ==> r,
        from == Status::Uploading && r ==> to == Status::Uploading || to == Status::Queued,
        to == Status::Uploading && r ==> from == Status::Uploading,
        to == Status::Done && from != Status::Done && r ==> pipeline(from),
        to == Status::Failed && from != Status::Failed && r ==> pipeline(from),
        (from == Status::Done || from == Status::Failed) && from != to && r ==> to
            == Status::Queued,
        from != Status::Uploading ==> (r || to != Status::Queued),
{
    if from == to {
        return true;
    }
    match from {
        Status::Uploading => to == Status::Queued,
        Status::Done | Status::Failed => to == Status::Queued,
        _ => is_pipeline(from) && to != Status::Uploading,
    }
}

// ---------------------------------------------------------------- MT2

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Range {
    Query { total: u64 },
    Bytes { start: u64, end: u64, total: u64 },
}

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Chunk {
    Append,
    Partial,
    Complete,
    Mismatch,
    Unsatisfiable,
    WrongTotal,
}

pub open spec fn appends(stored: u64, size: u64, whole: bool, r: Range) -> bool {
    match r {
        Range::Query { .. } => false,
        Range::Bytes { start, end, total } => total == size && !whole && start <= end && end
            < total && start == stored,
    }
}

pub fn chunk(stored: u64, size: u64, whole: bool, r: Range) -> (c: Chunk)
    ensures
        (c == Chunk::Append) == appends(stored, size, whole, r),
        c == Chunk::Complete ==> whole,
        c == Chunk::Partial ==> !whole,
        c == Chunk::Append ==> match r {
            Range::Bytes { start, end, .. } => start == stored && stored + (end - start + 1)
                <= size,
            Range::Query { .. } => false,
        },
{
    match r {
        Range::Query { total } => {
            if total != size {
                Chunk::WrongTotal
            } else if whole {
                Chunk::Complete
            } else {
                Chunk::Partial
            }
        },
        Range::Bytes { start, end, total } => {
            if total != size {
                Chunk::WrongTotal
            } else if whole {
                Chunk::Complete
            } else if end < start || end >= total {
                Chunk::Unsatisfiable
            } else if start != stored {
                Chunk::Mismatch
            } else {
                Chunk::Append
            }
        },
    }
}

/// Bytes on disk after a sequence of requests, each appending `got[i]` bytes (a body may be
/// cut short: got[i] <= end - start + 1) when MT2 says Append.
pub open spec fn stored_after(stored: nat, size: u64, reqs: Seq<(Range, nat)>) -> nat
    decreases reqs.len(),
{
    if reqs.len() == 0 {
        stored
    } else {
        let (r, got) = reqs[0];
        let next = if stored <= u64::MAX as nat && appends(stored as u64, size, false, r) {
            match r {
                Range::Bytes { start, end, .. } => if got <= (end - start + 1) as nat {
                    stored + got
                } else {
                    stored + (end - start + 1) as nat
                },
                Range::Query { .. } => stored,
            }
        } else {
            stored
        };
        stored_after(next, size, reqs.subrange(1, reqs.len() as int))
    }
}

/// Whatever the requests (duplicated, reordered, cut off), the stored length never passes the
/// declared size: the file stays a prefix of the client's.
pub proof fn lemma_prefix_after_appends(stored: nat, size: u64, reqs: Seq<(Range, nat)>)
    requires
        stored <= size as nat,
    ensures
        stored_after(stored, size, reqs) <= size as nat,
    decreases reqs.len(),
{
    if reqs.len() > 0 {
        let (r, got) = reqs[0];
        let next = if stored <= u64::MAX as nat && appends(stored as u64, size, false, r) {
            match r {
                Range::Bytes { start, end, .. } => if got <= (end - start + 1) as nat {
                    stored + got
                } else {
                    stored + (end - start + 1) as nat
                },
                Range::Query { .. } => stored,
            }
        } else {
            stored
        };
        assert(next <= size as nat);
        lemma_prefix_after_appends(next, size, reqs.subrange(1, reqs.len() as int));
    }
}

// ---------------------------------------------------------------- MT3

pub fn may_complete(verified: &[bool], audio: &[bool]) -> (r: bool)
    ensures
        r == (verified@.len() > 0 && verified@.len() == audio@.len() && (forall|j: int|
            0 <= j < verified@.len() ==> verified@[j]) && (exists|j: int|
            0 <= j < audio@.len() && audio@[j])),
{
    if verified.len() == 0 || verified.len() != audio.len() {
        return false;
    }
    let mut all = true;
    let mut any_audio = false;
    let mut i: usize = 0;
    while i < verified.len()
        invariant
            i <= verified@.len(),
            verified@.len() == audio@.len(),
            all == (forall|j: int| 0 <= j < i ==> verified@[j]),
            any_audio == (exists|j: int| 0 <= j < i && audio@[j]),
        decreases verified@.len() - i,
    {
        all = all && verified[i];
        any_audio = any_audio || audio[i];
        proof {
            assert(any_audio == (exists|j: int| 0 <= j < i + 1 && audio@[j])) by {
                if audio@[i as int] {
                    assert(0 <= i < i + 1 && audio@[i as int]);
                }
            }
        }
        i += 1;
    }
    all && any_audio
}

// ---------------------------------------------------------------- MT4

pub fn ready_to_finish(
    transcribed: bool,
    diarized: bool,
    analyzed: bool,
    briefed: bool,
    written: bool,
    noted: bool,
    vault: bool,
    remembered: bool,
    memory: bool,
    notified: bool,
) -> (r: bool)
    ensures
        r ==> transcribed && diarized && analyzed && briefed && written && notified,
        r && vault ==> noted,
        r && memory ==> remembered,
        r == (transcribed && diarized && analyzed && briefed && written && (noted || !vault) && (
        remembered || !memory) && notified),
{
    transcribed && diarized && analyzed && briefed && written && (noted || !vault) && (remembered
        || !memory) && notified
}

// ---------------------------------------------------------------- MT5

pub fn best_match(scores: &[i32], threshold: i32) -> (r: Option<usize>)
    ensures
        r.is_some() ==> r.unwrap() < scores@.len() && scores@[r.unwrap() as int] >= threshold,
        r.is_some() ==> forall|j: int|
            0 <= j < scores@.len() && #[trigger] scores@[j] >= threshold ==> scores@[j]
                <= scores@[r.unwrap() as int],
        r.is_some() ==> forall|j: int|
            0 <= j < r.unwrap() ==> #[trigger] scores@[j] < scores@[r.unwrap() as int],
        r.is_none() ==> forall|j: int| 0 <= j < scores@.len() ==> #[trigger] scores@[j]
            < threshold,
{
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < scores.len()
        invariant
            i <= scores@.len(),
            best.is_some() ==> best.unwrap() < i && scores@[best.unwrap() as int] >= threshold,
            best.is_some() ==> forall|j: int|
                0 <= j < i && #[trigger] scores@[j] >= threshold ==> scores@[j]
                    <= scores@[best.unwrap() as int],
            best.is_some() ==> forall|j: int|
                0 <= j < best.unwrap() ==> #[trigger] scores@[j] < scores@[best.unwrap() as int],
            best.is_none() ==> forall|j: int| 0 <= j < i ==> #[trigger] scores@[j] < threshold,
        decreases scores@.len() - i,
    {
        if scores[i] >= threshold {
            let take = match best {
                Some(b) => scores[i] > scores[b],
                None => true,
            };
            if take {
                best = Some(i);
            }
        }
        i += 1;
    }
    best
}

// ---------------------------------------------------------------- MT6

pub open spec fn spec_should_ask(recordings: u32, ask_after: u32, asked: bool, named: bool) -> bool {
    let min = if ask_after < 2 {
        2
    } else {
        ask_after
    };
    !asked && !named && recordings >= min
}

pub fn should_ask(recordings: u32, ask_after: u32, asked: bool, named: bool) -> (r: bool)
    ensures
        r == spec_should_ask(recordings, ask_after, asked, named),
        r ==> recordings >= 2 && !asked && !named,
{
    let min = if ask_after < 2 {
        2
    } else {
        ask_after
    };
    !asked && !named && recordings >= min
}

/// Questions over `runs` passes with the recordings seen at each pass; the marker (`asked`)
/// is written before each question.
pub open spec fn questions(asked: bool, recs: Seq<u32>, ask_after: u32, named: bool) -> nat
    decreases recs.len(),
{
    if recs.len() == 0 {
        0
    } else if spec_should_ask(recs[0], ask_after, asked, named) {
        1 + questions(true, recs.subrange(1, recs.len() as int), ask_after, named)
    } else {
        questions(asked, recs.subrange(1, recs.len() as int), ask_after, named)
    }
}

proof fn lemma_asked_never_asks(recs: Seq<u32>, ask_after: u32, named: bool)
    ensures
        questions(true, recs, ask_after, named) == 0,
    decreases recs.len(),
{
    if recs.len() > 0 {
        lemma_asked_never_asks(recs.subrange(1, recs.len() as int), ask_after, named);
    }
}

/// An unknown voice is asked about at most once, however many recordings and retries.
pub proof fn lemma_ask_at_most_once(asked: bool, recs: Seq<u32>, ask_after: u32, named: bool)
    ensures
        questions(asked, recs, ask_after, named) <= 1,
    decreases recs.len(),
{
    if recs.len() > 0 {
        if spec_should_ask(recs[0], ask_after, asked, named) {
            lemma_asked_never_asks(recs.subrange(1, recs.len() as int), ask_after, named);
        } else {
            lemma_ask_at_most_once(asked, recs.subrange(1, recs.len() as int), ask_after, named);
        }
    }
}

// ---------------------------------------------------------------- MT7

pub fn speaker_for(overlaps: &[i64]) -> (r: Option<usize>)
    ensures
        r.is_some() ==> r.unwrap() < overlaps@.len() && overlaps@[r.unwrap() as int] > 0,
        r.is_some() ==> forall|j: int|
            0 <= j < overlaps@.len() ==> #[trigger] overlaps@[j] <= overlaps@[r.unwrap() as int],
        r.is_some() ==> forall|j: int|
            0 <= j < r.unwrap() ==> #[trigger] overlaps@[j] < overlaps@[r.unwrap() as int],
        r.is_none() ==> forall|j: int| 0 <= j < overlaps@.len() ==> #[trigger] overlaps@[j] <= 0,
{
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < overlaps.len()
        invariant
            i <= overlaps@.len(),
            best.is_some() ==> best.unwrap() < i && overlaps@[best.unwrap() as int] > 0,
            best.is_some() ==> forall|j: int|
                0 <= j < i ==> #[trigger] overlaps@[j] <= overlaps@[best.unwrap() as int],
            best.is_some() ==> forall|j: int|
                0 <= j < best.unwrap() ==> #[trigger] overlaps@[j] < overlaps@[best.unwrap() as int],
            best.is_none() ==> forall|j: int| 0 <= j < i ==> #[trigger] overlaps@[j] <= 0,
        decreases overlaps@.len() - i,
    {
        if overlaps[i] > 0 {
            let take = match best {
                Some(b) => overlaps[i] > overlaps[b],
                None => true,
            };
            if take {
                best = Some(i);
            }
        }
        i += 1;
    }
    best
}

// ---------------------------------------------------------------- MT8

pub open spec fn spec_send_once(marker: Option<u32>, revision: u32) -> bool {
    marker != Some(revision)
}

pub fn send_once(marker: Option<u32>, revision: u32) -> (r: bool)
    ensures
        r == spec_send_once(marker, revision),
{
    match marker {
        Some(m) => m != revision,
        None => true,
    }
}

pub open spec fn sends(marker: Option<u32>, revision: u32, attempts: nat) -> nat
    decreases attempts,
{
    if attempts == 0 {
        0
    } else if spec_send_once(marker, revision) {
        1 + sends(Some(revision), revision, (attempts - 1) as nat)
    } else {
        sends(marker, revision, (attempts - 1) as nat)
    }
}

proof fn lemma_marked_never_sends(revision: u32, attempts: nat)
    ensures
        sends(Some(revision), revision, attempts) == 0,
    decreases attempts,
{
    if attempts > 0 {
        lemma_marked_never_sends(revision, (attempts - 1) as nat);
    }
}

/// One Telegram message and one memory entry per revision at most.
pub proof fn lemma_send_at_most_once(marker: Option<u32>, revision: u32, attempts: nat)
    ensures
        sends(marker, revision, attempts) <= 1,
    decreases attempts,
{
    if attempts > 0 {
        if spec_send_once(marker, revision) {
            lemma_marked_never_sends(revision, (attempts - 1) as nat);
        } else {
            lemma_send_at_most_once(marker, revision, (attempts - 1) as nat);
        }
    }
}

fn main() {
}

} // verus!
