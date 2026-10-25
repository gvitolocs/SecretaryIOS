//! Deals stay on the Node Lucina (crate::node_deals): the map is forwarded,
//! deal questions in chat get Node's reply, with the Rust grocery path as the
//! fallback.

mod common;

use std::time::Duration;

use common::{harness, script, TOKEN};
use lucina_server::node_deals::NodeDeals;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "planner reply"}
    ]}))
}

async fn node_harness(node: &MockServer) -> common::H {
    let uri = node.uri();
    harness(chat_script(), move |c| {
        c.node_url = uri;
        c.node_token = Some("node-token".into());
    })
    .await
}

async fn raw_get(h: &common::H, p: &str) -> (u16, String, reqwest::header::HeaderMap) {
    let r = h
        .http
        .get(h.url(p))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    let hd = r.headers().clone();
    (s, r.text().await.unwrap(), hd)
}

#[tokio::test]
async fn the_deals_map_is_forwarded_to_node_unchanged() {
    let node = MockServer::start().await;
    let map = json!({"ok": true, "data": {"generatedAt": "2026-10-24T12:00:00.000Z", "home": {"lat": 56.19633, "lng": 10.2011, "label": "Risskov"}, "radiusM": 5000, "failures": [], "stores": [{"id": "tjek:4FwHEJQZ", "chain": "SPAR", "chainKey": "spar", "name": "SPAR", "street": "Kamma Klitgårds Gade 47", "lat": 56.19566, "lng": 10.2077, "distanceM": 420, "listMatches": [], "clearances": [], "flyerOffers": []}]}});
    Mock::given(method("GET"))
        .and(path("/api/secretary/grocery/deals/map"))
        .and(query_param("radiusM", "5000"))
        .and(header("authorization", "Bearer node-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(map.clone()))
        .expect(1)
        .mount(&node)
        .await;
    let bad = json!({"ok": false, "error": "bad_request", "message": "radiusM must be an integer from 500 to 15000"});
    Mock::given(method("GET"))
        .and(path("/api/secretary/grocery/deals/map"))
        .and(query_param("radiusM", "10"))
        .respond_with(ResponseTemplate::new(400).set_body_json(bad.clone()))
        .mount(&node)
        .await;
    let h = node_harness(&node).await;

    let (s, body, hd) = raw_get(&h, "/grocery/deals/map?radiusM=5000").await;
    assert_eq!(s, 200);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), map);
    assert_eq!(hd["cache-control"], "no-store");
    assert!(hd["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));

    let (s, body, _) = raw_get(&h, "/grocery/deals/map?radiusM=10").await;
    assert_eq!(s, 400);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), bad);

    let (s, b, hd) = h
        .call(reqwest::Method::POST, "/grocery/deals/map", Some(json!({})))
        .await;
    assert_eq!((s, b["error"].as_str()), (405, Some("method_not_allowed")));
    assert_eq!(hd["allow"], "GET");
    // The rest of /grocery is still unknown here.
    assert_eq!(h.get("/grocery/deals").await.0, 404);
    assert_eq!(h.get("/grocery/deals/map/x").await.0, 404);
    // Wrong token on the Rust side: never forwarded.
    let r = h
        .http
        .get(h.url("/grocery/deals/map"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[tokio::test]
async fn node_errors_pass_through_and_failures_are_deals_unavailable() {
    let node = MockServer::start().await;
    let down = json!({"ok": false, "error": "deals_unavailable", "message": "Nearby deals are unavailable without home coordinates"});
    Mock::given(method("GET"))
        .and(path("/api/secretary/grocery/deals/map"))
        .respond_with(ResponseTemplate::new(503).set_body_json(down.clone()))
        .mount(&node)
        .await;
    let h = node_harness(&node).await;
    let (s, body, _) = raw_get(&h, "/grocery/deals/map").await;
    assert_eq!(
        (s, serde_json::from_str::<Value>(&body).unwrap()),
        (503, down)
    );

    // Node refuses the forward token: that is a misconfigured forward.
    let refusing = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"ok": false, "error": "unauthorized"})),
        )
        .mount(&refusing)
        .await;
    let h = node_harness(&refusing).await;
    let (s, b) = h.get("/grocery/deals/map").await;
    assert_eq!((s, b["error"].as_str()), (503, Some("deals_unavailable")));

    // No JSON (a proxy page): unavailable, not passed on.
    let html = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
        .mount(&html)
        .await;
    let h = node_harness(&html).await;
    let (s, b) = h.get("/grocery/deals/map").await;
    assert_eq!((s, b["error"].as_str()), (503, Some("deals_unavailable")));

    // Unreachable Node, and no token configured.
    let h = harness(chat_script(), |c| {
        c.node_url = "http://127.0.0.1:9".into();
        c.node_token = Some("node-token".into());
    })
    .await;
    let (s, b) = h.get("/grocery/deals/map").await;
    assert_eq!((s, b["error"].as_str()), (503, Some("deals_unavailable")));
    let h = harness(chat_script(), |c| c.node_token = None).await;
    let (s, b) = h.get("/grocery/deals/map").await;
    assert_eq!((s, b["error"].as_str()), (503, Some("deals_unavailable")));
}

#[tokio::test]
async fn a_slow_node_times_out_as_deals_unavailable() {
    let node = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "data": {}}))
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&node)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(
                    json!({"ok": true, "data": {"reply": "late", "intent": "grocery.deals.lunch"}}),
                )
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&node)
        .await;
    let deals = NodeDeals::new(
        &node.uri(),
        Some("node-token".into()),
        Duration::from_millis(200),
    );
    let e = deals.map(None).await.unwrap_err();
    assert_eq!(
        (e.status.as_u16(), e.code.as_str()),
        (503, "deals_unavailable")
    );
    assert_eq!(deals.answer("cheap healthy lunch?", "web").await, None);
}

#[tokio::test]
async fn deal_questions_in_chat_get_nodes_reply() {
    let node = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/secretary/grocery/deals/answer"))
        .and(header("authorization", "Bearer node-token"))
        .and(body_json(json!({"text": "Where is prosciutto crudo cheapest?", "channel": "ios"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "data": {"reply": "Prosciutto crudo is cheapest at Netto Risskov: 19.95 DKK.", "intent": "grocery.deals.cheapest"}})))
        .expect(1)
        .mount(&node)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/secretary/grocery/deals/answer"))
        .and(body_json(json!({"text": "cheap healthy lunch nearby?", "channel": "ios"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "data": {"reply": "Lunch: rugbrød with makrel, 24 DKK at Netto.", "intent": "grocery.deals.lunch"}})))
        .expect(2)
        .mount(&node)
        .await;
    let h = node_harness(&node).await;

    let d = h.chat("Where is prosciutto crudo cheapest?").await;
    assert_eq!(
        d["reply"],
        "Prosciutto crudo is cheapest at Netto Risskov: 19.95 DKK."
    );
    assert_eq!(d["action"], "grocery");
    // The turn is recorded like any other reply.
    let lines = h.timeline_lines();
    assert!(lines
        .iter()
        .any(|l| l["text"] == "Prosciutto crudo is cheapest at Netto Risskov: 19.95 DKK."));

    let d = h.chat("cheap healthy lunch nearby?").await;
    assert_eq!(d["reply"], "Lunch: rugbrød with makrel, 24 DKK at Netto.");

    // An MCP turn too (the allowlist admits the deal intents); the prefix is stripped.
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from claude-code] cheap healthy lunch nearby?", "sessionId": "mcp-claude-code", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["data"]["reply"],
        "Lunch: rugbrød with makrel, 24 DKK at Netto."
    );
}

#[tokio::test]
async fn without_node_deal_questions_fall_back_to_rust_and_others_never_reach_node() {
    let node = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/secretary/grocery/deals/answer"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"ok": false, "error": "not_found"})),
        )
        .expect(2)
        .mount(&node)
        .await;
    let h = node_harness(&node).await;
    let d = h.chat("Where is prosciutto crudo cheapest?").await;
    assert_eq!(
        d["reply"],
        "Nearby deal lookup is unavailable. Check home coordinates and deal sources."
    );
    let d = h.chat("any good deals?").await;
    assert_eq!(d["action"], "grocery");
    // Not a deal question: Node is not asked (the mock expects exactly 2 calls).
    let d = h.chat("ciao come stai").await;
    assert_ne!(d["action"], "grocery");
}
