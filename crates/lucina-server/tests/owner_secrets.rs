//! "Add secret" in Lucina: `/secret` and token-shaped text never reach the
//! model, the timeline, Honcho, notifications, files or logs; held values and
//! grants are relayed to the broker (a fake one on a Unix socket here) and
//! the receipt names the secret, never its value. Values are synthetic.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use common::{harness, script};
use lucina_memory::MemoryMode;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const VALUE: &str = "sk-proj-Synthetic0123456789AbCdEfGhIjKl";
const PASTED: &str = "ghp_SyntheticPastedToken0123456789ab";
/// Saved through the relay, so the process-wide redactor learns it: its own value, or the test of
/// a pasted token depends on which test runs first.
const RELAYED: &str = "ghp_SyntheticRelayedToken123456789ab";

/// Raw log capture (no redacting writer): the value must never be logged at all.
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        #[derive(Clone)]
        struct W(Arc<Mutex<Vec<u8>>>);
        impl Write for W {
            fn write(&mut self, d: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(d);
                Ok(d.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let w = W(buf.clone());
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || w.clone())
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
}

fn assert_clean(what: &str, text: &str) {
    for v in [VALUE, PASTED, RELAYED] {
        assert!(!text.contains(v), "{what} holds a value: {text}");
    }
}

fn scan_dir(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            scan_dir(&p);
        } else if let Ok(text) = std::fs::read_to_string(&p) {
            assert_clean(&p.display().to_string(), &text);
        }
    }
}

/// A broker on a Unix socket: records each request line, answers `answer`.
struct FakeBroker {
    socket: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    answer: Arc<Mutex<Value>>,
}

async fn fake_broker(dir: &Path) -> FakeBroker {
    let socket = dir.join("broker.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(json!({"ok": true, "result": {}})));
    let (s2, a2) = (seen.clone(), answer.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let (rd, mut wr) = stream.into_split();
            let mut line = String::new();
            BufReader::new(rd).read_line(&mut line).await.unwrap();
            s2.lock()
                .unwrap()
                .push(serde_json::from_str(&line).unwrap());
            let mut out = serde_json::to_vec(&*a2.lock().unwrap()).unwrap();
            out.push(b'\n');
            wr.write_all(&out).await.unwrap();
        }
    });
    FakeBroker {
        socket,
        seen,
        answer,
    }
}

struct Env {
    h: common::H,
    broker: FakeBroker,
    honcho: MockServer,
    secretary: MockServer,
    _side: tempfile::TempDir,
}

async fn setup() -> Env {
    logs();
    let side = tempfile::tempdir().unwrap();
    let broker = fake_broker(side.path()).await;
    let token_file = side.path().join("grants.token");
    std::fs::write(&token_file, "relay-token-0123456789abcdef\n").unwrap();
    let honcho = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&honcho)
        .await;
    let secretary = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&secretary)
        .await;
    let (socket, hu, su) = (broker.socket.clone(), honcho.uri(), secretary.uri());
    let h = harness(
        script(json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok, fatto.\"}"}]})),
        move |c| {
            c.broker.socket = socket;
            c.broker.token_file = Some(token_file);
            c.memory_mode = MemoryMode::ReadWrite;
            c.honcho.base_url = hu;
            c.honcho.api_key = "fake-honcho-key-123456".into();
            c.secretary_url = Some(su);
        },
    )
    .await;
    Env {
        h,
        broker,
        honcho,
        secretary,
        _side: side,
    }
}

impl Env {
    async fn chat(&self, message: &str) -> Value {
        let (s, b) = self
            .h
            .post("/chat", json!({"message": message, "sessionId": "test", "channel": "web", "clientMessageId": format!("c-{}", message.len())}))
            .await;
        assert_eq!(s, 200, "{b}");
        b["data"].clone()
    }

    /// Every sink: model requests, timeline and data files, Honcho, notify, logs.
    async fn assert_no_value_anywhere(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_clean(
            "model requests",
            &serde_json::to_string(&self.h.fake.requests()).unwrap(),
        );
        assert_clean(
            "timeline",
            &serde_json::to_string(&self.h.timeline_lines()).unwrap(),
        );
        scan_dir(self.h.dir.path());
        for r in self.honcho.received_requests().await.unwrap() {
            assert_clean("honcho", &String::from_utf8_lossy(&r.body));
        }
        for r in self.secretary.received_requests().await.unwrap() {
            assert_clean("secretary/notify", &String::from_utf8_lossy(&r.body));
        }
        assert_clean("logs", &String::from_utf8_lossy(&logs().lock().unwrap()));
    }

    fn offer_actions(&self) -> Vec<Value> {
        self.h
            .timeline_lines()
            .iter()
            .filter(|l| l["meta"]["kind"] == "secret_offer")
            .flat_map(|l| l["meta"]["actions"].as_array().cloned().unwrap_or_default())
            .filter(|a| a["kind"] == "secret")
            .collect()
    }
}

#[tokio::test]
async fn a_secret_command_never_reaches_a_sink() {
    let env = setup().await;
    let d = env
        .chat(&format!("/secret POKO_API_KEY {VALUE} poko/prod/Pokobot"))
        .await;
    assert_eq!(d["action"], "secret_command");
    assert_clean("reply", &d.to_string());
    assert!(env.h.fake.requests().is_empty(), "the model was called");
    assert!(
        env.h.state.client_messages.lock().unwrap().is_empty(),
        "the turn is in the replay cache"
    );
    // No user item; one Lucina card that opens the sheet with the held value.
    let lines = env.h.timeline_lines();
    assert!(lines.iter().all(|l| l["role"] == "lucina"), "{lines:?}");
    let a = &env.offer_actions()[0];
    assert_eq!(a["payload"]["mode"], "save");
    assert_eq!(a["payload"]["name"], "POKO_API_KEY");
    assert_eq!(a["payload"]["target"], "poko/prod/Pokobot");
    let held = a["payload"]["held"].as_str().unwrap();
    assert_eq!(
        env.h.state.held_secrets.get(held).unwrap().expose_secret(),
        VALUE
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_pasted_token_is_masked_before_the_turn_and_offered() {
    let env = setup().await;
    let d = env
        .chat(&format!("usa questo token per GitHub: {PASTED} grazie"))
        .await;
    assert_clean("reply", &d.to_string());
    // The model ran on the masked text.
    let reqs = serde_json::to_string(&env.h.fake.requests()).unwrap();
    assert!(reqs.contains("[secret:token]"), "{reqs}");
    // The owner's item is masked; the offer comes after Lucina's reply.
    let lines = env.h.timeline_lines();
    let user = lines.iter().find(|l| l["role"] == "user").unwrap();
    assert!(
        user["text"].as_str().unwrap().contains("[secret:token]"),
        "{user}"
    );
    assert_eq!(lines.last().unwrap()["meta"]["kind"], "secret_offer");
    let held = env.offer_actions()[0]["payload"]["held"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        env.h.state.held_secrets.get(&held).unwrap().expose_secret(),
        PASTED
    );
    env.assert_no_value_anywhere().await;
    // The checks saw real traffic: Honcho got the (masked) turn, logs were captured.
    let honcho = env.honcho.received_requests().await.unwrap();
    assert!(
        honcho
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("[secret:token]")),
        "Honcho got the masked turn"
    );
    assert!(String::from_utf8_lossy(&logs().lock().unwrap()).contains("secretary chat received"));
    // Ordinary chat makes no offer.
    let before = env.offer_actions().len();
    env.chat("ricordami il latte domani alle 9").await;
    assert_eq!(env.offer_actions().len(), before);
}

#[tokio::test]
async fn the_relay_passes_grants_and_writes_a_receipt() {
    let env = setup().await;
    *env.broker.answer.lock().unwrap() = json!({"ok": true, "result": {
        "key": "POKO_API_KEY", "project": {"id": "proj-poko", "name": "Poko"}, "environment": "prod",
        "path": "/Pokobot", "action": "created", "version": 1, "fingerprint": "ab12cd34"}});
    let (s, b) = env
        .h
        .post(
            "/secrets/grant",
            json!({"grant": "g1.payload.sig", "value": VALUE}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["data"]["receipt"],
        "Saved `POKO_API_KEY` in Poko / prod / /Pokobot (fingerprint ab12cd34)"
    );
    let sent = env.broker.seen.lock().unwrap().clone();
    assert_eq!(sent[0]["op"], "grant");
    assert_eq!(sent[0]["grant"], "g1.payload.sig");
    assert_eq!(sent[0]["value"], VALUE);
    assert_eq!(sent[0]["token"], "relay-token-0123456789abcdef");
    let receipt = env
        .h
        .timeline_lines()
        .into_iter()
        .find(|l| l["meta"]["kind"] == "secret_receipt")
        .unwrap();
    assert!(receipt["text"]
        .as_str()
        .unwrap()
        .contains("fingerprint ab12cd34"));
    // Lucina learned the value: it is masked by name from now on.
    assert_eq!(
        lucina_secrets::redactor().redact(&format!("x {VALUE} y")),
        "x [secret:POKO_API_KEY] y"
    );

    // A held value (from /secret) is relayed by id, then forgotten.
    env.chat(&format!("/secret OTHER_KEY {RELAYED}")).await;
    let held = env.offer_actions()[0]["payload"]["held"]
        .as_str()
        .unwrap()
        .to_owned();
    let (s, _) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "held": held}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(env.broker.seen.lock().unwrap()[1]["value"], RELAYED);
    assert!(env.h.state.held_secrets.get(&held).is_none());
    let (s, b) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "held": held}))
        .await;
    assert_eq!(s, 410, "{b}");

    // Broker refusals map to statuses; nothing echoes the value.
    *env.broker.answer.lock().unwrap() = json!({"ok": false, "error": {"code": "grant_reused", "message": "grant was already used"}});
    let (s, b) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "value": VALUE}))
        .await;
    assert_eq!(s, 403, "{b}");
    assert_clean("error", &b.to_string());
    *env.broker.answer.lock().unwrap() = json!({"ok": false, "error": {"code": "exists", "message": "POKO_API_KEY already exists there"}});
    let (s, _) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "value": VALUE}))
        .await;
    assert_eq!(s, 409);
    let (s, _) = env.h.post("/secrets/grant", json!({"value": VALUE})).await;
    assert_eq!(s, 400);
    let (s, _) = env
        .h
        .post(
            "/secrets/grant",
            json!({"grant": "g1.p.s", "value": VALUE, "held": "held-x12345678"}),
        )
        .await;
    assert_eq!(s, 400);
    // The grant request carried the value once per call, no copies in sinks.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_clean(
        "timeline",
        &serde_json::to_string(&env.h.timeline_lines()).unwrap(),
    );
    scan_dir(env.h.dir.path());
    assert_clean("logs", &String::from_utf8_lossy(&logs().lock().unwrap()));
}

#[tokio::test]
async fn a_project_request_gets_a_card_not_a_project() {
    let env = setup().await;
    let d = env.chat("crea un progetto Infisical per Poko Shorts").await;
    assert_eq!(d["action"], "secret_project_offer");
    assert!(
        env.h.fake.requests().is_empty(),
        "decided without the model"
    );
    assert!(
        env.broker.seen.lock().unwrap().is_empty(),
        "nothing was created"
    );
    let a = &env.offer_actions()[0];
    assert_eq!(a["payload"]["mode"], "project");
    assert_eq!(a["payload"]["projectName"], "Poko Shorts");

    *env.broker.answer.lock().unwrap() = json!({"ok": true, "result": {
        "project": {"id": "proj-new", "name": "Poko Shorts", "environments": [{"slug": "dev"}, {"slug": "prod"}]},
        "access": "owner identity only"}});
    let (s, b) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s"}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert!(b["data"]["receipt"].as_str().unwrap().contains("no access"));
}

#[tokio::test]
async fn without_a_broker_token_add_secret_is_unavailable() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "value": VALUE}))
        .await;
    assert_eq!(s, 503, "{b}");
    assert_eq!(b["error"], "secrets_unconfigured");
}

fn wifi(name: &str) -> Value {
    json!({"name": name, "project": "lucinaos", "environment": "prod", "reason": "The image joins the Wi-Fi at boot",
           "asking_client": "claude-code", "from": "gbgo-wifi"})
}

fn saved_answer(name: &str) -> Value {
    json!({"ok": true, "result": {"key": name, "project": {"id": "p-1", "name": "LucinaOS"}, "environment": "prod",
                                  "path": "/", "action": "created", "version": 1, "fingerprint": "ab12cd34"}})
}

impl Env {
    fn request_cards(&self) -> Vec<Value> {
        self.h.timeline_lines().into_iter().filter(|l| l["meta"]["kind"] == "secret_request").collect()
    }

    async fn queue(&self) -> Value {
        let (s, b) = self.h.get("/secrets/requests").await;
        assert_eq!(s, 200, "{b}");
        b["data"].clone()
    }
}

#[tokio::test]
async fn requests_come_one_card_at_a_time_and_the_value_goes_nowhere() {
    let env = setup().await;
    let (s, a) = env.h.post("/secrets/request", wifi("FIRST_KEY")).await;
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["data"]["status"], "asked");
    let (_, b) = env.h.post("/secrets/request", wifi("SECOND_KEY")).await;
    assert_eq!(b["data"]["status"], "queued");
    assert_eq!(b["data"]["ahead"], 1);
    assert_eq!(env.request_cards().len(), 1, "one card at a time");

    // The card keeps the old `secret` action for old clients, and names its request for new ones.
    let card = &env.request_cards()[0];
    let action = &card["meta"]["actions"][0];
    assert_eq!(action["kind"], "secret");
    assert_eq!(action["payload"]["name"], "FIRST_KEY");
    assert_eq!(action["payload"]["request"], a["data"]["requestId"]);
    assert_eq!(action["payload"]["target"], "lucinaos/prod");

    // OK on the card: the relay passes the value to the broker, the next card follows.
    *env.broker.answer.lock().unwrap() = saved_answer("FIRST_KEY");
    let (s, saved) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "value": VALUE, "request": a["data"]["requestId"]}))
        .await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["data"]["fingerprint"], "ab12cd34");
    assert!(saved["data"]["nextItemId"].is_string());
    assert_eq!(env.broker.seen.lock().unwrap().last().unwrap()["value"], VALUE);
    assert_eq!(env.request_cards().len(), 2);

    let q = env.queue().await;
    assert_eq!(q["active"], b["data"]["requestId"]);
    assert_eq!(q["requests"][0]["status"], "saved");
    assert_eq!(q["requests"][0]["fingerprint"], "ab12cd34");
    assert_clean("queue", &q.to_string());
    assert_clean("the save's answer", &saved.to_string());

    // "Not now" on the second one closes it; nothing is left open.
    let path = format!("/secrets/requests/{}/dismiss", b["data"]["requestId"].as_str().unwrap());
    let (s, d) = env.h.post(&path, json!({})).await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["data"]["status"], "dismissed");
    assert_eq!(env.queue().await["active"], Value::Null);

    // A repeat of what was just closed answers with the first card (REQUEST_REPEAT).
    let (_, again) = env.h.post("/secrets/request", wifi("SECOND_KEY")).await;
    assert_eq!(again["data"]["status"], "already_asked");

    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_request_carrying_a_value_is_refused_and_leaves_no_card() {
    let env = setup().await;
    let mut body = wifi("A_KEY");
    body["value"] = json!(VALUE);
    let (s, r) = env.h.post("/secrets/request", body).await;
    assert_eq!(s, 400, "{r}");
    assert_clean("the refusal", &r.to_string());
    let mut body = wifi("A_KEY");
    body["reason"] = json!(format!("use {VALUE}"));
    let (s, r) = env.h.post("/secrets/request", body).await;
    assert_eq!(s, 400, "{r}");
    assert!(env.request_cards().is_empty());
    assert_eq!(env.queue().await["requests"], json!([]));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn salva_un_segreto_opens_a_card_before_the_model() {
    let env = setup().await;
    let d = env.chat("salva il segreto GBGO_WIFI_PSK in lucinaos/prod").await;
    assert_eq!(d["action"], "secret_request");
    assert!(env.h.fake.requests().is_empty(), "decided without the model");
    assert!(d["userItemId"].is_string(), "his words are in the chat");
    assert_eq!(d["format"], "v1", "the card comes with the answer");
    assert_eq!(d["actions"][0]["payload"]["name"], "GBGO_WIFI_PSK");
    let q = env.queue().await;
    let e = &q["requests"][0];
    assert_eq!((e["name"].as_str(), e["project"].as_str(), e["environment"].as_str()), (Some("GBGO_WIFI_PSK"), Some("lucinaos"), Some("prod")));
    assert_eq!(q["active"], e["id"]);
    assert_eq!(d["lucinaItemId"], e["itemId"]);

    // A second one while that card is open: it waits, and the reply has its own item.
    let d2 = env.chat("add a secret called OTHER_ONE in lucinaos/prod").await;
    assert_eq!(d2["action"], "secret_request");
    assert!(d2["lucinaItemId"].is_string());
    assert!(d2.get("format").is_none());
    assert_eq!(env.queue().await["queued"], 1);

    // Without a name or a place: the card asks for them.
    env.h.post(&format!("/secrets/requests/{}/dismiss", e["id"].as_str().unwrap()), json!({})).await;
    let d3 = env.chat("salva un segreto").await;
    assert_eq!(d3["action"], "secret_request");
    let q = env.queue().await;
    assert_eq!(q["requests"][2]["name"], "");
    assert_eq!(q["requests"][2]["project"], "");
    assert!(env.h.fake.requests().is_empty());
}

#[tokio::test]
async fn a_value_typed_after_the_name_is_not_recorded_anywhere() {
    let env = setup().await;
    // Not token-shaped, so nothing masks it: only the words path keeps it from every sink.
    const PLAIN: &str = "correct-horse-battery";
    let d = env.chat(&format!("salva il segreto WIFI_PSK {PLAIN}")).await;
    assert_eq!(d["action"], "secret_request");
    assert!(d["userItemId"].is_null(), "not recorded");
    assert!(!d["reply"].as_str().unwrap().contains(PLAIN));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(env.h.fake.requests().is_empty());
    let timeline = serde_json::to_string(&env.h.timeline_lines()).unwrap();
    assert!(!timeline.contains(PLAIN), "{timeline}");
    let queue = env.queue().await.to_string();
    assert!(!queue.contains(PLAIN), "{queue}");
    assert!(!String::from_utf8_lossy(&logs().lock().unwrap()).contains(PLAIN));
    for r in env.honcho.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&r.body).contains(PLAIN));
    }
}

#[tokio::test]
async fn lucina_asks_in_her_own_turn_by_name_only() {
    logs();
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "wifi", "afterTool": false, "toolCalls": [{"name": "secret_request", "arguments": {
                "name": "GBGO_WIFI_PSK", "project": "lucinaos", "environment": "prod", "reason": "the laptop joins the Wi-Fi at boot"}}]},
            {"kind": "agent", "contains": "wifi", "afterTool": true, "content": "La scheda ti aspetta nell'app."},
            {"kind": "agent", "contains": "porta", "afterTool": false, "toolCalls": [{"name": "secret_request", "arguments": {
                "name": "X_KEY", "project": "p", "environment": "prod", "reason": "x", "value": VALUE}}]},
            {"kind": "agent", "contains": "porta", "afterTool": true, "content": "Non posso."}
        ]})),
        |c| c.planner = lucina_server::PlannerMode::Tools,
    )
    .await;
    let (s, b) = h.post("/chat", json!({"message": "serve la password del wifi", "sessionId": "test", "channel": "web"})).await;
    assert_eq!(s, 200, "{b}");
    let cards: Vec<Value> = h.timeline_lines().into_iter().filter(|l| l["meta"]["kind"] == "secret_request").collect();
    assert_eq!(cards.len(), 1, "{:?}", h.timeline_lines());
    assert_eq!(cards[0]["meta"]["actions"][0]["payload"]["name"], "GBGO_WIFI_PSK");
    assert!(cards[0]["text"].as_str().unwrap().contains("Lucina asks for the secret"));
    // A tool call with a value is refused by the schema: no card, and the queue has one entry.
    let (s, b) = h.post("/chat", json!({"message": "apri la porta", "sessionId": "test", "channel": "web"})).await;
    assert_eq!(s, 200, "{b}");
    let (_, q) = h.get("/secrets/requests").await;
    assert_eq!(q["data"]["requests"].as_array().unwrap().len(), 1);
    assert_clean("queue", &q.to_string());
}

#[tokio::test]
async fn an_mcp_turn_gets_no_secret_cards() {
    let env = setup().await;
    assert!(env.h.state.mcp_registry.get("secret_request").is_none());
    assert!(env.h.state.trusted_mcp_registry.get("secret_request").is_none());
    assert!(env.h.state.registry.get("secret_request").is_some());
}

#[tokio::test]
async fn in_his_own_words_lucina_asks_by_name_on_the_json_planner() {
    logs();
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "password del wifi", "afterTool": false, "toolCalls": [{"name": "secret_tile", "arguments": {
                "name": "GBGO_WIFI_PSK", "project": "lucinaos", "environment": "prod"}}]},
            {"kind": "agent", "contains": "password del wifi", "afterTool": true, "content": "La scheda ti aspetta nell'app."},
            {"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}
        ]})),
        |c| c.planner = lucina_server::PlannerMode::Json,
    )
    .await;
    let (s, b) = h.post("/chat", json!({"message": "salvami la password del wifi di casa", "sessionId": "test", "channel": "web"})).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["action"], "secret_request");
    let (_, q) = h.get("/secrets/requests").await;
    assert_eq!(q["data"]["requests"][0]["name"], "GBGO_WIFI_PSK");
    // His own request now (secret_tile): the card is his, at the bottom of his chat.
    assert_eq!(q["data"]["requests"][0]["who"], "");
    assert_eq!(q["data"]["requests"][0]["status"], "active");
}
