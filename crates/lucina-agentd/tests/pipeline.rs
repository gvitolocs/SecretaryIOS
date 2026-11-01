//! The pipeline end to end through the manager: a hand-made worktree is adopted, its merge
//! is queued, the worker runs the real `lagent pipeline-exec` (as a child process), main
//! moves, and the workspace is archived with its worktree and branch. Leases too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lucina_agentd::lane::LaneConfig;
use lucina_agentd::manager::{AdoptTask, LeaseRequest, Manager, ManagerError, MergeRequest, Options, ReleaseRequest};
use lucina_agentd::pipeline::{Config, JobState};
use lucina_agentd::store::Store;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn manager(root: &Path) -> Manager {
    let options = Options {
        pipeline: Config {
            dir: root.join("state/pipeline"),
            lagent: PathBuf::from(env!("CARGO_BIN_EXE_lagent")),
            systemd: false,
            target_root: root.join("targets"),
            work_root: root.join("wt"),
        },
        ..Options::default()
    };
    Manager::open_with(Store::open(&root.join("state")).unwrap(), Vec::new(), LaneConfig::default(), options).unwrap()
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
    Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&origin).status().unwrap();
    Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&repo).status().unwrap();
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["checkout", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join(".lucina")).unwrap();
    std::fs::write(repo.join(".lucina/pipeline.toml"), "[merge]\ndiffgate = false\ncargo = \"off\"\n").unwrap();
    commit(&repo, "a.txt", "1\n", "a");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);

    // A worktree made by hand, with one commit.
    let wt = root.join("wt/Repo/topic");
    git(&repo, &["worktree", "add", "-q", "-b", "topic", &wt.display().to_string(), "origin/main"]);
    let head = commit(&wt, "b.txt", "b\n", "add b");

    let m = manager(root);
    let (task, w) = m.adopt_task(AdoptTask { path: wt.display().to_string(), title: Some("Add b".into()), owner: None, session: Some("local_test".into()) }).await.unwrap();
    assert_eq!((task.via.as_str(), task.branch.as_str(), task.trunk.as_str()), ("adopt", "topic", "main"));
    assert!(w.owned_worktree);

    // Uncommitted changes refuse the merge.
    std::fs::write(wt.join("scratch.txt"), "x").unwrap();
    let e = m.queue_merge(MergeRequest { workspace: w.workspace_id.clone(), ..MergeRequest::default() }).await.unwrap_err();
    assert!(matches!(e, ManagerError::Invalid(_)), "{e}");
    std::fs::remove_file(wt.join("scratch.txt")).unwrap();

    let job = m.queue_merge(MergeRequest { workspace: "topic".into(), requested_by: Some("local_test".into()), ..MergeRequest::default() }).await.unwrap();
    // Queued twice at the same commit: the same job.
    let again = m.queue_merge(MergeRequest { workspace: wt.display().to_string(), ..MergeRequest::default() }).await.unwrap();
    assert_eq!(again.job_id, job.job_id);

    let done = run_until_done(&m, &job.job_id).await;
    let outcome = done.outcome.clone().unwrap();
    assert_eq!(done.state, JobState::Succeeded, "{outcome:?}\n{}", m.job_log(&job.job_id, 0).unwrap().1);
    assert_eq!(outcome.code, "MERGED");
    // A rebase of one commit onto an unchanged main is that commit.
    assert_eq!(git(&origin, &["rev-parse", "main"]), head);
    assert_eq!(done.after.as_deref(), Some("archived: worktree and branch removed"));
    assert!(!wt.exists(), "the worktree is gone");
    assert!(git(&repo, &["branch", "--list", "topic"]).is_empty(), "the branch is gone");
    assert_eq!(m.pipeline().task(&w.workspace_id).unwrap().merged_sha.as_deref(), Some(head.as_str()));
    let log = m.job_log(&job.job_id, 0).unwrap().1;
    assert!(log.contains("MERGED"), "{log}");

    let board = m.board().await;
    assert_eq!(board["recentJobs"][0]["jobId"], job.job_id.as_str());
}

#[tokio::test]
async fn leases_are_exclusive_until_they_expire_or_are_released() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path());
    let req = |h: &str| LeaseRequest { holder: h.into(), purpose: Some("test".into()), minutes: Some(5), workspace_id: None };
    let l = m.lease_device("iphone", req("local_a")).unwrap();
    assert_eq!(l.holder, "local_a");
    assert!(matches!(m.lease_device("iphone", req("local_b")), Err(ManagerError::Busy(_))));
    // The holder renews it.
    assert_eq!(m.lease_device("iphone", req("local_a")).unwrap().acquired_at, l.acquired_at);
    assert!(matches!(m.lease_device("toaster", req("local_a")), Err(ManagerError::NotFound(_))));
    // Someone else releases only with --force and a reason.
    let rel = |h: &str, force: bool, reason: Option<&str>| ReleaseRequest { holder: h.into(), force, reason: reason.map(str::to_string) };
    assert!(matches!(m.release_device("iphone", rel("local_b", true, None)), Err(ManagerError::Forbidden(_))));
    assert_eq!(m.release_device("iphone", rel("local_b", true, Some("owner test"))).unwrap()["released"], true);
    assert_eq!(m.lease_device("iphone", req("local_b")).unwrap().holder, "local_b");
    assert_eq!(m.release_device("iphone", rel("local_b", false, None)).unwrap()["released"], true);
    assert!(m.leases().iter().all(|d| d["lease"].is_null()));
}
