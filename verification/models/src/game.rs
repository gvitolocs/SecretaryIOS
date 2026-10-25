//! Lucina's Pokémon Red game control (`rust/lucina-pokemon/src/main.rs` and `macros.rs` in the Lucina repo,
//! branch `lucina-game-link`). One emulator behind a mutex; the actors are:
//!
//! - the clock thread: advances a frame in real time while no macro runs, and checkpoints the save every
//!   30 s. The current code reads `busy` *before* taking the lock (`if !s.busy.load(..) { let mut e = lock; .. }`);
//! - macro callers (`POST /macro`): the play loop's Hermes turn, and a second caller (a turn still running
//!   after a restart, or another MCP client). `busy` is taken by compare-and-swap; a macro presses a key with
//!   `with(..)` (no `running` check), advances frames with `frames(..)` (checks `running` under the lock and
//!   aborts, lifting the key, when paused), lifts the key, and at the end saves and clears `busy`;
//! - pause/play writers (`POST /control`): Lucina's chat (Telegram, lucina-rs), the iPhone and web screens
//!   (secretary-server → lucina-rs), the Mac viewer over an SSH tunnel that can drop, and the play loop's
//!   auto-pause after three failed turns. The current code saves inside `/control` whatever a macro is doing;
//! - `POST /save`, the same;
//! - restarts of the game server (`running` is not persisted: it comes back paused, and every macro thread
//!   dies with the process).
//!
//! Properties: one driver (`one_driver`, `keys_only_by_busy_holder`), no clock frame while a macro holds a key
//! (`no_clock_frame_during_input`), a pause is honoured within one step (`pause_within_one_step`), saves never
//! interleave with input (`no_save_during_input`); witness: a pause during a battle, then a resume that presses
//! keys in the same battle (`paused_in_battle_then_resumed`).

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const WORKERS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Worker {
    Idle,
    /// Holds `busy`; `left` frames still to run.
    Running {
        left: u8,
    },
    /// Done or aborted (key lifted): saves and clears `busy` next.
    Ending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clock {
    Idle,
    /// Read `busy == false` outside the lock (current code); takes the lock next.
    SawFree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Chat,
    Iphone,
    Web,
    Viewer,
    AutoPause,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub running: bool,
    /// The worker that won the compare-and-swap on `busy`.
    pub busy: Option<u8>,
    /// The worker whose key is down on the joypad.
    pub key: Option<u8>,
    pub workers: [Worker; WORKERS],
    pub clock: Clock,
    pub in_battle: bool,
    /// The viewer's tunnel is up (its pause/play requests reach the server).
    pub tunnel: bool,
    /// Frames advanced with a key down after the last pause was applied.
    pub input_frames_paused: u8,
    pub clock_frame_during_input: bool,
    pub save_during_input: bool,
    /// Two workers held `busy` at once, or a key went down without `busy`.
    pub two_drivers: bool,
    pub paused_in_battle: bool,
    pub resumed_in_battle: bool,
    pub budget: u8,
    pub restarts: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Clock thread: read `busy` (current code) or do the whole step under the lock (fix).
    ClockRead,
    /// Clock thread, under the lock: advance a frame, maybe checkpoint.
    ClockFrame {
        checkpoint: bool,
    },
    MacroStart(u8),
    Press(u8),
    Frame(u8),
    Lift(u8),
    End(u8),
    Control(Source, bool),
    Save,
    TunnelDrop,
    TunnelUp,
    BattleEnds,
    Restart,
}

#[derive(Clone, Debug)]
pub struct Game {
    pub budget: u8,
    /// Frames per macro.
    pub frames: u8,
    /// Fix Pk1: the clock thread checks `busy` after taking the emulator lock.
    pub fix_clock: bool,
    /// Fix Pk2: `/control` and `/save` do not save while a macro holds `busy` (the macro saves when it ends).
    pub fix_save: bool,
}

impl Game {
    fn save(&self, s: &mut State) {
        if s.key.is_some() {
            s.save_during_input = true;
        }
    }

    fn handler_save(&self, s: &mut State) {
        if !self.fix_save || s.busy.is_none() {
            self.save(s);
        }
    }

    fn running_workers(s: &State) -> usize {
        s.workers
            .iter()
            .filter(|w| !matches!(w, Worker::Idle))
            .count()
    }

    fn frame(&self, s: &mut State, by_clock: bool) {
        if s.key.is_some() {
            if by_clock {
                s.clock_frame_during_input = true;
            }
            if !s.running {
                s.input_frames_paused = s.input_frames_paused.saturating_add(1);
            }
        }
    }
}

impl Model for Game {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            running: true,
            busy: None,
            key: None,
            workers: [Worker::Idle; WORKERS],
            clock: Clock::Idle,
            in_battle: false,
            tunnel: true,
            input_frames_paused: 0,
            clock_frame_during_input: false,
            save_during_input: false,
            two_drivers: false,
            paused_in_battle: false,
            resumed_in_battle: false,
            budget: self.budget,
            restarts: 1,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        match s.clock {
            Clock::Idle if s.busy.is_none() || self.fix_clock => out.push(Action::ClockRead),
            Clock::SawFree => {
                out.push(Action::ClockFrame { checkpoint: false });
                out.push(Action::ClockFrame { checkpoint: true });
            }
            _ => {}
        }
        for w in 0..WORKERS as u8 {
            match s.workers[w as usize] {
                Worker::Idle if s.budget > 0 => out.push(Action::MacroStart(w)),
                Worker::Running { .. } => {
                    if s.key.is_none() {
                        out.push(Action::Press(w));
                    }
                    out.push(Action::Frame(w));
                    if s.key == Some(w) {
                        out.push(Action::Lift(w));
                    }
                }
                Worker::Ending => out.push(Action::End(w)),
                _ => {}
            }
        }
        if s.budget > 0 {
            for src in [Source::Chat, Source::Iphone, Source::Web, Source::Viewer] {
                if src == Source::Viewer && !s.tunnel {
                    continue;
                }
                out.push(Action::Control(src, !s.running));
            }
            // The play loop pauses itself after three failed turns.
            if s.running {
                out.push(Action::Control(Source::AutoPause, false));
            }
            out.push(Action::Save);
        }
        out.push(if s.tunnel {
            Action::TunnelDrop
        } else {
            Action::TunnelUp
        });
        if s.in_battle {
            out.push(Action::BattleEnds);
        }
        if s.restarts > 0 {
            out.push(Action::Restart);
        }
    }

    fn next_state(&self, last: &State, a: Action) -> Option<State> {
        let mut s = last.clone();
        match a {
            Action::ClockRead => {
                if self.fix_clock {
                    // Pk1: the whole step under the lock, `busy` read there.
                    if s.busy.is_none() {
                        self.frame(&mut s, true);
                        self.save(&mut s);
                    }
                } else {
                    s.clock = Clock::SawFree;
                }
            }
            Action::ClockFrame { checkpoint } => {
                self.frame(&mut s, true);
                if checkpoint {
                    self.save(&mut s);
                }
                s.clock = Clock::Idle;
            }
            Action::MacroStart(w) => {
                s.budget -= 1;
                if s.busy.is_some() {
                    return Some(s); // "Another action is still running."
                }
                s.busy = Some(w);
                s.workers[w as usize] = Worker::Running { left: self.frames };
            }
            Action::Press(w) => {
                if s.busy != Some(w) {
                    s.two_drivers = true;
                }
                s.key = Some(w);
                if s.in_battle && s.running && s.paused_in_battle {
                    s.resumed_in_battle = true;
                }
            }
            Action::Frame(w) => {
                let Worker::Running { left } = s.workers[w as usize] else {
                    return None;
                };
                if !s.running {
                    // frames(): "Paused by the owner" → the caller lifts its key and returns.
                    if s.key == Some(w) {
                        s.key = None;
                    }
                    s.workers[w as usize] = Worker::Ending;
                } else {
                    self.frame(&mut s, false);
                    // A frame with input in the overworld may walk into a battle.
                    if s.key == Some(w) && !s.in_battle && left % 2 == 0 {
                        s.in_battle = true;
                    }
                    s.workers[w as usize] = if left <= 1 {
                        if s.key == Some(w) {
                            s.key = None;
                        }
                        Worker::Ending
                    } else {
                        Worker::Running { left: left - 1 }
                    };
                }
            }
            Action::Lift(w) => {
                if s.key == Some(w) {
                    s.key = None;
                }
            }
            Action::End(w) => {
                self.save(&mut s);
                s.busy = None;
                s.workers[w as usize] = Worker::Idle;
            }
            Action::Control(_, run) => {
                s.budget -= 1;
                s.running = run;
                // Counted from the latest pause or play command.
                s.input_frames_paused = 0;
                if !run && s.in_battle {
                    s.paused_in_battle = true;
                }
                self.handler_save(&mut s);
            }
            Action::Save => {
                s.budget -= 1;
                self.handler_save(&mut s);
            }
            Action::TunnelDrop => s.tunnel = false,
            Action::TunnelUp => s.tunnel = true,
            Action::BattleEnds => s.in_battle = false,
            Action::Restart => {
                // The process dies: macro threads and the clock with it; `running` comes back false.
                s.restarts -= 1;
                s.running = false;
                s.input_frames_paused = 0;
                s.busy = None;
                s.key = None;
                s.workers = [Worker::Idle; WORKERS];
                s.clock = Clock::Idle;
            }
        }
        if Game::running_workers(&s) > 1 {
            s.two_drivers = true;
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("one_driver", |_, s: &State| !s.two_drivers),
            Property::always("keys_only_by_busy_holder", |_, s: &State| {
                s.key.is_none() || s.key == s.busy
            }),
            Property::always("no_clock_frame_during_input", |_, s: &State| {
                !s.clock_frame_during_input
            }),
            Property::always("pause_within_one_step", |_, s: &State| {
                s.input_frames_paused <= 1
            }),
            Property::always("no_save_during_input", |_, s: &State| !s.save_during_input),
            Property::sometimes("paused_in_battle_then_resumed", |_, s: &State| {
                s.resumed_in_battle
            }),
        ]
    }
}

pub const PROTOCOL: &str = "game";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let budget = if deep { 7 } else { 6 };
    // The clock race also breaks the pause: while paused, every macro call of a turn still running presses a
    // key before its first `frames(..)` check, and the clock can advance a frame with it.
    let all = |clock: Expect, save: Expect| {
        vec![
            ("one_driver", Holds),
            ("keys_only_by_busy_holder", Holds),
            ("no_clock_frame_during_input", clock),
            ("pause_within_one_step", clock),
            ("no_save_during_input", save),
            ("paused_in_battle_then_resumed", Reached),
        ]
    };
    vec![
        Case::new(
            PROTOCOL,
            "Pk game control, lucina-game-link 2f837e0",
            Game {
                budget,
                frames: 3,
                fix_clock: false,
                fix_save: false,
            },
            None,
            all(Violated, Violated),
        ),
        Case::new(
            PROTOCOL,
            "Pk + fixes Pk1 (clock under lock), Pk2 (no save while busy)",
            Game {
                budget,
                frames: 3,
                fix_clock: true,
                fix_save: true,
            },
            None,
            all(Holds, Holds),
        ),
    ]
}
