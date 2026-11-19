//! The "Lucina Voice" thread and the layered Jev decisions of a spoken turn (docs/lucinavoice-protocol.md
//! sections 7 and 9): channel `voice`, one shared conversation, its memory paths (the main chat's recent turns,
//! Honcho), and the `intent`, `slots`, `confirm` and `vocab` layers with their fail-open rules and the
//! three-calls-a-turn cap. Jev is a fake HTTP server that answers per question name; the model is the fake
//! DeepSeek, so "no model call" shows as "the fake saw no request".

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{harness, script, H, TOKEN};
use lucina_memory::MemoryMode;
use lucina_server::Config;
use serde_json::{json, Value};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const INTENT_CHAT: &str =
    "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}";
const REPLY: &str = "Ciao Giuseppe, sto bene. E tu come stai oggi?";

// ------------------------------------------------------------------------------------------ fake Jev

type IntentRule = (String, &'static str, f64);

/// What the fake answers. Everything is per message for the intent; the other questions have one answer each.
#[derive(Clone)]
struct Fake {
    intents: Arc<Mutex<Vec<IntentRule>>>,
    /// P(yes) of the `confirm` question; `None`: an HTTP 500.
    confirm: Option<f64>,
    /// P(yes) of the `slots_ok` question.
    slots_ok: f64,
    /// The `span0` choice and its confidence.
    span: (&'static str, f64),
    /// The `products` choice (`one` / `several`) and its confidence.
    products: (&'static str, f64),
    delay_for: Option<(&'static str, Duration)>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            intents: Arc::default(),
            confirm: Some(0.9),
            slots_ok: 0.9,
            span: ("c1", 0.95),
            products: ("several", 0.95),
            delay_for: None,
            requests: Arc::default(),
        }
    }
    fn intent(self, message: &str, intent: &'static str, confidence: f64) -> Self {
        self.intents
            .lock()
            .unwrap()
            .push((message.into(), intent, confidence));
        self
    }
    /// The questions each request asked, one entry per Jev call.
    fn asked(&self) -> Vec<Vec<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                r["questions"]
                    .as_object()
                    .map(|q| q.keys().cloned().collect())
                    .unwrap_or_default()
            })
            .collect()
    }
    fn calls_with(&self, question: &str) -> usize {
        self.asked()
            .iter()
            .filter(|q| q.iter().any(|n| n == question))
            .count()
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Respond for Fake {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        self.requests.lock().unwrap().push(body.clone());
        let qs = body["questions"].as_object().cloned().unwrap_or_default();
        let message = body["state"]["message"].as_str().unwrap_or("").to_owned();
        let mut answers = serde_json::Map::new();
        let mut delay = Duration::ZERO;
        if qs.contains_key("intent") {
            let (intent, c) = self
                .intents
                .lock()
                .unwrap()
                .iter()
                .find(|(m, _, _)| *m == message)
                .map(|(_, i, c)| (*i, *c))
                .unwrap_or(("unknown", 0.3));
            let other = if intent == "unknown" {
                "chat"
            } else {
                "unknown"
            };
            answers.insert(
                "intent".into(),
                json!({"type": "choice", "choice": intent, "confidence": c,
                "probabilities": {intent: c, other: 1.0 - c}}),
            );
            answers.insert(
                "scope".into(),
                json!({"type": "choice", "choice": "all", "confidence": 0.9,
                "probabilities": {"all": 0.9, "today": 0.1}}),
            );
            answers.insert("needs_memory".into(), json!({"type": "noul", "noul": 0.1}));
            answers.insert("needs_tools".into(), json!({"type": "noul", "noul": 0.1}));
        }
        if qs.contains_key("confirm") {
            match self.confirm {
                Some(p) => {
                    answers.insert("confirm".into(), json!({"type": "noul", "noul": p}));
                }
                None => return ResponseTemplate::new(500),
            }
        }
        if qs.contains_key("slots_ok") {
            answers.insert(
                "slots_ok".into(),
                json!({"type": "noul", "noul": self.slots_ok}),
            );
        }
        if qs.contains_key("products") {
            let (c, p) = self.products;
            let other = if c == "one" { "several" } else { "one" };
            answers.insert(
                "products".into(),
                json!({"type": "choice", "choice": c, "confidence": p,
                "probabilities": {c: p, other: 1.0 - p}}),
            );
        }
        if qs.contains_key("span0") {
            let (c, p) = self.span;
            let other = if c == "none" { "c1" } else { "none" };
            answers.insert(
                "span0".into(),
                json!({"type": "choice", "choice": c, "confidence": p,
                "probabilities": {c: p, other: 1.0 - p}}),
            );
            if let Some((name, d)) = self.delay_for {
                if name == "span0" {
                    delay = d;
                }
            }
        }
        ResponseTemplate::new(200)
            .set_body_json(
                json!({"model": "jev-1.14.0", "usage": {"input_tokens": 10, "output_tokens": 1},
                "answers": answers}),
            )
            .set_delay(delay)
    }
}

async fn fake_jev(f: &Fake) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(f.clone())
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
        c.voice_jev_cap = Duration::from_secs(3);
    }
}

fn chat_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": INTENT_CHAT},
        {"kind": "reply", "content": REPLY},
    ]}))
}

// ------------------------------------------------------------------------------------------- the calls

struct Sse(Vec<(String, Value)>);

impl Sse {
    fn names(&self) -> Vec<&str> {
        self.0.iter().map(|(n, _)| n.as_str()).collect()
    }
    fn find(&self, name: &str) -> &Value {
        &self
            .0
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", self.names()))
            .1
    }
}

fn voice_body(message: &str) -> Value {
    json!({"message": message, "channel": "voice", "voice": true, "device": "spk-1",
        "deviceName": "Kitchen", "voiceMeta": {"turnId": "t-1", "ms": {"stt": 80}}})
}

async fn stream(h: &H, body: Value) -> Sse {
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
    let mut out = Vec::new();
    let mut buf = String::new();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find("\n\n") {
            let frame: String = buf.drain(..i + 2).collect();
            let (mut name, mut data) = (None, String::new());
            for line in frame.lines() {
                if let Some(n) = line.strip_prefix("event:") {
                    name = Some(n.trim().to_owned());
                } else if let Some(d) = line.strip_prefix("data:") {
                    data.push_str(d.trim());
                }
            }
            if let Some(n) = name {
                out.push((n, serde_json::from_str(&data).unwrap_or(Value::Null)));
            }
        }
    }
    Sse(out)
}

fn items(h: &H) -> Vec<Value> {
    h.timeline_lines()
}

fn layer<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v.as_array()?.iter().find(|l| l["layer"] == name)
}

fn model_text(h: &H) -> String {
    h.fake
        .requests()
        .iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------------------------------- the thread (a)

#[tokio::test]
async fn a_voice_turn_lands_in_the_lucina_voice_thread() {
    let jev = Fake::new().intent("che ore sono?", "temporal_query", 0.97);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;

    let s = stream(&h, voice_body("che ore sono?")).await;
    assert_eq!(s.names(), ["route", "reply", "done"]);
    // The route event carries the layers decided before the route; `done` carries all of them.
    let route = s.find("route");
    assert_eq!(route["fast"], true);
    assert_eq!(
        layer(&route["layers"], "intent").unwrap()["verdict"],
        "temporal_query"
    );
    assert_eq!(layer(&route["layers"], "intent").unwrap()["used"], true);
    let done = &s.find("done")["data"];
    assert_eq!(done["sessionId"], "secretary_lucinavoice");
    assert_eq!(done["voice"]["jevCalls"], 1);
    assert_eq!(
        layer(&done["voice"]["layers"], "intent").unwrap()["verdict"],
        "temporal_query"
    );
    assert_eq!(done["voice"]["route"]["intent"], "temporal_query");

    // The timeline: both items on channel voice, with the speaker, the turn id, the spans and the route.
    let tl = items(&h);
    assert_eq!(tl.len(), 2, "{tl:?}");
    for it in &tl {
        assert_eq!(it["channel"], "voice");
        assert_eq!(it["meta"]["device"], "spk-1");
        assert_eq!(it["meta"]["deviceName"], "Kitchen");
        assert_eq!(it["meta"]["voice"]["turnId"], "t-1");
        assert_eq!(it["meta"]["voice"]["ms"]["stt"], 80);
        assert_eq!(it["meta"]["voice"]["route"]["intent"], "temporal_query");
        assert_eq!(it["meta"]["voice"]["route"]["fast"], true);
        assert_eq!(
            layer(&it["meta"]["voice"]["route"]["layers"], "intent").unwrap()["verdict"],
            "temporal_query"
        );
    }
    assert_eq!(tl[0]["role"], "user");
    assert_eq!(tl[0]["text"], "che ore sono?");
    assert_eq!(tl[1]["role"], "lucina");

    // It is one conversation, "Lucina Voice", whatever speaker; the app's own chat is another.
    let mut other = voice_body("che ore sono?");
    other["device"] = json!("spk-2");
    other["deviceName"] = json!("Desk");
    stream(&h, other).await;
    h.post(
        "/chat",
        json!({"message": "ciao", "sessionId": "test", "channel": "ios", "device": "Phone-1"}),
    )
    .await;
    let (status, body) = h.get("/timeline/conversations").await;
    assert_eq!(status, 200);
    let list = body["data"]["items"].as_array().unwrap();
    let voice: Vec<&Value> = list.iter().filter(|c| c["key"] == "voice").collect();
    assert_eq!(voice.len(), 1, "{list:?}");
    assert_eq!(voice[0]["title"], "Lucina Voice");
    assert_eq!(voice[0]["count"], 4);
    assert!(list.iter().any(|c| c["key"] == "device:Phone-1"));
    // And its items come back by key.
    let (_, page) = h.get("/timeline?conversation=voice&limit=50").await;
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 4);
}

/// What a directory holds at this moment, names and sizes only: the failure message of the wait below.
fn dir_contents(dir: &std::path::Path) -> String {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => return format!("unreadable ({e})"),
    };
    let listed: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let size = e.metadata().map(|m| m.len()).unwrap_or_default();
            format!("{} ({} bytes)", e.file_name().to_string_lossy(), size)
        })
        .collect();
    if listed.is_empty() {
        "nothing".to_owned()
    } else {
        listed.join(", ")
    }
}

#[tokio::test]
async fn an_owner_turn_keeps_jevs_answer_in_the_servers_data_dir() {
    let jev = Fake::new().intent("che ore sono?", "temporal_query", 0.97);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    stream(&h, voice_body("che ore sono?")).await;
    let dir = h.dir.path().join("jev-cal");
    // The sample is written by a blocking task spawned off the turn (`jev_samples::record`), so on a
    // loaded host it can be seconds late: the wait is bounded by a deadline, not by a turn count.
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_secs(30);
    let mut body = String::new();
    loop {
        if let Some(Ok(entry)) = std::fs::read_dir(&dir).ok().and_then(|mut d| d.next()) {
            body = std::fs::read_to_string(entry.path()).unwrap();
        }
        if !body.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "waited {:?} for the sample file the server writes in the background into {} \
                 (`jev_samples::record`), and the body stayed empty; at the deadline the directory \
                 held: {}",
                started.elapsed(),
                dir.display(),
                dir_contents(&dir),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let row: Value = serde_json::from_str(body.lines().next().expect("a sample")).unwrap();
    assert_eq!(row["message"], "che ore sono?");
    assert_eq!(row["intent"], "temporal_query");
}

#[tokio::test]
async fn the_speakers_turns_are_in_the_main_chats_recent_context_and_back() {
    let jev = Fake::new().intent("che ore sono?", "temporal_query", 0.97);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    stream(&h, voice_body("che ore sono?")).await;

    // The app asks about it: the speaker's lines are in the recent turns the model reads.
    let out = h.chat("cosa ti ho chiesto sul diffusore?").await;
    assert_eq!(out["reply"], REPLY);
    let seen = model_text(&h);
    assert!(seen.contains("LucinaVoice speaker"), "{seen}");
    assert!(seen.contains("Giuseppe: che ore sono?"), "{seen}");
    assert!(seen.contains("Lucina: It's "), "the reply too: {seen}");

    // And the speaker sees what was said in the app.
    let before = h.fake.requests().len();
    stream(&h, voice_body("cosa ti ho scritto nell'app?")).await;
    let after: String = h.fake.requests()[before..]
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(after.contains("Secretary app chat"), "{after}");
    assert!(
        after.contains("Giuseppe: cosa ti ho chiesto sul diffusore?"),
        "{after}"
    );
    // Its own thread is not repeated back to it as "the app".
    assert!(!after.contains("Giuseppe: che ore sono?\n"), "{after}");
}

#[tokio::test]
async fn a_voice_turn_is_an_owner_turn_and_the_channel_is_validated() {
    let h = harness(chat_script(), |_| {}).await;
    let (status, body) = h.post("/chat", voice_body("ciao")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["reply"], REPLY);
    // The platform rule of the thread reaches the model: an owner device, not an outside platform.
    assert!(model_text(&h).contains("Lucina Voice"));
    // Other channels are still refused, and so is a request without the bearer.
    let (status, body) = h
        .post("/chat", json!({"message": "ciao", "channel": "alexa"}))
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("voice"));
    let resp = h
        .http
        .post(h.url("/chat"))
        .json(&voice_body("ciao"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    // A voice item keeps the timeline's channel names honest: only the speakers' channel is `voice`.
    let tl = items(&h);
    assert!(tl.iter().all(|i| i["channel"] == "voice"));
}

async fn honcho() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn a_voice_turn_goes_to_honcho_like_a_web_turn() {
    let hon = honcho().await;
    let jev = Fake::new().intent("ciao come stai?", "chat", 0.95);
    let server = fake_jev(&jev).await;
    let uri = hon.uri();
    let h = harness(chat_script(), |c| {
        with_jev(&server)(c);
        c.memory_mode = MemoryMode::ReadWrite;
        c.honcho.base_url = uri;
    })
    .await;
    stream(&h, voice_body("ciao come stai?")).await;
    tokio::time::sleep(Duration::from_millis(500)).await; // Honcho writes are spawned
    let reqs = hon.received_requests().await.unwrap();
    let write = reqs
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .expect("the turn was written to Honcho");
    // The owner's personal memory: the speaker is written to like the app (a denied surface writes nothing).
    assert!(
        write.url.path().contains("secretary_lucinavoice"),
        "{}",
        write.url
    );
    let msgs: Value = serde_json::from_slice(&write.body).unwrap();
    let user = &msgs["messages"][0];
    assert_eq!(
        user["metadata"],
        json!({"role": "user"}),
        "the owner's own words, not an agent's"
    );
    assert!(user["content"]
        .as_str()
        .unwrap()
        .starts_with("[LucinaVoice speaker] ciao come stai?"));
}

// ------------------------------------------------------------------------------------- the layers (c)

#[tokio::test]
async fn a_slow_voice_turn_makes_one_intent_call() {
    let jev = Fake::new().intent("ciao come stai?", "chat", 0.95);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, voice_body("ciao come stai?")).await;
    assert_eq!(s.find("reply")["text"], REPLY);
    // The classifier of the turn took the route's verdict instead of asking again.
    assert_eq!(jev.calls(), 1, "{:?}", jev.asked());
    assert_eq!(jev.calls_with("intent"), 1);
    assert_eq!(s.find("done")["data"]["voice"]["jevCalls"], 1);
}

const CLAUDE_MSG: &str = "dì a Claude di controllare il deploy";

#[tokio::test]
async fn a_risky_action_is_held_for_a_spoken_yes() {
    let jev = Fake::new()
        .intent(CLAUDE_MSG, "agent_message", 0.95)
        .intent("sì", "chat", 0.9);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;

    let s = stream(&h, voice_body(CLAUDE_MSG)).await;
    assert_eq!(s.names(), ["route", "reply", "done"], "no ack, no planner");
    let route = s.find("route");
    assert_eq!(route["intent"], "agent_message");
    assert_eq!(route["fast"], true);
    let confirm = layer(&route["layers"], "confirm").unwrap();
    assert_eq!(confirm["verdict"], "ask");
    assert_eq!(confirm["used"], true);
    let text = s.find("reply")["text"].as_str().unwrap();
    assert!(text.starts_with("Are you sure?"), "{text}");
    assert_eq!(s.find("done")["data"]["action"], "voice_confirm");
    assert_eq!(
        s.find("done")["data"]["voice"]["jevCalls"],
        2,
        "intent and confirm"
    );
    assert!(h.fake.requests().is_empty(), "nothing ran, no model call");
    // The state Jev saw for the question: facts only.
    let state = jev.requests.lock().unwrap()[1]["state"].clone();
    assert_eq!(state["channel"], "voice");
    assert_eq!(state["risk"], "message");
    assert_eq!(state["message"], CLAUDE_MSG);
    assert_eq!(
        state
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["channel", "intent", "message", "risk"]
    );

    // A no cancels it; a later "sì" has nothing to run.
    let s = stream(&h, voice_body("no")).await;
    assert_eq!(
        s.find("reply")["text"],
        "OK, I won't do it.",
        "in the language of the conversation"
    );
    assert_eq!(
        layer(&s.find("done")["data"]["voice"]["layers"], "confirm").unwrap()["verdict"],
        "no"
    );
    assert!(h.fake.requests().is_empty());

    // Ask again; this time a yes runs the held request (the owner's item says "sì", the turn ran the held text).
    stream(&h, voice_body(CLAUDE_MSG)).await;
    let s = stream(&h, voice_body("sì")).await;
    let done = &s.find("done")["data"];
    let confirm = layer(&done["voice"]["layers"], "confirm").unwrap();
    assert_eq!(confirm["verdict"], "yes");
    assert_eq!(confirm["used"], true);
    assert_ne!(done["action"], "voice_confirm");
    assert!(
        !s.find("reply")["text"]
            .as_str()
            .unwrap()
            .starts_with("Sei sicuro"),
        "it does not ask twice"
    );
    assert_eq!(
        done["voice"]["route"]["intent"], "agent_message",
        "the held request was routed"
    );
    let tl = items(&h);
    assert!(
        tl.iter().any(|i| i["role"] == "user" && i["text"] == "sì"),
        "{tl:?}"
    );
}

#[tokio::test]
async fn confirm_fails_open_and_a_sure_act_goes_ahead() {
    // Jev says no need to ask.
    let mut jev = Fake::new().intent(CLAUDE_MSG, "agent_message", 0.95);
    jev.confirm = Some(0.1);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, voice_body(CLAUDE_MSG)).await;
    let done = &s.find("done")["data"];
    assert_ne!(done["action"], "voice_confirm");
    let confirm = layer(&done["voice"]["layers"], "confirm").unwrap();
    assert_eq!(confirm["verdict"], "act");
    assert_eq!(confirm["used"], false);

    // Jev erroring on the question: today's behaviour, the action goes ahead, the layer says why.
    let mut jev = Fake::new().intent(CLAUDE_MSG, "agent_message", 0.95);
    jev.confirm = None;
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, voice_body(CLAUDE_MSG)).await;
    let done = &s.find("done")["data"];
    assert_ne!(done["action"], "voice_confirm");
    let confirm = layer(&done["voice"]["layers"], "confirm").unwrap();
    assert_eq!(confirm["verdict"], "error");
    assert_eq!(confirm["used"], false);

    // The layers switched off: no question at all.
    let jev = Fake::new().intent(CLAUDE_MSG, "agent_message", 0.95);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), |c| {
        with_jev(&server)(c);
        c.voice_layers = false;
    })
    .await;
    stream(&h, voice_body(CLAUDE_MSG)).await;
    assert_eq!(jev.calls_with("confirm"), 0);
    // And a harmless intent is never asked about.
    let jev = Fake::new().intent("che ore sono?", "temporal_query", 0.97);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    stream(&h, voice_body("che ore sono?")).await;
    assert_eq!(jev.calls_with("confirm"), 0);
}

const VAGUE_REMINDER: &str = "ricordami domani di chiamare Marco";

#[tokio::test]
async fn slots_look_again_only_when_the_reminder_parse_is_unsure() {
    // A reminder with a clock time is not doubted: one Jev call, written at once.
    let sure = "ricordami domani alle 9 di pagare l'affitto";
    let jev = Fake::new().intent(sure, "schedule_reminder", 0.97).intent(
        VAGUE_REMINDER,
        "schedule_reminder",
        0.97,
    );
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, voice_body(sure)).await;
    assert_eq!(s.find("route")["fast"], true);
    assert_eq!(jev.calls_with("slots_ok"), 0);

    // No time said: the parser used a default hour, so one more question; a yes keeps it fast.
    let s = stream(&h, voice_body(VAGUE_REMINDER)).await;
    assert_eq!(jev.calls_with("slots_ok"), 1);
    assert_eq!(s.find("route")["fast"], true);
    let slots = layer(&s.find("route")["layers"], "slots").unwrap();
    assert_eq!(slots["verdict"], "ok");
    assert_eq!(slots["used"], false);
    assert_eq!(h.state.reminders.list(true).unwrap().len(), 2);
    let state = jev.requests.lock().unwrap().last().unwrap()["state"].clone();
    assert_eq!(state["parsed"]["kind"], "reminder");
    assert_eq!(state["parsed"]["text"], "chiamare Marco");
    assert_eq!(state["message"], VAGUE_REMINDER);

    // A no hands it to the planner: not fast, nothing written.
    let mut jev = Fake::new().intent(VAGUE_REMINDER, "schedule_reminder", 0.97);
    jev.slots_ok = 0.05;
    let server = fake_jev(&jev).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": INTENT_CHAT},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"A che ora?\"}"},
            {"kind": "reply", "content": "A che ora?"},
        ]})),
        with_jev(&server),
    )
    .await;
    let s = stream(&h, voice_body(VAGUE_REMINDER)).await;
    assert_eq!(s.find("route")["fast"], false);
    let slots = layer(&s.find("route")["layers"], "slots").unwrap();
    assert_eq!(slots["verdict"], "no");
    assert_eq!(slots["used"], true);
    // The normal path decided (not the quick command): no `fast`, so the speaker also gets the usual ack/wait.
    assert_ne!(s.find("done")["data"]["voice"]["route"]["fast"], true);
}

#[tokio::test]
async fn slots_fail_open_to_the_quick_command() {
    // The question errors: the parser's reminder is written as before.
    struct Failing(Fake);
    impl Respond for Failing {
        fn respond(&self, req: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            if body["questions"].get("slots_ok").is_some() {
                self.0.requests.lock().unwrap().push(body);
                return ResponseTemplate::new(500);
            }
            self.0.respond(req)
        }
    }
    let jev = Fake::new().intent(VAGUE_REMINDER, "schedule_reminder", 0.97);
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(Failing(jev.clone()))
        .mount(&server)
        .await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, voice_body(VAGUE_REMINDER)).await;
    assert_eq!(s.find("route")["fast"], true);
    let slots = layer(&s.find("route")["layers"], "slots").unwrap();
    assert_eq!(slots["verdict"], "error");
    assert_eq!(slots["used"], false);
    assert_eq!(h.state.reminders.list(true).unwrap().len(), 1);
}

// ----------------------------------------------------------------------------------------- vocab layer

const SPAN_MSG: &str = "porta il pacco a schiavi garde";

fn ambiguous_body() -> Value {
    let mut b = voice_body(SPAN_MSG);
    b["voiceMeta"]["ambiguous"] =
        json!([{"heard": "schiavi garde", "candidates": ["Skibigårdsvej", "Skibby Gård"]}]);
    b
}

#[tokio::test]
async fn the_vocab_layer_applies_a_sure_pick_and_fails_open() {
    // A sure pick of the first candidate.
    let corrected = "porta il pacco a Skibigårdsvej";
    let jev = Fake::new().intent(corrected, "chat", 0.95);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, ambiguous_body()).await;
    let done = &s.find("done")["data"];
    let vocab = layer(&done["voice"]["layers"], "vocab").unwrap();
    assert_eq!(vocab["verdict"], "c1");
    assert_eq!(vocab["used"], true);
    let tl = items(&h);
    assert_eq!(
        tl[0]["text"], corrected,
        "the owner's item holds the corrected sentence"
    );
    assert_eq!(jev.calls_with("span0"), 1);
    // The state Jev saw: the sentence and the candidates, nothing else.
    let state = jev.requests.lock().unwrap()[0]["state"].clone();
    assert_eq!(
        state
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["channel", "sentence", "spans"]
    );
    // Vocab and intent: two calls.
    assert_eq!(done["voice"]["jevCalls"], 2);

    // Not sure: the sentence stays as the STT wrote it.
    let mut jev = Fake::new().intent(SPAN_MSG, "chat", 0.95);
    jev.span = ("c1", 0.6);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let s = stream(&h, ambiguous_body()).await;
    assert_eq!(items(&h)[0]["text"], SPAN_MSG);
    let vocab = layer(&s.find("done")["data"]["voice"]["layers"], "vocab").unwrap();
    assert_eq!(vocab["used"], false);

    // "none": same.
    let mut jev = Fake::new().intent(SPAN_MSG, "chat", 0.95);
    jev.span = ("none", 0.97);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    stream(&h, ambiguous_body()).await;
    assert_eq!(items(&h)[0]["text"], SPAN_MSG);

    // Too slow (300 ms budget): fail open, and the turn still answers.
    let mut jev = Fake::new().intent(SPAN_MSG, "chat", 0.95);
    jev.delay_for = Some(("span0", Duration::from_millis(2500)));
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let started = std::time::Instant::now();
    let s = stream(&h, ambiguous_body()).await;
    assert!(started.elapsed() < Duration::from_millis(2400));
    assert_eq!(items(&h)[0]["text"], SPAN_MSG);
    let vocab = layer(&s.find("done")["data"]["voice"]["layers"], "vocab").unwrap();
    assert_eq!(vocab["verdict"], "timeout");
    assert_eq!(s.find("reply")["text"], REPLY);

    // No ambiguity in the request: no vocab call.
    let jev = Fake::new().intent(SPAN_MSG, "chat", 0.95);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    stream(&h, voice_body(SPAN_MSG)).await;
    assert_eq!(jev.calls_with("span0"), 0);
}

#[tokio::test]
async fn the_three_calls_of_a_turn_are_never_exceeded() {
    // Vocab, intent and confirm: the most a turn has, exactly three.
    let corrected = "dì a Claude di controllare Skibigårdsvej";
    let jev = Fake::new().intent(corrected, "agent_message", 0.95);
    let server = fake_jev(&jev).await;
    let h = harness(chat_script(), with_jev(&server)).await;
    let mut b = voice_body("dì a Claude di controllare schiavi garde");
    b["voiceMeta"]["ambiguous"] =
        json!([{"heard": "schiavi garde", "candidates": ["Skibigårdsvej"]}]);
    let s = stream(&h, b).await;
    let done = &s.find("done")["data"];
    assert_eq!(jev.calls(), 3, "{:?}", jev.asked());
    assert_eq!(done["voice"]["jevCalls"], 3);
    assert_eq!(done["action"], "voice_confirm");
    let names: Vec<&str> = done["voice"]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["layer"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["vocab", "intent", "confirm"]);
}

#[test]
fn the_trace_hands_out_three_calls() {
    let t = lucina_server::voice::Trace::default();
    assert!(t.take_call() && t.take_call() && t.take_call());
    assert!(!t.take_call(), "a fourth call is refused");
    assert_eq!(t.calls(), 3);
}

#[tokio::test]
async fn a_dead_jev_leaves_the_turn_as_today_with_no_layers_but_the_intent_failure() {
    let h = harness(chat_script(), |_| {}).await;
    let s = stream(&h, voice_body("ciao come stai?")).await;
    // No Jev client configured: the plain stream, and no `voice` key in the body.
    assert_eq!(s.find("reply")["text"], REPLY);
    assert!(s.find("done")["data"].get("voice").is_none());
    assert!(!s.names().contains(&"route"));
    let tl = items(&h);
    // The item still carries what the request said.
    assert_eq!(tl[1]["meta"]["voice"]["turnId"], "t-1");
    assert!(tl[1]["meta"]["voice"].get("route").is_none());
}

/// Needs the grocery database like the other grocery tests (`LUCINA_TEST_DATABASE_URL`).
#[tokio::test]
async fn a_shopping_sentence_with_several_products_is_split_by_the_slots_layer() {
    if std::env::var("LUCINA_TEST_DATABASE_URL").is_err() {
        return eprintln!("LUCINA_TEST_DATABASE_URL unset: the shopping tests were skipped");
    }
    let both = "aggiungi latte e uova alla lista della spesa";
    let ask = "cosa c'è nella lista della spesa?";
    let run = |products: (&'static str, f64)| async move {
        let mut jev = Fake::new()
            .intent(both, "grocery.shopping_list.add", 0.96)
            .intent(ask, "grocery.shopping_list.query", 0.97);
        jev.products = products;
        let server = fake_jev(&jev).await;
        let user = format!("vt-{}", uuid::Uuid::new_v4());
        let h = harness(chat_script(), |c| {
            with_jev(&server)(c);
            c.grocery_user_id = user;
        })
        .await;
        let s = stream(&h, voice_body(both)).await;
        let route = s.find("route").clone();
        let reply = s.find("reply")["text"].as_str().unwrap().to_owned();
        let list = stream(&h, voice_body(ask)).await;
        let listed = list.find("reply")["text"].as_str().unwrap().to_owned();
        (route, reply, listed, jev.calls_with("products"))
    };
    // Several: two items on the list, one sentence to say so, no model.
    let (route, reply, listed, calls) = run(("several", 0.95)).await;
    assert_eq!(calls, 1);
    assert_eq!(route["fast"], true);
    let slots = layer(&route["layers"], "slots").unwrap();
    assert_eq!(
        (slots["verdict"].as_str(), slots["used"].clone()),
        (Some("several"), json!(true))
    );
    // English or Danish, never Italian (owner rule 2026-11-02).
    assert_eq!(reply, "Added latte and uova to your shopping list.");
    assert!(
        listed.contains("Latte") && listed.contains("Uova"),
        "{listed}"
    );
    // Jev reads one product: the slots layer stays out, and the grocery path still splits a text that
    // is not exactly a catalog term (docs/design/shopping-labels.md: never one item made of a sentence).
    let (route, _, listed, _) = run(("one", 0.95)).await;
    assert_eq!(route["fast"], true);
    assert_eq!(layer(&route["layers"], "slots").unwrap()["used"], false);
    assert!(!listed.to_lowercase().contains("latte e uova"), "{listed}");
    assert!(
        listed.contains("Latte") && listed.contains("Uova"),
        "{listed}"
    );
}
