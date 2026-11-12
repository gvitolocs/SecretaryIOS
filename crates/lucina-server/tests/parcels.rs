//! Parcel tracking on a real chat turn (chat::parcel_turn): a message Jev routes to `parcel_track`
//! is answered without a model call, against a stub of the local parcel scraper. Its own test
//! binary: everyday turns use the process-wide Jev client, which reads its settings once.

mod common;

use common::{harness, script, H};
use lucina_server::Config;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NUMBER: &str = "3UW1S8IAAKA23";

struct JevByMessage;

impl Respond for JevByMessage {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let m = body["state"]["message"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        let (intent, p) = if m.contains("track") || m.contains("parcels") {
            ("parcel_track", 0.92)
        } else {
            ("chat", 0.95)
        };
        let other = if intent == "chat" {
            "parcel_track"
        } else {
            "chat"
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": {
                "intent": {"type": "choice", "choice": intent, "confidence": p, "probabilities": {intent: p, other: 1.0 - p}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.9, "probabilities": {"all": 0.9, "today": 0.1}},
                "needs_memory": {"type": "noul", "noul": 0.1},
                "needs_tools": {"type": "noul", "noul": 0.1},
            }
        }))
    }
}

struct DetectByText;

impl Respond for DetectByText {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let found = body["text"]
            .as_str()
            .unwrap_or("")
            .to_uppercase()
            .contains(NUMBER);
        ResponseTemplate::new(200).set_body_json(if found {
            json!({"trackingNumber": NUMBER, "source": "number", "carrier": "poste-italiane", "confidence": "high", "candidates": ["poste-italiane"], "preferred": []})
        } else {
            json!({"trackingNumber": null, "source": "none", "carrier": null, "confidence": "none", "candidates": [], "preferred": []})
        })
    }
}

async fn scraper_stub() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "carrier": "poste-italiane", "source": "poste-italiane", "attempts": [],
            "result": {"current_stage": "in_transit", "last_status_text": "Arrived at the sorting centre", "last_update": "2026-09-28T08:00:00Z",
                       "events": [{"time": "2026-09-28T08:00:00Z", "description": "Arrived", "location": "Milano", "stage": "in_transit", "instant": "2026-09-28T08:00:00Z"}]}
        })))
        .mount(&server)
        .await;
    server
}

/// A `/v1/track` result with two scans given oldest first: the apps see them newest first.
fn scanned() -> Value {
    json!({
        "carrier": "poste-italiane", "source": "poste-italiane", "attempts": [],
        "result": {
            "current_stage": "in_transit",
            "last_status_text": "Arrived at the sorting centre",
            "last_update": "2026-09-28T09:00:00Z",
            "destination_country": "DK",
            "events": [
                {"time": "2026-09-28T08:00:00Z", "description": "Left the hub", "location": "TAASTRUP", "stage": "in_transit", "instant": "2026-09-28T08:00:00Z"},
                {"time": "2026-09-28T09:00:00Z", "description": "Arrived at the sorting centre", "location": "AARHUS DEPOT", "stage": "in_transit", "instant": "2026-09-28T09:00:00Z"}
            ]
        }
    })
}

/// A `/v1/track` result of a parcel that is out for delivery: close to its destination.
fn out_for_delivery() -> Value {
    json!({
        "carrier": "poste-italiane", "source": "poste-italiane", "attempts": [],
        "result": {
            "current_stage": "out_for_delivery",
            "last_status_text": "The delivery of the shipment item is in progress.",
            "last_update": "2026-09-29T07:30:00Z",
            "expected_delivery": "2026-09-29",
            "destination_country": "DK",
            "events": [
                {"time": "2026-09-29T07:30:00Z", "description": "Out for delivery", "location": "AARHUS", "stage": "out_for_delivery", "instant": "2026-09-29T07:30:00Z"}
            ]
        }
    })
}

/// The scraper stub: detection as always, `/v1/track` answering `result`.
async fn scraper_answering(result: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(ResponseTemplate::new(200).set_body_json(result))
        .mount(&server)
        .await;
    server
}

/// How many times the stub has been asked to look a parcel up.
async fn lookups(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/track")
        .count()
}

/// The Parcels API with a stub scraper answering every lookup with `result`.
async fn api(result: Value) -> (H, MockServer) {
    let scraper = scraper_answering(result).await;
    let h = with_scraper(scraper.uri()).await;
    (h, scraper)
}

/// Follows `NUMBER` over the API and gives back the id Lucina chose.
async fn added(h: &H) -> String {
    let (s, b) = h.post("/parcels", json!({"number": NUMBER})).await;
    assert_eq!(s, 200, "{b}");
    b["data"]["parcel"]["id"].as_str().unwrap().to_owned()
}

fn instant(v: &Value) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(v.as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The stand-in Jev of this whole binary. `lucina_jev::shared()` reads its settings once, on its
/// very first use — and building a harness is such a use, so the test that wants Jev routing is not
/// necessarily the one that installs it. Therefore: one stub, mounted before the first harness, on a
/// runtime of its own thread that never ends and so outlives every test in here.
fn jev_stub() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let (gone, came) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("a runtime for the stand-in Jev");
            rt.block_on(async {
                let server = MockServer::start().await;
                Mock::given(wiremock::matchers::any())
                    .respond_with(JevByMessage)
                    .mount(&server)
                    .await;
                let _ = gone.send(server.uri());
                std::future::pending::<()>().await;
            });
        });
        let uri = came.recv().expect("the stand-in Jev came up");
        std::env::set_var("JEV_API_KEY", "test-key");
        std::env::set_var("LUCINA_JEV", "on");
        std::env::set_var("LUCINA_JEV_URL", format!("{uri}/v1/systemone"));
        assert!(
            lucina_jev::shared().available(),
            "the stand-in Jev answers, and the client of the binary is pointed at it"
        );
    });
}

fn chatty() -> lucina_server::fake::Script {
    jev_stub();
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "Oh, ok."},
    ]}))
}
async fn with_scraper(url: String) -> H {
    harness(chatty(), move |c: &mut Config| {
        c.parcels.enabled = true;
        c.parcels.scraper_url = url;
    })
    .await
}

fn stored(h: &H) -> Vec<lucina_parcels::Parcel> {
    h.state
        .tool_env
        .parcels
        .as_ref()
        .map(|s| s.store.list())
        .unwrap_or_default()
}

async fn chat_as(h: &H, session: &str, message: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": message, "sessionId": session, "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

#[test]
fn the_intent_is_one_jev_can_pick_and_lucina_knows() {
    let (_, text) = lucina_server::jev_route::INTENT_OPTIONS
        .iter()
        .find(|(n, _)| *n == "parcel_track")
        .expect("in INTENT_OPTIONS");
    assert!(text.contains("tracking number"));
    assert!(lucina_intent::tier0::INTENTS.contains(&"parcel_track"));
    assert!(lucina_server::jev_route::SIDE_EFFECT_INTENTS.contains(&"parcel_track"));
    assert_eq!(
        lucina_intent::tier0::default_reply_mode("parcel_track"),
        "chat"
    );
    // No regex rule: only the router sends a message here.
    let rule = lucina_intent::tier0::classify_tier0(
        "track this 3UW1S8IAAKA23",
        "",
        &lucina_intent::heuristics::Settings::default(),
    );
    assert!(rule.is_none_or(|c| c.intent != "parcel_track"));
}

#[tokio::test]
async fn jev_routes_parcel_turns() {
    jev_stub();
    assert!(
        lucina_jev::shared().available(),
        "the stand-in Jev of the binary is the one this turn is routed by"
    );
    let scraper = scraper_stub().await;

    // A number: carrier detected, saved, tracked once, answered with the status.
    let h = with_scraper(scraper.uri()).await;
    let d = chat_as(&h, "test", &format!("track this {NUMBER}")).await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with(&format!(
            "Tracking Poste Italiane {NUMBER}: Arrived at the sorting centre"
        )),
        "{reply}"
    );
    assert!(
        reply.contains("every hour once it's close to you"),
        "{reply}"
    );
    assert_eq!(d["action"], "parcel_track");
    let parcels = stored(&h);
    assert_eq!(parcels.len(), 1);
    assert_eq!(
        (parcels[0].number.as_str(), parcels[0].carrier.as_deref()),
        (NUMBER, Some("poste-italiane"))
    );
    assert!(parcels[0].status.is_some());

    // The same number again is the same parcel.
    let d = chat_as(&h, "test", &format!("track this {NUMBER}")).await;
    assert!(
        d["reply"]
            .as_str()
            .unwrap()
            .starts_with("I'm already tracking"),
        "{d}"
    );
    assert_eq!(stored(&h).len(), 1);

    // No number: the list.
    let d = chat_as(&h, "test", "where are my parcels").await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with("I'm tracking 1 parcel:")
            && reply.contains(NUMBER)
            && reply.contains("Arrived at the sorting centre"),
        "{reply}"
    );

    // Nothing tracked: an invitation.
    let empty = with_scraper(scraper.uri()).await;
    let d = chat_as(&empty, "test", "where are my parcels").await;
    assert_eq!(
        d["reply"],
        "I'm not tracking any parcel. Send me a tracking number and I'll follow it."
    );

    // A self-test session looks it up and writes nothing.
    let st = with_scraper(scraper.uri()).await;
    let d = chat_as(&st, "selftest-parcels", &format!("track this {NUMBER}")).await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with("Tracking Poste Italiane") && reply.ends_with(" (self-test: not saved)"),
        "{reply}"
    );
    assert!(stored(&st).is_empty());
    assert!(!st.dir.path().join("parcels").join("parcels.json").exists());

    // An MCP caller's turn is not handled here.
    let mcp = with_scraper(scraper.uri()).await;
    let d = chat_as(
        &mcp,
        "test",
        &format!("[via Lucina MCP, from claude] track this {NUMBER}"),
    )
    .await;
    assert_ne!(d["action"], "parcel_track", "{d}");
    assert!(stored(&mcp).is_empty());

    // The scraper down: the normal turn answers, nothing is saved.
    let down = with_scraper("http://127.0.0.1:1".into()).await;
    let d = chat_as(&down, "test", &format!("track this {NUMBER}")).await;
    assert_ne!(d["action"], "parcel_track", "{d}");
    assert!(stored(&down).is_empty());

    // Off means off: no service, no turn.
    let off = harness(chatty(), |c: &mut Config| c.parcels.enabled = false).await;
    assert!(off.state.tool_env.parcels.is_none());
    let d = chat_as(&off, "test", &format!("track this {NUMBER}")).await;
    assert_ne!(d["action"], "parcel_track", "{d}");

    // Nothing in the stub was asked for a parcel that was not saved or looked up.
    let tracked = scraper
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/track")
        .count();
    assert!(tracked >= 3, "{tracked}");
}

#[tokio::test]
async fn a_carrier_that_needs_more_or_has_no_record_is_said_plainly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "needs postcode", "field": "postcode", "hint": {"kind": "input_required"}})))
        .mount(&server)
        .await;
    let h = with_scraper(server.uri()).await;
    let svc = h.state.tool_env.parcels.clone().unwrap();
    let added = svc.add(NUMBER, None, None, chrono::Utc::now(), true).await;
    assert!(added.saved);
    assert_eq!(
        added.lookup,
        Err(lucina_parcels::TrackError::InputRequired("postcode".into()))
    );
    assert_eq!(stored(&h).len(), 1, "the parcel is kept");
}

#[tokio::test]
async fn the_api_adds_a_parcel_and_shows_its_events_and_schedule() {
    let (h, _scraper) = api(scanned()).await;

    let (s, b) = h
        .post("/parcels", json!({"number": NUMBER, "label": "Headphones"}))
        .await;
    assert_eq!(s, 200, "{b}");
    let p = b["data"]["parcel"].clone();
    let id = p["id"].as_str().unwrap().to_owned();
    assert!(lucina_parcels::valid_id(&id), "{id}");
    assert_eq!(p["number"], NUMBER);
    assert_eq!(p["carrier"], "poste-italiane", "detected from the number");
    assert_eq!(p["label"], "Headphones");
    assert_eq!(p["state"], "active");
    assert_eq!(p["addedAt"], p["lastChecked"], "just looked up");
    assert!(p["lastError"].is_null(), "{p}");

    // In transit far from home with no home city: one check a day, at the digest hour.
    assert_eq!(p["schedule"], "daily");
    assert_eq!(p["near"], json!(false));
    assert!(
        instant(&p["nextCheck"]) >= instant(&p["lastChecked"]),
        "{p}"
    );

    assert_eq!(p["status"]["stage"], "in_transit");
    assert_eq!(
        p["status"]["text"], "Arrived at the sorting centre",
        "the status line"
    );
    assert_eq!(p["status"]["lastUpdate"], "2026-09-28T09:00:00Z");
    assert_eq!(p["status"]["destinationCountry"], "DK");
    assert_eq!(
        p["status"]["location"], "AARHUS DEPOT",
        "the newest scan, not the first"
    );

    let events = p["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["description"], "Arrived at the sorting centre");
    assert_eq!(events[1]["description"], "Left the hub");
    assert_eq!(events[0]["time"], "2026-09-28T09:00:00Z");
    assert_eq!(events[0]["location"], "AARHUS DEPOT");
    assert_eq!(events[0]["stage"], "in_transit");
    assert!(
        events[0].get("instant").is_none(),
        "the apps don't see `instant`"
    );

    let (s, list) = h.get("/parcels").await;
    assert_eq!(s, 200, "{list}");
    assert_eq!(list["data"]["digestHour"], 9);
    let rows = list["data"]["parcels"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["schedule"], "daily");

    let (s, one) = h.get(&format!("/parcels/{id}")).await;
    assert_eq!(s, 200, "{one}");
    let got = &one["data"]["parcel"];
    assert_eq!(got["id"], id);
    assert_eq!(got["events"], p["events"]);
    assert_eq!(got["status"], p["status"]);
    assert_eq!(got["schedule"], p["schedule"]);
    assert!(!got["nextCheck"].is_null(), "{got}");
}

#[tokio::test]
async fn a_parcel_close_to_its_destination_is_hourly_and_due_in_an_hour() {
    let (h, _scraper) = api(out_for_delivery()).await;
    let id = added(&h).await;
    let (s, one) = h.get(&format!("/parcels/{id}")).await;
    assert_eq!(s, 200, "{one}");
    let p = &one["data"]["parcel"];
    assert_eq!(p["state"], "active");
    assert_eq!(p["schedule"], "hourly");
    assert_eq!(p["near"], json!(true));
    assert_eq!(p["status"]["stage"], "out_for_delivery");
    assert_eq!(p["status"]["expectedDelivery"], "2026-09-29");
    assert_eq!(
        instant(&p["nextCheck"]) - instant(&p["lastChecked"]),
        chrono::Duration::hours(1),
        "{p}"
    );
}

#[tokio::test]
async fn a_refresh_inside_ten_minutes_does_not_ask_the_carrier_again() {
    let (h, scraper) = api(scanned()).await;
    let id = added(&h).await;
    assert_eq!(lookups(&scraper).await, 1, "the add looked it up once");

    let (s, b) = h
        .post(&format!("/parcels/{id}/refresh"), json!({"ignored": true}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["parcel"]["id"], id);
    assert_eq!(
        lookups(&scraper).await,
        1,
        "under ten minutes old: the stored parcel answers"
    );

    // The same parcel, checked eleven minutes ago: the carrier is asked again.
    let svc = h.state.tool_env.parcels.clone().unwrap();
    let older = chrono::Utc::now() - chrono::Duration::minutes(11);
    assert!(svc
        .store
        .update(&id, |p| p.last_checked = Some(older))
        .is_some());
    let (s, _) = h.post(&format!("/parcels/{id}/refresh"), json!({})).await;
    assert_eq!(s, 200);
    assert_eq!(lookups(&scraper).await, 2);
}

#[tokio::test]
async fn the_api_renames_a_parcel_and_forgets_it() {
    let (h, _scraper) = api(scanned()).await;
    let id = added(&h).await;

    let (s, b, _) = h
        .call(
            reqwest::Method::PATCH,
            &format!("/parcels/{id}"),
            Some(json!({"label": "Headphones"})),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["parcel"]["label"], "Headphones");
    let (s, one) = h.get(&format!("/parcels/{id}")).await;
    assert_eq!(
        (s, one["data"]["parcel"]["label"].clone()),
        (200, json!("Headphones")),
        "{one}"
    );

    // Only the label, and at most 80 characters.
    for body in [
        json!({"label": "x".repeat(81)}),
        json!({"number": "ABCD"}),
        json!({}),
        json!({"label": 7}),
    ] {
        let (s, b, _) = h
            .call(
                reqwest::Method::PATCH,
                &format!("/parcels/{id}"),
                Some(body.clone()),
            )
            .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (400, Some("bad_request")),
            "{body}: {b}"
        );
    }
    let (s, b, _) = h
        .call(
            reqwest::Method::PATCH,
            &format!("/parcels/{id}"),
            Some(json!({"label": null})),
        )
        .await;
    assert_eq!(
        (s, b["data"]["parcel"]["label"].clone()),
        (200, Value::Null),
        "{b}"
    );

    let (s, b, _) = h
        .call(reqwest::Method::DELETE, &format!("/parcels/{id}"), None)
        .await;
    assert_eq!((s, b["data"].clone()), (200, json!({"ok": true})), "{b}");
    assert_eq!(h.get(&format!("/parcels/{id}")).await.0, 404);
    assert!(stored(&h).is_empty());
    assert_eq!(h.get("/parcels").await.1["data"]["parcels"], json!([]));
}

#[tokio::test]
async fn the_api_refuses_a_number_it_should_not_send_to_the_carrier() {
    let (h, scraper) = api(scanned()).await;
    for body in [
        json!({}),
        json!({"number": "abc"}),
        json!({"number": "x".repeat(81)}),
        json!({"number": 7}),
        json!([1]),
        json!({"number": NUMBER, "size": "large"}),
    ] {
        let (s, b) = h.post("/parcels", body.clone()).await;
        assert_eq!(
            (s, b["error"].as_str()),
            (400, Some("bad_request")),
            "{body}: {b}"
        );
    }
    assert!(stored(&h).is_empty());
    assert_eq!(
        lookups(&scraper).await,
        0,
        "none of them reached the scraper"
    );
}

#[tokio::test]
async fn a_number_the_carrier_calls_invalid_is_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "not a number", "hint": {"kind": "invalid_input"}})),
        )
        .mount(&server)
        .await;
    let h = with_scraper(server.uri()).await;
    let (s, b) = h.post("/parcels", json!({"number": NUMBER})).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("bad_request")), "{b}");
    assert!(
        stored(&h).is_empty(),
        "an answer, not an outage: don't keep it"
    );
}

#[tokio::test]
async fn an_unknown_parcel_id_is_a_404_with_lucinas_code() {
    let (h, _scraper) = api(scanned()).await;
    for (method, path) in [
        (reqwest::Method::GET, "/parcels/pc-00000000"),
        (reqwest::Method::DELETE, "/parcels/pc-00000000"),
        (reqwest::Method::GET, "/parcels/pc-1A2B3C4D"),
    ] {
        let (s, b, _) = h.call(method.clone(), path, None).await;
        assert_eq!(
            (s, b["error"].as_str()),
            (404, Some("not_found")),
            "{method} {path}: {b}"
        );
    }
    for (method, path) in [
        (reqwest::Method::POST, "/parcels/pc-00000000/refresh"),
        (reqwest::Method::PATCH, "/parcels/pc-00000000"),
    ] {
        let (s, b, _) = h
            .call(method.clone(), path, Some(json!({"label": "nope"})))
            .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (404, Some("not_found")),
            "{method} {path}: {b}"
        );
    }

    // Off is an answer too, and not a `not_found`: secretary-server turns it into 503.
    let off = harness(chatty(), |c: &mut Config| c.parcels.enabled = false).await;
    let (s, b) = off.get("/parcels").await;
    assert_eq!(
        (s, b["error"].as_str()),
        (404, Some("parcels_disabled")),
        "{b}"
    );
}
