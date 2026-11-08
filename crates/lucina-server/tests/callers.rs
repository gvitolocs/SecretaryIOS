//! Caller screening: /api/secretary/callers/* and the chat shortcut. Jev is
//! not configured in tests, so the deterministic rules answer (`source: rules`);
//! the public caller-ID site is a fake one.

mod common;

use common::{harness, script};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A 180.dk-like page (the structure the site served on 2026-10-29).
const PAGE_180: &str = r#"<title>20682758 | ADVARSEL &#x2013; rapporter: 6</title>
<span>20682758</span><span>(Danmark)</span><span>+4520682758</span>
<b>374</b><small>Antal gange s&#xF8;gte</small><b>6</b><small>Rapporteret af brugere</small>
<h3>Seneste kommentar</h3><p>26. oktober 2026</p><p>Irriterende</p><p>De ringer i min arbejdstid</p><p>Kommentar fra bruger</p>
<li>Forsigtig (37,5%)</li><li>Irriterende (37,5%)</li><li>Telemarketing (25%)</li>"#;

#[tokio::test]
async fn screen_records_calls_and_feeds_the_directory() {
    let h = harness(script(json!({"rules": []})), |c| c.callers_enabled = true).await;

    // An older phone build still sends the shift: accepted and ignored.
    let ring = json!({
        "numbers": ["+45 70 40 52 05"], "lang": "en",
        "context": {"courier": {"onShift": true, "orderActive": true}},
        "call": {"source": "android"}
    });
    let (s, b) = h.post("/callers/screen", ring.clone()).await;
    assert_eq!(s, 200, "{b}");
    let v = &b["data"]["results"][0];
    assert_eq!(v["number"], "+4570405205", "{b}");
    assert_eq!(v["type"], "non_geographic", "{b}");
    assert_eq!(v["kind"], "business", "{b}");
    assert_eq!(v["source"], "rules", "{b}");
    assert_eq!(v["label"], "Unknown caller", "{b}");
    assert_eq!(v["known"]["previousCalls"], 0, "{b}");
    assert!(b["data"]["table"]
        .as_str()
        .unwrap()
        .starts_with("| Number | Spam | Kind | Pick up |"));
    assert!(!b.to_string().to_lowercase().contains("wolt"), "{b}");

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
    assert_eq!(b["data"]["results"][0]["label"], "Ukendt opkald · ingen database for dette land", "{b}");
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
            {"number": 4570405205_i64, "label": "Unknown caller"},
            {"number": 35794095733_i64, "label": "Ukendt opkald"}
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
        "**+45 70 40 52 05** — Unknown caller · Danish 70-number (company line)",
        "| Number | Spam | Kind | Pick up |",
        "| +45 70 40 52 05 | 0.40 | business 0.45 | 0.40 |",
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
            .contains("| Number | Spam | Kind | Pick up |"),
        "{d}"
    );
}

#[tokio::test]
async fn the_public_page_is_read_first_and_quoted() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/nummer/20682758"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PAGE_180))
        .mount(&site)
        .await;
    let uri = site.uri();
    let h = harness(script(json!({"rules": []})), |c| {
        c.callers_enabled = true;
        c.callers_public_url = Some(uri);
    })
    .await;
    let d = h.chat("Who called me from 20 68 27 58?").await;
    assert_eq!(d["action"], "caller_screen", "{d}");
    let reply = d["reply"].as_str().unwrap();
    for s in [
        "**+45 20 68 27 58** — Likely telemarketing (0.50)",
        "180.dk: 6 reports",
        "180.dk: 6 user reports (looked up 374 times); categories: caution 37.5%, annoying 37.5%, telemarketing 25%",
        "De ringer i min arbejdstid",
        "https://www.180.dk/nummer/20682758",
    ] {
        assert!(reply.contains(s), "{s} missing: {reply}");
    }
}

#[tokio::test]
async fn rejections_allow_and_the_block_list() {
    let h = harness(script(json!({"rules": []})), |c| c.callers_enabled = true).await;
    // A reported number (the fake 180.dk is off here, so seed it through the store).
    let store = h.state.tool_env.callers.clone().unwrap();
    let (s, b) = h
        .post("/callers/screen", json!({"numbers": ["+45 70 40 52 05"]}))
        .await;
    assert_eq!(s, 200, "{b}");
    // Rules without report facts never block.
    let (_, b) = h.get("/callers/directory?block=1").await;
    assert_eq!(b["data"]["block"], json!([]), "{b}");
    assert_eq!(b["data"]["entries"][0]["number"], 4570405205_i64, "{b}");

    let (s, b) = h
        .post(
            "/callers/rejected",
            json!({"number": "+45 70 40 52 20", "line": "Rejected likely telemarketer +45 70 40 52 20 · 180.dk 1 reports",
                   "spam": 0.82, "pickUp": 0.11, "sources": "180.dk 1 reports", "source": "android"}),
        )
        .await;
    assert_eq!(
        (s, b["data"]["recorded"].clone()),
        (200, json!(true)),
        "{b}"
    );
    assert_eq!(
        store
            .rejections_since(chrono::Utc::now() - chrono::Duration::hours(1))
            .len(),
        1
    );
    for bad in [
        json!({"number": "x", "line": "l", "source": "android"}),
        json!({"number": "+4570405220", "line": "", "source": "android"}),
        json!({"number": "+4570405220", "line": "l", "source": "pager"}),
        json!({"number": "+4570405220", "line": "l", "source": "android", "spam": 2.0}),
    ] {
        let (s, b) = h.post("/callers/rejected", bad.clone()).await;
        assert_eq!(s, 400, "{bad} → {b}");
    }

    let (s, b) = h
        .post("/callers/allow", json!({"number": "70 40 52 05"}))
        .await;
    assert_eq!(
        (s, b["data"]["number"].clone()),
        (200, json!("+4570405205")),
        "{b}"
    );
    let (_, b) = h
        .get("/callers/directory?block=1&spam=0.5&pickup=0.3")
        .await;
    assert_eq!(b["data"]["entries"][0]["label"], "Allowed", "{b}");
    let (s, _) = h.get("/callers/directory?block=1&spam=2").await;
    assert_eq!(s, 400);
    let (s, _) = h.post("/callers/allow", json!({"number": "hello"})).await;
    assert_eq!(s, 400);

    // "allow <number>" in chat.
    let d = h.chat("allow +45 20 68 27 58").await;
    assert_eq!(d["action"], "caller_screen", "{d}");
    assert!(
        d["reply"]
            .as_str()
            .unwrap()
            .contains("+45 20 68 27 58 is allowed"),
        "{d}"
    );
    assert_eq!(
        store.known("+4520682758").owner_label.as_deref(),
        Some("Allowed")
    );
}
