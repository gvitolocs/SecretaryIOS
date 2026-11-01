//! LucinaVoice Jev fast path (docs/lucinavoice-protocol.md section 6): a spoken turn is routed by Jev at the
//! start of the turn; a fast intent is answered from the stores with a templated reply and no model call,
//! everything else (low confidence, a slow or failing Jev, another tier-0 rule, a slow intent) takes today's
//! path. Jev is a fake HTTP server (wiremock) that answers per message; the model is the fake DeepSeek, so a
//! fast turn shows as "the fake saw no request".

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Utc};
use common::{harness, script, H, TOKEN};
use lucina_server::Config;
use serde_json::{json, Value};
use wiremock::{Match, Mock, MockServer, Request, Respond, ResponseTemplate};

const INTENT_CHAT: &str =
    "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}";
const REPLY: &str = "Ciao Giuseppe, sto bene. E tu come stai oggi?";

// ------------------------------------------------------------------------------------------ fake Jev

#[derive(Clone)]
struct Verdict {
    intent: &'static str,
    confidence: f64,
    scope: &'static str,
    delay: Duration,
    /// An HTTP status other than 200.
    status: u16,
}

fn say(intent: &'static str, confidence: f64) -> Verdict {
    Verdict {
        intent,
        confidence,
        scope: "all",
        delay: Duration::ZERO,
        status: 200,
    }
}

/// Answers the intent questions per `state.message`; unknown messages are `unknown` at 0.3.
#[derive(Clone, Default)]
struct JevScript {
    by_message: Arc<Mutex<HashMap<String, Verdict>>>,
    states: Arc<Mutex<Vec<Value>>>,
}

impl JevScript {
    fn on(self, message: &str, v: Verdict) -> Self {
        self.by_message.lock().unwrap().insert(message.into(), v);
        self
    }
}

fn answer(v: &Verdict) -> Value {
    let other = if v.intent == "unknown" {
        "chat"
    } else {
        "unknown"
    };
    json!({"model": "jev-1.14.0", "usage": {"input_tokens": 100, "output_tokens": 10}, "answers": {
        "intent": {"type": "choice", "choice": v.intent, "confidence": v.confidence,
            "probabilities": {v.intent: v.confidence, other: 1.0 - v.confidence}},
        "scope": {"type": "choice", "choice": v.scope, "confidence": 0.9,
            "probabilities": {v.scope: 0.9, if v.scope == "today" { "all" } else { "today" }: 0.1}},
        "needs_memory": {"type": "noul", "noul": 0.1},
        "needs_tools": {"type": "noul", "noul": 0.1},
    }})
}

impl Respond for JevScript {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let state = body["state"].clone();
        let message = state["message"].as_str().unwrap_or("").to_owned();
        self.states.lock().unwrap().push(state);
        let v = self
            .by_message
            .lock()
            .unwrap()
            .get(&message)
            .cloned()
            .unwrap_or_else(|| say("unknown", 0.3));
        if v.status != 200 {
            return ResponseTemplate::new(v.status);
        }
        ResponseTemplate::new(200)
            .set_body_json(answer(&v))
            .set_delay(v.delay)
    }
}

struct Any;
impl Match for Any {
    fn matches(&self, _: &Request) -> bool {
        true
    }
}

async fn fake_jev(s: &JevScript) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(Any)
        .respond_with(s.clone())
        .mount(&server)
        .await;
    server
}

fn with_jev(server: &MockServer) -> impl FnOnce(&mut Config) + '_ {
    let url = format!("{}/v1/systemone", server.uri());
    move |c: &mut Config| {
        c.voice_jev = Some(lucina_jev::Config {
            url,
            model: "jev-1.14.0".into(),
            key: Some("test-key".into()),
            timeout: Duration::from_secs(5),
            enabled: true,
        });
        // The tests run on a busy machine in a debug build: a generous cap, so that only the tests of
        // the cap itself depend on it (the production default is checked in `the_defaults_…`).
        c.voice_jev_cap = Duration::from_secs(3);
    }
}

async fn jev_calls(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

// ------------------------------------------------------------------------------------------- the SSE

struct Ev {
    name: String,
    data: Value,
    at: Instant,
}

struct Sse {
    events: Vec<Ev>,
}

impl Sse {
    fn names(&self) -> Vec<&str> {
        self.events.iter().map(|e| e.name.as_str()).collect()
    }
    fn find(&self, name: &str) -> &Ev {
        self.events
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no {name} event in {:?}", self.names()))
    }
    fn count(&self, name: &str) -> usize {
        self.events.iter().filter(|e| e.name == name).count()
    }
}

async fn stream(h: &H, body: Value) -> (Sse, Instant) {
    let started = Instant::now();
    let mut resp = h
        .http
        .post(h.url("/chat"))
        .bearer_auth(TOKEN)
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut out = Sse { events: Vec::new() };
    let mut buf = String::new();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find("\n\n") {
            let frame: String = buf.drain(..i + 2).collect();
            let at = Instant::now();
            let (mut name, mut data) = (None, String::new());
            for line in frame.lines() {
                if let Some(n) = line.strip_prefix("event:") {
                    name = Some(n.trim().to_owned());
                } else if let Some(d) = line.strip_prefix("data:") {
                    data.push_str(d.trim());
                }
            }
            if let Some(name) = name {
                out.events.push(Ev {
                    name,
                    data: serde_json::from_str(&data).unwrap_or(Value::Null),
                    at,
                });
            }
        }
    }
    (out, started)
}

fn chat_body(message: &str) -> Value {
    json!({"message": message, "sessionId": "voice", "channel": "web", "voice": true})
}

fn chat_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": INTENT_CHAT},
        {"kind": "reply", "content": REPLY},
    ]}))
}

/// The unsure classification and the plan of a turn that needs the planner.
fn plan_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\",\"needs_tools\":false}"},
        {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Va bene.\"}"},
    ]}))
}

fn model_calls(h: &H) -> usize {
    h.fake.requests().len()
}

fn timeline_texts(h: &H) -> Vec<(String, String)> {
    h.timeline_lines()
        .iter()
        .map(|l| {
            (
                l["role"].as_str().unwrap_or("").to_owned(),
                l["text"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

/// The tier-0 and parser regexes compile on first use (a second or so in a debug build): do it before a
/// test that measures time.
fn warm() {
    let s = lucina_intent::heuristics::Settings::default();
    for t in [
        "che ore sono?",
        "ricordami domani alle 9 di pagare",
        "aggiungi latte alla lista della spesa",
    ] {
        let _ = lucina_intent::tier0::classify_tier0(t, "", &s);
        let _ = lucina_intent::grocery::parse_grocery_intent(t);
        let _ = lucina_store::parser::parse_reminder_message(
            t,
            &lucina_store::parser::ParseOptions {
                tz: chrono_tz::Europe::Rome,
                default_hour: 9,
                default_minute: 0,
                now: Utc::now(),
            },
        );
    }
    let _ = lucina_intent::domains::read_domains(
        "che ore sono?",
        Utc::now(),
        chrono_tz::Europe::Rome,
        false,
    );
    let _ = lucina_intent::notes::parse_notes_intent("che ore sono?", false);
    let _ = lucina_intent::heuristics::reminder_list_scope("che promemoria ho?");
    let _ = lucina_secrets::redactor().redact("warm");
}

fn rome_now() -> chrono::DateTime<chrono_tz::Tz> {
    Utc::now().with_timezone(&chrono_tz::Europe::Rome)
}

// ------------------------------------------------------------------------------------------ the tests

#[tokio::test]
async fn the_time_and_the_date_are_answered_without_a_model() {
    let jev = JevScript::default()
        .on("che ore sono?", say("temporal_query", 0.95))
        .on("che giorno è oggi?", say("temporal_query", 0.93))
        .on("what time is it?", say("temporal_query", 0.9));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;

    let (s, _) = stream(&h, chat_body("che ore sono?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"], "no delta, no ack");
    let route = &s.find("route").data;
    assert_eq!(route["intent"], "temporal_query");
    assert_eq!(route["fast"], true);
    assert_eq!(route["source"], "jev");
    assert!(route["confidence"].as_f64().unwrap() > 0.94);
    assert!(route["jev_ms"].as_u64().is_some());
    assert!(
        !route.to_string().contains("ore sono"),
        "no transcript in the route"
    );
    let reply = &s.find("reply").data;
    let text = reply["text"].as_str().unwrap();
    assert!(
        text.starts_with("Sono le ") || text.starts_with("È "),
        "{text}"
    );
    assert!(
        text.ends_with('.') && !text.contains(':'),
        "said, not written: {text}"
    );
    assert_eq!(reply["sensitive"], false);
    assert_eq!(reply["replaces"], true, "nothing was streamed");
    // `done` is the whole non-streaming body, with the same reply.
    let done = &s.find("done").data;
    assert_eq!(done["ok"], true);
    assert_eq!(done["data"]["reply"], text);
    assert_eq!(done["data"]["action"], "reply");
    assert!(done["data"]["lucinaItemId"].as_str().is_some());
    // The state Jev saw: facts only, one template.
    let seen = jev.states.lock().unwrap()[0].clone();
    let keys: Vec<&str> = seen
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["channel", "local_time", "message", "time_zone", "weekday"]
    );
    assert_eq!(seen["channel"], "voice");
    assert_eq!(seen["message"], "che ore sono?");
    assert_eq!(seen["time_zone"], "Europe/Rome");

    let (s, _) = stream(&h, chat_body("che giorno è oggi?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    let day = rome_now().day();
    let text = s.find("reply").data["text"].as_str().unwrap().to_owned();
    assert!(text.starts_with("Oggi è "), "{text}");
    // Midnight may pass between the turn and this check.
    assert!(
        text.contains(&format!(" {day} ")) || text.contains(&format!(" {} ", rome_now().day())),
        "{text}"
    );

    let (s, _) = stream(&h, chat_body("what time is it?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    let text = s.find("reply").data["text"].as_str().unwrap().to_owned();
    assert!(text.starts_with("It's ") && text.ends_with('.'), "{text}");

    // Zero model calls in all three turns; the timeline got the user and the Lucina items each time.
    assert_eq!(model_calls(&h), 0, "{:?}", h.fake.requests());
    let tl = timeline_texts(&h);
    assert_eq!(tl.iter().filter(|(r, _)| r == "user").count(), 3, "{tl:?}");
    assert_eq!(
        tl.iter().filter(|(r, _)| r == "lucina").count(),
        3,
        "{tl:?}"
    );
    assert_eq!(tl[0], ("user".to_owned(), "che ore sono?".to_owned()));
    assert!(tl[1].1.starts_with("Sono le ") || tl[1].1.starts_with("È "));
}

#[tokio::test]
async fn a_clock_intent_that_needs_data_takes_the_normal_path() {
    // Jev is sure, but this is a flight time, not the clock: the sentence is not a plain time question.
    let msg = "a che ora parte il volo per Londra?";
    let jev = JevScript::default().on(msg, say("temporal_query", 0.97));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(&h, chat_body(msg)).await;
    let route = &s.find("route").data;
    assert_eq!(route["intent"], "temporal_query");
    assert_eq!(route["fast"], false);
    assert_eq!(route["source"], "jev");
    assert_eq!(s.find("reply").data["text"], REPLY);
    assert!(model_calls(&h) >= 1);
}

#[tokio::test]
async fn low_confidence_takes_the_normal_path() {
    let jev = JevScript::default().on("che ore sono?", say("temporal_query", 0.6));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(&h, chat_body("che ore sono?")).await;
    let names = s.names();
    assert_eq!(names[0], "route", "{names:?}");
    assert_eq!(s.find("route").data["fast"], false);
    // The planner path: the casual chat reply streams, so there are deltas and no ack.
    assert!(s.count("delta") >= 1, "{names:?}");
    assert_eq!(s.count("ack"), 0, "{names:?}");
    assert_eq!(s.find("reply").data["text"], REPLY);
    assert!(model_calls(&h) >= 1);

    // The threshold is configurable: at 0.5 the same verdict is fast.
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), |c| {
        with_jev(&server)(c);
        c.voice_fast_min = 0.5;
    })
    .await;
    let (s, _) = stream(&h, chat_body("che ore sono?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(model_calls(&h), 0);
}

#[tokio::test]
async fn a_slow_jev_is_cut_off_at_the_cap_and_the_turn_still_succeeds() {
    let mut slow = say("temporal_query", 0.99);
    slow.delay = Duration::from_millis(5000);
    warm();
    let jev = JevScript::default().on("che ore sono?", slow);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), |c| {
        with_jev(&server)(c);
        c.voice_jev_cap = Duration::from_millis(150);
    })
    .await;
    let (s, started) = stream(&h, chat_body("che ore sono?")).await;
    assert!(
        started.elapsed() < Duration::from_millis(4000),
        "the turn did not wait for Jev"
    );
    let route = &s.find("route").data;
    assert_eq!(route["fast"], false);
    assert_ne!(route["source"], "jev", "{route}");
    assert!(route["jev_ms"].as_u64().unwrap() >= 140);
    assert_eq!(s.find("reply").data["text"], REPLY);
    assert_eq!(s.find("done").data["ok"], true);
    let tl = timeline_texts(&h);
    assert!(
        tl.contains(&("lucina".to_owned(), REPLY.to_owned())),
        "{tl:?}"
    );
}

#[tokio::test]
async fn a_failing_or_unconfigured_jev_leaves_the_turn_on_todays_path() {
    // A server error.
    let mut err = say("temporal_query", 0.99);
    err.status = 500;
    let jev = JevScript::default().on("che ore sono?", err);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(&h, chat_body("che ore sono?")).await;
    assert_eq!(s.find("route").data["fast"], false);
    assert_ne!(s.find("route").data["source"], "jev");
    assert_eq!(s.find("reply").data["text"], REPLY);
    assert_eq!(s.find("done").data["ok"], true);

    // Jev not configured at all (the tests' default: no key): today's stream, event for event, with no
    // route and no ack.
    let h = harness(chat_script(), |_| {}).await;
    let (s, _) = stream(&h, chat_body("che ore sono?")).await;
    assert_eq!(s.count("route") + s.count("ack"), 0, "{:?}", s.names());
    assert_eq!(s.find("reply").data["text"], REPLY);
    let h = harness(plan_script(), |_| {}).await;
    let (s, _) = stream(&h, chat_body("draw a cat")).await;
    assert_eq!(s.names(), ["reply", "done"]);
}

#[tokio::test]
async fn a_tier0_rule_for_another_intent_beats_jev() {
    // "quando scade …" is a tier-0 temporal rule (a deadline), not the reminders list Jev reads into it.
    let msg = "quando scade l'abbonamento?";
    let jev = JevScript::default().on(msg, say("list_reminders", 0.98));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(&h, chat_body(msg)).await;
    assert_eq!(s.find("route").data["fast"], false);
    assert_eq!(s.find("reply").data["text"], REPLY);
}

#[tokio::test]
async fn reminders_are_read_from_the_store() {
    let jev = JevScript::default()
        .on("quali promemoria ho oggi?", {
            let mut v = say("list_reminders", 0.96);
            v.scope = "today";
            v
        })
        .on("che promemoria ho?", say("list_reminders", 0.9))
        .on("what reminders do I have?", say("list_reminders", 0.9));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;

    let (s, _) = stream(&h, chat_body("quali promemoria ho oggi?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(s.find("route").data["intent"], "list_reminders");
    assert_eq!(s.find("route").data["fast"], true);
    assert_eq!(s.find("reply").data["text"], "Non hai promemoria per oggi.");
    assert_eq!(s.find("done").data["data"]["action"], "list_reminders");

    // Two reminders, tomorrow 09:00 and 17:30 (never "today", whatever time the test runs).
    let tomorrow = (rome_now() + chrono::Duration::days(1)).date_naive();
    for (hh, mm, text) in [(17, 30, "chiamare Anna"), (9, 0, "pagare l'affitto")] {
        let due = lucina_store::timeutil::zoned_to_utc(
            chrono_tz::Europe::Rome,
            tomorrow.year(),
            tomorrow.month(),
            tomorrow.day(),
            hh,
            mm,
            0,
        )
        .unwrap();
        h.state
            .writer
            .create(lucina_store::reminders::CreateReminder {
                text: text.into(),
                due_at: lucina_store::timeutil::iso_ms(due),
                chat_id: "42".into(),
                user_peer_id: "owner".into(),
                user_label: "Giuseppe".into(),
                persona: "Lucina".into(),
                time_zone: "Europe/Rome".into(),
                dedupe_key: None,
                source: "secretary".into(),
            })
            .await
            .unwrap();
    }
    let (s, _) = stream(&h, chat_body("che promemoria ho?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(
        s.find("reply").data["text"],
        "Hai due promemoria in programma: domani alle nove, pagare l'affitto; domani alle diciassette e trenta, chiamare Anna."
    );
    let (s, _) = stream(&h, chat_body("what reminders do I have?")).await;
    assert_eq!(
        s.find("reply").data["text"],
        "You have 2 reminders coming up: tomorrow at 9 AM, pagare l'affitto; tomorrow at 5:30 PM, chiamare Anna."
    );
    assert_eq!(model_calls(&h), 0);
}

#[tokio::test]
async fn a_reminder_the_parsers_resolve_is_written_through_the_normal_writer() {
    let msg = "ricordami domani alle 9 di pagare l'affitto";
    let timer = "tra 20 minuti ricordami di spegnere il forno";
    let vague = "ricordami di chiamare Anna";
    let jev = JevScript::default()
        .on(msg, say("schedule_reminder", 0.97))
        .on(timer, say("schedule_reminder", 0.95))
        .on(vague, say("schedule_reminder", 0.97));
    let server = fake_jev(&jev).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": INTENT_CHAT},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Quando?\"}"},
            {"kind": "reply", "content": "Quando vuoi che te lo ricordi?"},
        ]})),
        with_jev(&server),
    )
    .await;

    let (s, _) = stream(&h, chat_body(msg)).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(s.find("route").data["intent"], "schedule_reminder");
    assert_eq!(s.find("route").data["fast"], true);
    assert_eq!(
        s.find("reply").data["text"],
        "Fatto, ti ricordo domani alle nove: pagare l'affitto."
    );
    let done = &s.find("done").data["data"];
    assert_eq!(done["action"], "schedule_reminder");
    assert_eq!(done["reminder"]["text"], "pagare l'affitto");

    let (s, _) = stream(&h, chat_body(timer)).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    let text = s.find("reply").data["text"].as_str().unwrap().to_owned();
    assert!(
        text.starts_with("Fatto, ti ricordo tra venti minuti: ") && text.contains("forno"),
        "{text}"
    );
    assert_eq!(model_calls(&h), 0);
    let pending = h.state.reminders.list(true).unwrap();
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert!(pending
        .iter()
        .any(|r| r.text == "pagare l'affitto" && r.chat_id == "42"));
    assert!(pending.iter().all(|r| r.status == "pending"));

    // No time the parsers can read: the normal path (a planner pass may ask), and nothing is written.
    let (s, _) = stream(&h, chat_body(vague)).await;
    assert_eq!(s.find("route").data["fast"], false);
    assert!(model_calls(&h) >= 1);
    assert_eq!(h.state.reminders.list(true).unwrap().len(), 2);
}

#[tokio::test]
async fn the_route_comes_first_and_the_ack_only_before_a_slow_wait() {
    let jev = JevScript::default()
        // A slow intent that the normal path runs through the planner.
        .on("disegnami un gatto", say("generate_image", 0.93))
        // Jev does not know: the JSON plan, whose reply does not stream.
        .on("organizza il mio weekend", say("unknown", 0.3))
        .on("plan my weekend for me", say("unknown", 0.3))
        // Plain chat: the reply streams.
        .on("ciao come stai?", say("chat", 0.95))
        .on("che ore sono?", say("temporal_query", 0.95));
    let server = fake_jev(&jev).await;
    let h = harness(plan_script(), with_jev(&server)).await;

    let (s, _) = stream(&h, chat_body("disegnami un gatto")).await;
    assert_eq!(s.names(), ["route", "ack", "reply", "done"]);
    assert_eq!(s.find("route").data["fast"], false);
    assert_eq!(s.find("ack").data["text"], "Un attimo…");
    assert!(s.find("route").at <= s.find("ack").at);

    let (s, _) = stream(&h, chat_body("organizza il mio weekend")).await;
    assert_eq!(s.count("ack"), 1, "once: {:?}", s.names());
    assert_eq!(s.names()[..2], ["route", "ack"]);
    assert_eq!(s.find("ack").data["text"], "Un attimo…");
    let (s, _) = stream(&h, chat_body("plan my weekend for me")).await;
    assert_eq!(s.names(), ["route", "ack", "reply", "done"]);
    assert_eq!(s.find("ack").data["text"], "One moment…");

    let h2 = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(&h2, chat_body("ciao come stai?")).await;
    assert_eq!(s.names()[0], "route");
    assert_eq!(
        s.count("ack"),
        0,
        "never for streamed chat: {:?}",
        s.names()
    );
    assert!(s.count("delta") >= 1);
    assert!(s.find("route").at <= s.find("delta").at);

    let (s, _) = stream(&h2, chat_body("che ore sono?")).await;
    assert_eq!(s.count("ack"), 0, "never for a fast turn: {:?}", s.names());
    assert!(s.find("route").at <= s.find("reply").at);
}

#[tokio::test]
async fn non_voice_turns_never_call_jev_and_behave_as_before() {
    let jev = JevScript::default().on("che ore sono?", say("temporal_query", 0.99));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    // Plain JSON call, no voice flag.
    let (status, body) = h
        .post(
            "/chat",
            json!({"message": "che ore sono?", "sessionId": "test", "channel": "ios"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["reply"], REPLY);
    // A streamed turn without the voice flag has no route event.
    let (s, _) = stream(
        &h,
        json!({"message": "che ore sono?", "sessionId": "test", "channel": "ios"}),
    )
    .await;
    assert_eq!(s.count("route"), 0, "{:?}", s.names());
    assert_eq!(jev_calls(&server).await, 0);
}

#[tokio::test]
async fn a_spoken_turn_without_the_stream_is_fast_too() {
    let jev = JevScript::default().on("che ore sono?", say("temporal_query", 0.95));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (status, body) = h.post("/chat", chat_body("che ore sono?")).await;
    assert_eq!(status, 200, "{body}");
    let text = body["data"]["reply"].as_str().unwrap();
    assert!(
        text.starts_with("Sono le ") || text.starts_with("È "),
        "{text}"
    );
    assert_eq!(model_calls(&h), 0);
    assert_eq!(jev_calls(&server).await, 1);
}

#[tokio::test]
async fn an_mcp_voice_turn_is_not_routed() {
    let msg = "[via Lucina MCP, from claude] che ore sono?";
    let jev = JevScript::default().on("che ore sono?", say("temporal_query", 0.99));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let (s, _) = stream(
        &h,
        json!({"message": msg, "sessionId": "mcp-claude", "channel": "ios", "voice": true}),
    )
    .await;
    assert_eq!(s.count("route"), 0, "{:?}", s.names());
    assert_eq!(jev_calls(&server).await, 0);
    assert_eq!(s.find("done").data["ok"], true);
}

#[tokio::test]
async fn the_jev_call_starts_before_the_context_is_built() {
    // Jev takes 200 ms and the model 1.5 s: a fast turn costs Jev's latency plus a few milliseconds (the
    // call starts with the turn, not after the context), the same message on the normal path waits for
    // the model.
    warm();
    let msg = "che ore sono?";
    let mut v = say("temporal_query", 0.95);
    v.delay = Duration::from_millis(200);
    let jev = JevScript::default().on(msg, v);
    let server = fake_jev(&jev).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "delayMs": 600, "content": INTENT_CHAT},
            {"kind": "reply", "delayMs": 1500, "content": REPLY},
        ]})),
        with_jev(&server),
    )
    .await;
    let (s, started) = stream(&h, chat_body(msg)).await;
    let total = started.elapsed();
    let route_at = s.find("route").at.duration_since(started);
    let reply_at = s.find("reply").at.duration_since(started);
    eprintln!(
        "fast turn with a 200 ms Jev: route {route_at:?}, reply {reply_at:?}, total {total:?}"
    );
    // A debug build on a busy machine is far slower than release at the tier-0 and parser regexes (0.3 s
    // per turn on a quiet machine, about 13 ms in release): no absolute bound, the comparison below is
    // the point.
    assert!(route_at >= Duration::from_millis(190), "{route_at:?}");
    assert!(reply_at >= route_at && total >= reply_at);
    // The same message on the normal path (Jev off) waits for the model.
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "delayMs": 600, "content": INTENT_CHAT},
            {"kind": "reply", "delayMs": 1500, "content": REPLY},
        ]})),
        |_| {},
    )
    .await;
    let (s, started) = stream(&h, chat_body(msg)).await;
    let slow = s.find("reply").at.duration_since(started);
    eprintln!("same turn on the normal path: reply {slow:?}");
    assert!(slow >= Duration::from_millis(1400), "{slow:?}");
    assert!(
        slow > reply_at + Duration::from_millis(500),
        "{slow:?} vs {reply_at:?}"
    );
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let dir = tempfile::tempdir().unwrap();
    let c = Config::for_data_dir(dir.path());
    assert_eq!(c.voice_fast_min, 0.85);
    assert_eq!(c.voice_jev_cap, Duration::from_millis(400));
    assert!(c.voice_jev.is_none());
    let c = Config::from_lookup(|k| match k {
        "LUCINA_VOICE_FAST_MIN" => Some("0.9".into()),
        "LUCINA_VOICE_JEV_CAP_MS" => Some("250".into()),
        "LUCINA_DATA_DIR" => Some(dir.path().to_string_lossy().into_owned()),
        _ => None,
    })
    .unwrap();
    assert_eq!(c.voice_fast_min, 0.9);
    assert_eq!(c.voice_jev_cap, Duration::from_millis(250));
    let c = Config::from_lookup(|k| match k {
        "LUCINA_VOICE_FAST_MIN" => Some("7".into()),
        "LUCINA_DATA_DIR" => Some(dir.path().to_string_lossy().into_owned()),
        _ => None,
    })
    .unwrap();
    assert_eq!(c.voice_fast_min, 0.85, "out of range: the default");
}

/// The shopping list needs the grocery database; like the other grocery tests this runs only with
/// `LUCINA_TEST_DATABASE_URL` pointing at a test database.
#[tokio::test]
async fn the_shopping_list_is_read_and_changed_through_the_grocery_path() {
    if std::env::var("LUCINA_TEST_DATABASE_URL").is_err() {
        return eprintln!("LUCINA_TEST_DATABASE_URL unset: the shopping tests were skipped");
    }
    let jev = JevScript::default()
        .on(
            "cosa c'è nella lista della spesa?",
            say("grocery.shopping_list.query", 0.97),
        )
        .on(
            "aggiungi latte alla lista della spesa",
            say("grocery.shopping_list.add", 0.96),
        )
        .on(
            "togli il latte dalla lista della spesa",
            say("grocery.shopping_list.remove", 0.95),
        );
    let server = fake_jev(&jev).await;
    let user = format!("vf-{}", uuid::Uuid::new_v4());
    let h = harness(chat_script(), |c| {
        with_jev(&server)(c);
        c.grocery_user_id = user;
    })
    .await;

    let (s, _) = stream(&h, chat_body("cosa c'è nella lista della spesa?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(
        s.find("reply").data["text"],
        "La lista della spesa è vuota."
    );
    let (s, _) = stream(&h, chat_body("aggiungi latte alla lista della spesa")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    assert_eq!(s.find("route").data["fast"], true);
    assert_eq!(
        s.find("reply").data["text"],
        "Ho aggiunto Latte alla lista della spesa."
    );
    let (s, _) = stream(&h, chat_body("cosa c'è nella lista della spesa?")).await;
    assert_eq!(
        s.find("reply").data["text"],
        "Nella lista della spesa hai Latte."
    );
    let (s, _) = stream(&h, chat_body("togli il latte dalla lista della spesa")).await;
    assert_eq!(
        s.find("reply").data["text"],
        "Ho tolto Latte dalla lista della spesa."
    );
    assert_eq!(model_calls(&h), 0);
}
