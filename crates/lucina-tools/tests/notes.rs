//! The `notes` tool against a mocked secretary-server, and its local fallback.

use std::sync::Arc;
use std::time::Duration;

use lucina_tools::agent;
use lucina_tools::{Ctx, Effect, Registry, ToolEnv, ToolSelection};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "svc_test_service_token_0123456789abcdef";

fn ctx(dir: &std::path::Path, url: Option<String>) -> Ctx {
    let mut env = ToolEnv::new(dir, "Europe/Rome".parse().unwrap());
    if let Some(u) = url {
        let file = dir.join("service.token");
        std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
        env.secretary_url = Some(u);
        env.secretary_service_token_file = Some(file);
    }
    Ctx::new(Arc::new(env), "web", "app")
}

async fn call(ctx: &Ctx, args: Value) -> (bool, Value) {
    let registry = Registry::builtin(&ToolSelection::parse("notes"));
    let (ok, out, _) = agent::execute(&registry, "notes", args, ctx, Duration::from_secs(10)).await;
    (ok, out)
}

fn note(slug: &str, title: &str, content: &str) -> Value {
    json!({"id": "n_00000000000000000001", "slug": slug, "title": title, "content": content,
           "tags": ["work"], "source": "chatgpt", "pinned": true,
           "createdAt": "2026-10-22T11:30:00.000Z", "updatedAt": "2026-10-22T11:31:00.000Z"})
}

#[tokio::test]
async fn reads_lists_and_searches_secretary_notes() {
    let server = MockServer::start().await;
    let auth = format!("Bearer {TOKEN}");
    Mock::given(method("GET"))
        .and(path("/v1/notes/craftit-meeting"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(note(
            "craftit-meeting",
            "CRAFT-IT4SD — meeting with Marianne & Jonas",
            "# Briefing\n\nMarianne offers paid hours.",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/craft-it4sd-meeting"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("q", "CRAFT-IT4SD meeting"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD — meeting with Marianne & Jonas", "excerpt": "Marianne offers"}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("q", "marianne"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD", "excerpt": "Marianne offers paid hours."}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD", "updatedAt": "2026-10-22T11:31:00.000Z", "source": "chatgpt", "pinned": true, "tags": []}
        ]})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(format!("{}/", server.uri())));

    let (ok, out) = call(&c, json!({"action": "read", "slug": "craftit-meeting"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["content"], "# Briefing\n\nMarianne offers paid hours.");
    assert_eq!(
        (out["source"].as_str(), out["stored"].as_str()),
        (Some("chatgpt"), Some("secretary"))
    );
    // By title: not a slug, so the best search hit.
    let (ok, out) = call(
        &c,
        json!({"action": "read", "title": "CRAFT-IT4SD meeting"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["slug"], "craftit-meeting");
    let (ok, out) = call(&c, json!({"action": "search", "query": "marianne"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["results"][0]["slug"], "craftit-meeting");
    assert_eq!(out["results"][0]["lines"][0], "Marianne offers paid hours.");
    let (ok, out) = call(&c, json!({"action": "list"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["notes"][0]["slug"], "craftit-meeting");
    assert_eq!(out["notes"][0]["pinned"], true);
    assert!(
        !dir.path().join("notes").exists(),
        "nothing written locally"
    );
}

#[tokio::test]
async fn creates_and_appends_on_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .and(body_json(json!({"title": "Idee regalo", "content": "- libro", "source": "lucina", "tags": ["casa"]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(note("idee-regalo", "Idee regalo", "- libro")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes/idee-regalo/append"))
        .and(body_json(
            json!({"content": "- sciarpa", "separator": "\n", "source": "lucina"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(note(
            "idee-regalo",
            "Idee regalo",
            "- libro\n- sciarpa",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes/viaggio/append"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .and(body_json(
            json!({"title": "Viaggio", "content": "Roma", "source": "lucina", "slug": "viaggio"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(note("viaggio", "Viaggio", "Roma")))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Idee regalo", "content": "- libro", "tags": ["casa"]}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(
        (out["slug"].as_str(), out["stored"].as_str()),
        (Some("idee-regalo"), Some("secretary"))
    );
    let (ok, out) = call(
        &c,
        json!({"action": "append", "title": "Idee regalo", "content": "- sciarpa"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["chars"], 17);
    // Appending to a missing note starts it.
    let (ok, out) = call(
        &c,
        json!({"action": "append", "title": "Viaggio", "content": "Roma"}),
    )
    .await;
    assert!(ok, "{out}");
    let saved: Vec<String> = c
        .effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Note { title } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(saved, ["Idee regalo", "Idee regalo", "Viaggio"]);
    assert!(!dir.path().join("notes").exists());
}

#[tokio::test]
async fn refusals_are_reported_and_a_dead_server_falls_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"error": {"code": "slug_taken", "message": "exists"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/missing"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": []})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Dup", "content": "x"}),
    )
    .await;
    assert!(!ok);
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("exists; use append"),
        "{out}"
    );
    let (ok, out) = call(&c, json!({"action": "read", "slug": "missing"})).await;
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("no note"), "{out}");
    assert!(
        !dir.path().join("notes").exists(),
        "a refusal never writes locally"
    );

    // Nothing listening: local files.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(dead));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Offline", "content": "kept here"}),
    )
    .await;
    assert!(ok, "{out}");
    assert!(out.get("stored").is_none());
    assert!(dir.path().join("notes/offline.md").exists());
    let (ok, out) = call(&c, json!({"action": "read", "title": "Offline"})).await;
    assert!(ok);
    assert_eq!(out["content"], "# Offline\n\nkept here\n");
}
