//! The resumable event stream (Lucina only): `GET /v1/agents/stream` numbers every
//! event, keeps the recent ones, and lets a client that reconnects with
//! `Last-Event-ID` get what it missed (design: "Resumable delivery").
//!
//! Ids are integers, increasing across daemon restarts: each run starts at its start
//! time in milliseconds times 1000. So an id from an earlier run is older than
//! anything kept, and its client gets `stream.reset` instead of a replay with a gap.

/// Events are kept this long…
pub const KEEP_MS: u64 = 24 * 60 * 60 * 1000;
/// …and at most this many.
pub const KEEP_MAX: usize = 10_000;

/// The first id of a run that started at `start_ms` (Unix milliseconds).
pub fn first_id(start_ms: u64) -> u64 {
    start_ms.saturating_mul(1000).max(1)
}

/// What a (re)connecting client gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// No `Last-Event-ID`: live events only.
    Live,
    /// Replay every kept event with an id greater than this one, then live events.
    After(u64),
    /// The client missed events that are no longer kept (or sent an id this daemon
    /// never issued): `stream.reset`, then live events. It refetches its state.
    Reset,
}

/// Decide from the client's `Last-Event-ID` and the kept window: `next` is the id the
/// next event will get, `oldest` the oldest kept id (`None`: nothing kept).
///
/// An id `l` can be resumed only when every event after it is still kept: `l + 1` is
/// kept (`l + 1 >= oldest`), or nothing after it was ever issued (`l + 1 == next`).
/// Ids at or past `next` were never issued.
pub fn resume(last_event_id: Option<&str>, oldest: Option<u64>, next: u64) -> Resume {
    let Some(raw) = last_event_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Resume::Live;
    };
    let Some(l) = raw.parse::<u64>().ok().filter(|l| raw.bytes().all(|b| b.is_ascii_digit()) && *l > 0) else {
        return Resume::Reset;
    };
    if l >= next {
        return Resume::Reset;
    }
    let first_missing = l + 1;
    let kept = match oldest {
        Some(o) => first_missing >= o,
        None => false,
    };
    if kept || first_missing == next {
        Resume::After(l)
    } else {
        Resume::Reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions() {
        // Kept window 100..=109, next 110.
        let (oldest, next) = (Some(100), 110);
        assert_eq!(resume(None, oldest, next), Resume::Live);
        assert_eq!(resume(Some(""), oldest, next), Resume::Live);
        assert_eq!(resume(Some("109"), oldest, next), Resume::After(109), "up to date");
        assert_eq!(resume(Some("104"), oldest, next), Resume::After(104));
        assert_eq!(resume(Some("99"), oldest, next), Resume::After(99), "100 is the first missing one and is kept");
        assert_eq!(resume(Some("98"), oldest, next), Resume::Reset, "99 was evicted");
        assert_eq!(resume(Some("110"), oldest, next), Resume::Reset, "never issued");
        assert_eq!(resume(Some("x"), oldest, next), Resume::Reset);
        assert_eq!(resume(Some("-1"), oldest, next), Resume::Reset);
        assert_eq!(resume(Some("+5"), oldest, next), Resume::Reset);
        assert_eq!(resume(Some("0"), oldest, next), Resume::Reset);
        // Nothing kept yet in this run: only the id just before the run's first is current.
        assert_eq!(resume(Some("199"), None, 200), Resume::After(199));
        assert_eq!(resume(Some("150"), None, 200), Resume::Reset, "an earlier run's id");
    }

    #[test]
    fn exhaustive_small_windows() {
        // A replay never skips an id: After(l) only when l+1..next are all kept (or none exist).
        for oldest in [None, Some(5u64), Some(8)] {
            for next in 5u64..12 {
                if oldest.is_some_and(|o| o > next) {
                    continue;
                }
                for l in 0u64..14 {
                    let r = resume(Some(&l.to_string()), oldest, next);
                    if let Resume::After(a) = r {
                        assert!(a < next);
                        let gap_free = (a + 1..next).all(|id| oldest.is_some_and(|o| id >= o));
                        assert!(gap_free, "oldest={oldest:?} next={next} l={l}");
                    }
                }
            }
        }
    }

    #[test]
    fn runs_are_ordered() {
        assert!(first_id(1_761_600_000_000) > first_id(1_761_599_999_999) + 999);
        assert!(first_id(1_761_600_000_000) < (1u64 << 53), "ids stay exact in JavaScript");
        assert_eq!(first_id(0), 1);
    }
}
