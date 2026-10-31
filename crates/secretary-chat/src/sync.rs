//! `ChatSync.run` of the iPhone app (`ios/Secretary/Features/Chat/ChatSync.swift`) as a pure state machine.
//!
//! The Swift caller is a thin loop: ask the plan what to do, do it, report what happened.
//!
//! ```text
//! plan = SyncPlan::new(cursor); outcome = Started
//! loop { match plan.next(outcome) {
//!     Fetch { after, limit } => outcome = fetch(after, limit): Page { cursor, has_more } | Gone410 | Failed
//!     Apply { reset }        => apply(the page just fetched, reset); outcome = Applied (or Failed)
//!     Done                   => return
//!     Abort                  => throw the error of the Failed outcome
//! } }
//! ```
//!
//! The rules (the Swift code, line for line):
//! - No cursor: one `Fetch { after: None, limit: 50 }`, applied with `reset = false`, then done.
//! - With a cursor: up to 10 times `Fetch { after: cursor, limit: 200 }`, each applied with `reset = false`. Stop
//!   when the page has no more; leave the loop when the page's cursor is missing or equals the current one (a
//!   stuck cursor) or after the tenth page; otherwise move the cursor. A 410 (the cursor expired) leaves the loop;
//!   any other error aborts the whole run.
//! - Leaving the loop unfinished means a fresh window: `Fetch { after: None, limit: 50 }`, applied with
//!   `reset = true` (the store then drops its old server items and the stored cursor).
//!
//! Proved in `verification/verus/chat_merge.rs` (C1-C3) on the same transition function with `u64` cursors; the
//! differential test `tests/verus_chat_merge.rs` runs this code against the proof file's function bodies.
use serde::{Deserialize, Serialize};

/// Page size of the first fetch and of the fresh window.
pub const FIRST_LIMIT: u32 = 50;
/// Page size of a fetch after a cursor.
pub const PAGE_LIMIT: u32 = 200;
/// The cursor loop's page cap.
pub const MAX_PAGES: u32 = 10;

/// Where the plan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Nothing asked yet.
    Begin,
    /// A fetch is out.
    Fetching,
    /// A fetched page is being applied.
    Applying,
    Finished,
    Aborted,
}

/// What the caller reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// The first call.
    Started,
    /// The fetch succeeded: the page's cursor and whether the server has more.
    Page {
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        has_more: bool,
    },
    /// The page was applied.
    Applied,
    /// The server answered 410: the cursor expired.
    Gone410,
    /// Any other error, from the fetch or from applying.
    Failed,
}

/// What the caller does next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Ask the server for the items after `after` (`None`: the newest window), at most `limit`.
    Fetch { after: Option<String>, limit: u32 },
    /// Apply the page just fetched; `reset` means drop the store's old server items and cursor first.
    Apply { reset: bool },
    Done,
    /// Stop and throw the error of the `Failed` outcome (no further fetches).
    Abort,
}

/// The whole state of one run; it round-trips as JSON between calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    phase: Phase,
    /// The run started with a stored cursor (so a fresh window can follow).
    had_cursor: bool,
    /// The fetch in flight, or the page being applied, is the fresh window.
    fresh: bool,
    /// The cursor the next loop fetch asks after.
    cursor: Option<String>,
    /// Loop fetches issued so far.
    pages: u32,
    /// Fetches issued so far, the fresh window included.
    fetches: u32,
    /// The page being applied: its cursor and `has_more`.
    page_cursor: Option<String>,
    page_more: bool,
}

impl SyncPlan {
    /// A plan for a store whose last stored cursor is `cursor`.
    pub fn new(cursor: Option<String>) -> SyncPlan {
        SyncPlan {
            phase: Phase::Begin,
            had_cursor: cursor.is_some(),
            fresh: false,
            cursor,
            pages: 0,
            fetches: 0,
            page_cursor: None,
            page_more: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Fetches issued so far (at most 11).
    pub fn fetches(&self) -> u32 {
        self.fetches
    }

    fn abort(&mut self) -> Action {
        self.phase = Phase::Aborted;
        Action::Abort
    }

    fn finish(&mut self) -> Action {
        self.phase = Phase::Finished;
        Action::Done
    }

    fn fetch_loop(&mut self) -> Action {
        self.phase = Phase::Fetching;
        self.pages += 1;
        self.fetches += 1;
        Action::Fetch { after: self.cursor.clone(), limit: PAGE_LIMIT }
    }

    fn fetch_fresh(&mut self) -> Action {
        self.phase = Phase::Fetching;
        self.fresh = true;
        self.fetches += 1;
        Action::Fetch { after: None, limit: FIRST_LIMIT }
    }

    /// Leave the loop: a fresh window replaces what is on disk.
    fn leave_loop(&mut self) -> Action {
        self.fetch_fresh()
    }

    /// The next step, given what happened. Anything out of order aborts.
    pub fn next(&mut self, outcome: Outcome) -> Action {
        match (self.phase, outcome) {
            (Phase::Finished, _) => Action::Done,
            (Phase::Aborted, _) | (_, Outcome::Failed) => self.abort(),
            (Phase::Begin, Outcome::Started) => {
                if self.had_cursor {
                    self.fetch_loop()
                } else {
                    self.phase = Phase::Fetching;
                    self.fetches += 1;
                    Action::Fetch { after: None, limit: FIRST_LIMIT }
                }
            }
            (Phase::Fetching, Outcome::Page { cursor, has_more }) => {
                self.page_cursor = cursor;
                self.page_more = has_more;
                self.phase = Phase::Applying;
                Action::Apply { reset: self.fresh }
            }
            (Phase::Fetching | Phase::Applying, Outcome::Gone410) => {
                // Only the cursor loop catches a 410; the first fetch and the fresh window throw it.
                if self.had_cursor && !self.fresh {
                    self.leave_loop()
                } else {
                    self.abort()
                }
            }
            (Phase::Applying, Outcome::Applied) => {
                if self.fresh || !self.had_cursor || !self.page_more {
                    return self.finish();
                }
                match self.page_cursor.clone() {
                    Some(next) if self.cursor.as_deref() != Some(next.as_str()) => {
                        self.cursor = Some(next);
                        if self.pages >= MAX_PAGES {
                            self.leave_loop()
                        } else {
                            self.fetch_loop()
                        }
                    }
                    _ => self.leave_loop(),
                }
            }
            _ => self.abort(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(cursor: &str, more: bool) -> Outcome {
        Outcome::Page { cursor: Some(cursor.into()), has_more: more }
    }
    fn fetch(after: Option<&str>, limit: u32) -> Action {
        Action::Fetch { after: after.map(String::from), limit }
    }

    /// Drives a plan against a scripted server: `serve(after)` answers a fetch.
    fn run(
        cursor: Option<&str>,
        mut serve: impl FnMut(Option<&str>, u32) -> Outcome,
    ) -> (Vec<Action>, Vec<bool>) {
        let mut plan = SyncPlan::new(cursor.map(String::from));
        let mut outcome = Outcome::Started;
        let mut actions = vec![];
        let mut resets = vec![];
        loop {
            let action = plan.next(outcome.clone());
            actions.push(action.clone());
            outcome = match action {
                Action::Fetch { after, limit } => serve(after.as_deref(), limit),
                Action::Apply { reset } => {
                    resets.push(reset);
                    Outcome::Applied
                }
                Action::Done | Action::Abort => return (actions, resets),
            };
        }
    }

    // Ported from ios/SecretaryTests/ChatSyncTests.swift (ChatCursorSyncTests).

    #[test]
    fn cursor_loop_and_query() {
        let (actions, resets) = run(Some("c0"), |after, limit| {
            assert_eq!(limit, 200);
            match after {
                Some("c0") => page("c1", true),
                Some("c1") => page("c2", false),
                other => panic!("unexpected {other:?}"),
            }
        });
        assert_eq!(
            actions,
            [
                fetch(Some("c0"), 200),
                Action::Apply { reset: false },
                fetch(Some("c1"), 200),
                Action::Apply { reset: false },
                Action::Done
            ]
        );
        assert_eq!(resets, [false, false]);
    }

    #[test]
    fn ten_call_cap_then_fresh_window() {
        let mut calls = 0;
        let (actions, resets) = run(Some("start"), |after, limit| {
            calls += 1;
            if calls <= 10 {
                assert!(after.is_some());
                return page(&format!("c{calls}"), true);
            }
            assert_eq!((after, limit), (None, 50));
            Outcome::Page { cursor: Some("fresh".into()), has_more: false }
        });
        assert_eq!(calls, 11);
        let mut want = vec![false; 10];
        want.push(true);
        assert_eq!(resets, want);
        assert_eq!(actions.last(), Some(&Action::Done));
        // The tenth page's cursor was followed, not asked: the fresh window comes right after.
        let fetches: Vec<_> = actions.iter().filter(|a| matches!(a, Action::Fetch { .. })).collect();
        assert_eq!(fetches[9], &fetch(Some("c9"), 200));
        assert_eq!(fetches[10], &fetch(None, 50));
    }

    #[test]
    fn expired_cursor_goes_to_a_fresh_window() {
        let (actions, resets) = run(Some("old"), |after, _| match after {
            Some(_) => Outcome::Gone410,
            None => Outcome::Page { cursor: Some("fresh".into()), has_more: false },
        });
        assert_eq!(resets, [true]);
        assert_eq!(
            actions,
            [fetch(Some("old"), 200), fetch(None, 50), Action::Apply { reset: true }, Action::Done]
        );
    }

    #[test]
    fn without_cursor_fetches_one_small_page() {
        let (actions, resets) = run(None, |after, limit| {
            assert_eq!((after, limit), (None, 50));
            Outcome::Page { cursor: Some("first".into()), has_more: true }
        });
        assert_eq!(actions, [fetch(None, 50), Action::Apply { reset: false }, Action::Done]);
        assert_eq!(resets, [false]);
    }

    // The rules the Swift tests do not name.

    #[test]
    fn a_stuck_cursor_leaves_the_loop() {
        for stuck in [Some("c0"), None] {
            let (actions, resets) = run(Some("c0"), |after, _| match after {
                Some(_) => Outcome::Page { cursor: stuck.map(String::from), has_more: true },
                None => page("fresh", false),
            });
            assert_eq!(resets, [false, true], "{stuck:?}");
            assert_eq!(actions.iter().filter(|a| matches!(a, Action::Fetch { .. })).count(), 2);
        }
    }

    #[test]
    fn a_page_without_more_ends_the_run_whatever_its_cursor() {
        for c in [None, Some("c0"), Some("c9")] {
            let (actions, resets) = run(Some("c0"), |_, _| Outcome::Page { cursor: c.map(String::from), has_more: false });
            assert_eq!(actions.len(), 3);
            assert_eq!(resets, [false]);
        }
    }

    #[test]
    fn another_error_aborts_without_further_fetches() {
        // From the first fetch, a later page, the apply, the first (cursor-less) fetch and the fresh window.
        let (actions, _) = run(Some("c0"), |_, _| Outcome::Failed);
        assert_eq!(actions, [fetch(Some("c0"), 200), Action::Abort]);

        let mut calls = 0;
        let (actions, _) = run(Some("c0"), |_, _| {
            calls += 1;
            if calls == 1 {
                page("c1", true)
            } else {
                Outcome::Failed
            }
        });
        assert_eq!(actions.last(), Some(&Action::Abort));
        assert_eq!(calls, 2);

        let mut plan = SyncPlan::new(Some("c0".into()));
        plan.next(Outcome::Started);
        plan.next(page("c1", true));
        assert_eq!(plan.next(Outcome::Failed), Action::Abort, "a failed apply");
        assert_eq!(plan.next(Outcome::Applied), Action::Abort, "stays aborted");

        let (actions, _) = run(None, |_, _| Outcome::Gone410);
        assert_eq!(actions, [fetch(None, 50), Action::Abort], "410 is an error outside the cursor loop");

        let (actions, resets) = run(Some("old"), |_, _| Outcome::Gone410);
        assert_eq!(actions, [fetch(Some("old"), 200), fetch(None, 50), Action::Abort]);
        assert!(resets.is_empty(), "410 on the fresh window aborts");
    }

    #[test]
    fn a_410_while_applying_leaves_the_loop_too() {
        let mut plan = SyncPlan::new(Some("c0".into()));
        plan.next(Outcome::Started);
        plan.next(page("c1", true));
        assert_eq!(plan.next(Outcome::Gone410), fetch(None, 50));
    }

    #[test]
    fn out_of_order_outcomes_abort() {
        let mut plan = SyncPlan::new(None);
        assert_eq!(plan.next(Outcome::Applied), Action::Abort);
        let mut plan = SyncPlan::new(None);
        plan.next(Outcome::Started);
        assert_eq!(plan.next(Outcome::Started), Action::Abort);
        let mut plan = SyncPlan::new(None);
        plan.next(Outcome::Started);
        plan.next(page("a", false));
        assert_eq!(plan.next(page("b", false)), Action::Abort);
        let mut plan = SyncPlan::new(None);
        assert_eq!(plan.next(page("a", false)), Action::Abort);
    }

    #[test]
    fn a_finished_plan_stays_done() {
        let mut plan = SyncPlan::new(None);
        plan.next(Outcome::Started);
        plan.next(page("a", false));
        assert_eq!(plan.next(Outcome::Applied), Action::Done);
        assert_eq!(plan.next(Outcome::Started), Action::Done);
        assert_eq!(plan.phase(), Phase::Finished);
    }

    #[test]
    fn the_plan_round_trips_as_json_between_calls() {
        let mut plan = SyncPlan::new(Some("c0".into()));
        plan.next(Outcome::Started);
        let json = serde_json::to_string(&plan).unwrap();
        let mut again: SyncPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(again, plan);
        assert_eq!(again.next(page("c1", true)), plan.next(page("c1", true)));
    }

    #[test]
    fn never_more_than_eleven_fetches() {
        // Every server answer, in every order of 410, stuck and has-more pages, for a few hundred scripts.
        for script in 0u32..4096 {
            let mut plan = SyncPlan::new(Some("c0".into()));
            let mut outcome = Outcome::Started;
            let mut n = 0u32;
            let mut s = script;
            loop {
                let action = plan.next(outcome);
                match action {
                    Action::Fetch { .. } => {
                        let pick = s % 4;
                        s /= 4;
                        n += 1;
                        outcome = match pick {
                            0 => page(&format!("p{n}"), true),
                            1 => page("c0", true),
                            2 => Outcome::Gone410,
                            _ => page(&format!("q{n}"), false),
                        };
                    }
                    Action::Apply { .. } => outcome = Outcome::Applied,
                    Action::Done | Action::Abort => break,
                }
            }
            assert!(plan.fetches() <= 11, "script {script}: {}", plan.fetches());
        }
    }
}
