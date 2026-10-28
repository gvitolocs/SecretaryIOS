//! Parity row C1: next-run times against Paseo 0.13.2's own `computeNextRunAt`, for
//! cron in UTC and in zones across their DST changes (Rome, New York, Lord Howe's
//! half hour), a year with no match, and `every` cadences. The cases and Paseo's
//! answers are in `lucina-agents/tests/fixtures/next-run.json` (`gen-orchestration.mjs`).

use lucina_agentd::schedules::{iso, next_run_at, parse_iso};
use lucina_agents::schedule::Cadence;
use serde_json::Value;

#[test]
fn next_run_times_match_paseo() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../lucina-agents/tests/fixtures/next-run.json");
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(cases.len() >= 140);
    let mut failures = Vec::new();
    for c in &cases {
        let cadence: Cadence = serde_json::from_value(c["cadence"].clone()).unwrap();
        let after = parse_iso(c["after"].as_str().unwrap()).unwrap();
        let got = next_run_at(&cadence, after);
        let ok = match (&got, c["next"].as_str(), c["error"].as_str()) {
            (Ok(t), Some(n), _) => iso(*t) == n,
            (Err(e), None, Some(pe)) => e == pe,
            _ => false,
        };
        if !ok {
            failures.push(format!("{} after {}: paseo {} / {}, lucina {:?}", c["cadence"], c["after"], c["next"], c["error"], got.map(iso)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
