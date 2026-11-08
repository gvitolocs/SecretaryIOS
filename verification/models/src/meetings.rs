//! Meetings end to end (docs/meetings.md): the Mac uploader (Secretary Code), the
//! `/v1/meetings` store and the lucina-memo meetings worker.
//!
//! One recording, one track of `SIZE` bytes (byte `i` of the client's file is `i`), small
//! budgets. Actors and what they may do:
//!
//! - the Mac: create the meeting (retried after a lost response, same `Idempotency-Key`), ask
//!   for the stored offset (`Content-Range: bytes */N`), send the next byte at the offset it
//!   believes, complete (also early, or again: the server must check);
//! - the network: delivers PUTs in any order, duplicates one, or cuts one off (0 bytes kept);
//! - the store: kernel MT2 `chunk` (append only at the stored offset), the SHA-256 check
//!   when the track reaches its size (a mismatch resets it to 0), MT3 `may_complete`;
//! - the worker: transcribe (Whisper under the GPU lease), diarize, analyze + brief (the model
//!   may fail; after the attempts the deterministic fallback), write the folder files, the
//!   Obsidian note, the memory entry, the notification (Telegram), the "who is this voice?"
//!   question, done (MT4); it may crash between any two steps and inside the split ones
//!   (marker written / side effect done / step recorded);
//! - another GPU job; `gpu-lease` prunes a dead holder;
//! - the owner: names a speaker once the meeting is done (outputs again, no new notification).
//!
//! Switches (each one off reproduces the bug its fix prevents):
//! - `fix_offset_check`: MT2 appends only at the stored offset;
//! - `fix_idempotency`: a retried create returns the same meeting;
//! - `fix_complete_check`: MT3, `complete` only when the track is whole and verified;
//! - `fix_markers`: MT8/MT6, the Telegram, memory and voice-question markers are written
//!   before the side effect.

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const PROTOCOL: &str = "meetings";
const SIZE: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    Uploading,
    Queued,
    Transcribing,
    Diarizing,
    Analyzing,
    Publishing,
    Done,
}

/// MT1 (`meetings::kernel::status_allowed`, without `failed`, which needs the attempt limit).
fn status_allowed(from: Status, to: Status) -> bool {
    use Status::*;
    if from == to {
        return true;
    }
    match from {
        Uploading => to == Queued,
        Done => to == Queued,
        _ => to != Uploading,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lease {
    Free,
    Worker,
    Other,
    Dead,
}

/// A split worker step in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Idle,
    Whisper,
    /// The memory entry was sent; `remembered_at` not yet written.
    Remembered,
    /// The notification was sent; `notified_at` not yet written.
    Notified,
    /// The voice question was sent; not yet recorded as done for this run.
    Asked,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    // the Mac
    pub has_id: bool,
    pub belief: u8,
    pub confirmed: bool,
    // the network: PUT starts in flight (sorted)
    pub in_flight: Vec<u8>,
    // the store
    pub meetings: u8,
    pub stored: Vec<u8>,
    pub whole: bool,
    pub status: Status,
    pub bad_transition: bool,
    // the worker
    pub transcribed: bool,
    pub diarized: bool,
    pub analyzed: bool,
    pub written: bool,
    pub noted: bool,
    pub remembered: bool,
    pub notified: bool,
    pub asked_done: bool,
    pub mem_marker: bool,
    pub tg_marker: bool,
    pub ask_marker: bool,
    pub step: Step,
    pub lease: Lease,
    pub attempts: u8,
    pub fallback: bool,
    // the world
    pub memory_entries: u8,
    pub telegram: u8,
    pub questions: u8,
    pub saw_cut_then_whole: bool,
    pub cut_seen: bool,
    pub gpu_was_busy: bool,
    // budgets
    pub create_retries: u8,
    pub net_faults: u8,
    pub crashes: u8,
    pub llm_fails: u8,
    pub other_jobs: u8,
    pub owner_acts: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Create,
    RetryCreate,
    Query,
    Send,
    Deliver(u8),
    Duplicate(u8),
    Cut(u8),
    Complete,
    WorkerStep,
    LlmFails,
    WorkerCrash,
    OtherTakesGpu,
    OtherReleasesGpu,
    PruneDeadLease,
    OwnerNamesSpeaker,
}

#[derive(Clone, Debug)]
pub struct Meetings {
    pub fix_offset_check: bool,
    pub fix_idempotency: bool,
    pub fix_complete_check: bool,
    pub fix_markers: bool,
    pub create_retries: u8,
    pub net_faults: u8,
    pub crashes: u8,
    pub llm_fails: u8,
    pub other_jobs: u8,
    pub owner_acts: u8,
    /// Attempts before the deterministic analysis/brief.
    pub max_attempts: u8,
}

impl Meetings {
    fn pipeline(s: &State) -> bool {
        !matches!(s.status, Status::Uploading | Status::Done)
    }

    fn set_status(n: &mut State, to: Status) {
        if !status_allowed(n.status, to) {
            n.bad_transition = true;
        }
        n.status = to;
    }

    fn insert(v: &mut Vec<u8>, x: u8) {
        v.push(x);
        v.sort();
    }

    fn remove(v: &mut Vec<u8>, x: u8) {
        if let Some(i) = v.iter().position(|y| *y == x) {
            v.remove(i);
        }
    }
}

impl Model for Meetings {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            has_id: false,
            belief: 0,
            confirmed: false,
            in_flight: vec![],
            meetings: 0,
            stored: vec![],
            whole: false,
            status: Status::Uploading,
            bad_transition: false,
            transcribed: false,
            diarized: false,
            analyzed: false,
            written: false,
            noted: false,
            remembered: false,
            notified: false,
            asked_done: false,
            mem_marker: false,
            tg_marker: false,
            ask_marker: false,
            step: Step::Idle,
            lease: Lease::Free,
            attempts: 0,
            fallback: false,
            memory_entries: 0,
            telegram: 0,
            questions: 0,
            saw_cut_then_whole: false,
            cut_seen: false,
            gpu_was_busy: false,
            create_retries: self.create_retries,
            net_faults: self.net_faults,
            crashes: self.crashes,
            llm_fails: self.llm_fails,
            other_jobs: self.other_jobs,
            owner_acts: self.owner_acts,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if !s.has_id {
            out.push(Action::Create);
        }
        if s.has_id && s.create_retries > 0 {
            out.push(Action::RetryCreate);
        }
        if s.has_id && !s.confirmed {
            out.push(Action::Query);
            if s.belief < SIZE && !s.in_flight.contains(&s.belief) {
                out.push(Action::Send);
            }
        }
        for &x in &s.in_flight {
            out.push(Action::Deliver(x));
            if s.net_faults > 0 {
                out.push(Action::Duplicate(x));
                out.push(Action::Cut(x));
            }
        }
        if s.has_id && s.status == Status::Uploading {
            out.push(Action::Complete);
        }
        if Self::pipeline(s) || s.step != Step::Idle {
            out.push(Action::WorkerStep);
            if s.crashes > 0 {
                out.push(Action::WorkerCrash);
            }
        }
        if Self::pipeline(s) && s.step == Step::Idle && s.diarized && !s.analyzed && s.llm_fails > 0 {
            out.push(Action::LlmFails);
        }
        if s.other_jobs > 0 && s.lease == Lease::Free {
            out.push(Action::OtherTakesGpu);
        }
        if s.lease == Lease::Other {
            out.push(Action::OtherReleasesGpu);
        }
        if s.lease == Lease::Dead {
            out.push(Action::PruneDeadLease);
        }
        if s.status == Status::Done && s.owner_acts > 0 {
            out.push(Action::OwnerNamesSpeaker);
        }
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match a {
            Action::Create => {
                n.has_id = true;
                n.meetings += 1;
            }
            Action::RetryCreate => {
                // The response was lost; the Mac posts again with the same key.
                n.create_retries -= 1;
                if !self.fix_idempotency {
                    n.meetings += 1;
                }
            }
            Action::Query => {
                // 308 with the stored length, or 200 when whole.
                n.belief = s.stored.len() as u8;
                n.confirmed = s.whole;
            }
            Action::Send => {
                Self::insert(&mut n.in_flight, s.belief);
                n.belief += 1;
            }
            Action::Duplicate(x) => {
                n.net_faults -= 1;
                Self::insert(&mut n.in_flight, x);
            }
            Action::Cut(x) => {
                // The connection broke before any byte of the body arrived.
                n.net_faults -= 1;
                n.cut_seen = true;
                Self::remove(&mut n.in_flight, x);
            }
            Action::Deliver(x) => {
                Self::remove(&mut n.in_flight, x);
                let stored = s.stored.len() as u8;
                let append = if self.fix_offset_check {
                    // MT2 `chunk`.
                    !s.whole && x == stored && x < SIZE
                } else {
                    !s.whole && stored < SIZE && x < SIZE
                };
                if append {
                    n.stored.push(x);
                    if n.stored.len() as u8 == SIZE {
                        // SHA-256 against the declared digest.
                        if n.stored.iter().enumerate().all(|(i, b)| *b == i as u8) {
                            n.whole = true;
                            n.confirmed = true;
                            if s.cut_seen {
                                n.saw_cut_then_whole = true;
                            }
                        } else {
                            n.stored.clear();
                        }
                    }
                }
            }
            Action::Complete => {
                let ok = if self.fix_complete_check { s.whole } else { true };
                if !ok {
                    return None; // 409
                }
                Self::set_status(&mut n, Status::Queued);
            }
            Action::LlmFails => {
                n.llm_fails -= 1;
                n.attempts += 1;
                Self::set_status(&mut n, Status::Queued);
            }
            Action::WorkerCrash => {
                n.crashes -= 1;
                n.step = Step::Idle;
                if s.lease == Lease::Worker {
                    n.lease = Lease::Dead;
                }
            }
            Action::OtherTakesGpu => {
                n.other_jobs -= 1;
                n.lease = Lease::Other;
            }
            Action::OtherReleasesGpu | Action::PruneDeadLease => n.lease = Lease::Free,
            Action::OwnerNamesSpeaker => {
                n.owner_acts -= 1;
                n.written = false;
                n.noted = false;
                Self::set_status(&mut n, Status::Queued);
            }
            Action::WorkerStep => match s.step {
                Step::Whisper => {
                    n.step = Step::Idle;
                    n.lease = Lease::Free;
                    n.transcribed = true;
                }
                Step::Remembered => {
                    n.step = Step::Idle;
                    n.remembered = true;
                }
                Step::Notified => {
                    n.step = Step::Idle;
                    n.notified = true;
                }
                Step::Asked => {
                    n.step = Step::Idle;
                    n.asked_done = true;
                }
                Step::Idle => {
                    if !s.transcribed {
                        Self::set_status(&mut n, Status::Transcribing);
                        if s.lease == Lease::Free {
                            n.lease = Lease::Worker;
                            n.step = Step::Whisper;
                        } else if !s.gpu_was_busy {
                            // Queued again for a minute (recorded once, for the witness).
                            n.gpu_was_busy = true;
                            Self::set_status(&mut n, Status::Queued);
                        } else {
                            return None; // retry in a minute: the same state
                        }
                    } else if !s.diarized {
                        Self::set_status(&mut n, Status::Diarizing);
                        n.diarized = true;
                    } else if !s.analyzed {
                        Self::set_status(&mut n, Status::Analyzing);
                        n.analyzed = true;
                        // The model failed on every attempt but the last: the fallback.
                        n.fallback = s.attempts + 1 >= self.max_attempts;
                    } else if !s.written {
                        Self::set_status(&mut n, Status::Publishing);
                        n.written = true;
                    } else if !s.noted {
                        Self::set_status(&mut n, Status::Publishing);
                        n.noted = true;
                    } else if !s.remembered {
                        Self::set_status(&mut n, Status::Publishing);
                        let send = !self.fix_markers || !s.mem_marker;
                        if self.fix_markers {
                            n.mem_marker = true;
                        }
                        if send {
                            n.memory_entries += 1;
                        }
                        n.step = Step::Remembered;
                    } else if !s.notified {
                        Self::set_status(&mut n, Status::Publishing);
                        let send = !self.fix_markers || !s.tg_marker;
                        if self.fix_markers {
                            n.tg_marker = true;
                        }
                        if send {
                            n.telegram += 1;
                        }
                        n.step = Step::Notified;
                    } else if !s.asked_done {
                        // The recurring unknown voice (MT6).
                        let ask = !self.fix_markers || !s.ask_marker;
                        if self.fix_markers {
                            n.ask_marker = true;
                        }
                        if ask {
                            n.questions += 1;
                        }
                        n.step = Step::Asked;
                    } else {
                        Self::set_status(&mut n, Status::Done);
                    }
                }
            },
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("stored_is_prefix", |_, s: &State| {
                s.stored.iter().enumerate().all(|(i, b)| *b == i as u8) && s.stored.len() as u8 <= SIZE
            }),
            Property::always("one_meeting_per_recording", |_, s: &State| s.meetings <= 1),
            Property::always("queued_only_when_whole", |_, s: &State| {
                s.status == Status::Uploading || s.whole
            }),
            Property::always("status_transitions_allowed", |_, s: &State| !s.bad_transition),
            Property::always("telegram_at_most_once", |_, s: &State| s.telegram <= 1),
            Property::always("memory_at_most_once", |_, s: &State| s.memory_entries <= 1),
            Property::always("voice_asked_at_most_once", |_, s: &State| s.questions <= 1),
            Property::always("done_has_every_output", |_, s: &State| {
                s.status != Status::Done
                    || (s.whole
                        && s.transcribed
                        && s.diarized
                        && s.analyzed
                        && s.written
                        && s.noted
                        && s.remembered
                        && s.notified)
            }),
            Property::eventually("finished_with_brief_note_and_notification", |_, s: &State| {
                s.status == Status::Done
                    && s.notified
                    && s.noted
                    && s.telegram >= 1
                    && s.lease != Lease::Worker
                    && s.lease != Lease::Dead
            }),
            Property::sometimes("resumed_after_a_cut", |_, s: &State| {
                s.saw_cut_then_whole && s.status == Status::Done
            }),
            Property::sometimes("fallback_brief_done", |_, s: &State| s.fallback && s.status == Status::Done),
            Property::sometimes("gpu_busy_then_done", |_, s: &State| s.gpu_was_busy && s.status == Status::Done),
        ]
    }
}

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let base = |offset: bool, idem: bool, complete: bool, markers: bool| Meetings {
        fix_offset_check: offset,
        fix_idempotency: idem,
        fix_complete_check: complete,
        fix_markers: markers,
        create_retries: 1,
        net_faults: if deep { 2 } else { 1 },
        crashes: 1,
        llm_fails: 1,
        other_jobs: 1,
        owner_acts: 1,
        max_attempts: 2,
    };
    let all_hold = vec![
        ("stored_is_prefix", Holds),
        ("one_meeting_per_recording", Holds),
        ("queued_only_when_whole", Holds),
        ("status_transitions_allowed", Holds),
        ("telegram_at_most_once", Holds),
        ("memory_at_most_once", Holds),
        ("voice_asked_at_most_once", Holds),
        ("done_has_every_output", Holds),
        ("finished_with_brief_note_and_notification", Holds),
        ("resumed_after_a_cut", Reached),
        ("fallback_brief_done", Reached),
        ("gpu_busy_then_done", Reached),
    ];
    vec![
        Case::new(PROTOCOL, "MT current code (kernel MT1-MT8)", base(true, true, true, true), None, all_hold),
        Case::new(
            PROTOCOL,
            "MT ablation: append at any offset",
            base(false, true, true, true),
            None,
            vec![("stored_is_prefix", Violated), ("one_meeting_per_recording", Holds)],
        ),
        Case::new(
            PROTOCOL,
            "MT ablation: create without the idempotency key",
            base(true, false, true, true),
            None,
            vec![("one_meeting_per_recording", Violated), ("stored_is_prefix", Holds)],
        ),
        Case::new(
            PROTOCOL,
            "MT ablation: complete without checking the tracks",
            base(true, true, false, true),
            None,
            vec![("queued_only_when_whole", Violated), ("done_has_every_output", Violated)],
        ),
        Case::new(
            PROTOCOL,
            "MT ablation: side effect before its marker",
            base(true, true, true, false),
            None,
            vec![
                ("telegram_at_most_once", Violated),
                ("memory_at_most_once", Violated),
                ("voice_asked_at_most_once", Violated),
                ("done_has_every_output", Holds),
            ],
        ),
    ]
}
