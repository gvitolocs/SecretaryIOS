//! Differential test: `lucina_memo::meetings::kernel` (production) against the function bodies
//! proved in `verification/verus/meeting_kernel.rs`, copied below without the Verus
//! annotations. Every combination of the small inputs. If one side changes, this fails until
//! the other (and its proof) follows.
use lucina_memo::meetings::kernel::{self, Chunk, Range, Status};

mod proved {
    use super::*;

    fn is_pipeline(s: Status) -> bool {
        matches!(
            s,
            Status::Queued
                | Status::Transcribing
                | Status::Diarizing
                | Status::Analyzing
                | Status::Publishing
        )
    }

    pub fn status_allowed(from: Status, to: Status) -> bool {
        if from == to {
            return true;
        }
        match from {
            Status::Uploading => to == Status::Queued,
            Status::Done | Status::Failed => to == Status::Queued,
            _ => is_pipeline(from) && to != Status::Uploading,
        }
    }

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

    #[allow(clippy::too_many_arguments)]
    pub fn ready_to_finish(
        t: bool,
        d: bool,
        a: bool,
        b: bool,
        w: bool,
        n: bool,
        v: bool,
        r: bool,
        m: bool,
        x: bool,
    ) -> bool {
        t && d && a && b && w && (n || !v) && (r || !m) && x
    }

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

    pub fn should_ask(recordings: u32, ask_after: u32, asked: bool, named: bool) -> bool {
        let min = if ask_after < 2 { 2 } else { ask_after };
        !asked && !named && recordings >= min
    }

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

    pub fn send_once(marker: Option<u32>, revision: u32) -> bool {
        match marker {
            Some(m) => m != revision,
            None => true,
        }
    }
}

const STATUSES: [Status; 8] = [
    Status::Uploading,
    Status::Queued,
    Status::Transcribing,
    Status::Diarizing,
    Status::Analyzing,
    Status::Publishing,
    Status::Done,
    Status::Failed,
];

#[test]
fn statuses_match() {
    for a in STATUSES {
        for b in STATUSES {
            assert_eq!(
                kernel::status_allowed(a, b),
                proved::status_allowed(a, b),
                "{a:?} -> {b:?}"
            );
        }
    }
}

#[test]
fn chunks_match() {
    for stored in 0..5u64 {
        for size in 0..5u64 {
            for whole in [false, true] {
                for total in 0..6u64 {
                    let q = Range::Query { total };
                    assert_eq!(
                        kernel::chunk(stored, size, whole, q),
                        proved::chunk(stored, size, whole, q)
                    );
                    for start in 0..6u64 {
                        for end in 0..6u64 {
                            let r = Range::Bytes { start, end, total };
                            assert_eq!(
                                kernel::chunk(stored, size, whole, r),
                                proved::chunk(stored, size, whole, r)
                            );
                        }
                    }
                }
            }
        }
    }
    // The extremes.
    let r = Range::Bytes {
        start: u64::MAX,
        end: u64::MAX,
        total: u64::MAX,
    };
    assert_eq!(
        kernel::chunk(u64::MAX, u64::MAX, false, r),
        proved::chunk(u64::MAX, u64::MAX, false, r)
    );
}

/// Every vector of length 0..=4 over `values`.
fn vectors<T: Copy>(values: &[T]) -> Vec<Vec<T>> {
    let mut all = vec![vec![]];
    let mut last = vec![vec![]];
    for _ in 0..4 {
        let mut next = Vec::new();
        for v in &last {
            for x in values {
                let mut w: Vec<T> = v.clone();
                w.push(*x);
                next.push(w);
            }
        }
        all.extend(next.iter().cloned());
        last = next;
    }
    all
}

#[test]
fn lists_match() {
    let bools = vectors(&[false, true]);
    for v in &bools {
        for a in &bools {
            assert_eq!(kernel::may_complete(v, a), proved::may_complete(v, a));
        }
    }
    for s in vectors(&[-1i32, 0, 1, 2]) {
        for t in -2..4 {
            assert_eq!(
                kernel::best_match(&s, t),
                proved::best_match(&s, t),
                "{s:?} {t}"
            );
        }
    }
    for o in vectors(&[-1i64, 0, 1, 2]) {
        assert_eq!(kernel::speaker_for(&o), proved::speaker_for(&o), "{o:?}");
    }
}

#[test]
fn flags_match() {
    for bits in 0u32..1024 {
        let b = |i: u32| bits & (1 << i) != 0;
        assert_eq!(
            kernel::ready_to_finish(b(0), b(1), b(2), b(3), b(4), b(5), b(6), b(7), b(8), b(9)),
            proved::ready_to_finish(b(0), b(1), b(2), b(3), b(4), b(5), b(6), b(7), b(8), b(9))
        );
    }
    for rec in 0..5 {
        for after in 0..5 {
            for asked in [false, true] {
                for named in [false, true] {
                    assert_eq!(
                        kernel::should_ask(rec, after, asked, named),
                        proved::should_ask(rec, after, asked, named)
                    );
                }
            }
        }
    }
    for m in [None, Some(0), Some(1), Some(u32::MAX)] {
        for r in [0, 1, u32::MAX] {
            assert_eq!(kernel::send_once(m, r), proved::send_once(m, r));
        }
    }
}
