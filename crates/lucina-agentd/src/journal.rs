//! The numbered event bus behind `GET /stream` (design: "Resumable delivery").
//!
//! Every bus event gets an id (see [`lucina_agents::stream`]: increasing, and
//! increasing across restarts) and is kept for 24 h or the last 10,000 events. A client
//! that reconnects with `Last-Event-ID` gets the kept events after it; one that fell
//! further behind gets `stream.reset`. Ids are assigned and broadcast under one lock,
//! so live delivery is in id order and a subscriber never sees a gap between its
//! replay and the live events.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use lucina_agents::stream::{self, Resume};
use tokio::sync::broadcast;

use crate::manager::BusEvent;

/// One numbered event, serialized once.
#[derive(Debug)]
pub struct Sequenced {
    pub id: u64,
    pub at_ms: u64,
    pub event: BusEvent,
    pub data: String,
}

struct State {
    next: u64,
    kept: VecDeque<Arc<Sequenced>>,
    keep_max: usize,
    keep_ms: u64,
}

impl State {
    fn oldest(&self) -> Option<u64> {
        self.kept.front().map(|e| e.id)
    }

    fn after(&self, last: u64) -> Vec<Arc<Sequenced>> {
        self.kept.iter().filter(|e| e.id > last).cloned().collect()
    }

    fn evict(&mut self, now: u64) {
        while self.kept.len() > self.keep_max || self.kept.front().is_some_and(|e| now.saturating_sub(e.at_ms) > self.keep_ms) {
            self.kept.pop_front();
        }
    }
}

/// What a new stream starts with.
pub struct Subscription {
    pub resume: Resume,
    /// Kept events to send before the live ones (empty unless `resume` is `After`).
    pub replay: Vec<Arc<Sequenced>>,
    /// The last id issued when the subscription started: live events come after it.
    pub last: u64,
    pub live: broadcast::Receiver<Arc<Sequenced>>,
}

#[derive(Clone)]
pub struct Journal {
    state: Arc<Mutex<State>>,
    tx: broadcast::Sender<Arc<Sequenced>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Journal {
    /// The production window ([`stream::KEEP_MAX`] events, [`stream::KEEP_MS`]); the
    /// size can be lowered with `LUCINA_AGENTD_STREAM_KEEP` (tests).
    pub fn new() -> Self {
        let keep_max = std::env::var("LUCINA_AGENTD_STREAM_KEEP")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(stream::KEEP_MAX)
            .min(stream::KEEP_MAX);
        Self::with(stream::first_id(now_ms()), keep_max, stream::KEEP_MS)
    }

    pub fn with(first_id: u64, keep_max: usize, keep_ms: u64) -> Self {
        let (tx, _) = broadcast::channel(4096);
        Journal {
            state: Arc::new(Mutex::new(State { next: first_id, kept: VecDeque::new(), keep_max, keep_ms })),
            tx,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Number, keep and broadcast one event; returns its id.
    pub fn send(&self, event: BusEvent) -> u64 {
        let data = match serde_json::to_string(&event) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "unserializable bus event dropped");
                return 0;
            }
        };
        let now = now_ms();
        let mut st = self.lock();
        let id = st.next;
        st.next += 1;
        let entry = Arc::new(Sequenced { id, at_ms: now, event, data });
        st.kept.push_back(entry.clone());
        st.evict(now);
        let _ = self.tx.send(entry);
        id
    }

    /// Start a stream for a client whose last seen id is `last_event_id`.
    pub fn subscribe(&self, last_event_id: Option<&str>) -> Subscription {
        let mut st = self.lock();
        st.evict(now_ms());
        let resume = stream::resume(last_event_id, st.oldest(), st.next);
        let replay = match resume {
            Resume::After(l) => st.after(l),
            Resume::Live | Resume::Reset => Vec::new(),
        };
        Subscription { resume, replay, last: st.next - 1, live: self.tx.subscribe() }
    }

    /// The kept events after `last`, or `None` when some of them were evicted (a
    /// subscriber that lagged behind the broadcast channel catches up from here).
    pub fn since(&self, last: u64) -> Option<Vec<Arc<Sequenced>>> {
        let st = self.lock();
        match stream::resume(Some(&last.to_string()), st.oldest(), st.next) {
            Resume::After(l) => Some(st.after(l)),
            _ => None,
        }
    }

    /// The last id issued.
    pub fn last_id(&self) -> u64 {
        self.lock().next - 1
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(n: usize) -> BusEvent {
        BusEvent::AgentDeleted { agent_id: format!("a{n}") }
    }

    #[test]
    fn replay_after_last_seen() {
        let j = Journal::with(1000, 5, 60_000);
        for n in 0..3 {
            j.send(ev(n));
        }
        let s = j.subscribe(Some("1000"));
        assert_eq!(s.resume, Resume::After(1000));
        assert_eq!(s.replay.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1001, 1002]);
        assert_eq!(s.last, 1002);
        assert!(s.replay[0].data.contains("\"agentId\":\"a1\""));
        let s = j.subscribe(Some("1002"));
        assert!(s.replay.is_empty());
        assert_eq!(j.subscribe(None).resume, Resume::Live);
    }

    #[test]
    fn evicted_or_foreign_ids_reset() {
        let j = Journal::with(1000, 3, 60_000);
        for n in 0..6 {
            j.send(ev(n));
        }
        // Kept: 1003..=1005.
        assert_eq!(j.subscribe(Some("1002")).resume, Resume::After(1002));
        assert_eq!(j.subscribe(Some("1001")).resume, Resume::Reset);
        assert_eq!(j.subscribe(Some("1006")).resume, Resume::Reset);
        assert_eq!(j.subscribe(Some("garbage")).resume, Resume::Reset);
        assert!(j.since(1001).is_none());
        assert_eq!(j.since(1003).unwrap().len(), 2);
    }

    #[test]
    fn a_fresh_run_resumes_from_just_before_its_first_id() {
        let j = Journal::with(5000, 10, 60_000);
        assert_eq!(j.subscribe(Some("4999")).resume, Resume::After(4999));
        assert_eq!(j.subscribe(Some("4000")).resume, Resume::Reset, "an earlier run");
        assert_eq!(j.last_id(), 4999);
    }

    #[test]
    fn old_events_age_out() {
        let j = Journal::with(1000, 10, 0);
        j.send(ev(0));
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(j.subscribe(Some("999")).resume, Resume::Reset);
    }

    #[tokio::test]
    async fn live_events_follow_the_replay_without_a_gap() {
        let j = Journal::with(1, 100, 60_000);
        j.send(ev(0));
        let mut s = j.subscribe(Some("1"));
        j.send(ev(1));
        let next = s.live.recv().await.unwrap();
        assert_eq!(next.id, s.last + 1);
    }
}
