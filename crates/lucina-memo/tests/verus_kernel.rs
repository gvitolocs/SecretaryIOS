//! Differential test: `lucina_memo::kernel` (production) against the function
//! bodies proved in `verification/verus/memo_kernel.rs` (branch main), copied
//! below without the Verus annotations. Every combination of the small inputs.
//! If one side changes, this fails until the other (and its proof) follows.
use lucina_memo::kernel::{self, Choice, Gpu, Source};
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
