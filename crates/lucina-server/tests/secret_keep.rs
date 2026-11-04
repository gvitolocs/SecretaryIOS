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
    for canary in [CANARY, CANARY_WORDS, CANARY_FILE, CANARY_LINE, CANARY_GOOGLE] {
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
    let home = side.path().to_path_buf();
    std::fs::create_dir_all(home.join("config")).unwrap();
    std::fs::create_dir_all(home.join("projects")).unwrap();
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
        c.secret_keep.source_roots = vec![home.join("config"), home.join("projects")];
        c.secret_keep.source_deny = vec![home.join("config/keeper-tokens")];
        c.secret_keep.unit_dirs = vec![home.join("units")];
        c.secret_keep.home = home;
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
    assert!(lines.iter().any(|l| l["meta"]["kind"] == "secret_receipt" && l["meta"]["via"] == "agent"
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
        "ftp:/etc/passwd",
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
        lines.iter().any(|l| l["meta"]["kind"] == "secret_receipt" && l["meta"]["via"].is_null()
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

// ---------------------------------------------------------------- files and env files

const CANARY_FILE: &str = "canary-File-3b1d7e-Zz9";
const CANARY_LINE: &str = "canary-Line-a4f8c2-Kk7";
const CANARY_GOOGLE: &str = "canary-Google-77e1d0-Qp3";

impl Env {
    fn side(&self) -> PathBuf {
        self._side.path().to_path_buf()
    }

    /// A file of the user's, mode 0600, below the side folder.
    fn plant(&self, rel: &str, text: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = self.side().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        p
    }

    fn deposits(&self) -> Vec<(String, String)> {
        self.broker
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["op"] == "deposit")
            .map(|r| (r["ref"].as_str().unwrap().to_owned(), r["value"].as_str().unwrap().to_owned()))
            .collect()
    }

    fn move_state(j: &lucina_server::secret_keep::Job, key: &str) -> String {
        j.moves.iter().find(|m| m.key == key).unwrap_or_else(|| panic!("no {key}: {:?}", j.moves)).state.clone()
    }
}

fn mode_of(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

#[tokio::test]
async fn a_file_moves_with_no_model_and_stays_where_it_is() {
    let env = setup().await;
    let file = env.plant("config/flareon-list/login.key", &format!("{CANARY}\n"));
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_FLAREON_LIST_LOGIN"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    assert_eq!(env.deposits(), vec![("secret://personal/prod#KEEP_FLAREON_LIST_LOGIN".to_owned(), CANARY.to_owned())], "the value, without its newline");
    assert!(env.qwen.received_requests().await.unwrap().is_empty(), "no model read it");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{CANARY}\n"), "never removed unless asked");
    assert_eq!(mode_of(&file), 0o600);
    let c = j.cleared.clone().unwrap();
    assert!(!c.source && c.clear_state.is_none(), "{c:?}");
    assert_eq!(j.moves[0].state, "saved");
    assert_clean("the job's outcome", &j.outcome().to_string());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn tilde_means_the_configured_home() {
    let env = setup().await;
    env.plant("config/app/key", &format!("{CANARY}\n"));
    let id = env.job(json!({"source": "file:~/config/app/key", "name": "KEEP_APP"}));
    assert_eq!(env.run(&id).await.state, State::Saved);
}

#[tokio::test]
async fn an_env_file_by_plain_parsing_each_key_under_its_own_name() {
    let env = setup().await;
    let file = env.plant(
        "projects/work/app/.env",
        &format!(
            "# config\nPORT=8080\nAPI_TOKEN={CANARY}\nDB_PASSWORD=\"{CANARY_FILE}\"\nOTHER_SECRET=changeme\nEMPTY_TOKEN=\nREF_TOKEN=${{API_TOKEN}}\nGOOGLE_PASSWORD={CANARY_GOOGLE}\n{CANARY_LINE}\n"
        ),
    );
    let before = std::fs::read_to_string(&file).unwrap();
    let id = env.job(json!({"source": format!("env:{}", file.display()), "project": "apis", "environment": "dev", "path": "/keep"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    assert!(env.qwen.received_requests().await.unwrap().is_empty(), "no model at all");
    assert_eq!(
        env.deposits(),
        vec![
            ("secret://apis/dev/keep#API_TOKEN".to_owned(), CANARY.to_owned()),
            ("secret://apis/dev/keep#DB_PASSWORD".to_owned(), CANARY_FILE.to_owned()),
        ]
    );
    assert_eq!(Env::move_state(&j, "API_TOKEN"), "saved");
    assert_eq!(Env::move_state(&j, "PORT"), "left");
    assert_eq!(Env::move_state(&j, "OTHER_SECRET"), "placeholder");
    assert_eq!(Env::move_state(&j, "EMPTY_TOKEN"), "empty");
    assert_eq!(Env::move_state(&j, "REF_TOKEN"), "reference");
    assert_eq!(Env::move_state(&j, "GOOGLE_PASSWORD"), "refused_by_rule");
    assert_eq!(Env::move_state(&j, "line 9"), "unclear", "a stray line is only a number");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "the file stays");
    assert_clean("the job's outcome", &j.outcome().to_string());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn an_env_file_in_the_default_place_gets_names_its_grant_allows() {
    let env = setup().await;
    let file = env.plant("config/flareon-list/.env", &format!("API_TOKEN={CANARY}\nKEEP_FLAREON_LIST_OLD={CANARY_FILE}\n"));
    let id = env.job(json!({"source": format!("env:{}", file.display()), "keys": ["API_TOKEN", "KEEP_FLAREON_LIST_OLD"]}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let refs: Vec<String> = env.deposits().into_iter().map(|(r, _)| r).collect();
    assert_eq!(refs, vec!["secret://personal/prod#KEEP_FLAREON_LIST_API_TOKEN", "secret://personal/prod#KEEP_FLAREON_LIST_OLD"]);
    let m = j.moves.iter().find(|m| m.key == "API_TOKEN").unwrap();
    assert_eq!(m.name, "KEEP_FLAREON_LIST_API_TOKEN", "the receipt maps the key to the stored name");
    assert_eq!(m.reference, "secret://personal/prod#KEEP_FLAREON_LIST_API_TOKEN");
}

#[tokio::test]
async fn named_keys_are_the_only_ones_and_a_config_value_can_be_named() {
    let env = setup().await;
    let file = env.plant("config/app/.env", &format!("API_TOKEN={CANARY}\nSOME_SETTING={CANARY_FILE}\nGONE=1\n"));
    let id = env.job(json!({"source": format!("env:{}", file.display()), "project": "apis", "environment": "dev", "keys": ["SOME_SETTING", "NOT_THERE"]}));
    let j = env.run(&id).await;
    assert_eq!(env.deposits(), vec![("secret://apis/dev#SOME_SETTING".to_owned(), CANARY_FILE.to_owned())]);
    assert_eq!(Env::move_state(&j, "API_TOKEN"), "left");
    assert_eq!(Env::move_state(&j, "NOT_THERE"), "not_in_file");
}

#[tokio::test]
async fn hostile_paths_are_refused_before_a_job_exists() {
    let env = setup().await;
    let home = env.side();
    env.plant(".ssh/id_ed25519", CANARY);
    for bad in [
        "file:~/.ssh/id_ed25519".to_owned(),
        format!("file:{}/.ssh/id_ed25519", home.display()),
        format!("file:{}/config/app/../../.ssh/id_ed25519", home.display()),
        "file:~/config/../.ssh/id_ed25519".to_owned(),
        "file:~//.ssh/id_ed25519".to_owned(),
        "file:~root/.ssh/id_ed25519".to_owned(),
        "file:/etc/shadow".to_owned(),
        "file:/proc/self/environ".to_owned(),
        "env:/etc/infisical-broker/broker.toml".to_owned(),
        format!("file:{}/config/google-chrome/Default/Login Data", home.display()),
        format!("file:{}/config/infisical-mcp/owner.token", home.display()),
        format!("file:{}/projects/app/.aws/credentials", home.display()),
        format!("file:{}/config/.zcode/config.json", home.display()),
        format!("file:{}/config/keeper-tokens/app/x", home.display()),
        format!("file:{}/config/key-directly-in-the-root", home.display()),
        format!("env:{}/outside/app/.env", home.display()),
        "file:relative/app/.env".to_owned(),
        format!("file:{}/projects/app/GOOGLE_PASSWORD", home.display()),
    ] {
        let e = env.keeper().enqueue(&json!({"source": bad, "name": "KEEP_X"})).unwrap_err();
        assert_eq!(e.code, "path_refused", "{bad}");
        assert!(!e.message.contains(".ssh") && !e.message.contains("shadow"), "the same words for every reason");
    }
    assert!(!env.keeper().run_next(&env.h.state).await, "nothing queued");
    assert!(env.deposits().is_empty());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_link_out_of_a_root_is_refused_when_it_is_read() {
    let env = setup().await;
    let side = env.side();
    let secret = env.plant("outside/secret/key", CANARY);
    let ssh = env.plant(".ssh/id_ed25519", CANARY);
    std::fs::create_dir_all(side.join("config/app")).unwrap();
    std::os::unix::fs::symlink(&secret, side.join("config/app/link")).unwrap();
    std::os::unix::fs::symlink(&ssh, side.join("config/app/looks-fine")).unwrap();
    std::os::unix::fs::symlink(side.join("outside"), side.join("config/folder-link")).unwrap();
    std::os::unix::fs::symlink("../../.ssh/id_ed25519", side.join("config/app/relative-link")).unwrap();
    for (path, name) in [
        ("config/app/link", "KEEP_A"),
        ("config/app/looks-fine", "KEEP_B"),
        ("config/folder-link/secret/key", "KEEP_C"),
        ("config/app/relative-link", "KEEP_D"),
    ] {
        let id = env.job(json!({"source": format!("file:{}", side.join(path).display()), "name": name}));
        let j = env.run(&id).await;
        assert_eq!(j.state, State::Failed, "{path}");
        assert_eq!(j.code.as_deref(), Some("path_refused"), "{path}");
    }
    assert!(env.deposits().is_empty());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_link_that_stays_inside_works() {
    let env = setup().await;
    let side = env.side();
    let real = env.plant("config/app/real", &format!("{CANARY}\n"));
    std::os::unix::fs::symlink(&real, side.join("config/app/link")).unwrap();
    let id = env.job(json!({"source": format!("file:{}", side.join("config/app/link").display()), "name": "KEEP_A"}));
    assert_eq!(env.run(&id).await.state, State::Saved);
}

#[tokio::test]
async fn a_hard_link_binary_big_or_strange_file_is_refused() {
    let env = setup().await;
    let side = env.side();
    let secret = env.plant("outside/key", CANARY);
    std::fs::create_dir_all(side.join("config/app")).unwrap();
    std::fs::hard_link(&secret, side.join("config/app/hard")).unwrap();
    let binary = env.plant("config/app/binary", "x");
    std::fs::write(&binary, [0x41u8, 0, 0x42]).unwrap();
    let not_utf8 = env.plant("config/app/latin1", "x");
    std::fs::write(&not_utf8, [0x41u8, 0xff, 0xfe, 0x42]).unwrap();
    let big = env.plant("config/app/big", "x");
    std::fs::write(&big, "y".repeat(65_537)).unwrap();
    let empty = env.plant("config/app/empty", "");
    let fifo = side.join("config/app/pipe");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    std::fs::create_dir_all(side.join("config/app/folder")).unwrap();
    for (p, code) in [
        (side.join("config/app/hard"), "hard_linked"),
        (binary, "binary"),
        (not_utf8, "binary"),
        (big, "too_long"),
        (empty, "not_one_value"),
        (fifo, "not_a_file"),
        (side.join("config/app/folder"), "not_a_file"),
        (side.join("config/app/not-there"), "source_not_found"),
    ] {
        let id = env.job(json!({"source": format!("file:{}", p.display()), "name": "KEEP_A"}));
        let j = tokio::time::timeout(Duration::from_secs(10), env.run(&id)).await.expect("a pipe must not block the worker");
        assert_eq!(j.state, State::Failed, "{}", p.display());
        assert_eq!(j.code.as_deref(), Some(code), "{}", p.display());
    }
    assert!(env.deposits().is_empty());
}

#[tokio::test]
async fn the_google_password_is_never_moved_by_any_route() {
    let env = setup().await;
    let file = env.plant("config/app/.env", &format!("GOOGLE_PASSWORD={CANARY_GOOGLE}\nAPI_TOKEN={CANARY}\n"));
    let other = env.plant("config/app/key", CANARY_GOOGLE);
    for name in ["GOOGLE_PASSWORD", "KEEP_GOOGLE_PASSWORD", "keep-google-password"] {
        let e = env.keeper().enqueue(&json!({"source": format!("file:{}", other.display()), "name": name})).unwrap_err();
        assert_eq!(e.code, "refused_by_rule", "{name}");
        let e = env.keeper().enqueue(&json!({"source": "note:telegram", "name": name})).unwrap_err();
        assert_eq!(e.code, "refused_by_rule", "{name}");
    }
    // Asked for by name.
    let id = env.job(json!({"source": format!("env:{}", file.display()), "project": "apis", "environment": "dev", "keys": ["GOOGLE_PASSWORD"]}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Failed);
    assert_eq!(Env::move_state(&j, "GOOGLE_PASSWORD"), "refused_by_rule");
    // Or through a prefix that makes the stored name look like it.
    let e = env.keeper().enqueue(&json!({"source": format!("env:{}", file.display()), "project": "apis", "environment": "dev", "keys": ["API_TOKEN"], "prefix": "GOOGLE_PASSWORD_"})).unwrap_err();
    assert_eq!(e.code, "refused_by_rule");
    assert!(env.deposits().is_empty(), "{:?}", env.deposits().iter().map(|d| d.0.clone()).collect::<Vec<_>>());
    let (_, listed) = (0, env.keeper().project(&json!({"dir": file.parent().unwrap().display().to_string(), "deposit": true}), "", true).unwrap());
    let row = listed["not_in_a_list"].as_array().unwrap();
    assert!(row.iter().all(|r| r["name"] != "GOOGLE_PASSWORD"), "{listed}");
    env.keeper().make_all_due_for_test();
    while env.keeper().run_next(&env.h.state).await {}
    assert!(env.deposits().iter().all(|(r, v)| !r.contains("GOOGLE") && v != CANARY_GOOGLE));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_file_with_more_in_it_goes_through_the_local_model_or_not_at_all() {
    let env = setup().await;
    let file = env.plant("config/app/settings.yml", &format!("user: me\ntoken: {CANARY}\n"));
    // No words about which secret: nothing guesses.
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A"}));
    let j = env.run(&id).await;
    assert_eq!(j.code.as_deref(), Some("not_one_value"));
    assert!(env.deposits().is_empty());
    // The local model picks; what it picks must stand in the file.
    qwen_says(&env.qwen, json!({"found": true, "value": "something else entirely"})).await;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "what": "the token"}));
    assert_eq!(env.run(&id).await.code.as_deref(), Some("not_in_source"));
    qwen_says(&env.qwen, json!({"found": true, "value": CANARY})).await;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "what": "the token"}));
    assert_eq!(env.run(&id).await.state, State::Saved);
    assert!(env.broker_saw("secret://personal/prod#KEEP_A"));
    // Local model gone: the job waits, nothing else is tried.
    env.qwen.reset().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(503)).mount(&env.qwen).await;
    let before = env.deposits().len();
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_B", "what": "the token"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Queued);
    assert_eq!(j.code.as_deref(), Some("local_model_unavailable"));
    assert_eq!(env.deposits().len(), before);
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn clear_removes_the_file_only_after_the_read_back_and_only_when_asked() {
    let env = setup().await;
    let file = env.plant("config/app/key", &format!("{CANARY}\n"));
    // A wrong read-back: nothing happens to the file.
    *env.broker.corrupt.lock().unwrap() = true;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let j = env.run(&id).await;
    assert_eq!(j.code.as_deref(), Some("readback_mismatch"));
    assert!(file.exists(), "kept after a bad read-back");
    *env.broker.corrupt.lock().unwrap() = false;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let c = j.cleared.clone().unwrap();
    assert_eq!(c.clear_state.as_deref(), Some("cleared"), "{c:?}");
    assert!(c.source);
    assert!(!file.exists(), "removed");
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn another_value_under_the_name_is_never_overwritten_and_never_cleared() {
    let env = setup().await;
    env.broker.secrets.lock().unwrap().push(("secret://personal/prod#KEEP_A".to_owned(), "the-value-already-there".to_owned()));
    let file = env.plant("config/app/key", &format!("{CANARY}\n"));
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let j = env.run(&id).await;
    assert_eq!(j.code.as_deref(), Some("exists"));
    assert!(file.exists());
    assert_eq!(env.broker.secrets.lock().unwrap()[0].1, "the-value-already-there");
    let envf = env.plant("config/app/.env", &format!("KEEP_A={CANARY}\nKEEP_B={CANARY_FILE}\n"));
    let id = env.job(json!({"source": format!("env:{}", envf.display()), "keys": ["KEEP_A", "KEEP_B"], "prefix": "KEEP_", "project": "personal", "environment": "prod", "clear": true}));
    let j = env.run(&id).await;
    assert_eq!(Env::move_state(&j, "KEEP_A"), "exists");
    assert_eq!(Env::move_state(&j, "KEEP_B"), "saved");
    let left = std::fs::read_to_string(&envf).unwrap();
    assert!(left.contains(&format!("KEEP_A={CANARY}")), "the key that wasn't saved stays");
    assert!(!left.contains(CANARY_FILE), "the saved one left");
    assert!(left.contains("# KEEP_B moved to Infisical: secret://personal/prod#KEEP_B"), "{}", left.replace(CANARY, "[c]"));
}

#[tokio::test]
async fn clearing_an_env_file_keeps_its_mode_and_every_other_line() {
    use std::os::unix::fs::PermissionsExt;
    let env = setup().await;
    for mode in [0o600u32, 0o640] {
        let dir = format!("config/app{mode:o}");
        let file = env.plant(&format!("{dir}/.env"), &format!("# top\nPORT=8080\nAPI_TOKEN=\"{CANARY}\"\n\nexport HOST=local # note\n"));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        let id = env.job(json!({"source": format!("env:{}", file.display()), "project": "apis", "environment": "dev", "clear": true}));
        let j = env.run(&id).await;
        assert_eq!(j.cleared.clone().unwrap().clear_state.as_deref(), Some("cleared"), "{:?}", j.cleared);
        assert_eq!(mode_of(&file), mode, "the mode stays");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "# top\nPORT=8080\n# API_TOKEN moved to Infisical: secret://apis/dev#API_TOKEN\n\nexport HOST=local # note\n"
        );
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temp file left: {leftovers:?}");
    }
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_file_in_use_is_not_cleared_and_the_receipt_says_what_uses_it() {
    let env = setup().await;
    let file = env.plant("config/held/key", &format!("{CANARY}\n"));
    // Something has it open.
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("exec 3<'{}'; exec sleep 60", file.display()))
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let j = env.run(&id).await;
    let c = j.cleared.clone().unwrap();
    assert_eq!(c.clear_state.as_deref(), Some("in_use"), "{c:?}");
    assert!(c.users.iter().any(|u| u.contains("has it open")), "{:?}", c.users);
    assert!(file.exists());
    child.kill().unwrap();
    child.wait().unwrap();

    // A unit file names it the way units do: %h, and its folder.
    let unit = env.side().join("units/app.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, "[Service]\nEnvironmentFile=%h/config/held/key\n").unwrap();
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let c = env.run(&id).await.cleared.unwrap();
    assert_eq!(c.clear_state.as_deref(), Some("in_use"), "{c:?}");
    assert!(c.users.iter().any(|u| u.contains("app.service")), "{:?}", c.users);
    std::fs::write(&unit, "[Service]\nWorkingDirectory=%h/config/held\n").unwrap();
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let c = env.run(&id).await.cleared.unwrap();
    assert!(c.users.iter().any(|u| u.contains("its folder")), "{:?}", c.users);
    assert!(file.exists());

    // A process that runs from the folder, and one that names it in its arguments.
    std::fs::write(&unit, "[Service]\nExecStart=/bin/true\n").unwrap();
    let mut cwd = std::process::Command::new("sleep").arg("60").current_dir(file.parent().unwrap()).spawn().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let c = env.run(&id).await.cleared.unwrap();
    assert!(c.users.iter().any(|u| u.contains("runs in its folder")), "{:?}", c.users);
    cwd.kill().unwrap();
    cwd.wait().unwrap();
    let mut args = std::process::Command::new("sh").arg("-c").arg("sleep 30; echo \"$0\"").arg(&file).spawn().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let c = env.run(&id).await.cleared.unwrap();
    assert!(c.users.iter().any(|u| u.contains("cmdline")), "{:?}", c.users);
    assert!(file.exists());
    args.kill().unwrap();
    args.wait().unwrap();

    // The request's own command line names the path, and that is not a user.
    let mut request = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 30; : lucina-rs secret-keep save --source file:{} --clear", file.display()))
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Nothing left: it clears, and with no clear asked the users are still reported.
    let id = env.job(json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true}));
    let c = env.run(&id).await.cleared.unwrap();
    request.kill().unwrap();
    request.wait().unwrap();
    assert_eq!(c.clear_state.as_deref(), Some("cleared"), "{c:?}");
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_clear_from_chat_waits_for_his_answer() {
    use lucina_tools::tools::secret_keep::SecretKeeper;
    let env = setup().await;
    let file = env.plant("config/app/key", &format!("{CANARY}\n"));
    let out = SecretKeeper::keep(env.keeper(), "test", json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true})).await.unwrap();
    let id = out["job"].as_str().unwrap().to_owned();
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    assert!(file.exists(), "saved, not cleared");
    let j = env.keeper().job(&id).unwrap();
    assert_eq!(j.cleared.clone().unwrap().clear_state.as_deref(), Some("waiting_owner"));
    let d = j.decision.clone().expect("a question was asked");
    let asked = lucina_server::decide::get(&env.h.state, &d).unwrap();
    assert_eq!(asked["status"], "pending", "only he can answer this: {asked}");
    assert_clean("the question", &asked.to_string());
    // Nothing happens while it waits.
    env.keeper().settle_clears(&env.h.state).await;
    assert!(file.exists());
    // He says no.
    lucina_server::decide::owner_answer(&env.h.state, &format!("decision {d}: 2")).await.unwrap();
    env.keeper().settle_clears(&env.h.state).await;
    let j = env.keeper().job(&id).unwrap();
    assert_eq!(j.cleared.clone().unwrap().clear_state.as_deref(), Some("left_by_owner"));
    assert!(file.exists());

    // Again, and he says yes.
    let out = SecretKeeper::keep(env.keeper(), "test", json!({"source": format!("file:{}", file.display()), "name": "KEEP_A", "clear": true})).await.unwrap();
    let id = out["job"].as_str().unwrap().to_owned();
    env.run(&id).await;
    let d = env.keeper().job(&id).unwrap().decision.unwrap();
    lucina_server::decide::owner_answer(&env.h.state, &format!("decision {d}: 1")).await.unwrap();
    env.keeper().settle_clears(&env.h.state).await;
    let j = env.keeper().job(&id).unwrap();
    assert_eq!(j.cleared.clone().unwrap().clear_state.as_deref(), Some("cleared"), "{:?}", j.cleared);
    assert!(!file.exists());

    // The file changed while he decided: Infisical holds the old value, so it stays.
    let file = env.plant("config/app/key2", &format!("{CANARY}\n"));
    let out = SecretKeeper::keep(env.keeper(), "test", json!({"source": format!("file:{}", file.display()), "name": "KEEP_B", "clear": true})).await.unwrap();
    let id = out["job"].as_str().unwrap().to_owned();
    env.run(&id).await;
    let d = env.keeper().job(&id).unwrap().decision.unwrap();
    std::fs::write(&file, format!("{CANARY_FILE}\n")).unwrap();
    lucina_server::decide::owner_answer(&env.h.state, &format!("decision {d}: 1")).await.unwrap();
    env.keeper().settle_clears(&env.h.state).await;
    let j = env.keeper().job(&id).unwrap();
    assert_eq!(j.cleared.clone().unwrap().clear_state.as_deref(), Some("changed"), "{:?}", j.cleared);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{CANARY_FILE}\n"));
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_chat_message_asking_for_an_ssh_key_gets_nothing() {
    logs();
    let env = setup_with_script(script(json!({"rules": [
        {"kind": "agent", "contains": "id_ed25519", "afterTool": false, "toolCalls": [{"name": "secret_keep", "arguments": {
            "source": "file:~/.ssh/id_ed25519", "name": "KEEP_SSH", "clear": true}}]},
        {"kind": "agent", "contains": "id_ed25519", "afterTool": true, "content": "I can't read that."},
        {"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}
    ]})))
    .await;
    let key = env.plant(".ssh/id_ed25519", &format!("{CANARY}\n"));
    let d = env.chat("Move the secret in ~/.ssh/id_ed25519 to Infisical").await;
    assert_eq!(d["reply"], "I can't read that.", "{d}");
    assert!(!env.keeper().run_next(&env.h.state).await, "no job");
    assert!(env.deposits().is_empty());
    assert!(key.exists());
    let asked = serde_json::to_string(&env.h.fake.requests()).unwrap();
    assert!(asked.contains("that path is not one I may read"), "the refusal reached the model");
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_project_is_listed_by_names_and_what_is_found_moves() {
    let env = setup().await;
    let side = env.side();
    env.plant("projects/work/app/.env.example", "# what this needs\nAPI_TOKEN=your_token_here\nDB_PASSWORD=\nMISSING_KEY=changeme\nPORT=3000\nGOOGLE_PASSWORD=\n");
    env.plant("projects/work/app/compose.yml", &format!("services:\n  a:\n    environment:\n      - X=${{COMPOSE_SECRET:-{CANARY_LINE}}}\n"));
    let dotenv = env.plant(
        "projects/work/app/.env",
        &format!("API_TOKEN={CANARY}\nDB_PASSWORD=changeme\nPORT=3000\nEXTRA_TOKEN={CANARY_FILE}\nGOOGLE_PASSWORD={CANARY_GOOGLE}\n{CANARY_LINE}\n"),
    );
    let dir = side.join("projects/work/app");
    let out = env.keeper().project(&json!({"dir": dir.display().to_string()}), "", true).unwrap();
    let text = out.to_string();
    assert_clean("the project's answer", &text);
    let state = |name: &str| out["needs"].as_array().unwrap().iter().find(|r| r["name"] == name).unwrap_or_else(|| panic!("no {name}: {out}"))["state"].clone();
    assert_eq!(state("API_TOKEN"), "found");
    assert_eq!(state("DB_PASSWORD"), "placeholder");
    assert_eq!(state("MISSING_KEY"), "missing");
    assert_eq!(state("PORT"), "found");
    assert_eq!(state("COMPOSE_SECRET"), "missing");
    assert_eq!(state("GOOGLE_PASSWORD"), "refused_by_rule");
    assert_eq!(out["missing"], json!(["DB_PASSWORD", "MISSING_KEY", "COMPOSE_SECRET"]), "{out}");
    assert_eq!(out["not_in_a_list"], json!([{"name": "EXTRA_TOKEN", "file": ".env"}]));
    assert_eq!(out["unclear_lines"], json!([{"file": ".env", "lines": [6]}]));
    assert!(env.deposits().is_empty(), "listing moves nothing");

    let out = env.keeper().project(&json!({"dir": dir.display().to_string(), "deposit": true}), "", true).unwrap();
    assert_clean("the project's answer", &out.to_string());
    let job = out["jobs"][0]["job"].as_str().unwrap().to_owned();
    let j = env.run(&job).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let mut refs: Vec<String> = env.deposits().into_iter().map(|(r, _)| r).collect();
    refs.sort();
    assert_eq!(refs, vec!["secret://personal/prod#KEEP_APP_API_TOKEN", "secret://personal/prod#KEEP_APP_EXTRA_TOKEN"]);
    assert!(dotenv.exists());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_project_folder_out_of_bounds_is_refused() {
    let env = setup().await;
    let home = env.side();
    env.plant(".ssh/.env", CANARY);
    for dir in [
        "~/.ssh".to_owned(),
        format!("{}/.ssh", home.display()),
        format!("{}/config/../.ssh", home.display()),
        "/etc".to_owned(),
        format!("{}/projects", home.display()),
    ] {
        let e = env.keeper().project(&json!({"dir": dir}), "", true).unwrap_err();
        assert_eq!(e.code, "path_refused", "{dir}");
    }
    // A link in a project, to a value outside it: reported by name of the file, never read.
    let side = env.side();
    let outside = env.plant("outside/x", &format!("API_TOKEN={CANARY}\n"));
    std::fs::create_dir_all(side.join("projects/work/app")).unwrap();
    std::os::unix::fs::symlink(&outside, side.join("projects/work/app/.env")).unwrap();
    let out = env.keeper().project(&json!({"dir": side.join("projects/work/app").display().to_string()}), "", true).unwrap();
    assert_eq!(out["files_refused"], json!([{"file": ".env", "refused": "path_refused"}]), "{out}");
    assert!(!out.to_string().contains("API_TOKEN"));
}

#[tokio::test]
async fn the_tool_answers_with_names_and_only_for_his_own_message() {
    use lucina_tools::tools::secret_keep::{SecretKeepTool, SecretProjectTool};
    use lucina_tools::{Ctx, Origin, Tool};
    let env = setup().await;
    let file = env.plant("config/app/key", &format!("{CANARY}\n"));
    let keeper: Arc<dyn lucina_tools::tools::secret_keep::SecretKeeper> = env.h.state.secret_keep.clone();
    let tool = SecretKeepTool { keeper: keeper.clone() };
    let project = SecretProjectTool { keeper };
    let args = json!({"source": format!("file:{}", file.display()), "name": "APP_KEY"});
    let dir = json!({"dir": file.parent().unwrap().display().to_string()});
    // Her own initiative and another assistant's turn: not him.
    let hers = Ctx::new(env.h.state.tool_env.clone(), "web", "test");
    assert!(tool.call(args.clone(), &hers).await.is_err());
    assert!(project.call(dir.clone(), &hers).await.is_err());
    let mcp = Ctx::new(env.h.state.tool_env.clone(), "web", "test").owner_turn().with_origin(Origin::Mcp);
    assert!(tool.call(args.clone(), &mcp).await.is_err());
    assert!(project.call(dir.clone(), &mcp).await.is_err());
    assert!(env.keeper().job("nope").is_none());
    // His.
    let his = Ctx::new(env.h.state.tool_env.clone(), "web", "test").owner_turn();
    let out = tool.call(args, &his).await.unwrap();
    assert_clean("the tool's answer", &out.to_string());
    assert_eq!(out["name"], "KEPT_APP_KEY");
    let id = out["job"].as_str().unwrap().to_owned();
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let out = tool.call(json!({"job": id}), &his).await.unwrap();
    assert_clean("the tool's status", &out.to_string());
    env.assert_no_value_anywhere().await;
}

/// Juno, the owner's laptop: polls once and answers `secret_source` with `text`.
fn juno_laptop(env: &Env, text: String) -> tokio::task::JoinHandle<Value> {
    let b = env.h.state.juno.clone();
    tokio::spawn(async move {
        let call = b
            .next("juno1", "Juno (Galaxy Book Go)", Duration::from_secs(5))
            .await
            .expect("a call for the laptop");
        let id = call["id"].as_str().unwrap().to_owned();
        assert!(b.result("juno1", &id, Ok(json!({ "text": text }))));
        call
    })
}

#[tokio::test]
async fn a_juno_file_comes_through_the_bridge_as_an_internal_call() {
    let env = setup().await;
    let laptop = juno_laptop(&env, format!("{CANARY}\n"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let id = env.job(json!({"source": "juno:/data/home/.config/app/key", "name": "KEEP_JUNO_APP"}));
    let j = env.run(&id).await;
    assert_eq!(j.state, State::Saved, "{:?}", j.code);
    let call = laptop.await.unwrap();
    assert_eq!(call["tool"], "secret_source");
    assert_eq!(call["internal"], true);
    assert_eq!(call["arguments"], json!({"path": "/data/home/.config/app/key"}));
    assert_eq!(
        env.deposits(),
        vec![("secret://personal/prod#KEEP_JUNO_APP".to_owned(), CANARY.to_owned())]
    );
    assert!(env.qwen.received_requests().await.unwrap().is_empty(), "one value: no model read it");
    assert!(j.cleared.clone().unwrap().remains.iter().any(|r| r.contains("stays on Juno")));
    assert_clean("the job's outcome", &j.outcome().to_string());
    env.assert_no_value_anywhere().await;
}

#[tokio::test]
async fn a_juno_source_with_juno_offline_waits() {
    let env = setup().await;
    let id = env.job(json!({"source": "juno:/data/home/key", "name": "KEEP_JUNO_OFF"}));
    let j = env.run(&id).await;
    assert_eq!(j.code.as_deref(), Some("juno_offline"), "{:?}", j.state);
    assert!(env.deposits().is_empty());
}

#[tokio::test]
async fn juno_secretary_paths_are_refused_before_a_job_exists() {
    let env = setup().await;
    for source in [
        "juno:/data/lucina/secretary",
        "juno:/data/lucina/secretary/device.token",
        "juno:/run/lucina/secretary/session",
        "juno:/data/home/../lucina/secretary/x",
        "juno:/data/home/./x",
        "juno:relative/key",
        "juno:",
    ] {
        assert!(
            env.keeper().enqueue(&json!({"source": source, "name": "KEEP_X"})).is_err(),
            "{source}"
        );
    }
    assert!(env
        .keeper()
        .enqueue(&json!({"source": "juno:/data/lucina/secretary-notes/x", "name": "KEEP_X"}))
        .is_ok());
}
