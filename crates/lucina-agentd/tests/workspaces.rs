//! Workspaces end to end with the owner's real `wt` (skipped where it isn't installed),
//! on a temporary projects root and worktrees root: the worktree guard, worktrees made
//! from a prompt with `paseo.json` setup and a generated name, diffs, files, archive,
//! the merge sweep, checkout-branch copies, projects and clone.

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::{json, Value};

const WT: &str = "/home/nez/.local/bin/wt";

struct Env {
    root: tempfile::TempDir,
    d: Daemon,
}

impl Env {
    fn start() -> Option<Env> {
        let wt = std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/bin/wt")).unwrap_or_else(|_| PathBuf::from(WT));
        if !wt.exists() {
            eprintln!("workspaces: skipped (no wt at {})", wt.display());
            return None;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj")).unwrap();
        let d = Daemon::start_with_env(&[
            ("WT_ROOT", root.path().join("wt").display().to_string()),
            ("WT_PROJECTS", root.path().join("proj").display().to_string()),
            ("LUCINA_AGENTD_WT", wt.display().to_string()),
            ("LUCINA_AGENTD_METADATA_PROVIDER", "mock/e2e-fast-stream".into()),
        ]);
        Some(Env { root, d })
    }

    /// A repo `proj/<name>` (protected: under the projects root) with one commit.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let r = self.root.path().join("proj").join(name);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("README"), "hi\n").unwrap();
        for (p, t) in files {
            std::fs::write(r.join(p), t).unwrap();
        }
        git(&r, &["add", "."]);
        commit(&r, "init");
        r
    }

    fn wt_root(&self) -> PathBuf {
        self.root.path().join("wt")
    }

    fn http(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let mut s = UnixStream::connect(self.d.socket()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        s.write_all(req.as_bytes()).unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let raw = String::from_utf8_lossy(&raw).to_string();
        let (head, rest) = raw.split_once("\r\n\r\n").unwrap();
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        let text = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("").to_string()
        } else {
            rest.to_string()
        };
        (status, serde_json::from_str(text.trim()).unwrap_or(Value::String(text)))
    }

    fn workspaces(&self) -> Vec<Value> {
        self.d.json(&["workspace", "ls"]).as_array().cloned().unwrap_or_default()
    }

    fn record(&self, id: &str) -> Value {
        self.http("GET", &format!("/workspaces/{id}"), None).1
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", msg]);
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_guard_keeps_main_checkouts_clean() {
    let Some(e) = Env::start() else { return };
    let repo = e.repo("Repo", &[]);
    let cwd = repo.display().to_string();
    let r = e.d.lagent(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", &cwd, "edit", "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("WORKSPACE_GUARD") && r.stderr.contains("main checkout"), "{}", r.stderr);
    let plan = e.d.json(&["run", "--provider", "mock/e2e-fast-stream", "--mode", "plan", "--cwd", &cwd, "read only"]);
    assert_eq!(plan["status"], "completed");
    let allowed = e.d.json(&["run", "--provider", "mock/e2e-fast-stream", "--allow-main-checkout", "--cwd", &cwd, "owner says ok"]);
    assert_eq!(allowed["status"], "completed");
    // A worktree outside ~/Projects/.worktrees/<Repo>/ is refused too.
    let stray = e.root.path().join("stray");
    git(&repo, &["worktree", "add", "-q", "-b", "stray", &stray.display().to_string()]);
    let r = e.d.lagent(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", &stray.display().to_string(), "x", "--json"]);
    assert!(r.stderr.contains("outside ~/Projects/.worktrees"), "{}", r.stderr);
    // A repo outside the projects root (here: a temp dir) is not protected.
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q"]);
    let ok = e.d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", &tmp.path().display().to_string(), "fine"]);
    assert_eq!(ok["status"], "completed");
}

#[test]
fn worktree_from_a_prompt_runs_setup_and_gets_a_name() {
    let Some(e) = Env::start() else { return };
    let cfg = r#"{"worktree":{"setup":["echo setup $PASEO_BRANCH_NAME $PASEO_WORKTREE_PORT $PASEO_ROOT_PATH > setup.log","false","echo never > never.log"]}}"#;
    let repo = e.repo("Repo", &[("paseo.json", cfg)]);
    let cwd = repo.display().to_string();
    let r = e.d.lagent(&["run", "--new-workspace", "worktree", "--provider", "mock/e2e-fast-stream", "--cwd", &cwd, "Fix the login redirect bug", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stderr.contains("Created workspace wks_") && r.stderr.contains("(agent/fix-the-login-redirect-bug)"), "{}", r.stderr);
    let run: Value = serde_json::from_str(&r.stdout).unwrap();
    let wt_path = e.wt_root().join("Repo/agent-fix-the-login-redirect-bug");
    assert_eq!(run["cwd"], wt_path.display().to_string());
    let ws = e.workspaces().into_iter().find(|w| w["isolation"] == "worktree").unwrap();
    let id = ws["workspaceId"].as_str().unwrap().to_string();
    wait_for("setup", || e.record(&id)["setup"]["status"] == "failed");
    let rec = e.record(&id);
    let log = std::fs::read_to_string(wt_path.join("setup.log")).unwrap();
    assert!(log.starts_with("setup agent/fix-the-login-redirect-bug "), "{log}");
    assert!(log.trim_end().ends_with(&cwd), "{log}");
    assert_eq!(rec["setup"]["commands"][1]["exitCode"], 1);
    assert_eq!(rec["setup"]["commands"][2]["status"], "skipped");
    assert!(!wt_path.join("never.log").exists());
    // The mock answered the naming prompt: a title, and the placeholder kept or renamed.
    wait_for("auto name", || e.record(&id)["autoName"]["status"] == "attempted");
    let rec = e.record(&id);
    assert!(rec["title"].is_string(), "{rec}");
    assert_eq!(git(&wt_path, &["branch", "--show-current"]), rec["branch"].as_str().unwrap());
    assert!(rec["branch"].as_str().unwrap().starts_with("agent/"));
    // The agent inside sees its workspace.
    let agents = e.d.json(&["ls", "-a"]);
    assert!(agents.as_array().unwrap().iter().all(|a| a["name"] != "Branch name generator"), "the generator is internal and deleted");
}

#[test]
fn diffs_status_and_files() {
    let Some(e) = Env::start() else { return };
    let repo = e.repo("Repo", &[("f.txt", "a\nb\nc\n"), ("old.txt", "keep\n")]);
    let ws = e.d.json(&["workspace", "create", "--isolation", "worktree", "--path", &repo.display().to_string(), "--new-branch", "feat/x"]);
    assert_eq!(ws["name"], "feat/x");
    let id = ws["workspaceId"].as_str().unwrap().to_string();
    let dir = PathBuf::from(ws["cwd"].as_str().unwrap());
    assert_eq!(dir, e.wt_root().join("Repo/feat-x"));
    std::fs::write(dir.join("f.txt"), "a\nB\nc\nd\n").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir, "two");
    std::fs::write(dir.join("f.txt"), "a\nB\nc\nd\ne\n").unwrap();
    std::fs::write(dir.join("n.txt"), "new\n").unwrap();
    git(&dir, &["rm", "-q", "old.txt"]);
    std::fs::write(dir.join("bin.dat"), [0u8, 1]).unwrap();
    let paths = |v: &Value| v["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let un = e.http("GET", &format!("/workspaces/{id}/diff?mode=uncommitted"), None).1;
    assert_eq!(paths(&un), ["bin.dat", "f.txt", "n.txt", "old.txt"]);
    assert_eq!(un["files"][0]["status"], "binary");
    assert_eq!(un["files"][1]["hunks"][0]["lines"][0]["content"], "@@ -2,3 +2,4 @@");
    let base = e.http("GET", &format!("/workspaces/{id}/diff?mode=base"), None).1;
    assert_eq!(paths(&base), ["f.txt"]);
    assert_eq!((base["files"][0]["additions"].clone(), base["files"][0]["deletions"].clone()), (json!(2), json!(1)));
    let all = e.http("GET", &format!("/workspaces/{id}/diff"), None).1;
    assert_eq!(paths(&all), ["bin.dat", "f.txt", "n.txt", "old.txt"]);
    assert_eq!(all["files"][1]["additions"], 3);
    assert_eq!(git(&dir, &["status", "--porcelain"]).lines().filter(|l| l.starts_with("??")).count(), 2, "the real index is untouched");
    let st = e.http("GET", &format!("/workspaces/{id}/status"), None).1;
    assert_eq!((st["currentBranch"].clone(), st["baseRef"].clone(), st["isDirty"].clone(), st["aheadBehind"].clone()), (json!("feat/x"), json!("main"), json!(true), json!({"ahead": 1, "behind": 0})));

    let ls = e.http("GET", &format!("/workspaces/{id}/files"), None).1;
    let names: Vec<&str> = ls["entries"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, [".git", "README", "bin.dat", "f.txt", "n.txt"]);
    let f = e.http("GET", &format!("/workspaces/{id}/file?path=f.txt"), None).1;
    assert_eq!((f["kind"].clone(), f["content"].clone()), (json!("text"), json!("a\nB\nc\nd\ne\n")));
    let b = e.http("GET", &format!("/workspaces/{id}/file?path=bin.dat"), None).1;
    assert_eq!((b["kind"].clone(), b["encoding"].clone()), (json!("binary"), json!("none")));
    let (code, out) = e.http("GET", &format!("/workspaces/{id}/file?path=../../etc/passwd"), None);
    assert_eq!((code, out["error"]["message"].clone()), (400, json!("Access outside of workspace is not allowed")));
    let (code, w) = e.http("PUT", &format!("/workspaces/{id}/file"), Some(&json!({"path": "n.txt", "content": "edited\n", "expectedRevision": f["revision"]})));
    assert_eq!(code, 400, "a revision of another file is stale: {w}");
    let n = e.http("GET", &format!("/workspaces/{id}/file?path=n.txt"), None).1;
    let (code, _) = e.http("PUT", &format!("/workspaces/{id}/file"), Some(&json!({"path": "n.txt", "content": "edited\n", "expectedRevision": n["revision"]})));
    assert_eq!(code, 200);
    assert_eq!(std::fs::read_to_string(dir.join("n.txt")).unwrap(), "edited\n");
    let tok = e.http("POST", &format!("/workspaces/{id}/download-token"), Some(&json!({"path": "README"}))).1;
    let url = tok["url"].as_str().unwrap().to_string();
    assert_eq!(e.http("GET", &url, None), (200, Value::String("hi\n".into())));
    assert_eq!(e.http("GET", &url, None).0, 404, "one use");

    let sha = e.http("POST", &format!("/workspaces/{id}/commit"), Some(&json!({"message": "three", "addAll": true, "author": {"name": "t", "email": "t@t"}}))).1;
    assert_eq!(sha["sha"].as_str().map(str::len), Some(40));
    assert!(git(&dir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn archive_keeps_dirty_work_and_the_sweep_removes_merged_worktrees() {
    let Some(e) = Env::start() else { return };
    let marker = e.root.path().join("teardown.txt");
    let cfg = format!(r#"{{"worktree":{{"teardown":"echo bye $PASEO_BRANCH_NAME >> {}"}}}}"#, marker.display());
    let repo = e.repo("Repo", &[("paseo.json", &cfg)]);
    let p = repo.display().to_string();
    let mk = |branch: &str| {
        let w = e.d.json(&["workspace", "create", "--isolation", "worktree", "--path", &p, "--new-branch", branch]);
        (w["workspaceId"].as_str().unwrap().to_string(), PathBuf::from(w["cwd"].as_str().unwrap()))
    };
    // Dirty: archived, but the directory stays.
    let (dirty, dirty_dir) = mk("dirty");
    std::fs::write(dirty_dir.join("wip.txt"), "x").unwrap();
    let r = e.d.lagent(&["workspace", "archive", &dirty, "--json"]);
    assert_eq!(r.code, 0);
    assert!(r.stderr.contains("uncommitted changes"), "{}", r.stderr);
    assert!(dirty_dir.exists());
    // Clean and unmerged: the directory goes, the branch stays.
    let (unmerged, unmerged_dir) = mk("unmerged");
    std::fs::write(unmerged_dir.join("u.txt"), "u").unwrap();
    git(&unmerged_dir, &["add", "."]);
    commit(&unmerged_dir, "u");
    let run = e.d.json(&["run", "--provider", "mock/e2e-fast-stream", "--workspace", &unmerged, "hello"]);
    assert_eq!(run["status"], "completed");
    e.d.json(&["workspace", "archive", &unmerged]);
    assert!(!unmerged_dir.exists());
    assert!(git(&repo, &["branch", "--list", "unmerged"]).contains("unmerged"), "unmerged work is kept on its branch");
    let agent = e.d.json(&["inspect", run["agentId"].as_str().unwrap()]);
    assert_eq!(agent["Status"], "closed", "its agents are closed (Paseo)");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().filter(|l| l.starts_with("bye")).count(), 2, "teardown ran for both");
    // The sweep: a fresh worktree stays; a merged one goes with its branch.
    let (_fresh, fresh_dir) = mk("fresh");
    let (_merged, merged_dir) = mk("done");
    std::fs::write(merged_dir.join("d.txt"), "d").unwrap();
    git(&merged_dir, &["add", "."]);
    commit(&merged_dir, "d");
    assert_eq!(e.d.json(&["workspace", "sweep"]), json!([]), "not merged yet");
    git(&repo, &["merge", "-q", "--ff-only", "done"]);
    let swept = e.d.json(&["workspace", "sweep"]);
    assert_eq!(swept.as_array().unwrap().len(), 1, "{swept}");
    assert_eq!(swept[0]["reason"], "merged into main");
    assert!(!merged_dir.exists());
    assert!(git(&repo, &["branch", "--list", "done"]).is_empty(), "merged branch deleted");
    assert!(fresh_dir.exists(), "a worktree that never moved is not 'merged'");
}

#[test]
fn checkout_branch_errors_projects_and_clone() {
    let Some(e) = Env::start() else { return };
    let repo = e.repo("Repo", &[]);
    let p = repo.display().to_string();
    git(&repo, &["branch", "existing"]);
    let w = e.d.json(&["workspace", "create", "--isolation", "worktree", "--path", &p, "--mode", "checkout-branch", "--branch", "existing"]);
    assert_eq!(w["name"], "existing");
    // main is checked out in the main checkout: a copy of it.
    let copy = e.d.json(&["workspace", "create", "--isolation", "worktree", "--path", &p, "--mode", "checkout-branch", "--branch", "main"]);
    assert_eq!(copy["name"], "main-1");
    for (args, msg) in [
        (vec!["--isolation", "worktree", "--mode", "checkout-branch", "--branch", "nope"], "Unknown branch: nope"),
        (vec!["--isolation", "worktree", "--mode", "checkout-branch"], "--branch is required for --mode checkout-branch"),
        (vec!["--isolation", "local", "--base", "main"], "Worktree options require --isolation worktree"),
    ] {
        let mut a = vec!["workspace", "create", "--path", &p];
        a.extend(args);
        a.push("--json");
        let r = e.d.lagent(&a);
        let err: Value = serde_json::from_str(&r.stderr).unwrap();
        assert_eq!((err["error"]["code"].clone(), err["error"]["message"].clone()), (json!("WORKSPACE_CREATE_FAILED"), json!(msg)));
    }
    let r = e.d.lagent(&["workspace", "create", "--isolation", "worktree", "--path", "/tmp", "--json"]);
    assert!(r.stderr.contains("Create worktree requires a git repository"), "{}", r.stderr);
    let renamed = e.d.json(&["workspace", "rename", w["workspaceId"].as_str().unwrap(), "My title"]);
    assert_eq!(renamed["title"], "My title");
    assert_eq!(e.d.json(&["workspace", "rename", w["workspaceId"].as_str().unwrap(), "--reset"])["title"], Value::Null);
    // Pins and labels (Paseo: workspace.pin.set, workspace.label.assignment.set; keys collapsed, lowercased).
    let wid = w["workspaceId"].as_str().unwrap();
    let pinned = e.http("PATCH", &format!("/workspaces/{wid}"), Some(&json!({"pinned": true}))).1;
    assert!(pinned["pinnedAt"].is_string() && pinned["title"].is_null(), "pin keeps the title: {pinned}");
    let labelled = e.http("POST", &format!("/workspaces/{wid}/labels"), Some(&json!({"label": "  Needs   Review ", "assigned": true}))).1;
    assert_eq!(labelled["labels"], json!(["needs review"]));
    let unlabelled = e.http("POST", &format!("/workspaces/{wid}/labels"), Some(&json!({"label": "needs review", "assigned": false}))).1;
    assert!(unlabelled.get("labels").is_none_or(|l| l == &json!([])), "{unlabelled}");

    let projects = e.d.json(&["project", "ls"]);
    assert_eq!(projects[0]["name"], "Repo");
    let pid = projects[0]["projectId"].as_str().unwrap().to_string();
    assert_eq!(e.d.json(&["project", "rename", &pid, "Lucina"])["name"], "Lucina");
    // Clone a local bare repo by URL and register it.
    let bare = e.root.path().join("bare.git");
    git(e.root.path(), &["clone", "-q", "--bare", &p, &bare.display().to_string()]);
    let into = tempfile::tempdir().unwrap();
    let cloned = e.d.json(&["clone", &format!("file://{}", bare.display()), "--dir", &into.path().display().to_string()]);
    assert_eq!(cloned["checkoutPath"], into.path().join("bare").display().to_string());
    assert!(e.d.json(&["project", "ls"]).as_array().unwrap().iter().any(|p| p["name"] == "bare"));
    let del = e.d.json(&["project", "delete", &pid]);
    assert_eq!(del["removedWorkspaceIds"].as_array().map(Vec::len), Some(2));
    assert!(e.workspaces().iter().all(|w| w["project"] != "Lucina"));
}
