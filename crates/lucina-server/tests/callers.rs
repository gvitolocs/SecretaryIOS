//! Caller screening: /api/secretary/callers/* and the chat shortcut. Jev is
//! not configured in tests, so the deterministic rules answer (`source: rules`).

mod common;

use common::{harness, script};
use serde_json::json;

#[tokio::test]
async fn screen_records_calls_and_feeds_the_directory() {
    let h = harness(script(json!({"rules": []})), |c| c.callers_enabled = true).await;

    let ring = json!({
        "numbers": ["+45 70 40 52 05"], "lang": "en",
        "context": {"courier": {"onShift": true, "orderActive": false}},
        "call": {"source": "android"}
    });
    let (s, b) = h.post("/callers/screen", ring.clone()).await;
    assert_eq!(s, 200, "{b}");
    let v = &b["data"]["results"][0];
    assert_eq!(v["number"], "+4570405205", "{b}");
    assert_eq!(v["type"], "non_geographic", "{b}");
    assert_eq!(v["kind"], "telemarketing", "{b}");
    assert_eq!(v["source"], "rules", "{b}");
    assert_eq!(v["known"]["previousCalls"], 0, "{b}");
    assert!(b["data"]["table"]
        .as_str()
        .unwrap()
        .starts_with("| Number | Spam | Kind | Pick up |"));

    let (_, b) = h.post("/callers/screen", ring).await;
    assert_eq!(b["data"]["results"][0]["known"]["previousCalls"], 1, "{b}");
    assert!(b["data"]["results"][0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "called once before"));

    // A lookup of several numbers in Danish; contacts stay out of the directory.
    let (s, b) = h
        .post(
            "/callers/screen",
            json!({"numbers": ["+357 94 095733", "hello"], "lang": "da"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["lang"], "da");
    assert_eq!(
        b["data"]["results"][0]["label"], "Sandsynligvis svindel (0,60)",
        "{b}"
    );
    assert_eq!(b["data"]["results"][1]["type"], "invalid", "{b}");
    let (_, b) = h
        .post(
            "/callers/screen",
            json!({"numbers": ["20682758"], "context": {"contact": "Jerry's Pizza"}}),
        )
        .await;
    assert_eq!(b["data"]["results"][0]["source"], "contact", "{b}");

    let (s, b) = h.get("/callers/directory").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["data"]["entries"],
        json!([
            {"number": 4570405205_i64, "label": "Telemarketing 60%"},
            {"number": 35794095733_i64, "label": "Svindel 60%"}
        ]),
        "{b}"
    );
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(h.dir.path().join("callers/callers.json"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn bad_requests_and_the_off_switch() {
    let h = harness(script(json!({"rules": []})), |c| c.callers_enabled = true).await;
    for body in [
        json!({"numbers": []}),
        json!({"numbers": ["1"], "lang": "fr"}),
        json!({"numbers": ["1", "2"], "call": {"source": "android"}}),
        json!({"numbers": ["1"], "surprise": true}),
        json!({"numbers": ["1"], "call": {"source": "pager"}}),
    ] {
        let (s, b) = h.post("/callers/screen", body.clone()).await;
        assert_eq!(s, 400, "{body} → {b}");
    }
    let (s, _) = h.get("/callers/screen").await;
    assert_eq!(s, 405);
    let (s, _) = h.get("/callers/nothing").await;
    assert_eq!(s, 404);

    let off = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = off.post("/callers/screen", json!({"numbers": ["1"]})).await;
    assert_eq!(s, 404, "{b}");
    assert_eq!(b["error"], "callers_disabled", "{b}");
}

#[tokio::test]
async fn chat_answers_who_called_with_the_table() {
    // The reply model must not be asked: the numbers are the screen's.
    let h = harness(
        script(json!({"rules": [{"kind": "reply", "content": "That number is your bank."}]})),
        |c| c.callers_enabled = true,
    )
    .await;
    let d = h.chat("Who called me from +45 70 40 52 05?").await;
    assert_eq!(d["action"], "caller_screen", "{d}");
    let reply = d["reply"].as_str().unwrap();
    for s in [
        "**+45 70 40 52 05** — Likely telemarketing (0.60)",
        "| Number | Spam | Kind | Pick up |",
        "| +45 70 40 52 05 | 0.65 | telemarketing 0.60 | 0.20 |",
        "Source: basic rules",
    ] {
        assert!(reply.contains(s), "{s} missing: {reply}");
    }
    assert!(!reply.contains("your bank"), "{reply}");

    let list = "+45 70 40 52 05\n+357 94 095733\n+45 70 40 52 20\n+357 94 462808\n+45 20 68 27 58\n+44 7459 508537\n+45 66 77 87 02";
    let d = h.chat(list).await;
    assert_eq!(d["action"], "caller_screen", "{d}");
    let reply = d["reply"].as_str().unwrap();
    assert_eq!(reply.matches("\n| +").count(), 7, "{reply}");

    let d = h.chat("Chi mi ha chiamato dal 20 68 27 58?").await;
    assert_eq!(d["action"], "caller_screen", "{d}");
    assert!(
        d["reply"]
            .as_str()
            .unwrap()
            .contains("| Numero | Spam | Tipo | Rispondere |"),
        "{d}"
    );
}
