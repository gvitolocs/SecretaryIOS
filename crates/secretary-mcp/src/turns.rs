//! `ask_lucina` answers within a caller's patience. MCP clients give up on their own clocks (ZCode at 30 s), and a
//! photo or a computer task can take minutes: so the turn runs on its own task, the call returns her answer if it
//! comes in time and otherwise an id, and `lucina_job_status` picks the answer up. Answers are kept in memory for
//! an hour, and only their caller reads them; a restart loses them, but her reply is still on the timeline.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::backend::BackendError;

pub const PREFIX: &str = "turn_";
/// Under the shortest client limit seen.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(25);
/// `wait_seconds` at most: a call never outlives secretary-server's own 120 s for a turn by much.
pub const MAX_WAIT_SECS: u64 = 110;
const KEEP: Duration = Duration::from_secs(3600);

#[derive(Clone)]
enum State {
    Working,
    Done(Result<serde_json::Value, BackendError>),
}

struct Turn {
    caller: String,
    at: Instant,
    state: State,
}

/// What a caller sees of a turn.
#[derive(Debug, PartialEq)]
pub enum Seen {
    Unknown,
    Working,
    Done(Result<serde_json::Value, BackendError>),
}

#[derive(Clone, Default)]
pub struct Turns(Arc<Mutex<HashMap<String, Turn>>>);

impl Turns {
    pub fn begin(&self, caller: &str) -> String {
        let id = format!("{PREFIX}{}", uuid::Uuid::new_v4().simple());
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, t| t.at.elapsed() < KEEP);
        m.insert(
            id.clone(),
            Turn { caller: caller.to_owned(), at: Instant::now(), state: State::Working },
        );
        id
    }

    pub fn finish(&self, id: &str, r: Result<serde_json::Value, BackendError>) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = m.get_mut(id) {
            t.at = Instant::now();
            t.state = State::Done(r);
        }
    }

    pub fn see(&self, id: &str, caller: &str) -> Seen {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(id) {
            Some(t) if t.caller == caller => match &t.state {
                State::Working => Seen::Working,
                State::Done(r) => Seen::Done(r.clone()),
            },
            _ => Seen::Unknown,
        }
    }

    /// Waits for the turn to end, at most `wait`.
    pub async fn wait(&self, id: &str, caller: &str, wait: Duration) -> Seen {
        let until = Instant::now() + wait;
        loop {
            let seen = self.see(id, caller);
            if seen != Seen::Working || Instant::now() >= until {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(200).min(until - Instant::now())).await;
        }
    }
}

/// The answer when she isn't done yet.
pub fn still_working(id: &str) -> String {
    format!(
        "Lucina is still working on this and nothing is lost: looking at a photo can first load her vision model, and a computer task takes minutes. Her answer comes from lucina_job_status with job_id {id} and wait_seconds 25; call it again until she is done."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn an_answer_is_kept_for_its_caller_only() {
        let t = Turns::default();
        let id = t.begin("zcode");
        assert_eq!(t.see(&id, "zcode"), Seen::Working);
        assert_eq!(t.wait(&id, "zcode", Duration::from_millis(20)).await, Seen::Working);
        t.finish(&id, Ok(json!({"reply": "yes"})));
        assert_eq!(t.wait(&id, "zcode", Duration::from_secs(5)).await, Seen::Done(Ok(json!({"reply": "yes"}))));
        assert_eq!(t.see(&id, "mistral"), Seen::Unknown, "another client cannot read it");
        assert_eq!(t.see("turn_nope", "zcode"), Seen::Unknown);
    }
}
