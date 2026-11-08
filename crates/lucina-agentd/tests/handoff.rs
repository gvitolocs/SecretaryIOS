//! Quota hand-off (manager/handoff.rs), on the mock providers and a test lane config:
//! a run that stops on a usage limit continues on the next agent of `deep-lucina`, in
//! the same directory with its uncommitted changes, linked both ways; a real failure
//! stays in error; with no agent left the run says so.

mod common;

use common::Daemon;
use serde_json::Value;

/// lane's `claude-usage-limit` detector, word for word, applied to the mock kinds.
const CHAINS: &str = r#"
[lanes.mock-a]
kind = "mock"
model = "e2e-fast-stream"
[lanes.codex]
kind = "codex"
model = "gpt-6.1-sol"
[lanes.mock-b]
kind = "mock-alt"
model = "e2e-fast-stream"
[categories]
deep-lucina = ["mock-a", "codex", "mock-b"]
[[detectors]]
name = "claude-usage-limit"
kinds = ["mock", "mock-alt"]
outcome = "quota"
pattern = '(?i)you.ve hit your (session |weekly |monthly |usage |daily )?limit|usage limit|"api_error_status"\s*:\s*429|rate.?limit'
"#;

fn daemon(chains: &str) -> (Daemon, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("chains.toml");
    std::fs::write(&cfg, chains).unwrap();
    (Daemon::start_with_env(&[("LANE_CONFIG", cfg.display().to_string())]), dir)
}

/// The list items (`/agents/items`: they carry the bucket the Agents page colours by).
fn agents(d: &Daemon) -> Vec<Value> {
    let out = common::run(
        std::process::Command::new("curl").args(["-s", "--unix-socket", &d.socket().display().to_string(), "http://x/agents/items"]),
    );
    let v: Value = serde_json::from_str(&out.stdout).expect("agents json");
    v.as_array().cloned().or_else(|| v["agents"].as_array().cloned()).expect("agent list")
}

fn notes(d: &Daemon, id: &str) -> Vec<String> {
    let out = common::run(
        std::process::Command::new("curl")
            .args(["-s", "--unix-socket", &d.socket().display().to_string(), &format!("http://x/agents/{id}/timeline")]),
    );
    let v: Value = serde_json::from_str(&out.stdout).expect("timeline json");
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["item"]["type"] == "notification")
        .map(|r| r["item"]["message"].as_str().unwrap().to_string())
        .collect()
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

#[test]
fn usage_limit_continues_on_the_next_agent_with_the_changes_in_place() {
    let (d, _cfg) = daemon(CHAINS);
    let work = tempfile::tempdir().unwrap();
    git(work.path(), &["init", "-q"]);
    std::fs::write(work.path().join("partial.txt"), "half done\n").unwrap();
    let task = "emit synthetic quota failure while fixing the parser";
    let cwd = work.path().display().to_string();
    let run = d.json(&["run", "--provider", "mock", "--model", "e2e-fast-stream", "--cwd", &cwd, "--label", "reach=rt-000000000001", "--title", "Fix the parser", task]);
    // `run` waits, and the wait follows the continuation.
    assert_eq!(run["status"], "completed", "{run}");
    assert_eq!(run["provider"], "mock-alt");

    let all = agents(&d);
    assert_eq!(all.len(), 2, "{all:#?}");
    let old = all.iter().find(|a| a["provider"] == "mock").expect("the stopped run");
    let new = all.iter().find(|a| a["provider"] == "mock-alt").expect("the continuation");
    let (old_id, new_id) = (old["id"].as_str().unwrap(), new["id"].as_str().unwrap());
    assert_eq!(run["agentId"], new_id);
    // `run` came back only once the continuation had finished its turn, not while it was
    // created but not yet started (live run 2303cb37 -> 622649a7, 2026-11-08).
    assert_eq!(new["status"], "idle", "{new}");
    assert!(d.lagent(&["logs", new_id]).stdout.lines().any(|l| !l.starts_with("[User]") && !l.trim().is_empty()), "the continuation answered");

    // Linked both ways; the stopped run is no longer a red error in the list.
    assert_eq!(old["status"], "error");
    assert_eq!(old["labels"]["lucina.continued-on"], new_id);
    assert_eq!(old["labels"]["lucina.continued-provider"], "mock-alt");
    assert!(old["labels"].get("lucina.handoff").is_none(), "{}", old["labels"]);
    assert_eq!(old["bucket"], "done");
    assert_eq!(new["labels"]["lucina.continued-from"], old_id);
    assert_eq!(new["labels"]["reach"], "rt-000000000001", "the reach thread label carries over");
    assert_eq!(new["labels"]["lucina.handoff.tried"], "mock,mock-alt");
    assert_eq!(new["labels"]["lucina.handoff.count"], "1");
    assert_eq!(new["cwd"], old["cwd"]);
    assert_eq!(new["title"], "Fix the parser");

    // Codex was skipped (Lucina rule) and the next lane's model came from the lane.
    assert_eq!(d.json(&["inspect", new_id])["Model"], "e2e-fast-stream");
    let old_notes = notes(&d, old_id);
    assert!(old_notes.iter().any(|n| n.contains("continued on Mock Alt as agent") && n.contains("claude-usage-limit")), "{old_notes:?}");
    let new_notes = notes(&d, new_id);
    assert!(new_notes.iter().any(|n| n.starts_with("Continued from agent ")), "{new_notes:?}");

    // Same directory, changes untouched, and the prompt carries the task and the status.
    assert_eq!(std::fs::read_to_string(work.path().join("partial.txt")).unwrap(), "half done\n");
    let logs = d.lagent(&["logs", new_id]).stdout;
    assert!(logs.contains("<original-task>") && logs.contains(task), "{logs}");
    assert!(logs.contains("partial.txt"), "git status in the prompt: {logs}");
    assert!(logs.contains("chat-history-summary"), "the chat history goes first: {logs}");
}

#[test]
fn a_real_failure_is_not_handed_on() {
    let (d, _cfg) = daemon(CHAINS);
    let run = d.json(&["run", "--provider", "mock", "--model", "e2e-fast-stream", "--cwd", "/tmp", "emit synthetic turn failure"]);
    assert_eq!(run["status"], "error");
    let all = agents(&d);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert!(all[0]["labels"].get("lucina.continued-on").is_none());
    assert!(all[0]["labels"].get("lucina.handoff").is_none());
    assert_eq!(all[0]["bucket"], "failed");
}

#[test]
fn with_no_agent_left_the_run_says_so() {
    let only = CHAINS.replace(r#"deep-lucina = ["mock-a", "codex", "mock-b"]"#, r#"deep-lucina = ["mock-a", "codex"]"#);
    let (d, _cfg) = daemon(&only);
    let run = d.json(&["run", "--provider", "mock", "--model", "e2e-fast-stream", "--cwd", "/tmp", "emit synthetic quota failure"]);
    assert_eq!(run["status"], "error");
    let all = agents(&d);
    assert_eq!(all.len(), 1, "{all:#?}");
    let id = all[0]["id"].as_str().unwrap();
    assert_eq!(all[0]["labels"]["lucina.handoff"], "exhausted");
    assert!(notes(&d, id).iter().any(|n| n.contains("no other agent could take it over")));
}
