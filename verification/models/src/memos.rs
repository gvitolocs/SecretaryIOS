//! Voice memos end to end (docs/memos.md): the iPhone recorder and its Live
//! Activity, the upload queue, the server pipeline (lucina-memo worker), the
//! GPU lease (`gpu-lease`), the Obsidian vault, notifications.
//!
//! One recording, small budgets. Actors and what they may do:
//!
//! - the iPhone app: pause/resume (in the app or from the Live Activity, both
//!   through `MemoRecordingCommands`), stop, be killed by iOS at any time and
//!   relaunched; the upload queue sends the file (the network may fail) and the
//!   on-device transcript of a short memo;
//! - the worker: step 1 transcript (Whisper under a GPU lease, or the iPhone's
//!   transcript while the GPU is busy: `kernel::transcript_choice`), summary,
//!   PDF, Obsidian push, notification, done; the idle Whisper upgrade; it may
//!   crash (the systemd restart continues from the recorded steps);
//! - another GPU job taking and releasing the lease; `gpu-lease` prunes the
//!   lease of a dead holder on its next access;
//! - the owner: rename the memo, or reprocess it from the transcript.
//!
//! Switches (current code before 2026-10-25 vs the fixes):
//! - `fix_note_path`: the note's path is recorded before the rsync (kernel K7);
//! - `fix_no_downgrade`: step 1 never trades a Whisper transcript for the
//!   iPhone's (kernel K1);
//! - `fix_telegram_marker`: the Telegram revision is recorded before sending (K6);
//! - `fix_recovery`: a recording cut off by iOS killing the app is recovered and
//!   queued on the next launch (crash-safe CAF file);
//! - `prune_dead`: `gpu-lease` drops a dead holder (it does; switching it off
//!   shows the lease property depends on it).

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const PROTOCOL: &str = "memos";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rec {
    Recording,
    Paused,
    /// Stopped by the owner: the file is final and queued.
    Saved,
    /// iOS killed the app mid-recording; the file is cut off.
    Killed,
    /// The cut-off file was recovered and queued on relaunch.
    Recovered,
    /// The cut-off file can never be read or queued (the audio is lost).
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lease {
    Free,
    Worker,
    Other,
    /// Held by a worker process that crashed.
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Src {
    Device,
    Whisper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Idle,
    /// Whisper running under the lease.
    Whisper,
    /// rsync done, the meta not yet updated (current code: name not yet recorded).
    Pushed,
    /// Notification sent, `notified_at` not yet written.
    Sent,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    // iPhone
    pub rec: Rec,
    pub app_alive: bool,
    /// The Live Activity: shown, and its paused flag.
    pub la: Option<bool>,
    pub queued: bool,
    pub uploaded: bool,
    pub device_transcript_sent: bool,
    // server
    pub transcript: Option<Src>,
    pub had_whisper: bool,
    pub saw_device_then_whisper: bool,
    pub summarized: bool,
    pub rendered: bool,
    /// The note name recorded in meta (a title version).
    pub note_recorded: Option<u8>,
    pub published: bool,
    pub notified: bool,
    pub telegram_marker: Option<u8>,
    pub done: bool,
    pub revision: u8,
    pub title: u8,
    pub step: Step,
    pub lease: Lease,
    pub gpu_was_busy: bool,
    // the world
    /// Note names in the vault for this memo.
    pub vault: Vec<u8>,
    /// Telegram messages for the current revision; app inbox entries (dedup key = revision).
    pub telegram: u8,
    pub inbox: Vec<u8>,
    // budgets
    pub kills: u8,
    pub crashes: u8,
    pub net_fails: u8,
    pub owner_acts: u8,
    pub other_jobs: u8,
    pub toggles: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    TogglePauseInApp,
    TogglePauseFromLiveActivity,
    Stop,
    KillApp,
    Relaunch,
    UploadOk,
    UploadFails,
    SendDeviceTranscript,
    WorkerStep,
    WorkerCrash,
    OtherTakesGpu,
    OtherReleasesGpu,
    PruneDeadLease,
    OwnerRename,
    OwnerReprocess,
    Upgrade,
}

#[derive(Clone, Debug)]
pub struct Memos {
    pub fix_note_path: bool,
    pub fix_no_downgrade: bool,
    pub fix_telegram_marker: bool,
    pub fix_recovery: bool,
    pub prune_dead: bool,
    /// The memo is short (the iPhone transcribes it).
    pub short: bool,
    pub kills: u8,
    pub crashes: u8,
    pub net_fails: u8,
    pub owner_acts: u8,
    pub other_jobs: u8,
}

impl Memos {
    fn audio_on_server(s: &State) -> bool {
        s.uploaded
    }

    /// The first pipeline step still to do (the worker's order).
    fn pending_step(s: &State) -> Option<u8> {
        if !Self::audio_on_server(s) || s.done {
            return None;
        }
        Some(if s.transcript.is_none() {
            1
        } else if !s.summarized {
            2
        } else if !s.rendered {
            3
        } else if !s.published {
            4
        } else if !s.notified {
            5
        } else {
            6
        })
    }

    fn push_note(s: &mut State, name: u8) {
        if !s.vault.contains(&name) {
            s.vault.push(name);
            s.vault.sort();
        }
    }
}

impl Model for Memos {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            rec: Rec::Recording,
            app_alive: true,
            la: Some(false),
            queued: false,
            uploaded: false,
            device_transcript_sent: false,
            transcript: None,
            had_whisper: false,
            saw_device_then_whisper: false,
            summarized: false,
            rendered: false,
            note_recorded: None,
            published: false,
            notified: false,
            telegram_marker: None,
            done: false,
            revision: 0,
            title: 0,
            step: Step::Idle,
            lease: Lease::Free,
            gpu_was_busy: false,
            vault: vec![],
            telegram: 0,
            inbox: vec![],
            kills: self.kills,
            crashes: self.crashes,
            net_fails: self.net_fails,
            owner_acts: self.owner_acts,
            other_jobs: self.other_jobs,
            toggles: 2,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        let recording = matches!(s.rec, Rec::Recording | Rec::Paused);
        if s.app_alive && recording {
            if s.toggles > 0 {
                out.push(Action::TogglePauseInApp);
                if s.la.is_some() {
                    out.push(Action::TogglePauseFromLiveActivity);
                }
            }
            out.push(Action::Stop);
        }
        if s.app_alive && s.kills > 0 && s.rec != Rec::Lost {
            out.push(Action::KillApp);
        }
        if !s.app_alive {
            out.push(Action::Relaunch);
        }
        if s.app_alive && s.queued && !s.uploaded {
            out.push(Action::UploadOk);
            if s.net_fails > 0 {
                out.push(Action::UploadFails);
            }
        }
        if s.app_alive && self.short && s.queued && s.uploaded && !s.device_transcript_sent {
            out.push(Action::SendDeviceTranscript);
        }
        if Self::pending_step(s).is_some() || s.step != Step::Idle {
            out.push(Action::WorkerStep);
        }
        if s.crashes > 0 && (Self::pending_step(s).is_some() || s.step != Step::Idle) {
            out.push(Action::WorkerCrash);
        }
        if s.other_jobs > 0 && s.lease == Lease::Free {
            out.push(Action::OtherTakesGpu);
        }
        if s.lease == Lease::Other {
            out.push(Action::OtherReleasesGpu);
        }
        if self.prune_dead && s.lease == Lease::Dead {
            out.push(Action::PruneDeadLease);
        }
        // PATCH /v1/memos/{id} works in any status after the upload; reprocess once done.
        if s.owner_acts > 0 && s.uploaded {
            out.push(Action::OwnerRename);
        }
        if s.owner_acts > 0 && s.done {
            out.push(Action::OwnerReprocess);
        }
        if s.done && s.transcript == Some(Src::Device) && s.lease == Lease::Free {
            out.push(Action::Upgrade);
        }
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut n = s.clone();
        match a {
            Action::TogglePauseInApp | Action::TogglePauseFromLiveActivity => {
                // Both go through MemoRecorder.pause()/resume(), which update the activity.
                n.toggles -= 1;
                n.rec = if s.rec == Rec::Recording { Rec::Paused } else { Rec::Recording };
                n.la = Some(n.rec == Rec::Paused);
            }
            Action::Stop => {
                n.rec = Rec::Saved;
                n.la = None;
                n.queued = true;
            }
            Action::KillApp => {
                n.kills -= 1;
                n.app_alive = false;
                if matches!(s.rec, Rec::Recording | Rec::Paused) {
                    // The activity stays on the Lock Screen until the next launch ends it.
                    n.rec = Rec::Killed;
                }
            }
            Action::Relaunch => {
                n.app_alive = true;
                n.la = None; // endStaleActivities
                if s.rec == Rec::Killed {
                    if self.fix_recovery {
                        n.rec = Rec::Recovered;
                        n.queued = true;
                    } else {
                        n.rec = Rec::Lost;
                    }
                }
            }
            Action::UploadOk => n.uploaded = true,
            Action::UploadFails => n.net_fails -= 1,
            Action::SendDeviceTranscript => n.device_transcript_sent = true,
            Action::WorkerCrash => {
                n.crashes -= 1;
                if s.lease == Lease::Worker {
                    n.lease = Lease::Dead;
                }
                n.step = Step::Idle;
            }
            Action::OtherTakesGpu => {
                n.other_jobs -= 1;
                n.lease = Lease::Other;
            }
            Action::OtherReleasesGpu => n.lease = Lease::Free,
            Action::PruneDeadLease => n.lease = Lease::Free,
            Action::OwnerRename => {
                // A new title; the PDF and note follow, no new notification.
                n.owner_acts -= 1;
                n.title += 1;
                if s.rendered {
                    n.rendered = false;
                    n.published = false;
                    n.done = false;
                }
            }
            Action::OwnerReprocess => {
                // reprocess --transcript: step 1 again, new summary and notification.
                n.owner_acts -= 1;
                n.transcript = None;
                n.summarized = false;
                n.rendered = false;
                n.published = false;
                n.notified = false;
                n.done = false;
                n.revision += 1;
                n.telegram = 0;
            }
            Action::Upgrade => {
                // The idle worker: Whisper under the lease, then a quiet re-summary.
                n.transcript = Some(Src::Whisper);
                n.had_whisper = true;
                n.saw_device_then_whisper = true;
                n.summarized = false;
                n.rendered = false;
                n.published = false;
                n.done = false;
                n.revision += 1;
                n.telegram = 0;
                n.notified = true; // the owner was notified already
            }
            Action::WorkerStep => match s.step {
                Step::Whisper => {
                    n.step = Step::Idle;
                    n.lease = Lease::Free;
                    n.transcript = Some(Src::Whisper);
                    n.had_whisper = true;
                }
                Step::Pushed => {
                    n.step = Step::Idle;
                    n.note_recorded = Some(n.note_recorded.unwrap_or(s.title));
                    n.published = true;
                }
                Step::Sent => {
                    n.step = Step::Idle;
                    n.notified = true;
                }
                Step::Idle => match Self::pending_step(s)? {
                    1 => {
                        // `kernel::transcript_choice` over the reprocess's previous source.
                        let previous_whisper = s.had_whisper && self.fix_no_downgrade;
                        match s.lease {
                            Lease::Free => {
                                n.lease = Lease::Worker;
                                n.step = Step::Whisper;
                            }
                            _ => {
                                n.gpu_was_busy = true;
                                if s.device_transcript_sent && !previous_whisper {
                                    n.transcript = Some(Src::Device);
                                } else {
                                    return None; // wait (the retry is the same state)
                                }
                            }
                        }
                    }
                    2 => n.summarized = true,
                    3 => n.rendered = true,
                    4 => {
                        let name = s.note_recorded.unwrap_or(s.title);
                        if self.fix_note_path {
                            n.note_recorded = Some(name);
                        }
                        Self::push_note(&mut n, name);
                        n.step = Step::Pushed;
                    }
                    5 => {
                        let tg = !self.fix_telegram_marker || s.telegram_marker != Some(s.revision);
                        if self.fix_telegram_marker {
                            n.telegram_marker = Some(s.revision);
                        }
                        if tg {
                            n.telegram += 1;
                        }
                        if !n.inbox.contains(&s.revision) {
                            n.inbox.push(s.revision);
                        }
                        n.step = Step::Sent;
                    }
                    _ => n.done = true,
                },
            },
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no_duplicate_notes", |_, s: &State| s.vault.len() <= 1),
            Property::always("telegram_at_most_once", |_, s: &State| s.telegram <= 1),
            Property::always("app_inbox_once_per_revision", |_, s: &State| {
                let mut v = s.inbox.clone();
                v.dedup();
                v.len() == s.inbox.len()
            }),
            Property::always("whisper_never_downgraded", |_, s: &State| {
                !(s.had_whisper && s.transcript == Some(Src::Device))
            }),
            Property::always("no_lost_audio", |_, s: &State| s.rec != Rec::Lost),
            Property::always("live_activity_matches_recorder", |_, s: &State| {
                !s.app_alive
                    || match s.rec {
                        Rec::Recording => s.la == Some(false),
                        Rec::Paused => s.la == Some(true),
                        _ => s.la.is_none(),
                    }
            }),
            Property::always("done_has_every_output", |_, s: &State| {
                !s.done
                    || (s.transcript.is_some() && s.summarized && s.rendered && s.published && s.notified)
            }),
            Property::eventually("finished_with_one_note_pdf_notification", |_, s: &State| {
                s.done
                    && s.vault.len() == 1
                    && s.rendered
                    && !s.inbox.is_empty()
                    && s.lease != Lease::Dead
                    && s.lease != Lease::Worker
            }),
            Property::sometimes("gpu_busy_then_free", |_, s: &State| {
                s.gpu_was_busy && s.transcript == Some(Src::Whisper) && s.done
            }),
            Property::sometimes("device_transcript_replaced", |_, s: &State| {
                s.saw_device_then_whisper && s.done
            }),
        ]
    }
}

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let base = |fixed: bool, prune: bool| Memos {
        fix_note_path: fixed,
        fix_no_downgrade: fixed,
        fix_telegram_marker: fixed,
        fix_recovery: fixed,
        prune_dead: prune,
        short: true,
        kills: 1,
        crashes: if deep { 2 } else { 1 },
        net_fails: 1,
        owner_acts: 1,
        other_jobs: if deep { 2 } else { 1 },
    };
    let all = |bugs: Expect, live: Expect| {
        vec![
            ("no_duplicate_notes", bugs),
            ("telegram_at_most_once", bugs),
            ("app_inbox_once_per_revision", Holds),
            ("whisper_never_downgraded", bugs),
            ("no_lost_audio", bugs),
            ("live_activity_matches_recorder", Holds),
            ("done_has_every_output", Holds),
            ("finished_with_one_note_pdf_notification", live),
            ("gpu_busy_then_free", Reached),
            ("device_transcript_replaced", Reached),
        ]
    };
    vec![
        Case::new(PROTOCOL, "M current code (before the 2026-10-25 fixes)", base(false, true), None, all(Violated, Violated)),
        Case::new(PROTOCOL, "M + fixes (kernel K1/K6/K7, iPhone recovery)", base(true, true), None, all(Holds, Holds)),
        Case::new(
            PROTOCOL,
            "M fixed, gpu-lease without dead-holder pruning",
            base(true, false),
            None,
            vec![
                ("no_duplicate_notes", Holds),
                ("telegram_at_most_once", Holds),
                ("finished_with_one_note_pdf_notification", Violated),
            ],
        ),
    ]
}
