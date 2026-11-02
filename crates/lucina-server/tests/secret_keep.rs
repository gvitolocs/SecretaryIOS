//! Lucina moves a secret to Infisical herself: a canary value goes from a note or a chat through a
//! fake local model into a fake broker, and leaves its source. Every sink is searched for it: the
//! cloud model's requests, the timeline and every data file, Honcho, the notes server's other calls,
//! tool results and logs, on the failure paths too. With the local model down it fails closed.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{harness, script};
use lucina_memory::MemoryMode;
use lucina_server::secret_keep::State;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wiremock::matchers::{any, method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const CANARY: &str = "canary-Keep-7f3a9c1e-Xq2";
const CANARY_WORDS: &str = "Canary-Pass-9f21";

#[derive(Clone)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuf(buf.clone());
        tracing_subscriber::fmt()
            .with_env_filter("debug,wiremock=off")
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .ok();
        buf
    })
    .clone()
}

fn assert_clean(what: &str, text: &str) {
    for canary in [CANARY, CANARY_WORDS] {
        assert!(!text.contains(canary), "the value reached {what}");
        let hash: String = Sha256::digest(canary.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!text.contains(&hash), "the value's hash reached {what}");
    }
}

fn scan_dir(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            scan_dir(&p);
        } else if let Ok(bytes) = std::fs::read(&p) {
            assert_clean(&p.display().to_string(), &String::from_utf8_lossy(&bytes));
        }
    }
}

/// A broker that keeps what it is given and reads back its hash.
#[derive(Clone, Default)]
struct Broker {
    socket: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    secrets: Arc<Mutex<Vec<(String, String)>>>,
    answer: Arc<Mutex<Option<Value>>>,
    corrupt: Arc<Mutex<bool>>,
}

async fn broker(dir: &std::path::Path) -> Broker {
    let socket = dir.join("broker.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let b = Broker {
        socket,
        ..Broker::default()
    };
    let bb = b.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let (rd, mut wr) = stream.into_split();
            let mut line = String::new();
            BufReader::new(rd).read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            bb.seen.lock().unwrap().push(req.clone());
            let out = bb.answer.lock().unwrap().clone().unwrap_or_else(|| {
                let reference = req["ref"].as_str().unwrap_or_default().to_owned();
                let value = req["value"].as_str().unwrap_or_default().to_owned();
                let mut secrets = bb.secrets.lock().unwrap();
                if let Some((_, have)) = secrets.iter().find(|(r, _)| *r == reference) {
                    if *have != value {
                        return json!({"ok": false, "error": {"code": "exists", "message": "another value"}});
                    }
                } else {
                    secrets.push((reference.clone(), value.clone()));
                }
                let stored = if *bb.corrupt.lock().unwrap() { "other".to_owned() } else { value };
                let hash: String = Sha256::digest(stored.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
                json!({"ok": true, "result": {"created": true, "readback_sha256": hash}})
            });
            let mut bytes = serde_json::to_vec(&out).unwrap();
            bytes.push(b'\n');
            wr.write_all(&bytes).await.unwrap();
        }
    });
    b
}

/// The notes server: one note, which PATCH changes.
#[derive(Clone)]
struct Notes {
    note: Arc<Mutex<Value>>,
    refuse_patch: Arc<Mutex<bool>>,
}

impl Respond for Notes {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let mut note = self.note.lock().unwrap();
        match req.method.as_str() {
            "GET" if req.url.path().ends_with("/changes") => ResponseTemplate::new(200)
                .set_body_json(json!({
                    "notes": [note.clone()], "removed": [], "cursor": "9", "hasMore": false
                })),
            "GET"
                if note["slug"]
                    .as_str()
                    .is_some_and(|s| req.url.path().ends_with(s))
                    || note["id"]
                        .as_str()
                        .is_some_and(|s| req.url.path().ends_with(s)) =>
            {
                ResponseTemplate::new(200).set_body_json(note.clone())
            }
            "PATCH" => {
                if *self.refuse_patch.lock().unwrap() {
                    return ResponseTemplate::new(500)
                        .set_body_json(json!({"error": {"code": "nope"}}));
                }
                for k in ["content", "title", "items", "slug"] {
                    if let Some(v) = body.get(k) {
                        note[k] = v.clone();
                    }
                }
                ResponseTemplate::new(200).set_body_json(note.clone())
            }
            _ => ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "not_found"}})),
        }
    }
}

struct Env {
    h: common::H,
    broker: Broker,
    honcho: MockServer,
    notes: MockServer,
    note: Arc<Mutex<Value>>,
    refuse_patch: Arc<Mutex<bool>>,
    qwen: MockServer,
    _side: tempfile::TempDir,
    backups: PathBuf,
}

/// The fake gateway's answer to every call.
async fn qwen_says(q: &MockServer, content: Value) {
    q.reset().await;
    Mock::given(method("POST"))
        .and(path_regex("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": content.to_string()}}]
        })))
        .mount(q)
        .await;
}

async fn setup() -> Env {
    setup_with_script(script(json!({"rules": [{"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok, fatto.\"}"}]}))).await
}

async fn setup_with_script(s: lucina_server::fake::Script) -> Env {
    logs();
    let side = tempfile::tempdir().unwrap();
    let broker = broker(side.path()).await;
    let token_file = side.path().join("keeper.token");
    std::fs::write(&token_file, "keeper-token-0123456789abcdef\n").unwrap();
    let qwen_key = side.path().join("qwen.key");
    std::fs::write(&qwen_key, "qwen-local-key\n").unwrap();
    let service_token = side.path().join("svc.token");
    std::fs::write(&service_token, "svc_test_token\n").unwrap();
    let honcho = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&honcho)
        .await;
    let qwen = MockServer::start().await;
    let note = Arc::new(Mutex::new(json!({
        "id": "note1", "slug": "telegram", "title": "telegram",
        "content": format!("the bot\n{CANARY}\nkeep this line"),
        "updatedAt": "2026-11-02T00:00:00Z"
    })));
    let refuse_patch = Arc::new(Mutex::new(false));
    let notes = MockServer::start().await;
    Mock::given(any())
        .respond_with(Notes {
            note: note.clone(),
            refuse_patch: refuse_patch.clone(),
        })
        .mount(&notes)
        .await;
    let backups = side.path().join("backups");
    std::fs::create_dir_all(backups.join("2026-11-01")).unwrap();
    std::fs::create_dir_all(backups.join("2026-11-02")).unwrap();
    let copy = format!("{{\"id\":\"note1\",\"content\":\"{CANARY}\"}}");
    std::fs::write(backups.join("2026-11-01/note1.json"), &copy).unwrap();
    std::fs::hard_link(
        backups.join("2026-11-01/note1.json"),
        backups.join("2026-11-02/note1.json"),
    )
    .unwrap();
    let (hu, nu, qu, socket, b2) = (
        honcho.uri(),
        notes.uri(),
        format!("{}/v1", qwen.uri()),
        broker.socket.clone(),
        backups.clone(),
    );
    let h = harness(s, move |c| {
        c.memory_mode = MemoryMode::ReadWrite;
        c.honcho.base_url = hu;
        c.honcho.api_key = "fake-honcho-key-123456".into();
        c.secretary_url = Some(nu);
        c.secretary_service_token_file = Some(service_token);
        c.secret_keep.enabled = true;
        c.secret_keep.token_file = Some(token_file);
        c.secret_keep.socket = socket;
        c.secret_keep.local.url = qu;
        c.secret_keep.local.key_file = Some(qwen_key);
        c.secret_keep.local.timeout = Duration::from_secs(2);
        c.secret_keep.backup_dir = Some(b2);
        c.secret_keep.scrub_dirs = vec![c.data_dir.clone()];
    })
    .await;
    Env {
        h,
        broker,
        honcho,
        notes,
        note,
        refuse_patch,
        qwen,
        _side: side,
        backups,
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

    fn keeper(&self) -> &lucina_server::secret_keep::Keeper {
        &self.h.state.secret_keep
    }

    fn job(&self, body: Value) -> String {
        self.keeper().enqueue(&body).unwrap().id
    }

    async fn run(&self, id: &str) -> lucina_server::secret_keep::Job {
        self.keeper().make_all_due_for_test();
        assert!(
            self.keeper().run_next(&self.h.state).await,
            "nothing was due"
        );
        self.keeper().job(id).unwrap()
    }

    /// Every sink but the two that must hold it: the local model's calls and the broker's.
    async fn assert_no_value_anywhere(&self) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_clean(
            "cloud model requests",
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
        let logged = String::from_utf8_lossy(&logs().lock().unwrap()).into_owned();
        assert_clean("logs", &logged);
    }

    fn broker_saw(&self, reference: &str) -> bool {
        self.broker
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["op"] == "deposit" && r["ref"] == reference && r["value"] == CANARY)
    }
}

#[tokio::test]
async fn a_note_goes_to_infisical_and_only_the_value_leaves_it() {
    let env = setup().await;
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_TELEGRAM_BOT_TOKEN", "project": "Personal", "environment": "prod", "what": "the bot token"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let c = j.cleared.clone().unwrap();
    assert!(c.note, "{c:?}");
    assert_eq!(c.backups, 2, "both hard-linked snapshots");
    assert!(env.broker_saw("secret://personal/prod#KEEP_TELEGRAM_BOT_TOKEN"));
    let note = env.note.lock().unwrap().clone();
    let content = note["content"].as_str().unwrap();
    assert_eq!(
        content,
        "the bot\n[secret:KEEP_TELEGRAM_BOT_TOKEN]\nkeep this line"
    );
    assert_clean(
        "backup",
        &std::fs::read_to_string(env.backups.join("2026-11-01/note1.json")).unwrap(),
    );
    assert_clean(
        "backup",
        &std::fs::read_to_string(env.backups.join("2026-11-02/note1.json")).unwrap(),
    );
    // The outcome is names: this is what the cloud model would get.
    assert_clean("outcome", &j.outcome().to_string());
    let lines = env.h.timeline_lines();
    assert!(lines.iter().any(|l| l["meta"]["kind"] == "secret_receipt"
        && l["text"]
            .as_str()
            .unwrap()
            .contains("KEEP_TELEGRAM_BOT_TOKEN")));
    // The local step used the local model and nothing else.
    assert!(env.h.fake.requests().is_empty(), "a cloud model was called");
    let asked = env.qwen.received_requests().await.unwrap();
    assert_eq!(asked.len(), 1);
    // A note that is only a value says what it is in its title.
    assert!(String::from_utf8_lossy(&asked[0].body).contains("TITLE: telegram"));
    // From now on the value is masked at every sink.
    assert!(lucina_secrets::redactor()
        .redact(CANARY)
        .starts_with("[secret:"));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_chat_message_is_cleared_everywhere() {
    let env = setup().await;
    // Written before this existed: an old message that holds the value, and a recent turn.
    let item = env
        .h
        .state
        .timeline
        .append(lucina_store::timeline::NewItem {
            channel: "web".into(),
            role: "user".into(),
            text: format!(
                "old message Kanary {}",
                CANARY_WORDS.replace("Canary", "Kanary")
            ),
            ..Default::default()
        })
        .unwrap();
    let value = CANARY_WORDS.replace("Canary", "Kanary");
    env.h.state.recent.append(
        "web:test",
        "user",
        &format!("said {value}"),
        chrono::Utc::now(),
    );
    qwen_says(&env.qwen, json!({"found": true, "value": value})).await;
    let id = env.job(json!({"source": format!("chat:{}", item.id), "name": "KEEP_OLD", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let c = j.cleared.clone().unwrap();
    assert_eq!(c.chat_items, 1);
    assert_eq!(c.recent_turns, 1);
    let got = env.h.state.timeline.get(&item.id).unwrap().unwrap();
    assert!(!got.raw.contains(&value));
    assert!(
        got.raw.contains("old message"),
        "the rest of the message stays"
    );
    assert!(got.raw.contains("[secret:KEEP_OLD]"));
    assert!(!env.h.state.recent.turns("web:test")[0]
        .text
        .contains(&value));
    scan_dir(env.h.dir.path());
}

#[tokio::test]
async fn whole_note_mode_needs_no_model_and_leaves_a_pointer() {
    let env = setup().await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_PASSWORDS_NOTE", "project": "personal", "environment": "prod", "whole": true}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    assert!(env.qwen.received_requests().await.unwrap().is_empty());
    let note = env.note.lock().unwrap().clone();
    assert_eq!(
        note["content"],
        "[secret:KEEP_PASSWORDS_NOTE] (moved to Infisical)"
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_list_note_clears_each_item_or_becomes_a_pointer() {
    let env = setup().await;
    *env.note.lock().unwrap() = json!({
        "id": "note1", "slug": "telegram", "title": "telegram",
        "content": format!("- [ ] bot {CANARY}\n- [ ] other"),
        "items": [{"id": "i_000000000001", "text": format!("bot {CANARY}"), "checked": false, "indent": 0},
                  {"id": "i_000000000002", "text": "other", "checked": true, "indent": 0}],
        "updatedAt": "2026-11-02T00:00:00Z"
    });
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_LIST", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let note = env.note.lock().unwrap().clone();
    assert_eq!(note["items"][0]["text"], "bot [secret:KEEP_LIST]");
    assert_eq!(note["items"][1]["text"], "other");
    assert_eq!(note["items"][1]["checked"], true);
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_LIST_WHOLE", "project": "personal", "environment": "prod", "whole": true}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let note = env.note.lock().unwrap().clone();
    assert_eq!(
        note["items"],
        json!([{"text": "[secret:KEEP_LIST_WHOLE] (moved to Infisical)"}])
    );
}

#[tokio::test]
async fn the_local_model_down_fails_closed() {
    let env = setup().await;
    // Nothing mounted: the gateway answers 404, as it does while it can't serve.
    env.qwen.reset().await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(
        j.state,
        State::Queued,
        "it waits, it doesn't give up at once"
    );
    assert_eq!(j.code.as_deref(), Some("local_model_unavailable"));
    assert!(env.h.fake.requests().is_empty(), "a cloud model was called");
    assert!(
        env.broker.seen.lock().unwrap().is_empty(),
        "nothing was saved"
    );
    assert!(
        env.note.lock().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains(CANARY),
        "nothing was cleared"
    );
    // After the whole wait it fails, says so and still never used anything else.
    env.keeper().age_all_for_test(7 * 3600);
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Failed);
    assert_eq!(j.code.as_deref(), Some("local_model_unavailable"));
    assert!(env.h.fake.requests().is_empty());
    assert!(env.note.lock().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains(CANARY));
    let lines = env.h.timeline_lines();
    assert!(
        lines
            .iter()
            .any(|l| l["text"].as_str().unwrap_or("").contains("didn't save")),
        "{lines:?}"
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_cloud_address_is_refused_before_anything() {
    logs();
    let env = setup().await;
    let cfg = lucina_server::secret_keep::local::LocalConfig {
        url: "https://api.deepseek.com/v1".into(),
        model: "x".into(),
        key_file: None,
        timeout: Duration::from_secs(1),
    };
    assert!(!cfg.is_loopback());
    assert_eq!(
        lucina_server::secret_keep::local::extract(&reqwest::Client::new(), &cfg, "A", "", "", CANARY)
            .await
            .err(),
        Some(lucina_server::secret_keep::local::LocalError::NotLocal)
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_value_the_model_made_up_is_not_trusted() {
    let env = setup().await;
    qwen_says(
        &env.qwen,
        json!({"found": true, "value": "hallucinated-value-123"}),
    )
    .await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(
        (j.state, j.code.as_deref()),
        (State::Failed, Some("not_in_source"))
    );
    assert!(env.broker.seen.lock().unwrap().is_empty());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn found_nothing_or_garbage_clears_nothing() {
    let env = setup().await;
    qwen_says(&env.qwen, json!({"found": false})).await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod"}));
    assert_eq!(env.run(&id).await.code.as_deref(), Some("no_secret"));
    env.qwen.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"choices": [{"message": {"content": format!("sure, it is {CANARY}")}}]}),
        ))
        .mount(&env.qwen)
        .await;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_Y", "project": "personal", "environment": "prod"}));
    assert_eq!(
        env.run(&id).await.code.as_deref(),
        Some("local_model_unclear")
    );
    assert!(env.note.lock().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains(CANARY));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn nothing_is_cleared_unless_the_save_is_confirmed() {
    let env = setup().await;
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    let body = json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod"});

    *env.broker.corrupt.lock().unwrap() = true;
    let id = env.job(body.clone());
    assert_eq!(
        env.run(&id).await.code.as_deref(),
        Some("readback_mismatch")
    );
    assert!(env.note.lock().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains(CANARY));
    *env.broker.corrupt.lock().unwrap() = false;

    for (code, want) in [
        ("not_allowed", "not_allowed"),
        ("exists", "exists"),
        ("unknown_project", "unknown_place"),
    ] {
        *env.broker.answer.lock().unwrap() =
            Some(json!({"ok": false, "error": {"code": code, "message": "m"}}));
        let id = env.job(body.clone());
        assert_eq!(env.run(&id).await.code.as_deref(), Some(want));
        assert!(
            env.note.lock().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains(CANARY),
            "{code}"
        );
    }
    // The broker unreachable: it waits and clears nothing.
    *env.broker.answer.lock().unwrap() =
        Some(json!({"ok": false, "error": {"code": "upstream_unavailable", "message": "m"}}));
    let id = env.job(body.clone());
    let j = env.run(&id).await;
    assert_eq!(
        (j.state, j.code.as_deref()),
        (State::Queued, Some("broker_unavailable"))
    );
    assert!(env.note.lock().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains(CANARY));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_failed_clear_is_said_plainly_and_the_save_stands() {
    let env = setup().await;
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    *env.refuse_patch.lock().unwrap() = true;
    let id = env.job(json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved);
    let c = j.cleared.unwrap();
    assert!(!c.note);
    assert!(
        c.remains.iter().any(|r| r.contains("still holds it")),
        "{:?}",
        c.remains
    );
    let lines = env.h.timeline_lines();
    assert!(lines
        .iter()
        .any(|l| l["text"].as_str().unwrap_or("").contains("clearing failed")));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_value_in_the_cloud_models_hint_is_refused() {
    let env = setup().await;
    for what in [
        "the value ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_owned(),
        format!("the password is {CANARY_WORDS}"),
    ] {
        let e = env.keeper().enqueue(&json!({"source": "note:telegram", "name": "KEEP_X", "project": "personal", "environment": "prod", "what": what})).unwrap_err();
        assert_eq!(e.code, "value_refused");
    }
    for source in [
        "telegram",
        "held:abc",
        "note:",
        "chat:not-an-id",
        "file:/etc/passwd",
    ] {
        let e = env
            .keeper()
            .enqueue(&json!({"source": source, "name": "KEEP_X"}))
            .unwrap_err();
        assert_eq!(e.code, "bad_request", "{source}");
    }
}

#[tokio::test]
async fn a_password_in_a_message_never_reaches_a_sink_and_gets_saved() {
    let env = setup().await;
    qwen_says(
        &env.qwen,
        json!({"credential": true, "name": "home wifi password"}),
    )
    .await;
    let d = env
        .chat(&format!("la password del wifi è {CANARY_WORDS}"))
        .await;
    assert_clean("reply", &d.to_string());
    // The turn was answered from the masked text.
    let asked = serde_json::to_string(&env.h.fake.requests()).unwrap();
    assert!(
        asked.contains("[secret:credential]"),
        "the model got the masked text"
    );
    env.assert_no_value_anywhere().await;
    // And the worker saves it, naming it through the local model.
    assert!(
        env.keeper().run_next(&env.h.state).await,
        "a job should be queued"
    );
    let saved = env.broker.seen.lock().unwrap().clone();
    assert_eq!(
        saved[0]["ref"],
        "secret://personal/prod#KEPT_HOME_WIFI_PASSWORD"
    );
    assert_eq!(saved[0]["value"], CANARY_WORDS);
    let lines = env.h.timeline_lines();
    assert!(
        lines.iter().any(|l| l["meta"]["kind"] == "secret_receipt"
            && l["text"]
                .as_str()
                .unwrap()
                .contains("KEPT_HOME_WIFI_PASSWORD")),
        "{lines:?}"
    );
    assert!(
        env.h.state.held_secrets.is_empty(),
        "the held value is gone"
    );
    assert!(
        !lines.iter().any(|l| l["meta"]["kind"] == "secret_offer"),
        "no card when it was saved"
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn with_the_local_model_down_a_message_gets_the_card() {
    let env = setup().await;
    env.qwen.reset().await;
    env.chat(&format!("la password del wifi è {CANARY_WORDS}"))
        .await;
    assert!(env.keeper().run_next(&env.h.state).await);
    assert!(env.broker.seen.lock().unwrap().is_empty());
    // Still waiting for it; then the card comes while the value is still held.
    assert!(!env
        .h
        .timeline_lines()
        .iter()
        .any(|l| l["meta"]["kind"] == "secret_offer"));
    env.keeper().age_all_for_test(3600);
    env.keeper().make_all_due_for_test();
    assert!(env.keeper().run_next(&env.h.state).await);
    assert!(env
        .h
        .timeline_lines()
        .iter()
        .any(|l| l["meta"]["kind"] == "secret_offer"));
    assert!(env
        .h
        .fake
        .requests()
        .iter()
        .all(|r| !r.to_string().contains(CANARY_WORDS)));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn an_ordinary_word_the_local_model_rejects_is_dropped() {
    let env = setup().await;
    qwen_says(&env.qwen, json!({"credential": false})).await;
    env.chat("the password is cavolfiore").await;
    assert!(env.keeper().run_next(&env.h.state).await);
    assert!(env.broker.seen.lock().unwrap().is_empty());
    assert!(env.h.state.held_secrets.is_empty());
    assert!(!env
        .h
        .timeline_lines()
        .iter()
        .any(|l| l["meta"]["kind"] == "secret_offer"));
}

#[tokio::test]
async fn the_scan_returns_references_only() {
    let env = setup().await;
    env.note.lock().unwrap()["content"] = json!(format!("the password is {CANARY_WORDS}"));
    let out = lucina_tools::tools::secret_keep::SecretKeeper::scan(env.keeper(), "test")
        .await
        .unwrap();
    assert_eq!(out["notes"][0]["source"], "note:note1");
    assert_clean("scan", &out.to_string());
    let _ = &env.notes;
}

#[tokio::test]
async fn a_name_made_from_the_value_goes_too_and_never_shows() {
    let env = setup().await;
    let value = "Zz9Yy8Xx7Ww6Vv5Uu4Tt3-fake";
    let slug = lucina_secrets::labelled::as_slug(value);
    {
        let mut note = env.note.lock().unwrap();
        note["id"] = json!("n_00112233445566778899");
        note["slug"] = json!(slug);
        note["title"] = json!("");
        note["content"] = json!(format!("{value}\nthe bot"));
    }
    // The scan names it by id, and its name isn't in the answer.
    let out = lucina_tools::tools::secret_keep::SecretKeeper::scan(env.keeper(), "test")
        .await
        .unwrap();
    assert_eq!(out["notes"][0]["source"], "note:n_00112233445566778899", "{out}");
    assert_eq!(out["notes"][0]["title"], "untitled");
    assert!(!out.to_string().contains(&slug), "{out}");
    // The sweep's offer is the same: no slug in the card or its button.
    lucina_server::secret_keep::sweep::run(&env.h.state).await;
    let lines = serde_json::to_string(&env.h.timeline_lines()).unwrap();
    assert!(lines.contains("secret_offer"), "{lines}");
    assert!(lines.contains("note n_00112233445566778899"), "{lines}");
    assert!(!lines.contains(&slug), "{lines}");
    // And saving it renames the note.
    qwen_says(&env.qwen, json!({"found": true, "value": value})).await;
    let id = env.job(json!({"source": "note:n_00112233445566778899", "name": "KEEP_SLUG", "project": "personal", "environment": "prod"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let note = env.note.lock().unwrap().clone();
    assert_eq!(note["slug"], "kept-0011223344");
    assert_eq!(note["content"], "[secret:KEEP_SLUG]\nthe bot");
}

#[tokio::test]
async fn lucina_moves_it_in_her_own_turn_by_reference() {
    logs();
    let env = setup_with_script(script(json!({"rules": [
        {"kind": "agent", "contains": "note telegram", "afterTool": false, "toolCalls": [{"name": "secret_keep", "arguments": {
            "source": "note:telegram", "name": "TELEGRAM_BOT_TOKEN", "what": "the bot token"}}]},
        {"kind": "agent", "contains": "note telegram", "afterTool": true, "content": "It's on its way to Infisical."},
        {"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}
    ]})))
    .await;
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    let d = env
        .chat("Move the password in note telegram to Infisical")
        .await;
    assert_eq!(d["reply"], "It's on its way to Infisical.", "{d}");
    assert_clean("reply", &d.to_string());
    assert!(
        env.keeper().run_next(&env.h.state).await,
        "the job should be queued"
    );
    // No project named: the default place, and a name its grant allows.
    assert!(env.broker_saw("secret://personal/prod#KEPT_TELEGRAM_BOT_TOKEN"));
    assert_eq!(
        env.note.lock().unwrap()["content"],
        "the bot\n[secret:KEPT_TELEGRAM_BOT_TOKEN]\nkeep this line"
    );
    // What the cloud model read back: the reference and names, and never the note.
    let asked = serde_json::to_string(&env.h.fake.requests()).unwrap();
    assert!(
        asked.contains("KEPT_TELEGRAM_BOT_TOKEN"),
        "the tool result reached the model"
    );
    assert!(
        !asked.contains("keep this line"),
        "the note's text reached the cloud model"
    );
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn an_agents_turn_is_not_masked_as_words() {
    let env = setup().await;
    let (masked, held) = lucina_server::owner_secrets::mask_message(
        &env.h.state,
        "the password is Cavolo-23",
        false,
    );
    assert!(held.is_empty());
    assert_eq!(masked, "the password is Cavolo-23");
    let (masked, held) =
        lucina_server::owner_secrets::mask_message(&env.h.state, "the password is Cavolo-23", true);
    assert_eq!(held.len(), 1);
    assert!(
        !held[0].1.contains("Cav"),
        "a value in words shows no characters: {}",
        held[0].1
    );
    assert_eq!(masked, "the password is [secret:credential]");
}
