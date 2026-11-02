//! Real multipart, disk manifests, planner turns, and timeline; external
//! services are local axum servers and the existing fake LLM.
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::State,
    routing::{get, post, put},
    Json, Router,
};
use common::{harness, script, H};
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};

#[derive(Default)]
struct Seen {
    media: Mutex<Vec<Value>>,
    stored: Mutex<Option<std::path::PathBuf>>,
    vision: Mutex<Vec<Value>>,
    pause_vision: std::sync::atomic::AtomicBool,
    resume_vision: tokio::sync::Notify,
}
struct Services {
    base: String,
    seen: Arc<Seen>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Services {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn services() -> Services {
    async fn media(State(s): State<Arc<Seen>>, Json(v): Json<Value>) -> Json<Value> {
        s.media.lock().unwrap().push(v);
        Json(json!({"item_id":"itm_test", "duplicate":false, "version":1}))
    }
    /// lucina-media's ingest: "moves" the file into a store dir next to it.
    async fn ingest(State(s): State<Arc<Seen>>, Json(v): Json<Value>) -> Json<Value> {
        let src = std::path::PathBuf::from(v["path"].as_str().unwrap());
        let store = src
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("store");
        std::fs::create_dir_all(&store).unwrap();
        let dst = store.join("blob-itm_test");
        std::fs::rename(&src, &dst).unwrap();
        *s.stored.lock().unwrap() = Some(dst);
        s.media.lock().unwrap().push(v);
        Json(json!({"item_id":"itm_test", "duplicate":false, "version":1, "kind":"photo"}))
    }
    async fn item_path(State(s): State<Arc<Seen>>) -> Json<Value> {
        Json(json!({"path": s.stored.lock().unwrap().clone(), "mime":"image/png", "kind":"photo"}))
    }
    async fn vision(State(s): State<Arc<Seen>>, Json(v): Json<Value>) -> Json<Value> {
        s.vision.lock().unwrap().push(v);
        if s.pause_vision.load(std::sync::atomic::Ordering::SeqCst) {
            s.resume_vision.notified().await;
        }
        Json(
            json!({"choices":[{"message":{"content":"", "reasoning_content":"A bright green square. VISION_EXTRACTED"}}]}),
        )
    }
    let seen = Arc::new(Seen::default());
    let router = Router::new()
        .route("/page", get(|| async { axum::response::Html("<html><head><title>Test page title</title></head><body><nav>JUNK_NAV</nav><p>URL_EXTRACTED readable article.</p><script>JUNK_SCRIPT</script></body></html>") }))
        .route("/redirect-private", get(|| async { axum::response::Redirect::temporary("http://169.254.169.254/latest/meta-data/") }))
        .route("/v1/chat/completions", post(vision))
        .route("/internal/v1/external/{source}/{id}", put(media))
        .route("/internal/v1/ingest", post(ingest))
        .route("/internal/v1/items/{id}/path", get(item_path))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Services { base, seen, task }
}
async fn setup(services: &Services) -> H {
    harness(script(json!({"rules":[
        {"kind":"intent", "content":"{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind":"agent", "content":"I read your shared text, article and image."}
    ]})), |c| {
        c.share.dir = c.data_dir.join("shares");
        c.share.key_file = Some(c.data_dir.join("share.key"));
        std::fs::write(c.share.key_file.as_ref().unwrap(), "  gateway-test-secret\n").unwrap();
        c.share.media_url = services.base.clone();
        c.share.vision_url = Some(services.base.clone());
        c.share.test_allow_loopback = true;
    }).await
}
async fn submit(h: &H, form: Form) -> reqwest::Response {
    h.http
        .post(format!("{}/v1/share", h.base))
        .header("x-share-gateway", "gateway-test-secret")
        .multipart(form)
        .send()
        .await
        .unwrap()
}
async fn finished(h: &H, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let r = h
                .http
                .get(format!("{}/v1/share/{id}", h.base))
                .header("x-share-gateway", "gateway-test-secret")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            let v: Value = r.json().await.unwrap();
            if v["status"] != "processing" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("share must finish")
}
#[tokio::test]
async fn multipart_share_reads_everything_and_deduplicates_across_restart() {
    let services = services().await;
    let h = setup(&services).await;
    services
        .seen
        .pause_vision
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // Generate a real PNG with the same installed conversion tool used by ingestion.
    let png = h.dir.path().join("green.png");
    assert!(tokio::process::Command::new("convert")
        .args(["-size", "16x16", "xc:green"])
        .arg(&png)
        .status()
        .await
        .unwrap()
        .success());
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-integration-123")
            .text("channel", "ios")
            .text("note", "Please read these")
            .text("source", "ios-share")
            .text("text", "TEXT_EXTRACTED my short snippet")
            .text("url", format!("{}/page", services.base))
            .part(
                "file",
                Part::bytes(std::fs::read(&png).unwrap())
                    .file_name("../../green.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    let ack: Value = r.json().await.unwrap();
    assert_eq!(ack["status"], "processing");
    assert_eq!(ack["duplicate"], false);
    assert_eq!(ack["stage"], "received");
    let attachments = ack["attachments"].as_array().unwrap();
    // Every part, with its index and reading state (the short text is also in the item text).
    assert_eq!(attachments.len(), 3);
    assert!(attachments
        .iter()
        .enumerate()
        .all(|(i, a)| a["index"] == i && a["state"] == "pending"));
    assert!(attachments.iter().any(|a| a["kind"] == "image"
        && a["mediaId"] == "itm_test"
        && a["mime"] == "image/png"
        && a["bytes"].as_u64().unwrap() > 0));
    let retry = submit(
        &h,
        Form::new()
            .text("shareId", "share-integration-123")
            .text("text", "retry during processing"),
    )
    .await;
    assert_eq!(retry.status(), 202);
    let retry: Value = retry.json().await.unwrap();
    assert_eq!(retry["duplicate"], true);
    assert_eq!(retry["status"], "processing");
    assert_eq!(retry["userItemId"], ack["userItemId"]);
    services.seen.resume_vision.notify_one();
    let done = finished(&h, "share-integration-123").await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["stage"], "done");
    assert!(
        done["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["state"] == "read"),
        "{done}"
    );
    assert_eq!(done["userItemId"], ack["userItemId"]);
    assert_eq!(done["reply"], "I read your shared text, article and image.");
    assert!(done["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "Test page title"));
    let lines = h.timeline_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(lines[0]["channel"], "ios");
    assert_eq!(
        lines[0]["meta"],
        json!({"kind":"share", "shareId":"share-integration-123", "intent":"ask"})
    );
    assert!(lines[0]["text"]
        .as_str()
        .unwrap()
        .contains("Please read these"));
    assert!(lines[0]["text"]
        .as_str()
        .unwrap()
        .contains("TEXT_EXTRACTED"));
    assert!(!lines[0]["text"].as_str().unwrap().contains("URL_EXTRACTED"));
    assert_eq!(lines[1]["replyTo"], ack["userItemId"]);
    // The timeline item keeps what was known at once: the library id, the link's host.
    let item_attachments = lines[0]["attachments"].as_array().unwrap();
    assert_eq!(item_attachments.len(), 2);
    assert!(item_attachments
        .iter()
        .any(|a| a["kind"] == "image" && a["mediaId"] == "itm_test"));
    assert!(item_attachments
        .iter()
        .any(|a| a["kind"] == "link" && a["name"] == "127.0.0.1"));
    let requests = h.fake.requests();
    let turn = requests.iter().find(|r| r["tools"].is_array()).unwrap();
    let message = turn["messages"].to_string();
    for marker in [
        "TEXT_EXTRACTED",
        "URL_EXTRACTED",
        "VISION_EXTRACTED",
        "<Shared content>",
        "Owner's question or note: \\\"Please read these\\\"",
        "How to handle it: Image",
        "How to handle it: Web page",
    ] {
        assert!(message.contains(marker), "{marker}: {message}");
    }
    for marker in ["JUNK_NAV", "JUNK_SCRIPT"] {
        assert!(!message.contains(marker));
    }
    assert_eq!(services.seen.vision.lock().unwrap().len(), 1);
    assert_eq!(
        services.seen.vision.lock().unwrap()[0]["chat_template_kwargs"]["enable_thinking"],
        false
    );
    let media = services.seen.media.lock().unwrap().clone();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0]["album"], "Shared");
    assert_eq!(media[0]["caption"], "Please read these");
    assert_eq!(media[0]["move"], true);
    assert!(std::path::Path::new(media[0]["path"].as_str().unwrap()).is_absolute());
    // Moved into the library; vision read it there through a link, and no work
    // file was written next to the stored original.
    let stored = services.seen.stored.lock().unwrap().clone().unwrap();
    assert!(stored.exists());
    assert_eq!(
        std::fs::read_dir(stored.parent().unwrap()).unwrap().count(),
        1
    );
    // What she read is in the attachment memory.
    let memory = &h.state.share.memory;
    let hits = memory.search("readable article", None, None, 5).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].kind, "link");
    assert_eq!(hits[0].name, "Test page title");
    assert_eq!(memory.recent(0, 10).unwrap().len(), 3);
    let status_file = h
        .state
        .cfg
        .share
        .dir
        .join(chrono::Utc::now().format("%Y-%m").to_string())
        .join("share-integration-123/status.json");
    let mut persisted: Value =
        serde_json::from_slice(&std::fs::read(&status_file).unwrap()).unwrap();
    persisted["status"] = json!("processing");
    std::fs::write(&status_file, serde_json::to_vec(&persisted).unwrap()).unwrap();
    // Build a fresh process state against the same persisted manifests.
    let restarted = lucina_server::AppState::build(h.state.cfg.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, lucina_server::app(restarted))
            .await
            .unwrap()
    });
    let before = h.fake.requests().len();
    assert_eq!(
        finished(&h, "share-integration-123").await["status"],
        "done"
    );
    let r = h
        .http
        .post(format!("{base}/v1/share"))
        .header("x-share-gateway", "gateway-test-secret")
        .multipart(
            Form::new()
                .text("shareId", "share-integration-123")
                .text("text", "retry"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let duplicate: Value = r.json().await.unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["userItemId"], ack["userItemId"]);
    assert_eq!(duplicate["lucinaItemId"], done["lucinaItemId"]);
    assert_eq!(h.fake.requests().len(), before);
    assert_eq!(h.timeline_lines().len(), 2);
    assert_eq!(services.seen.media.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn auth_validation_limits_and_library_without_llm() {
    let services = services().await;
    let h = setup(&services).await;
    for method in [reqwest::Method::GET, reqwest::Method::POST] {
        let endpoint = if method == reqwest::Method::POST {
            "/v1/share"
        } else {
            "/v1/share/share-auth-test"
        };
        for token in [None, Some("wrong")] {
            let mut request = h
                .http
                .request(method.clone(), format!("{}{endpoint}", h.base));
            if let Some(token) = token {
                request = request.header("x-share-gateway", token);
            }
            assert_eq!(request.send().await.unwrap().status(), 401);
        }
    }
    for (name, value) in [
        ("shareId", "../badid"),
        ("intent", "delete"),
        ("channel", "android"),
        ("sessionId", "a b"),
        ("note", &"x".repeat(4001)),
    ] {
        assert_eq!(
            submit(
                &h,
                Form::new()
                    .text("text", "input")
                    .text(name, value.to_owned())
            )
            .await
            .status(),
            400,
            "{name}"
        );
    }
    assert_eq!(
        submit(&h, Form::new().text("note", "no content"))
            .await
            .status(),
        400
    );
    assert_eq!(
        submit(
            &h,
            Form::new()
                .text("intent", "ask")
                .text("intent", "note")
                .text("text", "input")
        )
        .await
        .status(),
        400
    );
    let mut form = Form::new();
    for _ in 0..33 {
        form = form.text("text", "hello");
    }
    assert_eq!(submit(&h, form).await.status(), 400);
    // Known Content-Length is rejected before attempting to read its body.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = h.base.strip_prefix("http://").unwrap();
    let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    conn.write_all(format!("POST /v1/share HTTP/1.1\r\nHost: {addr}\r\nX-Share-Gateway: gateway-test-secret\r\nContent-Type: multipart/form-data; boundary=a\r\nContent-Length: 2147483649\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(2), conn.read(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&response[..n]).starts_with("HTTP/1.1 413"));
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "library-test-123")
            .text("intent", "library")
            .text("lang", "it")
            .text("text", "A saved library note"),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finished(&h, "library-test-123").await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["stage"], "done");
    assert!(
        done["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["state"] == "read"),
        "{done}"
    );
    // `lang=it` is the device locale and is ignored; with no note and no recent messages: English.
    assert_eq!(done["reply"], "Saved to the Shared library.");
    assert!(
        h.fake.requests().is_empty(),
        "library must not call the chat model"
    );
    assert!(h
        .state
        .cfg
        .data_dir
        .join("notes/a-saved-library-note.md")
        .exists());
    let off = harness(script(json!({})), |c| {
        c.share.dir = c.data_dir.join("shares")
    })
    .await;
    for endpoint in ["/v1/share", "/v1/share/library-test-123"] {
        assert_eq!(
            off.http
                .post(format!("{}{endpoint}", off.base))
                .send()
                .await
                .unwrap()
                .status(),
            if endpoint == "/v1/share" { 503 } else { 405 }
        );
        if endpoint != "/v1/share" {
            assert_eq!(
                off.http
                    .get(format!("{}{endpoint}", off.base))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                503
            );
        }
    }
}

#[tokio::test]
async fn redirects_recheck_addresses_and_media_outage_keeps_original() {
    let services = services().await;
    let h = setup(&services).await;
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "redirect-test-123")
            .text("url", format!("{}/redirect-private", services.base)),
    )
    .await;
    assert_eq!(r.status(), 202);
    assert_eq!(finished(&h, "redirect-test-123").await["status"], "done");
    assert!(h.fake.requests().iter().any(|r| r
        .to_string()
        .contains("URL resolves to a forbidden address")));
    let blocked = harness(script(json!({"default":{"content":"Read failed"}})), |c| {
        c.share.dir = c.data_dir.join("shares");
        c.share.key_file = Some(c.data_dir.join("share.key"));
        std::fs::write(c.share.key_file.as_ref().unwrap(), "gateway-test-secret").unwrap();
        c.share.media_url = "http://127.0.0.1:1".into();
    })
    .await;
    assert!(!blocked.state.cfg.share.test_allow_loopback);
    let r = submit(
        &blocked,
        Form::new()
            .text("shareId", "blocked-test-123")
            .text("url", format!("{}/page", services.base))
            .part(
                "file",
                Part::bytes(b"FILE_EXTRACTED".to_vec())
                    .file_name("test.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    let ack: Value = r.json().await.unwrap();
    assert!(ack["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a.get("mediaId").is_none()));
    let done = finished(&blocked, "blocked-test-123").await;
    assert_eq!(done["status"], "done");
    let all = blocked
        .fake
        .requests()
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("URL resolves to a forbidden address"));
    assert!(all.contains("FILE_EXTRACTED"));
    let files = std::fs::read_dir(&blocked.state.cfg.share.dir)
        .unwrap()
        .filter_map(Result::ok)
        // Month folders only (attachments.db sits next to them).
        .filter(|m| m.path().is_dir())
        .flat_map(|m| std::fs::read_dir(m.path()).unwrap())
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    assert!(files[0].path().join("03-test.txt").exists());
}

/// 2026-10-26/27: a Danish device locale made summaries and replies Danish for Italian notes and for bare
/// links. The reply language is the owner's note, else his recent messages, else English.
#[tokio::test]
async fn the_device_locale_never_sets_the_reply_language() {
    let services = services().await;
    let h = setup(&services).await;
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-lang-it-001")
            .text("lang", "da-DK")
            .text(
                "note",
                "[selftest] Comprare: latte, uova, pane integrale, 2 limoni",
            )
            .text("text", "Lista di prova"),
    )
    .await;
    assert_eq!(r.status(), 202);
    assert_eq!(finished(&h, "share-lang-it-001").await["status"], "done");
    let all = h
        .fake
        .requests()
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(!all.contains("Device language"), "{all}");
    assert!(!all.contains("da-DK"), "{all}");
    assert!(!all.contains("Se hvad jeg har delt"), "{all}");
    assert!(all.contains("Reply language: Italian"), "{all}");

    // A bare link: no words of his own, no recent messages, so English; never the device's Danish.
    let before = h.fake.requests().len();
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-lang-en-001")
            .text("lang", "da-DK")
            .text(
                "text",
                "https://en.wikipedia.org/wiki/The_Little_Mermaid_(statue)",
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    assert_eq!(finished(&h, "share-lang-en-001").await["status"], "done");
    let all = h.fake.requests()[before..]
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(!all.contains("Device language"), "{all}");
    assert!(all.contains("Reply language: English"), "{all}");
}

#[tokio::test]
async fn later_chat_turns_recall_earlier_attachments() {
    let services = services().await;
    let h = setup(&services).await;
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-recall-0001")
            .text("lang", "it")
            .text(
                "text",
                "Bolletta Enel: totale da pagare 84,30 euro, scadenza 15/11/2026",
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    assert_eq!(finished(&h, "share-recall-0001").await["status"], "done");
    let before = h.fake.requests().len();
    // A message about something else gets no attachment block.
    h.chat("come stai oggi?").await;
    let unrelated = h.fake.requests()[before..]
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(!unrelated.contains("Earlier attachments"));
    let before = h.fake.requests().len();
    h.chat("quanto costava la bolletta che ti ho mandato?")
        .await;
    let asked = h.fake.requests()[before..]
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(asked.contains("Earlier attachments"), "{asked}");
    assert!(asked.contains("84,30"), "{asked}");
}

#[tokio::test]
async fn recovery_finds_a_reply_far_behind_newer_items() {
    let services = services().await;
    let h = setup(&services).await;
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-window-0001")
            .text("text", "WINDOW_TEXT a short note"),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finished(&h, "share-window-0001").await;
    assert_eq!(done["status"], "done");
    // Other writers keep appending: the reply ends up far from the end.
    for i in 0..600 {
        h.state
            .timeline
            .append(lucina_store::timeline::NewItem {
                channel: "telegram".into(),
                role: "user".into(),
                text: format!("other writer {i}"),
                ..Default::default()
            })
            .unwrap();
    }
    // A crash after the reply was written but before status.json said done.
    let status_file = h
        .state
        .cfg
        .share
        .dir
        .join(chrono::Utc::now().format("%Y-%m").to_string())
        .join("share-window-0001/status.json");
    let mut persisted: Value =
        serde_json::from_slice(&std::fs::read(&status_file).unwrap()).unwrap();
    persisted["status"] = json!("processing");
    std::fs::write(&status_file, serde_json::to_vec(&persisted).unwrap()).unwrap();
    let lines_before = h.timeline_lines().len();
    let calls_before = h.fake.requests().len();
    let restarted = lucina_server::AppState::build(h.state.cfg.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, lucina_server::app(restarted))
            .await
            .unwrap()
    });
    assert_eq!(
        finished(&h, "share-window-0001").await["lucinaItemId"],
        done["lucinaItemId"]
    );
    assert_eq!(h.timeline_lines().len(), lines_before, "no second answer");
    assert_eq!(
        h.fake.requests().len(),
        calls_before,
        "no second model call"
    );
}

#[tokio::test]
async fn a_shared_location_counts_toward_the_part_limit() {
    let services = services().await;
    let h = setup(&services).await;
    let mut form = Form::new().text("shareId", "share-limit-0001");
    for i in 0..32 {
        form = form.text("text", format!("part {i}"));
    }
    let r = submit(
        &h,
        form.text("meta", r#"{"location":{"lat":55.6,"lon":12.5}}"#),
    )
    .await;
    assert_eq!(r.status(), 400);
    let mut form = Form::new().text("shareId", "share-limit-0002");
    for i in 0..30 {
        form = form.text("text", format!("part {i}"));
    }
    let r = submit(
        &h,
        form.text("meta", r#"{"location":{"lat":55.6,"lon":12.5}}"#),
    )
    .await;
    assert_eq!(r.status(), 202);
}

#[tokio::test]
async fn a_failed_append_still_processes_the_share() {
    let services = services().await;
    let h = setup(&services).await;
    // The timeline's lock sidecar can't be opened (a directory): every append fails.
    let lock = std::path::PathBuf::from(format!("{}.lock", h.state.cfg.timeline_path.display()));
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir_all(&lock).unwrap();
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-stuck-0001")
            .text("text", "STUCK_TEXT"),
    )
    .await;
    // Accepted (status.json is saved), not a 500 that leaves it processing forever.
    assert_eq!(r.status(), 202);
    let done = finished(&h, "share-stuck-0001").await;
    assert_ne!(done["status"], "processing", "{done}");
    std::fs::remove_dir(&lock).unwrap();
}

const VOICE_TEXT: &str = "ricordami di comprare il latte";

fn voice_form(id: &str, bytes: Vec<u8>, transcript: Option<&str>, caption: &str) -> Form {
    Form::new()
        .text("shareId", id.to_owned())
        .text("source", "ios-chat")
        .text("channel", "ios")
        .text("lang", "it")
        .text("note", caption.to_owned())
        .text(
            "meta",
            json!({"files":[{"index":0,"voice":true,"transcript":transcript,
            "durationMs":1000,"waveform":"AAEC/w=="}]})
            .to_string(),
        )
        .part(
            "file",
            Part::bytes(bytes)
                .file_name("note.m4a")
                .mime_str("audio/mp4")
                .unwrap(),
        )
}

async fn voice_setup(services: &Services, tools: bool, whisper_text: &str) -> H {
    voice_setup_with(services, tools, whisper_text, |_| {}).await
}

async fn voice_setup_with(
    services: &Services,
    tools: bool,
    whisper_text: &str,
    tweak: impl FnOnce(&mut lucina_server::Config),
) -> H {
    let whisper_json = json!({"result":{"language":"it"},"transcription":[
        {"offsets":{"from":0,"to":1000},"text":whisper_text}]})
    .to_string();
    harness(script(json!({"rules":[
        {"kind":"intent", "content":"{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind":"plan", "content":"{\"action\":\"reply\",\"text\":\"Va bene.\"}"},
        {"kind":"reply", "content":"Va bene."},
        {"kind":"agent", "afterTool":false, "toolCalls":[{"name":"calculator", "arguments":{"expression":"2+2"}}]},
        {"kind":"agent", "afterTool":true, "content":"Va bene."}
    ]})), |c| {
        // Force the JSON planner so its exact input is observable, even for tier-0 reminders.
        c.intent.intent_router = false;
        c.share.dir = c.data_dir.join("shares");
        c.share.key_file = Some(c.data_dir.join("share.key"));
        std::fs::write(c.share.key_file.as_ref().unwrap(), "gateway-test-secret").unwrap();
        c.share.media_url = services.base.clone();
        if tools { c.planner = lucina_server::PlannerMode::Tools; }
        // Exercise ffmpeg + lucina-memo's real Whisper invocation and JSON parser.
        let cli = c.data_dir.join("whisper-test");
        std::fs::write(&cli, format!("#!/bin/sh\necho called >> \"$0.calls\"\nwhile [ \"$#\" -gt 0 ]; do\n if [ \"$1\" = -of ]; then shift; prefix=$1; fi\n shift\ndone\ncat > \"$prefix.json\" <<'TRANSCRIPT'\n{whisper_json}\nTRANSCRIPT\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        c.share.whisper.cli = cli;
        c.share.whisper.model = c.data_dir.join("test-model");
        std::fs::write(&c.share.whisper.model, "test").unwrap();
        c.share.whisper.vad_model = None;
        tweak(c);
    }).await
}

async fn voice_bytes(h: &H) -> Vec<u8> {
    let path = h.dir.path().join("voice.m4a");
    assert!(tokio::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac"
        ])
        .arg(&path)
        .status()
        .await
        .unwrap()
        .success());
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn voice_notes_use_typed_planner_and_preserve_one_pair_of_items() {
    for tools in [false, true] {
        for on_device in [false, true] {
            let services = services().await;
            let h = voice_setup(&services, tools, VOICE_TEXT).await;
            let bytes = voice_bytes(&h).await;
            let id = "voice-note-0001";
            let ack: Value = submit(
                &h,
                voice_form(id, bytes.clone(), on_device.then_some(VOICE_TEXT), "  "),
            )
            .await
            .json()
            .await
            .unwrap();
            assert_eq!(ack["attachments"][0]["kind"], "voice");
            let done = finished(&h, id).await;
            assert_eq!(done["status"], "done", "{done}");
            assert_eq!(done["attachments"][0]["transcript"], VOICE_TEXT);
            let lines = h.timeline_lines();
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert_eq!(lines[0]["role"], "user");
            let a = &lines[0]["attachments"][0];
            assert_eq!(a["kind"], "voice");
            assert_eq!(a["waveform"], "AAEC/w==");
            assert_eq!(a["durationMs"], 1000);
            assert_eq!(a["transcript"].as_str(), on_device.then_some(VOICE_TEXT));
            assert_eq!(lines[1]["role"], "lucina");
            assert_eq!(lines[1]["meta"]["transcript"], VOICE_TEXT);
            assert_eq!(lines[1]["meta"]["voiceReply"], true);
            assert_eq!(lines[1]["replyTo"], lines[0]["id"]);
            let requests = h.fake.requests();
            let all = requests.iter().map(Value::to_string).collect::<String>();
            assert!(all.contains(VOICE_TEXT), "{all}");
            assert!(!all.contains("<Shared content>"), "{all}");
            assert!(
                all.contains("The owner spoke this as a voice note"),
                "{all}"
            );
            // User text is precisely the typed-turn block, with no share wrapper.
            assert!(requests
                .iter()
                .any(|r| r["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["role"] == "user"
                        && m["content"]
                            .as_str()
                            .is_some_and(|s| s.ends_with(VOICE_TEXT)
                                && !s.contains("Owner's question or note")))));
            let records = h.state.share.memory.recent(0, 10).unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].kind, "audio");
            assert_eq!(records[0].subtype.as_deref(), Some("voice"));
            assert_eq!(records[0].text, VOICE_TEXT);
            assert_eq!(
                h.dir.path().join("whisper-test.calls").exists(),
                !on_device,
                "on-device transcript must bypass Whisper"
            );
            if tools {
                assert!(
                    requests.iter().any(|r| r["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "tool")),
                    "a tool ran without creating a progress timeline item"
                );
            }
            let count = requests.len();
            let retry: Value = submit(&h, voice_form(id, bytes, Some(VOICE_TEXT), ""))
                .await
                .json()
                .await
                .unwrap();
            assert_eq!(retry["duplicate"], true);
            assert_eq!(retry["lucinaItemId"], done["lucinaItemId"]);
            assert_eq!(h.timeline_lines().len(), 2);
            assert_eq!(h.fake.requests().len(), count);
        }
    }
}

#[tokio::test]
async fn failed_or_empty_voice_transcription_asks_to_repeat_without_model() {
    for invalid_audio in [false, true] {
        let services = services().await;
        let h = voice_setup(&services, false, "").await;
        let bytes = if invalid_audio {
            b"unreadable".to_vec()
        } else {
            voice_bytes(&h).await
        };
        assert_eq!(
            submit(&h, voice_form("voice-failed-001", bytes, Some("  "), ""))
                .await
                .status(),
            202
        );
        let done = finished(&h, "voice-failed-001").await;
        assert_eq!(done["status"], "done", "{done}");
        assert_eq!(done["attachments"][0]["state"], "failed");
        let lines = h.timeline_lines();
        assert_eq!(lines.len(), 2);
        // The device locale (`lang=it`) is ignored: no words of his own and no recent messages: English.
        assert_eq!(
            lines[1]["text"],
            "I couldn't hear you. Could you repeat that?"
        );
        assert_eq!(lines[1]["meta"]["voiceReply"], true);
        assert!(h.fake.requests().is_empty());
    }
}

#[tokio::test]
async fn the_repeat_request_follows_the_language_of_his_caption() {
    let services = services().await;
    let h = voice_setup(&services, false, "").await;
    let bytes = b"unreadable".to_vec();
    assert_eq!(
        submit(
            &h,
            voice_form(
                "voice-caption-1",
                bytes,
                Some("  "),
                "Non si sente niente, cosa dice?"
            )
        )
        .await
        .status(),
        202
    );
    let done = finished(&h, "voice-caption-1").await;
    assert_eq!(done["status"], "done", "{done}");
    let lines = h.timeline_lines();
    assert_eq!(
        lines[1]["text"],
        "Non sono riuscita a sentirti. Puoi ripetere?"
    );
}

#[tokio::test]
async fn captioned_and_mixed_voice_notes_keep_the_share_turn() {
    for mixed in [false, true] {
        let services = services().await;
        let h = setup(&services).await;
        let mut form = voice_form(
            "voice-caption-01",
            b"audio".to_vec(),
            Some(VOICE_TEXT),
            if mixed { "" } else { "Che ne pensi?" },
        );
        if mixed {
            form = form.text("text", "another shared part");
        }
        assert_eq!(submit(&h, form).await.status(), 202);
        let done = finished(&h, "voice-caption-01").await;
        assert_eq!(done["status"], "done", "{done}");
        let all = h
            .fake
            .requests()
            .iter()
            .map(Value::to_string)
            .collect::<String>();
        assert!(all.contains("<Shared content>"), "{all}");
        assert!(all.contains(VOICE_TEXT));
        assert!(all.contains("audio/voice"));
        assert!(all.contains("The owner spoke this as a voice note"));
        assert_eq!(h.timeline_lines().len(), 2);
        assert_eq!(h.timeline_lines()[1]["meta"]["voiceReply"], true);
    }
}

#[tokio::test]
async fn whisper_voice_audio_is_capped_at_ten_minutes() {
    let services = services().await;
    let h = voice_setup(&services, false, VOICE_TEXT).await;
    let path = h.dir.path().join("long.m4a");
    assert!(tokio::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=16000:cl=mono",
            "-t",
            "601",
            "-c:a",
            "aac"
        ])
        .arg(&path)
        .status()
        .await
        .unwrap()
        .success());
    assert_eq!(
        submit(
            &h,
            voice_form("voice-duration-01", std::fs::read(path).unwrap(), None, "")
        )
        .await
        .status(),
        202
    );
    assert_eq!(finished(&h, "voice-duration-01").await["status"], "done");
    let dir = h
        .state
        .cfg
        .share
        .dir
        .join(chrono::Utc::now().format("%Y-%m").to_string())
        .join("voice-duration-01");
    let wav = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".share.wav"))
        .unwrap();
    let duration = lucina_memo::transcribe::duration(&h.state.cfg.share.whisper, &wav)
        .await
        .unwrap();
    assert!((599.9..=600.0).contains(&duration), "{duration}");
}

#[tokio::test]
async fn long_voice_transcripts_skip_summary_and_reach_planner_without_timeline_caps() {
    let services = services().await;
    let h = voice_setup(&services, false, "unused").await;
    let transcript = format!("{} END_OF_VOICE", "parola ".repeat(2000));
    assert_eq!(
        submit(
            &h,
            voice_form(
                "voice-longtext-01",
                b"audio".to_vec(),
                Some(&transcript),
                ""
            )
        )
        .await
        .status(),
        202
    );
    let done = finished(&h, "voice-longtext-01").await;
    assert_eq!(done["status"], "done", "{done}");
    let records = h.state.share.memory.recent(0, 10).unwrap();
    assert_eq!(records[0].text, transcript);
    assert!(records[0].summary.is_none());
    let lines = h.timeline_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0]["attachments"][0]["transcript"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        4000
    );
    assert_eq!(
        lines[1]["meta"]["transcript"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        4000
    );
    assert!(h
        .fake
        .requests()
        .iter()
        .any(|r| r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.ends_with("END_OF_VOICE")))));
}

const REPLY_HEADER: &str = "The owner is replying to this earlier message";

/// Swipe to reply on an image share: the chat turn gets what was read from it.
#[tokio::test]
async fn a_chat_reply_to_an_image_share_quotes_what_was_read() {
    let services = services().await;
    let h = setup(&services).await;
    let png = h.dir.path().join("green.png");
    assert!(tokio::process::Command::new("convert")
        .args(["-size", "16x16", "xc:green"])
        .arg(&png)
        .status()
        .await
        .unwrap()
        .success());
    let ack: Value = submit(
        &h,
        Form::new()
            .text("shareId", "share-reply-image-1")
            .text("channel", "ios")
            .text("note", "Guarda qui")
            .part(
                "file",
                Part::bytes(std::fs::read(&png).unwrap())
                    .file_name("green.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(finished(&h, "share-reply-image-1").await["status"], "done");
    let before = h.fake.requests().len();
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "ok, e quindi?", "sessionId": "test", "channel": "ios",
                   "replyTo": ack["userItemId"]}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let asked = h.fake.requests()[before..]
        .iter()
        .map(|r| r.to_string())
        .collect::<String>();
    assert!(asked.contains(REPLY_HEADER), "{asked}");
    assert!(asked.contains("«Guarda qui»"), "{asked}");
    assert!(
        asked.contains(r#"- image \"green.png\", read from it: "#),
        "{asked}"
    );
    assert!(asked.contains("VISION_EXTRACTED"), "{asked}");
    // From the quote, not from the "Earlier attachments" recall.
    assert!(!asked.contains("Earlier attachments"), "{asked}");
    let user = h
        .timeline_lines()
        .into_iter()
        .find(|l| l["id"] == b["data"]["userItemId"])
        .unwrap();
    assert_eq!(user["replyTo"], ack["userItemId"]);
}

/// `replyTo` on `/v1/share`: the share's user item records it and the share
/// turn quotes the earlier message; a malformed one is refused, an unknown
/// one ignored.
#[tokio::test]
async fn a_share_can_reply_to_an_earlier_message() {
    let services = services().await;
    let h = setup(&services).await;
    let quoted = h
        .state
        .timeline
        .append(lucina_store::timeline::NewItem {
            channel: "ios".into(),
            role: "lucina".into(),
            text: "QUOTED_LINE: mandami la ricevuta".into(),
            ..Default::default()
        })
        .unwrap();
    let bad = submit(&h, Form::new().text("text", "x").text("replyTo", "../etc")).await;
    assert_eq!(bad.status(), 400);
    let ack: Value = submit(
        &h,
        Form::new()
            .text("shareId", "share-reply-text-1")
            .text("channel", "ios")
            .text("text", "TEXT_EXTRACTED ricevuta 12 euro")
            .text("replyTo", quoted.id.clone()),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(finished(&h, "share-reply-text-1").await["status"], "done");
    let lines = h.timeline_lines();
    let user = lines.iter().find(|l| l["id"] == ack["userItemId"]).unwrap();
    assert_eq!(user["replyTo"], quoted.id.as_str());
    let turn = h
        .fake
        .requests()
        .into_iter()
        .find(|r| r["tools"].is_array() && r.to_string().contains("TEXT_EXTRACTED"))
        .unwrap();
    let message = turn["messages"].to_string();
    assert!(message.contains(REPLY_HEADER), "{message}");
    assert!(message.contains("(from Lucina, "), "{message}");
    assert!(
        message.contains("«QUOTED_LINE: mandami la ricevuta»"),
        "{message}"
    );
    // An id that names nothing: the share goes through without replyTo.
    let ack: Value = submit(
        &h,
        Form::new()
            .text("shareId", "share-reply-text-2")
            .text("text", "TEXT_EXTRACTED altro")
            .text("replyTo", "01K83ZK4Q6X1V9W2D3E4F5G6H7"),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(finished(&h, "share-reply-text-2").await["status"], "done");
    let user = h
        .timeline_lines()
        .into_iter()
        .find(|l| l["id"] == ack["userItemId"])
        .unwrap();
    assert!(user.get("replyTo").is_none(), "{user}");
}

#[tokio::test]
async fn shared_text_about_secrets_is_not_the_owner_asking() {
    let services = services().await;
    let h = setup(&services).await;
    // OCR of a screenshot (or any shared text) can read like a secrets question; the owner's
    // caption decides, so this is a normal share turn, not the secrets status reply.
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-secrets-0001")
            .text("note", "riassumi")
            .text(
                "text",
                "is the todoist token configured? which secrets are set?",
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finished(&h, "share-secrets-0001").await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(
        done["reply"], "I read your shared text, article and image.",
        "{done}"
    );
}

/// A Jev that answers the vocab layer's `span0` question with `pick` at 0.95 (or fails with a 500).
struct VocabJev {
    pick: Option<&'static str>,
    calls: Arc<Mutex<usize>>,
}

impl wiremock::Respond for VocabJev {
    fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
        *self.calls.lock().unwrap() += 1;
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(
            body["questions"].get("span0").is_some(),
            "only the vocab layer asks here"
        );
        let Some(c) = self.pick else {
            return wiremock::ResponseTemplate::new(500);
        };
        let other = if c == "none" { "c1" } else { "none" };
        wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0", "usage": {"input_tokens": 10, "output_tokens": 1},
            "answers": {"span0": {"type": "choice", "choice": c, "confidence": 0.95,
                "probabilities": {c: 0.95, other: 0.05}}}}))
    }
}

async fn vocab_jev(pick: Option<&'static str>) -> (wiremock::MockServer, Arc<Mutex<usize>>) {
    let server = wiremock::MockServer::start().await;
    let calls = Arc::new(Mutex::new(0));
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(VocabJev {
            pick,
            calls: calls.clone(),
        })
        .mount(&server)
        .await;
    (server, calls)
}

fn with_vocab_jev(server: &wiremock::MockServer) -> impl FnOnce(&mut lucina_server::Config) + '_ {
    let url = format!("{}/v1/systemone", server.uri());
    move |c| {
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

fn write_vocab(h: &H, entries: &[(&str, &[&str], Option<&str>)]) {
    let mut v = lucina_vocab::Vocab::default();
    for (i, (word, sounds, meaning)) in entries.iter().enumerate() {
        let mut e = lucina_vocab::Entry::new(*word);
        e.id = format!("vl_t{i}");
        e.sounds_like = sounds.iter().map(|s| s.to_string()).collect();
        e.meaning = meaning.map(str::to_owned);
        v.entries.push(e);
    }
    v.save(h.state.vocab.path()).unwrap();
}

/// Sends an on-device transcript as a voice note and returns what the share saw of it.
async fn on_device_note(h: &H, id: &str, transcript: &str) -> (Value, String) {
    let bytes = voice_bytes(h).await;
    let ack = submit(h, voice_form(id, bytes, Some(transcript), "  ")).await;
    assert_eq!(ack.status(), 202);
    let done = finished(h, id).await;
    assert_eq!(done["status"], "done", "{done}");
    let all = h
        .fake
        .requests()
        .iter()
        .map(Value::to_string)
        .collect::<String>();
    (done, all)
}

#[tokio::test]
async fn an_on_device_voice_note_gets_the_vocabulary_and_its_meanings() {
    let services = services().await;
    let h = voice_setup(&services, false, "unused").await;
    write_vocab(
        &h,
        &[(
            "Skibigårdsvej",
            &["Skibi gaardsvej"],
            Some("the owner's home street"),
        )],
    );
    let (done, all) =
        on_device_note(&h, "voice-vocab-0001", "porta il pacco a Skibi gaardsvej").await;
    // The turn and the memory record read the corrected words, and the meaning joins the context.
    assert!(all.contains("porta il pacco a Skibigårdsvej"), "{all}");
    assert!(!all.contains("Skibi gaardsvej"), "{all}");
    assert!(
        all.contains("Vocabulary: Skibigårdsvej = the owner's home street"),
        "{all}"
    );
    let records = h.state.share.memory.recent(0, 10).unwrap();
    assert_eq!(records[0].text, "porta il pacco a Skibigårdsvej");
    assert_eq!(done["status"], "done");
    // The phone's own transcript stays on the owner's item as the phone made it.
    assert_eq!(
        h.timeline_lines()[0]["attachments"][0]["transcript"],
        "porta il pacco a Skibi gaardsvej"
    );
    assert!(
        !h.dir.path().join("whisper-test.calls").exists(),
        "Whisper stayed out of it"
    );
}

#[tokio::test]
async fn an_ambiguous_word_in_a_voice_note_goes_to_jev_and_fails_open() {
    let entries: &[(&str, &[&str], Option<&str>)] = &[("Nicola", &[], None), ("Nicolò", &[], None)];
    let text = "Ho parlato con Nicolu.";
    let first = lucina_vocab::Vocab {
        entries: entries
            .iter()
            .enumerate()
            .map(|(i, (w, _, _))| {
                let mut e = lucina_vocab::Entry::new(*w);
                e.id = format!("vl_t{i}");
                e
            })
            .collect(),
    }
    .correct(text)
    .ambiguous[0]
        .candidates[0]
        .clone();

    // Jev picks the first candidate: the note says it.
    let (jev, calls) = vocab_jev(Some("c1")).await;
    let svc = services().await;
    let h = voice_setup_with(&svc, false, "unused", with_vocab_jev(&jev)).await;
    write_vocab(&h, entries);
    let (_, all) = on_device_note(&h, "voice-vocab-0002", text).await;
    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(all.contains(&format!("Ho parlato con {first}.")), "{all}");
    assert!(!all.contains("Nicolu"), "{all}");

    // "none", a Jev error and no Jev at all leave the words as the phone heard them.
    for pick in [Some("none"), None] {
        let (jev, calls) = vocab_jev(pick).await;
        let svc = services().await;
        let h = voice_setup_with(&svc, false, "unused", with_vocab_jev(&jev)).await;
        write_vocab(&h, entries);
        let (done, all) = on_device_note(&h, "voice-vocab-0003", text).await;
        assert_eq!(*calls.lock().unwrap(), 1, "{pick:?}");
        assert!(all.contains("Ho parlato con Nicolu."), "{all}");
        assert_eq!(done["status"], "done", "the note is still answered");
    }
    let svc = services().await;
    let h = voice_setup(&svc, false, "unused").await;
    write_vocab(&h, entries);
    let (_, all) = on_device_note(&h, "voice-vocab-0004", text).await;
    assert!(all.contains("Ho parlato con Nicolu."), "{all}");
}

fn offered_tools(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

/// 2 Nov 2026: both came with a screenshot, and she answered that her calendar, mail and Lucina Agents
/// work "only in my normal chat". A request with an image reaches the model with her real tools.
#[tokio::test]
async fn a_request_with_an_image_has_her_full_tools() {
    let services = services().await;
    let h = setup(&services).await;
    for (id, note) in [
        ("share-full-tools-1", "6 november and 13 November are missing events check our chat and university email also"),
        ("share-full-tools-2", "Tell one agent to fix this"),
    ] {
        let before = h.fake.requests().len();
        let r = submit(&h, Form::new().text("shareId", id).text("note", note).text("text", "from a screenshot")).await;
        assert_eq!(r.status(), 202);
        assert_eq!(finished(&h, id).await["status"], "done");
        let reqs = h.fake.requests();
        let turn = reqs[before..]
            .iter()
            .find(|r| !offered_tools(r).is_empty())
            .expect("the share turn is a tool loop");
        let tools = offered_tools(turn);
        for needed in ["calendar", "notes", "tasks", "reminders"] {
            assert!(tools.iter().any(|t| t == needed), "{note}: {needed} not in {tools:?}");
        }
        assert!(!tools.iter().any(|t| t == "secrets_status"), "{tools:?}");
        let system = turn["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("your one chat with him"), "{system}");
        assert!(!system.contains("These tools are only the ones for this step"), "{system}");
    }
    // A bare file keeps the focused set.
    let before = h.fake.requests().len();
    submit(&h, Form::new().text("shareId", "share-full-tools-3").text("text", "just a file")).await;
    finished(&h, "share-full-tools-3").await;
    let reqs = h.fake.requests();
    let tools = offered_tools(reqs[before..].iter().find(|r| !offered_tools(r).is_empty()).unwrap());
    assert!(tools.iter().any(|t| t == "notes"), "{tools:?}");
    assert!(!tools.iter().any(|t| t == "calendar"), "{tools:?}");
}

/// A cold vision start used to eat the whole read: IMG_7759 came back with nothing, OCR included.
#[tokio::test]
async fn slow_vision_costs_only_vision() {
    let services = services().await;
    services.seen.pause_vision.store(true, std::sync::atomic::Ordering::SeqCst);
    let h = harness(script(json!({"rules":[
        {"kind":"intent", "content":"{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind":"agent", "content":"I read what I could."}
    ]})), |c| {
        c.share.dir = c.data_dir.join("shares");
        c.share.key_file = Some(c.data_dir.join("share.key"));
        std::fs::write(c.share.key_file.as_ref().unwrap(), "  gateway-test-secret\n").unwrap();
        c.share.media_url = services.base.clone();
        c.share.vision_url = Some(services.base.clone());
        c.share.vision_budget = Duration::from_millis(200);
        c.share.test_allow_loopback = true;
    }).await;
    let png = h.dir.path().join("green.png");
    assert!(std::process::Command::new("convert")
        .args(["-size", "16x16", "xc:green"])
        .arg(&png)
        .status()
        .unwrap()
        .success());
    let r = submit(
        &h,
        Form::new()
            .text("shareId", "share-slow-vision")
            .text("note", "what is this?")
            .part(
                "file",
                Part::bytes(std::fs::read(&png).unwrap())
                    .file_name("IMG_7759.png")
                    .mime_str("image/png")
                    .unwrap(),
            ),
    )
    .await;
    assert_eq!(r.status(), 202);
    let done = finished(&h, "share-slow-vision").await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["attachments"][0]["state"], "read", "the read went on without vision: {done}");
    let prompts = h.fake.requests().iter().map(|r| r.to_string()).collect::<String>();
    assert!(prompts.contains("vision took too long"), "the model is told: {prompts}");
}
