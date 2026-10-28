//! The owner-only Instagram / Pixiv plugin tools through `/chat`: the owner's
//! app chat runs them (with the gateway secret, a private turn, a button to the
//! plugin page), Lucina MCP callers never do, and a missing secret is a clean
//! "not available" for the model.

mod common;

use common::{harness, script, H};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "ig-secret-for-the-test";

async fn instagram() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/stories/natgeo"))
        .and(header("x-lucina-gateway", SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [
            {"id": "s1", "kind": "image", "url": "/v1/instagram/s/a/1/9999999999/sig",
             "thumbUrl": "/v1/instagram/s/a/1t/9999999999/sigt"},
        ]})))
        .mount(&server)
        .await;
    server
}

fn rules() -> Value {
    json!({"rules": [
        {"kind": "agent", "afterTool": true, "content": "Ecco la storia di natgeo."},
        {"kind": "agent", "contains": "stories", "toolCalls": [{"name": "instagram_stories", "arguments": {"username": "natgeo"}}]},
    ]})
}

async fn owner_harness(server: &MockServer, with_secret: bool) -> H {
    let uri = server.uri();
    harness(script(rules()), move |c| {
        let file = c.data_dir.join("ig.secret");
        if with_secret {
            std::fs::write(&file, format!("{SECRET}\n")).unwrap();
        }
        c.rich_replies = true;
        c.plugins.instagram_url = Some(uri.clone());
        c.plugins.instagram_secret_file = Some(file);
        c.plugins.public_url = Some("https://getsecretary.tech/".into());
    })
    .await
}

fn tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|x| x["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_owner_app_chat_runs_the_plugin_tools_privately() {
    let server = instagram().await;
    let h = owner_harness(&server, true).await;
    let d = h
        .chat("show me the latest instagram stories of @natgeo")
        .await;
    assert_eq!(d["action"], "re_plugins", "{d}");
    assert_eq!(d["reply"], "Ecco la storia di natgeo.", "{d}");
    // the plugin page button
    let buttons = serde_json::to_string(&d).unwrap();
    assert!(buttons.contains("Open Instagram"), "{buttons}");
    // exactly the ten plugin tools were on offer
    let agent = h
        .fake
        .requests()
        .into_iter()
        .find(|r| !tool_names(r).is_empty())
        .expect("an agent request");
    let mut names = tool_names(&agent);
    names.sort();
    assert_eq!(
        names,
        [
            "instagram_new",
            "instagram_profile",
            "instagram_save_latest",
            "instagram_stories",
            "instagram_unwatch",
            "instagram_watch",
            "instagram_watchlist",
            "pixiv_ranking",
            "pixiv_search",
            "save_to_gallery"
        ]
    );
    // the model got the signed url as an absolute link
    let after = h
        .fake
        .requests()
        .into_iter()
        .filter_map(|r| {
            r["messages"]
                .as_array()?
                .iter()
                .rfind(|m| m["role"] == "tool")
                .map(|m| m["content"].to_string())
        })
        .next_back()
        .expect("a tool result");
    assert!(
        after.contains("https://getsecretary.tech/v1/instagram/s/a/1/9999999999/sig"),
        "{after}"
    );
    // the timeline line of the call names the tool and nothing else
    let tool_line = h
        .timeline_lines()
        .into_iter()
        .find(|l| l["text"].as_str().is_some_and(|t| t.starts_with("🔧")))
        .expect("tool line");
    assert_eq!(tool_line["text"], "🔧 instagram_stories → ok");
}

#[tokio::test]
async fn a_missing_gateway_secret_is_a_clean_not_available_for_the_model() {
    let server = instagram().await;
    let h = owner_harness(&server, false).await;
    let d = h.chat("show me instagram stories of @natgeo").await;
    assert_eq!(d["action"], "re_plugins", "{d}");
    let seen: String = h
        .fake
        .requests()
        .iter()
        .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].to_string())
        .collect();
    assert!(seen.contains("Instagram is not available"), "{seen}");
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no request without the secret"
    );
}

#[tokio::test]
async fn a_lucina_mcp_caller_never_gets_the_plugin_tools() {
    let server = instagram().await;
    let h = owner_harness(&server, true).await;
    let (s, body) = h
        .post(
            "/chat",
            json!({"message": "show me the latest instagram stories of @natgeo",
                   "sessionId": "mcp-grok", "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200, "{body}");
    assert_ne!(body["data"]["action"], "re_plugins", "{body}");
    for r in h.fake.requests() {
        let names = tool_names(&r);
        assert!(
            names
                .iter()
                .all(|n| !lucina_tools::OWNER_ONLY_TOOLS.contains(&n.as_str())),
            "{names:?}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unrelated_owner_message_does_not_start_a_plugin_turn() {
    let server = instagram().await;
    let h = owner_harness(&server, true).await;
    let d = h.chat("che tempo fa domani?").await;
    assert_ne!(d["action"], "re_plugins", "{d}");
    assert!(server.received_requests().await.unwrap().is_empty());
}
