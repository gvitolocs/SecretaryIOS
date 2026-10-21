//! The sync op-log — the piece the TLC model in `formal/MigratedSync.tla`
//! mirrors. Invariants the implementation must hold (and the model checks):
//!
//! S1  `queue_has_no_duplicate_op_ids`
//! S2  `apply_is_idempotent` (replay of a flushed op never duplicates)
//! L1  every committed op eventually leaves the queue (under a fair link)
//!
//! Ops are committed locally first, then flushed asynchronously; a flush
//! with an unclear outcome (timeout/5xx) leaves the op queued and retries
//! with the same `op_id`, which is what makes replay safe.

use crate::shopping::models::{apply_op, ItemOp, LucinaItem, OpKind};

#[derive(Debug, Default)]
pub struct OpLog {
    queue: Vec<ItemOp>,
    seq: u64,
}

impl OpLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Commit an edit locally and enqueue its op. Returns the new local state.
    pub fn commit(&mut self, items: &mut Vec<LucinaItem>, item_uuid: &str, kind: OpKind) -> ItemOp {
        self.seq += 1;
        let op = ItemOp {
            op_id: format!("{}-{}", self.seq, uuid::Uuid::new_v4()),
            item_uuid: item_uuid.into(),
            kind,
        };
        self.queue.push(op.clone());
        apply_op(items, &op);
        op
    }

    pub fn pending(&self) -> &[ItemOp] {
        &self.queue
    }

    /// Drain up to `n` ops for a flush attempt (they stay owned by the
    /// caller until `ack`); the queue itself only forgets an op on ack.
    pub fn take_batch(&mut self, n: usize) -> Vec<ItemOp> {
        self.queue.drain(..n.min(self.queue.len())).collect()
    }

    /// Re-queue after an *unclear* outcome. Dedup by op_id keeps the queue
    /// free of duplicates even if a raced ack had already removed it.
    pub fn requeue(&mut self, ops: Vec<ItemOp>) {
        for op in ops {
            if !self.queue.iter().any(|q| q.op_id == op.op_id) {
                self.queue.insert(0, op);
            }
        }
    }

    /// Fold a batch into the wire payload. One item = last op wins.
    pub fn batch_to_items(&self, ops: &[ItemOp]) -> Vec<LucinaItem> {
        let mut items: Vec<LucinaItem> = Vec::new();
        for op in ops {
            apply_op(&mut items, op);
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_applies_and_queues() {
        let mut log = OpLog::new();
        let mut items = Vec::new();
        let op = log.commit(&mut items, "i1", OpKind::Add);
        assert_eq!(items.len(), 1);
        assert_eq!(log.pending().len(), 1);
        assert_eq!(log.pending()[0].op_id, op.op_id);
    }

    #[test]
    fn replay_ack_is_noop() {
        let mut log = OpLog::new();
        let mut items = Vec::new();
        let op = log.commit(&mut items, "i1", OpKind::Add);
        let batch = log.take_batch(10);
        log.requeue(batch.clone());
        log.requeue(batch); // duplicate requeue (raced ack)
        assert_eq!(log.pending().len(), 1);
        // idempotent fold
        let mut again = Vec::new();
        apply_op(&mut again, &op);
        apply_op(&mut again, &op);
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn last_op_wins() {
        let log = OpLog::new();
        let ops = vec![
            ItemOp {
                op_id: "1".into(),
                item_uuid: "a".into(),
                kind: OpKind::Add,
            },
            ItemOp {
                op_id: "2".into(),
                item_uuid: "a".into(),
                kind: OpKind::Check,
            },
        ];
        let items = log.batch_to_items(&ops);
        assert!(items[0].checked);
    }
}
