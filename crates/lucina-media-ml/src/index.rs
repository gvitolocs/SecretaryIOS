//! Every vector of one model in memory, searched by brute force.
//!
//! The vectors sit in one contiguous `Vec<f32>` (row `i` is item `ids[i]`), so a scan is a
//! straight pass over memory. A top-k heap keeps the best hits; ties go to the smaller id, so the
//! same query always gives the same order.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::vector::{dot, normalize};

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: String,
    /// Cosine similarity, -1..1.
    pub score: f32,
}

#[derive(Default)]
pub struct VectorIndex {
    dim: usize,
    ids: Vec<String>,
    pos: HashMap<String, usize>,
    data: Vec<f32>,
}

/// A heap entry ordered so the heap's top is the worst hit kept so far.
struct Worst<'a> {
    score: f32,
    id: &'a str,
}

impl PartialEq for Worst<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Worst<'_> {}
impl PartialOrd for Worst<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Worst<'_> {
    /// Greater = worse: a lower score, or the same score and a larger id.
    fn cmp(&self, o: &Self) -> Ordering {
        o.score
            .total_cmp(&self.score)
            .then_with(|| self.id.cmp(o.id))
    }
}

impl VectorIndex {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            ..Default::default()
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.pos.contains_key(id)
    }

    /// Adds or replaces `id`'s vector, normalized. A vector of the wrong length or with no
    /// direction is refused (`false`).
    pub fn upsert(&mut self, id: &str, v: &[f32]) -> bool {
        if v.len() != self.dim {
            return false;
        }
        let mut v = v.to_vec();
        if !normalize(&mut v) {
            return false;
        }
        match self.pos.get(id) {
            Some(&i) => self.data[i * self.dim..(i + 1) * self.dim].copy_from_slice(&v),
            None => {
                self.pos.insert(id.to_string(), self.ids.len());
                self.ids.push(id.to_string());
                self.data.extend_from_slice(&v);
            }
        }
        true
    }

    /// Drops `id` (the last row moves into its place).
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(i) = self.pos.remove(id) else {
            return false;
        };
        let last = self.ids.len() - 1;
        if i != last {
            let (head, tail) = self.data.split_at_mut(last * self.dim);
            head[i * self.dim..(i + 1) * self.dim].copy_from_slice(&tail[..self.dim]);
            self.ids.swap(i, last);
            self.pos.insert(self.ids[i].clone(), i);
        }
        self.ids.pop();
        self.data.truncate(last * self.dim);
        true
    }

    pub fn get(&self, id: &str) -> Option<&[f32]> {
        self.pos
            .get(id)
            .map(|&i| &self.data[i * self.dim..(i + 1) * self.dim])
    }

    /// The `k` vectors closest to `query` (a unit vector) with a cosine of at least `min_score`,
    /// best first; `exclude` is left out (the photo a "similar" search starts from).
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        min_score: f32,
        exclude: Option<&str>,
    ) -> Vec<Hit> {
        if query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let mut heap: BinaryHeap<Worst> = BinaryHeap::with_capacity(k + 1);
        for (i, id) in self.ids.iter().enumerate() {
            if exclude == Some(id.as_str()) {
                continue;
            }
            let score = dot(query, &self.data[i * self.dim..(i + 1) * self.dim]);
            if score.is_nan() || score < min_score {
                continue;
            }
            let cand = Worst { score, id };
            if heap.len() < k {
                heap.push(cand);
            } else if heap.peek().is_some_and(|w| cand < *w) {
                heap.pop();
                heap.push(cand);
            }
        }
        let mut hits: Vec<Hit> = heap
            .into_iter()
            .map(|w| Hit {
                id: w.id.to_string(),
                score: w.score,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(angle_deg: f32) -> Vec<f32> {
        let a = angle_deg.to_radians();
        vec![a.cos(), a.sin()]
    }

    #[test]
    fn finds_the_closest_in_order() {
        let mut ix = VectorIndex::new(2);
        for (id, deg) in [("a", 0.0), ("b", 10.0), ("c", 90.0), ("d", 45.0)] {
            assert!(ix.upsert(id, &unit(deg)));
        }
        let hits = ix.search(&unit(5.0), 3, -1.0, None);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "d"]);
        assert!(hits[0].score >= hits[1].score && hits[1].score >= hits[2].score);
    }

    #[test]
    fn min_score_and_exclude() {
        let mut ix = VectorIndex::new(2);
        ix.upsert("a", &unit(0.0));
        ix.upsert("b", &unit(80.0));
        ix.upsert("c", &unit(3.0));
        let hits = ix.search(&unit(0.0), 10, 0.5, Some("a"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "c");
    }

    #[test]
    fn ties_break_by_id_so_results_are_stable() {
        let mut ix = VectorIndex::new(2);
        for id in ["z", "m", "a", "q"] {
            ix.upsert(id, &unit(30.0));
        }
        let ids: Vec<String> = ix
            .search(&unit(30.0), 2, -1.0, None)
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(ids, ["a", "m"]);
    }

    #[test]
    fn upsert_replaces_and_remove_keeps_the_rest_findable() {
        let mut ix = VectorIndex::new(2);
        ix.upsert("a", &unit(0.0));
        ix.upsert("b", &unit(90.0));
        ix.upsert("c", &unit(180.0));
        ix.upsert("a", &unit(90.0));
        assert_eq!(ix.len(), 3);
        assert!(ix.remove("a"));
        assert!(!ix.remove("a"));
        assert_eq!(ix.len(), 2);
        assert!(!ix.contains("a"));
        let top = ix.search(&unit(180.0), 1, -1.0, None);
        assert_eq!(top[0].id, "c");
        let top = ix.search(&unit(90.0), 1, -1.0, None);
        assert_eq!(top[0].id, "b");
        assert!((ix.get("c").unwrap()[0] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn refuses_wrong_length_and_zero_vectors() {
        let mut ix = VectorIndex::new(2);
        assert!(!ix.upsert("a", &[1.0, 0.0, 0.0]));
        assert!(!ix.upsert("a", &[0.0, 0.0]));
        assert!(ix.is_empty());
        assert!(ix.search(&[1.0], 5, -1.0, None).is_empty());
    }

    #[test]
    fn stores_normalized_vectors() {
        let mut ix = VectorIndex::new(2);
        ix.upsert("a", &[10.0, 0.0]);
        assert_eq!(ix.get("a").unwrap(), &[1.0, 0.0]);
    }
}
