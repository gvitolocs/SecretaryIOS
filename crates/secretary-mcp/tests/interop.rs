//! Interop with the official rmcp client over Streamable HTTP, in both
//! lifecycles: 2026-07-28 (stateless, `server/discover`) and 2025-11-25
//! (`initialize`).

mod common;

use common::*;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ClientLifecycleMode, ClientServiceExt};

async fn exercise(lifecycle: ClientLifecycleMode) {
    let h = start().await;
    let token = h.issue("claude-code", "read,chat,write");
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", h.base)).auth_header(token),
    );
    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("interop", "1"),
    )
    .serve_with_lifecycle(transport, lifecycle)
    .await
    .expect("connect");
    let info = client.peer_info().expect("server info");
    assert_eq!(
        info.server_info.as_ref().map(|i| i.name.as_str()),
        Some("lucina-mcp")
    );
    let tools = client.list_all_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 39);
    let r = client
        .call_tool(
            CallToolRequestParams::new("ask_lucina").with_arguments(
                serde_json::json!({"message": "ciao"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("tools/call");
    assert_ne!(r.is_error, Some(true));
    let text = serde_json::to_value(&r.content).unwrap()[0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        text.contains("[via Lucina MCP, from claude-code] ciao"),
        "{text}"
    );
    let res = client.list_all_resources().await.expect("resources/list");
    assert_eq!(res.len(), 2);
    let prompts = client.list_all_prompts().await.expect("prompts/list");
    assert_eq!(prompts[0].name, "daily_briefing");
    client.cancel().await.ok();
}

#[tokio::test]
async fn rmcp_client_2026_07_28() {
    exercise(ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
    })
    .await;
}

#[tokio::test]
async fn rmcp_client_2025_11_25_initialize() {
    exercise(ClientLifecycleMode::Initialize).await;
}
