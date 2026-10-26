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
        Status::Queued | Status::Transcribing | Status::Summarizing | Status::Rendering | Status::Publishing
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_never_downgraded() {
        for gpu in [Gpu::Done, Gpu::Busy, Gpu::Failed] {
            for dev in [false, true] {
                assert_ne!(transcript_choice(Some(Source::Whisper), gpu, dev), Choice::UseDevice);
            }
        }
        assert_eq!(transcript_choice(None, Gpu::Busy, true), Choice::UseDevice);
        assert_eq!(transcript_choice(Some(Source::Whisper), Gpu::Busy, true), Choice::Wait);
    }

    #[test]
    fn statuses() {
        assert!(status_allowed(Status::Uploading, Status::Queued));
        assert!(!status_allowed(Status::Uploading, Status::Done));
        assert!(!status_allowed(Status::Done, Status::Uploading));
        assert!(status_allowed(Status::Publishing, Status::Done));
        assert!(!status_allowed(Status::Done, Status::Transcribing));
    }
}
