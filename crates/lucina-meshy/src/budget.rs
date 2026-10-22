//! Credit budget: what Lucina may spend at Meshy.
//!
//! - **Per plan** (`LUCINA_MESHY_MAX_PLAN_CREDITS`, default 60): a larger plan
//!   is refused before the owner is even asked.
//! - **Per day** (`LUCINA_MESHY_DAILY_CREDITS`, default 60, the owner's time
//!   zone): reserved + spent today plus the new plan must fit.
//! - **Hard cap** ([`HARD_CAP_PER_DAY`], compiled in): both settings are
//!   clamped to it, so a typo in the config cannot open the wallet.
//!
//! A confirmed plan **reserves** its estimate before the first Meshy call;
//! when the job ends the entry is **settled** with the credits Meshy reports
//! (`consumed_credits`; failed tasks are refunded by Meshy, so they count 0).
//! The ledger is `data/lucina-rs/meshy-ledger.json` (atomic, 0600).

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_store::write_atomic;
use serde::{Deserialize, Serialize};

/// No setting can raise the daily spend above this.
pub const HARD_CAP_PER_DAY: u32 = 300;
const KEEP_DAYS: i64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub daily_budget: u32,
    pub max_plan: u32,
}

impl Limits {
    pub fn new(daily_budget: u32, max_plan: u32) -> Self {
        let daily_budget = daily_budget.min(HARD_CAP_PER_DAY);
        Self {
            daily_budget,
            max_plan: max_plan.min(daily_budget),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new(60, 60)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitError {
    #[error("this plan costs {cost} credits, over the {cap}-credit limit per request (LUCINA_MESHY_MAX_PLAN_CREDITS); nothing was spent")]
    OverPlanCap { cost: u32, cap: u32 },
    #[error("this plan costs {cost} credits but only {left} of today's {budget}-credit budget are left (LUCINA_MESHY_DAILY_CREDITS); nothing was spent")]
    OverDailyBudget { cost: u32, left: u32, budget: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub job_id: String,
    /// `YYYY-MM-DD` in the owner's time zone.
    pub day: String,
    pub reserved: u32,
    #[serde(default)]
    pub consumed: Option<u32>,
    /// reserved | settled
    pub status: String,
    pub at: String,
}

impl Entry {
    fn counts(&self) -> u32 {
        match self.consumed {
            Some(c) => c,
            None => self.reserved,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Debug)]
pub struct Ledger {
    path: Option<PathBuf>,
    tz: Tz,
    mem: Mutex<Vec<Entry>>,
}

impl Ledger {
    pub fn file(path: impl Into<PathBuf>, tz: Tz) -> Self {
        Self {
            path: Some(path.into()),
            tz,
            mem: Mutex::new(Vec::new()),
        }
    }

    pub fn memory(tz: Tz) -> Self {
        Self {
            path: None,
            tz,
            mem: Mutex::new(Vec::new()),
        }
    }

    fn day(&self, now: DateTime<Utc>) -> String {
        now.with_timezone(&self.tz).format("%Y-%m-%d").to_string()
    }

    fn with<R>(&self, write: bool, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
        let mut g = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = &self.path {
            *g = std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<File>(&b).ok())
                .map(|f| f.entries)
                .unwrap_or_default();
        }
        let r = f(&mut g);
        if write {
            if let Some(p) = &self.path {
                let text = serde_json::to_string_pretty(&serde_json::json!({"entries": *g}))
                    .unwrap_or_default();
                if let Err(e) = write_atomic(p, format!("{text}\n").as_bytes()) {
                    tracing::warn!(error = %e, "meshy ledger write failed");
                }
            }
        }
        r
    }

    fn spent_on(list: &[Entry], day: &str) -> u32 {
        list.iter()
            .filter(|e| e.day == day)
            .map(Entry::counts)
            .sum()
    }

    /// Credits reserved or spent today.
    pub fn spent_today(&self, now: DateTime<Utc>) -> u32 {
        let day = self.day(now);
        self.with(false, |l| Self::spent_on(l, &day))
    }

    fn check_in(list: &[Entry], day: &str, limits: Limits, cost: u32) -> Result<(), LimitError> {
        if cost > limits.max_plan {
            return Err(LimitError::OverPlanCap {
                cost,
                cap: limits.max_plan,
            });
        }
        let left = limits
            .daily_budget
            .saturating_sub(Self::spent_on(list, day));
        if cost > left {
            return Err(LimitError::OverDailyBudget {
                cost,
                left,
                budget: limits.daily_budget,
            });
        }
        Ok(())
    }

    /// Would `cost` fit right now?
    pub fn check(&self, limits: Limits, cost: u32, now: DateTime<Utc>) -> Result<(), LimitError> {
        let day = self.day(now);
        self.with(false, |l| Self::check_in(l, &day, limits, cost))
    }

    /// Check and reserve in one step (two confirmations racing cannot both fit).
    pub fn reserve(
        &self,
        limits: Limits,
        job_id: &str,
        cost: u32,
        now: DateTime<Utc>,
    ) -> Result<(), LimitError> {
        let day = self.day(now);
        let cutoff = self.day(now - chrono::Duration::days(KEEP_DAYS));
        self.with(true, |l| {
            l.retain(|e| e.day >= cutoff);
            Self::check_in(l, &day, limits, cost)?;
            l.push(Entry {
                job_id: job_id.to_owned(),
                day,
                reserved: cost,
                consumed: None,
                status: "reserved".into(),
                at: now.to_rfc3339(),
            });
            Ok(())
        })
    }

    /// The job ended: record what Meshy actually charged.
    pub fn settle(&self, job_id: &str, consumed: u32) {
        self.with(true, |l| {
            if let Some(e) = l.iter_mut().find(|e| e.job_id == job_id) {
                e.consumed = Some(consumed);
                e.status = "settled".into();
            }
        });
    }

    pub fn entry(&self, job_id: &str) -> Option<Entry> {
        self.with(false, |l| l.iter().find(|e| e.job_id == job_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_and_daily_budget() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::file(dir.path().join("ledger.json"), chrono_tz::Europe::Rome);
        let limits = Limits::new(60, 45);
        let now = Utc::now();
        assert_eq!(
            l.check(limits, 50, now),
            Err(LimitError::OverPlanCap { cost: 50, cap: 45 })
        );
        l.reserve(limits, "j1", 41, now).unwrap();
        assert_eq!(l.spent_today(now), 41);
        assert_eq!(
            l.reserve(limits, "j2", 20, now),
            Err(LimitError::OverDailyBudget {
                cost: 20,
                left: 19,
                budget: 60
            })
        );
        // Meshy refunded a failed rig: only 30 were charged.
        l.settle("j1", 30);
        assert_eq!(l.spent_today(now), 30);
        l.reserve(limits, "j2", 20, now).unwrap();
        // tomorrow is a new day
        assert!(l.check(limits, 45, now + chrono::Duration::days(1)).is_ok());
        // persisted
        let again = Ledger::file(dir.path().join("ledger.json"), chrono_tz::Europe::Rome);
        assert_eq!(again.spent_today(now), 50);
        assert_eq!(again.entry("j1").unwrap().consumed, Some(30));
    }

    #[test]
    fn hard_cap_clamps_settings() {
        let l = Limits::new(100_000, 100_000);
        assert_eq!(l.daily_budget, HARD_CAP_PER_DAY);
        assert_eq!(l.max_plan, HARD_CAP_PER_DAY);
        assert_eq!(
            Limits::new(30, 60).max_plan,
            30,
            "a plan never exceeds the day"
        );
    }
}
