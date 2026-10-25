//! Honcho client against a mocked Honcho (wiremock).

use std::time::Duration;

use lucina_memory::surface::{BackgroundOrigin, DiscordFacts, OwnerIds};
use lucina_memory::{
    secretary_session_id, HonchoConfig, Memory, MemoryError, MemoryMode, TurnFacts, TurnSurface,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> HonchoConfig {
    HonchoConfig {
        base_url: server.uri(),
        use_vector_search: true,
        context_tokens: 3000,
        timeout: Duration::from_secs(2),
        ..HonchoConfig::default()
    }
}

fn app() -> TurnSurface {
    TurnSurface::classify(
        &TurnFacts::secretary("web", true, None),
        &OwnerIds::default(),
    )
}

fn mcp(caller: &str) -> TurnSurface {
    TurnSurface::classify(
        &TurnFacts::secretary("web", true, Some(caller.into())),
        &OwnerIds::default(),
    )
}

fn unknown() -> TurnSurface {
    TurnSurface::classify(&TurnFacts::default(), &OwnerIds::default())
}

#[test]
fn session_ids_match_node() {
    assert_eq!(secretary_session_id("", "secretary_app"), "secretary_app");
    assert_eq!(
        secretary_session_id("iphone", "secretary_app"),
        "secretary_iphone"
    );
    assert_eq!(secretary_session_id("secretary_ios", "x"), "secretary_ios");
    assert_eq!(secretary_session_id("", ""), "secretary_app");
}

#[tokio::test]
async fn context_is_read_only_and_rendered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v3/workspaces/hermes-peer1/sessions/secretary_app/context"))
        .and(query_param("peer_target", "giuseppe"))
        .and(query_param("peer_perspective", "hermes"))
        .and(query_param("search_query", "che tempo fa?"))
        .and(query_param("tokens", "3000"))
        .and(header("authorization", "Bearer local-dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "secretary_app",
            "messages": [{"peer_id": "giuseppe", "content": "ciao"}, {"peer_id": "hermes", "content": "ciao!"}],
            "summary": {"content": "Giuseppe lives in Copenhagen.", "created_at": "2026-10-20T10:00:00Z"},
            "peer_representation": "Likes cycling.",
            "peer_card": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadOnly, cfg(&server)).unwrap();
    let ctx = mem
        .build_context(&app(), "giuseppe", "secretary_app", "che tempo fa?")
        .await;
    assert!(ctx.contains("Honcho session summary (derived 2026-10-20T10:00:00Z"));
    assert!(ctx.contains("Giuseppe lives in Copenhagen."));
    assert!(ctx.contains("conditioned on the CURRENT message: \"che tempo fa?\""));
    assert!(ctx.contains("giuseppe: ciao\nhermes: ciao!"));
    // read-only: remember refuses and nothing else was called (expect(1) above)
    assert!(matches!(
        mem.remember(&app(), "giuseppe", "secretary_app", "a", "b")
            .await,
        Err(MemoryError::ReadOnly)
    ));
}

#[tokio::test]
async fn context_failure_is_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadOnly, cfg(&server)).unwrap();
    assert_eq!(
        mem.build_context(&app(), "giuseppe", "secretary_app", "x")
            .await,
        ""
    );
    assert_eq!(
        Memory::off()
            .build_context(&app(), "giuseppe", "s", "x")
            .await,
        ""
    );
}

#[tokio::test]
async fn remember_writes_like_node() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/workspaces"))
        .and(body_json(json!({"id": "hermes-peer1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "hermes-peer1"})))
        .expect(1)
        .mount(&server)
        .await;
    for peer in ["giuseppe", "hermes"] {
        Mock::given(method("POST"))
            .and(path("/v3/workspaces/hermes-peer1/peers"))
            .and(body_json(json!({"id": peer})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": peer})))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v3/workspaces/hermes-peer1/sessions"))
        .and(body_json(json!({"id": "secretary_iphone"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "secretary_iphone"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/v3/workspaces/hermes-peer1/sessions/secretary_iphone/peers",
        ))
        .and(body_json(json!({"giuseppe": {}, "hermes": {}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/v3/workspaces/hermes-peer1/sessions/secretary_iphone/messages"))
        .and(body_json(json!({"messages": [
            {"peer_id": "giuseppe", "content": "[Secretary app] ciao", "metadata": {"role": "user"}},
            {"peer_id": "hermes", "content": "[action:reply]\n\nciao!", "metadata": {"role": "assistant"}}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([]))).expect(2).mount(&server).await;
    let mem = Memory::new(MemoryMode::ReadWrite, cfg(&server)).unwrap();
    for _ in 0..2 {
        mem.remember(
            &app(),
            "giuseppe",
            "secretary_iphone",
            "[Secretary app] ciao",
            "[action:reply]\n\nciao!",
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn search_and_representation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/workspaces/hermes-peer1/peers/giuseppe/search"))
        .and(body_json(json!({"query": "bike", "limit": 3})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"peer_id": "giuseppe", "content": "my bike"}])),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/v3/workspaces/hermes-peer1/peers/hermes/representation",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"representation": "facts"})))
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadOnly, cfg(&server)).unwrap();
    assert_eq!(
        mem.search(&app(), "bike", 3).await.unwrap()[0].content,
        "my bike"
    );
    let rep = mem
        .client()
        .unwrap()
        .representation("hermes", None, Some("giuseppe"), Some("bike"))
        .await
        .unwrap();
    assert_eq!(rep, "facts");
}

#[tokio::test]
async fn denied_reads_and_writes_make_no_requests() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadWrite, cfg(&server)).unwrap();
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
        &OwnerIds {
            discord_user_id: Some("649020113026023475".into()),
            ..OwnerIds::default()
        },
    );
    let background = TurnSurface::classify(
        &TurnFacts::background(BackgroundOrigin::Proactive, true),
        &OwnerIds::default(),
    );
    let quiet = TurnSurface::classify(
        &TurnFacts::background(BackgroundOrigin::Cron, false),
        &OwnerIds::default(),
    );
    for s in [unknown(), guild.clone(), quiet.clone()] {
        assert_ne!(s.class_name(), "owner_direct");
        assert_eq!(
            mem.build_context(&s, "giuseppe", "secretary_app", "x")
                .await,
            ""
        );
        assert!(mem.search(&s, "x", 3).await.unwrap().is_empty());
    }
    for s in [unknown(), guild, background, quiet] {
        assert!(matches!(
            mem.remember(&s, "giuseppe", "secretary_app", "a", "b")
                .await,
            Err(MemoryError::Denied { .. })
        ));
    }
    // An MCP delegate may not touch another person's records.
    assert_eq!(
        mem.build_context(&mcp("grok"), "someone", "s", "x").await,
        ""
    );
    assert!(matches!(
        mem.remember(&mcp("grok"), "someone", "s", "a", "b").await,
        Err(MemoryError::Denied {
            surface: "owner_delegate",
            scope: "person"
        })
    ));
    // expect(0) is verified when the server drops.
}

#[tokio::test]
async fn mcp_writes_are_tagged_with_the_caller() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/workspaces/hermes-peer1/sessions/secretary_mcp-grok/messages"))
        .and(body_json(json!({"messages": [
            {"peer_id": "giuseppe", "content": "q", "metadata": {"role": "user", "via": "mcp", "caller": "grok"}},
            {"peer_id": "hermes", "content": "a", "metadata": {"role": "assistant"}}
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadWrite, cfg(&server)).unwrap();
    mem.remember(&mcp("grok"), "giuseppe", "secretary_mcp-grok", "q", "a")
        .await
        .unwrap();
}

#[tokio::test]
async fn background_reads_only_for_the_owner() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/workspaces/hermes-peer1/peers/giuseppe/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"peer_id": "giuseppe", "content": "standup notes"}])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mem = Memory::new(MemoryMode::ReadOnly, cfg(&server)).unwrap();
    let owner_addressed = TurnSurface::classify(
        &TurnFacts::background(BackgroundOrigin::Proactive, true),
        &OwnerIds::default(),
    );
    let not_owner = TurnSurface::classify(
        &TurnFacts::background(BackgroundOrigin::Proactive, false),
        &OwnerIds::default(),
    );
    assert_eq!(
        mem.search(&owner_addressed, "standup", 5)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(mem
        .search(&not_owner, "standup", 5)
        .await
        .unwrap()
        .is_empty());
}
