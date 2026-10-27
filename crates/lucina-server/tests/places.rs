//! Place blocks for calendar answers through `/chat` (`LUCINA_RICH_REPLIES`,
//! `LUCINA_CALENDAR`): a fake secretary-server lists the events, a fake map
//! server geocodes the location.

mod common;

use chrono::{Duration, Utc};
use common::{harness, script};
use lucina_server::geocode::GeocodeConfig;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn servers() -> MockServer {
    let at = |h: i64| {
        (Utc::now() + Duration::days(1))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            + Duration::hours(h)
    };
    let tomorrow = at(0).format("%Y-%m-%d").to_string();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/calendar/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"events": [
            {"id": "iphone:42", "title": "Hack Night: Rust meets AI", "start": at(16).to_rfc3339(),
             "end": at(19).to_rfc3339(), "allDay": false, "calendar": "iPhone", "location": "Folkehuset Møllestien"},
            {"id": "google:7", "title": "Compleanno di Anna", "start": tomorrow, "end": tomorrow,
             "allDay": true, "calendar": "Google", "location": null},
        ], "sources": {"google": {"status": "connected"}}})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/maps/geocode"))
        .and(query_param("q", "Folkehuset Møllestien"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"results": [{"name": "Folkehuset Møllestien", "address": "Aarhus", "lat": 56.1611, "lng": 10.2006}]}),
        ))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_calendar_answer_gets_a_place_for_the_event_with_a_location() {
    let server = servers().await;
    let uri = server.uri();
    let h = harness(script(json!({"rules": []})), |c| {
        let token = c.data_dir.join("svc.token");
        std::fs::write(&token, "svc_test\n").unwrap();
        c.rich_replies = true;
        c.calendar_enabled = true;
        c.secretary_url = Some(uri.clone());
        c.secretary_service_token_file = Some(token);
        c.maps_geocode = Some(GeocodeConfig {
            url: format!("{uri}/v1/maps/geocode"),
            key: None,
        });
    })
    .await;
    let d = h.chat("cosa ho in calendario domani?").await;
    assert_eq!(d["action"], "calendar", "{d}");
    let places: Vec<&Value> = d["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "place")
        .collect();
    assert_eq!(places.len(), 1, "{d}");
    let p = places[0];
    assert_eq!(p["title"], "Hack Night: Rust meets AI");
    assert_eq!(p["address"], "Folkehuset Møllestien");
    assert_eq!(p["kind"], "event");
    assert_eq!(p["eventId"], "iphone:42");
    assert!(p["when"].as_str().unwrap().contains("T16:00:00"));
    assert_eq!(p["lat"], 56.1611);
    assert_eq!(p["lng"], 10.2006);
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.contains("📍 Hack Night: Rust meets AI — Folkehuset Møllestien"),
        "{reply}"
    );
    // The short calendar answer was split without a composer call.
    assert!(h
        .fake
        .requests()
        .iter()
        .all(|r| lucina_server::fake::kind_of(r) != "rich"));
}

/// `LUCINA_PLANNER=tools`: the calendar tool's events reach the reply.
async fn tools_turn(server: &MockServer, final_reply: &str) -> Value {
    let uri = server.uri();
    let rules = json!({"rules": [
        {"kind": "agent", "afterTool": true, "content": final_reply},
        {"kind": "agent", "toolCalls": [{"name": "calendar", "arguments": {"action": "list", "range": "tomorrow"}}]},
    ]});
    let h = harness(script(rules), |c| {
        let token = c.data_dir.join("svc.token");
        std::fs::write(&token, "svc_test\n").unwrap();
        c.planner = lucina_server::PlannerMode::Tools;
        c.rich_replies = true;
        c.secretary_url = Some(uri.clone());
        c.secretary_service_token_file = Some(token);
    })
    .await;
    h.chat("che impegni ho domani?").await
}

#[tokio::test]
async fn a_tools_turn_names_the_event_and_gets_its_place() {
    let server = servers().await;
    let d = tools_turn(
        &server,
        "Domani alle 17 hai Hack Night: Rust meets AI, e c'è il compleanno di Anna 🎂",
    )
    .await;
    let places: Vec<&Value> = d["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "place")
        .collect();
    assert_eq!(places.len(), 1, "{d}");
    assert_eq!(places[0]["address"], "Folkehuset Møllestien");
    assert_eq!(places[0]["eventId"], "iphone:42");
    assert!(places[0].get("lat").is_none(), "no geocoder configured");
}

#[tokio::test]
async fn a_tools_turn_that_does_not_name_the_event_gets_no_place() {
    let server = servers().await;
    let d = tools_turn(
        &server,
        "Domani hai solo un paio di cose, niente di urgente.",
    )
    .await;
    assert_eq!(d["format"], "v1", "{d}");
    let listed = server.received_requests().await.unwrap_or_default();
    assert!(
        listed.iter().any(|r| r.url.path() == "/v1/calendar/events"),
        "the calendar tool ran"
    );
    assert!(
        d["blocks"]
            .as_array()
            .is_none_or(|b| b.iter().all(|b| b["type"] != "place")),
        "{d}"
    );
}
