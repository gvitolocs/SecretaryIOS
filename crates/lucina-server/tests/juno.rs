//! The Juno bridge over HTTP (`/api/secretary/device/juno/*`), the `juno_*`
//! tools' registries, and a Juno Chat turn that takes a screenshot.

mod common;

use base64::Engine as _;
use common::{harness, script, TOKEN};
use serde_json::{json, Value};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-screen";

#[tokio::test]
async fn juno_routes_need_the_token_and_refuse_unknown_calls() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let r = h
        .http
        .post(h.url("/device/juno/next"))
        .json(&json!({"device": {"id": "dev1", "name": "Juno"}, "waitSecs": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let (s, b) = h
        .post(
            "/device/juno/next",
            json!({"device": {"id": "dev1", "name": "Juno"}, "waitSecs": 0}),
        )
        .await;
    assert_eq!((s, &b["data"]), (200, &json!({"call": null})), "{b}");
    assert_eq!(h.state.juno.status()["devices"][0]["id"], "dev1");

    let (s, b) = h.post("/device/juno/next", json!({"waitSecs": 0})).await;
    assert_eq!(
        (s, b["error"].as_str()),
        (400, Some("bad_request")),
        "no device"
    );

    let (s, b) = h
        .post(
            "/device/juno/result",
            json!({"device": {"id": "dev1", "name": "Juno"}, "id": "nope", "ok": true}),
        )
        .await;
    assert_eq!((s, b["error"].as_str()), (404, Some("unknown_call")), "{b}");

    let (s, _) = h.get("/device/juno/next").await;
    assert_eq!(s, 405);
}

#[tokio::test]
async fn juno_result_route_takes_a_screenshot_sized_body() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let post = |n: usize| {
        let body = json!({"device": {"id": "dev1", "name": "Juno"}, "id": "nope", "ok": true,
            "result": {"text": "x", "image": {"data": "A".repeat(n), "mimeType": "image/png"}}});
        h.http
            .post(h.url("/device/juno/result"))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
    };
    // Past Lucina's 64 KiB default: read, then refused only for its unknown id.
    assert_eq!(post(9 * 1024 * 1024).await.unwrap().status(), 404);
    assert_eq!(post(13 * 1024 * 1024).await.unwrap().status(), 413);
}

#[tokio::test]
async fn juno_tools_are_in_the_owners_registry_only() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    for n in lucina_tools::tools::juno::NAMES {
        assert!(h.state.registry.get(n).is_some(), "{n}");
        assert!(h.state.mcp_registry.get(n).is_none(), "{n}");
        assert!(h.state.trusted_mcp_registry.get(n).is_none(), "{n}");
    }
    assert!(h.state.tool_env.juno.is_some());
    let off = harness(script(json!({"rules": []})), |c| c.juno_tools = false).await;
    for n in lucina_tools::tools::juno::NAMES {
        assert!(off.state.registry.get(n).is_none(), "{n}");
    }
    assert!(off.state.tool_env.juno.is_none());
}

#[tokio::test]
async fn juno_chat_turn_gets_the_device_fact_and_shows_the_screenshot() {
    let rules = json!({"rules": [
        {"kind": "agent", "afterTool": true, "content": "Gallery is in front."},
        {"kind": "agent", "toolCalls": [{"name": "juno_screenshot", "arguments": {}}]},
    ]});
    let h = harness(script(rules), |c| {
        c.planner = lucina_server::PlannerMode::Tools;
        c.rich_replies = true;
    })
    .await;
    // The laptop: polls until it gets a call, then answers it with a screenshot.
    let (http, next, result) = (
        h.http.clone(),
        h.url("/device/juno/next"),
        h.url("/device/juno/result"),
    );
    let device = json!({"id": "dev1", "name": "Juno"});
    let (s, _) = h
        .post(
            "/device/juno/next",
            json!({"device": device, "waitSecs": 0}),
        )
        .await;
    assert_eq!(s, 200);
    let laptop = tokio::spawn(async move {
        loop {
            let b: Value = http
                .post(&next)
                .bearer_auth(TOKEN)
                .json(&json!({"device": device, "waitSecs": 5}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let call = &b["data"]["call"];
            if call.is_null() {
                continue;
            }
            let data = base64::engine::general_purpose::STANDARD.encode(PNG);
            let r = http
                .post(&result)
                .bearer_auth(TOKEN)
                .json(&json!({"device": device, "id": call["id"], "ok": true,
                    "result": {"text": "Gallery in front, 1920x1080", "image": {"data": data, "mimeType": "image/png"}}}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            return call.clone();
        }
    });
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "show me the screen", "sessionId": "juno-dev1", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let call = laptop.await.unwrap();
    assert_eq!(call["tool"], "screenshot");

    let d = &b["data"];
    let image = d["blocks"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["type"] == "image"))
        .unwrap_or_else(|| panic!("no image block: {d}"));
    let src = image["src"].as_str().unwrap();
    assert!(
        src.starts_with("/v1/juno/screenshots/") && src.ends_with(".png"),
        "{src}"
    );
    // secretary-server serves `/v1/juno/…` from Lucina's `/api/secretary/juno/…`.
    let r = h
        .http
        .get(h.url(src.trim_start_matches("/v1")))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.bytes().await.unwrap().as_ref(), PNG);

    // The model got the device fact, the screen's text, and never the PNG.
    let requests = h.fake.requests();
    let agent: Vec<String> = requests
        .iter()
        .filter(|r| r["tools"].is_array())
        .map(Value::to_string)
        .collect();
    assert!(
        agent
            .iter()
            .any(|r| r.contains("The owner is writing from Juno")),
        "no device fact"
    );
    assert!(agent
        .iter()
        .any(|r| r.contains("Gallery in front, 1920x1080")));
    let data = base64::engine::general_purpose::STANDARD.encode(PNG);
    assert!(requests.iter().all(|r| !r.to_string().contains(&data)));
}

#[tokio::test]
async fn juno_device_fact_only_for_juno_sessions() {
    assert!(lucina_server::chat::juno_system("secretary_juno-dev1").contains("Juno, his laptop"));
    assert!(lucina_server::chat::juno_system("juno-dev1").contains("juno_* tools"));
    assert_eq!(lucina_server::chat::juno_system("secretary_ios"), "");
    assert_eq!(lucina_server::chat::juno_system("secretary_test"), "");
}
