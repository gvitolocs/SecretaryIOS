//! `root_exec` end to end with the mock approvals server and a fake sudo.

use std::sync::Arc;
use std::time::Duration;

use lucina_approvals::root::Who;
use lucina_approvals::testing::{Fixture, GrantSpec};
use lucina_approvals::{plan_hash, Approvals, RootOutcome, RootReport};

fn plan(cmds: &[&[&str]]) -> Vec<Vec<String>> {
    cmds.iter()
        .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

fn who() -> Who {
    Who::lucina("root tests")
}

/// Ask for `plan`, let `decide` answer once the request exists, return the report.
async fn run_with(
    fx: &Fixture,
    a: &Arc<Approvals>,
    plan: Vec<Vec<String>>,
    decide: impl FnOnce(&Fixture, &str),
) -> RootReport {
    let a2 = a.clone();
    let task = tokio::spawn(async move {
        a2.root_exec_wait(&plan, "test reason", &who(), Duration::from_secs(10))
            .await
    });
    let ids = fx.mock.wait_created(1).await;
    decide(fx, &ids[ids.len() - 1]);
    task.await.unwrap()
}

#[tokio::test]
async fn approved_plan_runs_in_order_with_spaces_kept_in_one_argv() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let p = plan(&[
        &["echo", "one"],
        &["echo", "two words", "and 'quotes'"],
        &["true"],
    ]);
    let report = run_with(&fx, &a, p.clone(), |fx, id| {
        fx.mock.approve(id);
    })
    .await;
    assert_eq!(report.outcome, RootOutcome::Done, "{}", report.summary());
    assert_eq!(report.steps.len(), 3);
    assert_eq!(report.steps[0].stdout.trim(), "one");
    assert_eq!(report.steps[1].stdout.trim(), "two words and 'quotes'");
    assert_eq!(
        fx.sudo_calls(),
        vec![
            "<echo><one>".to_owned(),
            "<echo><two words><and 'quotes'>".to_owned(),
            "<true>".to_owned()
        ]
    );
    // what the owner was shown
    let body = &fx.mock.created()[0];
    assert_eq!(body["action"], "root.exec");
    assert_eq!(body["sensitive"], true);
    assert_eq!(body["risk"], "high");
    assert_eq!(body["payloadHash"], plan_hash(&p));
    assert_eq!(
        body["details"],
        "echo one\necho 'two words' 'and '\\''quotes'\\'''\ntrue"
    );
    assert_eq!(body["requester"]["kind"], "lucina");
    // audited, mode 0600
    let audit = fx.audit_lines();
    let events: Vec<_> = audit.iter().map(|l| l["event"].as_str().unwrap()).collect();
    assert_eq!(events, vec!["requested", "approved", "ran"]);
    assert_eq!(audit[2]["steps"].as_array().unwrap().len(), 3);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&fx.cfg.root_audit)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn stops_at_the_first_failure() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let report = run_with(
        &fx,
        &a,
        plan(&[&["true"], &["false"], &["echo", "never"]]),
        |fx, id| {
            fx.mock.approve(id);
        },
    )
    .await;
    assert_eq!(report.outcome, RootOutcome::Failed { index: 1 });
    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.steps[1].exit_code, Some(1));
    assert_eq!(
        fx.sudo_calls(),
        vec!["<true>".to_owned(), "<false>".to_owned()]
    );
    assert!(report.summary().contains("step 2"));
    let events: Vec<_> = fx
        .audit_lines()
        .iter()
        .map(|l| l["event"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(events.last().map(String::as_str), Some("failed"));
}

#[tokio::test]
async fn deny_and_expiry_run_nothing() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let r = run_with(&fx, &a, plan(&[&["echo", "x"]]), |fx, id| fx.mock.deny(id)).await;
    assert!(
        matches!(r.outcome, RootOutcome::Denied { .. }),
        "{:?}",
        r.outcome
    );
    assert!(r.steps.is_empty());

    let fx2 = Fixture::new().await;
    let a2 = Arc::new(fx2.approvals());
    let r = run_with(&fx2, &a2, plan(&[&["echo", "x"]]), |fx, id| {
        fx.mock.expire(id)
    })
    .await;
    assert_eq!(r.outcome, RootOutcome::Expired);
    for f in [&fx, &fx2] {
        assert!(f.sudo_calls().is_empty(), "nothing ran");
    }
    let events: Vec<_> = fx
        .audit_lines()
        .iter()
        .map(|l| l["event"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(events, vec!["requested", "denied"]);
}

#[tokio::test]
async fn unacceptable_grants_run_nothing() {
    let p = plan(&[&["echo", "x"]]);
    for spec in [
        GrantSpec {
            payload_hash: Some(plan_hash(&plan(&[&["echo", "y"]]))),
            ..Default::default()
        },
        GrantSpec {
            action: Some("secret.reveal".into()),
            ..Default::default()
        },
        GrantSpec {
            exp_in: -5,
            ..Default::default()
        },
        GrantSpec {
            wrong_key: true,
            ..Default::default()
        },
        GrantSpec {
            biometric: false,
            ..Default::default()
        },
    ] {
        let fx = Fixture::new().await;
        let a = Arc::new(fx.approvals());
        let r = run_with(&fx, &a, p.clone(), |fx, id| {
            fx.mock.approve_with(id, &spec);
        })
        .await;
        assert!(
            matches!(r.outcome, RootOutcome::Refused(_)),
            "{spec:?}: {:?}",
            r.outcome
        );
        assert!(fx.sudo_calls().is_empty(), "{spec:?}: nothing ran");
        assert_eq!(
            fx.audit_lines().last().unwrap()["event"],
            "refused",
            "{spec:?}"
        );
    }
}

#[tokio::test]
async fn a_grant_cannot_be_replayed() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let p = plan(&[&["echo", "once"]]);
    let pending = a.root_begin(&p, "replay test", &who()).await.unwrap();
    fx.mock.approve(&pending.begun.id);
    let first = a.root_finish(&pending, Duration::from_secs(5)).await;
    assert_eq!(first.outcome, RootOutcome::Done);
    let second = a.root_finish(&pending, Duration::from_secs(5)).await;
    match &second.outcome {
        RootOutcome::Refused(why) => assert!(why.contains("already used"), "{why}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(fx.sudo_calls().len(), 1, "it ran once");
}

#[tokio::test]
async fn a_hanging_command_times_out_and_stops_the_plan() {
    let mut fx = Fixture::new().await;
    fx.cfg.root_cmd_timeout = Duration::from_millis(600);
    let a = Arc::new(fx.approvals());
    let started = std::time::Instant::now();
    let r = run_with(
        &fx,
        &a,
        plan(&[&["sleep", "30"], &["echo", "no"]]),
        |fx, id| {
            fx.mock.approve(id);
        },
    )
    .await;
    assert_eq!(r.outcome, RootOutcome::Failed { index: 0 });
    assert!(r.steps[0].timed_out);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(fx.sudo_calls().len(), 1);
}

#[tokio::test]
async fn output_is_capped() {
    let mut fx = Fixture::new().await;
    fx.cfg.output_cap = 300;
    let a = Arc::new(fx.approvals());
    let r = run_with(
        &fx,
        &a,
        plan(&[&["sh", "-c", "head -c 200000 /dev/zero | tr '\\0' a"]]),
        |fx, id| {
            fx.mock.approve(id);
        },
    )
    .await;
    assert_eq!(r.outcome, RootOutcome::Done);
    assert_eq!(r.steps[0].stdout.len(), 300);
    assert!(r.steps[0].truncated);
}

#[tokio::test]
async fn bad_plans_never_reach_the_server() {
    let fx = Fixture::new().await;
    let a = fx.approvals();
    for p in [
        vec![],
        vec![vec![]],
        vec![vec!["echo".to_owned(), "a\0b".to_owned()]],
        // too long to show the owner in full (4000 characters)
        vec![
            vec!["echo".to_owned(), "x".repeat(3900)],
            vec!["echo".to_owned(), "y".repeat(300)],
        ],
    ] {
        let r = a
            .root_exec_wait(&p, "bad", &who(), Duration::from_millis(100))
            .await;
        assert!(
            matches!(r.outcome, RootOutcome::Refused(_)),
            "{:?}",
            r.outcome
        );
    }
    let r = a
        .root_exec_wait(
            &plan(&[&["true"]]),
            "  ",
            &who(),
            Duration::from_millis(100),
        )
        .await;
    assert!(matches!(r.outcome, RootOutcome::Refused(_)));
    assert!(fx.mock.created().is_empty());
}

#[tokio::test]
async fn pending_plan_reports_pending_and_a_second_ask_reuses_it() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let p = plan(&[&["echo", "later"]]);
    let r = a
        .root_exec_wait(&p, "slow owner", &who(), Duration::from_millis(300))
        .await;
    assert!(matches!(r.outcome, RootOutcome::Pending { .. }));
    let id = r.approval_id.clone().unwrap();
    let r2 = a
        .root_exec_wait(&p, "slow owner", &who(), Duration::from_millis(300))
        .await;
    assert_eq!(r2.approval_id.as_deref(), Some(id.as_str()));
    assert_eq!(fx.mock.created().len(), 1);
    fx.mock.approve(&id);
    let r3 = a
        .root_exec_wait(&p, "slow owner", &who(), Duration::from_secs(5))
        .await;
    assert_eq!(r3.outcome, RootOutcome::Done);
}

#[tokio::test]
async fn spawn_returns_at_once_and_reports_when_done() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let p = plan(&[&["echo", "bg"]]);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let s = a
        .root_spawn(
            p.clone(),
            "background".into(),
            who(),
            move |rep| async move {
                let _ = tx.send(rep);
            },
        )
        .await
        .unwrap();
    assert!(!s.already_waiting);
    // the same plan again while the waiter is alive: nothing new
    let s2 = a
        .root_spawn(p.clone(), "background".into(), who(), |_| async {})
        .await
        .unwrap();
    assert!(s2.already_waiting && s2.id == s.id);
    assert_eq!(fx.mock.created().len(), 1);
    fx.mock.approve(&s.id);
    let rep = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rep.outcome, RootOutcome::Done);
    assert_eq!(fx.sudo_calls(), vec!["<echo><bg>".to_owned()]);
}
