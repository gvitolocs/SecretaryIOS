//! The meeting pipeline's decisions as pure functions, proved for all inputs in
//! `verification/verus/meeting_kernel.rs` (same bodies). The differential test
//! `tests/verus_meeting_kernel.rs` runs these and the proof file's copies side by side over
//! every small input.
//!
//! - [`status_allowed`] (MT1): the status transitions; `MeetingStore::update` checks it.
//! - [`chunk`] (MT2): one `PUT …/tracks/{name}` request against the stored length. Bytes are
//!   appended only at the stored offset and never past the declared size, so the stored file is
//!   always a prefix of the client's.
//! - [`may_complete`] (MT3): `complete` only with every declared track whole and verified, and
//!   at least one track with audio.
//! - [`ready_to_finish`] (MT4): `done` only with every output.
//! - [`best_match`] (MT5): the voice profile a cluster belongs to.
//! - [`should_ask`] (MT6): an unknown recurring voice is asked about once.
//! - [`speaker_for`] (MT7): the speaker of a word: the turn overlapping it most.
//! - [`send_once`] (MT8): a side effect without idempotency (Telegram, the memory entry) at most
//!   once per revision: the marker is written first.

/// Where a meeting is. `queued` → `transcribing` → `diarizing` → `analyzing` → `publishing` →
/// `done`; any step may go back to `queued` (retry) or end in `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl Status {
    pub fn is_pipeline(self) -> bool {
        matches!(
            self,
            Status::Queued
                | Status::Transcribing
                | Status::Diarizing
                | Status::Analyzing
                | Status::Publishing
        )
    }

    /// The Mac app's four states (`MEETINGS_API.md`).
    pub fn public(self) -> &'static str {
        match self {
            Status::Uploading => "uploading",
            Status::Done => "ready",
            Status::Failed => "failed",
            _ => "processing",
        }
    }
}

/// MT1. `uploading` is left only for `queued` and never re-entered; `done` and `failed` go
/// back only to `queued` (rename of a speaker, reprocess); pipeline steps move among
/// themselves and end in `done` or `failed`.
pub fn status_allowed(from: Status, to: Status) -> bool {
    if from == to {
        return true;
    }
    match from {
        Status::Uploading => to == Status::Queued,
        Status::Done | Status::Failed => to == Status::Queued,
        _ => from.is_pipeline() && to != Status::Uploading,
    }
}

/// A parsed `Content-Range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// `bytes */total`: how much is stored?
    Query { total: u64 },
    /// `bytes start-end/total`, `end` inclusive.
    Bytes { start: u64, end: u64, total: u64 },
}

/// MT2's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chunk {
    /// Append the body (exactly `end - start + 1` bytes) at the stored offset.
    Append,
    /// 308 with the stored length.
    Partial,
    /// 200: the track is whole (and verified).
    Complete,
    /// 409: `start` is not the stored length.
    Mismatch,
    /// 416: an empty, reversed or out-of-file range.
    Unsatisfiable,
    /// 400: `total` is not the declared size.
    WrongTotal,
}

/// MT2. `stored`: bytes on disk; `size`: the declared size; `whole`: the track was completed.
pub fn chunk(stored: u64, size: u64, whole: bool, r: Range) -> Chunk {
    match r {
        Range::Query { total } => {
            if total != size {
                Chunk::WrongTotal
            } else if whole {
                Chunk::Complete
            } else {
                Chunk::Partial
            }
        }
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
        }
    }
}

/// MT3. Every declared track whole and verified, at least one track, at least one with audio.
pub fn may_complete(verified: &[bool], audio: &[bool]) -> bool {
    if verified.is_empty() || verified.len() != audio.len() {
        return false;
    }
    let mut all = true;
    let mut any_audio = false;
    let mut i = 0;
    while i < verified.len() {
        all = all && verified[i];
        any_audio = any_audio || audio[i];
        i += 1;
    }
    all && any_audio
}

/// MT4. `done` only with the transcript, the speakers, the analysis, the brief, the files in the
/// meeting folder, the Obsidian note (when a vault is configured), the memory entry (when memory
/// writes are on) and the notification.
#[allow(clippy::too_many_arguments)]
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
) -> bool {
    transcribed
        && diarized
        && analyzed
        && briefed
        && written
        && (noted || !vault)
        && (remembered || !memory)
        && notified
}

/// MT5. The index of the highest score at or above `threshold`; the first of equal ones.
pub fn best_match(scores: &[i32], threshold: i32) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut i = 0;
    while i < scores.len() {
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

/// MT6. Ask about an unknown voice once it was heard in `recordings >= ask_after` (at least 2)
/// recordings, unless already asked or named.
pub fn should_ask(recordings: u32, ask_after: u32, asked: bool, named: bool) -> bool {
    let min = if ask_after < 2 { 2 } else { ask_after };
    !asked && !named && recordings >= min
}

/// MT7. The speaker whose turn overlaps a word most (milliseconds per speaker); None when no
/// turn overlaps it. The first of equal ones.
pub fn speaker_for(overlaps: &[i64]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut i = 0;
    while i < overlaps.len() {
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

/// MT8. Send unless this revision's marker is already recorded (the marker is written before
/// the send, so a crash after it skips a second send: at most once).
pub fn send_once(marker: Option<u32>, revision: u32) -> bool {
    match marker {
        Some(m) => m != revision,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks() {
        let q = |total| Range::Query { total };
        let b = |start, end, total| Range::Bytes { start, end, total };
        assert_eq!(chunk(0, 10, false, q(10)), Chunk::Partial);
        assert_eq!(chunk(10, 10, true, q(10)), Chunk::Complete);
        assert_eq!(chunk(0, 10, false, q(11)), Chunk::WrongTotal);
        assert_eq!(chunk(0, 10, false, b(0, 3, 10)), Chunk::Append);
        assert_eq!(chunk(4, 10, false, b(0, 3, 10)), Chunk::Mismatch);
        assert_eq!(chunk(4, 10, false, b(4, 10, 10)), Chunk::Unsatisfiable);
        assert_eq!(chunk(4, 10, false, b(5, 4, 10)), Chunk::Unsatisfiable);
        assert_eq!(chunk(10, 10, true, b(0, 3, 10)), Chunk::Complete);
        // Exhaustive: an append never goes past the declared size and starts at the stored end.
        for stored in 0..6u64 {
            for size in 0..6u64 {
                for start in 0..7u64 {
                    for end in 0..7u64 {
                        if chunk(stored, size, false, b(start, end, size)) == Chunk::Append {
                            assert!(
                                start == stored && end < size && stored + (end - start + 1) <= size
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn statuses() {
        use Status::*;
        assert!(status_allowed(Uploading, Queued));
        assert!(!status_allowed(Uploading, Transcribing));
        assert!(status_allowed(Analyzing, Queued));
        assert!(status_allowed(Publishing, Done));
        assert!(!status_allowed(Done, Publishing));
        assert!(!status_allowed(Queued, Uploading));
        assert_eq!(Diarizing.public(), "processing");
        assert_eq!(Done.public(), "ready");
    }

    #[test]
    fn matching_asking_and_words() {
        assert_eq!(best_match(&[500, 820, 820, 700], 600), Some(1));
        assert_eq!(best_match(&[500, 590], 600), None);
        assert_eq!(best_match(&[], 0), None);
        assert!(!should_ask(1, 2, false, false));
        assert!(should_ask(2, 2, false, false));
        assert!(should_ask(2, 0, false, false));
        assert!(!should_ask(1, 0, false, false));
        assert!(!should_ask(5, 2, true, false));
        assert!(!should_ask(5, 2, false, true));
        assert_eq!(speaker_for(&[0, 300, 300]), Some(1));
        assert_eq!(speaker_for(&[0, -5]), None);
        assert!(may_complete(&[true, true], &[false, true]));
        assert!(!may_complete(&[true, false], &[true, true]));
        assert!(!may_complete(&[true], &[false]));
        assert!(!may_complete(&[], &[]));
        assert!(send_once(None, 0) && send_once(Some(0), 1) && !send_once(Some(3), 3));
    }
}
