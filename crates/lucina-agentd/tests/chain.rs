//! Lane chains with quota fall-through, on the mock provider and a test lane config:
//! a quota-classified failure moves to the next lane; an unclassified one stops the
//! chain; the agent keeps its id and its timeline across lanes.

mod common;

use common::Daemon;
use serde_json::Value;

const CHAINS: &str = r#"
[lanes.mock-a]
kind = "mock"
model = "e2e-fast-stream"
[lanes.mock-b]
kind = "mock"
model = "ten-second-stream"
[categories]
test = ["mock-a", "mock-b"]
[[detectors]]
name = "mock-a-quota"
kinds = ["mock"]
lanes = ["mock-a"]
outcome = "quota"
pattern = 'Requested mock provider failure'
"#;

fn timeline_texts(d: &Daemon, id: &str) -> Vec<String> {
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

#[test]
fn quota_falls_through_and_unclassified_failure_stops() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("chains.toml");
    std::fs::write(&cfg, CHAINS).unwrap();
    let d = Daemon::start_with_env(&[("LANE_CONFIG", cfg.display().to_string())]);
    let run = d.json(&["run", "--chain", "test", "--cwd", "/tmp", "emit synthetic turn failure"]);
    assert_eq!(run["status"], "error");
    assert_eq!(run["title"], "emit synthetic turn failure");
    let id = run["agentId"].as_str().unwrap().to_string();
    let notes = timeline_texts(&d, &id);
    assert_eq!(
        notes,
        [
            "lane mock-a: quota (mock-a-quota); trying lane mock-b".to_string(),
            "lane mock-b: failed; the chain stops".to_string(),
        ]
    );
    let inspect = d.json(&["inspect", &id]);
    assert_eq!(inspect["Model"], "ten-second-stream", "the agent moved to lane mock-b");
    let users = d.lagent(&["logs", &id]).stdout.lines().filter(|l| l.starts_with("[User]")).count();
    assert_eq!(users, 2, "the prompt was sent once per lane");
}

#[test]
fn success_on_first_lane_never_falls_through() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("chains.toml");
    std::fs::write(&cfg, CHAINS).unwrap();
    let d = Daemon::start_with_env(&[("LANE_CONFIG", cfg.display().to_string())]);
    let run = d.json(&["run", "--chain", "test", "--cwd", "/tmp", "hello"]);
    assert_eq!(run["status"], "completed");
    let id = run["agentId"].as_str().unwrap().to_string();
    assert!(timeline_texts(&d, &id).is_empty());
    assert_eq!(d.json(&["inspect", &id])["Model"], "e2e-fast-stream");
}

#[test]
fn unknown_chain_is_refused() {
    let d = Daemon::start();
    let r = d.lagent(&["run", "--chain", "nope", "--cwd", "/tmp", "--json", "hi"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("no lane of chain"), "{}", r.stderr);
}
