//! AGENT-01 (2026-11-19): an agent that failed on an expired login kept its provider and
//! MCP processes, and nothing limited how many agents a caller made. Pinned here:
//! - an errored agent's provider process and the processes under it end, the row stays `error`;
//! - an authentication or quota failure trips the provider's breaker, `create` is refused
//!   before anything starts, other providers are untouched, `providers clear` (or a reset time,
//!   or a success) reopens it;
//! - the create caps (per caller live, per caller per minute, host-wide) refuse with 429.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{
    ActiveTurnBehavior, CapsConfig, CreateRequest, Manager, ManagerError, Options,
};
use lucina_agentd::process::{Proc, Spec};
use lucina_agentd::provider::{
    EventSink, Provider, ProviderError, ProviderInfo, Session, SessionConfig,
};
use lucina_agentd::store::Store;
use lucina_agents::types::{Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{AgentStatus, PermissionResponse, StreamEvent};

/// What a fake provider's next turn does: `None` completes, `Some((error, code))` fails.
type Failure = Arc<Mutex<Option<(String, Option<String>)>>>;

#[derive(Clone)]
struct Fake {
    id: &'static str,
    /// Where `provider.pid` and `child.pid` go, when `spawn` is set.
    dir: PathBuf,
    starts: Arc<AtomicUsize>,
    failure: Failure,
    /// Start a real process group: a shell and a `sleep` child.
    spawn: bool,
}

impl Fake {
    fn new(id: &'static str, dir: &Path) -> Fake {
        Fake {
            id,
            dir: dir.to_path_buf(),
            starts: Arc::new(AtomicUsize::new(0)),
            failure: Arc::new(Mutex::new(None)),
            spawn: false,
        }
    }

    fn failing(self, error: &str, code: Option<&str>) -> Fake {
        *self.failure.lock().unwrap() = Some((error.into(), code.map(str::to_string)));
        self
    }

    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for Fake {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.into(),
            label: self.id.into(),
            default_mode_id: None,
            modes: Vec::new(),
            models: Vec::new(),
            capabilities: Capabilities {
                supports_streaming: true,
                ..Capabilities::default()
            },
            echoes_user_message: false,
        }
    }

    async fn create_session(
        &self,
        _config: SessionConfig,
        events: EventSink,
    ) -> Result<Arc<dyn Session>, ProviderError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let proc_ = if self.spawn {
            let child = self.dir.join("child.pid");
            let spec = Spec {
                program: "sh".into(),
                args: vec![
                    "-c".into(),
                    format!("sleep 300 & echo $! > {}; wait", child.display()),
                ],
                cwd: self.dir.display().to_string(),
                ..Spec::default()
            };
            let (p, _stdin, _stdout) =
                Proc::spawn(&spec, None).map_err(|e| ProviderError::new(e.to_string()))?;
            std::fs::write(self.dir.join("provider.pid"), p.pid.to_string()).unwrap();
            Some(p)
        } else {
            None
        };
        Ok(Arc::new(FakeSession {
            fake: self.clone(),
            events,
            proc_: tokio::sync::Mutex::new(proc_),
        }))
    }

    async fn resume_session(
        &self,
        _h: &PersistenceHandle,
        config: SessionConfig,
        events: EventSink,
    ) -> Result<Arc<dyn Session>, ProviderError> {
        self.create_session(config, events).await
    }
}

struct FakeSession {
    fake: Fake,
    events: EventSink,
    proc_: tokio::sync::Mutex<Option<Proc>>,
}

#[async_trait]
impl Session for FakeSession {
    fn session_id(&self) -> String {
        "fake-session".into()
    }

    async fn start_turn(
        &self,
        _prompt: &str,
        _client_message_id: Option<String>,
    ) -> Result<String, ProviderError> {
        let turn = uuid::Uuid::new_v4().to_string();
        let id = self.fake.id.to_string();
        let _ = self.events.send(StreamEvent::TurnStarted {
            provider: id.clone(),
            turn_id: Some(turn.clone()),
        });
        let failure = self.fake.failure.lock().unwrap().clone();
        let _ = self.events.send(match failure {
            Some((error, code)) => StreamEvent::TurnFailed {
                provider: id,
                turn_id: Some(turn.clone()),
                error,
                code,
                diagnostic: None,
            },
            None => StreamEvent::TurnCompleted {
                provider: id,
                turn_id: Some(turn.clone()),
                usage: None,
            },
        });
        Ok(turn)
    }

    async fn steer(&self, _prompt: &str, _turn: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn respond_permission(
        &self,
        _id: &str,
        _r: PermissionResponse,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn close(&self) {
        if let Some(mut p) = self.proc_.lock().await.take() {
            p.shutdown(Duration::from_secs(2)).await;
        }
    }

    fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            provider: self.fake.id.into(),
            session_id: Some("fake-session".into()),
            model: None,
            thinking_option_id: None,
            mode_id: None,
            extra: None,
        }
    }

    fn persistence(&self) -> PersistenceHandle {
        PersistenceHandle {
            provider: self.fake.id.into(),
            session_id: "fake-session".into(),
            native_handle: None,
            metadata: None,
        }
    }

    fn current_mode(&self) -> Option<String> {
        None
    }
}

const AUTH: &str = "Failed to authenticate: OAuth session expired and could not be refreshed";

fn manager(dir: &Path, providers: &[&Fake], caps: CapsConfig) -> Manager {
    let state = dir.join("state");
    let options = Options {
        caps,
        breaker_file: Some(state.join("provider-breaker.json")),
        ..Options::default()
    };
    Manager::open_with(
        Store::open(&state).unwrap(),
        providers
            .iter()
            .map(|f| Arc::new((*f).clone()) as Arc<dyn Provider>)
            .collect(),
        LaneConfig::default(),
        options,
    )
    .unwrap()
}

fn roomy() -> CapsConfig {
    CapsConfig {
        per_caller: 1000,
        per_minute: 1000,
        host: 1000,
    }
}

fn request(provider: &str, cwd: &Path, by: Option<&str>) -> CreateRequest {
    CreateRequest {
        provider: provider.into(),
        cwd: cwd.display().to_string(),
        created_by: by.map(str::to_string),
        ..CreateRequest::default()
    }
}

fn status(m: &Manager, id: &str) -> AgentStatus {
    m.snapshots(true)
        .into_iter()
        .find(|a| a.id == id)
        .expect("agent")
        .status
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

fn pid_in(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn an_errored_agent_releases_its_provider_and_the_processes_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = Fake::new("fake-a", dir.path()).failing(AUTH, None);
    fake.spawn = true;
    let m = manager(dir.path(), &[&fake], roomy());
    let snap = m.create(request("fake-a", dir.path(), None)).await.unwrap();
    until("the child pid file", || {
        dir.path().join("child.pid").exists()
    })
    .await;
    until("the child pid", || {
        std::fs::read_to_string(dir.path().join("child.pid")).is_ok_and(|s| !s.trim().is_empty())
    })
    .await;
    let provider = pid_in(&dir.path().join("provider.pid"));
    let child = pid_in(&dir.path().join("child.pid"));
    assert!(
        alive(provider) && alive(child),
        "both run before the failure"
    );

    m.send(&snap.id, "go", ActiveTurnBehavior::Interrupt)
        .await
        .unwrap();
    until("status error", || {
        status(&m, &snap.id) == AgentStatus::Error
    })
    .await;
    until("both processes gone", || !alive(provider) && !alive(child)).await;
    assert_eq!(
        status(&m, &snap.id),
        AgentStatus::Error,
        "the row stays error"
    );
}

#[tokio::test]
async fn an_auth_failure_trips_its_provider_and_clear_reopens_it() {
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path()).failing(AUTH, None);
    let b = Fake::new("fake-b", dir.path());
    let m = manager(dir.path(), &[&a, &b], roomy());
    let mut req = request("fake-a", dir.path(), None);
    req.prompt = Some("go".into());
    let first = m.create(req).await.unwrap();
    until("status error", || {
        status(&m, &first.id) == AgentStatus::Error
    })
    .await;
    until("breaker tripped", || {
        m.tripped_providers().contains_key("fake-a")
    })
    .await;
    assert_eq!(a.starts(), 1);

    let refused = m
        .create(request("fake-a", dir.path(), None))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ManagerError::ProviderUnavailable(_)),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("fake-a") && text.contains("authentication"),
        "{text}"
    );
    assert!(text.contains("providers clear fake-a"), "{text}");
    assert_eq!(a.starts(), 1, "nothing was spawned for the refused create");

    m.create(request("fake-b", dir.path(), None))
        .await
        .expect("another provider is untouched");

    // The state file survives a restart.
    let again = manager(dir.path(), &[&a, &b], roomy());
    assert!(again.tripped_providers().contains_key("fake-a"));

    assert!(m.clear_provider("fake-a"));
    m.create(request("fake-a", dir.path(), None))
        .await
        .expect("cleared");
    assert_eq!(a.starts(), 2);
}

#[tokio::test]
async fn a_quota_failure_with_a_reset_time_refuses_until_that_time() {
    let dir = tempfile::tempdir().unwrap();
    let reset = chrono::Utc::now() + chrono::Duration::seconds(2);
    let error = format!(
        "You've hit your usage limit · resets_at={}",
        reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    let a = Fake::new("fake-a", dir.path()).failing(&error, Some("quota"));
    let m = manager(dir.path(), &[&a], roomy());
    let mut req = request("fake-a", dir.path(), None);
    req.prompt = Some("go".into());
    let first = m.create(req).await.unwrap();
    until("status error", || {
        status(&m, &first.id) == AgentStatus::Error
    })
    .await;
    until("breaker tripped", || {
        m.tripped_providers().contains_key("fake-a")
    })
    .await;
    let refused = m
        .create(request("fake-a", dir.path(), None))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ManagerError::ProviderUnavailable(_)),
        "{refused}"
    );
    assert!(refused.to_string().contains("quota"), "{refused}");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    m.create(request("fake-a", dir.path(), None))
        .await
        .expect("allowed after the reset time");
}

#[tokio::test]
async fn a_success_clears_a_tripped_provider() {
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path()).failing(AUTH, None);
    let m = manager(dir.path(), &[&a], roomy());
    let mut req = request("fake-a", dir.path(), None);
    req.prompt = Some("go".into());
    let first = m.create(req).await.unwrap();
    until("tripped", || m.tripped_providers().contains_key("fake-a")).await;
    // The login is fixed: the same agent's next turn works and clears the breaker.
    *a.failure.lock().unwrap() = None;
    until("status error", || {
        status(&m, &first.id) == AgentStatus::Error
    })
    .await;
    m.send(&first.id, "again", ActiveTurnBehavior::Interrupt)
        .await
        .unwrap();
    until("cleared", || m.tripped_providers().is_empty()).await;
}

#[tokio::test]
async fn the_per_caller_cap_refuses_a_fifth_live_agent_and_closed_ones_do_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path());
    let caps = CapsConfig {
        per_caller: 4,
        per_minute: 1000,
        host: 1000,
    };
    let m = manager(dir.path(), &[&a], caps);
    let mut ids = Vec::new();
    for _ in 0..4 {
        ids.push(
            m.create(request("fake-a", dir.path(), Some("local")))
                .await
                .unwrap()
                .id,
        );
    }
    let refused = m
        .create(request("fake-a", dir.path(), Some("local")))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ManagerError::TooManyAgents(_)),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(text.contains("cap 4") && text.contains("local 4"), "{text}");
    assert_eq!(a.starts(), 4, "the refused create started nothing");
    // Another caller has its own count.
    m.create(request("fake-a", dir.path(), Some("agent:x")))
        .await
        .unwrap();
    // Archived agents do not count.
    m.archive(&ids[0]).await.unwrap();
    m.create(request("fake-a", dir.path(), Some("local")))
        .await
        .expect("a slot is free again");
}

#[tokio::test]
async fn the_host_cap_refuses_a_thirteenth_live_agent() {
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path());
    let m = manager(dir.path(), &[&a], CapsConfig::default());
    for caller in ["c1", "c2", "c3"] {
        for _ in 0..4 {
            m.create(request("fake-a", dir.path(), Some(caller)))
                .await
                .unwrap();
        }
    }
    assert_eq!(m.capped_agents(), 12);
    let refused = m
        .create(request("fake-a", dir.path(), Some("c4")))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ManagerError::TooManyAgents(_)),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("host") && text.contains("12") && text.contains("c1 4"),
        "{text}"
    );
}

#[tokio::test]
async fn the_per_minute_cap_refuses_a_fifth_create_even_when_the_others_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path());
    let m = manager(dir.path(), &[&a], CapsConfig::default());
    for _ in 0..4 {
        let id = m
            .create(request("fake-a", dir.path(), Some("local")))
            .await
            .unwrap()
            .id;
        m.archive(&id).await.unwrap();
    }
    assert_eq!(m.capped_agents(), 0);
    let refused = m
        .create(request("fake-a", dir.path(), Some("local")))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ManagerError::TooManyAgents(_)),
        "{refused}"
    );
    assert!(refused.to_string().contains("per window"), "{refused}");
}

#[tokio::test]
async fn agents_the_caps_do_not_count_are_not_refused() {
    // A create that did not come through the route (no `created_by`: a hand-off, a
    // schedule, a helper) is neither counted nor refused.
    let dir = tempfile::tempdir().unwrap();
    let a = Fake::new("fake-a", dir.path());
    let caps = CapsConfig {
        per_caller: 1,
        per_minute: 1,
        host: 1,
    };
    let m = manager(dir.path(), &[&a], caps);
    for _ in 0..3 {
        m.create(request("fake-a", dir.path(), None)).await.unwrap();
    }
}

fn git_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(d.path())
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());
    d
}

fn post_create(socket: &Path, cwd: &Path, provider: &str) -> String {
    let body = format!(r#"{{"provider":"{provider}","cwd":"{}"}}"#, cwd.display());
    let out = common::run(std::process::Command::new("curl").args([
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "--unix-socket",
        &socket.display().to_string(),
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-d",
        &body,
        "http://x/agents",
    ]));
    out.stdout
}

#[test]
fn a_dispatcher_burst_of_29_creates_gets_4_agents_and_25_refusals() {
    let d = common::Daemon::start();
    let work = git_dir();
    let socket = d.socket();
    let cwd = work.path().to_path_buf();
    let handles: Vec<_> = (0..29)
        .map(|_| {
            let (socket, cwd) = (socket.clone(), cwd.clone());
            std::thread::spawn(move || post_create(&socket, &cwd, "mock"))
        })
        .collect();
    let codes: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = codes.iter().filter(|c| *c == "200").count();
    let limited = codes.iter().filter(|c| *c == "429").count();
    assert_eq!((ok, limited), (4, 25), "{codes:?}");
}

#[test]
fn providers_clear_reopens_a_provider_after_a_login_failure_through_the_daemon() {
    let d = common::Daemon::start();
    let work = git_dir();
    let cwd = work.path().display().to_string();
    let first = d.lagent(&[
        "run",
        "--provider",
        "mock",
        "--cwd",
        &cwd,
        "emit synthetic auth failure",
    ]);
    assert!(first.stdout.contains("error"), "{}", first.stdout);
    // The breaker is tripped once the turn failed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let refused = loop {
        let r = d.lagent(&["run", "--provider", "mock", "--cwd", &cwd, "hello"]);
        if r.code != 0 && r.stderr.contains("providers clear mock") {
            break r;
        }
        assert!(
            Instant::now() < deadline,
            "never refused: {} {}",
            r.stdout,
            r.stderr
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(refused.stderr.contains("mock"), "{}", refused.stderr);
    // Creates only (a `run` waits for the turn): another provider is untouched.
    assert_eq!(post_create(&d.socket(), work.path(), "mock-alt"), "200");
    let clear = d.lagent(&["providers", "clear", "mock"]);
    assert_eq!(clear.code, 0, "{}", clear.stderr);
    assert_eq!(post_create(&d.socket(), work.path(), "mock"), "200");
}
