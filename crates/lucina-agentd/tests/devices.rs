//! Installs on a leased device through the manager: a fake executor stands in for the Mac
//! (`ios-install executor`), fake `deploy-guard` and `lucina-rs decide` record what they
//! were asked. Main installs take only origin/main's tip and are recorded; test builds need
//! the caller's lease and the owner's own approval; grants follow the lease.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lucina_agentd::devices::{
    Config as DevConfig, DevError, DeviceSpec, GrantDone, GrantRequest, InstallRequest,
};
use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{LeaseRequest, Manager, Options, ReleaseRequest};
use lucina_agentd::store::Store;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), msg).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

/// The same helper the unit tests use, compiled from one file rather than copied.
#[path = "../src/testutil.rs"]
mod testutil;

fn script(path: &Path, body: &str) {
    testutil::write_exec(path, &format!("#!/bin/sh\n{body}\n"));
}

fn lease(m: &Manager, holder: &str) {
    m.lease_device(
        "iphone",
        LeaseRequest {
            holder: holder.into(),
            purpose: Some("owner test".into()),
            minutes: Some(30),
            workspace_id: None,
        },
    )
    .unwrap();
}

fn release(m: &Manager, holder: &str) {
    m.release_device(
        "iphone",
        ReleaseRequest {
            holder: holder.into(),
            force: false,
            reason: None,
        },
    )
    .unwrap();
}

fn install(holder: &str) -> InstallRequest {
    InstallRequest {
        holder: holder.into(),
        ..InstallRequest::default()
    }
}

fn refused(r: Result<serde_json::Value, DevError>, expect: &str) {
    match r {
        Err(DevError::Refused(why)) => {
            assert!(why.join("; ").contains(expect), "{why:?} lacks {expect:?}")
        }
        other => panic!("expected a refusal with {expect:?}, got {other:?}"),
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[tokio::test]
async fn installs_follow_main_the_lease_and_the_owners_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let origin = root.join("origin.git");
    let repo = root.join("SecretaryIOS");
    Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&origin)
        .status()
        .unwrap();
    Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&repo)
        .status()
        .unwrap();
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["checkout", "-q", "-b", "main"]);
    let a = commit(&repo, "a", "first");
    git(&repo, &["push", "-q", "origin", "main"]);

    // Fakes: the executor prints its result, deploy-guard and decide log what they were asked.
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = root.join("calls");
    script(&bin.join("executor"), &format!("echo \"$@\" >> {0}/executor\nif [ -f {0}/fail ]; then echo 'IOS_INSTALL_RESULT {{\"ok\":false,\"error\":\"devicectl failed\"}}'; exit 3; fi\necho 'IOS_INSTALL_RESULT {{\"ok\":true,\"appex\":\"SecretaryPhotoUpload.appex,SecretaryWidgets.appex\"}}'", root.display()));
    script(
        &bin.join("deploy-guard"),
        &format!("echo \"$@\" >> {}\nexit 0", calls.display()),
    );
    let decisions = root.join("decisions");
    std::fs::create_dir_all(&decisions).unwrap();
    script(
        &bin.join("lucina-rs"),
        &format!(
            "cat {}/$3.json || {{ echo 'Error: no such decision' >&2; exit 1; }}",
            decisions.display()
        ),
    );
    std::env::set_var("DEPLOY_GUARD_LEDGER", root.join("no-ledger"));

    let options = Options {
        devices: DevConfig {
            dir: root.join("state/devices"),
            devices: vec![DeviceSpec {
                name: "iphone".into(),
                label: "iPhone".into(),
                hosts: vec!["local".into()],
                executor: bin.join("executor").display().to_string(),
                repo: repo.clone(),
                runtime: "ios".into(),
            }],
            deploy_guard: bin.join("deploy-guard").display().to_string(),
            decide: bin.join("lucina-rs").display().to_string(),
            install_timeout: Duration::from_secs(60),
        },
        ..Options::default()
    };
    let m = Manager::open_with(
        Store::open(&root.join("state")).unwrap(),
        Vec::new(),
        LaneConfig::default(),
        options,
    )
    .unwrap();
    let d = m.devices();

    // A main install of the tip: checked and recorded by deploy-guard, its own lease given back.
    let v = d.install(&m, "iphone", &install("local_s1")).await.unwrap();
    assert_eq!(v["commit"], a.as_str());
    assert_eq!(v["appex"][0], "SecretaryPhotoUpload.appex");
    let log = std::fs::read_to_string(&calls).unwrap();
    assert!(log.contains("check --runtime ios --no-lock-check"), "{log}");
    assert!(
        log.contains(&format!("record --runtime ios --commit {a}")),
        "{log}"
    );
    assert!(
        m.pipeline().lease("iphone").is_none(),
        "the install's own lease is released"
    );
    assert_eq!(d.installed("iphone").unwrap().kind.as_str(), "main");

    // Someone else's lease refuses it; an older main commit is refused too.
    lease(&m, "local_s2");
    refused(
        d.install(&m, "iphone", &install("local_s1")).await,
        "leased to local_s2",
    );
    let b = commit(&repo, "b", "second");
    git(&repo, &["push", "-q", "origin", "main"]);
    let old = InstallRequest {
        commit: Some(a.clone()),
        ..install("local_s2")
    };
    refused(d.install(&m, "iphone", &old).await, "not origin/main's tip");

    // A test build of a branch: lease + the owner's own go naming the commit.
    git(&repo, &["checkout", "-q", "-b", "ig-gallery-paging"]);
    let c = commit(&repo, "c", "branch work");
    git(&repo, &["push", "-q", "origin", "ig-gallery-paging"]);
    let test = |approval: Option<&str>| InstallRequest {
        test: true,
        commit: Some(c.clone()),
        approval: approval.map(String::from),
        ..install("local_s2")
    };
    refused(
        d.install(&m, "iphone", &test(None)).await,
        "needs --approval",
    );
    let decision = |id: &str, by: &str, q: &str| {
        let j = serde_json::json!({"status": "decided", "decision": "go", "decided_by": by, "decided_at": now_iso(), "question": q});
        std::fs::write(decisions.join(format!("{id}.json")), j.to_string()).unwrap();
    };
    decision(
        "dc-00000000000a",
        "lucina",
        &format!("Install {}?", &c[..8]),
    );
    refused(
        d.install(&m, "iphone", &test(Some("dc-00000000000a")))
            .await,
        "decided by lucina",
    );
    decision("dc-00000000000b", "owner", "Install the branch?");
    refused(
        d.install(&m, "iphone", &test(Some("dc-00000000000b")))
            .await,
        "does not name",
    );
    decision(
        "dc-00000000000c",
        "owner",
        &format!("Install test build {} on the iPhone?", &c[..8]),
    );
    let records_before = std::fs::read_to_string(&calls)
        .unwrap()
        .matches("record")
        .count();
    let v = d
        .install(&m, "iphone", &test(Some("dc-00000000000c")))
        .await
        .unwrap();
    assert_eq!(v["kind"], "test");
    assert_eq!(
        std::fs::read_to_string(&calls)
            .unwrap()
            .matches("record")
            .count(),
        records_before,
        "a test build never reaches the ledger"
    );
    assert_eq!(
        d.installed("iphone").unwrap().approval.as_deref(),
        Some("dc-00000000000c")
    );
    assert!(
        m.pipeline().lease("iphone").is_some(),
        "the caller's own lease stays"
    );
    // Not the lease holder: refused even with the approval.
    let other = InstallRequest {
        holder: "local_s1".into(),
        ..test(Some("dc-00000000000c"))
    };
    refused(d.install(&m, "iphone", &other).await, "leased to local_s2");

    // A test-run grant follows the lease and is used once.
    let g = d
        .grant(
            &m,
            "iphone",
            &GrantRequest {
                holder: "local_s2".into(),
                commit: b.clone(),
                ..GrantRequest::default()
            },
        )
        .await
        .unwrap();
    let id = g["grant"]["id"].as_str().unwrap().to_string();
    assert_eq!(g["requireCleanTip"], true);
    d.grant_check(&m, "iphone", &id).unwrap();
    release(&m, "local_s2");
    assert!(
        matches!(d.grant_check(&m, "iphone", &id), Err(DevError::Refused(w)) if w[0].contains("no longer holds"))
    );
    lease(&m, "local_s2");
    d.grant_done(
        "iphone",
        &id,
        &GrantDone {
            exit: 0,
            command: "xcodebuild test".into(),
            ..GrantDone::default()
        },
    )
    .unwrap();
    assert!(d.grant_check(&m, "iphone", &id).is_err(), "used once");
    assert_eq!(d.installed("iphone").unwrap().kind.as_str(), "test-run");

    // An executor failure is reported and nothing is recorded.
    release(&m, "local_s2");
    std::fs::write(root.join("fail"), "").unwrap();
    refused(
        d.install(&m, "iphone", &install("local_s1")).await,
        "devicectl failed",
    );
    assert_eq!(d.installed("iphone").unwrap().kind.as_str(), "test-run");
    assert!(
        m.pipeline().lease("iphone").is_none(),
        "the failed install's lease is released"
    );
    let hist = d.history_tail("iphone", 50).unwrap();
    assert!(hist["history"].as_array().unwrap().len() >= 4);
}
