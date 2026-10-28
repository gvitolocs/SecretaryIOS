//! The approvals bridge end to end, in process: a real `Manager` with the mock
//! provider, and a fake secretary-server (`lucina_approvals::testing::Mock`) that signs
//! grants the way the real one does.
//!
//! The four conditions the bridge keeps, each with a positive and a negative test:
//! 1. it answers only after verifying the grant itself (pinned Ed25519 key), bound to this
//!    approval, this request and this agent, once, before expiry, deciding device read
//!    from inside the grant;
//! 2. the answer is an ordinary owner answer through the kernel (`by: owner_device`);
//! 3. it refuses approvals it did not create and grants not decided by an owner device;
//! 4. withdrawing is idempotent and a late approval of an answered request is a no-op.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lucina_agentd::approvals_bridge::{build_request, payload_hash_of, Applied, Bridge, BridgeConfig, Filed, PermKey, ACTION};
use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{CreateRequest, ExternalRequest, Manager, ManagerError, Options, PermissionView};
use lucina_agentd::mock::MockProvider;
use lucina_agentd::provider::Provider;
use lucina_agentd::store::Store;
use lucina_agents::permission_gate::{Origin, Status, Verdict};
use lucina_approvals::client::{Client, WaitReply};
use lucina_approvals::testing::{GrantSpec, Mock, SERVICE_TOKEN};
use lucina_approvals::used::UsedRegistry;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

struct Rig {
    mock: Mock,
    manager: Manager,
    bridge: Arc<Bridge>,
    handle: Option<JoinHandle<()>>,
    used_file: PathBuf,
    _dir: tempfile::TempDir,
}

async fn rig_with(run: bool, permission_ttl: Duration) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::start().await;
    let pubkey = dir.path().join("approval-grant.pub");
    mock.write_pubkey_json(&pubkey);
    let used_file = dir.path().join("used.jsonl");
    let providers: Vec<Arc<dyn Provider>> = vec![Arc::new(MockProvider)];
    let options = Options { permission_ttl, ..Options::default() };
    let manager = Manager::open_with(Store::open(&dir.path().join("state")).unwrap(), providers, LaneConfig::default(), options).unwrap();
    let mut cfg = BridgeConfig::new(&mock.url, SERVICE_TOKEN, pubkey, used_file.clone());
    cfg.retry_base = Duration::from_millis(40);
    cfg.retry_max = Duration::from_millis(160);
    cfg.wait_secs = 1;
    let bridge = Bridge::new(manager.clone(), cfg).unwrap();
    let handle = run.then(|| bridge.spawn());
    Rig { mock, manager, bridge, handle, used_file, _dir: dir }
}

async fn rig() -> Rig {
    rig_with(true, Duration::from_secs(3600)).await
}

/// No bridge task: the test files approvals and feeds answers by hand.
async fn quiet_rig() -> Rig {
    rig_with(false, Duration::from_secs(3600)).await
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn external(m: &Manager, title: &str, details: Value) -> PermissionView {
    m.request_external(
        ExternalRequest {
            kind: "external".into(),
            source: "infisical-brokerd".into(),
            title: title.into(),
            description: None,
            details: details.as_object().cloned(),
            expires_in_sec: Some(120),
        },
        Origin::Local,
    )
    .unwrap()
}

fn view_of(m: &Manager, v: &PermissionView) -> PermissionView {
    m.permission_view(v.agent_id.as_deref(), &v.id).expect("request in the book")
}

/// An agent (mock provider) that asks for an MCP tool; returns the pending request.
async fn agent_request(m: &Manager, tool: &str) -> PermissionView {
    let snap = m
        .create(CreateRequest {
            provider: "mock".into(),
            cwd: "/tmp".into(),
            title: Some("Fix the tests".into()),
            prompt: Some(format!("lucina mock mcp permission {tool}")),
            ..CreateRequest::default()
        })
        .await
        .unwrap();
    let id = snap.id.clone();
    for _ in 0..600 {
        if let Some(v) = m.permissions(false).into_iter().find(|p| p.agent_id.as_deref() == Some(id.as_str())) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the mock agent never asked");
}

fn key_of(v: &PermissionView) -> PermKey {
    PermKey::of(v)
}

/// File a request by hand, as the bridge's task does.
async fn file_by_hand(r: &Rig, v: &PermissionView) -> Filed {
    let c = r.manager.approval_candidate(v.agent_id.as_deref(), &v.id).expect("pending");
    let req = build_request(&c, lucina_approvals::now_unix());
    let client = Client::with_token(&r.mock.url, SERVICE_TOKEN.into()).unwrap();
    let created = client.create(&req).await.unwrap();
    assert!(r.manager.attach_approval(v.agent_id.as_deref(), &v.id, &created.id));
    Filed { approval_id: created.id, sensitive: req.sensitive }
}

fn reply(id: &str, status: &str, grant: Option<&str>, decided_by: Option<&str>) -> WaitReply {
    serde_json::from_value(json!({"id": id, "status": status, "grant": grant, "decidedBy": decided_by})).unwrap()
}

fn assert_untouched(r: &Rig, v: &PermissionView) {
    let now = view_of(&r.manager, v);
    assert_eq!(now.status, Status::Pending, "{now:?}");
    assert!(now.resolution.is_none());
}

// -- the plan: filing, answering ----------------------------------------------------

#[tokio::test]
async fn an_agent_request_is_filed_and_a_verified_approval_allows_it_as_the_owner_device() {
    let r = rig().await;
    let mut events = r.manager.journal().subscribe(None).live;
    let v = agent_request(&r.manager, "lucina/notes_search").await;
    let ids = r.mock.wait_created(1).await;

    // What the owner is asked.
    let body = &r.mock.created()[0];
    assert_eq!(body["action"], "agent.permission");
    assert_eq!(body["requester"]["kind"], "agentd");
    assert_eq!(body["requester"]["name"], "Lucina Agents");
    assert_eq!(body["requester"]["detail"], "Fix the tests · mock");
    assert_eq!(body["summary"], "Mock wants to run: lucina: notes_search x");
    assert_eq!(body["sensitive"], false);
    assert_eq!(body["payloadHash"], payload_hash_of(&v));
    assert!(body["details"].as_str().unwrap().starts_with("Tool: mcp__lucina__notes_search\nAgent: Fix the tests\nInput:\nquery: x"));
    let ttl = body["ttlSecs"].as_u64().unwrap();
    assert!((30..=900).contains(&ttl));

    // The approval id is on the permission, in the API view and in the stream.
    until("the approval id is attached", || view_of(&r.manager, &v).approval_id.as_deref() == Some(ids[0].as_str())).await;
    let json = serde_json::to_value(view_of(&r.manager, &v)).unwrap();
    assert_eq!(json["approvalId"], ids[0].as_str());
    let mut saw = false;
    while let Ok(e) = events.try_recv() {
        saw |= e.data.contains(&format!("\"approvalId\":\"{}\"", ids[0]));
    }
    assert!(saw, "a permission event carries approvalId");

    // The owner presses Yes on his phone: the grant says dev_main, the reply around it
    // carries the device's *name*.
    r.mock.approve(&ids[0]);
    until("the request is allowed", || view_of(&r.manager, &v).status == Status::Allowed).await;
    let done = view_of(&r.manager, &v);
    let res = done.resolution.unwrap();
    assert_eq!(res.by, "owner_device");
    assert_eq!(res.device_id.as_deref(), Some("dev_main"));
    // The provider got the answer: nothing is pending for the agent any more.
    until("the agent is no longer waiting", || r.manager.permissions(false).is_empty()).await;
    // Answered here: nothing to withdraw.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(r.mock.withdrawn().is_empty(), "{:?}", r.mock.withdrawn());
}

#[tokio::test]
async fn a_denial_from_the_phone_denies() {
    let r = rig().await;
    let v = external(&r.manager, "Reveal the Cloudflare token", json!({"name": "CLOUDFLARE_TOKEN"}));
    let ids = r.mock.wait_created(1).await;
    let body = &r.mock.created()[0];
    assert_eq!(body["sensitive"], true, "external requests are sensitive");
    assert_eq!(body["risk"], "high");
    assert_eq!(body["summary"], "infisical-brokerd asks: Reveal the Cloudflare token");
    r.mock.deny(&ids[0]);
    until("denied", || view_of(&r.manager, &v).status == Status::Denied).await;
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!((res.by, res.behavior), ("owner_device", Verdict::Deny));
    assert_eq!(res.device_id, None, "a denial has no grant to name a device");
}

#[tokio::test]
async fn secret_values_never_leave_in_the_details() {
    let r = rig().await;
    external(&r.manager, "Use a token", json!({"command": "curl -H 'Authorization: Bearer abcdef1234567890xyz' https://x", "env": "API_KEY=sk-abcdefghijklmnopqrstuvwx"}));
    r.mock.wait_created(1).await;
    let body = r.mock.created()[0].to_string();
    assert!(!body.contains("abcdef1234567890xyz") && !body.contains("sk-abcdefghijklmnop"), "{body}");
    assert!(body.contains("[redacted]"));
}

#[tokio::test]
async fn questions_are_not_filed_but_plans_are() {
    let r = rig().await;
    let q = r
        .manager
        .create(CreateRequest { provider: "mock".into(), cwd: "/tmp".into(), prompt: Some("emit synthetic questions".into()), ..CreateRequest::default() })
        .await
        .unwrap();
    until("the question is pending", || r.manager.permissions(false).iter().any(|p| p.agent_id.as_deref() == Some(q.id.as_str()))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.mock.created().is_empty(), "a question needs an answer, not a Yes");
    let plan = r
        .manager
        .create(CreateRequest { provider: "mock".into(), cwd: "/tmp".into(), prompt: Some("emit synthetic plan approval".into()), ..CreateRequest::default() })
        .await
        .unwrap();
    r.mock.wait_created(1).await;
    assert_eq!(r.mock.created().len(), 1);
    let _ = plan;
}

#[tokio::test]
async fn server_errors_are_retried_with_backoff() {
    let r = rig().await;
    r.mock.fail_create_with(500);
    let v = external(&r.manager, "Retry me", json!({}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.mock.created().is_empty());
    assert_eq!(view_of(&r.manager, &v).approval_id, None);
    r.mock.fail_create_with(0);
    let ids = r.mock.wait_created(1).await;
    until("filed after the server recovered", || view_of(&r.manager, &v).approval_id.as_deref() == Some(ids[0].as_str())).await;
    // A client error is not retried forever.
    r.mock.fail_create_with(400);
    let w = external(&r.manager, "Refused", json!({}));
    tokio::time::sleep(Duration::from_millis(500)).await;
    r.mock.fail_create_with(0);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(r.mock.created().len(), 1, "the refused one stays with the web and lagent");
    assert_eq!(view_of(&r.manager, &w).approval_id, None);
}

#[tokio::test]
async fn without_the_bridge_nothing_changes() {
    let r = quiet_rig().await;
    let v = external(&r.manager, "Plain", json!({}));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(r.mock.created().is_empty());
    let json = serde_json::to_value(view_of(&r.manager, &v)).unwrap();
    assert!(json.get("approvalId").is_none(), "{json}");
    r.manager.answer_external(&v.id, Verdict::Allow, None, Origin::Local).unwrap();
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!((res.by, res.device_id), ("local", None));
}

#[tokio::test]
async fn shutdown_ends_the_tasks_and_withdraws_what_is_open() {
    let mut r = rig().await;
    let v = external(&r.manager, "Open at shutdown", json!({}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;
    r.manager.close_streams();
    let h = r.handle.take().unwrap();
    tokio::time::timeout(Duration::from_secs(3), h).await.expect("the bridge ends on shutdown").unwrap();
    assert_eq!(r.mock.withdrawn(), vec![ids[0].clone()]);
    assert_eq!(r.mock.status_of(&ids[0]), "expired");
}

// -- condition 1: the grant is verified by agentd itself ----------------------------

#[tokio::test]
async fn a_grant_that_does_not_verify_or_does_not_fit_answers_nothing() {
    let bad_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let cases: Vec<(&str, GrantSpec, &str)> = vec![
        ("another payload hash", GrantSpec { payload_hash: Some(bad_hash), ..GrantSpec::default() }, "different content"),
        ("another key", GrantSpec { wrong_key: true, ..GrantSpec::default() }, "signature"),
        ("another action", GrantSpec { action: Some("root.exec".into()), ..GrantSpec::default() }, "another action"),
        ("another approval id", GrantSpec { id: Some("apr_somebody_else".into()), ..GrantSpec::default() }, "another approval"),
        ("past its expiry", GrantSpec { exp_in: -5, ..GrantSpec::default() }, "expired"),
        ("no biometric on a sensitive request", GrantSpec { biometric: false, ..GrantSpec::default() }, "biometric"),
    ];
    for (name, spec, why) in cases {
        let r = quiet_rig().await;
        let v = external(&r.manager, "Reveal", json!({"name": "X"}));
        let filed = file_by_hand(&r, &v).await;
        let grant = r.mock.approve_with(&filed.approval_id, &spec);
        let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&filed.approval_id, "approved", Some(&grant), Some("iPhone"))).await;
        match out {
            Applied::Refused(msg) => assert!(msg.contains(why), "{name}: {msg}"),
            other => panic!("{name}: {other:?}"),
        }
        assert_untouched(&r, &v);
    }
}

#[tokio::test]
async fn a_bad_grant_through_the_live_task_leaves_the_request_pending() {
    let r = rig().await;
    let v = external(&r.manager, "Reveal", json!({"name": "X"}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;
    r.mock.approve_with(&ids[0], &GrantSpec { wrong_key: true, ..GrantSpec::default() });
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_untouched(&r, &v);
    // Still answerable the ordinary way.
    r.manager.answer_external(&v.id, Verdict::Deny, None, Origin::Local).unwrap();
}

#[tokio::test]
async fn a_grant_is_bound_to_its_request_and_its_agent() {
    let r = quiet_rig().await;
    let a = external(&r.manager, "First", json!({"name": "A"}));
    let b = external(&r.manager, "Second", json!({"name": "B"}));
    let fa = file_by_hand(&r, &a).await;
    let fb = file_by_hand(&r, &b).await;
    let grant_a = r.mock.approve(&fa.approval_id);
    // A's grant, presented for B (under B's approval id, or A's): not accepted.
    for filed in [&fb, &fa] {
        let out = r.bridge.apply_reply(&key_of(&b), filed, reply(&filed.approval_id, "approved", Some(&grant_a), None)).await;
        assert!(matches!(out, Applied::Refused(_) | Applied::NoOp(_)), "{out:?}");
    }
    assert_untouched(&r, &b);
    // The same request id under another agent is another payload: the agent id is part of the hash.
    let agent_req = agent_request(&r.manager, "lucina/notes_search").await;
    let mut twin = agent_req.clone();
    twin.agent_id = Some("another_agent".into());
    assert_ne!(payload_hash_of(&agent_req), payload_hash_of(&twin));
    // A's grant does answer A.
    let out = r.bridge.apply_reply(&key_of(&a), &fa, reply(&fa.approval_id, "approved", Some(&grant_a), None)).await;
    assert_eq!(out, Applied::Allowed { device_id: "dev_main".into() });
}

#[tokio::test]
async fn a_grant_is_used_once() {
    let r = quiet_rig().await;
    let v = external(&r.manager, "Once", json!({}));
    let filed = file_by_hand(&r, &v).await;
    let grant = r.mock.approve(&filed.approval_id);
    // The id was spent before (another process, an earlier run): the grant is refused.
    UsedRegistry::new(&r.used_file).consume(&filed.approval_id, lucina_approvals::now_unix() + 100, lucina_approvals::now_unix()).unwrap();
    let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&filed.approval_id, "approved", Some(&grant), None)).await;
    assert_eq!(out, Applied::Refused("the grant was already used".into()));
    assert_untouched(&r, &v);

    // A fresh one is accepted exactly once, and recorded.
    let w = external(&r.manager, "Twice", json!({}));
    let fw = file_by_hand(&r, &w).await;
    let gw = r.mock.approve(&fw.approval_id);
    let first = r.bridge.apply_reply(&key_of(&w), &fw, reply(&fw.approval_id, "approved", Some(&gw), None)).await;
    assert!(matches!(first, Applied::Allowed { .. }), "{first:?}");
    assert!(UsedRegistry::new(&r.used_file).contains(&fw.approval_id, lucina_approvals::now_unix()));
    let second = r.bridge.apply_reply(&key_of(&w), &fw, reply(&fw.approval_id, "approved", Some(&gw), None)).await;
    assert!(matches!(second, Applied::NoOp(_)), "{second:?}");
}

#[tokio::test]
async fn who_decided_comes_from_inside_the_grant_never_from_the_reply() {
    let r = quiet_rig().await;
    let v = external(&r.manager, "Who", json!({}));
    let filed = file_by_hand(&r, &v).await;
    let grant = r.mock.approve_with(&filed.approval_id, &GrantSpec { decided_by: Some("dev_phone".into()), ..GrantSpec::default() });
    // The reply around the grant names somebody else, even something that looks like a device.
    let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&filed.approval_id, "approved", Some(&grant), Some("dev_attacker"))).await;
    assert_eq!(out, Applied::Allowed { device_id: "dev_phone".into() });
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!((res.by, res.device_id.as_deref()), ("owner_device", Some("dev_phone")));
}

#[tokio::test]
async fn an_approval_without_a_grant_or_for_another_id_is_not_trusted() {
    let r = quiet_rig().await;
    let v = external(&r.manager, "Odd", json!({}));
    let filed = file_by_hand(&r, &v).await;
    let none = r.bridge.apply_reply(&key_of(&v), &filed, reply(&filed.approval_id, "approved", None, Some("iPhone"))).await;
    assert!(matches!(none, Applied::Refused(_)), "{none:?}");
    let grant = r.mock.approve(&filed.approval_id);
    let other = r.bridge.apply_reply(&key_of(&v), &filed, reply("apr_not_mine", "approved", Some(&grant), None)).await;
    assert!(matches!(other, Applied::Refused(_)), "{other:?}");
    assert_untouched(&r, &v);
}

// -- condition 2 and 3: the kernel decides; only agentd's own approvals, only devices ---

#[tokio::test]
async fn the_answer_is_an_ordinary_owner_answer_and_nobody_else_gets_through() {
    let r = quiet_rig().await;
    let v = external(&r.manager, "Kernel", json!({}));
    let filed = file_by_hand(&r, &v).await;

    // Not the approval filed for this request: refused before the kernel is asked.
    let err = r.manager.answer_from_approval(None, &v.id, "apr_not_mine", Verdict::Allow, Some("dev_x")).await.unwrap_err();
    assert!(matches!(err, ManagerError::Forbidden(_)), "{err:?}");
    assert_untouched(&r, &v);
    // A request with no approval attached cannot be answered this way at all.
    let plain = external(&r.manager, "Unfiled", json!({}));
    let err = r.manager.answer_from_approval(None, &plain.id, "apr_x", Verdict::Allow, Some("dev_x")).await.unwrap_err();
    assert!(matches!(err, ManagerError::Forbidden(_)), "{err:?}");
    // The kernel's other rules are untouched: an MCP token or an agent still can't answer.
    for origin in [Origin::Mcp, Origin::Agent, Origin::Unknown] {
        let err = r.manager.answer_external(&v.id, Verdict::Allow, None, origin).unwrap_err();
        assert!(matches!(err, ManagerError::Forbidden(_)), "{origin:?}: {err:?}");
    }
    assert_untouched(&r, &v);

    // The right approval answers as the owner's device, once.
    r.manager.answer_from_approval(None, &v.id, &filed.approval_id, Verdict::Allow, Some("dev_main")).await.unwrap();
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!((res.by, res.device_id.as_deref(), res.behavior), ("owner_device", Some("dev_main"), Verdict::Allow));
    let err = r.manager.answer_from_approval(None, &v.id, &filed.approval_id, Verdict::Deny, Some("dev_main")).await.unwrap_err();
    assert!(matches!(err, ManagerError::AlreadyAnswered(_)), "{err:?}");
}

#[tokio::test]
async fn grants_not_decided_by_an_owner_device_or_not_requested_by_agentd_are_refused() {
    for (name, spec) in [
        ("the service token", GrantSpec { decided_by: Some("svc".into()), ..GrantSpec::default() }),
        ("an MCP token", GrantSpec { decided_by: Some("mcp:hermes".into()), ..GrantSpec::default() }),
        ("a service origin", GrantSpec { decided_by: Some("service:infisical-broker".into()), ..GrantSpec::default() }),
        ("an agent", GrantSpec { decided_by: Some("agent_1".into()), ..GrantSpec::default() }),
        ("nobody", GrantSpec { decided_by: Some(String::new()), ..GrantSpec::default() }),
        ("an approval made by another requester", GrantSpec { requester_kind: Some("lucina".into()), ..GrantSpec::default() }),
        ("an approval made by an MCP client", GrantSpec { requester_kind: Some("mcp".into()), ..GrantSpec::default() }),
    ] {
        let r = quiet_rig().await;
        let v = external(&r.manager, "Who may", json!({}));
        let filed = file_by_hand(&r, &v).await;
        let grant = r.mock.approve_with(&filed.approval_id, &spec);
        let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&filed.approval_id, "approved", Some(&grant), Some("iPhone"))).await;
        assert!(matches!(out, Applied::Refused(_)), "{name}: {out:?}");
        assert_untouched(&r, &v);
        // Nothing was spent either: the right grant can still come.
        assert!(!UsedRegistry::new(&r.used_file).contains(&filed.approval_id, lucina_approvals::now_unix()), "{name}");
    }
}

#[tokio::test]
async fn an_approval_that_agentd_did_not_file_is_ignored() {
    let r = rig().await;
    let v = external(&r.manager, "Mine", json!({"n": 1}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;
    // Somebody else files an approval for the same thing on the server (another requester, an
    // MCP client, an agent process): agentd never waits on it, and its grant would not match.
    let other = Client::with_token(&r.mock.url, SERVICE_TOKEN.into()).unwrap();
    let c = r.manager.approval_candidate(None, &v.id).unwrap();
    let mut req = build_request(&c, lucina_approvals::now_unix());
    req.requester.kind = "mcp".into();
    let foreign = other.create(&req).await.unwrap();
    r.mock.approve(&foreign.id);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_untouched(&r, &v);
    // Its own approval still works.
    r.mock.approve(&ids[0]);
    until("allowed by its own approval", || view_of(&r.manager, &v).status == Status::Allowed).await;
    assert_eq!(ACTION, "agent.permission");
}

// -- condition 4: withdraw and late approvals ----------------------------------------

#[tokio::test]
async fn a_request_answered_elsewhere_is_withdrawn_and_a_late_approval_is_a_no_op() {
    let r = rig().await;
    let v = external(&r.manager, "Elsewhere", json!({}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;

    // `lagent permit` / the web /agents page answers first.
    r.manager.answer_external(&v.id, Verdict::Deny, None, Origin::Local).unwrap();
    until("withdrawn", || r.mock.withdrawn() == vec![ids[0].clone()]).await;
    assert_eq!(r.mock.status_of(&ids[0]), "expired");

    // The owner's phone approves late (the server's race): nothing happens, nothing is logged loudly.
    let grant = r.mock.approve(&ids[0]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!((res.by, res.behavior, res.device_id), ("local", Verdict::Deny, None));
    assert_eq!(r.mock.withdrawn().len(), 1, "no second withdraw");

    // Even if the late approval is fed in directly: AlreadyAnswered is a quiet no-op.
    let filed = Filed { approval_id: ids[0].clone(), sensitive: true };
    let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&ids[0], "approved", Some(&grant), None)).await;
    assert!(matches!(out, Applied::NoOp(_)), "{out:?}");
    let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&ids[0], "denied", None, None)).await;
    assert!(matches!(out, Applied::NoOp(_)), "{out:?}");
    // The manager says the same when asked straight.
    let err = r.manager.answer_from_approval(None, &v.id, &ids[0], Verdict::Allow, Some("dev_main")).await.unwrap_err();
    assert!(matches!(err, ManagerError::AlreadyAnswered(_)), "{err:?}");
}

#[tokio::test]
async fn withdrawing_twice_is_the_same_as_once() {
    let r = rig().await;
    let v = external(&r.manager, "Twice", json!({}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;
    r.manager.answer_external(&v.id, Verdict::Allow, None, Origin::Local).unwrap();
    until("withdrawn", || !r.mock.withdrawn().is_empty()).await;
    let client = Client::with_token(&r.mock.url, SERVICE_TOKEN.into()).unwrap();
    for _ in 0..2 {
        let status = client.withdraw(&ids[0]).await.unwrap();
        assert_eq!(format!("{status:?}"), "Expired");
    }
    // An id the server never heard of is not an error to the bridge: the client reports NotFound.
    assert_eq!(client.withdraw("apr_nope").await.unwrap_err(), lucina_approvals::ApprovalError::NotFound);
    assert_eq!(view_of(&r.manager, &v).resolution.unwrap().by, "local", "the local answer stands");
}

#[tokio::test]
async fn a_request_that_expires_locally_is_withdrawn_and_a_late_approval_is_a_no_op() {
    let r = rig_with(true, Duration::from_millis(700)).await;
    let v = agent_request(&r.manager, "lucina/notes_search").await;
    let ids = r.mock.wait_created(1).await;
    until("denied by expiry", || view_of(&r.manager, &v).status == Status::Denied || view_of(&r.manager, &v).status == Status::Expired).await;
    until("withdrawn", || !r.mock.withdrawn().is_empty()).await;
    let res = view_of(&r.manager, &v).resolution.unwrap();
    assert_eq!(res.by, "expiry");
    let grant = r.mock.approve(&ids[0]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(view_of(&r.manager, &v).resolution.unwrap().by, "expiry", "the late Yes changes nothing");
    let filed = Filed { approval_id: ids[0].clone(), sensitive: false };
    let out = r.bridge.apply_reply(&key_of(&v), &filed, reply(&ids[0], "approved", Some(&grant), None)).await;
    assert!(matches!(out, Applied::NoOp(_)), "{out:?}");
}

#[tokio::test]
async fn an_expired_approval_leaves_the_request_pending() {
    let r = rig().await;
    let v = external(&r.manager, "Expire", json!({}));
    let ids = r.mock.wait_created(1).await;
    until("filed", || view_of(&r.manager, &v).approval_id.is_some()).await;
    r.mock.expire(&ids[0]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_untouched(&r, &v);
    assert!(r.mock.withdrawn().is_empty(), "nothing to withdraw: it expired on the server");
    r.manager.answer_external(&v.id, Verdict::Allow, None, Origin::Local).unwrap();
}
