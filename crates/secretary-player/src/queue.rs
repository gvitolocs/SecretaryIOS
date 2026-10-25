//! The play queue: an ordered list of items, a cursor, repeat and shuffle.
//!
//! Shuffle keeps `items` in the order the user sees and plays through a
//! permutation (`order`), so turning shuffle off resumes the list order from
//! the current item.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// One playable item. `id` is the server's media id; everything the client
/// sends beyond the known fields is kept and returned unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queue {
    pub items: Vec<Item>,
    /// Play order: indexes into `items`. Identity unless shuffled.
    order: Vec<usize>,
    /// Position in `order`, `None` when nothing is current.
    cursor: Option<usize>,
    pub repeat: Repeat,
    pub shuffle: bool,
    seed: u64,
}

/// What the client renders: the items in list order and the current one.
#[derive(Debug, Clone, Serialize)]
pub struct View<'a> {
    pub items: &'a [Item],
    pub current: Option<usize>,
    pub item: Option<&'a Item>,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub has_next: bool,
    pub has_prev: bool,
}

/// Seconds into a track after which "previous" restarts it instead.
pub const RESTART_THRESHOLD_MS: u64 = 3_000;

impl Queue {
    pub fn view(&self) -> View<'_> {
        let current = self.current_index();
        View {
            items: &self.items,
            current,
            item: current.map(|i| &self.items[i]),
            repeat: self.repeat,
            shuffle: self.shuffle,
            has_next: self.peek(1).is_some(),
            has_prev: self.peek(-1).is_some(),
        }
    }

    /// Index into `items` of the current item.
    pub fn current_index(&self) -> Option<usize> {
        self.cursor.map(|c| self.order[c])
    }

    pub fn current(&self) -> Option<&Item> {
        self.current_index().map(|i| &self.items[i])
    }

    /// Replaces the queue and starts at `start` (list index).
    pub fn set(&mut self, items: Vec<Item>, start: usize, seed: u64) {
        self.items = items;
        self.seed = seed | 1;
        self.order = (0..self.items.len()).collect();
        self.cursor = None;
        if self.items.is_empty() {
            return;
        }
        let start = start.min(self.items.len() - 1);
        if self.shuffle {
            self.reshuffle(start);
        } else {
            self.cursor = Some(start);
        }
    }

    /// Adds items after the current one (`next`) or at the end.
    pub fn add(&mut self, items: Vec<Item>, next: bool) {
        let at = if next {
            self.current_index().map_or(self.items.len(), |i| i + 1)
        } else {
            self.items.len()
        };
        let count = items.len();
        // Shift every play-order index at or after the insertion point.
        for o in &mut self.order {
            if *o >= at {
                *o += count;
            }
        }
        self.items.splice(at..at, items);
        let new: Vec<usize> = (at..at + count).collect();
        let pos = match (next, self.cursor) {
            (true, Some(c)) => c + 1,
            _ => self.order.len(),
        };
        self.order.splice(pos..pos, new);
        if self.cursor.is_none() && !self.items.is_empty() {
            self.cursor = Some(0);
        }
    }

    /// Removes the item at list index `index`. Removing the current item
    /// makes the next one current (or the previous one at the end).
    pub fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        let pos = self.order.iter().position(|&o| o == index).expect("order covers items");
        self.items.remove(index);
        self.order.remove(pos);
        for o in &mut self.order {
            if *o > index {
                *o -= 1;
            }
        }
        self.cursor = match self.cursor {
            _ if self.order.is_empty() => None,
            Some(c) if c > pos => Some(c - 1),
            Some(c) => Some(c.min(self.order.len() - 1)),
            None => None,
        };
    }

    /// Moves an item in the list. Play order follows the list unless shuffled.
    pub fn move_item(&mut self, from: usize, to: usize) {
        let len = self.items.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let current = self.current_index();
        let item = self.items.remove(from);
        self.items.insert(to, item);
        let remap = |i: usize| -> usize {
            if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            }
        };
        if self.shuffle {
            for o in &mut self.order {
                *o = remap(*o);
            }
        } else {
            self.order = (0..len).collect();
            self.cursor = current.map(remap);
        }
    }

    /// Makes list index `index` current.
    pub fn jump(&mut self, index: usize) -> bool {
        match self.order.iter().position(|&o| o == index) {
            Some(pos) => {
                self.cursor = Some(pos);
                true
            }
            None => false,
        }
    }

    /// The item `step` places away in play order, honouring repeat-all.
    fn peek(&self, step: isize) -> Option<usize> {
        let c = self.cursor? as isize;
        let len = self.order.len() as isize;
        let target = c + step;
        if (0..len).contains(&target) {
            Some(target as usize)
        } else if self.repeat == Repeat::All && len > 0 {
            Some(target.rem_euclid(len) as usize)
        } else {
            None
        }
    }

    /// Advances. `auto` means the current item finished playing on its own,
    /// which replays it under repeat-one; a user tap always moves on.
    /// Returns false at the end of the queue (the cursor stays put).
    pub fn next(&mut self, auto: bool) -> bool {
        if auto && self.repeat == Repeat::One && self.cursor.is_some() {
            return true;
        }
        match self.peek(1) {
            Some(c) => {
                self.cursor = Some(c);
                true
            }
            None => false,
        }
    }

    /// Goes back, or asks the player to restart the current item when it has
    /// played for more than `RESTART_THRESHOLD_MS`. Returns `Restart` / `Moved` / `None`.
    pub fn prev(&mut self, position_ms: u64) -> Prev {
        if self.cursor.is_none() {
            return Prev::None;
        }
        if position_ms > RESTART_THRESHOLD_MS {
            return Prev::Restart;
        }
        match self.peek(-1) {
            Some(c) => {
                self.cursor = Some(c);
                Prev::Moved
            }
            None => Prev::Restart,
        }
    }

    pub fn set_mode(&mut self, repeat: Option<Repeat>, shuffle: Option<bool>) {
        if let Some(r) = repeat {
            self.repeat = r;
        }
        if let Some(s) = shuffle {
            if s != self.shuffle {
                self.shuffle = s;
                let current = self.current_index();
                if s {
                    if let Some(i) = current {
                        self.reshuffle(i);
                    } else {
                        self.reshuffle_all();
                    }
                } else {
                    self.order = (0..self.items.len()).collect();
                    self.cursor = current;
                }
            }
        }
    }

    /// Shuffles with `first` playing now and the rest after it.
    fn reshuffle(&mut self, first: usize) {
        let mut rest: Vec<usize> = (0..self.items.len()).filter(|&i| i != first).collect();
        self.fisher_yates(&mut rest);
        self.order = std::iter::once(first).chain(rest).collect();
        self.cursor = Some(0);
    }

    fn reshuffle_all(&mut self) {
        let mut all: Vec<usize> = (0..self.items.len()).collect();
        self.fisher_yates(&mut all);
        self.order = all;
        self.cursor = None;
    }

    fn fisher_yates(&mut self, v: &mut [usize]) {
        for i in (1..v.len()).rev() {
            // xorshift64: no RNG dependency, and a stored seed keeps tests deterministic.
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            let j = (self.seed % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Prev {
    None,
    Moved,
    Restart,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                id: format!("m{i}"),
                kind: "audio".into(),
                title: format!("Track {i}"),
                duration_ms: None,
                extra: Map::new(),
            })
            .collect()
    }

    fn ids(q: &Queue) -> Vec<String> {
        q.items.iter().map(|i| i.id.clone()).collect()
    }

    #[test]
    fn plays_in_order_and_stops_at_end() {
        let mut q = Queue::default();
        q.set(items(3), 0, 7);
        assert_eq!(q.current().unwrap().id, "m0");
        assert!(q.next(true));
        assert!(q.next(true));
        assert_eq!(q.current().unwrap().id, "m2");
        assert!(!q.next(true));
        assert_eq!(q.current().unwrap().id, "m2");
    }

    #[test]
    fn repeat_modes() {
        let mut q = Queue::default();
        q.set(items(2), 1, 7);
        q.set_mode(Some(Repeat::All), None);
        assert!(q.next(false));
        assert_eq!(q.current().unwrap().id, "m0");
        q.set_mode(Some(Repeat::One), None);
        assert!(q.next(true));
        assert_eq!(q.current().unwrap().id, "m0");
        assert!(q.next(false));
        assert_eq!(q.current().unwrap().id, "m1");
    }

    #[test]
    fn prev_restarts_after_threshold() {
        let mut q = Queue::default();
        q.set(items(3), 1, 7);
        assert_eq!(q.prev(10_000), Prev::Restart);
        assert_eq!(q.current().unwrap().id, "m1");
        assert_eq!(q.prev(500), Prev::Moved);
        assert_eq!(q.current().unwrap().id, "m0");
        assert_eq!(q.prev(500), Prev::Restart);
    }

    #[test]
    fn shuffle_keeps_current_and_visits_all() {
        let mut q = Queue::default();
        q.set(items(20), 5, 99);
        q.set_mode(None, Some(true));
        assert_eq!(q.current().unwrap().id, "m5");
        let mut seen = vec![q.current_index().unwrap()];
        while q.next(false) {
            seen.push(q.current_index().unwrap());
        }
        seen.sort();
        assert_eq!(seen, (0..20).collect::<Vec<_>>());
        q.set_mode(None, Some(false));
        let cur = q.current_index().unwrap();
        assert!(q.next(false) || cur == 19);
    }

    #[test]
    fn add_next_and_remove_current() {
        let mut q = Queue::default();
        q.set(items(3), 1, 7);
        let mut extra = items(1);
        extra[0].id = "x".into();
        q.add(extra, true);
        assert_eq!(ids(&q), ["m0", "m1", "x", "m2"]);
        assert!(q.next(false));
        assert_eq!(q.current().unwrap().id, "x");
        q.remove(2);
        assert_eq!(q.current().unwrap().id, "m2");
        q.remove(2);
        assert_eq!(q.current().unwrap().id, "m1");
        q.remove(0);
        q.remove(0);
        assert!(q.current().is_none());
    }

    #[test]
    fn move_keeps_current() {
        let mut q = Queue::default();
        q.set(items(4), 1, 7);
        q.move_item(1, 3);
        assert_eq!(ids(&q), ["m0", "m2", "m3", "m1"]);
        assert_eq!(q.current().unwrap().id, "m1");
        q.move_item(0, 2);
        assert_eq!(q.current().unwrap().id, "m1");
    }

    #[test]
    fn add_to_empty_queue_starts_it() {
        let mut q = Queue::default();
        q.add(items(2), false);
        assert_eq!(q.current().unwrap().id, "m0");
    }

    #[test]
    fn extra_fields_round_trip() {
        let item: Item = serde_json::from_str(r#"{"id":"a","kind":"video","title":"T","poster_url":"/p.jpg"}"#).unwrap();
        let back = serde_json::to_value(&item).unwrap();
        assert_eq!(back["poster_url"], "/p.jpg");
    }
}
