//! The pipeline end to end through the manager: a hand-made worktree is adopted, its merge
//! is queued, the worker runs the real `lagent pipeline-exec` (as a child process), main
//! moves, and the workspace is archived with its worktree and branch. Leases too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{
    AdoptTask, LeaseRequest, Manager, ManagerError, MergeRequest, Options, ReleaseRequest,
    StartTask,
};
use lucina_agentd::pipeline::{Config, JobState};
use lucina_agentd::store::Store;

/// The same helper the unit tests use, compiled from one file rather than copied.
#[path = "../src/testutil.rs"]
mod testutil;

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

fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn manager(root: &Path) -> Manager {
    manager_with_wt_root(root, &root.join("wt"))
}

fn manager_with_wt_root(root: &Path, wt_root: &Path) -> Manager {
    let mut workspaces = lucina_agentd::workspaces::Config::from_env();
    workspaces.wt_root = wt_root.to_path_buf();
    let options = Options {
        workspaces,
        pipeline: Config {
            dir: root.join("state/pipeline"),
            lagent: PathBuf::from(env!("CARGO_BIN_EXE_lagent")),
            systemd: false,
            target_root: root.join("targets"),
            work_root: root.join("wt"),
            // Not the real lock: the gate running these tests may hold it.
            lock: root.join("merge.lock"),
        },
        ..Options::default()
    };
    Manager::open_with(
        Store::open(&root.join("state")).unwrap(),
        Vec::new(),
        LaneConfig::default(),
        options,
    )
    .unwrap()
}

async fn run_until_done(m: &Manager, id: &str) -> lucina_agentd::pipeline::Job {
    for _ in 0..200 {
        m.pipeline_tick().await;
        let j = m.job(id).unwrap();
        if j.state.done() {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} did not finish: {:?}", m.job(id).unwrap());
}

#[tokio::test]
async fn an_adopted_worktree_merges_through_the_queue_and_is_archived() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let origin = root.join("origin.git");
    let repo = root.join("Repo");
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
    std::fs::create_dir_all(repo.join(".lucina")).unwrap();
    std::fs::write(
        repo.join(".lucina/pipeline.toml"),
        "[merge]\ndiffgate = false\ncargo = \"off\"\n",
    )
    .unwrap();
    commit(&repo, "a.txt", "1\n", "a");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);

    // A worktree made by hand, with one commit.
    let wt = root.join("wt/Repo/topic");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "topic",
            &wt.display().to_string(),
            "origin/main",
        ],
    );
    let head = commit(&wt, "b.txt", "b\n", "add b");

    let m = manager(root);
    let (task, w) = m
        .adopt_task(AdoptTask {
            path: wt.display().to_string(),
            title: Some("Add b".into()),
            owner: None,
            session: Some("local_test".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        (task.via.as_str(), task.branch.as_str(), task.trunk.as_str()),
        ("adopt", "topic", "main")
    );
    assert!(w.owned_worktree);
    // Adopting again gives the task to the session asking; nothing else changes.
    let (again, same) = m
        .adopt_task(AdoptTask {
            path: wt.display().to_string(),
            session: Some("local_back".into()),
            ..AdoptTask::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (again.session.as_deref(), same.workspace_id.as_str()),
        (Some("local_back"), w.workspace_id.as_str())
    );

    // Uncommitted changes refuse the merge.
    std::fs::write(wt.join("scratch.txt"), "x").unwrap();
    let e = m
        .queue_merge(MergeRequest {
            workspace: w.workspace_id.clone(),
            ..MergeRequest::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(e, ManagerError::Invalid(_)), "{e}");
    std::fs::remove_file(wt.join("scratch.txt")).unwrap();

    let job = m
        .queue_merge(MergeRequest {
            workspace: "topic".into(),
            requested_by: Some("local_test".into()),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    // Queued twice at the same commit: the same job.
    let again = m
        .queue_merge(MergeRequest {
            workspace: wt.display().to_string(),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(again.job_id, job.job_id);

    let done = run_until_done(&m, &job.job_id).await;
    let outcome = done.outcome.clone().unwrap();
    assert_eq!(
        done.state,
        JobState::Succeeded,
        "{outcome:?}\n{}",
        m.job_log(&job.job_id, 0).unwrap().1
    );
    assert_eq!(outcome.code, "MERGED");
    // Always a merge commit, also onto an unchanged main: the branch's commit is its second parent.
    let landed = git(&origin, &["rev-parse", "main"]);
    assert_ne!(landed, head);
    assert_eq!(git(&origin, &["rev-parse", "main^2"]), head);
    assert_eq!(outcome.method.as_deref(), Some("merge"));
    assert_eq!(
        done.after.as_deref(),
        Some("archived: worktree and branch removed")
    );
    assert!(!wt.exists(), "the worktree is gone");
    assert!(
        git(&repo, &["branch", "--list", "topic"]).is_empty(),
        "the branch is gone"
    );
    assert_eq!(
        m.pipeline()
            .task(&w.workspace_id)
            .unwrap()
            .merged_sha
            .as_deref(),
        Some(landed.as_str())
    );
    let log = m.job_log(&job.job_id, 0).unwrap().1;
    assert!(log.contains("MERGED"), "{log}");

    let board = m.board().await;
    assert_eq!(board["recentJobs"][0]["jobId"], job.job_id.as_str());
}

#[tokio::test]
async fn a_moved_checkout_keeps_its_workspaces_and_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let old = root.join("Old");
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&old)
        .status()
        .unwrap();
    git(&old, &["config", "user.email", "t@t"]);
    git(&old, &["config", "user.name", "t"]);
    commit(&old, "a.txt", "1\n", "a");
    let m = manager_with_wt_root(&root, &root.join("wt"));
    let wt = root.join("wt/Old/topic");
    git(
        &old,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "topic",
            &wt.display().to_string(),
        ],
    );
    let stray = root.join("wt/Old/stray");
    git(
        &old,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "stray",
            &stray.display().to_string(),
        ],
    );
    let (task, w) = m
        .adopt_task(AdoptTask {
            path: wt.display().to_string(),
            ..AdoptTask::default()
        })
        .await
        .unwrap();
    let (_, kept) = m
        .adopt_task(AdoptTask {
            path: stray.display().to_string(),
            ..AdoptTask::default()
        })
        .await
        .unwrap();

    // Moved on disk, but `stray` was left out of the new worktrees folder.
    let new = root.join("New");
    std::fs::rename(&old, &new).unwrap();
    std::fs::create_dir_all(root.join("wt/New")).unwrap();
    let wt2 = root.join("wt/New/topic");
    std::fs::rename(&wt, &wt2).unwrap();
    git(&new, &["worktree", "repair", &wt2.display().to_string()]);

    let bad = m
        .move_project(&w.project_id, root.join("nowhere").to_str().unwrap(), None)
        .unwrap_err();
    assert!(matches!(bad, ManagerError::Workspace(_)), "{bad}");
    let v = m
        .move_project(&w.project_id, new.to_str().unwrap(), Some("Renamed".into()))
        .unwrap();
    assert_eq!(v["project"]["rootPath"], new.display().to_string());
    assert_eq!(v["moved"][0]["workspaceId"], w.workspace_id.as_str());
    assert_eq!(v["skipped"][0]["workspaceId"], kept.workspace_id.as_str());
    let after = m.workspaces().get(&w.workspace_id).unwrap();
    assert_eq!(after.cwd, wt2.display().to_string());
    assert_eq!(after.main_repo_root.as_deref(), Some(new.to_str().unwrap()));
    assert_eq!(
        m.workspaces().get(&kept.workspace_id).unwrap().cwd,
        stray.display().to_string()
    );
    assert_eq!(
        m.pipeline().task(&task.workspace_id).unwrap().repo,
        new.display().to_string()
    );
    assert_eq!(
        m.workspaces().project(&w.project_id).unwrap().name(),
        "Renamed"
    );
}

/// A repo with an origin, whose merge gates are `gate`, and a worktree of `topic` with one commit.
fn repo_with_gate(root: &Path, name: &str, gate: &str) -> PathBuf {
    let origin = root.join(format!("{name}.git"));
    let repo = root.join(name);
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
    std::fs::create_dir_all(repo.join(".lucina")).unwrap();
    std::fs::write(repo.join(".lucina/pipeline.toml"), format!("[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"g\"\nrun = \"{gate}\"\n")).unwrap();
    commit(&repo, "a.txt", "1\n", "a");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    repo
}

fn topic(root: &Path, repo: &Path, name: &str, branch: &str) -> PathBuf {
    let wt = root.join("wt").join(name).join(branch);
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            &wt.display().to_string(),
            "origin/main",
        ],
    );
    commit(&wt, &format!("{branch}.txt"), "x\n", branch);
    wt
}

#[tokio::test]
async fn a_long_gate_holds_up_only_its_own_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let m = manager(&root);
    let slow = repo_with_gate(&root, "Slow", "sleep 8");
    let fast = repo_with_gate(&root, "Fast", "true");
    let queue = |wt: PathBuf| {
        let m = m.clone();
        async move {
            m.adopt_task(AdoptTask {
                path: wt.display().to_string(),
                ..AdoptTask::default()
            })
            .await
            .unwrap();
            m.queue_merge(MergeRequest {
                workspace: wt.display().to_string(),
                ..MergeRequest::default()
            })
            .await
            .unwrap()
        }
    };
    let long = queue(topic(&root, &slow, "Slow", "one")).await;
    let waits = queue(topic(&root, &slow, "Slow", "two")).await;
    let urgent = queue(topic(&root, &slow, "Slow", "three")).await;
    let other = queue(topic(&root, &fast, "Fast", "four")).await;
    m.pipeline_tick().await;
    m.prioritize_job(&urgent.job_id).unwrap();
    // The other repo's job finishes while the slow repo's first is still at its gate.
    let done = run_until_done(&m, &other.job_id).await;
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.outcome);
    assert_eq!(m.job(&long.job_id).unwrap().state, JobState::Running);
    assert_eq!(
        m.job(&waits.job_id).unwrap().state,
        JobState::Queued,
        "one at a time per repo"
    );
    // `job next` goes ahead of the older queued job of its repo.
    let u = run_until_done(&m, &long.job_id).await;
    assert_eq!(u.state, JobState::Succeeded, "{:?}", u.outcome);
    m.pipeline_tick().await;
    assert_eq!(m.job(&urgent.job_id).unwrap().state, JobState::Running);
    assert_eq!(m.job(&waits.job_id).unwrap().state, JobState::Queued);
    assert!(
        matches!(
            m.prioritize_job(&long.job_id),
            Err(ManagerError::Invalid(_))
        ),
        "only a queued job"
    );
    let _ = m.cancel_job(&urgent.job_id).await;
    let _ = m.cancel_job(&waits.job_id).await;
}

#[tokio::test]
async fn a_flaw_task_records_its_merge_on_the_flaw() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    // Stands in for `lucina-rs flaws`: what it was called with goes in a file.
    let calls = root.join("calls.txt");
    let bin = root.join("fake-lucina-rs");
    testutil::write_exec(
        &bin,
        &format!("#!/bin/sh\necho \"$@\" >> {}\n", calls.display()),
    );
    std::env::set_var("LUCINA_AGENTD_LUCINA_RS", &bin);
    let m = manager(&root);
    let repo = repo_with_gate(&root, "Flawed", "true");
    let (task, w) = m
        .start_task(StartTask {
            title: "Flaw fix".into(),
            repo: Some(repo.display().to_string()),
            branch: Some("flaw-fix".into()),
            flaw: Some("fl-0123abcd".into()),
            flaw_target: Some("web".into()),
            ..StartTask::default()
        })
        .await
        .unwrap();
    assert_eq!(task.flaw.as_deref(), Some("fl-0123abcd"));
    let wt = PathBuf::from(&w.cwd);
    git(&wt, &["config", "user.email", "t@t"]);
    git(&wt, &["config", "user.name", "t"]);
    commit(&wt, "fix.txt", "fixed\n", "the fix");
    let job = m
        .queue_merge(MergeRequest {
            workspace: w.workspace_id.clone(),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    let done = run_until_done(&m, &job.job_id).await;
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.outcome);
    let landed = done.outcome.unwrap().landed.unwrap();
    let said = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(
        said.trim(),
        format!("flaws merged fl-0123abcd web {landed}")
    );
}

#[tokio::test]
async fn leases_are_exclusive_until_they_expire_or_are_released() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());
    let req = |h: &str| LeaseRequest {
        holder: h.into(),
        purpose: Some("test".into()),
        minutes: Some(5),
        workspace_id: None,
    };
    let l = m.lease_device("iphone", req("local_a")).unwrap();
    assert_eq!(l.holder, "local_a");
    assert!(matches!(
        m.lease_device("iphone", req("local_b")),
        Err(ManagerError::Busy(_))
    ));
    // The holder renews it.
    assert_eq!(
        m.lease_device("iphone", req("local_a"))
            .unwrap()
            .acquired_at,
        l.acquired_at
    );
    assert!(matches!(
        m.lease_device("toaster", req("local_a")),
        Err(ManagerError::NotFound(_))
    ));
    // Someone else releases only with --force and a reason.
    let rel = |h: &str, force: bool, reason: Option<&str>| ReleaseRequest {
        holder: h.into(),
        force,
        reason: reason.map(str::to_string),
    };
    assert!(matches!(
        m.release_device("iphone", rel("local_b", true, None)),
        Err(ManagerError::Forbidden(_))
    ));
    assert_eq!(
        m.release_device("iphone", rel("local_b", true, Some("owner test")))
            .unwrap()["released"],
        true
    );
    assert_eq!(
        m.lease_device("iphone", req("local_b")).unwrap().holder,
        "local_b"
    );
    assert_eq!(
        m.release_device("iphone", rel("local_b", false, None))
            .unwrap()["released"],
        true
    );
    assert!(m.leases().iter().all(|d| d["lease"].is_null()));
}

/// The LucinaNet-servo shape: `origin` is the upstream servo (its `main` is the mirror's HEAD,
/// and its push URL refuses), while the fork's trunk is the local branch `lucinanet`, which
/// origin has no branch for. The checkout is named as the compiled table knows it.
fn servo_repo(root: &Path) -> (PathBuf, PathBuf) {
    let origin = root.join("LucinaNet-servo.git");
    let repo = root.join("LucinaNet-servo");
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
    std::fs::create_dir_all(repo.join(".lucina")).unwrap();
    // The file names `main`: what sent merges to the mirror before the table knew this repo.
    std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\ncargo = \"off\"\ntrunk = \"main\"\n[[gate]]\nname = \"g\"\nrun = \"true\"\n").unwrap();
    commit(&repo, "a.txt", "1\n", "a");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["checkout", "-q", "-b", "lucinanet"]);
    commit(&repo, "fork.txt", "ours\n", "the fork's own work");
    git(&repo, &["config", "remote.origin.pushurl", "no-push://x"]);
    (origin, repo)
}

#[tokio::test]
async fn a_table_trunk_merges_on_the_local_branch_and_pushes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (origin, repo) = servo_repo(&root);
    let mirror = git(&origin, &["rev-parse", "main"]);
    let trunk = git(&repo, &["rev-parse", "lucinanet"]);
    let m = manager(&root);
    let (task, w) = m
        .start_task(StartTask {
            title: "Servo fix".into(),
            repo: Some(repo.display().to_string()),
            ..StartTask::default()
        })
        .await
        .unwrap();
    assert_eq!(
        task.trunk, "lucinanet",
        "the table's trunk, not the mirror or the file's"
    );
    let wt = PathBuf::from(&w.cwd);
    assert_eq!(w.base_ref.as_deref(), Some("refs/heads/lucinanet"));
    assert_eq!(
        git(&wt, &["rev-parse", "HEAD"]),
        trunk,
        "the workspace branches off the local trunk"
    );
    assert!(
        wt.join("fork.txt").is_file(),
        "which is ahead of the mirror origin has"
    );
    git(&wt, &["config", "user.email", "t@t"]);
    git(&wt, &["config", "user.name", "t"]);
    let head = commit(&wt, "fix.txt", "fixed\n", "the fix");
    // A trunk in the repo's git config is not read by anything.
    git(&repo, &["config", "lucina.trunk", "main"]);
    let job = m
        .queue_merge(MergeRequest {
            workspace: w.workspace_id.clone(),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    let done = run_until_done(&m, &job.job_id).await;
    let out = done.outcome.clone().unwrap();
    assert_eq!(
        done.state,
        JobState::Succeeded,
        "{out:?}\n{}",
        m.job_log(&job.job_id, 0).unwrap().1
    );
    assert_eq!(
        (out.code.as_str(), out.method.as_deref()),
        ("MERGED", Some("merge"))
    );
    assert_eq!(out.gates.len(), 1, "the trunk's gates ran as usual");
    let landed = git(&repo, &["rev-parse", "lucinanet"]);
    assert_eq!(out.landed.as_deref(), Some(landed.as_str()));
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{landed}^2")]),
        head,
        "the branch's commit is the merge's second parent"
    );
    assert!(
        git(&repo, &["log", "--format=%s", "-1", "lucinanet"])
            .starts_with(&format!("Merge {}: ", task.branch)),
        "a merge commit naming the branch"
    );
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        mirror,
        "the mirror did not move"
    );
    assert!(
        git(&origin, &["branch", "--list", "lucinanet"]).is_empty(),
        "nothing was pushed"
    );
    assert_eq!(
        git(&repo, &["config", "--get", "remote.origin.pushurl"]),
        "no-push://x"
    );
    // The report names the trunk it landed on.
    assert!(out.message.contains("lucinanet"), "{}", out.message);
    let log = m.job_log(&job.job_id, 0).unwrap().1;
    assert!(log.contains("into lucinanet"), "{log}");
    assert_eq!(
        done.after.as_deref(),
        Some("archived: worktree and branch removed")
    );
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]),
        landed,
        "the checkout the trunk is in followed it"
    );
}

#[tokio::test]
async fn a_conflict_is_reported_against_the_local_trunk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (origin, repo) = servo_repo(&root);
    let mirror = git(&origin, &["rev-parse", "main"]);
    let trunk = git(&repo, &["rev-parse", "lucinanet"]);
    let m = manager(&root);
    // A branch off the mirror that edits what the fork's own commit added: it clashes with
    // `lucinanet`, and with nothing else.
    let wt = root.join("wt/LucinaNet-servo/clash");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "clash",
            &wt.display().to_string(),
            "main",
        ],
    );
    commit(&wt, "fork.txt", "theirs\n", "clash");
    let (task, _) = m
        .adopt_task(AdoptTask {
            path: wt.display().to_string(),
            ..AdoptTask::default()
        })
        .await
        .unwrap();
    assert_eq!(task.trunk, "lucinanet");
    let job = m
        .queue_merge(MergeRequest {
            workspace: "clash".into(),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    let done = run_until_done(&m, &job.job_id).await;
    let out = done.outcome.clone().unwrap();
    assert_eq!(out.code, "CONFLICT", "{out:?}");
    assert_eq!(out.conflicts, vec!["fork.txt".to_string()]);
    assert!(
        out.message
            .starts_with("clash conflicts with lucinanet: merge lucinanet into the branch"),
        "{}",
        out.message
    );
    assert_eq!(
        git(&repo, &["rev-parse", "lucinanet"]),
        trunk,
        "the trunk did not move"
    );
    assert_eq!(git(&origin, &["rev-parse", "main"]), mirror);
}

#[tokio::test]
async fn the_board_measures_a_worktree_with_no_base_against_the_local_trunk() {
    use lucina_agentd::workspaces::CreateWorkspace;
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (_, repo) = servo_repo(&root);
    // A worktree of the mirror's tip, registered without a base of its own: the board measures
    // it against the repo's trunk, which is one commit ahead.
    let wt = root.join("wt/LucinaNet-servo/old");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "old",
            &wt.display().to_string(),
            "main",
        ],
    );
    let m = manager(&root);
    let (w, _) = m
        .workspaces()
        .create(CreateWorkspace {
            isolation: "local".into(),
            path: Some(wt.display().to_string()),
            ..CreateWorkspace::default()
        })
        .await
        .unwrap();
    assert!(
        w.base_ref.is_none(),
        "the board takes its base from the repo"
    );
    let board = m.board().await;
    let row = board["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["cwd"] == wt.display().to_string())
        .expect("the worktree is on the board");
    assert_eq!(
        (row["ahead"].as_u64(), row["behind"].as_u64()),
        (Some(0), Some(1)),
        "{row}"
    );
    assert_eq!(row["state"], "merged");
}

/// A repo the table does not mark local lands through its remote: a trunk the remote does not
/// carry is a refused merge, and nothing moves anywhere.
#[tokio::test]
async fn a_merge_to_a_trunk_the_remote_does_not_carry_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = repo_with_gate(&root, "Plain", "true");
    let origin = root.join("Plain.git");
    let m = manager(&root);
    let (task, w) = m
        .start_task(StartTask {
            title: "Add b".into(),
            repo: Some(repo.display().to_string()),
            ..StartTask::default()
        })
        .await
        .unwrap();
    assert_eq!(task.trunk, "main", "the branch origin does have");
    let wt = PathBuf::from(&w.cwd);
    git(&wt, &["config", "user.email", "t@t"]);
    git(&wt, &["config", "user.name", "t"]);
    let head = commit(&wt, "b.txt", "b\n", "b");
    let rules = "trunk = \"release\"\n[merge]\ndiffgate = false\ncargo = \"off\"\n[[gate]]\nname = \"g\"\nrun = \"true\"\n";
    let main_before = git(&repo, &["rev-parse", "main"]);
    // The trunk's file now names `release`, which the remote never gets: only a local branch of
    // that name exists, and a local branch is not where this merge may land.
    commit(
        &repo,
        ".lucina/pipeline.toml",
        rules,
        "the trunk moves to release",
    );
    let main = git(&repo, &["rev-parse", "main"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["branch", "release", &main_before]);
    let job = m
        .queue_merge(MergeRequest {
            workspace: w.workspace_id.clone(),
            ..MergeRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(job.spec.trunk, "release");
    assert_eq!(job.spec.remote.as_deref(), Some("origin"));
    let done = run_until_done(&m, &job.job_id).await;
    let out = done.outcome.clone().unwrap();
    assert_eq!(done.state, JobState::Failed, "{done:?}");
    assert_eq!(out.code, "FETCH_FAILED", "{out:?}");
    assert!(
        out.message.contains("origin/release does not exist"),
        "{}",
        out.message
    );
    assert!(
        out.message.contains("the remote does not carry the trunk"),
        "{}",
        out.message
    );
    assert!(out.landed.is_none(), "nothing landed: {out:?}");
    assert_eq!(git(&repo, &["rev-parse", "main"]), main, "main unmoved");
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        main,
        "nothing was pushed"
    );
    assert!(
        git(&origin, &["branch", "--list", "release"]).is_empty(),
        "origin still has no release"
    );
    assert_eq!(
        git(&repo, &["rev-parse", "release"]),
        main_before,
        "the local branch of the trunk's name was not landed on either"
    );
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head, "the work is kept");
}

/// `lagent task start --base`: a commit on the trunk is taken, a commit off it is refused
/// before anything is made, and with no `--base` the task still starts from the trunk tip.
#[tokio::test]
async fn a_task_can_be_based_on_a_trunk_commit_and_on_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = repo_with_gate(&root, "Baseable", "true");
    // Two commits on the trunk, and a third on a branch nothing on the trunk leads to.
    let older = git(&repo, &["rev-parse", "HEAD"]);
    let tip = commit(&repo, "b.txt", "b\n", "b");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["checkout", "-q", "-b", "side", &older]);
    let side = commit(&repo, "side.txt", "side\n", "the side commit");
    git(&repo, &["checkout", "-q", "main"]);

    let m = manager(&root);
    let start = |title: &str, branch: &str, base: Option<&str>| {
        let m = m.clone();
        let title = title.to_string();
        let repo = repo.display().to_string();
        let branch = branch.to_string();
        let base = base.map(str::to_string);
        async move {
            m.start_task(StartTask {
                title,
                repo: Some(repo),
                branch: Some(branch),
                base,
                ..StartTask::default()
            })
            .await
        }
    };

    // With no base asked for: the trunk tip, and nothing recorded about a base.
    let (task, w) = start("Plain start", "no-base", None).await.unwrap();
    assert_eq!(git(&PathBuf::from(&w.cwd), &["rev-parse", "HEAD"]), tip);
    assert_eq!(task.base, None, "no base asked for, none recorded");
    assert_eq!(task.base_ref, None);

    // An older commit of the trunk, named by an abbreviation: the worktree is made at it, and
    // the record keeps both the full id and what was typed.
    let short = older[..10].to_string();
    let (task, w) = start("From the older commit", "older-base", Some(short.as_str()))
        .await
        .unwrap();
    assert_eq!(
        git(&PathBuf::from(&w.cwd), &["rev-parse", "HEAD"]),
        older,
        "the worktree starts at the commit asked for"
    );
    assert_eq!(task.base.as_deref(), Some(older.as_str()));
    assert_eq!(task.base_ref.as_deref(), Some(short.as_str()));
    assert_eq!(
        m.pipeline().task(&w.workspace_id),
        Some(task),
        "the stored record is the one handed back"
    );

    // A commit the trunk does not have: refused, and nothing was made for it.
    let trees = git(&repo, &["worktree", "list", "--porcelain"]);
    let ManagerError::Invalid(msg) =
        start("From the side commit", "side-base", Some(side.as_str()))
            .await
            .expect_err("the side commit is no base")
    else {
        panic!("a base off the trunk is a refused request, not another error");
    };
    assert_eq!(
        msg,
        format!(
            "base {} is not on origin/main nor on an allowed base of Baseable; allowed: none",
            &side[..10]
        )
    );
    assert!(
        git(&repo, &["branch", "--list", "side-base"]).is_empty(),
        "no branch was made"
    );
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"]),
        trees,
        "no worktree was made"
    );
    assert!(m.pipeline().tasks().iter().all(|t| t.branch != "side-base"));

    // A word that is no commit here at all: refused the same way, before anything is measured.
    let ManagerError::Invalid(msg) = start("Nonsense", "words-base", Some("not-a-rev"))
        .await
        .expect_err("not-a-rev is no commit")
    else {
        panic!("a base that is no commit is a refused request");
    };
    assert_eq!(msg, "base not-a-rev is not a commit in Baseable");
    assert!(git(&repo, &["branch", "--list", "words-base"]).is_empty());
}
