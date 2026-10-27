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
    for v in [VALUE, PASTED] {
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
    env.chat(&format!("/secret OTHER_KEY {PASTED}")).await;
    let held = env.offer_actions()[0]["payload"]["held"]
        .as_str()
        .unwrap()
        .to_owned();
    let (s, _) = env
        .h
        .post("/secrets/grant", json!({"grant": "g1.p.s", "held": held}))
        .await;
    assert_eq!(s, 200);
    assert_eq!(env.broker.seen.lock().unwrap()[1]["value"], PASTED);
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
