//! Honcho is a sink: writes and queries go through the redactor.

use lucina_memory::surface::OwnerIds;
use lucina_memory::{HonchoConfig, Memory, MemoryMode, TurnFacts, TurnSurface};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const VALUE: &str = "honcho-sink-Secret-1234abc";

#[tokio::test]
async fn honcho_writes_and_queries_are_redacted() {
    lucina_secrets::redactor().register("HONCHO_TEST_KEY", VALUE);
    let s = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&s)
        .await;
    let cfg = HonchoConfig {
        base_url: s.uri(),
        ..HonchoConfig::default()
    };
    let m = Memory::new(MemoryMode::ReadWrite, cfg).unwrap();
    let app = TurnSurface::classify(
        &TurnFacts::secretary("ios", true, None),
        &OwnerIds::default(),
    );
    m.remember(
        &app,
        "giuseppe",
        "secretary_t",
        &format!("my key is {VALUE}"),
        "noted, ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234",
    )
    .await
    .unwrap();
    let _ = m.search(&app, &format!("where is {VALUE}"), 5).await;
    let _ = m
        .build_context(&app, "giuseppe", "secretary_t", &format!("context {VALUE}"))
        .await;
    let reqs = s.received_requests().await.unwrap();
    assert!(reqs.iter().any(|r| r.url.path().ends_with("/messages")));
    for r in reqs {
        let body = String::from_utf8_lossy(&r.body);
        assert!(!body.contains(VALUE), "{body}");
        assert!(!body.contains("ghp_ABCDEFGHIJKLMNOPQRST"), "{body}");
        assert!(!r.url.as_str().contains(VALUE), "{}", r.url);
    }
}
