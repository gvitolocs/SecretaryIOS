//! The pipeline's decisions as pure functions, proved for all inputs in
//! `verification/verus/memo_kernel.rs` (branch main; same bodies). The
//! differential test `tests/verus_kernel.rs` runs these and the proof file's
//! copies side by side over every small input.
//!
//! - [`transcript_choice`]: what becomes the memo's transcript in step 1. A
//!   Whisper transcript is never replaced by the iPhone's (K1).
//! - [`upgrade_replaces`]: whether Whisper's later transcript replaces the
//!   iPhone's (K2).
//! - [`route_on_device`]: which memos the iPhone transcribes itself (K3, the
//!   Swift side mirrors it: `MemoOnDeviceTranscriber`).
//! - [`status_allowed`]: the status transitions (K4); `Store::update` checks it.
//! - [`ready_to_finish`]: a memo is `done` only with every output (K5).
//! - [`send_telegram`]: Telegram at most once per revision (K6).
//! - [`note_base`]: one note file per memo whatever its later titles (K7).
//! - [`pick_event`]: the calendar event a recording belongs to (K8), with
//!   [`overlap_ms`], [`eligible`] and [`better`].
//! - [`may_auto_link`]: the worker never replaces the owner's event choice (K9).
//! - [`title_from_event`]: the linked event names the memo unless the owner did (K10).
use crate::model::Status;

/// Where a transcript came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The recording device (iPhone), possibly kept after a weaker Whisper run.
    Device,
    /// whisper.cpp on the server.
    Whisper,
}

impl Source {
    /// `Memo::transcript_source` as stored (`whisper`, `device`, `device-kept`).
    pub fn parse(s: Option<&str>) -> Option<Source> {
        match s {
            Some("whisper") => Some(Source::Whisper),
            Some("device") | Some("device-kept") => Some(Source::Device),
            _ => None,
        }
    }
}

/// What Whisper did in step 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpu {
    /// A transcript came back.
    Done,
    /// No lease (GPU busy) or no model (MyBook locked): try again later.
    Busy,
    /// Whisper itself failed.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    UseWhisper,
    UseDevice,
    /// Keep the memo queued (no attempt counted) and retry.
    Wait,
    /// Count a failed attempt.
    Fail,
}

/// Step 1. `existing`: the transcript the memo already has (a reprocess runs
/// step 1 again). The iPhone's transcript stands in only while no Whisper
/// transcript exists.
pub fn transcript_choice(existing: Option<Source>, gpu: Gpu, device_available: bool) -> Choice {
    match gpu {
        Gpu::Done => Choice::UseWhisper,
        Gpu::Failed => Choice::Fail,
        Gpu::Busy => {
            if device_available && existing != Some(Source::Whisper) {
                Choice::UseDevice
            } else {
                Choice::Wait
            }
        }
    }
}

/// The idle upgrade: Whisper replaces the iPhone's transcript unless it heard
/// less than half as much (VAD or silence filtering cut real speech).
pub fn upgrade_replaces(whisper_chars: u64, device_chars: u64) -> bool {
    (whisper_chars as u128) * 2 >= device_chars as u128
}

/// The iPhone transcribes a memo itself only when it has a model for the
/// memo's language and the memo is at most `max_ms` long.
pub fn route_on_device(duration_ms: u64, max_ms: u64, model_available: bool) -> bool {
    model_available && duration_ms <= max_ms
}

fn pipeline(s: Status) -> bool {
    matches!(
        s,
        Status::Queued
            | Status::Transcribing
            | Status::Summarizing
            | Status::Rendering
            | Status::Publishing
    )
}

/// Every status change the service makes.
/// - `uploading` is left only for `queued` (the upload completed), never entered again;
/// - pipeline steps move among themselves and end in `done`, `failed` or back in `queued`;
/// - `done` and `failed` go back to `queued` only (re-summary, rename, reprocess, upgrade).
pub fn status_allowed(from: Status, to: Status) -> bool {
    if from == to {
        return true;
    }
    match from {
        Status::Uploading => to == Status::Queued,
        Status::Done | Status::Failed => to == Status::Queued,
        _ => pipeline(from) && to != Status::Uploading,
    }
}

/// A memo may be marked `done` only with its transcript, summary, PDF, note
/// (when a vault is configured) and notification.
pub fn ready_to_finish(
    transcribed: bool,
    summarized: bool,
    rendered: bool,
    published: bool,
    vault: bool,
    notified: bool,
) -> bool {
    transcribed && summarized && rendered && (published || !vault) && notified
}

/// The note's file name: once a memo has one, it keeps it (titles may change
/// later; a second name would be a second note in the vault) (K7).
pub fn note_base(existing: Option<String>, computed: impl FnOnce() -> String) -> String {
    match existing {
        Some(e) => e,
        None => computed(),
    }
}

/// Telegram has no idempotency key: the revision is recorded before sending,
/// and a retry of the same revision skips Telegram (at most once). The app
/// inbox deduplicates by key, so it is always (re)sent.
pub fn send_telegram(marker: Option<u32>, revision: u32) -> bool {
    marker != Some(revision)
}

/// Milliseconds two half-open intervals share; 0 when either is empty or reversed.
pub fn overlap_ms(a0: i64, a1: i64, b0: i64, b1: i64) -> i64 {
    if a1 <= a0 || b1 <= b0 {
        return 0;
    }
    let lo = if a0 > b0 { a0 } else { b0 };
    let hi = if a1 < b1 { a1 } else { b1 };
    if hi <= lo {
        return 0;
    }
    let d = hi as i128 - lo as i128;
    if d > i64::MAX as i128 {
        i64::MAX
    } else {
        d as i64
    }
}

/// A calendar event as [`pick_event`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cand {
    pub start_ms: i64,
    pub end_ms: i64,
    pub all_day: bool,
    /// The event's place is where the device was.
    pub loc_match: bool,
}

/// K8, which events may hold a recording of `rec_len` ms: timed ones covering half of it or
/// at least ten minutes of it (a short memo inside a long lecture; a long memo across two
/// meetings goes to the one it mostly overlaps).
pub fn eligible(rec_len: i128, overlap: i64, all_day: bool) -> bool {
    !all_day && overlap > 0 && (2 * (overlap as i128) >= rec_len || overlap >= 600_000)
}

/// K8, the order: the event at the device's location first, then more overlap, then the
/// shorter event (the more specific one), then the earlier one.
pub fn better(c: Cand, c_overlap: i64, p: Cand, p_overlap: i64) -> bool {
    if c.loc_match != p.loc_match {
        return c.loc_match;
    }
    if c_overlap != p_overlap {
        return c_overlap > p_overlap;
    }
    let dc = c.end_ms as i128 - c.start_ms as i128;
    let dp = p.end_ms as i128 - p.start_ms as i128;
    if dc != dp {
        return dc < dp;
    }
    c.start_ms < p.start_ms
}

/// K8: the index of the event the recording `[rec0, rec1)` belongs to, `None` when no event
/// is eligible. Of equal events the first wins.
pub fn pick_event(rec0: i64, rec1: i64, cands: &[Cand]) -> Option<usize> {
    let span = rec1 as i128 - rec0 as i128;
    let len = if span < 1 { 1 } else { span };
    let mut best: Option<usize> = None;
    let mut best_overlap: i64 = 0;
    let mut i = 0;
    while i < cands.len() {
        let c = cands[i];
        let o = overlap_ms(rec0, rec1, c.start_ms, c.end_ms);
        if eligible(len, o, c.all_day) {
            let take = match best {
                None => true,
                Some(b) => better(c, o, cands[b], best_overlap),
            };
            if take {
                best = Some(i);
                best_overlap = o;
            }
        }
        i += 1;
    }
    best
}

/// Who linked the memo's event (`Memo::event_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    None,
    Auto,
    Owner,
}

impl LinkSource {
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("owner") => Self::Owner,
            Some("auto") => Self::Auto,
            _ => Self::None,
        }
    }
}

/// K9: the worker (and a reprocess) links an event only while the owner has not chosen one,
/// "no event" included.
pub fn may_auto_link(source: LinkSource) -> bool {
    !matches!(source, LinkSource::Owner)
}

/// Where the memo's title came from (`Memo::title_source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    Auto,
    Lucina,
    Event,
    Owner,
}

impl TitleSource {
    pub fn parse(s: &str) -> Self {
        match s {
            "owner" => Self::Owner,
            "event" => Self::Event,
            "lucina" => Self::Lucina,
            _ => Self::Auto,
        }
    }
}

/// K10: the linked event names the memo unless the owner named it.
pub fn title_from_event(source: TitleSource) -> bool {
    !matches!(source, TitleSource::Owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_never_downgraded() {
        for gpu in [Gpu::Done, Gpu::Busy, Gpu::Failed] {
            for dev in [false, true] {
                assert_ne!(
                    transcript_choice(Some(Source::Whisper), gpu, dev),
                    Choice::UseDevice
                );
            }
        }
        assert_eq!(transcript_choice(None, Gpu::Busy, true), Choice::UseDevice);
        assert_eq!(
            transcript_choice(Some(Source::Whisper), Gpu::Busy, true),
            Choice::Wait
        );
    }

    #[test]
    fn statuses() {
        assert!(status_allowed(Status::Uploading, Status::Queued));
        assert!(!status_allowed(Status::Uploading, Status::Done));
        assert!(!status_allowed(Status::Done, Status::Uploading));
        assert!(status_allowed(Status::Publishing, Status::Done));
        assert!(!status_allowed(Status::Done, Status::Transcribing));
    }

    fn cand(start_ms: i64, end_ms: i64) -> Cand {
        Cand {
            start_ms,
            end_ms,
            all_day: false,
            loc_match: false,
        }
    }

    #[test]
    fn overlaps() {
        assert_eq!(overlap_ms(0, 10, 5, 20), 5);
        assert_eq!(overlap_ms(0, 10, 10, 20), 0);
        assert_eq!(overlap_ms(10, 0, 0, 10), 0);
        assert_eq!(overlap_ms(i64::MIN, i64::MAX, i64::MIN, i64::MAX), i64::MAX);
        // Symmetric and never longer than either interval, over a small exhaustive range.
        for a0 in -3..4 {
            for a1 in -3..4 {
                for b0 in -3..4 {
                    for b1 in -3..4 {
                        let o = overlap_ms(a0, a1, b0, b1);
                        assert_eq!(o, overlap_ms(b0, b1, a0, a1));
                        assert!(o >= 0 && o <= (a1 - a0).max(0) && o <= (b1 - b0).max(0));
                    }
                }
            }
        }
    }

    #[test]
    fn picks_the_event_the_recording_is_in() {
        let min = 60_000;
        // A 5-minute memo inside a 2-hour lecture.
        assert_eq!(
            pick_event(30 * min, 35 * min, &[cand(0, 120 * min)]),
            Some(0)
        );
        // A 2-hour recording across two meetings goes to the one it mostly overlaps.
        let two = [cand(0, 30 * min), cand(30 * min, 150 * min)];
        assert_eq!(pick_event(10 * min, 130 * min, &two), Some(1));
        // A sliver of overlap (under 10 min and under half the memo) does not count.
        assert_eq!(pick_event(0, 60 * min, &[cand(55 * min, 90 * min)]), None);
        // All-day events never hold a memo.
        let all_day = Cand {
            all_day: true,
            ..cand(0, 1_440 * min)
        };
        assert_eq!(pick_event(10 * min, 20 * min, &[all_day]), None);
        // The event where the phone was wins over a bigger overlap elsewhere.
        let here = Cand {
            loc_match: true,
            ..cand(10 * min, 25 * min)
        };
        assert_eq!(pick_event(0, 30 * min, &[cand(0, 30 * min), here]), Some(1));
        // Equal overlap: the shorter (more specific) event, then the earlier, then the first.
        assert_eq!(
            pick_event(
                10 * min,
                20 * min,
                &[cand(0, 60 * min), cand(5 * min, 30 * min)]
            ),
            Some(1)
        );
        assert_eq!(
            pick_event(
                10 * min,
                20 * min,
                &[cand(5 * min, 30 * min), cand(0, 25 * min)]
            ),
            Some(1)
        );
        assert_eq!(
            pick_event(10 * min, 20 * min, &[cand(0, 30 * min), cand(0, 30 * min)]),
            Some(0)
        );
        assert_eq!(pick_event(0, 10, &[]), None);
    }

    #[test]
    fn the_pick_is_eligible_and_none_is_better() {
        // Every pick over small inputs: eligible, and no other eligible candidate beats it.
        let bounds = [0i64, 1, 2, 3];
        let mut cands = Vec::new();
        for s in bounds {
            for e in bounds {
                for loc in [false, true] {
                    cands.push(Cand {
                        start_ms: s,
                        end_ms: e,
                        all_day: false,
                        loc_match: loc,
                    });
                }
            }
        }
        for r0 in bounds {
            for r1 in bounds {
                let len = (r1 as i128 - r0 as i128).max(1);
                for a in 0..cands.len() {
                    for b in 0..cands.len() {
                        let set = [cands[a], cands[b]];
                        let ok = |c: &Cand| {
                            eligible(len, overlap_ms(r0, r1, c.start_ms, c.end_ms), c.all_day)
                        };
                        match pick_event(r0, r1, &set) {
                            None => assert!(!set.iter().any(ok)),
                            Some(i) => {
                                assert!(ok(&set[i]));
                                let oi = overlap_ms(r0, r1, set[i].start_ms, set[i].end_ms);
                                for (j, c) in set.iter().enumerate() {
                                    if j != i && ok(c) {
                                        assert!(!better(
                                            *c,
                                            overlap_ms(r0, r1, c.start_ms, c.end_ms),
                                            set[i],
                                            oi
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_owner_s_choices_win() {
        assert!(may_auto_link(LinkSource::parse(None)));
        assert!(may_auto_link(LinkSource::parse(Some("auto"))));
        assert!(!may_auto_link(LinkSource::parse(Some("owner"))));
        for s in ["auto", "lucina", "event"] {
            assert!(title_from_event(TitleSource::parse(s)));
        }
        assert!(!title_from_event(TitleSource::parse("owner")));
    }
}
