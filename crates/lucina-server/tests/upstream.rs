//! `POST /api/secretary/jev/upstream` (docs/upstream-watch.md): typed body, Jev's verdict or why not.

mod common;

use common::{harness, script};
use serde_json::json;

fn body() -> serde_json::Value {
    json!({
        "upstream": {
            "id": "asterinas", "name": "Asterinas", "url": "https://github.com/asterinas/asterinas",
            "source": "github", "repo": "asterinas/asterinas", "pin": "v0.19.0",
            "ours": [{"repo": "/r", "path": "kernel"}],
            "licence_upstream": "MPL-2.0", "licence_ours": "MPL-2.0",
            "relation": "fork", "confidence": "confirmed", "rely_on": ["vfs"]
        },
        "changes": {
            "id": "asterinas", "pin": "v0.19.0", "latest": "v0.20.0",
            "releases": [], "commits_total": 1,
            "commits": [{"sha7": "1234567", "date": "2026-10-20T00:00:00Z", "subject": "fix(vfs): leak"}],
            "files_touched": [], "compare_url": null, "skipped": null, "error": null
        },
        "summary": {"headline": "1 commit (1 fix)", "lines": ["Fix: fix(vfs): leak"],
                    "counts": {"releases": 0, "commits": 1, "security": 0, "breaking": 0, "feat": 0, "fix": 1, "perf": 0, "other": 0}}
    })
}

#[tokio::test]
async fn jev_off_gives_no_verdict_and_bad_bodies_are_refused() {
    // Jev off for this test binary (the shared client reads the environment once).
    std::env::set_var("LUCINA_JEV", "off");
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = h.post("/jev/upstream", body()).await;
    assert_eq!(s, 200, "{b}");
    assert!(b["data"]["verdict"].is_null(), "{b}");
    assert_eq!(b["data"]["jev"], "not_configured");
    assert!(b["data"]["elapsedMs"].is_u64());

    let mut bad = body();
    bad["upstream"]["relation"] = json!("copy");
    let (s, b) = h.post("/jev/upstream", bad).await;
    assert_eq!(s, 400, "{b}");
    let mut missing = body();
    missing.as_object_mut().unwrap().remove("summary");
    let (s, _) = h.post("/jev/upstream", missing).await;
    assert_eq!(s, 400);
    let (s, _) = h.get("/jev/upstream").await;
    assert_eq!(s, 405);
}
