//! The `lucina-root` binary: reads lucina-rs-style env files, prints each
//! step, and exits non-zero on deny, expiry or failure.

use lucina_approvals::testing::Fixture;

fn env_file(fx: &Fixture) -> std::path::PathBuf {
    let c = &fx.cfg;
    let path = fx.dir.path().join("lucina.env");
    std::fs::write(
        &path,
        format!(
            "# test\nLUCINA_SECRETARY_SERVER_URL={}\nLUCINA_SECRETARY_SERVICE_TOKEN_FILE={}\nLUCINA_APPROVAL_PUBKEY_FILE={}\nLUCINA_APPROVAL_USED_FILE={}\nLUCINA_ROOT_SUDO={}\nLUCINA_ROOT_AUDIT_FILE={}\n",
            c.server_url.as_ref().unwrap(),
            c.token_file.as_ref().unwrap().display(),
            c.pubkey_file.display(),
            c.used_file.display(),
            c.root_sudo.display(),
            c.root_audit.display(),
        ),
    )
    .unwrap();
    path
}

async fn run(
    fx: &Fixture,
    plan: &str,
    decide: Option<fn(&Fixture, &str)>,
) -> (i32, String, String) {
    let envf = env_file(fx);
    let plan_path = fx.dir.path().join("plan.json");
    std::fs::write(&plan_path, plan).unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_lucina-root"))
        .args(["--reason", "cli test", "--wait-secs", "10", "--env-file"])
        .arg(&envf)
        .arg("--plan")
        .arg(&plan_path)
        .env_remove("LUCINA_SECRETARY_SERVER_URL")
        .env_remove("LUCINA_ROOT_SUDO")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(d) = decide {
        let ids = fx.mock.wait_created(1).await;
        d(fx, &ids[0]);
    }
    let out = child.wait_with_output().await.unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn approved_plan_prints_each_step_and_exits_zero() {
    let fx = Fixture::new().await;
    let (code, out, err) = run(
        &fx,
        r#"[["echo","hello world"],["true"]]"#,
        Some(|fx, id| {
            fx.mock.approve(id);
        }),
    )
    .await;
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("[1/2] echo 'hello world'  ->  ok"), "{out}");
    assert!(out.contains("hello world"), "{out}");
    assert!(out.contains("[2/2] true  ->  ok"), "{out}");
    assert_eq!(
        fx.sudo_calls(),
        vec!["<echo><hello world>".to_owned(), "<true>".to_owned()]
    );
    assert!(!err.contains("svc_"), "the token is never printed");
}

#[tokio::test]
async fn failure_deny_and_bad_usage_exit_non_zero() {
    let fx = Fixture::new().await;
    let (code, out, _) = run(
        &fx,
        r#"[["false"],["echo","no"]]"#,
        Some(|fx, id| {
            fx.mock.approve(id);
        }),
    )
    .await;
    assert_eq!(code, 1);
    assert!(
        out.contains("FAILED (exit 1)") && !out.contains("[2/2]"),
        "{out}"
    );

    let fx = Fixture::new().await;
    let (code, _, err) = run(&fx, r#"[["echo","x"]]"#, Some(|fx, id| fx.mock.deny(id))).await;
    assert_eq!(code, 3, "{err}");
    assert!(fx.sudo_calls().is_empty());

    let fx = Fixture::new().await;
    let (code, _, _) = run(&fx, r#"[["echo","x"]]"#, Some(|fx, id| fx.mock.expire(id))).await;
    assert_eq!(code, 4);

    let fx = Fixture::new().await;
    let (code, _, err) = run(&fx, r#"{"not":"a plan"}"#, None).await;
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = run(&fx, "[[]]", None).await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn dry_run_prints_what_the_owner_would_see_and_asks_nobody() {
    let fx = Fixture::new().await;
    let plan_path = fx.dir.path().join("plan.json");
    std::fs::write(&plan_path, r#"[["install","-m","644","/a b","/etc/x"]]"#).unwrap();
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_lucina-root"))
        .args(["--reason", "x", "--dry-run", "--plan"])
        .arg(&plan_path)
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("install -m 644 '/a b' /etc/x\nsha256:"),
        "{text}"
    );
    assert!(fx.mock.created().is_empty());
}
