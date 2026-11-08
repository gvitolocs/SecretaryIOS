//! Persistent state: today's budget use per agent, the owner's switches
//! (agents off, "stop delegating") and the pauses a cap caused.
//!
//! Budgets are a silent runaway guard: a call that fits is never asked
//! about; the call that hits a cap pauses delegation (that agent, or all of
//! it) until the day changes, and the caller tells the owner once.
//!
//! A backend that answered "out of credits", "not logged in" or "rate limit" is remembered as down
//! ([`Down`]) for a few hours, across restarts, so the next request does not burn the same minutes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Budget;
use crate::policy::Refusal;
use crate::types::AgentId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub calls: u32,
    pub wall_ms: u64,
    pub turns: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    #[serde(default)]
    day: String,
    #[serde(default)]
    usage: BTreeMap<String, Usage>,
    #[serde(default)]
    total_calls: u32,
    /// Agents the owner switched off.
    #[serde(default)]
    disabled: BTreeSet<String>,
    /// "stop delegating".
    #[serde(default)]
    stopped: bool,
    /// `agent` or `all` -> why (cleared when the day changes).
    #[serde(default)]
    paused: BTreeMap<String, String>,
    /// `agent` -> its backend that is out of credits, signed out or rate-limited (kept across days).
    #[serde(default)]
    down: BTreeMap<String, Down>,
}

/// A backend that said it cannot work (out of credits, signed out, rate-limited).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Down {
    pub backend: String,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    /// `out of credits`, `signed out` or `rate-limited`.
    pub state: String,
    /// The error it gave, clipped.
    pub reason: String,
}

/// What a failed run's error says about its backend: the state word and how long to leave it alone.
pub fn outage(error: &str) -> Option<(&'static str, Duration)> {
    static RES: OnceLock<[Regex; 3]> = OnceLock::new();
    let [credits, auth, rate] = RES.get_or_init(|| {
        let re = |p: &str| Regex::new(p).expect("outage regex");
        [
            re(r"(?i)out of credits|insufficient credits|credit balance|usage limit|quota|billing|payment required|\b402\b"),
            re(r"(?i)not logged in|login|unauthorized|\b401\b|expired"),
            re(r"(?i)rate limit|\b429\b|too many requests"),
        ]
    });
    if credits.is_match(error) {
        Some(("out of credits", Duration::hours(6)))
    } else if auth.is_match(error) {
        Some(("signed out", Duration::hours(6)))
    } else if rate.is_match(error) {
        Some(("rate-limited", Duration::minutes(30)))
    } else {
        None
    }
}

/// A cap was just hit (tell the owner once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseNotice {
    pub scope: String,
    pub why: String,
}

#[derive(Debug)]
pub struct StateStore {
    path: Option<PathBuf>,
    tz: Tz,
    mem: Mutex<StateFile>,
}

impl StateStore {
    pub fn new(path: Option<PathBuf>, tz: Tz) -> Self {
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<StateFile>(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            tz,
            mem: Mutex::new(file),
        }
    }

    fn today(&self, now: DateTime<Utc>) -> String {
        now.with_timezone(&self.tz).format("%Y-%m-%d").to_string()
    }

    fn with<R>(&self, now: DateTime<Utc>, f: impl FnOnce(&mut StateFile) -> R) -> R {
        let mut g = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        let today = self.today(now);
        if g.day != today {
            g.day = today;
            g.usage.clear();
            g.total_calls = 0;
            g.paused.clear();
        }
        let r = f(&mut g);
        if let Some(p) = &self.path {
            if let Ok(text) = serde_json::to_vec_pretty(&*g) {
                if let Err(e) = lucina_store::write_atomic(p, &text) {
                    tracing::warn!(error = %e, "delegate state write failed");
                }
            }
        }
        r
    }

    /// "stop delegating" and a pause of all delegation.
    pub fn check_global(&self, now: DateTime<Utc>) -> Result<(), Refusal> {
        self.with(now, |s| {
            if s.stopped {
                return Err(Refusal::Stopped);
            }
            if let Some(why) = s.paused.get("all") {
                return Err(Refusal::Paused(why.clone()));
            }
            Ok(())
        })
    }

    /// The owner's switches and today's pauses, without reserving anything.
    pub fn check(&self, agent: AgentId, now: DateTime<Utc>) -> Result<(), Refusal> {
        self.with(now, |s| {
            if s.stopped {
                return Err(Refusal::Stopped);
            }
            if s.disabled.contains(agent.as_str()) {
                return Err(Refusal::AgentOff(agent));
            }
            if let Some(why) = s.paused.get("all").or_else(|| s.paused.get(agent.as_str())) {
                return Err(Refusal::Paused(why.clone()));
            }
            Ok(())
        })
    }

    /// Count one call for `agent`. A cap already reached pauses delegation
    /// (`Some(notice)` the first time) and refuses the call.
    pub fn reserve(
        &self,
        agent: AgentId,
        budget: &Budget,
        now: DateTime<Utc>,
    ) -> Result<(), (Refusal, Option<PauseNotice>)> {
        self.with(now, |s| {
            if s.stopped {
                return Err((Refusal::Stopped, None));
            }
            if s.disabled.contains(agent.as_str()) {
                return Err((Refusal::AgentOff(agent), None));
            }
            if let Some(why) = s.paused.get("all").or_else(|| s.paused.get(agent.as_str())) {
                return Err((Refusal::Paused(why.clone()), None));
            }
            let b = budget.of(agent);
            let u = s.usage.get(agent.as_str()).copied().unwrap_or_default();
            let hit = if s.total_calls >= budget.total_calls_per_day {
                Some((
                    "all".to_owned(),
                    format!("{} delegations today (cap)", s.total_calls),
                ))
            } else if u.calls >= b.calls_per_day {
                Some((
                    agent.as_str().to_owned(),
                    format!("{} calls to {} today (cap)", u.calls, agent.label()),
                ))
            } else if u.wall_ms >= u64::from(b.wall_minutes_per_day) * 60_000 {
                Some((
                    agent.as_str().to_owned(),
                    format!(
                        "{} minutes of {} today (cap)",
                        u.wall_ms / 60_000,
                        agent.label()
                    ),
                ))
            } else if b.turns_per_day > 0 && u.turns >= b.turns_per_day {
                Some((
                    agent.as_str().to_owned(),
                    format!("{} {} turns today (cap)", u.turns, agent.label()),
                ))
            } else {
                None
            };
            if let Some((scope, why)) = hit {
                let newly = s.paused.insert(scope.clone(), why.clone()).is_none();
                return Err((
                    Refusal::Paused(why.clone()),
                    newly.then_some(PauseNotice { scope, why }),
                ));
            }
            let e = s.usage.entry(agent.as_str().to_owned()).or_default();
            e.calls += 1;
            s.total_calls += 1;
            Ok(())
        })
    }

    /// Give back a reserved call that never ran (a skipped judgement).
    pub fn release(&self, agent: AgentId, now: DateTime<Utc>) {
        self.with(now, |s| {
            if let Some(e) = s.usage.get_mut(agent.as_str()) {
                e.calls = e.calls.saturating_sub(1);
            }
            s.total_calls = s.total_calls.saturating_sub(1);
        });
    }

    /// Add a finished job's wall time and turns.
    pub fn record(&self, agent: AgentId, wall_ms: u64, turns: u32, now: DateTime<Utc>) -> Usage {
        self.with(now, |s| {
            let e = s.usage.entry(agent.as_str().to_owned()).or_default();
            e.wall_ms += wall_ms;
            e.turns += turns;
            *e
        })
    }

    /// Claude turns left today (for `--max-turns`).
    pub fn turns_left(&self, agent: AgentId, budget: &Budget, now: DateTime<Utc>) -> u32 {
        let cap = budget.of(agent).turns_per_day;
        if cap == 0 {
            return u32::MAX;
        }
        self.with(now, |s| {
            cap.saturating_sub(s.usage.get(agent.as_str()).map_or(0, |u| u.turns))
        })
    }

    pub fn usage(&self, agent: AgentId, now: DateTime<Utc>) -> (Usage, u32) {
        self.with(now, |s| {
            (
                s.usage.get(agent.as_str()).copied().unwrap_or_default(),
                s.total_calls,
            )
        })
    }

    /// Remember that `agent`'s backend is down until `down.until`.
    pub fn mark_down(&self, agent: AgentId, down: Down, now: DateTime<Utc>) {
        self.with(now, |s| {
            s.down.insert(agent.as_str().to_owned(), down);
        });
    }

    /// A job of `agent` succeeded: it works again.
    pub fn clear_down(&self, agent: AgentId, now: DateTime<Utc>) {
        self.with(now, |s| {
            s.down.remove(agent.as_str());
        });
    }

    /// `agent`'s outage, while it lasts (any backend). An expired one is forgotten.
    pub fn down(&self, agent: AgentId, now: DateTime<Utc>) -> Option<Down> {
        self.with(now, |s| {
            s.down.retain(|_, d| d.until > now);
            s.down.get(agent.as_str()).cloned()
        })
    }

    /// The owner's switch. `None`: all delegation.
    pub fn set_enabled(&self, agent: Option<AgentId>, on: bool, now: DateTime<Utc>) {
        self.with(now, |s| match agent {
            Some(a) if on => {
                s.disabled.remove(a.as_str());
                // Turning an agent on also lifts a global stop for it.
                s.stopped = false;
            }
            Some(a) => {
                s.disabled.insert(a.as_str().to_owned());
            }
            None => s.stopped = !on,
        });
    }

    pub fn snapshot(&self, budget: &Budget, now: DateTime<Utc>) -> Value {
        let tz = self.tz;
        self.with(now, |s| {
            let agents: serde_json::Map<String, Value> = AgentId::ALL
                .iter()
                .map(|a| {
                    let u = s.usage.get(a.as_str()).copied().unwrap_or_default();
                    let b = budget.of(*a);
                    let health = match s.down.get(a.as_str()).filter(|d| d.until > now) {
                        Some(d) => format!(
                            "down until {}: {} {} ({})",
                            d.until.with_timezone(&tz).format("%H:%M"),
                            d.backend,
                            d.state,
                            d.reason
                        ),
                        None => "ok".to_owned(),
                    };
                    (
                        a.as_str().to_owned(),
                        json!({
                            "health": health,
                            "enabled": !s.disabled.contains(a.as_str()),
                            "paused": s.paused.get(a.as_str()),
                            "calls": u.calls, "callsCap": b.calls_per_day,
                            "wallMinutes": u.wall_ms / 60_000, "wallMinutesCap": b.wall_minutes_per_day,
                            "turns": u.turns, "turnsCap": b.turns_per_day,
                        }),
                    )
                })
                .collect();
            json!({
                "day": s.day,
                "stopped": s.stopped,
                "paused": s.paused.get("all"),
                "totalCalls": s.total_calls,
                "totalCallsCap": budget.total_calls_per_day,
                "agents": agents,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentBudget;

    fn budget() -> Budget {
        Budget {
            total_calls_per_day: 5,
            claude: AgentBudget {
                calls_per_day: 3,
                wall_minutes_per_day: 10,
                turns_per_day: 50,
            },
            grok: AgentBudget {
                calls_per_day: 10,
                wall_minutes_per_day: 1,
                turns_per_day: 0,
            },
            chatgpt: AgentBudget::default(),
            mistral: AgentBudget::default(),
        }
    }

    #[test]
    fn caps_pause_once_and_reset_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let s = StateStore::new(Some(path.clone()), chrono_tz::Europe::Rome);
        let b = budget();
        let now = Utc::now();
        for _ in 0..3 {
            s.reserve(AgentId::Claude, &b, now).unwrap();
        }
        let (r, notice) = s.reserve(AgentId::Claude, &b, now).unwrap_err();
        assert!(matches!(r, Refusal::Paused(_)));
        assert_eq!(notice.unwrap().scope, "claude");
        let (_, notice) = s.reserve(AgentId::Claude, &b, now).unwrap_err();
        assert!(notice.is_none(), "the owner is told once");
        // other agents still run, until the total cap
        s.reserve(AgentId::Chatgpt, &b, now).unwrap();
        s.reserve(AgentId::Chatgpt, &b, now).unwrap();
        let (_, notice) = s.reserve(AgentId::Grok, &b, now).unwrap_err();
        assert_eq!(notice.unwrap().scope, "all");
        assert!(matches!(
            s.check(AgentId::Grok, now),
            Err(Refusal::Paused(_))
        ));
        // persisted
        let again = StateStore::new(Some(path), chrono_tz::Europe::Rome);
        assert!(again.check(AgentId::Chatgpt, now).is_err());
        // tomorrow
        let tomorrow = now + chrono::Duration::days(1);
        assert!(again.reserve(AgentId::Claude, &b, tomorrow).is_ok());
    }

    #[test]
    fn wall_time_and_turns() {
        let s = StateStore::new(None, chrono_tz::Europe::Rome);
        let b = budget();
        let now = Utc::now();
        s.reserve(AgentId::Grok, &b, now).unwrap();
        s.record(AgentId::Grok, 61_000, 0, now);
        assert!(matches!(
            s.reserve(AgentId::Grok, &b, now).unwrap_err().0,
            Refusal::Paused(_)
        ));
        assert_eq!(s.turns_left(AgentId::Claude, &b, now), 50);
        s.reserve(AgentId::Claude, &b, now).unwrap();
        s.record(AgentId::Claude, 1000, 50, now);
        assert_eq!(s.turns_left(AgentId::Claude, &b, now), 0);
        assert!(s.reserve(AgentId::Claude, &b, now).is_err());
        s.release(AgentId::Chatgpt, now);
    }

    #[test]
    fn switches() {
        let s = StateStore::new(None, chrono_tz::Europe::Rome);
        let now = Utc::now();
        s.set_enabled(Some(AgentId::Grok), false, now);
        assert_eq!(
            s.check(AgentId::Grok, now),
            Err(Refusal::AgentOff(AgentId::Grok))
        );
        assert!(s.check(AgentId::Claude, now).is_ok());
        s.set_enabled(None, false, now);
        assert_eq!(s.check(AgentId::Claude, now), Err(Refusal::Stopped));
        s.set_enabled(None, true, now);
        s.set_enabled(Some(AgentId::Grok), true, now);
        assert!(s.check(AgentId::Grok, now).is_ok());
        let snap = s.snapshot(&budget(), now);
        assert_eq!(snap["agents"]["grok"]["enabled"], true);
        assert_eq!(snap["agents"]["mistral"]["health"], "ok");
    }

    #[test]
    fn outages_are_classified_remembered_and_expire() {
        assert_eq!(
            outage("ERROR: Your workspace is out of credits").map(|o| o.0),
            Some("out of credits")
        );
        assert_eq!(
            outage("HTTP 402 Payment Required").unwrap().1,
            Duration::hours(6)
        );
        assert_eq!(
            outage("codex: Not logged in").map(|o| o.0),
            Some("signed out")
        );
        assert_eq!(outage("token EXPIRED").map(|o| o.0), Some("signed out"));
        assert_eq!(
            outage("429 Too Many Requests"),
            Some(("rate-limited", Duration::minutes(30)))
        );
        assert_eq!(
            outage("insufficient_quota (429)").map(|o| o.0),
            Some("out of credits")
        );
        assert_eq!(outage("claude timed out after 900 s"), None);
        assert_eq!(outage("exit 14012"), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let s = StateStore::new(Some(path.clone()), chrono_tz::Europe::Rome);
        let now = Utc::now();
        s.mark_down(
            AgentId::Chatgpt,
            Down {
                backend: "codex-cli".into(),
                since: now,
                until: now + Duration::hours(6),
                state: "out of credits".into(),
                reason: "Your workspace is out of credits".into(),
            },
            now,
        );
        // kept across a restart and a day change
        let again = StateStore::new(Some(path), chrono_tz::Europe::Rome);
        assert!(again
            .down(AgentId::Chatgpt, now + Duration::hours(5))
            .is_some());
        let snap = again.snapshot(&budget(), now);
        assert!(snap["agents"]["chatgpt"]["health"]
            .as_str()
            .unwrap()
            .starts_with("down until "));
        assert!(again
            .down(AgentId::Chatgpt, now + Duration::hours(7))
            .is_none());
        assert!(
            again.down(AgentId::Chatgpt, now).is_none(),
            "an expired outage is forgotten"
        );
        s.clear_down(AgentId::Chatgpt, now);
        assert!(s.down(AgentId::Chatgpt, now).is_none());
    }
}
