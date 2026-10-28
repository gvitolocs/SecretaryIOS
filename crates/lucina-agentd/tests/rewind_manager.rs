//! Rewind in process (parity row A16): a real `Manager` with a probe provider whose
//! capability flags, rewind behaviour and turns the test sets, to pin down what the
//! manager does around the provider: which modes each flag opens, that a refused
//! rewind leaves a running turn alone, what the provider is asked, and that a rewind
//! holds the agent against new prompts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{ActiveTurnBehavior, Caller, CreateRequest, Manager, ManagerError, RewindRequest};
use lucina_agentd::provider::{EventSink, Provider, ProviderError, ProviderInfo, Rewind, Session, SessionConfig};
use lucina_agentd::store::Store;
use lucina_agents::rewind::{unsupported_message, RewindMode};
use lucina_agents::types::{Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{AgentStatus, PermissionResponse, StreamEvent, TimelineItem};

type Calls = Arc<Mutex<Vec<(RewindMode, String, usize)>>>;

#[derive(Clone)]
struct Probe {
    id: &'static str,
    caps: Capabilities,
    /// Whether its sessions implement `rewind` (else the trait's default answers).
    implements: bool,
    delay: Duration,
    fail: Option<&'static str>,
    calls: Calls,
}

impl Probe {
    fn new(id: &'static str, modes: &[RewindMode]) -> Probe {
        let has = |m| modes.contains(&m);
        Probe {
            id,
            caps: Capabilities {
                supports_streaming: true,
                supports_rewind_conversation: has(RewindMode::Conversation),
                supports_rewind_files: has(RewindMode::Files),
                supports_rewind_both: has(RewindMode::Both),
                ..Capabilities::default()
            },
            implements: true,
            delay: Duration::ZERO,
            fail: None,
            calls: Calls::default(),
        }
    }
}

#[async_trait]
impl Provider for Probe {
    fn info(&self) -> ProviderInfo {
        ProviderInfo { id: self.id.into(), label: "Probe".into(), default_mode_id: None, modes: Vec::new(), models: Vec::new(), capabilities: self.caps.clone(), echoes_user_message: false }
    }

    async fn create_session(&self, _config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        Ok(Arc::new(ProbeSession { probe: self.clone(), events, running: Mutex::new(None), interrupts: Mutex::new(0) }))
    }

    async fn resume_session(&self, _h: &PersistenceHandle, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.create_session(config, events).await
    }
}

struct ProbeSession {
    probe: Probe,
    events: EventSink,
    running: Mutex<Option<String>>,
    interrupts: Mutex<u32>,
}

#[async_trait]
impl Session for ProbeSession {
    fn session_id(&self) -> String {
        "probe-session".into()
    }

    /// "long" never ends by itself; anything else answers and completes.
    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        let turn = uuid::Uuid::new_v4().to_string();
        let _ = self.events.send(StreamEvent::TurnStarted { provider: self.probe.id.into(), turn_id: Some(turn.clone()) });
        if prompt.contains("long") {
            *self.running.lock().unwrap() = Some(turn.clone());
        } else {
            let _ = self.events.send(StreamEvent::Timeline {
                provider: self.probe.id.into(),
                item: TimelineItem::AssistantMessage { text: format!("re: {prompt}"), message_id: Some(format!("a-{turn}")) },
                turn_id: Some(turn.clone()),
            });
            let _ = self.events.send(StreamEvent::TurnCompleted { provider: self.probe.id.into(), turn_id: Some(turn.clone()), usage: None });
        }
        Ok(turn)
    }

    async fn steer(&self, _prompt: &str, _turn: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        if let Some(turn) = self.running.lock().unwrap().take() {
            *self.interrupts.lock().unwrap() += 1;
            let _ = self.events.send(StreamEvent::TurnCanceled { provider: self.probe.id.into(), turn_id: Some(turn), reason: "Interrupted".into() });
        }
        Ok(())
    }

    async fn respond_permission(&self, _id: &str, _r: PermissionResponse) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn rewind(&self, rewind: &Rewind) -> Result<(), ProviderError> {
        if !self.probe.implements {
            return Err(ProviderError::new(unsupported_message(rewind.mode)));
        }
        tokio::time::sleep(self.probe.delay).await;
        if let Some(e) = self.probe.fail {
            return Err(ProviderError::new(e));
        }
        self.probe.calls.lock().unwrap().push((rewind.mode, rewind.message_id.clone(), rewind.user_turn));
        Ok(())
    }

    async fn close(&self) {}

    fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo { provider: self.probe.id.into(), session_id: Some("probe-session".into()), model: None, thinking_option_id: None, mode_id: None, extra: None }
    }

    fn persistence(&self) -> PersistenceHandle {
        PersistenceHandle { provider: self.probe.id.into(), session_id: "probe-session".into(), native_handle: None, metadata: None }
    }

    fn current_mode(&self) -> Option<String> {
        None
    }
}

fn owner() -> Caller {
    Caller::Owner("local".into())
}

fn manager(dir: &tempfile::TempDir, probes: Vec<Probe>) -> Manager {
    let providers: Vec<Arc<dyn Provider>> = probes.into_iter().map(|p| Arc::new(p) as Arc<dyn Provider>).collect();
    Manager::open(Store::open(&dir.path().join("state")).unwrap(), providers, LaneConfig::default()).unwrap()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

/// An agent with two answered prompts; returns its id and its user messages' ids.
async fn two_prompts(m: &Manager, provider: &str) -> (String, Vec<String>) {
    let snap = m.create(CreateRequest { provider: provider.into(), cwd: "/tmp".into(), prompt: Some("first".into()), ..CreateRequest::default() }).await.unwrap();
    let id = snap.id;
    until("idle", || m.snapshot(&id).unwrap().status == AgentStatus::Idle).await;
    m.send(&id, "second", ActiveTurnBehavior::Interrupt).await.unwrap();
    until("idle again", || m.timeline(&id, 0, 0).unwrap().1.len() >= 4 && m.snapshot(&id).unwrap().status == AgentStatus::Idle).await;
    (id.clone(), user_ids(m, &id))
}

fn user_ids(m: &Manager, id: &str) -> Vec<String> {
    m.timeline(id, 0, 0)
        .unwrap()
        .1
        .iter()
        .filter_map(|r| match &r.item {
            TimelineItem::UserMessage { message_id, .. } => message_id.clone(),
            _ => None,
        })
        .collect()
}

fn request(message_id: &str, mode: RewindMode) -> RewindRequest {
    RewindRequest { message_id: message_id.into(), mode, allow_main_checkout: false }
}

/// Each capability flag opens its own mode and no other; a refusal says what Paseo says,
/// asks the provider nothing, and leaves a running turn running.
#[tokio::test]
async fn each_flag_opens_only_its_mode() {
    for supported in RewindMode::ALL {
        let dir = tempfile::tempdir().unwrap();
        let probe = Probe::new("probe", &[supported]);
        let calls = probe.calls.clone();
        let m = manager(&dir, vec![probe]);
        let (id, users) = two_prompts(&m, "probe").await;
        assert_eq!(users.len(), 2, "manager-recorded user messages carry ids");
        m.send(&id, "a long one", ActiveTurnBehavior::Interrupt).await.unwrap();
        until("running", || m.snapshot(&id).unwrap().status == AgentStatus::Running).await;
        for mode in RewindMode::ALL.into_iter().filter(|m| *m != supported) {
            let err = m.rewind(&owner(), &id, request(&users[1], mode)).await.unwrap_err();
            assert!(matches!(err, ManagerError::RewindUnsupported(_)), "{err:?}");
            assert_eq!(err.code(), "REWIND_UNSUPPORTED");
            assert_eq!(err.to_string(), format!("Provider does not support rewinding {}", mode.as_str()));
        }
        assert_eq!(m.snapshot(&id).unwrap().status, AgentStatus::Running, "a refused rewind doesn't cancel the turn");
        assert!(calls.lock().unwrap().is_empty());
        let before = m.timeline(&id, 0, 0).unwrap().1.len();
        let out = m.rewind(&owner(), &id, request(&users[1], supported)).await.unwrap();
        assert!(out.ok);
        assert_eq!(m.snapshot(&id).unwrap().status, AgentStatus::Idle, "the running turn was cancelled first");
        assert_eq!(*calls.lock().unwrap(), vec![(supported, users[1].clone(), 1)], "the provider got the target and its place among the user messages");
        let rows = m.timeline(&id, 0, 0).unwrap().1;
        if supported == RewindMode::Files {
            assert_eq!((out.removed_rows, rows.len() >= before), (0, true), "files mode keeps the conversation");
        } else {
            assert!(out.removed_rows > 0 && rows.len() < before);
            assert_eq!(user_ids(&m, &id), vec![users[0].clone()]);
        }
    }
}

/// A provider that sets the flag but has no `rewind` fails with the trait's default message.
#[tokio::test]
async fn a_flag_without_an_implementation_is_a_provider_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut probe = Probe::new("probe", &RewindMode::ALL);
    probe.implements = false;
    let m = manager(&dir, vec![probe]);
    let (id, users) = two_prompts(&m, "probe").await;
    let before = m.timeline(&id, 0, 0).unwrap();
    let err = m.rewind(&owner(), &id, request(&users[1], RewindMode::Both)).await.unwrap_err();
    assert!(matches!(&err, ManagerError::Provider(msg) if msg == "Provider does not support rewinding both"), "{err:?}");
    assert_eq!(m.timeline(&id, 0, 0).unwrap().0, before.0, "same epoch");
    assert_eq!(m.timeline(&id, 0, 0).unwrap().1.len(), before.1.len());
}

/// A provider failure leaves the timeline as it was.
#[tokio::test]
async fn a_failed_rewind_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut probe = Probe::new("probe", &RewindMode::ALL);
    probe.fail = Some("the transcript is gone");
    let m = manager(&dir, vec![probe]);
    let (id, users) = two_prompts(&m, "probe").await;
    let before = m.timeline(&id, 0, 0).unwrap();
    let err = m.rewind(&owner(), &id, request(&users[0], RewindMode::Conversation)).await.unwrap_err();
    assert!(matches!(&err, ManagerError::Provider(msg) if msg == "the transcript is gone"), "{err:?}");
    assert_eq!(err.code(), "PROVIDER_ERROR");
    let after = m.timeline(&id, 0, 0).unwrap();
    assert_eq!((after.0, after.1.len()), (before.0, before.1.len()));
    // And the agent takes prompts as before: nothing is left held.
    m.send(&id, "still here", ActiveTurnBehavior::Interrupt).await.unwrap();
}

/// The target must be a user message of this agent; an unknown or assistant id is refused
/// before anything runs.
#[tokio::test]
async fn the_target_must_be_a_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let probe = Probe::new("probe", &RewindMode::ALL);
    let calls = probe.calls.clone();
    let m = manager(&dir, vec![probe]);
    let (id, _) = two_prompts(&m, "probe").await;
    let assistant = m
        .timeline(&id, 0, 0)
        .unwrap()
        .1
        .iter()
        .find_map(|r| match &r.item {
            TimelineItem::AssistantMessage { message_id, .. } => message_id.clone(),
            _ => None,
        })
        .unwrap();
    for bad in ["nope", assistant.as_str(), ""] {
        let err = m.rewind(&owner(), &id, request(bad, RewindMode::Conversation)).await.unwrap_err();
        assert!(matches!(err, ManagerError::RewindTargetNotFound(_)), "{bad:?}: {err:?}");
        assert_eq!(err.code(), "REWIND_TARGET_NOT_FOUND");
    }
    assert!(calls.lock().unwrap().is_empty());
    // Another agent's message is not this agent's.
    let (_, other_users) = two_prompts(&m, "probe").await;
    assert!(matches!(m.rewind(&owner(), &id, request(&other_users[0], RewindMode::Conversation)).await, Err(ManagerError::RewindTargetNotFound(_))));
}

/// While the provider reverts, the agent takes no new prompt and no second rewind.
#[tokio::test]
async fn a_rewind_holds_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let mut probe = Probe::new("probe", &RewindMode::ALL);
    probe.delay = Duration::from_millis(600);
    let m = manager(&dir, vec![probe]);
    let (id, users) = two_prompts(&m, "probe").await;
    let rewinding = {
        let (m, id, target) = (m.clone(), id.clone(), users[1].clone());
        tokio::spawn(async move { m.rewind(&owner(), &id, request(&target, RewindMode::Conversation)).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let busy = m.send(&id, "too soon", ActiveTurnBehavior::Interrupt).await.unwrap_err();
    assert!(matches!(busy, ManagerError::Busy(_)), "{busy:?}");
    assert_eq!(busy.code(), "AGENT_BUSY");
    let again = m.rewind(&owner(), &id, request(&users[0], RewindMode::Conversation)).await.unwrap_err();
    assert!(matches!(again, ManagerError::Busy(_)), "{again:?}");
    rewinding.await.unwrap().unwrap();
    m.send(&id, "now", ActiveTurnBehavior::Interrupt).await.unwrap();
}

/// The timeline a conversation rewind leaves is stored: a new daemon reads the same rows
/// under the same new epoch, and the next prompt continues the numbering.
#[tokio::test]
async fn the_rewound_timeline_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir, vec![Probe::new("probe", &RewindMode::ALL)]);
    let (id, users) = two_prompts(&m, "probe").await;
    let epoch = m.timeline(&id, 0, 0).unwrap().0;
    let out = m.rewind(&owner(), &id, request(&users[1], RewindMode::Both)).await.unwrap();
    assert_ne!(out.epoch, epoch);
    let rows = m.timeline(&id, 0, 0).unwrap().1;
    assert_eq!(rows.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
    m.shutdown().await;
    drop(m);
    let m = manager(&dir, vec![Probe::new("probe", &RewindMode::ALL)]);
    let (epoch_again, rows_again) = m.timeline(&id, 0, 0).unwrap();
    assert_eq!(epoch_again, out.epoch);
    assert_eq!(rows_again.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
    m.send(&id, "after the restart", ActiveTurnBehavior::Interrupt).await.unwrap();
    until("the new answer", || m.timeline(&id, 0, 0).unwrap().1.len() == 4).await;
    assert_eq!(m.timeline(&id, 0, 0).unwrap().1[2].seq, 3);
}
