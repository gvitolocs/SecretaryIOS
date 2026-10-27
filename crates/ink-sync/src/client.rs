//! The client sync loop as a pure state machine.
//!
//! The machine holds only volatile scheduling state (the batch in flight, the
//! retry attempt and the time before which it must not send). The durable
//! truth is the [`crate::outbox::Outbox`]: after a crash a fresh machine
//! resumes from the outbox's unacked ops, so nothing unacked is dropped and
//! nothing acked is re-sent. Retries never give up; their delay grows
//! exponentially up to a cap, with jitter.
use crate::op::OpId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base_ms: u64,
    pub cap_ms: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            base_ms: 500,
            cap_ms: 5 * 60 * 1000,
        }
    }
}

impl Backoff {
    /// "Equal jitter": the delay before retry `attempt` (0-based) lies in
    /// `[d/2, d]` with `d = min(cap, base * 2^attempt)`; `rand` picks the point.
    pub fn delay(&self, attempt: u32, rand: u64) -> u64 {
        let d = self
            .base_ms
            .saturating_mul(1u64 << attempt.min(32))
            .min(self.cap_ms);
        let half = d / 2;
        half + rand % (d - half + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send these pending ops (oldest first) now.
    Send(Vec<OpId>),
    /// Nothing may be sent before this time (ms).
    WaitUntil(u64),
    /// A batch is in flight: wait for its result.
    InFlight,
    /// Everything is acked.
    Idle,
}

#[derive(Debug, Clone)]
pub struct ClientMachine {
    pub max_batch: usize,
    pub backoff: Backoff,
    attempt: u32,
    not_before: u64,
    inflight: Option<Vec<OpId>>,
}

impl ClientMachine {
    pub fn new(max_batch: usize, backoff: Backoff) -> Self {
        ClientMachine {
            max_batch: max_batch.max(1),
            backoff,
            attempt: 0,
            not_before: 0,
            inflight: None,
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// `pending`: the outbox's unacked op ids, oldest first.
    pub fn next(&mut self, now: u64, pending: &[OpId]) -> Step {
        if self.inflight.is_some() {
            return Step::InFlight;
        }
        if pending.is_empty() {
            self.attempt = 0;
            self.not_before = 0;
            return Step::Idle;
        }
        if now < self.not_before {
            return Step::WaitUntil(self.not_before);
        }
        let batch: Vec<OpId> = pending.iter().take(self.max_batch).cloned().collect();
        self.inflight = Some(batch.clone());
        Step::Send(batch)
    }

    /// The server answered. Acks for ops outside the batch are fine too (the
    /// server applied them). A response that acks nothing of the batch (every
    /// op deferred) backs off like a failure.
    pub fn on_response(&mut self, now: u64, acked: &[OpId], rand: u64) {
        let batch = self.inflight.take().unwrap_or_default();
        if batch.iter().any(|id| acked.contains(id)) {
            self.attempt = 0;
            self.not_before = 0;
        } else {
            self.schedule_retry(now, rand);
        }
    }

    /// The request failed (network error, timeout, 5xx): the batch stays in the
    /// outbox with the same op ids and is retried after the backoff.
    pub fn on_failure(&mut self, now: u64, rand: u64) {
        self.inflight = None;
        self.schedule_retry(now, rand);
    }

    fn schedule_retry(&mut self, now: u64, rand: u64) {
        self.not_before = now.saturating_add(self.backoff.delay(self.attempt, rand));
        self.attempt = self.attempt.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded_and_jittered() {
        let b = Backoff {
            base_ms: 100,
            cap_ms: 10_000,
        };
        for attempt in 0..80 {
            for rand in [0, 1, 7, u64::MAX] {
                let d = b.delay(attempt, rand);
                let full = (100u64 << attempt.min(32)).min(10_000);
                assert!(d >= full / 2 && d <= full && d <= 10_000, "{attempt} {d}");
            }
        }
        assert_ne!(b.delay(3, 1), b.delay(3, 2));
    }

    #[test]
    fn batches_retries_and_resets() {
        let ids: Vec<OpId> = (1..=5).map(|n| OpId::new("d", n)).collect();
        let mut m = ClientMachine::new(2, Backoff::default());
        assert_eq!(m.next(0, &ids), Step::Send(ids[..2].to_vec()));
        assert_eq!(m.next(0, &ids), Step::InFlight);
        m.on_failure(0, 0);
        assert!(matches!(m.next(1, &ids), Step::WaitUntil(t) if t >= 250));
        assert_eq!(m.next(1_000, &ids), Step::Send(ids[..2].to_vec()));
        m.on_response(1_000, &ids[..2], 0);
        assert_eq!(m.attempt(), 0);
        assert_eq!(m.next(1_000, &ids[2..]), Step::Send(ids[2..4].to_vec()));
        m.on_response(1_000, &[], 0);
        assert_eq!(m.attempt(), 1);
        assert_eq!(m.next(u64::MAX, &[]), Step::Idle);
    }
}
