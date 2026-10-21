//! Derived statistics. Pure functions over entries; integer grams only.
use serde::{Deserialize, Serialize};

use crate::kernel;
use crate::models::Entry;

pub const DAY_MS: u64 = 86_400_000;
pub const WEEK_MS: u64 = 7 * DAY_MS;

/// One entry on the chart line: its weight and the 7-day average at it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Point {
    pub entry_id: String,
    pub measured_ms: u64,
    pub weight_grams: u64,
    pub avg7_grams: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stats {
    pub count: usize,
    pub current_grams: Option<u64>,
    pub current_measured_ms: Option<u64>,
    pub avg7_grams: Option<u64>,
    pub weekly_change_grams: Option<i64>,
    pub min_grams: Option<u64>,
    pub max_grams: Option<u64>,
    pub trend30_grams: Option<i64>,
    pub trend90_grams: Option<i64>,
    pub goal_grams: Option<u64>,
    pub goal_distance_grams: Option<i64>,
    pub goal_reached: Option<bool>,
}

/// Rounded (half up) mean of a slice of gram values; `None` when empty.
pub fn rounded_mean(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let sum: u64 = values.iter().sum();
    Some(kernel::rounded_mean(sum, values.len() as u64))
}

/// Entries sorted oldest first by `(measured_ms, id)`, with prefix sums so a
/// window mean is two binary searches.
pub struct Timeline<'a> {
    pub sorted: Vec<&'a Entry>,
    prefix: Vec<u64>,
}

impl<'a> Timeline<'a> {
    pub fn new(entries: &'a [Entry]) -> Self {
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        sorted.sort_by(|a, b| (a.measured_ms, &a.id).cmp(&(b.measured_ms, &b.id)));
        let mut prefix = Vec::with_capacity(sorted.len() + 1);
        prefix.push(0u64);
        for e in &sorted {
            prefix.push(prefix.last().unwrap() + e.weight_grams);
        }
        Self { sorted, prefix }
    }

    /// Index range of the entries measured in `(after, until]` (`after = None`: from the start).
    fn range(&self, after: Option<u64>, until: u64) -> (usize, usize) {
        let lo = after.map_or(0, |a| self.sorted.partition_point(|e| e.measured_ms <= a));
        let hi = self.sorted.partition_point(|e| e.measured_ms <= until);
        (lo, hi.max(lo))
    }

    /// Mean of the entries measured in `(t − 7 days, t]`.
    pub fn avg7(&self, t: u64) -> Option<u64> {
        let (lo, hi) = self.range(t.checked_sub(WEEK_MS), t);
        (hi > lo).then(|| kernel::rounded_mean(self.prefix[hi] - self.prefix[lo], (hi - lo) as u64))
    }

    pub fn series(&self) -> Vec<Point> {
        self.sorted
            .iter()
            .map(|e| Point {
                entry_id: e.id.clone(),
                measured_ms: e.measured_ms,
                weight_grams: e.weight_grams,
                avg7_grams: self.avg7(e.measured_ms).unwrap_or(e.weight_grams),
            })
            .collect()
    }

    /// `avg7(last) − avg7(first)` over the entries measured in `(now − days, now]`.
    fn trend(&self, now: u64, days: u64) -> Option<i64> {
        let (lo, hi) = self.range(now.checked_sub(days * DAY_MS), now);
        if hi - lo < 2 {
            return None;
        }
        let first = self.avg7(self.sorted[lo].measured_ms)?;
        let last = self.avg7(self.sorted[hi - 1].measured_ms)?;
        Some(last as i64 - first as i64)
    }
}

pub fn stats(entries: &[Entry], goal_grams: Option<u64>, now: u64) -> Stats {
    let t = Timeline::new(entries);
    let mut s = Stats {
        count: entries.len(),
        goal_grams,
        ..Stats::default()
    };
    let (Some(first), Some(latest)) = (t.sorted.first(), t.sorted.last()) else {
        return s;
    };
    let l = latest.measured_ms;
    s.current_grams = Some(latest.weight_grams);
    s.current_measured_ms = Some(l);
    s.avg7_grams = t.avg7(l);
    s.min_grams = entries.iter().map(|e| e.weight_grams).min();
    s.max_grams = entries.iter().map(|e| e.weight_grams).max();
    if let (Some(now_avg), Some(week_ago)) = (s.avg7_grams, l.checked_sub(WEEK_MS)) {
        s.weekly_change_grams = t.avg7(week_ago).map(|a| now_avg as i64 - a as i64);
    }
    s.trend30_grams = t.trend(now, 30);
    s.trend90_grams = t.trend(now, 90);
    if let (Some(goal), Some(avg)) = (goal_grams, s.avg7_grams) {
        s.goal_distance_grams = Some(avg as i64 - goal as i64);
        // Direction from the first-ever entry: above the goal means losing.
        s.goal_reached = Some(if first.weight_grams > goal {
            avg <= goal
        } else {
            avg >= goal
        });
    }
    s
}
