//! The applied-op-id set of one device, bounded: per-device counters are
//! monotonic, so the server keeps the largest contiguous counter plus a small
//! sparse set of counters above it (ops that arrived out of order).
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Out-of-order counters kept above the contiguous mark. An op further ahead
/// is deferred (not acked) and retried by the client once the gap closes.
pub const MAX_SPARSE: usize = 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watermark {
    /// Every counter `1..=contiguous` is applied.
    pub contiguous: u64,
    /// Applied counters above `contiguous + 1` (never `contiguous + 1` itself).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub sparse: BTreeSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

impl Watermark {
    pub fn contains(&self, c: u64) -> bool {
        c <= self.contiguous || self.sparse.contains(&c)
    }

    /// `Ok(true)`: newly recorded; `Ok(false)`: already there; `Err(Full)`:
    /// the sparse set is full and `c` is not the next contiguous counter.
    pub fn insert(&mut self, c: u64) -> Result<bool, Full> {
        if self.contains(c) {
            return Ok(false);
        }
        if c == self.contiguous + 1 {
            self.contiguous = c;
            while self.sparse.remove(&(self.contiguous + 1)) {
                self.contiguous += 1;
            }
            return Ok(true);
        }
        if self.sparse.len() >= MAX_SPARSE {
            return Err(Full);
        }
        self.sparse.insert(c);
        Ok(true)
    }

    /// Canonical: the sparse set is above `contiguous + 1`.
    pub fn is_canonical(&self) -> bool {
        self.sparse
            .first()
            .is_none_or(|&first| first > self.contiguous + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_then_gap_closes() {
        let mut w = Watermark::default();
        assert_eq!(w.insert(3), Ok(true));
        assert_eq!(w.insert(2), Ok(true));
        assert!(w.contains(3) && w.contains(2) && !w.contains(1));
        assert_eq!(w.insert(3), Ok(false));
        assert_eq!(w.insert(1), Ok(true));
        assert_eq!(
            w,
            Watermark {
                contiguous: 3,
                sparse: BTreeSet::new()
            }
        );
        assert!(w.is_canonical());
    }

    #[test]
    fn bounded_sparse_set() {
        let mut w = Watermark::default();
        for c in 0..MAX_SPARSE as u64 {
            assert_eq!(w.insert(c + 3), Ok(true));
        }
        assert_eq!(w.insert(5000), Err(Full));
        assert_eq!(w.insert(1), Ok(true));
        assert_eq!(w.insert(2), Ok(true));
        assert_eq!(w.contiguous, MAX_SPARSE as u64 + 2);
        assert!(w.sparse.is_empty());
    }
}
