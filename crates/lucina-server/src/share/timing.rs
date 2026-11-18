//! The timing lines of one share: one per phase, one summary when its reply is handed over (`lucina_share::timing`).
//!
//! A line carries the share id, the phase name, its milliseconds, how it ended and the sizes it read
//! (bytes of a file, characters of text). Nothing else: a phase name is a fixed word from the code, and
//! the file name, the owner's note, the extracted text, the vision description and any URL stay out of
//! the lines. What a share spends its time on is then in the log itself, which is what the 177 s wait of
//! 2026-11-18 could not be answered from.
//!
//! The collector is `Clone` and shares one list, so the HTTP receive that stores the files, the
//! processing task that reads them and the chat turn they are handed to all end up in one summary.

use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How a phase ended. `Open`: it began and has not ended, which is what a wait that never finishes is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Open,
    Ok,
    Timeout,
    Error,
    Skipped,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Open => "open",
            Outcome::Ok => "ok",
            Outcome::Timeout => "timeout",
            Outcome::Error => "error",
            Outcome::Skipped => "skipped",
        }
    }

    /// The outcome of a call that failed or answered, and of nothing else.
    pub(crate) fn of<T, E>(result: &Result<T, E>) -> Outcome {
        if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        }
    }
}

/// The sizes one phase read. Numbers only, like every other field of these lines.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Sizes {
    pub(crate) bytes: Option<u64>,
    pub(crate) chars: Option<u64>,
}

impl Sizes {
    pub(crate) const NONE: Sizes = Sizes {
        bytes: None,
        chars: None,
    };

    pub(crate) fn bytes(bytes: u64) -> Sizes {
        Sizes {
            bytes: Some(bytes),
            chars: None,
        }
    }

    pub(crate) fn chars(chars: usize) -> Sizes {
        Sizes {
            bytes: None,
            chars: Some(chars as u64),
        }
    }
}

fn ms_between(from: Instant, to: Instant) -> u64 {
    to.checked_duration_since(from)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// A phase name is a word from the code; only letters, digits and `_` ever reach a line.
fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_');
    if s.is_empty() {
        return "phase".into();
    }
    s.chars().take(32).collect()
}

/// One phase of one share: when it ran, how it ended and what it read.
struct Phase {
    name: String,
    started: Instant,
    /// `None`: still running.
    ended: Option<Instant>,
    outcome: Outcome,
    sizes: Sizes,
}

impl Phase {
    /// Its milliseconds: how long it has run so far when it never ended.
    fn ms(&self) -> u64 {
        ms_between(self.started, self.ended.unwrap_or_else(Instant::now))
    }
}

struct Inner {
    /// The share's id; empty until it is known (a request whose id arrives with its own fields).
    share: Mutex<String>,
    /// `false`: a pipeline call outside a share job (a Telegram photo). It collects nothing to log.
    writes: bool,
    /// When this share's first known step began, for a summary that has no phase yet.
    started: Instant,
    phases: Mutex<Vec<Phase>>,
}

/// The phases of one share, in the order they began.
#[derive(Clone)]
pub(crate) struct ShareTiming {
    inner: Arc<Inner>,
}

impl Default for ShareTiming {
    /// A job read back from `status.json`: its phases are logged, its id is filled in by [`tie`](Self::tie).
    fn default() -> Self {
        Self::build(String::new(), true)
    }
}

impl ShareTiming {
    fn build(share: String, writes: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                share: Mutex::new(share),
                writes,
                started: Instant::now(),
                phases: Mutex::new(Vec::new()),
            }),
        }
    }

    pub(crate) fn new(share: &str) -> Self {
        Self::build(safe_id(share), true)
    }

    /// A read that belongs to no share (a photo from Telegram, a Drive file): no lines.
    pub(crate) fn silent() -> Self {
        Self::build(String::new(), false)
    }

    /// Give a timing the id of the share it turned out to belong to.
    pub(crate) fn tie(&self, share: &str) {
        let mut id = self.inner.share.lock().unwrap_or_else(|e| e.into_inner());
        if id.is_empty() {
            *id = safe_id(share);
        }
    }

    fn share(&self) -> String {
        self.inner
            .share
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Begins a phase, and returns the handle that ends it.
    ///
    /// `waiting`: the phase waits on something the share does not control (the single worker, the ingest
    /// lock, the GPU lease, the vision server coming up). Its line is then written as the wait begins too,
    /// so a wait that never ends leaves a trace instead of silence.
    pub(crate) fn begin(&self, name: &str, waiting: bool) -> Running {
        let name = slug(name);
        let started = Instant::now();
        let index = {
            let mut phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
            let index = phases.len();
            phases.push(Phase {
                name: name.clone(),
                started,
                ended: None,
                outcome: Outcome::Open,
                sizes: Sizes::NONE,
            });
            index
        };
        if waiting {
            self.write(&name, 0, Outcome::Open, Sizes::NONE);
        }
        Running {
            timing: self.clone(),
            index,
            name,
        }
    }

    /// A phase that ran before this timing existed: recorded and written now, with the start and end it
    /// really had. The multipart body of a share is read before its `shareId` field arrives.
    pub(crate) fn record(
        &self,
        name: &str,
        started: Instant,
        ended: Instant,
        outcome: Outcome,
        sizes: Sizes,
    ) {
        let name = slug(name);
        let ms = ms_between(started, ended);
        {
            let mut phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
            phases.push(Phase {
                name: name.clone(),
                started,
                ended: Some(ended),
                outcome,
                sizes,
            });
        }
        self.write(&name, ms, outcome, sizes);
    }

    fn finish(&self, index: usize, name: &str, outcome: Outcome, sizes: Sizes) {
        let ms = {
            let mut phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
            match phases.get_mut(index) {
                Some(phase) => {
                    phase.ended = Some(Instant::now());
                    phase.outcome = outcome;
                    phase.sizes = sizes;
                    phase.ms()
                }
                None => 0,
            }
        };
        self.write(name, ms, outcome, sizes);
    }

    /// One phase's line: id, name, milliseconds, outcome, and the sizes it read.
    fn write(&self, name: &str, ms: u64, outcome: Outcome, sizes: Sizes) {
        if !self.inner.writes {
            return;
        }
        let share = self.share();
        match (sizes.bytes, sizes.chars) {
            (Some(bytes), Some(chars)) => tracing::info!(
                target: "lucina_share::timing", share = %share, phase = name, ms,
                outcome = outcome.label(), bytes, chars, "lucina share phase"),
            (Some(bytes), None) => tracing::info!(
                target: "lucina_share::timing", share = %share, phase = name, ms,
                outcome = outcome.label(), bytes, "lucina share phase"),
            (None, Some(chars)) => tracing::info!(
                target: "lucina_share::timing", share = %share, phase = name, ms,
                outcome = outcome.label(), chars, "lucina share phase"),
            (None, None) => tracing::info!(
                target: "lucina_share::timing", share = %share, phase = name, ms,
                outcome = outcome.label(), "lucina share phase"),
        }
    }

    /// The phases in the order they began, each with the key it has in the summary: a name that repeats
    /// (two files in one share) is numbered.
    fn entries(&self) -> Vec<(String, u64, Outcome)> {
        let phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = std::collections::HashMap::<String, usize>::new();
        phases
            .iter()
            .map(|p| {
                let n = seen
                    .entry(p.name.clone())
                    .and_modify(|k| *k += 1)
                    .or_insert(1);
                let key = if *n == 1 {
                    p.name.clone()
                } else {
                    format!("{}_{}", p.name, n)
                };
                (key, p.ms(), p.outcome)
            })
            .collect()
    }

    /// Milliseconds from the beginning of the share's first phase to the end of its last one (to now,
    /// while one is still running). Always at least the sum of phases that did not overlap.
    pub(crate) fn total_ms(&self) -> u64 {
        let phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let first = phases
            .iter()
            .map(|p| p.started)
            .min()
            .unwrap_or(self.inner.started)
            .min(self.inner.started);
        let last = phases
            .iter()
            .filter_map(|p| p.ended)
            .max()
            .unwrap_or(now)
            .max(now);
        ms_between(first, last)
    }

    /// The per-phase milliseconds, in the order they ran, as a JSON array: `[{"phase":"ocr",
    /// "outcome":"ok","ms":3021}, …]`. Names, numbers and outcomes and nothing else, so the array cannot
    /// carry a file name or a text.
    pub(crate) fn summary(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .entries()
            .iter()
            .map(|(name, ms, outcome)| {
                serde_json::json!({"phase": name, "outcome": outcome.label(), "ms": ms})
            })
            .collect();
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
    }

    /// The one summary line of the share: its total, and every phase's milliseconds in order.
    pub(crate) fn log_total(&self, outcome: Outcome) {
        if !self.inner.writes {
            return;
        }
        tracing::info!(
            target: "lucina_share::timing", share = %self.share(), phase = "total",
            ms = self.total_ms(), outcome = outcome.label(),
            phases = %self.summary(), "lucina share timing");
    }
}

/// A phase that has begun and not yet ended.
pub(crate) struct Running {
    timing: ShareTiming,
    index: usize,
    name: String,
}

impl Running {
    /// Ends the phase and writes its line.
    pub(crate) fn finish(self, outcome: Outcome, sizes: Sizes) {
        self.timing.finish(self.index, &self.name, outcome, sizes);
    }
}

/// A share id is not a name, a path or a text, but it goes into a field: keep the characters a field allows.
fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn phases_beginning_in_order_are_summarised_in_that_order() {
        let timing = ShareTiming::new("share-order-1");
        for name in ["receive", "ocr", "vision", "memory"] {
            timing.begin(name, false).finish(Outcome::Ok, Sizes::NONE);
        }
        let keys: Vec<String> = timing
            .entries()
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect();
        assert_eq!(keys, ["receive", "ocr", "vision", "memory"]);
        let summary = timing.summary();
        for (before, after) in [("receive", "ocr"), ("ocr", "vision"), ("vision", "memory")] {
            let (i, j) = (summary.find(before).unwrap(), summary.find(after).unwrap());
            assert!(i < j, "{summary}");
        }
    }

    #[test]
    fn one_phase_per_file_in_a_share_keeps_its_own_slot_in_the_summary() {
        let timing = ShareTiming::new("share-twice-1");
        for _ in 0..2 {
            timing.begin("ocr", false).finish(Outcome::Ok, Sizes::NONE);
        }
        let keys: Vec<String> = timing
            .entries()
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect();
        assert_eq!(keys, ["ocr", "ocr_2"]);
    }

    #[test]
    fn the_total_reaches_the_end_of_every_phase_it_was_given() {
        let timing = ShareTiming::new("share-total-1");
        let first = Instant::now();
        // Three non-overlapping phases of a second each, back to back.
        for i in 0..3u64 {
            let started = first + Duration::from_secs(i);
            timing.record(
                "step",
                started,
                started + Duration::from_secs(1),
                Outcome::Ok,
                Sizes::NONE,
            );
        }
        assert!(
            timing.total_ms() >= 3_000,
            "the total is at least the sum of the phases that did not overlap: {}",
            timing.total_ms()
        );
    }

    #[test]
    fn a_phase_that_never_ends_is_open_in_the_summary_and_not_dropped() {
        let timing = ShareTiming::new("share-open-1");
        timing.begin("ocr", false).finish(Outcome::Ok, Sizes::NONE);
        // The vision server never becomes ready: the phase is left as it stands.
        let _waiting = timing.begin("vision_ready", true);
        let entries = timing.entries();
        assert_eq!(entries.len(), 2, "{entries:?}");
        let (name, _, outcome) = &entries[1];
        assert_eq!(name, "vision_ready");
        assert_eq!(*outcome, Outcome::Open);
        assert!(
            timing.summary().contains("\"open\""),
            "{}",
            timing.summary()
        );
    }

    #[test]
    fn the_summary_carries_nothing_but_the_names_numbers_and_outcomes_given() {
        let timing = ShareTiming::new("share-shape-1");
        timing.begin("ocr", false).finish(
            Outcome::Ok,
            Sizes {
                bytes: Some(4096),
                chars: Some(120),
            },
        );
        timing
            .begin("vision ready: ../../secrets", false)
            .finish(Outcome::Timeout, Sizes::NONE);
        let summary = timing.summary();
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&summary).expect("the summary is one JSON array");
        assert_eq!(entries.len(), 2, "{summary}");
        for entry in &entries {
            let object = entry.as_object().expect("an object per phase");
            let keys: Vec<&String> = object.keys().collect();
            assert_eq!(keys.len(), 3, "{summary}");
            for key in ["phase", "outcome", "ms"] {
                assert!(object.contains_key(key), "{summary}");
            }
            assert!(object["phase"].is_string(), "{summary}");
            assert!(object["outcome"].is_string(), "{summary}");
            assert!(object["ms"].is_u64(), "{summary}");
        }
        // A name is a word, never a path, a text or a note: only letters, digits and `_` survive, and at
        // most 32 of them.
        let phase = entries[1]["phase"].as_str().unwrap();
        assert!(
            phase.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "{summary}"
        );
        assert!(phase.chars().count() <= 32, "{summary}");
        assert!(!summary.contains('/'), "{summary}");
        // The sizes belong to the phase's own line; the summary is milliseconds and outcomes.
        assert!(
            !summary.contains("4096") && !summary.contains("120"),
            "{summary}"
        );
    }

    #[test]
    fn a_read_of_no_share_writes_nothing() {
        let timing = ShareTiming::silent();
        timing
            .begin("ocr", true)
            .finish(Outcome::Ok, Sizes::bytes(10));
        timing.log_total(Outcome::Ok);
        assert!(!timing.inner.writes, "silent collects no lines");
    }

    #[test]
    fn a_share_id_reaches_the_lines_unchanged_or_tied_in_later() {
        let timing = ShareTiming::new("ab12-34");
        assert_eq!(timing.share(), "ab12-34");
        let later = ShareTiming::default();
        later.tie("../../share id: note");
        let id = later.share();
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{id}"
        );
        assert!(!id.contains('.') && !id.contains(' '), "{id}");
        later.tie("other-id");
        assert_eq!(later.share(), id, "the id of a share is not rewritten");
    }
}
