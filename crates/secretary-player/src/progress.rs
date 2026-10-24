//! Resume positions: when to resume, when an item counts as finished, when to
//! push a position to the server, and how local and server positions merge.
//!
//! Merge rule: last writer wins by `updated_at` (ms since the epoch, set by the
//! device that played). A newer local position that has not reached the
//! server yet is never overwritten by an older server one.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Push a position at most this often while playing.
pub const SYNC_EVERY_MS: u64 = 15_000;
/// A jump larger than this is a seek and is pushed at once.
pub const SEEK_JUMP_MS: u64 = 20_000;
/// Never resume less than this far in (or a tenth of a short item).
pub const RESUME_MIN_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub position_ms: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub finished: bool,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Local {
    entry: Option<Entry>,
    /// Changed since the server last acknowledged it.
    dirty: bool,
    /// When this item was last pushed (or acknowledged), local clock.
    last_push: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Book {
    entries: BTreeMap<String, Local>,
}

/// The answer to one position report.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Tick {
    /// Push `entry` to the server now.
    pub sync: bool,
    pub finished: bool,
    pub entry: Entry,
}

/// Remaining time under which an item counts as finished: 5% of it, at
/// least 2 s and at most 2 min (credits, a memo's trailing silence).
pub fn finish_margin(duration_ms: u64) -> u64 {
    (duration_ms / 20).clamp(2_000, 120_000)
}

pub fn is_finished(position_ms: u64, duration_ms: u64) -> bool {
    duration_ms > 0 && position_ms + finish_margin(duration_ms) >= duration_ms
}

/// Where to start an item: 0 for unseen, finished, or barely started ones.
pub fn resume_point(entry: Option<&Entry>, duration_ms: Option<u64>) -> u64 {
    let Some(e) = entry else { return 0 };
    let duration = duration_ms.filter(|d| *d > 0).unwrap_or(e.duration_ms);
    if e.finished || (duration > 0 && is_finished(e.position_ms, duration)) {
        return 0;
    }
    let min = if duration > 0 { RESUME_MIN_MS.min(duration / 10) } else { RESUME_MIN_MS };
    if e.position_ms < min {
        0
    } else {
        // Back up a little so the viewer finds their place.
        e.position_ms.saturating_sub(2_000)
    }
}

impl Book {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id).and_then(|l| l.entry.as_ref())
    }

    /// Records a position from the player. `playing` false (pause, stop,
    /// background) always asks for a push.
    pub fn tick(
        &mut self,
        id: &str,
        position_ms: u64,
        duration_ms: u64,
        playing: bool,
        now: u64,
        device: Option<String>,
    ) -> Tick {
        let local = self.entries.entry(id.to_string()).or_default();
        let previous = local.entry.as_ref().map(|e| e.position_ms);
        let finished = is_finished(position_ms, duration_ms)
            // Finished stays finished while the end credits keep ticking.
            || (local.entry.as_ref().is_some_and(|e| e.finished) && previous.is_some_and(|p| position_ms >= p));
        let entry = Entry {
            id: id.to_string(),
            position_ms,
            duration_ms,
            finished,
            updated_at: now,
            device,
        };
        let changed = local.entry.as_ref().map(|e| (e.position_ms, e.finished)) != Some((position_ms, finished));
        let newly_finished = finished && !local.entry.as_ref().is_some_and(|e| e.finished);
        let jumped = previous.is_some_and(|p| p.abs_diff(position_ms) > SEEK_JUMP_MS);
        local.entry = Some(entry.clone());
        local.dirty |= changed;
        let sync = local.dirty
            && (!playing || newly_finished || jumped || local.last_push == 0 || now.saturating_sub(local.last_push) >= SYNC_EVERY_MS);
        if sync {
            local.last_push = now;
        }
        Tick { sync, finished, entry }
    }

    /// Entries the server has not acknowledged.
    pub fn pending(&self) -> Vec<Entry> {
        self.entries
            .values()
            .filter(|l| l.dirty)
            .filter_map(|l| l.entry.clone())
            .collect()
    }

    /// The server stored these (id, updated_at). An entry changed after the
    /// acknowledged write stays dirty.
    pub fn acknowledge(&mut self, acks: &[(String, u64)]) {
        for (id, at) in acks {
            if let Some(l) = self.entries.get_mut(id) {
                if l.entry.as_ref().is_some_and(|e| e.updated_at <= *at) {
                    l.dirty = false;
                }
            }
        }
    }

    /// Takes server entries newer than the local ones. Returns the ids that changed.
    pub fn merge(&mut self, remote: Vec<Entry>) -> Vec<String> {
        let mut changed = Vec::new();
        for e in remote {
            let local = self.entries.entry(e.id.clone()).or_default();
            let newer = local.entry.as_ref().is_none_or(|l| e.updated_at > l.updated_at);
            if newer {
                changed.push(e.id.clone());
                local.entry = Some(e);
                local.dirty = false;
            }
        }
        changed
    }

    /// Started, unfinished items, most recent first ("Continue").
    pub fn recent(&self, limit: usize) -> Vec<Entry> {
        let mut v: Vec<Entry> = self
            .entries
            .values()
            .filter_map(|l| l.entry.clone())
            .filter(|e| !e.finished && resume_point(Some(e), None) > 0)
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        v.truncate(limit);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;

    #[test]
    fn finish_margin_scales() {
        assert_eq!(finish_margin(30_000), 2_000);
        assert_eq!(finish_margin(10 * MIN), 30_000);
        assert_eq!(finish_margin(120 * MIN), 120_000);
        assert!(is_finished(118 * MIN, 120 * MIN));
        assert!(!is_finished(100 * MIN, 120 * MIN));
    }

    #[test]
    fn resume_rules() {
        let e = |pos, dur, fin| Entry { id: "a".into(), position_ms: pos, duration_ms: dur, finished: fin, updated_at: 1, device: None };
        assert_eq!(resume_point(None, Some(MIN)), 0);
        assert_eq!(resume_point(Some(&e(5_000, 60 * MIN, false)), None), 0);
        assert_eq!(resume_point(Some(&e(30 * MIN, 60 * MIN, false)), None), 30 * MIN - 2_000);
        assert_eq!(resume_point(Some(&e(30 * MIN, 60 * MIN, true)), None), 0);
        assert_eq!(resume_point(Some(&e(59 * MIN, 60 * MIN, false)), None), 0);
        // A 40 s memo resumes from 5 s.
        assert_eq!(resume_point(Some(&e(5_000, 40_000, false)), None), 3_000);
    }

    #[test]
    fn tick_throttles_and_pushes_on_pause_seek_finish() {
        let mut b = Book::default();
        let d = 60 * MIN;
        assert!(b.tick("a", 1_000, d, true, 1_000, None).sync);
        assert!(!b.tick("a", 6_000, d, true, 6_000, None).sync);
        assert!(b.tick("a", 17_000, d, true, 17_000, None).sync);
        assert!(!b.tick("a", 18_000, d, true, 18_000, None).sync);
        // Seek.
        assert!(b.tick("a", 40 * MIN, d, true, 19_000, None).sync);
        // Pause.
        assert!(b.tick("a", 40 * MIN + 500, d, false, 19_500, None).sync);
        // Unchanged pause report: nothing new to push once acknowledged.
        b.acknowledge(&[("a".into(), 19_500)]);
        assert!(!b.tick("a", 40 * MIN + 500, d, false, 20_000, None).sync);
        // Finish.
        let t = b.tick("a", 59 * MIN, d, true, 21_000, None);
        assert!(t.sync && t.finished);
        assert!(b.tick("a", 59 * MIN + 1_000, d, true, 22_000, None).finished);
    }

    #[test]
    fn merge_is_last_writer_wins() {
        let mut b = Book::default();
        b.tick("a", 10 * MIN, 60 * MIN, false, 1_000, None);
        let remote = |at| Entry { id: "a".into(), position_ms: 20 * MIN, duration_ms: 60 * MIN, finished: false, updated_at: at, device: Some("web".into()) };
        assert!(b.merge(vec![remote(500)]).is_empty());
        assert_eq!(b.get("a").unwrap().position_ms, 10 * MIN);
        assert_eq!(b.pending().len(), 1);
        assert_eq!(b.merge(vec![remote(2_000)]), vec!["a".to_string()]);
        assert_eq!(b.get("a").unwrap().position_ms, 20 * MIN);
        assert!(b.pending().is_empty());
    }

    #[test]
    fn ack_of_older_write_keeps_dirty() {
        let mut b = Book::default();
        b.tick("a", 10 * MIN, 60 * MIN, false, 1_000, None);
        b.tick("a", 11 * MIN, 60 * MIN, false, 2_000, None);
        b.acknowledge(&[("a".into(), 1_000)]);
        assert_eq!(b.pending().len(), 1);
        b.acknowledge(&[("a".into(), 2_000)]);
        assert!(b.pending().is_empty());
    }

    #[test]
    fn recent_lists_unfinished() {
        let mut b = Book::default();
        b.tick("a", 10 * MIN, 60 * MIN, false, 1_000, None);
        b.tick("b", 59 * MIN, 60 * MIN, false, 2_000, None);
        b.tick("c", 20 * MIN, 60 * MIN, false, 3_000, None);
        let r: Vec<String> = b.recent(10).into_iter().map(|e| e.id).collect();
        assert_eq!(r, ["c", "a"]);
    }
}
