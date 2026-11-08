//! LucinaVoice: the streaming variant of `POST /api/secretary/chat` (docs/lucinavoice-protocol.md section 1),
//! against the fake DeepSeek (which streams when asked). Events: `delta`*, `reply` once, then `done`
//! (the exact non-streaming body) or `error`.

mod common;

use std::time::{Duration, Instant};

use common::{harness, script, H, TOKEN};
use lucina_server::voice::SPOKEN_RULE;
use serde_json::{json, Value};

const INTENT_CHAT: &str =
    "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}";
const REPLY: &str = "Ciao Giuseppe, sto bene. E tu come stai oggi?";

struct Ev {
    name: String,
    data: Value,
    at: Instant,
}

struct Sse {
    events: Vec<Ev>,
    /// `: ...` comment lines.
    comments: Vec<String>,
    status: u16,
    content_type: String,
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

    fn deltas(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|e| e.name == "delta")
            .map(|e| e.data["text"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn parse(buf: &mut String, out: &mut Sse) {
    while let Some(i) = buf.find("\n\n") {
        let frame: String = buf.drain(..i + 2).collect();
        let at = Instant::now();
        let mut name = None;
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(c) = line.strip_prefix(':') {
                out.comments.push(c.trim().to_owned());
            } else if let Some(n) = line.strip_prefix("event:") {
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

fn request(h: &H, body: &Value, accept: Option<&str>) -> reqwest::RequestBuilder {
    let mut rb = h.http.post(h.url("/chat")).bearer_auth(TOKEN).json(body);
    if let Some(a) = accept {
        rb = rb.header("accept", a);
    }
    rb
}

/// A streamed chat turn, read to the end.
async fn stream(h: &H, body: Value) -> Sse {
    let mut resp = request(h, &body, Some("text/event-stream"))
        .send()
        .await
        .unwrap();
    let mut out = Sse {
        events: Vec::new(),
        comments: Vec::new(),
        status: resp.status().as_u16(),
        content_type: resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned(),
    };
    let mut buf = String::new();
    while let Some(chunk) = resp.chunk().await.unwrap() {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        parse(&mut buf, &mut out);
    }
    out
}

fn casual(extra: Vec<Value>) -> lucina_server::fake::Script {
    let mut rules = vec![
        json!({"kind": "intent", "content": INTENT_CHAT}),
        json!({"kind": "reply", "content": REPLY}),
    ];
    rules.extend(extra);
    script(json!({ "rules": rules }))
}

fn chat_body(message: &str) -> Value {
    json!({"message": message, "sessionId": "voice", "channel": "web", "voice": true})
}

fn system_of(req: &Value) -> String {
    req["messages"][0]["content"].as_str().unwrap().to_owned()
}

fn strip_ids(mut v: Value) -> Value {
    for k in ["userItemId", "lucinaItemId"] {
        v["data"]
            .as_object_mut()
            .unwrap()
            .insert(k.into(), Value::Null);
    }
    v
}

#[tokio::test]
async fn a_streamed_turn_is_delta_then_reply_then_done_equal_to_the_plain_response() {
    let h = harness(casual(vec![]), |_| {}).await;
    let s = stream(&h, chat_body("ciao come stai?")).await;
    assert_eq!(s.status, 200);
    assert!(
        s.content_type.starts_with("text/event-stream"),
        "{}",
        s.content_type
    );
    let names = s.names();
    assert!(
        names.iter().filter(|n| **n == "delta").count() >= 2,
        "{names:?}"
    );
    let reply_at = names.iter().position(|n| *n == "reply").unwrap();
    assert!(names[..reply_at].iter().all(|n| *n == "delta"), "{names:?}");
    assert_eq!(names[reply_at + 1..], ["done"], "{names:?}");
    // The deltas are the reply as the model wrote it.
    assert_eq!(s.deltas().concat(), REPLY);
    let reply = &s.find("reply").data;
    assert_eq!(reply["text"], REPLY);
    assert_eq!(reply["sensitive"], false);
    assert_eq!(reply["replaces"], false);
    // The model was asked for a stream, once, for the reply.
    let streamed: Vec<_> = h
        .fake
        .requests()
        .into_iter()
        .filter(|r| r["stream"] == true)
        .collect();
    assert_eq!(streamed.len(), 1);
    assert_eq!(lucina_server::fake::kind_of(&streamed[0]), "reply");

    // `done` is the body the plain call returns: a second server, same script, same message.
    let done = s.find("done").data.clone();
    assert_eq!(done["ok"], true);
    assert_eq!(done["data"]["reply"], REPLY);
    assert_eq!(done["data"]["action"], "reply");
    let plain = harness(casual(vec![]), |_| {}).await;
    let (status, body, _) = plain
        .call(
            reqwest::Method::POST,
            "/chat",
            Some(chat_body("ciao come stai?")),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(strip_ids(done.clone()), strip_ids(body));
    assert!(
        plain.fake.requests().iter().all(|r| r["stream"] != true),
        "the plain call never streams"
    );
    // The turn wrote its timeline like any other.
    let lines = h.timeline_lines();
    let item = lines
        .iter()
        .find(|l| l["id"] == done["data"]["lucinaItemId"])
        .expect("the Lucina item");
    assert_eq!(item["text"], REPLY);
    assert!(s.comments.iter().any(|c| c == "connected"));
}

#[tokio::test]
async fn without_the_accept_header_the_route_is_unchanged() {
    let h = harness(casual(vec![]), |_| {}).await;
    for accept in [None, Some("application/json"), Some("*/*")] {
        let resp = request(&h, &chat_body("ciao come stai?"), accept)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let ct = resp.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(ct.starts_with("application/json"), "{ct}");
        let b: Value = resp.json().await.unwrap();
        assert_eq!(b["ok"], true);
        assert_eq!(b["data"]["reply"], REPLY);
    }
    assert!(h.fake.requests().iter().all(|r| r["stream"] != true));
}

#[tokio::test]
async fn voice_true_adds_the_spoken_rule_to_the_reply_and_plan_prompts_without_the_stream() {
    // The casual shortcut (generate_reply) ...
    let h = harness(casual(vec![]), |_| {}).await;
    let (s, b) = h.post("/chat", chat_body("ciao come stai?")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["reply"], REPLY);
    let reqs = h.fake.requests();
    let reply = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .unwrap();
    assert!(
        system_of(reply).contains(SPOKEN_RULE),
        "{}",
        system_of(reply)
    );
    // ... and the classifier, if the model was asked at all, is not told (it is not speech).
    assert!(reqs
        .iter()
        .filter(|r| lucina_server::fake::kind_of(r) == "intent")
        .all(|r| !system_of(r).contains(SPOKEN_RULE)));

    // Without the flag, no rule.
    let h = harness(casual(vec![]), |_| {}).await;
    let (_, b) = h
        .post(
            "/chat",
            json!({"message": "ciao come stai?", "sessionId": "voice"}),
        )
        .await;
    assert_eq!(b["data"]["reply"], REPLY);
    assert!(h
        .fake
        .requests()
        .iter()
        .all(|r| !system_of(r).contains(SPOKEN_RULE)));

    // The planner: an unsure classification goes to the JSON plan.
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Va bene.\"}"},
        ]})),
        |_| {},
    )
    .await;
    let (_, b) = h.post("/chat", chat_body("draw a cat")).await;
    assert_eq!(b["data"]["reply"], "Va bene.");
    let reqs = h.fake.requests();
    let plan = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "plan")
        .expect("a plan call");
    assert!(system_of(plan).contains(SPOKEN_RULE));
}

#[tokio::test]
async fn the_reply_event_comes_before_the_rich_reply_work() {
    let wall = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/inventario-reply-2026-10-22.txt"
    ))
    .unwrap();
    let rich = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/inventario-rich-v1.json"
    ))
    .unwrap();
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": INTENT_CHAT},
            {"kind": "reply", "content": wall},
            {"kind": "rich", "delayMs": 1200, "content": rich},
        ]})),
        |c| c.rich_replies = true,
    )
    .await;
    let s = stream(
        &h,
        chat_body("Mi serve SOLO un inventario fattuale e breve di ciò che la tua app/backend supporta ORA. Distingui presente da ipotesi."),
    )
    .await;
    let (reply, done) = (s.find("reply"), s.find("done"));
    assert_eq!(reply.data["text"].as_str().unwrap().trim(), wall.trim());
    assert!(
        done.at.duration_since(reply.at) >= Duration::from_millis(900),
        "done came {:?} after the reply: the composer ran first",
        done.at.duration_since(reply.at)
    );
    // The composer did run (after the reply), and the plain body is the composed one.
    assert!(h
        .fake
        .requests()
        .iter()
        .any(|r| lucina_server::fake::kind_of(r) == "rich"));
    assert_eq!(s.find("done").data["data"]["format"], "v1");
}

#[tokio::test]
async fn a_retry_after_tool_markup_replaces_what_was_streamed() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": INTENT_CHAT},
            {"kind": "reply", "once": true, "content": "Controllo subito. <tool_call>{\"name\":\"drive\"}</tool_call>"},
            {"kind": "reply", "content": "Va tutto bene."},
        ]})),
        |_| {},
    )
    .await;
    let s = stream(&h, chat_body("ciao come stai?")).await;
    // What came before the markup was spoken; the markup never was.
    let streamed = s.deltas().concat();
    assert!(streamed.starts_with("Controllo"), "{streamed}");
    assert!(!streamed.contains("tool_call"), "{streamed}");
    let reply = &s.find("reply").data;
    assert_eq!(reply["text"], "Va tutto bene.");
    assert_eq!(reply["replaces"], true);
    assert_eq!(s.find("done").data["data"]["reply"], "Va tutto bene.");
    // Exactly one reply event.
    assert_eq!(s.names().iter().filter(|n| **n == "reply").count(), 1);
}

#[tokio::test]
async fn a_plan_reply_has_no_deltas() {
    // The plan path answers in the plan's text: no delta, a reply event, a done.
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Va bene.\"}"},
        ]})),
        |_| {},
    )
    .await;
    let s = stream(&h, chat_body("draw a cat")).await;
    assert_eq!(s.names(), ["reply", "done"]);
    assert_eq!(s.find("reply").data["text"], "Va bene.");
    assert_eq!(s.find("reply").data["replaces"], true);
}

#[tokio::test]
async fn a_secret_command_is_a_sensitive_reply_with_no_deltas_and_no_model() {
    let h = harness(casual(vec![]), |_| {}).await;
    let s = stream(
        &h,
        chat_body("/secret POKO_API_KEY sk-proj-Synthetic0123456789AbCdEfGhIjKl poko/prod/Pokobot"),
    )
    .await;
    assert_eq!(s.names(), ["reply", "done"]);
    let reply = &s.find("reply").data;
    assert_eq!(reply["sensitive"], true);
    assert!(
        !reply.to_string().contains("Synthetic0123456789"),
        "{reply}"
    );
    assert_eq!(s.find("done").data["data"]["action"], "secret_command");
    assert_eq!(s.find("done").data["data"]["sensitive"], true);
    assert!(h.fake.requests().is_empty(), "the model was called");
}

#[tokio::test]
async fn a_failed_turn_is_an_error_event_and_no_done() {
    let h = harness(casual(vec![]), |_| {}).await;
    let s = stream(&h, json!({"voice": true})).await;
    assert_eq!(s.status, 200);
    assert_eq!(s.names(), ["error"]);
    let e = &s.find("error").data;
    assert_eq!(e["code"], "bad_request");
    assert!(e["message"].as_str().unwrap().contains("message"));
    // Auth and body checks are plain HTTP, before the stream starts.
    let resp = h
        .http
        .post(h.url("/chat"))
        .header("accept", "text/event-stream")
        .json(&chat_body("ciao"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn a_client_that_leaves_does_not_cancel_the_turn() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": INTENT_CHAT},
            {"kind": "reply", "delayMs": 600, "content": REPLY},
        ]})),
        |_| {},
    )
    .await;
    let resp = request(&h, &chat_body("ciao come stai?"), Some("text/event-stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if h.timeline_lines()
            .iter()
            .any(|l| l["role"] == "lucina" && l["text"] == REPLY)
        {
            break;
        }
        assert!(Instant::now() < deadline, "the turn did not finish");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
