//! Differential test: `lucina_memo::kernel` (production) against the function
//! bodies proved in `verification/verus/memo_kernel.rs` (branch main), copied
//! below without the Verus annotations. Every combination of the small inputs.
//! If one side changes, this fails until the other (and its proof) follows.
use lucina_memo::kernel::{self, Cand, Choice, Gpu, LinkSource, Source, TitleSource};
use lucina_memo::model::Status;

mod proved {
    use super::*;

    pub fn transcript_choice(existing: Option<Source>, gpu: Gpu, device_available: bool) -> Choice {
        let has_whisper = matches!(existing, Some(Source::Whisper));
        match gpu {
            Gpu::Done => Choice::UseWhisper,
            Gpu::Failed => Choice::Fail,
            Gpu::Busy => {
                if device_available && !has_whisper {
                    Choice::UseDevice
                } else {
                    Choice::Wait
                }
            }
        }
    }

    pub fn upgrade_replaces(whisper_chars: u64, device_chars: u64) -> bool {
        (whisper_chars as u128) * 2 >= device_chars as u128
    }

    pub fn route_on_device(duration_ms: u64, max_ms: u64, model_available: bool) -> bool {
        model_available && duration_ms <= max_ms
    }

    fn is_pipeline(s: Status) -> bool {
        matches!(
            s,
            Status::Queued
                | Status::Transcribing
                | Status::Summarizing
                | Status::Rendering
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

    pub fn ready_to_finish(t: bool, s: bool, r: bool, p: bool, v: bool, n: bool) -> bool {
        t && s && r && (p || !v) && n
    }

    pub fn send_telegram(marker: Option<u32>, revision: u32) -> bool {
        match marker {
            Some(m) => m != revision,
            None => true,
        }
    }

    pub fn note_base(existing: Option<u64>, computed: u64) -> u64 {
        match existing {
            Some(e) => e,
            None => computed,
        }
    }

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

    pub fn eligible(rec_len: i128, overlap: i64, all_day: bool) -> bool {
        !all_day && overlap > 0 && (2 * (overlap as i128) >= rec_len || overlap >= 600_000)
    }

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

    pub fn may_auto_link(source: LinkSource) -> bool {
        !matches!(source, LinkSource::Owner)
    }

    pub fn title_from_event(source: TitleSource) -> bool {
        !matches!(source, TitleSource::Owner)
    }

    pub fn diarize_memo(available: bool, labelled: bool, attempted: bool) -> bool {
        available && !labelled && !attempted
    }
}

const STATUSES: [Status; 8] = [
    Status::Uploading,
    Status::Queued,
    Status::Transcribing,
    Status::Summarizing,
    Status::Rendering,
    Status::Publishing,
    Status::Done,
    Status::Failed,
];

#[test]
fn transcript_choice_matches() {
    for existing in [None, Some(Source::Device), Some(Source::Whisper)] {
        for gpu in [Gpu::Done, Gpu::Busy, Gpu::Failed] {
            for dev in [false, true] {
                assert_eq!(
                    kernel::transcript_choice(existing, gpu, dev),
                    proved::transcript_choice(existing, gpu, dev),
                    "{existing:?} {gpu:?} {dev}"
                );
            }
        }
    }
    assert_eq!(
        kernel::Source::parse(Some("device-kept")),
        Some(Source::Device)
    );
    assert_eq!(
        kernel::Source::parse(Some("whisper")),
        Some(Source::Whisper)
    );
    assert_eq!(kernel::Source::parse(None), None);
}

#[test]
fn numbers_match() {
    let edge = [
        0u64,
        1,
        2,
        3,
        99,
        100,
        101,
        199,
        200,
        201,
        599_999,
        600_000,
        600_001,
        u64::MAX / 2,
        u64::MAX - 1,
        u64::MAX,
    ];
    for &a in &edge {
        for &b in &edge {
            assert_eq!(
                kernel::upgrade_replaces(a, b),
                proved::upgrade_replaces(a, b),
                "{a} {b}"
            );
            for m in [false, true] {
                assert_eq!(
                    kernel::route_on_device(a, b, m),
                    proved::route_on_device(a, b, m),
                    "{a} {b} {m}"
                );
            }
        }
    }
}

#[test]
fn statuses_match() {
    for from in STATUSES {
        for to in STATUSES {
            assert_eq!(
                kernel::status_allowed(from, to),
                proved::status_allowed(from, to),
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn finish_telegram_and_note_match() {
    for bits in 0u8..64 {
        let b = |i: u8| bits & (1 << i) != 0;
        assert_eq!(
            kernel::ready_to_finish(b(0), b(1), b(2), b(3), b(4), b(5)),
            proved::ready_to_finish(b(0), b(1), b(2), b(3), b(4), b(5))
        );
    }
    for marker in [None, Some(0u32), Some(1), Some(2), Some(u32::MAX)] {
        for rev in [0u32, 1, 2, u32::MAX] {
            assert_eq!(
                kernel::send_telegram(marker, rev),
                proved::send_telegram(marker, rev)
            );
        }
    }
    for existing in [None, Some(7u64), Some(9)] {
        let prod = kernel::note_base(existing.map(|e| e.to_string()), || "8".to_string());
        assert_eq!(prod, proved::note_base(existing, 8).to_string());
    }
}

/// The iPhone's threshold (MemoOnDeviceTranscriber.maxSeconds = 600) in the kernel's terms.
#[test]
fn iphone_threshold() {
    assert!(kernel::route_on_device(600_000, 600_000, true));
    assert!(!kernel::route_on_device(600_001, 600_000, true));
    assert!(!kernel::route_on_device(1_000, 600_000, false));
}

#[test]
fn calendar_link_matches() {
    let points = [
        i64::MIN,
        -600_001,
        -1,
        0,
        1,
        2,
        600_000,
        1_200_000,
        i64::MAX,
    ];
    for &a0 in &points {
        for &a1 in &points {
            for &b0 in &points {
                for &b1 in &points {
                    assert_eq!(
                        kernel::overlap_ms(a0, a1, b0, b1),
                        proved::overlap_ms(a0, a1, b0, b1),
                        "{a0} {a1} {b0} {b1}"
                    );
                }
            }
        }
    }
    // Every pair (and the empty and single lists) of small candidates, every small recording.
    let t = [0i64, 600_000, 1_200_000, 1_800_000];
    let mut cands = Vec::new();
    for &s0 in &t {
        for &e0 in &t {
            for (all_day, loc_match) in [(false, false), (false, true), (true, false)] {
                cands.push(Cand {
                    start_ms: s0,
                    end_ms: e0,
                    all_day,
                    loc_match,
                });
            }
        }
    }
    for &r0 in &t {
        for &r1 in &t {
            assert_eq!(
                kernel::pick_event(r0, r1, &[]),
                proved::pick_event(r0, r1, &[])
            );
            for a in &cands {
                assert_eq!(
                    kernel::pick_event(r0, r1, &[*a]),
                    proved::pick_event(r0, r1, &[*a])
                );
                for b in &cands {
                    let set = [*a, *b];
                    assert_eq!(
                        kernel::pick_event(r0, r1, &set),
                        proved::pick_event(r0, r1, &set),
                        "{r0} {r1} {set:?}"
                    );
                }
            }
        }
    }
    for s in [LinkSource::None, LinkSource::Auto, LinkSource::Owner] {
        assert_eq!(kernel::may_auto_link(s), proved::may_auto_link(s));
    }
    for s in [
        TitleSource::Auto,
        TitleSource::Lucina,
        TitleSource::Event,
        TitleSource::Owner,
    ] {
        assert_eq!(kernel::title_from_event(s), proved::title_from_event(s));
    }
}

#[test]
fn diarize_memo_matches() {
    for a in [false, true] {
        for l in [false, true] {
            for t in [false, true] {
                assert_eq!(kernel::diarize_memo(a, l, t), proved::diarize_memo(a, l, t));
            }
        }
    }
}
