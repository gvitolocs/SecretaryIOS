// Voice-memo pipeline kernel (lucina-memo, docs/memos.md).
//
// Production (branch lucina-rs): crates/lucina-memo/src/kernel.rs has the same function
// bodies; the worker (worker.rs), the store (store.rs: every status change is checked
// with status_allowed) and the iPhone (MemoOnDeviceTranscriber.maxSeconds, the language
// check) use them. Differential test: crates/lucina-memo/tests/verus_kernel.rs runs the
// production functions and the copies below over every small input.
//
// Proven for all inputs:
//   K1 transcript_choice: a Whisper transcript is never replaced by the iPhone's; Whisper
//      wins whenever it ran; the iPhone's transcript is used only while the GPU is busy
//      (or the model missing) and no Whisper transcript exists, and then always (no wait).
//   K2 upgrade_replaces: Whisper replaces the iPhone's transcript iff it heard at least
//      half as much; never overflows.
//   K3 route_on_device: a memo is transcribed on the iPhone only with a model for its
//      language and only up to the threshold; longer memos go to the server only.
//   K4 status_allowed: `uploading` is left only for `queued` and never re-entered; `done`
//      and `failed` are entered only from a pipeline step; any status but `uploading` can
//      be re-queued.
//   K5 ready_to_finish: `done` only with transcript, summary, PDF, note (with a vault) and
//      notification.
//   K6 send_telegram: Telegram is sent at most once per revision across retries.
//   K7 note_base: a memo keeps its first note name over any sequence of later titles, so
//      it has one note in the vault (lemma over all sequences).
//   K11 diarize_memo: speaker labels are made only when a diarizer is available, the
//      transcript has none and no attempt was made on this transcript; a labelled transcript
//      is never relabelled and a failing diarizer is not retried on the same transcript
//      (lemma: at most one attempt per transcript).
//
// Bug found while writing this (fixed in lucina-rs with the kernel): a reprocess with the
// GPU busy replaced a Whisper transcript with the iPhone's (K1 failed on `existing`).
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Source {
    Device,
    Whisper,
}

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Gpu {
    Done,
    Busy,
    Failed,
}

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Choice {
    UseWhisper,
    UseDevice,
    Wait,
    Fail,
}

#[derive(PartialEq, Eq, Clone, Copy, Structural)]
pub enum Status {
    Uploading,
    Queued,
    Transcribing,
    Summarizing,
    Rendering,
    Publishing,
    Done,
    Failed,
}

// ---------------------------------------------------------------- K1

pub fn transcript_choice(existing: Option<Source>, gpu: Gpu, device_available: bool) -> (c: Choice)
    ensures
        existing == Some(Source::Whisper) ==> c != Choice::UseDevice,
        (gpu == Gpu::Done) == (c == Choice::UseWhisper),
        (gpu == Gpu::Failed) == (c == Choice::Fail),
        c == Choice::UseDevice ==> gpu == Gpu::Busy && device_available,
        gpu == Gpu::Busy && device_available && existing != Some(Source::Whisper) ==> c
            == Choice::UseDevice,
{
    let has_whisper = match existing {
        Some(Source::Whisper) => true,
        _ => false,
    };
    match gpu {
        Gpu::Done => Choice::UseWhisper,
        Gpu::Failed => Choice::Fail,
        Gpu::Busy => {
            if device_available && !has_whisper {
                Choice::UseDevice
            } else {
                Choice::Wait
            }
        },
    }
}

// ---------------------------------------------------------------- K2

pub fn upgrade_replaces(whisper_chars: u64, device_chars: u64) -> (r: bool)
    ensures
        r == (2 * whisper_chars as int >= device_chars as int),
        whisper_chars >= device_chars ==> r,
{
    (whisper_chars as u128) * 2 >= device_chars as u128
}

// ---------------------------------------------------------------- K3

pub fn route_on_device(duration_ms: u64, max_ms: u64, model_available: bool) -> (r: bool)
    ensures
        r == (model_available && duration_ms <= max_ms),
        duration_ms > max_ms ==> !r,
        !model_available ==> !r,
{
    model_available && duration_ms <= max_ms
}

// ---------------------------------------------------------------- K4

pub open spec fn pipeline(s: Status) -> bool {
    s == Status::Queued || s == Status::Transcribing || s == Status::Summarizing || s
        == Status::Rendering || s == Status::Publishing
}

pub fn is_pipeline(s: Status) -> (r: bool)
    ensures
        r == pipeline(s),
{
    match s {
        Status::Queued | Status::Transcribing | Status::Summarizing | Status::Rendering
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
        from != Status::Uploading ==> (r || to != Status::Queued),
        (from == Status::Done || from == Status::Failed) && from != to && r ==> to
            == Status::Queued,
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

// ---------------------------------------------------------------- K5

pub fn ready_to_finish(
    transcribed: bool,
    summarized: bool,
    rendered: bool,
    published: bool,
    vault: bool,
    notified: bool,
) -> (r: bool)
    ensures
        r == (transcribed && summarized && rendered && (published || !vault) && notified),
        r ==> transcribed && summarized && rendered && notified,
        r && vault ==> published,
{
    transcribed && summarized && rendered && (published || !vault) && notified
}

// ---------------------------------------------------------------- K6

pub open spec fn spec_send_telegram(marker: Option<u32>, revision: u32) -> bool {
    marker != Some(revision)
}

pub fn send_telegram(marker: Option<u32>, revision: u32) -> (r: bool)
    ensures
        r == spec_send_telegram(marker, revision),
{
    match marker {
        Some(m) => m != revision,
        None => true,
    }
}

/// Telegram messages sent over `attempts` tries of one revision, starting from `marker`:
/// each try sends when `send_telegram` says so, after recording `Some(revision)` (the
/// worker writes the marker before calling Telegram, so a crash or error right after the
/// send still leaves the marker).
pub open spec fn telegram_sends(marker: Option<u32>, revision: u32, attempts: nat) -> nat
    decreases attempts,
{
    if attempts == 0 {
        0
    } else if spec_send_telegram(marker, revision) {
        1 + telegram_sends(Some(revision), revision, (attempts - 1) as nat)
    } else {
        telegram_sends(marker, revision, (attempts - 1) as nat)
    }
}

proof fn lemma_marked_never_sends(revision: u32, attempts: nat)
    ensures
        telegram_sends(Some(revision), revision, attempts) == 0,
    decreases attempts,
{
    if attempts > 0 {
        lemma_marked_never_sends(revision, (attempts - 1) as nat);
    }
}

/// At most one Telegram message per revision, however many retries and crashes.
pub proof fn lemma_telegram_at_most_once(marker: Option<u32>, revision: u32, attempts: nat)
    ensures
        telegram_sends(marker, revision, attempts) <= 1,
    decreases attempts,
{
    if attempts > 0 {
        if spec_send_telegram(marker, revision) {
            lemma_marked_never_sends(revision, (attempts - 1) as nat);
        } else {
            lemma_telegram_at_most_once(marker, revision, (attempts - 1) as nat);
        }
    }
}

// ---------------------------------------------------------------- K7

pub open spec fn spec_note_base(existing: Option<u64>, computed: u64) -> u64 {
    match existing {
        Some(e) => e,
        None => computed,
    }
}

pub fn note_base(existing: Option<u64>, computed: u64) -> (r: u64)
    ensures
        r == spec_note_base(existing, computed),
        existing.is_some() ==> r == existing.unwrap(),
{
    match existing {
        Some(e) => e,
        None => computed,
    }
}

/// The note name after publishing with each title in `titles` (each publish records the
/// name it used, the next publish passes it as `existing`).
pub open spec fn names_after(existing: Option<u64>, titles: Seq<u64>) -> Seq<u64>
    decreases titles.len(),
{
    if titles.len() == 0 {
        Seq::empty()
    } else {
        let n = spec_note_base(existing, titles[0]);
        seq![n].add(names_after(Some(n), titles.subrange(1, titles.len() as int)))
    }
}

/// However many times the title changes, every publish uses the first name: one note.
pub proof fn lemma_one_note(first: u64, titles: Seq<u64>)
    ensures
        forall|i: int| 0 <= i < titles.len() ==> #[trigger] names_after(Some(first), titles)[i]
            == first,
        names_after(Some(first), titles).len() == titles.len(),
    decreases titles.len(),
{
    if titles.len() > 0 {
        lemma_one_note(first, titles.subrange(1, titles.len() as int));
        let rest = names_after(Some(first), titles.subrange(1, titles.len() as int));
        assert(names_after(Some(first), titles) =~= seq![first].add(rest));
    }
}

pub proof fn lemma_one_note_from_scratch(titles: Seq<u64>)
    requires
        titles.len() > 0,
    ensures
        forall|i: int| 0 <= i < titles.len() ==> #[trigger] names_after(None, titles)[i]
            == titles[0],
{
    lemma_one_note(titles[0], titles.subrange(1, titles.len() as int));
    let rest = names_after(Some(titles[0]), titles.subrange(1, titles.len() as int));
    assert(names_after(None, titles) =~= seq![titles[0]].add(rest));
}

// ---------------------------------------------------------------- K11

pub open spec fn spec_diarize_memo(available: bool, labelled: bool, attempted: bool) -> bool {
    available && !labelled && !attempted
}

pub fn diarize_memo(available: bool, labelled: bool, attempted: bool) -> (r: bool)
    ensures
        r == spec_diarize_memo(available, labelled, attempted),
        labelled ==> !r,
        attempted ==> !r,
{
    available && !labelled && !attempted
}

/// Attempts over runs on one transcript, with the diarizer's availability at each run; each
/// attempt records `diarized_at` (attempted) whatever its outcome.
pub open spec fn attempts(attempted: bool, labelled: bool, avail: Seq<bool>) -> nat
    decreases avail.len(),
{
    if avail.len() == 0 {
        0
    } else if spec_diarize_memo(avail[0], labelled, attempted) {
        1 + attempts(true, labelled, avail.subrange(1, avail.len() as int))
    } else {
        attempts(attempted, labelled, avail.subrange(1, avail.len() as int))
    }
}

proof fn lemma_attempted_stops(labelled: bool, avail: Seq<bool>)
    ensures
        attempts(true, labelled, avail) == 0,
    decreases avail.len(),
{
    if avail.len() > 0 {
        lemma_attempted_stops(labelled, avail.subrange(1, avail.len() as int));
    }
}

/// At most one diarization per transcript, however many runs, restarts and outages.
pub proof fn lemma_diarize_once_per_transcript(attempted: bool, labelled: bool, avail: Seq<bool>)
    ensures
        attempts(attempted, labelled, avail) <= 1,
    decreases avail.len(),
{
    if avail.len() > 0 {
        if spec_diarize_memo(avail[0], labelled, attempted) {
            lemma_attempted_stops(labelled, avail.subrange(1, avail.len() as int));
        } else {
            lemma_diarize_once_per_transcript(attempted, labelled, avail.subrange(1, avail.len() as int));
        }
    }
}

fn main() {
}

} // verus!
