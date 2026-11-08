//! The API end to end, with fake models, notes and speech.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::embed::fake::Letters;
use crate::ingest::Basic;
use crate::keep::fake::Memory;
use crate::llm::fake::Scripted;
use crate::podcast::Tts;
use crate::store::Store;
use crate::{api, Config, Deps, Notebooks};

struct Beep;

#[async_trait::async_trait]
impl Tts for Beep {
    async fn speak(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<u8>> {
        // 0.2 s of silence, 24 kHz mono 16-bit.
        let n: u32 = 9600;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        for x in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 24000u32.to_le_bytes().to_vec(), 48000u32.to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec(), 16u16.to_le_bytes().to_vec()] {
            b.extend_from_slice(&x);
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&n.to_le_bytes());
        b.resize(b.len() + n as usize, 0);
        Ok(b)
    }
}

fn setup(replies: &[&str]) -> (Notebooks, Arc<Scripted>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let llm = Arc::new(Scripted::new(replies));
    let mut cfg = Config::new(dir.path());
    cfg.ffmpeg = "/usr/bin/ffmpeg".into();
    for d in ["files", "podcasts", "work"] {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    let nb = Notebooks::with_store(
        cfg,
        Deps {
            extractor: Arc::new(Basic),
            embedder: Some(Arc::new(Letters)),
            llm: llm.clone(),
            keep: Arc::new(Memory::with(&[("lecture-1", "Lecture 1", "Port security limits MAC addresses per port.")])),
            tts: Arc::new(Beep),
        },
        Store::in_memory().unwrap(),
    )
    .unwrap();
    (nb, llm, dir)
}

async fn call(nb: &Notebooks, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = api::router(nb.clone()).oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn notebook_text_source_chat_with_citations_and_save_to_note() {
    // Replies in order: the Dense Summary insight, then the chat answer.
    let (nb, llm, _d) = setup(&[
        "DHCP snooping trusts uplinks and drops rogue offers.",
        "Rogue DHCP offers are dropped on untrusted ports [source:SRC-0]. Port security also helps [note:lecture-1]. Fake [source:s000000000a-3].",
    ]);
    let (s, v) = call(&nb, "POST", "/", Some(json!({ "name": "CNS", "description": "Communication and Network Security" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();

    let (s, v) = call(&nb, "POST", &format!("/{id}/sources"), Some(json!({ "kind": "text", "title": "DHCP lecture", "text": "DHCP snooping drops DHCP offers that arrive on untrusted ports, which stops rogue DHCP servers." }))).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["status"], "queued");
    let src = v["id"].as_str().unwrap().to_owned();

    let (s, v) = call(&nb, "POST", &format!("/{id}/notes"), Some(json!({ "slug": "lecture-1" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["indexed"], true);

    nb.drain().await;
    let (_, v) = call(&nb, "GET", &format!("/sources/{src}"), None).await;
    assert_eq!(v["status"], "ready", "{v}");
    assert_eq!(v["chunks"], 1);
    let (_, v) = call(&nb, "GET", &format!("/sources/{src}/insights"), None).await;
    assert_eq!(v["insights"][0]["status"], "ready", "{v}");
    assert_eq!(v["insights"][0]["transformation_id"], "t-dense-summary");

    // The scripted answer cites the real passage id.
    {
        let mut r = llm.replies.lock().unwrap();
        let last = r.pop().unwrap();
        r.push(last.replace("SRC", &src));
    }
    let (s, chat) = call(&nb, "POST", &format!("/{id}/chats"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let c = chat["id"].as_str().unwrap().to_owned();
    let (s, v) = call(&nb, "POST", &format!("/chats/{c}/messages"), Some(json!({ "text": "How are rogue DHCP servers stopped?" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let reply = &v["reply"];
    let text = reply["text"].as_str().unwrap();
    assert!(text.contains(&format!("[source:{src}-0]")), "{text}");
    assert!(!text.contains("s000000000a"), "{text}");
    let cites = reply["citations"].as_array().unwrap();
    assert_eq!(cites.len(), 2, "{reply}");
    assert_eq!(cites[0]["title"], "DHCP lecture");
    assert_eq!(cites[1]["slug"], "lecture-1");
    // The system prompt had the passage and the note.
    let asked = llm.asked.lock().unwrap().last().unwrap().turns[0].text.clone();
    assert!(asked.contains(&format!("id=\"source:{src}-0\"")), "{asked}");
    assert!(asked.contains("id=\"note:lecture-1\""), "{asked}");

    let (_, v) = call(&nb, "GET", &format!("/chats/{c}"), None).await;
    assert_eq!(v["messages"].as_array().unwrap().len(), 2);
    assert_eq!(v["title"], "How are rogue DHCP servers stopped?");

    // Save the answer to Keep: it becomes a linked note.
    let m = reply["id"].as_str().unwrap();
    let (s, v) = call(&nb, "POST", &format!("/chats/{c}/messages/{m}/note"), Some(json!({ "title": "Rogue DHCP" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["slug"], "rogue-dhcp");
    let (_, v) = call(&nb, "GET", &format!("/{id}"), None).await;
    assert_eq!(v["counts"]["notes"], 2, "{v}");
    assert_eq!(v["sources"][0]["context"], "full");

    // Search finds the passage by word.
    let (_, v) = call(&nb, "GET", &format!("/{id}/search?q=rogue"), None).await;
    assert!(v["hits"].as_array().unwrap().iter().any(|h| h["source_id"] == src.as_str()), "{v}");
}

#[tokio::test]
async fn context_off_hides_a_source_from_chat() {
    let (nb, llm, _d) = setup(&["summary", "Not in the sources."]);
    let (_, v) = call(&nb, "POST", "/", Some(json!({ "name": "RM" }))).await;
    let id = v["id"].as_str().unwrap().to_owned();
    let (_, v) = call(&nb, "POST", &format!("/{id}/sources"), Some(json!({ "kind": "text", "text": "Likert scales measure attitudes in questionnaires." }))).await;
    let src = v["id"].as_str().unwrap().to_owned();
    assert_eq!(v["title"], "Likert scales measure attitudes in questionnaires.");
    nb.drain().await;
    let (s, v) = call(&nb, "PATCH", &format!("/{id}/sources/{src}"), Some(json!({ "context": "off" }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, chat) = call(&nb, "POST", &format!("/{id}/chats"), None).await;
    let c = chat["id"].as_str().unwrap();
    let (_, v) = call(&nb, "POST", &format!("/chats/{c}/messages"), Some(json!({ "text": "What do Likert scales measure?" }))).await;
    assert_eq!(v["reply"]["citations"], json!([]));
    let asked = llm.asked.lock().unwrap().last().unwrap().turns[0].text.clone();
    assert!(asked.contains("(none"), "{asked}");
}

#[tokio::test]
async fn errors_have_the_contract_shape() {
    let (nb, _, _d) = setup(&[]);
    let (s, v) = call(&nb, "GET", "/nb0000000000", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    let (s, _) = call(&nb, "GET", "/not-an-id", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = call(&nb, "POST", "/", Some(json!({ "name": " " }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (_, v) = call(&nb, "POST", "/", Some(json!({ "name": "X" }))).await;
    let id = v["id"].as_str().unwrap();
    let (s, _) = call(&nb, "POST", &format!("/{id}/sources"), Some(json!({ "kind": "sandbox", "path": "/home/cua/../../etc/passwd" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&nb, "POST", &format!("/{id}/sources"), Some(json!({ "kind": "url", "url": "file:///etc/passwd" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(&nb, "POST", &format!("/{id}/podcasts"), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, _) = call(&nb, "DELETE", "/transformations/t-dense-summary", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, v) = call(&nb, "GET", "/transformations", None).await;
    assert_eq!(v["transformations"].as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn delete_notebook_with_exclusive_sources() {
    let (nb, _, _d) = setup(&["s1", "s2"]);
    let (_, a) = call(&nb, "POST", "/", Some(json!({ "name": "A" }))).await;
    let (_, b) = call(&nb, "POST", "/", Some(json!({ "name": "B" }))).await;
    let (a, b) = (a["id"].as_str().unwrap().to_owned(), b["id"].as_str().unwrap().to_owned());
    let (_, s1) = call(&nb, "POST", &format!("/{a}/sources"), Some(json!({ "kind": "text", "text": "only in A, long enough text" }))).await;
    let (_, s2) = call(&nb, "POST", &format!("/{a}/sources"), Some(json!({ "kind": "text", "text": "shared with B, long enough text" }))).await;
    let s2 = s2["id"].as_str().unwrap();
    let (s, _) = call(&nb, "POST", &format!("/{b}/sources"), Some(json!({ "kind": "source", "id": s2 }))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = call(&nb, "DELETE", &format!("/{a}?sources=delete-exclusive"), None).await;
    assert_eq!(v["sources_deleted"], 1);
    let (s, _) = call(&nb, "GET", &format!("/sources/{}", s1["id"].as_str().unwrap()), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&nb, "GET", &format!("/sources/{s2}"), None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn podcast_end_to_end_with_fake_voices() {
    if !std::path::Path::new("/usr/bin/ffmpeg").exists() {
        return;
    }
    let (nb, _, _d) = setup(&[
        "Dense summary of switches.",
        r#"{"title":"Switch security","segments":[{"name":"Intro","description":"what","size":"short"},{"name":"Wrap","description":"end","size":"short"}]}"#,
        r#"{"dialogue":[{"speaker":"Lucina","text":"Welcome."},{"speaker":"Marco","text":"Hi!"}]}"#,
        r#"{"dialogue":[{"speaker":"Lucina","text":"That's all."}]}"#,
    ]);
    let (_, v) = call(&nb, "POST", "/", Some(json!({ "name": "CNS" }))).await;
    let id = v["id"].as_str().unwrap().to_owned();
    call(&nb, "POST", &format!("/{id}/sources"), Some(json!({ "kind": "text", "text": "Switches learn MAC addresses per port and flood unknown ones." }))).await;
    nb.drain().await;
    let (s, p) = call(&nb, "POST", &format!("/{id}/podcasts"), Some(json!({ "minutes": 1 }))).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{p}");
    let pid = p["id"].as_str().unwrap().to_owned();
    nb.drain().await;
    let (_, p) = call(&nb, "GET", &format!("/podcasts/{pid}"), None).await;
    assert_eq!(p["status"], "ready", "{p}");
    assert_eq!(p["title"], "Switch security");
    assert_eq!(p["transcript"].as_array().unwrap().len(), 3);
    assert!(p["duration_s"].as_f64().unwrap() > 0.5);
    let req = Request::builder().uri(format!("/podcasts/{pid}/audio")).header(header::RANGE, "bytes=0-9").body(Body::empty()).unwrap();
    let resp = api::router(nb.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "audio/mpeg");
    assert_eq!(resp.into_body().collect().await.unwrap().to_bytes().len(), 10);
}

#[tokio::test]
async fn streamed_chat_sends_deltas_then_the_message() {
    let (nb, _, _d) = setup(&["sum", "Short answer."]);
    let (_, v) = call(&nb, "POST", "/", Some(json!({ "name": "S" }))).await;
    let id = v["id"].as_str().unwrap().to_owned();
    let (_, chat) = call(&nb, "POST", &format!("/{id}/chats"), None).await;
    // No sources: the first scripted reply goes to the chat.
    nb.deps().llm.complete(Default::default(), None).await.unwrap();
    let c = chat["id"].as_str().unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/chats/{c}/messages"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::from(json!({ "text": "hi" }).to_string()))
        .unwrap();
    let resp = api::router(nb.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(body.contains("event: delta"), "{body}");
    assert!(body.contains("event: message"), "{body}");
    assert!(body.contains("Short answer."), "{body}");
}
