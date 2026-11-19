//! What keeps a dispatcher from burying the host (AGENT-01, 2026-11-19: 29 agents created
//! in one second, each failed on an expired login in 2 s, and every one kept its provider
//! and MCP processes, 8.8 GiB).
//!
//! 1. **Release**: an agent that turned `error` and is not being handed on gives up its
//!    session ([`Manager::release_if_errored`]): the provider process and the processes
//!    under it end ([`crate::process::kill_tree`]). The row keeps status `error`, its log and
//!    its timeline; a later `send` starts a new session for it.
//! 2. **Provider breaker**: a turn that failed on authentication, or on a quota with no
//!    one to hand on to, marks its provider unavailable ([`Breaker`]); `create` for it is
//!    refused until the first successful turn of that provider, the reset time a quota
//!    error carries, or `lagent providers clear <provider>`. Kept in memory and in
//!    `provider-breaker.json` in the state dir. The trip is logged once as a `tracing`
//!    warning with the stable message `provider breaker tripped` (agentd has no ops-record
//!    helper).
//! 3. **Create caps** ([`CapsConfig`]): counted are only agents made through
//!    [`Manager::create`] with a caller (`CreateRequest::created_by`: set by the create route, by
//!    a fork for the forking caller, and by a schedule fire for the schedule's owner)
//!    for which agentd spawns a provider process. NOT counted: worker runs of
//!    `lagent work start` (lane's own pool is their limit), merge and deploy jobs (the
//!    pipeline's `lagent-job` units), `claude-session` and other external agents (nothing
//!    is spawned), helper agents, and continuations of a hand-off. The caps never refuse
//!    a work run, a merge or a deploy.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use lucina_agents::external as ext;
use lucina_agents::{AgentStatus, TimelineItem};
use serde::{Deserialize, Serialize};

use super::handoff::HANDOFF_LABEL;
use super::{Agent, Inner, Manager, ManagerError};
use crate::lane::Outcome;

/// Who created the agent (the API's caller identity); set by `create`, never from a body.
pub const CALLER_LABEL: &str = "lucina.created-by";

/// The rolling window of the creates-per-caller cap.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The create caps. `0` turns a cap off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsConfig {
    /// Live spawned agents one caller may hold (`LUCINA_AGENTD_MAX_AGENTS_PER_CALLER`, 4).
    pub per_caller: usize,
    /// Creates one caller may make in [`WINDOW`] (`LUCINA_AGENTD_MAX_CREATES_PER_MINUTE`, 4).
    pub per_minute: usize,
    /// Live spawned agents on the host (`LUCINA_AGENTD_MAX_AGENTS_HOST`, 12).
    pub host: usize,
}

impl Default for CapsConfig {
    fn default() -> Self {
        CapsConfig {
            per_caller: 4,
            per_minute: 4,
            host: 12,
        }
    }
}

impl CapsConfig {
    pub fn from_env() -> Self {
        let d = CapsConfig::default();
        let num = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(default)
        };
        CapsConfig {
            per_caller: num("LUCINA_AGENTD_MAX_AGENTS_PER_CALLER", d.per_caller),
            per_minute: num("LUCINA_AGENTD_MAX_CREATES_PER_MINUTE", d.per_minute),
            host: num("LUCINA_AGENTD_MAX_AGENTS_HOST", d.host),
        }
    }
}

/// The caps' bookkeeping: creates in the window, and slots reserved by creates that
/// are on their way into the agent table.
#[derive(Default)]
pub(super) struct Caps {
    state: Mutex<CapsState>,
}

#[derive(Default)]
struct CapsState {
    windows: HashMap<String, VecDeque<Instant>>,
    reserved: HashMap<String, usize>,
}

/// A reserved slot; released when the agent is in the table, or on any early exit.
pub(super) struct Slot {
    inner: Arc<Inner>,
    caller: String,
    held: bool,
}

impl Slot {
    /// The agent is in the table: count the create in the rolling window, give the slot back
    /// (call with the agent table's lock held, right after the insert, so the agent is never
    /// counted twice). A create that fails before this is not counted by the rate cap.
    pub(super) fn commit(&mut self) {
        if self.held {
            let mut st = self
                .inner
                .caps
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            st.windows
                .entry(self.caller.clone())
                .or_default()
                .push_back(Instant::now());
        }
        self.release();
    }

    /// Give the slot back without counting the create (a create that failed).
    pub(super) fn release(&mut self) {
        if self.held {
            self.held = false;
            let mut st = self
                .inner
                .caps
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(n) = st.reserved.get_mut(&self.caller) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    st.reserved.remove(&self.caller);
                }
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
    }
}

/// An agent that counts toward the caps: live (starting, running or idle), a provider process
/// agentd spawns, and made for a caller (it carries [`CALLER_LABEL`], which only `create` with
/// `created_by` sets). A hand-off continuation carries none, so it never counts.
fn counts(a: &Agent) -> bool {
    let r = &a.record;
    !r.internal
        && !r.external
        && r.labels.contains_key(CALLER_LABEL)
        && r.archived_at.is_none()
        && !ext::is_session(&r.provider)
        && r.provider != ext::PROVIDER
        && matches!(
            a.k.status,
            AgentStatus::Initializing | AgentStatus::Running | AgentStatus::Idle
        )
}

/// Why a provider is refused, and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trip {
    /// `authentication` or `quota`.
    pub reason: String,
    /// The first line of the failure.
    pub detail: String,
    pub since: String,
    /// The provider's reset time, when the error carried one: the breaker clears then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

impl Trip {
    fn until_time(&self) -> Option<DateTime<Utc>> {
        self.until
            .as_deref()
            .and_then(|u| DateTime::parse_from_rfc3339(u).ok())
            .map(|d| d.with_timezone(&Utc))
    }
}

/// The providers agentd refuses to start agents for.
#[derive(Default)]
pub struct Breaker {
    file: Option<PathBuf>,
    trips: Mutex<BTreeMap<String, Trip>>,
}

impl Breaker {
    /// Load the state file (when there is one); an unreadable file is an empty breaker.
    pub fn open(file: Option<PathBuf>) -> Self {
        let trips = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Breaker {
            file,
            trips: Mutex::new(trips),
        }
    }

    fn save(&self, trips: &BTreeMap<String, Trip>) {
        let Some(file) = &self.file else {
            return;
        };
        let Ok(json) = serde_json::to_string_pretty(trips) else {
            return;
        };
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Trip>> {
        self.trips.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The trip for `provider` that still holds at `now` (an elapsed reset time clears it).
    pub fn check(&self, provider: &str, now: DateTime<Utc>) -> Option<Trip> {
        let mut trips = self.lock();
        let trip = trips.get(provider)?.clone();
        if trip.until_time().is_some_and(|u| u <= now) {
            trips.remove(provider);
            self.save(&trips);
            tracing::info!(provider, reason = %trip.reason, "provider breaker cleared: the reset time passed");
            return None;
        }
        Some(trip)
    }

    /// Mark `provider` unavailable. Returns whether it was available before.
    pub fn trip(&self, provider: &str, trip: Trip) -> bool {
        let mut trips = self.lock();
        let fresh = !trips.contains_key(provider);
        trips.insert(provider.to_string(), trip);
        self.save(&trips);
        fresh
    }

    /// Clear `provider`; returns whether it was tripped.
    pub fn clear(&self, provider: &str) -> bool {
        let mut trips = self.lock();
        let had = trips.remove(provider).is_some();
        if had {
            self.save(&trips);
        }
        had
    }

    /// Every tripped provider.
    pub fn list(&self) -> BTreeMap<String, Trip> {
        self.lock().clone()
    }
}

/// Whether a failure is the provider refusing the login.
pub fn is_auth_failure(text: &str) -> bool {
    text.to_lowercase().contains("failed to authenticate")
}

fn month_number(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| n.starts_with(m))
    .map(|i| i as u32 + 1)
}

/// The reset time a limit message carries, as the lane detectors' texts show it:
/// `resets_at=<RFC 3339>` / `reset at <RFC 3339>`, `resets 8pm`, or
/// `resets Nov 4, 12pm (UTC)`. A time of day without a zone is the owner's zone
/// ([`super::owner_timezone`]); one without a date is the next such time after `now`.
pub fn parse_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    use regex::Regex;
    let iso = Regex::new(
        r"(?i)resets?(?:[_ ]at)?\s*[:=]?\s*(\d{4}-\d{2}-\d{2}T[0-9:.]+(?:Z|[+-]\d{2}:\d{2}))",
    )
    .ok()?;
    if let Some(c) = iso.captures(text) {
        if let Ok(t) = DateTime::parse_from_rfc3339(&c[1]) {
            return Some(t.with_timezone(&Utc));
        }
    }
    let human = Regex::new(
        r"(?i)resets?\s+(?:([a-z]{3})[a-z]*\.?\s+(\d{1,2}),?\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)(?:\s*\(([^)]+)\))?",
    )
    .ok()?;
    let c = human.captures(text)?;
    let hour12: u32 = c[3].parse().ok()?;
    if !(1..=12).contains(&hour12) {
        return None;
    }
    let minute: u32 = c.get(4).map_or(Some(0), |m| m.as_str().parse().ok())?;
    let pm = c[5].eq_ignore_ascii_case("pm");
    let hour = hour12 % 12 + if pm { 12 } else { 0 };
    let zone = c.get(6).map(|z| z.as_str().trim().to_string());
    let tz: chrono_tz::Tz = match zone.as_deref() {
        Some(z) if z.eq_ignore_ascii_case("utc") || z.eq_ignore_ascii_case("gmt") => chrono_tz::UTC,
        Some(z) => z.parse().ok()?,
        None => super::owner_timezone().parse().ok()?,
    };
    let local_now = now.with_timezone(&tz);
    let at = |date: NaiveDate| {
        tz.from_local_datetime(&date.and_hms_opt(hour, minute, 0)?)
            .earliest()
            .map(|t| t.with_timezone(&Utc))
    };
    match (c.get(1), c.get(2)) {
        (Some(m), Some(d)) => {
            let (month, day): (u32, u32) = (month_number(m.as_str())?, d.as_str().parse().ok()?);
            let year = local_now.year();
            let t = at(NaiveDate::from_ymd_opt(year, month, day)?)?;
            if t > now {
                Some(t)
            } else {
                at(NaiveDate::from_ymd_opt(year + 1, month, day)?)
            }
        }
        _ => {
            let today = local_now.date_naive();
            let t = at(today)?;
            if t > now {
                Some(t)
            } else {
                at(today.succ_opt()?)
            }
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

impl Manager {
    /// `create` for `provider` is refused while it is tripped.
    pub(super) fn check_breaker(&self, provider: &str) -> Result<(), ManagerError> {
        match self.inner.breaker.check(provider, Utc::now()) {
            None => Ok(()),
            Some(t) => Err(ManagerError::ProviderUnavailable(format!(
                "Provider {provider} is unavailable: its last turn failed on {} ({}), since {}. New agents for it are refused so they don't pile up. {}Clear it by hand with `lagent providers clear {provider}`.",
                t.reason,
                t.detail,
                t.since,
                match &t.until {
                    Some(u) => format!("It clears by itself at {u}, the provider's reset time, or after the first successful turn. "),
                    None => "It clears by itself after the first successful turn of an agent with this provider. ".into(),
                }
            ))),
        }
    }

    /// Tripped providers, for `lagent providers`.
    pub fn tripped_providers(&self) -> BTreeMap<String, Trip> {
        let now = Utc::now();
        let names: Vec<String> = self.inner.breaker.list().into_keys().collect();
        names
            .into_iter()
            .filter_map(|p| self.inner.breaker.check(&p, now).map(|t| (p, t)))
            .collect()
    }

    /// `lagent providers clear <provider>`: whether it was tripped.
    pub fn clear_provider(&self, provider: &str) -> bool {
        let had = self.inner.breaker.clear(provider);
        if had {
            tracing::info!(provider, "provider breaker cleared by hand");
        }
        had
    }

    /// A turn of `agent` just failed (called with the agent table's lock held, after
    /// its error is set): trip the breaker when it was a login or a quota failure.
    pub(super) fn breaker_observe_failure(&self, agent: &Agent) {
        let r = &agent.record;
        if r.external || ext::is_session(&r.provider) || r.provider == ext::PROVIDER {
            return;
        }
        let text = failure_text(agent);
        let (reason, until) = if is_auth_failure(&text) {
            ("authentication", None)
        } else {
            let quota = text.contains("code: quota")
                || matches!(
                    self.inner
                        .lanes
                        .lane_of_provider(
                            &r.provider,
                            r.labels.get("lucina.lane").map(String::as_str)
                        )
                        .map(|(name, l)| self.inner.lanes.classify(&name, &l.kind, &text, None)),
                    Some(Outcome::Quota(_))
                );
            if !quota {
                return;
            }
            ("quota", parse_reset(&text, Utc::now()))
        };
        if until.is_some_and(|u| u <= Utc::now()) {
            return;
        }
        let trip = Trip {
            reason: reason.into(),
            detail: first_line(&text),
            since: crate::now_iso(),
            until: until.map(|u| u.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        };
        let detail = trip.detail.clone();
        let until_s = trip.until.clone().unwrap_or_default();
        if self.inner.breaker.trip(&r.provider, trip) {
            tracing::warn!(provider = %r.provider, reason, detail = %detail, until = %until_s, agent = %r.id, "provider breaker tripped");
        }
    }

    /// A turn of an agent with this provider succeeded: the provider works again.
    pub(super) fn breaker_observe_success(&self, provider: &str) {
        if self.inner.breaker.clear(provider) {
            tracing::info!(provider, "provider breaker cleared: a turn succeeded");
        }
    }

    /// An agent in `error` that is not being handed on gives up its session: the provider
    /// process and everything under it ends. Called with the agent table's lock held.
    pub(super) fn release_if_errored(&self, agent: &mut Agent) {
        let r = &agent.record;
        if agent.k.status != AgentStatus::Error
            || agent.chain_active
            || r.external
            || ext::is_session(&r.provider)
            || r.provider == ext::PROVIDER
            || r.labels.get(HANDOFF_LABEL).is_some_and(|v| v == "pending")
        {
            return;
        }
        let Some(session) = agent.session.take() else {
            return;
        };
        agent.released = true;
        tracing::info!(agent = %agent.record.id, provider = %agent.record.provider, "agent failed: releasing its provider process and the processes under it");
        tokio::spawn(async move { session.close().await });
    }

    /// Reserve a slot for a create by `caller`, or refuse it (HTTP 429).
    pub(super) fn reserve_slot(&self, caller: &str) -> Result<Slot, ManagerError> {
        let cfg = self.inner.options.caps;
        let agents = self.lock();
        let mut st = self
            .inner
            .caps
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut held: BTreeMap<String, usize> = BTreeMap::new();
        for a in agents.values().filter(|a| counts(a)) {
            let who = a
                .record
                .labels
                .get(CALLER_LABEL)
                .cloned()
                .unwrap_or_else(|| "(no caller)".into());
            *held.entry(who).or_default() += 1;
        }
        for (who, n) in &st.reserved {
            *held.entry(who.clone()).or_default() += n;
        }
        let holders = || {
            held.iter()
                .map(|(w, n)| format!("{w} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mine = held.get(caller).copied().unwrap_or(0);
        if cfg.per_caller > 0 && mine >= cfg.per_caller {
            return Err(ManagerError::TooManyAgents(format!(
                "Agent create refused: caller {caller} already holds {mine} live agents (cap {} per caller, LUCINA_AGENTD_MAX_AGENTS_PER_CALLER). Slots held: {}. Close, archive or delete some first.",
                cfg.per_caller,
                holders()
            )));
        }
        let in_flight = st.reserved.get(caller).copied().unwrap_or(0);
        let window = st.windows.entry(caller.to_string()).or_default();
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            window.pop_front();
        }
        if cfg.per_minute > 0 && window.len() + in_flight >= cfg.per_minute {
            return Err(ManagerError::TooManyAgents(format!(
                "Agent create refused: caller {caller} made {} creates in the last {} s (cap {} per window, LUCINA_AGENTD_MAX_CREATES_PER_MINUTE). Slots held: {}. Wait and retry.",
                window.len() + in_flight,
                WINDOW.as_secs(),
                cfg.per_minute,
                holders()
            )));
        }
        let total: usize = held.values().sum();
        if cfg.host > 0 && total >= cfg.host {
            return Err(ManagerError::TooManyAgents(format!(
                "Agent create refused: the host already runs {total} live agents (cap {}, LUCINA_AGENTD_MAX_AGENTS_HOST). Slots held: {}. Close, archive or delete some first.",
                cfg.host,
                holders()
            )));
        }
        *st.reserved.entry(caller.to_string()).or_default() += 1;
        Ok(Slot {
            inner: self.inner.clone(),
            caller: caller.to_string(),
            held: true,
        })
    }

    /// How many live spawned agents count against the caps now (tests, diagnostics).
    pub fn capped_agents(&self) -> usize {
        self.lock().values().filter(|a| counts(a)).count()
    }
}

/// The error and the last system error item of a failed run.
fn failure_text(a: &Agent) -> String {
    let last_system = a.rows.iter().rev().find_map(|r| match &r.item {
        TimelineItem::AssistantMessage { text, .. }
            if text.starts_with(lucina_agents::events::SYSTEM_ERROR_PREFIX) =>
        {
            Some(text.clone())
        }
        _ => None,
    });
    format!(
        "{}\n{}",
        a.record.last_error.clone().unwrap_or_default(),
        last_system.unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn reset_times_in_the_forms_the_providers_print() {
        let now = at("2026-11-03T09:00:00Z");
        assert_eq!(
            parse_reset(
                "You've hit your weekly limit · resets Nov 4, 12pm (UTC)",
                now
            ),
            Some(at("2026-11-04T12:00:00Z"))
        );
        assert_eq!(
            parse_reset("quota; resets_at=2026-11-03T10:30:00.500Z", now),
            Some(at("2026-11-03T10:30:00.500Z"))
        );
        assert_eq!(parse_reset("rate limited, try later", now), None);
        // A date already past this year means next year's.
        assert_eq!(
            parse_reset("resets Jan 2, 1am (UTC)", now),
            Some(at("2027-01-02T01:00:00Z"))
        );
    }

    #[test]
    fn auth_failures_are_recognised() {
        assert!(is_auth_failure(
            "Failed to authenticate: OAuth session expired and could not be refreshed"
        ));
        assert!(!is_auth_failure("You've hit your weekly limit"));
    }

    #[test]
    fn the_breaker_keeps_its_state_and_clears_at_the_reset_time() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("provider-breaker.json");
        let b = Breaker::open(Some(file.clone()));
        let now = at("2026-11-03T09:00:00Z");
        let trip = |until: Option<&str>| Trip {
            reason: "quota".into(),
            detail: "limit".into(),
            since: "2026-11-03T08:00:00.000Z".into(),
            until: until.map(str::to_string),
        };
        assert!(b.trip("claude", trip(Some("2026-11-03T09:00:02.000Z"))));
        assert!(
            !b.trip("claude", trip(Some("2026-11-03T09:00:02.000Z"))),
            "one trip, one log line"
        );
        let again = Breaker::open(Some(file));
        assert!(
            again.check("claude", now).is_some(),
            "a restart keeps the breaker"
        );
        assert!(again.check("other", now).is_none());
        assert!(again.check("claude", at("2026-11-03T09:00:02Z")).is_none());
        assert!(again.list().is_empty());
        assert!(!again.clear("claude"));
    }
}
