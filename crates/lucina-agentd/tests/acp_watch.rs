//! The ACP turn watch (providers/acp.rs, lucina_agents::turn_watch) on a fake agent
//! (`tests/support/fake_acp.rs`) that reproduces `opencode acp` on Zen's free tier: it logs
//! "Free usage limit reached" and never answers the prompt (agent cc504145 hung as
//! "running", 2026-11-08). Without the watch that turn never ends; with it the turn ends
//! as a quota stop, which the manager hands on. Long tools and permissions waiting for
//! the owner are not stalls.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lucina_agentd::provider::{Provider, Session, SessionConfig};
use lucina_agentd::providers::acp::AcpProvider;
use lucina_agents::{PermissionResponse, StreamEvent};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

/// The fake agent (`tests/support/fake_acp.rs`, bin `lucina-fake-acp`).
fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lucina-fake-acp"))
}

struct Fake {
    session: Arc<dyn Session>,
    events: UnboundedReceiver<StreamEvent>,
    _dir: tempfile::TempDir,
}

async fn start(mode: &str, stall: Option<Duration>, error_log: Option<PathBuf>, env: &[(&str, &str)]) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let provider = AcpProvider {
        id: "opencode".into(),
        label: "OpenCode".into(),
        bin: fake_bin(),
        args: Vec::new(),
        default_model: None,
        client_meta: None,
        registry: None,
        stall,
        error_log,
    };
    let mut vars: Vec<(String, String)> = vec![("FAKE_ACP_MODE".into(), mode.into())];
    vars.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let config = SessionConfig { cwd: dir.path().display().to_string(), env: vars, ..SessionConfig::default() };
    let (tx, events) = unbounded_channel();
    let session = provider.create_session(config, tx).await.expect("fake agent starts");
    Fake { session, events, _dir: dir }
}

/// The event that ends the turn, or `None` when none came within `limit`. Permission
/// requests on the way are returned too.
async fn turn_end(f: &mut Fake, limit: Duration) -> (Option<StreamEvent>, Vec<String>) {
    let deadline = tokio::time::Instant::now() + limit;
    let mut permissions = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, f.events.recv()).await {
            Err(_) | Ok(None) => return (None, permissions),
            Ok(Some(e @ (StreamEvent::TurnFailed { .. } | StreamEvent::TurnCompleted { .. } | StreamEvent::TurnCanceled { .. }))) => return (Some(e), permissions),
            Ok(Some(StreamEvent::PermissionRequested { request, .. })) => {
                permissions.push(request.id.clone());
                return (None, permissions);
            }
            Ok(Some(_)) => {}
        }
    }
}

fn log_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.log");
    // Earlier lines of other runs are already there and must not count.
    std::fs::write(&path, "timestamp=2026-11-08T09:13:53.657Z level=ERROR message=\"stream error\" session.id=ses_fake0123456789abcdefABCD error.error=\"AI_APICallError: Free usage limit reached. (an earlier turn)\"\n").unwrap();
    (dir, path)
}

#[tokio::test]
async fn without_the_watch_the_zen_limit_hangs_the_turn() {
    // The bug as it was: no stall rule, no log to read: nothing ever ends the turn.
    let mut f = start("limit", None, None, &[]).await;
    f.session.start_turn("Create done.txt", None).await.unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(3)).await;
    assert!(end.is_none(), "the fake agent must hang like opencode: {end:?}");
    f.session.close().await;
}

#[tokio::test]
async fn the_zen_limit_in_the_agents_log_ends_the_turn_as_a_quota_stop() {
    let (_d, log) = log_file();
    let mut f = start("limit", None, Some(log.clone()), &[("FAKE_ACP_LOG", log.to_str().unwrap())]).await;
    f.session.start_turn("Create done.txt", None).await.unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(15)).await;
    match end {
        Some(StreamEvent::TurnFailed { error, code, diagnostic, .. }) => {
            assert!(error.starts_with("AI_APICallError: Free usage limit reached."), "{error}");
            assert!(!error.contains("an earlier turn"), "only lines written during this turn count");
            assert_eq!(code.as_deref(), Some("quota"));
            assert!(diagnostic.unwrap_or_default().contains("usage limit in the agent's log"));
        }
        other => panic!("expected a quota stop, got {other:?}"),
    }
    f.session.close().await;
}

#[tokio::test]
async fn another_sessions_limit_does_not_end_this_turn() {
    let (_d, log) = log_file();
    let mut f = start("limit", None, Some(log.clone()), &[("FAKE_ACP_LOG", log.to_str().unwrap()), ("FAKE_ACP_LOG_SESSION", "ses_someone_else")]).await;
    f.session.start_turn("Create done.txt", None).await.unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(3)).await;
    assert!(end.is_none(), "{end:?}");
    f.session.close().await;
}

#[tokio::test]
async fn a_silent_turn_stalls_into_a_quota_stop() {
    let mut f = start("silent", Some(Duration::from_millis(800)), None, &[]).await;
    f.session.start_turn("Create done.txt", None).await.unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(15)).await;
    match end {
        Some(StreamEvent::TurnFailed { error, code, .. }) => {
            assert!(error.contains("stalled"), "{error}");
            assert_eq!(code.as_deref(), Some("quota"));
        }
        other => panic!("expected a stall, got {other:?}"),
    }
    // The agent was told to stop; the next turn starts normally.
    f.session.close().await;
}

#[tokio::test]
async fn a_long_tool_is_not_a_stall() {
    let mut f = start("tool", Some(Duration::from_millis(400)), None, &[("FAKE_ACP_TOOL_MS", "2500")]).await;
    f.session.start_turn("Build it", None).await.unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(15)).await;
    assert!(matches!(end, Some(StreamEvent::TurnCompleted { .. })), "{end:?}");
    f.session.close().await;
}

#[tokio::test]
async fn a_permission_waiting_for_the_owner_is_not_a_stall() {
    let mut f = start("permission", Some(Duration::from_millis(400)), None, &[]).await;
    f.session.start_turn("Build it", None).await.unwrap();
    let (end, permissions) = turn_end(&mut f, Duration::from_secs(10)).await;
    assert!(end.is_none(), "{end:?}");
    assert_eq!(permissions.len(), 1);
    // The owner takes his time: five stall periods pass with nothing.
    let (end, _) = turn_end(&mut f, Duration::from_secs(2)).await;
    assert!(end.is_none(), "a pending permission must not stall the turn: {end:?}");
    f.session
        .respond_permission(&permissions[0], PermissionResponse::Allow { selected_action_id: Some("allow".into()), updated_input: None, updated_permissions: None })
        .await
        .unwrap();
    let (end, _) = turn_end(&mut f, Duration::from_secs(10)).await;
    assert!(matches!(end, Some(StreamEvent::TurnCompleted { .. })), "{end:?}");
    f.session.close().await;
}
