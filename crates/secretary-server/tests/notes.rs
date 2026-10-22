//! `/v1/notes*` on the real router and real files: create, find by slug,
//! search, chunked appends, big bodies, the trash, persistence across a
//! restart, CSRF for cookie callers and the service token for Lucina.

use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct H {
    base: String,
    stop: Option<oneshot::Sender<()>>,
    http: Client,
    cfg: Config,
    token: String,
    service: String,
    _dir: tempfile::TempDir,
}

async fn start(cfg: &Config) -> (String, oneshot::Sender<()>) {
    let state = AppState::new(cfg).expect("state");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_on(listener, app(state), async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    (base, tx)
}

impl H {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.rate_limit_per_minute = 10_000;
        cfg.login_rate_per_minute = 10_000;
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let service = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let (base, stop) = start(&cfg).await;
        Self {
            base,
            stop: Some(stop),
            http: Client::builder().pool_max_idle_per_host(0).build().unwrap(),
            cfg,
            token,
            service,
            _dir: dir,
        }
    }

    async fn restart(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (base, stop) = start(&self.cfg).await;
        self.base = base;
        self.stop = Some(stop);
    }

    fn req(&self, m: Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{path}", self.base))
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut rb = self
            .req(m, path)
            .bearer_auth(&self.token)
            .header("x-secretary-client", "ios");
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        self.send(rb).await
    }

    async fn cookie(&self) -> String {
        let r = self
            .req(Method::POST, "/v1/session")
            .json(&json!({"token": self.token}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let c = r.headers()["set-cookie"].to_str().unwrap().to_owned();
        c.split(';').next().unwrap().to_owned()
    }
}

#[tokio::test]
async fn create_find_search_and_persist() {
    let mut h = H::new().await;
    let (s, v) = h.call(Method::GET, "/v1/notes", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"notes": []})));

    let (s, a) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({
                "title": "CRAFT-IT4SD — meeting with Marianne & Jonas",
                "slug": "craftit-meeting",
                "content": "# Briefing\n\nMarianne (Aarhus University) offers paid hours.",
                "tags": ["Work", "aarhus"],
                "source": "chatgpt",
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(a["slug"], "craftit-meeting");
    assert_eq!(a["source"], "chatgpt");
    assert_eq!(a["tags"], json!(["work", "aarhus"]));
    assert_eq!(a["pinned"], false);
    assert!(a["id"].as_str().unwrap().starts_with("n_"));
    assert_eq!(a["createdAt"], a["updatedAt"]);

    let (s, b) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Shopping ideas", "content": "pasta, olive oil", "pinned": true})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["slug"], "shopping-ideas");
    assert_eq!(b["source"], "owner", "the app's default source");

    // By slug or id.
    let (s, v) = h.call(Method::GET, "/v1/notes/craftit-meeting", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["id"], a["id"]);
    let id = a["id"].as_str().unwrap().to_owned();
    let (_, v) = h.call(Method::GET, &format!("/v1/notes/{id}"), None).await;
    assert_eq!(v["slug"], "craftit-meeting");
    let (s, v) = h.call(Method::GET, "/v1/notes/nope", None).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("note_not_found"))
    );

    // List: pinned first, no content, an excerpt; search; tag filter.
    let (_, v) = h.call(Method::GET, "/v1/notes", None).await;
    let notes = v["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["slug"], "shopping-ideas");
    assert!(notes[0].get("content").is_none());
    assert_eq!(notes[0]["excerpt"], "pasta, olive oil");
    let (_, v) = h
        .call(Method::GET, "/v1/notes?q=aarhus%20university", None)
        .await;
    let hits = v["notes"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("Aarhus University"));
    let (_, v) = h.call(Method::GET, "/v1/notes?tag=work", None).await;
    assert_eq!(v["notes"].as_array().unwrap().len(), 1);
    let (_, v) = h.call(Method::GET, "/v1/notes?q=craftit", None).await;
    assert_eq!(v["notes"].as_array().unwrap().len(), 1, "slug matches");

    // Slug taken; bad fields.
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "x", "slug": "craftit-meeting"})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("slug_taken"))
    );
    for bad in [
        json!({"content": "no title"}),
        json!({"title": "x", "colour": "red"}),
        json!({"title": "x", "slug": "Not A Slug"}),
        json!({"title": "x", "tags": "work"}),
    ] {
        let (s, v) = h.call(Method::POST, "/v1/notes", Some(bad.clone())).await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} → {s} {v}"
        );
    }

    // Patch with a version check.
    let (s, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/craftit-meeting",
            Some(json!({"pinned": true, "baseUpdatedAt": a["updatedAt"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["pinned"], true);
    assert_eq!(v["content"], a["content"]);
    let (s, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/craftit-meeting",
            Some(json!({"title": "stale", "baseUpdatedAt": "2020-01-01T00:00:00Z"})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );

    h.restart().await;
    let (_, v) = h.call(Method::GET, "/v1/notes/craftit-meeting", None).await;
    assert_eq!(v["pinned"], true, "persisted across a restart");
    use std::os::unix::fs::PermissionsExt;
    let file = h.cfg.data_dir.join("notes").join(format!("{id}.json"));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn long_documents_and_appends() {
    let h = H::new().await;
    // ~250 KB of quotes: 500 KB of JSON, above the general 256 KB body limit.
    let quoted = "\"".repeat(250 * 1024);
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Quotes", "content": quoted})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", v["error"]);
    assert_eq!(v["content"].as_str().unwrap().len(), 250 * 1024);

    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Too big", "content": "x".repeat(256 * 1024 + 1)})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("note_too_large"))
    );

    // A document sent in chunks.
    let (s, _) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Doc", "slug": "doc", "content": "Part 1 ends mid-wo"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes/doc/append",
            Some(json!({"content": "rd. Part 2."})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["content"], "Part 1 ends mid-word. Part 2.");
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes/doc/append",
            Some(json!({"content": "## Part 3", "separator": "\n\n"})),
        )
        .await;
    assert_eq!(v["content"], "Part 1 ends mid-word. Part 2.\n\n## Part 3");
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes/doc/append",
            Some(json!({"content": "x".repeat(256 * 1024)})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("note_too_large"))
    );
    let (s, _) = h
        .call(
            Method::POST,
            "/v1/notes/missing/append",
            Some(json!({"content": "x"})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn trash_and_restore() {
    let h = H::new().await;
    let (_, a) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Alpha", "content": "one"})),
        )
        .await;
    let id = a["id"].as_str().unwrap().to_owned();
    let (s, v) = h.call(Method::DELETE, "/v1/notes/alpha", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["deleted"], true);
    assert!(v["purgeAfter"].as_str().is_some());
    let (s, _) = h.call(Method::GET, "/v1/notes/alpha", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, v) = h.call(Method::GET, "/v1/notes/trash", None).await;
    assert_eq!(v["days"], 30);
    assert_eq!(v["notes"][0]["id"], json!(id));
    assert!(v["notes"][0]["deletedAt"].is_string());
    let (s, v) = h
        .call(Method::POST, &format!("/v1/notes/trash/{id}/restore"), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["slug"].as_str(), v["content"].as_str()),
        (Some("alpha"), Some("one"))
    );
    assert!(v.get("deletedAt").is_none());
    let (_, v) = h.call(Method::GET, "/v1/notes/trash", None).await;
    assert_eq!(v["notes"], json!([]));
    let (s, _) = h
        .call(
            Method::POST,
            "/v1/notes/trash/n_00000000000000000000/restore",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn auth_csrf_and_the_service_token() {
    let h = H::new().await;
    let (s, _) = h.send(h.req(Method::GET, "/v1/notes")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = h
        .send(h.req(Method::GET, "/v1/notes").bearer_auth("sec_nope"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Browser: the cookie reads freely, writes need the CSRF header.
    let cookie = h.cookie().await;
    let (s, _) = h
        .send(h.req(Method::GET, "/v1/notes").header("cookie", &cookie))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .send(
            h.req(Method::POST, "/v1/notes")
                .header("cookie", &cookie)
                .json(&json!({"title": "web note"})),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("csrf"))
    );
    let (s, v) = h
        .send(
            h.req(Method::POST, "/v1/notes")
                .header("cookie", &cookie)
                .header("x-secretary-csrf", "1")
                .header("x-secretary-client", "web")
                .json(&json!({"title": "web note"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["source"], "owner");

    // Lucina: the service token reads and writes notes; her default source is lucina.
    let (s, v) = h
        .send(
            h.req(Method::POST, "/v1/notes")
                .bearer_auth(&h.service)
                .json(&json!({"title": "Remembered", "content": "x"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["source"], "lucina");
    let (s, v) = h
        .send(
            h.req(Method::GET, "/v1/notes?q=web")
                .bearer_auth(&h.service),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["notes"].as_array().unwrap().len(), 1);
    let (s, _) = h
        .send(
            h.req(Method::GET, "/v1/notes")
                .bearer_auth("svc_wrong_wrong_wrong_wrong_wrong_wrong"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[test]
fn the_repo_seed_notes_import() {
    use secretary_server::cli::{run_notes, NotesAction};
    let seeds = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/seed/notes");
    let dir = tempfile::tempdir().unwrap();
    let mut out = Vec::new();
    run_notes(
        Some(dir.path().to_owned()),
        NotesAction::Import {
            paths: vec![seeds.clone()],
            replace: false,
        },
        &mut out,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("created craftit-meeting"), "{text}");
    let store = secretary_server::notes::NoteStore::new(dir.path());
    let n = store.get("craftit-meeting").unwrap();
    assert_eq!(n.title, "CRAFT-IT4SD — meeting with Marianne & Jonas");
    assert_eq!((n.source.as_str(), n.pinned), ("chatgpt", true));
    assert!(n.content.starts_with("# CRAFT-IT4SD"));
    for must in [
        "We would of course pay your hours",
        "Jonas Kasper Jensen",
        "Next Life",
        "100–180 ore",
        "31/12/2026",
        "subject to AU approval",
    ] {
        assert!(n.content.contains(must), "{must}");
    }
    assert!(
        !n.content.contains("[via Lucina MCP") && !n.content.contains("Lucina, Giuseppe mi chiede")
    );
    // A second import keeps it; --replace overwrites.
    let mut out = Vec::new();
    run_notes(
        Some(dir.path().to_owned()),
        NotesAction::Import {
            paths: vec![seeds.clone()],
            replace: false,
        },
        &mut out,
    )
    .unwrap();
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("kept craftit-meeting"));
    let mut out = Vec::new();
    run_notes(
        Some(dir.path().to_owned()),
        NotesAction::Import {
            paths: vec![seeds],
            replace: true,
        },
        &mut out,
    )
    .unwrap();
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("replaced craftit-meeting"));
    let mut out = Vec::new();
    run_notes(Some(dir.path().to_owned()), NotesAction::List, &mut out).unwrap();
    let list = String::from_utf8(out).unwrap();
    assert!(
        list.contains("craftit-meeting") && list.contains("[pinned]"),
        "{list}"
    );
}
