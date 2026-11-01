//! The Rust port against the real thing.
//!
//! Every recorded Hermes stream-json fixture is parsed twice: by the retired
//! Node Lucina's `parseComputerResult` (run through `node`, verbatim copy in
//! `tests/node/`) and by [`lucina_computer_use::parse_computer_result`]. The
//! two must agree, including the receipt filter, the success rules, the
//! `status`, the reply text, the executed tools and the error class.
//!
//! The test is skipped, with a printed reason, when `node` is not installed;
//! it never silently passes on a mismatch.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> Vec<PathBuf> {
    let dir = manifest_dir().join("tests/fixtures/computer");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "jsonl").then_some(path)
        })
        .collect();
    files.sort();
    files
}

/// `None` when Node cannot be run here.
fn run_node(files: &[PathBuf]) -> Option<Vec<Value>> {
    let driver = manifest_dir().join("tests/node/diff.mjs");
    let mut cmd = Command::new("node");
    cmd.arg(&driver);
    cmd.args(files);
    cmd.current_dir(manifest_dir());
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: node is not installed, so the Node parser cannot be run");
            return None;
        }
        Err(e) => panic!("running node: {e}"),
    };
    assert!(
        out.status.success(),
        "the Node driver failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: Vec<Value> = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("the Node driver printed no JSON ({e})"));
    Some(parsed)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[test]
fn the_rust_port_matches_node_on_every_fixture() {
    let files = fixtures();
    assert!(
        files.len() >= 20,
        "expected the recorded Hermes fixtures, found {}",
        files.len()
    );
    let Some(expected) = run_node(&files) else {
        return;
    };
    assert_eq!(expected.len(), files.len());
    let mut completed = 0;
    let mut rejected = 0;
    for (path, node) in files.iter().zip(expected) {
        let name = name_of(path);
        let output = std::fs::read_to_string(path).unwrap();
        let node_ok = node.get("ok").and_then(Value::as_bool).unwrap_or(false);
        match lucina_computer_use::parse_computer_result(&output) {
            Ok(result) => {
                assert!(
                    node_ok,
                    "{name}: Node rejected it ({:?}), the Rust port accepted {:?}",
                    node.get("errorKind"),
                    result
                );
                assert_eq!(
                    result.to_json(),
                    node["value"],
                    "{name}: the Rust port and Node disagree"
                );
                completed += 1;
            }
            Err(e) => {
                assert!(
                    !node_ok,
                    "{name}: the Rust port rejected it ({:?}), Node returned {:?}",
                    e,
                    node.get("value")
                );
                assert_eq!(
                    e.code(),
                    node["errorKind"].as_str().unwrap_or_default(),
                    "{name}: a different error class"
                );
                rejected += 1;
            }
        }
    }
    assert!(completed > 0 && rejected > 0, "the fixtures must cover both outcomes");
    eprintln!("{completed} fixtures accepted, {rejected} rejected, in agreement with Node");
}

#[test]
fn fixtures_are_one_json_object_per_line() {
    // `08_invalid_stream.jsonl` is the one fixture that is malformed on
    // purpose: a truncated JSON line must be refused, never guessed at.
    let mut broken = 0;
    for path in fixtures() {
        let name = name_of(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value) => assert!(
                    value.is_object(),
                    "{name}:{}: not an object",
                    i + 1
                ),
                Err(e) => {
                    assert_eq!(name, "08_invalid_stream.jsonl", "{name}:{}: {e}", i + 1);
                    broken += 1;
                }
            }
        }
    }
    assert_eq!(broken, 1, "the malformed fixture must stay malformed");
}

#[test]
fn the_recorded_fixtures_cover_the_receipt_rules() {
    let statuses: Vec<String> = fixtures()
        .iter()
        .filter_map(|p| {
            lucina_computer_use::parse_computer_result(&std::fs::read_to_string(p).ok()?)
                .ok()
                .map(|r| r.status.as_str().to_owned())
        })
        .collect();
    for want in ["completed", "partial", "unverified"] {
        assert!(statuses.iter().any(|s| s == want), "no fixture is {want}: {statuses:?}");
    }
    let partial = lucina_computer_use::parse_computer_result(
        &std::fs::read_to_string(manifest_dir().join("tests/fixtures/computer/05_partial.jsonl"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(partial.tool_errors, json!(1).as_u64().unwrap() as usize);
    assert_eq!(partial.executed_tools, vec!["terminal"]);
}
