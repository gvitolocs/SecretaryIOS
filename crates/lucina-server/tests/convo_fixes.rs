//! 2026-10-28 conversation fixes, end to end against the fake DeepSeek: a paused game run no longer forces
//! owner turns through the JSON planner, and a plan that is not JSON twice falls back to a reply about the
//! latest message only.

mod common;

use common::{harness, script};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn game_server(running: bool) -> MockServer {
    let game = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"running": running, "actions": 17274, "events": ["Healing the team"]}),
        ))
        .mount(&game)
        .await;
    Mock::given(method("GET"))
        .and(path("/observe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("MODE: Dialog\nLOCATION: CHAMPIONS_ROOM (map 120) x=4 y=3"),
        )
        .mount(&game)
        .await;
    game
}

fn kinds(h: &common::H) -> Vec<&'static str> {
    h.fake
        .requests()
        .iter()
        .map(lucina_server::fake::kind_of)
        .collect()
}

fn system_of(req: &Value) -> &str {
    req["messages"][0]["content"].as_str().unwrap()
}

// One test: the game server address is process-wide (env), so the two scenarios run in sequence.
#[tokio::test]
async fn paused_game_and_failed_plans() {
    // 1. The run is paused (finished): an unrelated message takes the cheap router, not plan_response, and
    //    the paused block carries no rule that pushes game actions.
    let paused = game_server(false).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"WRONG: the full planner ran\"}"},
            {"kind": "reply", "content": "Ciao!"}
        ]})),
        |_| {},
    )
    .await;
    std::env::set_var("LUCINA_GAME", "true"); // after the harness, which pins it off
    std::env::set_var("LUCINA_GAME_URL", paused.uri());
    let d = h.chat("che ne pensi dei gatti?").await;
    assert_eq!(d["reply"], "Ciao!", "{d}");
    assert!(
        !kinds(&h).contains(&"plan"),
        "a paused run must not force plan_response: {:?}",
        kinds(&h)
    );
    let reqs = h.fake.requests();
    let reply = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .unwrap();
    let system = system_of(reply);
    assert!(system.contains("CHAMPIONS_ROOM"), "kept as information");
    assert!(system.contains("it is paused"));
    assert!(!system.contains("PRIORITY: this live game block outranks"));
    assert!(system.contains("Reply language: Italian"));

    // 2. A playing run does take the planner; its answer is not JSON twice, so she says so briefly in the
    //    language of the message instead of a tool-less guess (no reply call).
    let playing = game_server(true).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "Sure, I will look at the calendar for you."},
            {"kind": "reply", "content": "It is at 12:00."}
        ]})),
        |_| {},
    )
    .await;
    std::env::set_var("LUCINA_GAME", "true");
    std::env::set_var("LUCINA_GAME_URL", playing.uri());
    let now = chrono::Utc::now();
    h.state.recent.append(
        "secretary_test",
        "user",
        "Ho fatto la lavanderia, poi vado alla posta per il pacco",
        now,
    );
    let d = h
        .chat("You know what time is the meeting therese è teresa")
        .await;
    assert_eq!(
        d["reply"],
        "Sorry, I got tangled up working out how to handle that message. Could you send it again?",
        "{d}"
    );
    let ks = kinds(&h);
    assert_eq!(
        ks.iter().filter(|k| **k == "plan").count(),
        2,
        "one plan call and one repair retry: {ks:?}"
    );
    let reqs = h.fake.requests();
    let plans: Vec<&Value> = reqs
        .iter()
        .filter(|r| lucina_server::fake::kind_of(r) == "plan")
        .collect();
    assert_eq!(plans[0]["response_format"], json!({"type": "json_object"}));
    assert!(plans[1]["messages"]
        .to_string()
        .contains("Return only the JSON object for this turn"));
    assert!(
        !ks.contains(&"reply"),
        "no tool-less guess after two failed plans: {ks:?}"
    );
}

/// 10:59: "Explode outlook in your sandbox" (speech-to-text for "Explore ...") is routed to her computer before
/// any planner or reply call; with the computer off the owner is told so.
#[tokio::test]
async fn the_computer_route_comes_before_the_planner() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"WRONG\"}"},
            {"kind": "reply", "content": "WRONG"}
        ]})),
        |_| {},
    )
    .await;
    let d = h.chat("Explode outlook in your sandbox").await;
    assert_eq!(d["action"], "computer_task", "{d}");
    assert!(d["reply"].as_str().unwrap().contains("switched off"), "{d}");
    let ks = kinds(&h);
    assert!(!ks.contains(&"plan") && !ks.contains(&"reply"), "{ks:?}");
}

/// 30 Oct 2026, the owner: "Mentor session 6 november at 12:30". She answered that she could not work out
/// when to remind him and saved nothing. A date, a time and a thing are an event in his own chat.
#[tokio::test]
async fn a_plain_event_message_lands_in_the_calendar() {
    let secretary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/calendar/events"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "lucina:m1", "title": "Mentor session", "start": "2026-11-06T12:30:00+01:00",
            "end": "2026-11-06T13:30:00+01:00", "allDay": false, "source": "lucina"
        })))
        .mount(&secretary)
        .await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"events": []})))
        .mount(&secretary).await;
    let uri = secretary.uri();
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.5,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"WRONG: the planner ran\"}"},
            {"kind": "reply", "content": "x"}
        ]})),
        move |c| {
            c.calendar_enabled = true;
            c.secretary_url = Some(uri.clone());
            let f = c.data_dir.join("svc.token");
            std::fs::write(&f, "svc").unwrap();
            c.secretary_service_token_file = Some(f);
        },
    )
    .await;
    let v = h.chat("Mentor session 6 november at 12:30").await;
    let reply = v["reply"].as_str().unwrap().to_owned();
    assert!(!reply.contains("could not work out"), "{reply}");
    let reqs = secretary.received_requests().await.unwrap();
    let post = reqs.iter().find(|r| r.method.as_str() == "POST" && r.url.path() == "/v1/calendar/events")
        .unwrap_or_else(|| panic!("no event created: {reply}"));
    let body: Value = serde_json::from_slice(&post.body).unwrap();
    assert!(body["title"].as_str().unwrap().contains("Mentor session"), "{body}");
    assert!(body["start"].as_str().unwrap().contains("T12:30"), "{body}");
}

/// The dead end itself: a reminder nobody can place becomes a question.
#[tokio::test]
async fn a_reminder_nobody_can_place_is_a_question() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"schedule_reminder\",\"confidence\":0.99,\"reply_mode\":\"direct\",\"needs_tools\":true}"},
            {"kind": "plan", "content": "{\"action\":\"schedule_reminder\",\"text\":\"the thing\",\"dueDate\":\"\"}"},
            {"kind": "reply", "content": "x"}
        ]})),
        |_| {},
    )
    .await;
    let v = h.chat("remind me about the thing with Therese").await;
    let reply = v["reply"].as_str().unwrap();
    assert!(!reply.contains("could not work out"), "{reply}");
    assert!(reply.contains('?'), "a question: {reply}");
}
