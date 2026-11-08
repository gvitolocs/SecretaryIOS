//! Memory surfaces end to end: owner and MCP turns still read the owner's
//! Honcho context, and an MCP turn is remembered with its caller (label and
//! metadata), in tools mode too.

mod common;

use common::{harness, script};
use lucina_memory::MemoryMode;
use lucina_server::PlannerMode;
use serde_json::{json, Value};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn honcho() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    s
}

/// The `/messages` writes Honcho received (their `messages` arrays).
async fn written(s: &MockServer) -> Vec<Value> {
    s.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .flat_map(|r| {
            serde_json::from_slice::<Value>(&r.body).unwrap()["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

async fn context_reads(s: &MockServer) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.path().ends_with("/context"))
        .count()
}

async fn tools_harness(h: &MockServer) -> common::H {
    let uri = h.uri();
    harness(
        script(json!({"rules": [], "default": {"content": "Va bene."}})),
        move |c| {
            c.planner = PlannerMode::Tools;
            c.memory_mode = MemoryMode::ReadWrite;
            c.honcho.base_url = uri;
        },
    )
    .await
}

#[tokio::test]
async fn an_mcp_tools_turn_reads_context_and_is_remembered_with_its_caller() {
    let hon = honcho().await;
    let h = tools_harness(&hon).await;
    let (status, body) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from grok] che ne pensi del mio piano di allenamento?", "sessionId": "mcp-grok", "channel": "web"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await; // Honcho writes are spawned
    assert!(
        context_reads(&hon).await >= 1,
        "the owner delegate read Honcho context"
    );
    let msgs = written(&hon).await;
    let user = msgs
        .iter()
        .find(|m| m["metadata"]["role"] == "user")
        .expect("the turn was written to Honcho");
    assert_eq!(user["metadata"]["via"], "mcp", "{user}");
    assert_eq!(user["metadata"]["caller"], "grok", "{user}");
    assert!(
        user["content"]
            .as_str()
            .unwrap()
            .contains("via Lucina MCP from grok"),
        "{user}"
    );
}

#[tokio::test]
async fn an_owner_tools_turn_reads_context_and_is_remembered_untagged() {
    let hon = honcho().await;
    let h = tools_harness(&hon).await;
    let (status, body) = h
        .post(
            "/chat",
            json!({"message": "che ne pensi del mio piano di allenamento?", "sessionId": "test", "channel": "ios"}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        context_reads(&hon).await >= 1,
        "the owner read Honcho context"
    );
    let msgs = written(&hon).await;
    let user = msgs
        .iter()
        .find(|m| m["metadata"]["role"] == "user")
        .expect("the turn was written to Honcho");
    assert_eq!(user["metadata"], json!({"role": "user"}), "{user}");
}

#[tokio::test]
async fn a_request_without_the_bearer_reaches_no_memory() {
    let hon = honcho().await;
    let h = tools_harness(&hon).await;
    let resp = h
        .http
        .post(h.url("/chat"))
        .json(&json!({"message": "[via Lucina MCP, from grok] ciao", "sessionId": "mcp-grok"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(hon.received_requests().await.unwrap().is_empty());
    assert!(h.timeline_lines().is_empty());
}

#[tokio::test]
async fn a_non_owner_surface_gets_no_owner_context() {
    use lucina_memory::surface::{DiscordFacts, OwnerIds};
    use lucina_memory::{TurnFacts, TurnSurface};
    let hon = honcho().await;
    let h = tools_harness(&hon).await;
    // A first owner turn leaves recent turns in the session.
    h.chat("ricordati che il mio colore preferito è il verde")
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let before = hon.received_requests().await.unwrap().len();
    let owner = OwnerIds {
        discord_user_id: Some("649020113026023475".into()),
        ..OwnerIds::default()
    };
    // A guild message from the owner's own user id: a channel surface.
    let guild = TurnSurface::classify(
        &TurnFacts {
            channel: lucina_memory::surface::ChannelKind::Discord,
            discord: Some(DiscordFacts {
                author_id: Some("649020113026023475".into()),
                guild_id: Some("g".into()),
                channel_id: Some("c".into()),
            }),
            ..TurnFacts::default()
        },
        &owner,
    );
    for s in [guild, TurnSurface::classify(&TurnFacts::default(), &owner)] {
        let p = lucina_server::context::prepare_with(
            &h.state,
            &s,
            "secretary_test",
            "qual è il mio colore preferito?",
            "Giuseppe",
            "secretary",
            true,
        )
        .await;
        assert!(!p.memory_context.contains("verde"), "{}", p.memory_context);
        assert!(p.recent_context.is_empty());
    }
    assert_eq!(
        hon.received_requests().await.unwrap().len(),
        before,
        "no Honcho call for a non-owner surface"
    );
    // The owner's own app request still gets the recent turns and Honcho.
    let app = TurnSurface::classify(&TurnFacts::secretary("ios", true, None), &owner);
    let p = lucina_server::context::prepare_with(
        &h.state,
        &app,
        "secretary_test",
        "qual è il mio colore preferito?",
        "Giuseppe",
        "secretary",
        true,
    )
    .await;
    assert!(p.memory_context.contains("verde"), "{}", p.memory_context);
    assert!(hon.received_requests().await.unwrap().len() > before);
}
