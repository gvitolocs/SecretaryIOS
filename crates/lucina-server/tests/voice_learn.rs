//! Voice Learn (docs/lucinavoice-protocol.md section 8, lucina-rs half): the vocabulary API, the suggestion from
//! a transcript edit, the meaning as context on every turn whose text holds the word, and the meaning as a
//! Honcho conclusion that follows create, edit and delete.

mod common;

use std::sync::{Arc, Mutex};

use common::{harness, script, H, TOKEN};
use lucina_memory::MemoryMode;
use reqwest::Method;
use serde_json::{json, Value};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const REPLY: &str = "Va bene.";

fn chat_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": REPLY},
    ]}))
}

async fn api(h: &H, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let (s, b, _) = h.call(method, path, body).await;
    (s, b)
}

fn home() -> Value {
    json!({"word": "Skibigårdsvej", "soundsLike": ["schiavi garde"], "meaning": "the owner's home street", "lang": "da"})
}

fn model_text(h: &H) -> String {
    h.fake
        .requests()
        .iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_api_needs_the_bearer_like_the_rest_of_secretary() {
    let h = harness(chat_script(), |_| {}).await;
    for (m, p) in [
        (Method::GET, "/voice-learn"),
        (Method::POST, "/voice-learn"),
        (Method::PATCH, "/voice-learn/vl_x"),
        (Method::DELETE, "/voice-learn/vl_x"),
        (Method::POST, "/voice-learn/suggest"),
    ] {
        let resp = h
            .http
            .request(m, h.url(p))
            .json(&json!({"word": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 401, "{p}");
        let resp = h
            .http
            .get(h.url(p))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 401);
    }
    assert!(!h.state.vocab.path().exists(), "nothing was written");
    let _ = TOKEN;
}

#[tokio::test]
async fn entries_are_created_listed_edited_and_deleted() {
    let h = harness(chat_script(), |_| {}).await;
    let (s, b) = api(&h, Method::GET, "/voice-learn", None).await;
    assert_eq!(
        (s, b["data"]["entries"].as_array().unwrap().len()),
        (200, 0)
    );
    assert_eq!(b["data"]["max"], 500);

    let (s, b) = api(&h, Method::POST, "/voice-learn", Some(home())).await;
    assert_eq!(s, 201, "{b}");
    let e = &b["data"]["entry"];
    let id = e["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("vl_"));
    assert_eq!(e["word"], "Skibigårdsvej");
    assert_eq!(e["source"], "app");
    assert_eq!(e["createdAt"], e["updatedAt"]);
    assert_eq!(
        b["data"]["memory"], "skipped",
        "memory is off in the harness"
    );
    // The file is the one lucina-memo and the STT paths read.
    let file = Vocab::load(h.state.vocab.path()).unwrap();
    assert_eq!(file.entries.len(), 1);

    let (_, b) = api(&h, Method::GET, "/voice-learn", None).await;
    assert_eq!(b["data"]["entries"][0]["id"], id);
    let (s, b) = api(&h, Method::GET, &format!("/voice-learn/{id}"), None).await;
    assert_eq!(
        (s, b["data"]["entry"]["word"].as_str()),
        (200, Some("Skibigårdsvej"))
    );

    // PATCH merges; null clears; absent keeps.
    let (s, b) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"soundsLike": ["schiavi garde", "ski bi gor"], "lang": null})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let e = &b["data"]["entry"];
    assert_eq!(e["soundsLike"].as_array().unwrap().len(), 2);
    assert_eq!(e["meaning"], "the owner's home street");
    assert!(e.get("lang").is_none());
    assert_ne!(e["updatedAt"], e["createdAt"]);

    let (s, b) = api(&h, Method::DELETE, &format!("/voice-learn/{id}"), None).await;
    assert_eq!((s, b["data"]["deleted"].clone()), (200, json!(true)));
    let (s, _) = api(&h, Method::DELETE, &format!("/voice-learn/{id}"), None).await;
    assert_eq!(s, 404);
    let (s, _) = api(
        &h,
        Method::PATCH,
        "/voice-learn/vl_none",
        Some(json!({"word": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    let (_, b) = api(&h, Method::GET, "/voice-learn", None).await;
    assert!(b["data"]["entries"].as_array().unwrap().is_empty());
}

use lucina_vocab::Vocab;

#[tokio::test]
async fn invalid_entries_are_refused() {
    let h = harness(chat_script(), |_| {}).await;
    let long = "x".repeat(101);
    for (body, what) in [
        (json!({}), "no word"),
        (json!({"word": "  "}), "blank word"),
        (json!({"word": 5}), "word not a string"),
        (json!({"word": long}), "word over 100 characters"),
        (
            json!({"word": "ok", "meaning": "y".repeat(101)}),
            "meaning too long",
        ),
        (json!({"word": "ok", "lang": "fr"}), "bad lang"),
        (json!({"word": "ok", "source": "robot"}), "bad source"),
        (
            json!({"word": "ok", "soundsLike": "no"}),
            "soundsLike not a list",
        ),
        (json!({"word": "ok", "soundsLike": [1]}), "soundsLike item"),
        (
            json!({"word": "ok", "soundsLike": vec!["a"; 25].into_iter().enumerate().map(|(i, _)| format!("s{i}")).collect::<Vec<_>>()}),
            "21+ soundsLike",
        ),
    ] {
        let (s, b) = api(&h, Method::POST, "/voice-learn", Some(body)).await;
        assert_eq!(s, 400, "{what}: {b}");
    }
    // A duplicate word (case-insensitive) is a conflict.
    let (s, _) = api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "Marco"})),
    )
    .await;
    assert_eq!(s, 201);
    let (s, b) = api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "marco"})),
    )
    .await;
    assert_eq!(s, 409, "{b}");
    // A patch cannot break the rules either.
    let (_, b) = api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "Anna"})),
    )
    .await;
    let id = b["data"]["entry"]["id"].as_str().unwrap().to_owned();
    let (s, _) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"word": "MARCO"})),
    )
    .await;
    assert_eq!(s, 409);
    let (s, _) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"lang": "xx"})),
    )
    .await;
    assert_eq!(s, 400);
    // Wrong methods.
    let (s, _) = api(&h, Method::PUT, "/voice-learn", Some(json!({}))).await;
    assert_eq!(s, 405);
    // The 500-entry cap.
    let mut v = Vocab::load(h.state.vocab.path()).unwrap();
    let have = v.entries.len();
    for i in have..lucina_vocab::MAX_ENTRIES {
        let mut e = lucina_vocab::Entry::new(format!("word{i}"));
        e.id = format!("vl_fill{i}");
        v.entries.push(e);
    }
    v.save(h.state.vocab.path()).unwrap();
    let (s, b) = api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "one more"})),
    )
    .await;
    assert_eq!(s, 400, "{b}");
    assert!(b["message"].as_str().unwrap().contains("500"));
}

#[tokio::test]
async fn a_transcript_edit_suggests_entries() {
    let h = harness(chat_script(), |_| {}).await;
    let (s, b) = api(
        &h,
        Method::POST,
        "/voice-learn/suggest",
        Some(json!({"transcript": "porta il pacco a schiavi garde vai", "corrected": "porta il pacco a Skibigårdsvej"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let c = b["data"]["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0]["word"], "Skibigårdsvej");
    assert_eq!(c[0]["soundsLike"], json!(["schiavi garde vai"]));
    assert_eq!(c[0]["source"], "correction");
    // Nothing was stored: the app offers, the owner accepts with POST.
    assert!(!h.state.vocab.path().exists());
    // A known word points at its entry.
    let (_, b) = api(&h, Method::POST, "/voice-learn", Some(home())).await;
    let id = b["data"]["entry"]["id"].clone();
    let (_, b) = api(
        &h,
        Method::POST,
        "/voice-learn/suggest",
        Some(json!({"transcript": "vado a ski bi gor", "corrected": "vado a Skibigårdsvej"})),
    )
    .await;
    assert_eq!(b["data"]["candidates"][0]["existingId"], id);
    // No edit, no candidates; missing fields are refused.
    let (_, b) = api(
        &h,
        Method::POST,
        "/voice-learn/suggest",
        Some(json!({"transcript": "ciao Marco", "corrected": "ciao Marco"})),
    )
    .await;
    assert!(b["data"]["candidates"].as_array().unwrap().is_empty());
    let (s, _) = api(
        &h,
        Method::POST,
        "/voice-learn/suggest",
        Some(json!({"transcript": "x"})),
    )
    .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn a_meaning_reaches_every_turn_that_mentions_the_word() {
    let h = harness(chat_script(), |_| {}).await;
    api(&h, Method::POST, "/voice-learn", Some(home())).await;
    // A word without a meaning adds nothing.
    api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "Marco"})),
    )
    .await;

    // An app turn.
    h.chat("porta il pacco a Skibigårdsvej, da Marco").await;
    let seen = model_text(&h);
    assert!(
        seen.contains("Vocabulary: Skibigårdsvej = the owner's home street"),
        "{seen}"
    );
    assert!(!seen.contains("Vocabulary: Marco"));

    // A turn that does not mention it does not get it.
    let before = h.fake.requests().len();
    h.chat("che tempo fa oggi?").await;
    let later: String = h.fake.requests()[before..]
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(!later.contains("Vocabulary:"), "{later}");

    // A spoken turn.
    let before = h.fake.requests().len();
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "quanto manca a skibigardsvej?", "channel": "voice", "voice": true}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let spoken: String = h.fake.requests()[before..]
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(
        spoken.contains("Vocabulary: Skibigårdsvej = the owner's home street"),
        "{spoken}"
    );

    // Editing the meaning changes the line; deleting the entry removes it (the file is re-read).
    let (_, b) = api(&h, Method::GET, "/voice-learn", None).await;
    let id = b["data"]["entries"][0]["id"].as_str().unwrap().to_owned();
    api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"meaning": "casa di Giuseppe"})),
    )
    .await;
    let before = h.fake.requests().len();
    h.chat("vado a Skibigårdsvej").await;
    let edited: String = h.fake.requests()[before..]
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(
        edited.contains("Vocabulary: Skibigårdsvej = casa di Giuseppe"),
        "{edited}"
    );
    api(&h, Method::DELETE, &format!("/voice-learn/{id}"), None).await;
    let before = h.fake.requests().len();
    h.chat("vado a Skibigårdsvej").await;
    let gone: String = h.fake.requests()[before..]
        .iter()
        .map(|r| format!("{r:?}"))
        .collect();
    assert!(!gone.contains("Vocabulary:"), "{gone}");
}

// ----------------------------------------------------------------------------------------- the memory

/// A Honcho that hands out conclusion ids and keeps what it was asked.
#[derive(Clone, Default)]
struct Honcho {
    next: Arc<Mutex<u32>>,
}

impl Respond for Honcho {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        if req.method.as_str() == "POST" && req.url.path().ends_with("/conclusions") {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            return ResponseTemplate::new(200).set_body_json(json!([{"id": format!("c{n}")}]));
        }
        ResponseTemplate::new(200).set_body_json(json!({}))
    }
}

async fn conclusion_calls(s: &MockServer) -> Vec<(String, String, Value)> {
    s.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().contains("/conclusions"))
        .map(|r| {
            (
                r.method.to_string(),
                r.url.path().to_owned(),
                serde_json::from_slice(&r.body).unwrap_or(Value::Null),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_meaning_is_written_replaced_and_removed_in_honcho() {
    let hon = MockServer::start().await;
    Mock::given(any())
        .respond_with(Honcho::default())
        .mount(&hon)
        .await;
    let uri = hon.uri();
    let h = harness(chat_script(), move |c| {
        c.memory_mode = MemoryMode::ReadWrite;
        c.honcho.base_url = uri;
    })
    .await;

    // No meaning, nothing to remember.
    let (_, b) = api(
        &h,
        Method::POST,
        "/voice-learn",
        Some(json!({"word": "Marco"})),
    )
    .await;
    assert_eq!(b["data"]["memory"], "unchanged");
    assert!(conclusion_calls(&hon).await.is_empty());

    // Created with a meaning: one conclusion, the owner's, from Lucina's peer.
    let (_, b) = api(&h, Method::POST, "/voice-learn", Some(home())).await;
    assert_eq!(b["data"]["memory"], "written");
    let id = b["data"]["entry"]["id"].as_str().unwrap().to_owned();
    let calls = conclusion_calls(&hon).await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "POST");
    let c = &calls[0].2["conclusions"][0];
    assert_eq!(c["observer_id"], "hermes");
    assert_eq!(c["observed_id"], "giuseppe");
    let content = c["content"].as_str().unwrap();
    assert!(
        content.contains("Skibigårdsvej") && content.contains("the owner's home street"),
        "{content}"
    );

    // Saving the same thing again writes nothing twice (a patch that changes only the sounds).
    let (_, b) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"soundsLike": ["schiavi garde", "ski bi gor"]})),
    )
    .await;
    assert_eq!(b["data"]["memory"], "unchanged");
    assert_eq!(conclusion_calls(&hon).await.len(), 1);

    // A new meaning: the new conclusion first, then the old one is dropped.
    let (_, b) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"meaning": "via di casa, a Aarhus"})),
    )
    .await;
    assert_eq!(b["data"]["memory"], "written");
    let calls = conclusion_calls(&hon).await;
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[1].0, "POST");
    assert!(calls[1].2["conclusions"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Aarhus"));
    assert_eq!(calls[2].0, "DELETE");
    assert!(calls[2].1.ends_with("/conclusions/c1"), "{}", calls[2].1);

    // The meaning goes: the conclusion goes.
    let (_, b) = api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"meaning": null})),
    )
    .await;
    assert_eq!(b["data"]["memory"], "removed");
    let calls = conclusion_calls(&hon).await;
    assert_eq!(calls.len(), 4);
    assert!(calls[3].0 == "DELETE" && calls[3].1.ends_with("/conclusions/c2"));

    // Back with a meaning, then the entry is deleted: removed again, and a second delete finds nothing.
    api(
        &h,
        Method::PATCH,
        &format!("/voice-learn/{id}"),
        Some(json!({"meaning": "casa"})),
    )
    .await;
    let (s, b) = api(&h, Method::DELETE, &format!("/voice-learn/{id}"), None).await;
    assert_eq!((s, b["data"]["memory"].as_str()), (200, Some("removed")));
    let calls = conclusion_calls(&hon).await;
    assert!(
        calls.last().unwrap().0 == "DELETE" && calls.last().unwrap().1.ends_with("/conclusions/c3")
    );
    let n = calls.len();
    let (s, _) = api(&h, Method::DELETE, &format!("/voice-learn/{id}"), None).await;
    assert_eq!(s, 404);
    assert_eq!(conclusion_calls(&hon).await.len(), n);
}

#[tokio::test]
async fn a_honcho_that_fails_never_fails_the_api() {
    let hon = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&hon)
        .await;
    let uri = hon.uri();
    let h = harness(chat_script(), move |c| {
        c.memory_mode = MemoryMode::ReadWrite;
        c.honcho.base_url = uri;
    })
    .await;
    let (s, b) = api(&h, Method::POST, "/voice-learn", Some(home())).await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["data"]["memory"], "failed");
    // The entry is saved all the same, and the next save tries again.
    let (_, b) = api(&h, Method::GET, "/voice-learn", None).await;
    assert_eq!(b["data"]["entries"].as_array().unwrap().len(), 1);
}
