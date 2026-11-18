//! Agenda answers through `/chat` (`LUCINA_RICH_REPLIES`, `LUCINA_CALENDAR`): one sentence and
//! one card per event instead of a wall of text, and questions about one event ("Dove si tiene
//! la Hack Night di domani e a che ora?") answered from the calendar on both planners. A fake
//! secretary-server lists the owner's real Tuesday; a fake map server geocodes and routes.

mod common;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use common::{harness, script, H};
use lucina_server::geocode::GeocodeConfig;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TZ: Tz = chrono_tz::Europe::Copenhagen;

/// Tomorrow (in the owner's zone) at `h`:00 UTC.
fn at(h: i64) -> DateTime<Utc> {
    (Utc::now().with_timezone(&TZ) + Duration::days(1))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        + Duration::hours(h)
}

fn hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&TZ).format("%H:%M").to_string()
}

async fn servers() -> MockServer {
    let tomorrow = at(0).format("%Y-%m-%d").to_string();
    let day_after = (at(0) + Duration::days(1)).format("%Y-%m-%d").to_string();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/calendar/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"events": [
            {"id": "google:7", "title": "Compleanno di Anna", "start": tomorrow, "end": day_after,
             "allDay": true, "calendar": "vitologiuseppe17@gmail.com"},
            {"id": "ics:au:1", "title": "Security and Privacy (Aarhus Universitet - Personal timetable: 810677)",
             "calendar": "Aarhus Universitet - Personal timetable: 810677", "start": at(7).to_rfc3339(),
             "end": at(9).to_rfc3339(), "allDay": false, "location": "5789-003B Seminarrum"},
            {"id": "google:standup", "title": "Standup vitologiuseppe17@gmail.com", "calendar": "vitologiuseppe17@gmail.com",
             "start": at(11).to_rfc3339(), "end": at(12).to_rfc3339(), "allDay": false},
            {"id": "iphone:42", "title": "Hack Night: Rust meets AI", "start": at(16).to_rfc3339(),
             "end": at(19).to_rfc3339(), "allDay": false, "calendar": "iPhone", "location": "Folkehuset Møllestien"},
        ], "sources": {"google": {"status": "connected"}}})))
        .mount(&server)
        .await;
    for (q, lat, lng) in [
        ("Folkehuset Møllestien", 56.1611, 10.2006),
        ("5789-003B Seminarrum", 56.1685, 10.2034),
    ] {
        Mock::given(method("GET"))
            .and(path("/v1/maps/geocode"))
            .and(query_param("q", q))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"results": [{"name": q, "address": "Aarhus", "lat": lat, "lng": lng}]}),
            ))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/v1/maps/config"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"home": {"lat": 56.19633, "lng": 10.2011, "label": "Home"}})),
        )
        .mount(&server)
        .await;
    // No Google key on the map server: straight-line estimates.
    Mock::given(method("GET"))
        .and(path("/v1/maps/routes"))
        .and(query_param("estimate", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"provider": "estimate",
            "routes": [{"mode": "walk", "durationS": 3000, "estimate": true}, {"mode": "bike", "durationS": 960, "estimate": true}],
            "failed": ["transit"]})))
        .mount(&server)
        .await;
    server
}

async fn lucina(server: &MockServer, rules: Value, tools: bool) -> H {
    let uri = server.uri();
    harness(script(rules), |c| {
        let token = c.data_dir.join("svc.token");
        std::fs::write(&token, "svc_test\n").unwrap();
        c.rich_replies = true;
        c.calendar_enabled = true;
        c.time_zone = TZ;
        if tools {
            c.planner = lucina_server::PlannerMode::Tools;
        }
        c.secretary_url = Some(uri.clone());
        c.secretary_service_token_file = Some(token);
        c.maps_geocode = Some(GeocodeConfig {
            url: format!("{uri}/v1/maps/geocode"),
            key: None,
        });
    })
    .await
}

/// What the model would say without the calendar (the bug: "Non ho niente sulla Hack Night…").
fn from_memory() -> Value {
    json!({"rules": [
        {"kind": "agent", "content": "Non ho niente sulla Hack Night nelle tue note."},
    ], "default": {"content": "{\"action\":\"reply\",\"text\":\"Non ho niente sulla Hack Night nelle tue note.\"}"}})
}

fn kinds(h: &H) -> Vec<&'static str> {
    h.fake
        .requests()
        .iter()
        .map(lucina_server::fake::kind_of)
        .collect()
}

#[tokio::test]
async fn an_agenda_is_one_sentence_and_one_card_per_event() {
    let server = servers().await;
    let h = lucina(&server, json!({"rules": []}), false).await;
    let d = h.chat("cosa ho in calendario domani?").await;
    assert_eq!(d["action"], "calendar", "{d}");
    assert_eq!(d["bubbles"], json!(["Tomorrow you have 4 events 📅"]));
    let blocks = d["blocks"].as_array().unwrap();
    let shape: Vec<(&str, &str)> = blocks
        .iter()
        .map(|b| (b["type"].as_str().unwrap(), b["title"].as_str().unwrap()))
        .collect();
    assert_eq!(
        shape,
        [
            ("card", "Compleanno di Anna"),
            ("place", "Security and Privacy"),
            ("card", "Standup"),
            ("place", "Hack Night: Rust meets AI"),
        ]
    );
    assert_eq!(blocks[0]["subtitle"], "all day");
    assert_eq!(
        blocks[2]["subtitle"],
        format!("{}–{}", hm(at(11)), hm(at(12)))
    );
    let hack = &blocks[3];
    assert_eq!(
        (hack["kind"].as_str(), hack["eventId"].as_str()),
        (Some("event"), Some("iphone:42"))
    );
    assert_eq!(
        (hack["lat"].clone(), hack["lng"].clone()),
        (json!(56.1611), json!(10.2006))
    );
    assert_eq!(
        (
            hack["travelMin"].clone(),
            hack["travelMode"].clone(),
            hack["travelApprox"].clone()
        ),
        (json!(16), json!("bike"), json!(true))
    );
    // A "Navigate" button per place, to the in-app map.
    let actions = d["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2, "{d}");
    for a in actions {
        assert_eq!(a["kind"], "route");
        assert!(a["label"].as_str().unwrap().starts_with("Navigate · "));
        assert!(a["payload"]["path"]
            .as_str()
            .unwrap()
            .starts_with("/map/place?navigate=1&lat="));
    }
    assert!(actions[1]["payload"]["path"]
        .as_str()
        .unwrap()
        .contains("title=Hack%20Night%3A%20Rust%20meets%20AI"));
    // Plain surfaces: one compact line per event.
    assert_eq!(
        d["reply"],
        format!(
            "Tomorrow you have 4 events 📅\n\n\
             • all day Compleanno di Anna\n\
             • {}–{} Security and Privacy — 5789-003B Seminarrum · 🚲 ≈ 16 min\n\
             • {}–{} Standup\n\
             • {}–{} Hack Night: Rust meets AI — Folkehuset Møllestien · 🚲 ≈ 16 min",
            hm(at(7)),
            hm(at(9)),
            hm(at(11)),
            hm(at(12)),
            hm(at(16)),
            hm(at(19))
        )
    );
    let all = d.to_string();
    for bad in [
        "📍",
        "timetable",
        "810677",
        "@gmail.com",
        "maps.apple.com",
        "google.com/maps",
    ] {
        assert!(!all.contains(bad), "{bad} in {all}");
    }
    assert!(kinds(&h).iter().all(|k| *k != "rich"), "no composer call");
    // The timeline keeps the cards and the compact text.
    let item = h.timeline_lines().pop().unwrap();
    assert_eq!(item["meta"]["blocks"].as_array().unwrap().len(), 4);
    assert_eq!(item["text"], d["reply"]);
}

#[tokio::test]
async fn where_and_when_of_the_hack_night_come_from_the_calendar() {
    // Regression: answered "Non ho niente sulla Hack Night…" from memory and notes while the
    // event was in the calendar.
    for tools in [false, true] {
        let server = servers().await;
        let h = lucina(&server, from_memory(), tools).await;
        let d = h
            .chat("Dove si tiene la Hack Night di domani e a che ora?")
            .await;
        let expected = format!(
            "Hack Night: Rust meets AI is tomorrow at {}, at Folkehuset Møllestien.",
            hm(at(16))
        );
        assert_eq!(d["action"], "calendar", "tools={tools} {d}");
        assert_eq!(d["bubbles"], json!([expected]), "tools={tools}");
        assert_eq!(
            d["blocks"].as_array().unwrap().len(),
            1,
            "only the Hack Night: {d}"
        );
        assert_eq!(d["blocks"][0]["eventId"], "iphone:42");
        assert_eq!(d["actions"][0]["label"], "Navigate");
        assert!(!d.to_string().contains("Non ho niente"));
        let tomorrow = at(0).format("%Y-%m-%d").to_string();
        let asked = server.received_requests().await.unwrap_or_default();
        assert!(
            asked.iter().any(|r| r.url.path() == "/v1/calendar/events"
                && r.url
                    .query()
                    .is_some_and(|q| q == format!("from={tomorrow}&to={tomorrow}"))),
            "tools={tools}: the calendar was read for tomorrow"
        );
        assert!(
            kinds(&h).iter().all(|k| *k != "plan" && *k != "agent"),
            "tools={tools}: no model answered from memory: {:?}",
            kinds(&h)
        );
    }
}

#[tokio::test]
async fn the_next_lecture_is_a_card() {
    let server = servers().await;
    let h = lucina(&server, from_memory(), false).await;
    let d = h.chat("when is my next lecture?").await;
    assert_eq!(
        d["bubbles"],
        json!([format!(
            "Security and Privacy is tomorrow at {}, at 5789-003B Seminarrum.",
            hm(at(7))
        )])
    );
    assert_eq!(d["actions"][0]["label"], "Navigate");
}

/// 17 Nov 2026, the owner correcting one hour for another: the direct rule reads his whole
/// sentence as the event name and files it with that title. The sentence names no event, so the
/// planner — which sees the recent turns — is the one asked to name it.
#[tokio::test]
async fn a_correction_of_one_hour_lets_the_planner_name_the_event() {
    let server = servers().await;
    Mock::given(method("POST"))
        .and(path("/v1/calendar/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "lucina:c1", "title": "Call with the clinic", "start": at(9).to_rfc3339(),
            "end": at(10).to_rfc3339(), "allDay": false, "source": "lucina"
        })))
        .mount(&server)
        .await;
    // Tomorrow (in the owner's zone) at 10:00, as the planner is told to put it.
    let day = (Utc::now().with_timezone(&TZ) + Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let plan = format!(
        "{{\"action\":\"calendar_event\",\"title\":\"Call with the clinic\",\"start\":\"{day}T10:00\"}}"
    );
    let h = lucina(
        &server,
        json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"calendar_event\",\"confidence\":0.95,\"reply_mode\":\"direct\",\"needs_tools\":false}"},
            {"kind": "plan", "content": plan}
        ], "default": {"content": "{\"action\":\"reply\",\"text\":\"WRONG: the model answered this turn.\"}"}}),
        false,
    )
    .await;
    let correction =
        "That is the hour of the chat, not of the meeting, the meeting is tomorrow at 10";
    let d = h.chat(correction).await;
    let asked = server.received_requests().await.unwrap_or_default();
    let post = asked
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v1/calendar/events")
        .unwrap_or_else(|| panic!("no event created: {d}"));
    let body: Value = serde_json::from_slice(&post.body).unwrap();
    let title = body["title"].as_str().unwrap_or_default().to_owned();
    assert!(
        !title.contains("hour of the chat"),
        "the sentence became the event name: {body}"
    );
    assert_eq!(
        title, "Call with the clinic",
        "the planner named it: {body}"
    );
    assert!(
        body["start"].as_str().unwrap_or_default().starts_with(&day),
        "{body}"
    );
    assert!(
        !d["reply"].as_str().unwrap_or_default().contains(correction),
        "{d}"
    );
}

#[tokio::test]
async fn an_event_the_calendar_does_not_have_goes_to_the_planner_with_that_fact() {
    let server = servers().await;
    let h = lucina(
        &server,
        json!({"default": {"content": "{\"action\":\"reply\",\"text\":\"Non la trovo in calendario.\"}"}}),
        false,
    )
    .await;
    let d = h.chat("Dove si tiene la cena da Marco domani?").await;
    assert_ne!(d["action"], "calendar", "{d}");
    let told = h.fake.requests().iter().any(|r| {
        r.to_string()
            .contains(r#"no event matches \"cena da marco\""#)
    });
    assert!(
        told,
        "the planner's context says the calendar has no such event"
    );
}
