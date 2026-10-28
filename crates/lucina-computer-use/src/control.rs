//! Who drives each sandbox: Lucina (her engine and her tools), an outside
//! agent through the MCP, or the owner watching live in the apps.
//!
//! The owner's takeover is a lease: [`Control::take`] stops Lucina's running
//! task, refuses every mutating call from Lucina or an agent until the owner
//! gives the desktop back ([`Control::release`]) or stops renewing the lease
//! ([`Control::heartbeat`], [`OWNER_LEASE`]). Every change of driver bumps the
//! sandbox's `epoch`, so a control-mode live view minted under an earlier
//! epoch is dead (the view proxy checks it on every request and closes its
//! socket).
//!
//! Every takeover, give-back, expiry, stopped task and owner input batch is
//! appended to a JSONL audit log (`LUCINA_COMPUTER_AUDIT_LOG`) and kept in a
//! small ring that the apps show. Typed text is never logged, only its length.
//!
//! One process-wide [`control()`] and [`shared_client()`]: the busy guard of
//! [`Client`] only holds when every caller shares the same instance.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::{Cancel, Client, ComputerResult, ComputerUseConfig, Error};

/// How long a takeover lasts without a heartbeat.
pub const OWNER_LEASE: Duration = Duration::from_secs(45);
/// How long the owner may drive without real input (keys, mouse, touch) before
/// the desktop goes back to Lucina by itself. Heartbeats alone never renew past it.
pub const OWNER_IDLE: Duration = Duration::from_secs(120);
/// An agent call this recent still shows the agent as driving.
const AGENT_RECENT: Duration = Duration::from_secs(15);
/// Audit entries kept in memory for the apps.
const RING: usize = 100;
/// Default audit log.
pub const DEFAULT_AUDIT_LOG: &str = "/home/nez/Projects/Lucina/data/computer-audit.jsonl";

/// One of Lucina's two sandboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SandboxId {
    Workspace,
    Android,
}

impl SandboxId {
    pub const ALL: [SandboxId; 2] = [SandboxId::Workspace, SandboxId::Android];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Android => "android",
        }
    }

    /// The names the API accepts (`workspace`/`linux`/`desktop`, `android`/`phone`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "workspace" | "linux" | "desktop" => Some(Self::Workspace),
            "android" | "phone" => Some(Self::Android),
            _ => None,
        }
    }

    /// The Cua space id.
    pub fn space(self) -> &'static str {
        match self {
            Self::Workspace => crate::WORKSPACE_SPACE,
            Self::Android => crate::ANDROID_SPACE,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Workspace => 0,
            Self::Android => 1,
        }
    }
}

#[derive(Debug)]
struct Lease {
    device: String,
    since_ms: u64,
    expires: Instant,
    /// The owner's last real input (or the takeover itself).
    active: Instant,
}

#[derive(Debug)]
struct Work {
    id: u64,
    who: String,
    what: String,
    since_ms: u64,
    cancel: Cancel,
    preempted: bool,
}

#[derive(Debug, Default)]
struct Slot {
    owner: Option<Lease>,
    epoch: u64,
    task: Option<Work>,
    last_agent: Option<(String, Instant, u64)>,
}

#[derive(Debug, Default)]
struct Inner {
    slots: [Slot; 2],
    next_task: u64,
    ring: VecDeque<Value>,
}

/// The driver state of both sandboxes.
#[derive(Debug)]
pub struct Control {
    inner: Mutex<Inner>,
    lease: Duration,
    idle: Duration,
    audit_log: Option<PathBuf>,
}

/// What [`Control::take`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Takeover {
    pub epoch: u64,
    /// The task that was stopped, as `who: what`.
    pub stopped_task: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clip(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        return t.to_owned();
    }
    t.chars().take(n).collect::<String>() + "…"
}

impl Control {
    pub fn new(lease: Duration, audit_log: Option<PathBuf>) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            lease,
            idle: OWNER_IDLE,
            audit_log,
        }
    }

    /// A shorter idle limit (tests).
    pub fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, inner: &mut Inner, sb: SandboxId, event: &str, mut fields: Value) {
        let obj = fields
            .as_object_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut entry = serde_json::Map::new();
        entry.insert("at".into(), json!(now_ms()));
        entry.insert("sandbox".into(), json!(sb.as_str()));
        entry.insert("event".into(), json!(event));
        entry.extend(obj);
        let entry = Value::Object(entry);
        tracing::info!(target: "computer_audit", sandbox = sb.as_str(), event, entry = %entry, "computer control");
        if let Some(path) = &self.audit_log {
            let line = format!("{entry}\n");
            let res = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(line.as_bytes()));
            if let Err(e) = res {
                tracing::warn!(error = %e, path = %path.display(), "computer audit log not written");
            }
        }
        inner.ring.push_back(entry);
        while inner.ring.len() > RING {
            inner.ring.pop_front();
        }
    }

    /// Drops an expired lease (and bumps the epoch, so control views die).
    fn expire(&self, inner: &mut Inner, sb: SandboxId) {
        let slot = &mut inner.slots[sb.index()];
        let expired = slot
            .owner
            .as_ref()
            .is_some_and(|l| l.expires <= Instant::now());
        if expired {
            let lease = slot.owner.take().expect("checked above");
            slot.epoch += 1;
            self.record(inner, sb, "expired", json!({"device": lease.device}));
        }
    }

    /// The owner holds this sandbox.
    pub fn owner_driving(&self, sb: SandboxId) -> bool {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        inner.slots[sb.index()].owner.is_some()
    }

    /// The current epoch (bumped by every takeover, give-back and expiry).
    pub fn epoch(&self, sb: SandboxId) -> u64 {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        inner.slots[sb.index()].epoch
    }

    /// A direct mutating call by Lucina or an MCP agent (`who`). Refused while
    /// the owner drives; otherwise recorded as recent agent activity.
    pub fn agent_call(&self, sb: SandboxId, who: &str) -> Result<(), Error> {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let slot = &mut inner.slots[sb.index()];
        if slot.owner.is_some() {
            return Err(Error::OwnerDriving);
        }
        slot.last_agent = Some((who.to_owned(), Instant::now(), now_ms()));
        Ok(())
    }

    /// Registers a whole engine task. Refused while the owner drives or while
    /// another task runs. The guard unregisters it when dropped.
    pub fn begin_task(
        &self,
        sb: SandboxId,
        who: &str,
        what: &str,
        cancel: Cancel,
    ) -> Result<TaskGuard<'_>, Error> {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        inner.next_task += 1;
        let id = inner.next_task;
        let slot = &mut inner.slots[sb.index()];
        if slot.owner.is_some() {
            return Err(Error::OwnerDriving);
        }
        if slot.task.is_some() {
            return Err(Error::Busy);
        }
        slot.task = Some(Work {
            id,
            who: who.to_owned(),
            what: clip(what, 160),
            since_ms: now_ms(),
            cancel,
            preempted: false,
        });
        Ok(TaskGuard {
            control: self,
            sb,
            id,
        })
    }

    /// The owner takes over: Lucina's running task on this sandbox is stopped.
    pub fn take(&self, sb: SandboxId, device: &str) -> Takeover {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let lease = self.lease;
        let slot = &mut inner.slots[sb.index()];
        let renewed = slot.owner.is_some();
        let since_ms = slot
            .owner
            .as_ref()
            .map(|l| l.since_ms)
            .unwrap_or_else(now_ms);
        let now = Instant::now();
        slot.owner = Some(Lease {
            device: device.to_owned(),
            since_ms,
            expires: now + lease,
            active: now,
        });
        if !renewed {
            slot.epoch += 1;
        }
        let epoch = slot.epoch;
        let stopped_task = slot.task.as_mut().filter(|w| !w.preempted).map(|w| {
            w.preempted = true;
            w.cancel.cancel();
            format!("{}: {}", w.who, w.what)
        });
        if !renewed {
            self.record(
                &mut inner,
                sb,
                "take",
                json!({"device": device, "stoppedTask": stopped_task}),
            );
        }
        Takeover {
            epoch,
            stopped_task,
        }
    }

    /// Renews the owner's lease, counting it as input (an older client that does
    /// not say whether the owner was active); `Err(NotDriving)` when it already ended.
    pub fn heartbeat(&self, sb: SandboxId) -> Result<u64, Error> {
        self.heartbeat_idle(sb, true)
    }

    /// Renews the owner's lease. `active`: there was real input (keys, mouse,
    /// touch) since the last heartbeat. Without any for [`OWNER_IDLE`] the
    /// desktop goes back to Lucina (`idle` in the audit, `Err(OwnerIdle)`):
    /// an open page or socket alone never keeps it.
    pub fn heartbeat_idle(&self, sb: SandboxId, active: bool) -> Result<u64, Error> {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let (lease, idle) = (self.lease, self.idle);
        let now = Instant::now();
        let slot = &mut inner.slots[sb.index()];
        let Some(l) = slot.owner.as_mut() else {
            return Err(Error::NotDriving);
        };
        if active {
            l.active = now;
        }
        let idle_for = now.duration_since(l.active);
        if idle_for < idle {
            l.expires = now + lease;
            return Ok(slot.epoch);
        }
        let device = slot.owner.take().map(|l| l.device).unwrap_or_default();
        slot.epoch += 1;
        self.record(
            &mut inner,
            sb,
            "idle",
            json!({"device": device, "idleMs": idle_for.as_millis() as u64}),
        );
        Err(Error::OwnerIdle)
    }

    /// Real owner input (keys, mouse, touch): renews the lease and the idle clock.
    pub fn active(&self, sb: SandboxId) -> Result<(), Error> {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let lease = self.lease;
        let now = Instant::now();
        match inner.slots[sb.index()].owner.as_mut() {
            Some(l) => {
                l.active = now;
                l.expires = now + lease;
                Ok(())
            }
            None => Err(Error::NotDriving),
        }
    }

    /// The owner gives the sandbox back. Idempotent.
    pub fn release(&self, sb: SandboxId, device: &str) {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let slot = &mut inner.slots[sb.index()];
        if let Some(lease) = slot.owner.take() {
            slot.epoch += 1;
            let held_ms = now_ms().saturating_sub(lease.since_ms);
            self.record(
                &mut inner,
                sb,
                "release",
                json!({"device": device, "heldMs": held_ms}),
            );
        }
    }

    /// An owner input batch (live view socket traffic or fallback events).
    /// Needs the owner's lease.
    pub fn owner_input(&self, sb: SandboxId, device: &str, summary: Value) -> Result<(), Error> {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        if inner.slots[sb.index()].owner.is_none() {
            return Err(Error::NotDriving);
        }
        self.record(
            &mut inner,
            sb,
            "input",
            json!({"device": device, "batch": summary}),
        );
        Ok(())
    }

    /// Records a live-view event that is not input (a view opened, closed or refused).
    pub fn note(&self, sb: SandboxId, event: &str, fields: Value) {
        let mut inner = self.lock();
        self.record(&mut inner, sb, event, fields);
    }

    /// What the apps show: who drives, the owner's lease, Lucina's task.
    pub fn state(&self, sb: SandboxId) -> Value {
        let mut inner = self.lock();
        self.expire(&mut inner, sb);
        let slot = &inner.slots[sb.index()];
        let now = Instant::now();
        let recent_agent = slot
            .last_agent
            .as_ref()
            .filter(|(_, at, _)| now.duration_since(*at) < AGENT_RECENT);
        let driver = if slot.owner.is_some() {
            "owner"
        } else if slot.task.is_some() {
            "lucina"
        } else if recent_agent.is_some() {
            "agent"
        } else {
            "none"
        };
        json!({
            "sandbox": sb.as_str(),
            "driver": driver,
            "epoch": slot.epoch,
            "owner": slot.owner.as_ref().map(|l| json!({
                "device": l.device,
                "since": l.since_ms,
                "expiresInMs": l.expires.saturating_duration_since(now).as_millis() as u64,
            })),
            "task": slot.task.as_ref().map(|w| json!({
                "who": w.who,
                "what": w.what,
                "since": w.since_ms,
                "stopping": w.preempted,
            })),
            "lastAgent": slot.last_agent.as_ref().map(|(who, _, at)| json!({"who": who, "at": at})),
            "leaseSecs": self.lease.as_secs(),
        })
    }

    /// The newest audit entries for this sandbox, newest first.
    pub fn recent(&self, sb: SandboxId, n: usize) -> Vec<Value> {
        let inner = self.lock();
        inner
            .ring
            .iter()
            .rev()
            .filter(|e| e.get("sandbox").and_then(Value::as_str) == Some(sb.as_str()))
            .take(n)
            .cloned()
            .collect()
    }
}

/// A registered engine task; unregisters on drop.
#[derive(Debug)]
pub struct TaskGuard<'a> {
    control: &'a Control,
    sb: SandboxId,
    id: u64,
}

impl TaskGuard<'_> {
    /// The owner took over while this task ran.
    pub fn preempted(&self) -> bool {
        let inner = self.control.lock();
        inner.slots[self.sb.index()]
            .task
            .as_ref()
            .is_some_and(|w| w.id == self.id && w.preempted)
    }
}

impl Drop for TaskGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.control.lock();
        let slot = &mut inner.slots[self.sb.index()];
        if slot.task.as_ref().is_some_and(|w| w.id == self.id) {
            slot.task = None;
        }
    }
}

/// The process-wide driver state.
pub fn control() -> &'static Control {
    static C: OnceLock<Control> = OnceLock::new();
    C.get_or_init(|| {
        // Test binaries (this crate's and its dependents') never write the real log.
        if std::env::var_os("RUST_TEST_THREADS").is_some() || cfg!(test) || running_under_test() {
            return Control::new(OWNER_LEASE, None);
        }
        let path = std::env::var("LUCINA_COMPUTER_AUDIT_LOG")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_AUDIT_LOG.to_owned());
        let audit = (path != "off").then(|| PathBuf::from(path));
        Control::new(OWNER_LEASE, audit)
    })
}

/// `cargo test` binaries live in `target/*/deps/` and are named `<crate>-<hash>`.
fn running_under_test() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.ends_with("deps")))
        .unwrap_or(false)
}

/// The process-wide engine client (config from the environment at first use).
pub fn shared_client() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| {
        Client::new(ComputerUseConfig::from_lookup(&|k: &str| {
            std::env::var(k).ok()
        }))
    })
}

/// Runs one engine task on the workspace for `who` (`lucina`, `mcp:claude`, …)
/// under the driver lock. A task the owner interrupted fails with
/// [`Error::OwnerDriving`].
///
/// The Jev engine ([`crate::jev_drive`]) tries first when the task is browsing
/// or the Android phone; whatever it cannot finish goes to Hermes, told what
/// was already done.
pub async fn run_task(who: &str, prompt: &str, cancel: &Cancel) -> Result<ComputerResult, Error> {
    let client = shared_client();
    if !client.ready() {
        return Err(Error::NotEnabled);
    }
    let mut prompt = std::borrow::Cow::Borrowed(prompt);
    if let Some(r) = jev_first(&prompt, cancel).await? {
        match r {
            Ok(done) => return Ok(done),
            Err(handover) => prompt = std::borrow::Cow::Owned(handover),
        }
    }
    let guard = control().begin_task(SandboxId::Workspace, who, &prompt, cancel.clone())?;
    let out = client.run(&prompt, cancel).await;
    match out {
        Err(_) if guard.preempted() => Err(Error::OwnerDriving),
        other => other,
    }
}

/// The Jev engine's try: `None` when it does not apply (off, no quota, not a
/// browsing or phone task), `Some(Ok(result))` when it finished, and
/// `Some(Err(prompt))` with the task for Hermes, told what was already done.
async fn jev_first(
    prompt: &str,
    cancel: &Cancel,
) -> Result<Option<Result<ComputerResult, String>>, Error> {
    use crate::jev_drive::{self, Outcome, Where};
    let jev = lucina_jev::shared();
    if !jev_drive::enabled() || !jev.available() {
        return Ok(None);
    }
    let target = match jev_drive::route(jev, prompt).await {
        Some(t @ (Where::Desktop | Where::Android)) => t,
        _ => return Ok(None),
    };
    match jev_drive::drive(jev, target, prompt, cancel).await {
        Ok(Outcome::Done { reply, actions }) => {
            tracing::info!(target: "lucina_jev::drive", ?target, steps = actions.len(), "jev engine finished the task");
            Ok(Some(Ok(ComputerResult {
                status: crate::Status::Completed,
                reply,
                session_id: None,
                executed_tools: actions.iter().map(|a| format!("jev: {a}")).collect(),
                tool_errors: 0,
            })))
        }
        Ok(Outcome::Escalate {
            reason,
            seen,
            actions,
        }) => {
            tracing::info!(target: "lucina_jev::drive", ?target, %reason, steps = actions.len(), "jev engine hands the task to Hermes");
            if actions.is_empty() {
                return Ok(Some(Err(prompt.to_owned())));
            }
            Ok(Some(Err(format!(
                "{prompt}\n\n(A quick first attempt already did: {}. It stopped on \"{seen}\" ({reason}). Continue from the current screen; do not repeat those steps.)",
                actions.join("; ")
            ))))
        }
        Err(e @ (Error::OwnerDriving | Error::Cancelled | Error::Busy)) => Err(e),
        Err(e) => {
            tracing::warn!(target: "lucina_jev::drive", error = %e, "jev engine failed; Hermes takes the task");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctl() -> Control {
        Control::new(Duration::from_millis(200), None)
    }

    #[test]
    fn takeover_refuses_agents_and_stops_the_task() {
        let c = ctl();
        let cancel = Cancel::new();
        let guard = c
            .begin_task(
                SandboxId::Workspace,
                "lucina",
                "open example.com",
                cancel.clone(),
            )
            .unwrap();
        assert_eq!(c.state(SandboxId::Workspace)["driver"], "lucina");
        let t = c.take(SandboxId::Workspace, "iphone");
        assert_eq!(t.stopped_task.as_deref(), Some("lucina: open example.com"));
        assert!(cancel.is_cancelled());
        assert!(guard.preempted());
        assert_eq!(
            c.agent_call(SandboxId::Workspace, "mcp:claude"),
            Err(Error::OwnerDriving)
        );
        assert!(matches!(
            c.begin_task(SandboxId::Workspace, "lucina", "x", Cancel::new()),
            Err(Error::OwnerDriving)
        ));
        // The other sandbox is unaffected.
        assert!(c.agent_call(SandboxId::Android, "mcp:claude").is_ok());
        assert_eq!(c.state(SandboxId::Workspace)["driver"], "owner");
        drop(guard);
        c.release(SandboxId::Workspace, "iphone");
        assert!(c.agent_call(SandboxId::Workspace, "mcp:claude").is_ok());
        assert_eq!(c.state(SandboxId::Workspace)["driver"], "agent");
    }

    #[test]
    fn epoch_moves_on_take_release_and_expiry_but_not_on_renewal() {
        let c = ctl();
        let e0 = c.epoch(SandboxId::Android);
        let t = c.take(SandboxId::Android, "mac");
        assert_eq!(t.epoch, e0 + 1);
        assert_eq!(
            c.take(SandboxId::Android, "mac").epoch,
            e0 + 1,
            "a second take renews"
        );
        assert_eq!(c.heartbeat(SandboxId::Android), Ok(e0 + 1));
        std::thread::sleep(Duration::from_millis(250));
        assert!(!c.owner_driving(SandboxId::Android));
        assert_eq!(c.epoch(SandboxId::Android), e0 + 2);
        assert_eq!(c.heartbeat(SandboxId::Android), Err(Error::NotDriving));
        let recent = c.recent(SandboxId::Android, 10);
        assert_eq!(recent[0]["event"], "expired");
        assert_eq!(recent[1]["event"], "take");
    }

    #[test]
    fn an_idle_owner_gives_the_desktop_back_however_often_the_page_beats() {
        let c = Control::new(Duration::from_secs(5), None).with_idle(Duration::from_millis(120));
        let sb = SandboxId::Workspace;
        let e0 = c.epoch(sb);
        c.take(sb, "mac");
        // An open page or socket alone (heartbeats without input) keeps it only until idle.
        assert_eq!(c.heartbeat_idle(sb, false), Ok(e0 + 1));
        std::thread::sleep(Duration::from_millis(70));
        assert_eq!(c.heartbeat_idle(sb, false), Ok(e0 + 1));
        std::thread::sleep(Duration::from_millis(70));
        assert_eq!(c.heartbeat_idle(sb, false), Err(Error::OwnerIdle));
        assert!(!c.owner_driving(sb), "Lucina has it again");
        assert_eq!(c.epoch(sb), e0 + 2, "control views die with it");
        let recent = c.recent(sb, 10);
        assert_eq!(recent[0]["event"], "idle");
        assert_eq!(recent[0]["device"], "mac");
        assert!(recent[0]["idleMs"].as_u64().unwrap() >= 120);
        assert_eq!(c.heartbeat_idle(sb, false), Err(Error::NotDriving));
        assert_eq!(c.active(sb), Err(Error::NotDriving));
    }

    #[test]
    fn real_input_keeps_the_owner_and_old_heartbeats_count_as_input() {
        let c = Control::new(Duration::from_secs(5), None).with_idle(Duration::from_millis(120));
        let sb = SandboxId::Android;
        c.take(sb, "mac");
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(50));
            c.active(sb).unwrap(); // keys, mouse, touch
            assert!(c.heartbeat_idle(sb, false).is_ok());
        }
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                c.heartbeat_idle(sb, true).is_ok(),
                "a page that saw input says so"
            );
        }
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                c.heartbeat(sb).is_ok(),
                "an app that does not say keeps today's behaviour"
            );
        }
        assert!(c.owner_driving(sb));
        c.release(sb, "mac");
    }

    #[test]
    fn input_needs_the_lease_and_one_task_at_a_time() {
        let c = ctl();
        assert_eq!(
            c.owner_input(SandboxId::Workspace, "mac", json!({"events": 1})),
            Err(Error::NotDriving)
        );
        let g = c
            .begin_task(SandboxId::Workspace, "lucina", "a", Cancel::new())
            .unwrap();
        assert!(matches!(
            c.begin_task(SandboxId::Workspace, "mcp:grok", "b", Cancel::new()),
            Err(Error::Busy)
        ));
        drop(g);
        let _g = c
            .begin_task(SandboxId::Workspace, "mcp:grok", "b", Cancel::new())
            .unwrap();
        c.take(SandboxId::Workspace, "mac");
        assert!(c
            .owner_input(SandboxId::Workspace, "mac", json!({"events": 2}))
            .is_ok());
        assert_eq!(c.recent(SandboxId::Workspace, 1)[0]["event"], "input");
    }

    #[test]
    fn audit_log_is_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let c = Control::new(Duration::from_secs(5), Some(path.clone()));
        c.take(SandboxId::Workspace, "mac");
        c.release(SandboxId::Workspace, "mac");
        let text = std::fs::read_to_string(path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "take");
        assert_eq!(lines[1]["event"], "release");
        assert_eq!(lines[1]["device"], "mac");
    }
}
