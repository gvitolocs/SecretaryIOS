//! The timing lines of one share: one per phase, and one summary line for the share itself
//! (`lucina_share::timing`). The summary is written when the share names its outcome — the receive that
//! stored the files, the worker that read them — and, if nobody ever does, when the last reference to
//! its collector goes away: an error return, a job dropped, a worker that panicked.
//!
//! A line carries the share id, the phase name, its milliseconds, how it ended and the sizes it read
//! (bytes of a file, characters of text). Nothing else: a phase name is a fixed word from the code, and
//! the file name, the owner's note, the extracted text, the vision description and any URL stay out of
//! the lines. What a share spends its time on is then in the log itself, which is what the 177 s wait of
//! 2026-11-18 could not be answered from.
//!
//! The collector is `Clone` and shares one list, so the HTTP receive that stores the files, the
//! processing task that reads them and the chat turn they are handed to all end up in one summary.
//! Nothing here changes what a share does. A phase that is left without its `finish` — an early `?`, a
//! budget that drops the call it wrapped — is ended by the handle's `Drop` as `dropped`, so it leaves a
//! line and stops counting instead of stretching to the end of the share.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

/// How a phase ended. `Open`: it began and has not ended, which is what a wait that never finishes is.
/// `Dropped`: its handle went away without an outcome, which is what an early return does to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Open,
    Ok,
    Timeout,
    Error,
    Skipped,
    Dropped,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Open => "open",
            Outcome::Ok => "ok",
            Outcome::Timeout => "timeout",
            Outcome::Error => "error",
            Outcome::Skipped => "skipped",
            Outcome::Dropped => "dropped",
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

/// One phase of one share, in the milliseconds its share had been alive when it ran. Numbers, because an
/// `Instant` of one task and one of another are not comparable.
struct Phase {
    name: String,
    started: u64,
    /// `None`: still running.
    ended: Option<u64>,
    outcome: Outcome,
    sizes: Sizes,
}

impl Phase {
    /// Its milliseconds: how long it has run so far when it never ended.
    fn ms(&self, now: u64) -> u64 {
        self.ended.unwrap_or(now).saturating_sub(self.started)
    }
}

struct Inner {
    /// The share's id; empty until it is known (a request whose id arrives with its own fields).
    share: Mutex<String>,
    /// `false`: a pipeline call outside a share job (a Telegram photo). It collects nothing to log.
    writes: bool,
    /// When this share's first known step began, which every phase's milliseconds count from.
    started: Instant,
    phases: Mutex<Vec<Phase>>,
    /// `true` once the summary line of this share has been written, by hand or by the `Drop` below.
    summarised: AtomicBool,
}

impl Inner {
    fn id(&self) -> String {
        self.share.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Milliseconds from the beginning of this share to `at` (0, when `at` is before it).
    fn ms_from(&self, at: Instant) -> u64 {
        at.checked_duration_since(self.started)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default()
    }

    fn now_ms(&self) -> u64 {
        self.ms_from(Instant::now())
    }

    /// One phase's line: id, name, milliseconds, outcome, and the sizes it read.
    fn write(&self, name: &str, ms: u64, outcome: Outcome, sizes: Sizes) {
        if !self.writes {
            return;
        }
        let share = self.id();
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
        let now = self.now_ms();
        let phases = self.phases.lock().unwrap_or_else(|e| e.into_inner());
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
                (key, p.ms(now), p.outcome)
            })
            .collect()
    }

    /// Milliseconds from the beginning of this share to the later of the end of its last phase and now,
    /// so the total covers the quiet between the phases too. A phase that has ended keeps the
    /// milliseconds it ran for; only one still running reaches now.
    fn total_ms(&self) -> u64 {
        if !self.writes {
            return 0;
        }
        let last = self
            .phases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|p| p.ended)
            .max()
            .unwrap_or_default();
        self.now_ms().max(last)
    }

    /// The per-phase milliseconds, in the order they ran, as a JSON array: `[{"phase":"ocr",
    /// "outcome":"ok","ms":3021}, …]`. Names, numbers and outcomes and nothing else, so the array cannot
    /// carry a file name or a text.
    fn summary(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .entries()
            .iter()
            .map(|(name, ms, outcome)| {
                serde_json::json!({"phase": name, "outcome": outcome.label(), "ms": ms})
            })
            .collect();
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".into())
    }

    /// The one summary line of the share: its total, and every phase's milliseconds in order. Whoever
    /// comes first writes it, and the share never gets a second one.
    fn log_total(&self, outcome: Outcome) {
        if !self.writes || self.summarised.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::info!(
            target: "lucina_share::timing", share = %self.id(), phase = "total",
            ms = self.total_ms(), outcome = outcome.label(),
            phases = %self.summary(), "lucina share timing");
    }
}

impl Drop for Inner {
    /// The end of a share nobody summarised — an error return, a job dropped on the floor, a worker that
    /// panicked — is itself an outcome: the line it leaves says `error`.
    fn drop(&mut self) {
        self.log_total(Outcome::Error);
    }
}

/// The phases of one share, in the order they began.
#[derive(Clone)]
pub(crate) struct ShareTiming {
    inner: Arc<Inner>,
}

/// A collector that belongs to no share, for a pipeline call nobody will ever summarise (a photo from
/// Telegram, one frame of a video). One value for the whole process, so it costs nothing to hand out.
static SILENT: LazyLock<ShareTiming> = LazyLock::new(|| ShareTiming::build(String::new(), false));

impl Default for ShareTiming {
    /// A job read back from `status.json`: its phases are logged, its id is filled in by [`tie`](Self::tie).
    fn default() -> Self {
        Self::build(String::new(), true)
    }
}

impl ShareTiming {
    fn build(share: String, writes: bool) -> Self {
        ShareTiming {
            inner: Arc::new(Inner {
                share: Mutex::new(share),
                writes,
                started: Instant::now(),
                phases: Mutex::new(Vec::new()),
                summarised: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn new(share: &str) -> Self {
        Self::build(safe_id(share), true)
    }

    /// A read that belongs to no share (a photo from Telegram, a Drive file): no lines, and nothing
    /// allocated for it.
    pub(crate) fn silent() -> &'static ShareTiming {
        &SILENT
    }

    /// Give a timing the id of the share it turned out to belong to.
    pub(crate) fn tie(&self, share: &str) {
        let mut id = self.inner.share.lock().unwrap_or_else(|e| e.into_inner());
        if id.is_empty() {
            *id = safe_id(share);
        }
    }

    /// Begins a phase, and returns the handle that ends it.
    ///
    /// `waiting`: the phase waits on something the share does not control (the single worker, the ingest
    /// lock, the GPU lease, the vision server coming up). Its line is then written as the wait begins too,
    /// so a wait that never ends leaves a trace instead of silence.
    pub(crate) fn begin(&self, name: &str, waiting: bool) -> Running {
        if !self.inner.writes {
            return Running::none();
        }
        let name = slug(name);
        let index = {
            let started = self.inner.now_ms();
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
            self.inner.write(&name, 0, Outcome::Open, Sizes::NONE);
        }
        Running {
            timing: Some(self.clone()),
            index,
            name,
            done: false,
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
        if !self.inner.writes {
            return;
        }
        let name = slug(name);
        let (first, last) = (self.inner.ms_from(started), self.inner.ms_from(ended));
        {
            let mut phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
            phases.push(Phase {
                name: name.clone(),
                started: first,
                ended: Some(last),
                outcome,
                sizes,
            });
        }
        self.inner
            .write(&name, last.saturating_sub(first), outcome, sizes);
    }

    /// Ends the phase at `index` and writes its line. A phase that already ended keeps what it got.
    fn close(&self, index: usize, name: &str, outcome: Outcome, sizes: Sizes) {
        let ms = {
            let now = self.inner.now_ms();
            let mut phases = self.inner.phases.lock().unwrap_or_else(|e| e.into_inner());
            match phases.get_mut(index) {
                Some(phase) => {
                    if phase.ended.is_none() {
                        phase.ended = Some(now);
                        phase.outcome = outcome;
                        phase.sizes = sizes;
                    }
                    phase.ms(now)
                }
                None => 0,
            }
        };
        self.inner.write(name, ms, outcome, sizes);
    }

    /// The one summary line of this share, unless it has been written already.
    pub(crate) fn log_total(&self, outcome: Outcome) {
        self.inner.log_total(outcome);
    }
}

/// A phase that has begun and not yet ended. Its `Drop` ends it as `dropped`, so a handle nobody
/// finished leaves one line and stops counting instead of stretching to the end of the share.
pub(crate) struct Running {
    /// `None`: no share owns this phase, so it writes nothing.
    timing: Option<ShareTiming>,
    index: usize,
    name: String,
    done: bool,
}

impl Running {
    /// A phase of no share: it writes nothing, and ends for free.
    fn none() -> Running {
        Running {
            timing: None,
            index: 0,
            name: String::new(),
            done: true,
        }
    }

    /// Ends the phase and writes its line.
    pub(crate) fn finish(mut self, outcome: Outcome, sizes: Sizes) {
        self.done = true;
        if let Some(timing) = &self.timing {
            timing.close(self.index, &self.name, outcome, sizes);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Some(timing) = &self.timing {
            timing.close(self.index, &self.name, Outcome::Dropped, Sizes::NONE);
        }
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
    use tracing::field::{Field, Visit};
    use tracing::{Event, Metadata, Subscriber};

    /// The fields of one captured line.
    #[derive(Default)]
    struct Fields(Vec<(String, String)>);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
    }

    /// Every `lucina_share::timing` line written while it runs, in the order they came.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<Vec<(String, String)>>>>);

    impl Capture {
        fn lines(&self) -> Vec<Vec<(String, String)>> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}

        fn event(&self, event: &Event<'_>) {
            if event.metadata().target() != "lucina_share::timing" {
                return;
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
    }

    /// The lines `run` wrote to `lucina_share::timing`. A subscriber of this thread only, so the tests
    /// of other threads keep whatever logging the machine has.
    fn captured<T>(run: impl FnOnce() -> T) -> (T, Vec<Vec<(String, String)>>) {
        let capture = Capture::default();
        let value = tracing::subscriber::with_default(capture.clone(), run);
        (value, capture.lines())
    }

    fn field(line: &[(String, String)], name: &str) -> String {
        line.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    /// The captured lines of one phase, by name.
    fn lines_of<'a>(
        lines: &'a [Vec<(String, String)>],
        name: &str,
    ) -> Vec<&'a Vec<(String, String)>> {
        lines
            .iter()
            .filter(|line| field(line, "phase") == name)
            .collect()
    }

    #[test]
    fn phases_beginning_in_order_are_summarised_in_that_order() {
        let timing = ShareTiming::new("share-order-1");
        for name in ["receive", "ocr", "vision", "memory"] {
            timing.begin(name, false).finish(Outcome::Ok, Sizes::NONE);
        }
        let keys: Vec<String> = timing
            .inner
            .entries()
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect();
        assert_eq!(keys, ["receive", "ocr", "vision", "memory"]);
        let summary = timing.inner.summary();
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
            .inner
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
            timing.inner.total_ms() >= 3_000,
            "the total is at least the sum of the phases that did not overlap: {}",
            timing.inner.total_ms()
        );
    }

    #[test]
    fn a_wait_that_is_still_running_is_open_in_the_summary_and_kept_in_it() {
        let timing = ShareTiming::new("share-open-1");
        timing.begin("ocr", false).finish(Outcome::Ok, Sizes::NONE);
        // The vision server never becomes ready: the phase is left as it stands.
        let waiting = timing.begin("vision_ready", true);
        let entries = timing.inner.entries();
        assert_eq!(entries.len(), 2, "{entries:?}");
        let (name, _, outcome) = &entries[1];
        assert_eq!(name, "vision_ready");
        assert_eq!(*outcome, Outcome::Open);
        assert!(
            timing.inner.summary().contains("\"open\""),
            "{}",
            timing.inner.summary()
        );
        drop(waiting);
        assert_eq!(
            timing.inner.entries()[1].2,
            Outcome::Dropped,
            "the handle that goes away ends the wait it began"
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
        let summary = timing.inner.summary();
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
        let started = Instant::now();
        let (_, lines) = captured(|| {
            let timing = ShareTiming::silent();
            timing
                .begin("ocr", true)
                .finish(Outcome::Ok, Sizes::bytes(10));
            timing.record("body", started, Instant::now(), Outcome::Ok, Sizes::NONE);
            timing.log_total(Outcome::Ok);
        });
        assert!(lines.is_empty(), "a read of no share wrote {lines:?}");
    }

    #[test]
    fn a_share_id_reaches_the_lines_unchanged_or_tied_in_later() {
        let timing = ShareTiming::new("ab12-34");
        assert_eq!(timing.inner.id(), "ab12-34");
        let later = ShareTiming::default();
        later.tie("../../share id: note");
        let id = later.inner.id();
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{id}"
        );
        assert!(!id.contains('.') && !id.contains(' '), "{id}");
        later.tie("other-id");
        assert_eq!(later.inner.id(), id, "the id of a share is not rewritten");
    }

    #[test]
    fn a_handle_left_without_its_finish_writes_one_dropped_line_and_stops_counting() {
        let timing = ShareTiming::new("share-dropped-1");
        let (_, lines) = captured(|| {
            // The vision budget: the call it wraps is dropped, and so is the handle it began.
            let left = timing.begin("vision", true);
            std::thread::sleep(Duration::from_millis(40));
            drop(left);
        });
        let dropped = lines_of(&lines, "vision")
            .into_iter()
            .filter(|line| field(line, "outcome") == "dropped")
            .collect::<Vec<_>>();
        assert_eq!(dropped.len(), 1, "{lines:?}");
        let ms: u64 = field(dropped[0], "ms").parse().expect("the ms of a line");
        assert!(
            ms >= 30,
            "the line carries the time the phase did run: {ms}"
        );
        let entries = timing.inner.entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].2, Outcome::Dropped);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(
            timing.inner.entries()[0].1,
            entries[0].1,
            "a phase that ended does not keep counting to the summary"
        );
    }

    #[test]
    fn a_share_that_ends_by_itself_writes_one_summary_and_calls_it_an_error() {
        let (_, lines) = captured(|| {
            let timing = ShareTiming::new("share-ended-1");
            timing
                .begin("store", false)
                .finish(Outcome::Ok, Sizes::bytes(7));
            // The exit the review named: the receive returns on its error path, no summary asked for.
            drop(timing);
        });
        let totals = lines_of(&lines, "total");
        assert_eq!(totals.len(), 1, "{lines:?}");
        assert_eq!(field(totals[0], "outcome"), "error");
        assert_eq!(field(totals[0], "share"), "share-ended-1");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&field(totals[0], "phases"))
            .expect("the phases of a summary are one JSON array");
        assert_eq!(entries.len(), 1, "{lines:?}");
        assert_eq!(entries[0]["phase"], "store");
    }

    #[test]
    fn a_share_that_named_its_own_outcome_is_summarised_once() {
        let (_, lines) = captured(|| {
            let timing = ShareTiming::new("share-once-1");
            timing.begin("read", true).finish(Outcome::Ok, Sizes::NONE);
            timing.log_total(Outcome::Ok);
            drop(timing);
        });
        let totals = lines_of(&lines, "total");
        assert_eq!(totals.len(), 1, "the summary is written once: {lines:?}");
        assert_eq!(field(totals[0], "outcome"), "ok");
    }

    #[test]
    fn a_clone_that_goes_away_while_the_share_lives_writes_no_summary() {
        let timing = ShareTiming::new("share-cloned-1");
        let (_, lines) = captured(|| {
            let handed = timing.clone();
            handed
                .begin("memory", false)
                .finish(Outcome::Ok, Sizes::NONE);
            drop(handed);
        });
        assert!(
            lines_of(&lines, "total").is_empty(),
            "the clone of a live collector ends no share: {lines:?}"
        );
        let (_, lines) = captured(|| drop(timing));
        let totals = lines_of(&lines, "total");
        assert_eq!(totals.len(), 1, "{lines:?}");
        assert_eq!(field(totals[0], "outcome"), "error");
    }
}
