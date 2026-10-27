//! `/v1/notes*` on the real router and real files: create, find by slug,
//! search, chunked appends, big bodies, the trash, persistence across a
//! restart, CSRF for cookie callers and the service token for Lucina, and
//! Lucina Keep: checklists, labels, `changes`, attachments, batch and the
//! reminders made through a mock Lucina (docs/keep.md).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method as AxMethod, Uri};
use axum::response::{IntoResponse, Response};
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
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

/// What the mock Lucina saw: method, path, body.
type SeenCall = (String, String, Value);

/// A mock Lucina (docs/secretary-api.md reminders): records requests, can be
/// switched off (answers 500) and can claim a reminder is already gone.
#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<SeenCall>>,
    down: AtomicBool,
    next: AtomicUsize,
    gone: Mutex<HashSet<String>>,
}

impl Mock {
    fn calls(&self) -> Vec<SeenCall> {
        self.seen.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }
}

fn lucina_ok(status: StatusCode, data: Value) -> Response {
    (status, axum::Json(json!({"ok": true, "data": data}))).into_response()
}

fn lucina_fail(status: StatusCode, code: &str) -> Response {
    (
        status,
        axum::Json(json!({"ok": false, "error": code, "message": code})),
    )
        .into_response()
}

async fn mock_lucina(
    State(m): State<Arc<Mock>>,
    method: AxMethod,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.seen
        .lock()
        .unwrap()
        .push((method.to_string(), path.clone(), body.clone()));
    if m.down.load(Ordering::SeqCst) {
        return lucina_fail(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    }
    let reminder = |id: &str, status: &str| {
        json!({"id": id, "text": body["text"], "dueAt": body["dueAt"], "timeZone": "Europe/Rome",
               "status": status, "source": "secretary", "createdAt": "2026-10-21T15:02:11.481Z",
               "sentAt": null, "cancelledAt": null})
    };
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/secretary/reminders") => {
            let id = format!("r{}", m.next.fetch_add(1, Ordering::SeqCst) + 1);
            lucina_ok(
                StatusCode::CREATED,
                json!({"created": true, "reminder": reminder(&id, "pending")}),
            )
        }
        ("DELETE", p) if p.starts_with("/api/secretary/reminders/") => {
            let id = p.rsplit('/').next().unwrap();
            if m.gone.lock().unwrap().contains(id) {
                lucina_fail(StatusCode::NOT_FOUND, "reminder_not_found")
            } else {
                lucina_ok(
                    StatusCode::OK,
                    json!({"cancelled": true, "reminder": reminder(id, "cancelled")}),
                )
            }
        }
        _ => lucina_ok(StatusCode::OK, json!({})),
    }
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
    /// Without a Lucina (reminders answer 503 `lucina_not_configured`).
    async fn new() -> Self {
        Self::build(false).await
    }

    /// With a mock Lucina behind the server's token.
    async fn with_lucina() -> Self {
        Self::build(true).await
    }

    async fn build(lucina: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(Mock::default());
        let mut cfg = if lucina {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let router = axum::Router::new()
                .fallback(mock_lucina)
                .with_state(mock.clone());
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Config::new(url, Some("lucina-server-secret".into()))
        } else {
            Config::new("http://127.0.0.1:9", None)
        };
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
            mock,
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

    /// A raw body (attachment uploads) with extra headers; returns status,
    /// headers and bytes.
    async fn raw(
        &self,
        m: Method,
        path: &str,
        ctype: Option<&str>,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
        let mut rb = self
            .req(m, path)
            .bearer_auth(&self.token)
            .header("x-secretary-client", "web");
        if let Some(c) = ctype {
            rb = rb.header("content-type", c);
        }
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        let r = rb.body(body).send().await.unwrap();
        let (s, h) = (r.status(), r.headers().clone());
        (s, h, r.bytes().await.unwrap().to_vec())
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
        json!({}),
        json!({"title": ""}),
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

// ---------------------------------------------------------------- Lucina Keep

fn err_code(v: &Value) -> Option<&str> {
    v["error"]["code"].as_str()
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-test-image-";

#[tokio::test]
async fn keep_fields_checklists_and_untitled_notes() {
    let h = H::new().await;
    // Untitled: content alone is a note; nothing at all is not.
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"content": "Call the dentist tomorrow morning about the crown"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["title"], "");
    assert_eq!(v["slug"], "call-the-dentist-tomorrow-morning-about");
    assert_eq!(
        (v["color"].as_str(), v["archived"].clone()),
        (Some("default"), json!(false))
    );
    assert!(v["position"].as_f64().unwrap() > 1e12);
    assert!(v["rev"].as_u64().unwrap() >= 1);
    assert!(v.get("items").is_none() && v.get("reminder").is_none() && v.get("import").is_none());
    assert_eq!(v["attachments"], json!([]));
    let (s, v) = h
        .call(Method::POST, "/v1/notes", Some(json!({"content": ""})))
        .await;
    assert_eq!(
        (s, v["error"]["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("title: must not be empty"))
    );

    // A list note.
    let (s, l) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({
                "title": "Groceries", "color": "Teal", "pinned": true,
                "items": [{"text": "milk"}, {"text": "eggs", "checked": true}, {"text": "oat", "indent": 1}],
                "labels": ["Shop", "#Home", "SHOP"],
                "collaborators": ["a@example.com"],
                "annotations": [{"url": "https://example.com/x", "title": "X"}],
                "position": 42.5,
            })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{l}");
    assert_eq!(l["content"], "- [ ] milk\n- [x] eggs\n  - [ ] oat");
    assert_eq!(l["color"], "teal");
    assert_eq!(l["tags"], json!(["shop", "home"]));
    assert_eq!(l["position"], 42.5);
    assert_eq!(
        l["items"][1],
        json!({"id": l["items"][1]["id"], "text": "eggs", "checked": true, "indent": 0})
    );
    assert!(l["items"][0]["id"].as_str().unwrap().starts_with("i_"));
    let key = l["id"].as_str().unwrap().to_owned();

    // The old way still works: PATCH content parses back into items.
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"content": "- [x] milk\n- [ ] eggs\nbread", "rev": 1, "import": {"x": 1}})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let texts: Vec<(String, bool)> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["text"].as_str().unwrap().to_owned(),
                i["checked"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        texts,
        [
            ("milk".to_owned(), true),
            ("eggs".to_owned(), false),
            ("bread".to_owned(), false)
        ]
    );
    assert_eq!(v["items"][0]["id"], l["items"][0]["id"], "ids survive");
    assert!(v.get("import").is_none(), "server-set fields are ignored");
    // Append on a list note.
    let (_, v) = h
        .call(
            Method::POST,
            &format!("/v1/notes/{key}/append"),
            Some(json!({"content": "- [x] coffee"})),
        )
        .await;
    assert_eq!(v["items"].as_array().unwrap().len(), 4);
    assert_eq!(v["items"][3]["checked"], true);
    // Summary fields.
    let (_, sm) = h
        .call(
            Method::GET,
            "/v1/notes?type=list&color=teal&archived=exclude",
            None,
        )
        .await;
    let n = &sm["notes"][0];
    assert_eq!(
        (
            n["isList"].clone(),
            n["itemsTotal"].clone(),
            n["itemsDone"].clone(),
            n["color"].clone(),
            n["labels"].clone()
        ),
        (
            json!(true),
            json!(4),
            json!(2),
            json!("teal"),
            json!(["Shop", "Home"])
        )
    );
    assert!(n["rev"].as_u64().unwrap() > 0 && n["position"].is_number());
    assert_eq!(
        (n["attachmentCount"].clone(), n["hasImage"].clone()),
        (json!(0), json!(false))
    );
    assert!(n.get("reminderAt").is_none());
    // items: null is refused; convert works; unknown values are refused.
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"items": null})),
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (_, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"convert": "text"})),
        )
        .await;
    assert!(v.get("items").is_none());
    assert_eq!(v["content"], "milk\neggs\nbread\ncoffee");
    let (_, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"convert": "list"})),
        )
        .await;
    assert_eq!(v["items"].as_array().unwrap().len(), 4);
    for bad in [
        json!({"convert": "pdf"}),
        json!({"color": "mauve"}),
        json!({"items": [{"text": "a", "indent": 3}]}),
        json!({"reminder": {"at": "soon"}}),
    ] {
        let (s, v) = h
            .call(
                Method::PATCH,
                &format!("/v1/notes/{key}"),
                Some(bad.clone()),
            )
            .await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} → {s} {v}"
        );
    }
    // Widened label rules: spaces, slashes and unicode are fine, ',' is not.
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"tags": ["Fisica 2", "a/b", "ünï"]})),
        )
        .await;
    assert_eq!(
        (s, v["tags"].clone()),
        (StatusCode::OK, json!(["fisica 2", "a/b", "ünï"]))
    );
    let (s, _) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"tags": ["a,b"]})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Reserved words are not slugs and not found as notes.
    for w in [
        "trash",
        "labels",
        "changes",
        "attachments",
        "import",
        "batch",
    ] {
        let (s, v) = h
            .call(
                Method::POST,
                "/v1/notes",
                Some(json!({"title": "x", "slug": w})),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{w}: {v}");
    }
    let (_, v) = h
        .call(Method::POST, "/v1/notes", Some(json!({"title": "Labels"})))
        .await;
    assert_eq!(v["slug"], "labels-note");
}

#[tokio::test]
async fn label_endpoints() {
    let h = H::new().await;
    let post = |t: &str, tags: Value| json!({"title": t, "tags": tags});
    let (_, a) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(post("a", json!(["GodLevel", "Fisica 2"]))),
        )
        .await;
    h.call(
        Method::POST,
        "/v1/notes",
        Some(post("b", json!(["godlevel"]))),
    )
    .await;
    let (s, v) = h.call(Method::GET, "/v1/notes/labels", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["labels"],
        json!([{"tag": "fisica 2", "name": "Fisica 2", "count": 1}, {"tag": "godlevel", "name": "GodLevel", "count": 2}])
    );
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes/labels",
            Some(json!({"name": "  #Ideas "})),
        )
        .await;
    assert_eq!(
        (s, v.clone()),
        (
            StatusCode::CREATED,
            json!({"tag": "ideas", "name": "Ideas", "count": 0})
        )
    );
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes/labels",
            Some(json!({"name": "IDEAS"})),
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::CONFLICT, Some("label_exists"))
    );
    let (s, _) = h
        .call(Method::POST, "/v1/notes/labels", Some(json!({"name": ""})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Rename display only (the tag has a space: percent-encoded in the path).
    let (s, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/labels/fisica%202",
            Some(json!({"name": "FISICA 2"})),
        )
        .await;
    assert_eq!(
        (s, v.clone()),
        (
            StatusCode::OK,
            json!({"tag": "fisica 2", "name": "FISICA 2", "count": 1})
        )
    );
    // Rename the tag: notes follow, a merge keeps the other label's name.
    let (s, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/labels/ideas",
            Some(json!({"name": "Plans"})),
        )
        .await;
    assert_eq!((s, v["tag"].clone()), (StatusCode::OK, json!("plans")));
    let (_, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/labels/godlevel",
            Some(json!({"name": "plans"})),
        )
        .await;
    assert_eq!(
        (v["name"].clone(), v["count"].clone()),
        (json!("Plans"), json!(2))
    );
    let (_, n) = h
        .call(
            Method::GET,
            &format!("/v1/notes/{}", a["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(n["tags"], json!(["plans", "fisica 2"]));
    let (s, v) = h
        .call(
            Method::PATCH,
            "/v1/notes/labels/nope",
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::NOT_FOUND, Some("label_not_found"))
    );
    // Delete: out of every note.
    let (s, v) = h.call(Method::DELETE, "/v1/notes/labels/plans", None).await;
    assert_eq!(
        (s, v.clone()),
        (StatusCode::OK, json!({"ok": true, "removed": 2}))
    );
    let (_, n) = h
        .call(
            Method::GET,
            &format!("/v1/notes/{}", a["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(n["tags"], json!(["fisica 2"]));
    let (_, v) = h.call(Method::GET, "/v1/notes/labels", None).await;
    assert_eq!(v["labels"].as_array().unwrap().len(), 1);
    let (s, _) = h.call(Method::DELETE, "/v1/notes/labels/plans", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // The registry survives a restart; so does the label of an empty note.
    let mut h = h;
    h.restart().await;
    let (_, v) = h.call(Method::GET, "/v1/notes/labels", None).await;
    assert_eq!(
        v["labels"],
        json!([{"tag": "fisica 2", "name": "FISICA 2", "count": 1}])
    );
}

#[tokio::test]
async fn changes_feed_trash_endpoints_and_tombstones() {
    let h = H::new().await;
    let (s, v) = h.call(Method::GET, "/v1/notes/changes", None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"notes": [], "removed": [], "cursor": "0", "hasMore": false})
        )
    );
    let mut ids = Vec::new();
    for t in ["a", "b", "c", "d", "e"] {
        let (_, v) = h
            .call(Method::POST, "/v1/notes", Some(json!({"title": t})))
            .await;
        ids.push(v["id"].as_str().unwrap().to_owned());
    }
    // Paging.
    let (_, p1) = h.call(Method::GET, "/v1/notes/changes?limit=2", None).await;
    assert_eq!(
        (
            p1["notes"].as_array().unwrap().len(),
            p1["hasMore"].clone(),
            p1["cursor"].clone()
        ),
        (2, json!(true), json!("2"))
    );
    let (_, p2) = h
        .call(Method::GET, "/v1/notes/changes?since=2&limit=2", None)
        .await;
    assert_eq!(
        (
            p2["notes"][0]["title"].clone(),
            p2["cursor"].clone(),
            p2["hasMore"].clone()
        ),
        (json!("c"), json!("4"), json!(true))
    );
    let (_, p3) = h
        .call(Method::GET, "/v1/notes/changes?since=4&limit=2", None)
        .await;
    assert_eq!(
        (
            p3["notes"].as_array().unwrap().len(),
            p3["cursor"].clone(),
            p3["hasMore"].clone()
        ),
        (1, json!("5"), json!(false))
    );
    let (_, p4) = h.call(Method::GET, "/v1/notes/changes?since=5", None).await;
    assert_eq!(
        (p4["notes"].clone(), p4["cursor"].clone()),
        (json!([]), json!("5"))
    );
    // Archive, trash, restore, delete forever, empty the trash.
    h.call(
        Method::PATCH,
        &format!("/v1/notes/{}", ids[0]),
        Some(json!({"archived": true})),
    )
    .await;
    h.call(Method::DELETE, &format!("/v1/notes/{}", ids[1]), None)
        .await;
    h.call(Method::DELETE, &format!("/v1/notes/{}", ids[2]), None)
        .await;
    let (_, c) = h.call(Method::GET, "/v1/notes/changes?since=5", None).await;
    assert_eq!(c["notes"].as_array().unwrap().len(), 3);
    assert_eq!(c["notes"][0]["archived"], true);
    assert!(
        c["notes"][1]["deletedAt"].is_string(),
        "trashed notes arrive in full, with deletedAt"
    );
    let cursor = c["cursor"].as_str().unwrap().to_owned();
    // DELETE /trash/{id}: forever.
    let (s, v) = h
        .call(Method::DELETE, &format!("/v1/notes/trash/{}", ids[1]), None)
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({"ok": true, "removed": 1})));
    let (s, v) = h
        .call(Method::DELETE, &format!("/v1/notes/trash/{}", ids[1]), None)
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::NOT_FOUND, Some("note_not_found"))
    );
    let (s, v) = h
        .call(Method::DELETE, &format!("/v1/notes/trash/{}", ids[3]), None)
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::NOT_FOUND, Some("note_not_found")),
        "a live note is not in the trash"
    );
    let (_, c) = h
        .call(
            Method::GET,
            &format!("/v1/notes/changes?since={cursor}"),
            None,
        )
        .await;
    assert_eq!(
        (c["removed"].clone(), c["notes"].clone()),
        (json!([ids[1]]), json!([]))
    );
    // DELETE /trash: empty it.
    let (s, v) = h.call(Method::DELETE, "/v1/notes/trash", None).await;
    assert_eq!((s, v), (StatusCode::OK, json!({"ok": true, "removed": 1})));
    let (_, t) = h.call(Method::GET, "/v1/notes/trash", None).await;
    assert_eq!(t["notes"], json!([]));
    let (_, c) = h
        .call(
            Method::GET,
            &format!("/v1/notes/changes?since={}", c["cursor"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(c["removed"], json!([ids[2]]));
    // Everything from the start: live notes, both removals.
    let (_, all) = h
        .call(Method::GET, "/v1/notes/changes?limit=500", None)
        .await;
    assert_eq!(all["notes"].as_array().unwrap().len(), 3);
    assert_eq!(all["removed"].as_array().unwrap().len(), 2);
    // Bad and foreign cursors.
    let (s, v) = h
        .call(Method::GET, "/v1/notes/changes?since=abc", None)
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (s, v) = h
        .call(Method::GET, "/v1/notes/changes?since=99999", None)
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::GONE, Some("cursor_expired"))
    );
    // The counter survives a restart and keeps counting.
    let mut h = h;
    let before = all["cursor"].as_str().unwrap().parse::<u64>().unwrap();
    h.restart().await;
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "after restart"})),
        )
        .await;
    assert_eq!(v["rev"].as_u64().unwrap(), before + 1);
    let (_, c) = h
        .call(
            Method::GET,
            &format!("/v1/notes/changes?since={before}"),
            None,
        )
        .await;
    assert_eq!(c["notes"].as_array().unwrap().len(), 1);
    // Lucina's service token reads the feed too.
    let (s, _) = h
        .send(
            h.req(Method::GET, "/v1/notes/changes")
                .bearer_auth(&h.service),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn attachments_over_http() {
    let h = H::new().await;
    let (_, a) = h
        .call(Method::POST, "/v1/notes", Some(json!({"title": "photos"})))
        .await;
    let (_, b) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "more photos"})),
        )
        .await;
    let (ka, kb) = (
        a["id"].as_str().unwrap().to_owned(),
        b["id"].as_str().unwrap().to_owned(),
    );
    let (s, _, body) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments?name=Holiday%20pic.png"),
            Some("image/png"),
            PNG.to_vec(),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
    let n: Value = serde_json::from_slice(&body).unwrap();
    let att = n["attachments"][0].clone();
    assert_eq!(att["id"].as_str().unwrap().len(), 64);
    assert_eq!(
        att["file"],
        json!(format!("{}.png", att["id"].as_str().unwrap()))
    );
    assert_eq!(
        (
            att["mimetype"].clone(),
            att["size"].clone(),
            att["name"].clone()
        ),
        (
            json!("image/png"),
            json!(PNG.len()),
            json!("Holiday pic.png")
        )
    );
    assert!(att["createdAt"].is_string());
    let file = att["file"].as_str().unwrap().to_owned();
    // The same bytes on another note: one file on disk.
    let (s, _, _) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{kb}/attachments"),
            Some("image/png"),
            PNG.to_vec(),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let adir = h.cfg.data_dir.join("notes/attachments");
    assert_eq!(std::fs::read_dir(&adir).unwrap().count(), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(adir.join(&file))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // Serve: bytes, caching headers, ETag, Range, 304.
    let (s, hd, body) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{file}"),
            None,
            vec![],
            &[],
        )
        .await;
    assert_eq!((s, body.as_slice()), (StatusCode::OK, PNG));
    assert_eq!(hd["content-type"], "image/png");
    assert_eq!(hd["cache-control"], "private, max-age=31536000, immutable");
    assert_eq!(hd["x-content-type-options"], "nosniff");
    let etag = hd["etag"].to_str().unwrap().to_owned();
    assert_eq!(hd["accept-ranges"], "bytes");
    let (s, hd, body) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{file}"),
            None,
            vec![],
            &[("range", "bytes=2-5")],
        )
        .await;
    assert_eq!(
        (s, body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &PNG[2..=5])
    );
    assert_eq!(
        hd["content-range"].to_str().unwrap(),
        format!("bytes 2-5/{}", PNG.len())
    );
    let (s, _, body) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{file}"),
            None,
            vec![],
            &[("range", "bytes=-3")],
        )
        .await;
    assert_eq!(
        (s, body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &PNG[PNG.len() - 3..])
    );
    let (s, hd, _) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{file}"),
            None,
            vec![],
            &[("range", "bytes=999-")],
        )
        .await;
    assert_eq!(
        (s, hd["content-range"].to_str().unwrap().to_owned()),
        (
            StatusCode::RANGE_NOT_SATISFIABLE,
            format!("bytes */{}", PNG.len())
        )
    );
    let (s, _, body) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{file}"),
            None,
            vec![],
            &[("if-none-match", &etag)],
        )
        .await;
    assert_eq!((s, body.len()), (StatusCode::NOT_MODIFIED, 0));
    // File names must match the stored shape; auth applies.
    for bad in [
        "x.png",
        "../x",
        &format!("{}.PNG", &file[..64]),
        &format!("{}.png", "A".repeat(64)),
        &format!("{}.", &file[..64]),
        &format!("{}.toolong", &file[..64]),
    ] {
        let (s, _, _) = h
            .raw(
                Method::GET,
                &format!("/v1/notes/attachments/{bad}"),
                None,
                vec![],
                &[],
            )
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{bad}");
    }
    let missing = format!("{}.png", "0".repeat(64));
    let (s, _, _) = h
        .raw(
            Method::GET,
            &format!("/v1/notes/attachments/{missing}"),
            None,
            vec![],
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .send(h.req(Method::GET, &format!("/v1/notes/attachments/{file}")))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Refusals: type, empty, no content type, too many, size (also above the JSON body limit).
    let (s, _, body) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments"),
            Some("text/html"),
            b"<b>".to_vec(),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        err_code(&serde_json::from_slice(&body).unwrap()),
        Some("unsupported_media_type")
    );
    let (s, _, _) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments"),
            None,
            PNG.to_vec(),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (s, _, _) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments"),
            Some("image/png"),
            vec![],
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = h
        .raw(
            Method::POST,
            "/v1/notes/nope/attachments",
            Some("image/png"),
            PNG.to_vec(),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let three_mb = vec![7u8; 3 * 1024 * 1024];
    let (s, _, body) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments"),
            Some("application/pdf"),
            three_mb,
            &[],
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "uploads may exceed the JSON body limit: {}",
        String::from_utf8_lossy(&body)
    );
    let too_big = vec![9u8; 25 * 1024 * 1024 + 1];
    let (s, _, body) = h
        .raw(
            Method::POST,
            &format!("/v1/notes/{ka}/attachments"),
            Some("application/pdf"),
            too_big,
            &[],
        )
        .await;
    assert_eq!(
        s,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(matches!(
        err_code(&serde_json::from_slice(&body).unwrap()),
        Some("attachment_too_large" | "payload_too_large")
    ));
    // JSON routes keep their smaller limit.
    let (s, _) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "x", "content": "y".repeat(1024 * 1024)})),
        )
        .await;
    assert!(s == StatusCode::PAYLOAD_TOO_LARGE, "{s}");

    // The summary counts; removing from one note keeps the shared file ...
    let (_, l) = h.call(Method::GET, "/v1/notes?type=image", None).await;
    assert_eq!(l["notes"].as_array().unwrap().len(), 2);
    assert_eq!(
        (
            l["notes"][0]["hasImage"].clone(),
            l["notes"][0]["attachmentCount"].clone()
        ),
        (json!(true), l["notes"][0]["attachmentCount"].clone())
    );
    let id = att["id"].as_str().unwrap();
    let (s, v) = h
        .call(
            Method::DELETE,
            &format!("/v1/notes/{ka}/attachments/{id}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["id"] != json!(id)));
    assert!(adir.join(&file).exists());
    let (s, v) = h
        .call(
            Method::DELETE,
            &format!("/v1/notes/{ka}/attachments/{id}"),
            None,
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::NOT_FOUND, Some("attachment_not_found"))
    );
    // ... and delete forever of the last note using it collects the file.
    h.call(Method::DELETE, &format!("/v1/notes/{kb}"), None)
        .await;
    assert!(adir.join(&file).exists(), "the trash still holds it");
    h.call(Method::DELETE, &format!("/v1/notes/trash/{kb}"), None)
        .await;
    assert!(!adir.join(&file).exists());
    // Cookie callers need the CSRF header for uploads too.
    let cookie = h.cookie().await;
    let r = h
        .req(Method::POST, &format!("/v1/notes/{ka}/attachments"))
        .header("cookie", &cookie)
        .header("content-type", "image/png")
        .body(PNG.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn batch_endpoint() {
    let h = H::new().await;
    let mut ids = Vec::new();
    for t in ["a", "b", "c"] {
        let (_, v) = h
            .call(Method::POST, "/v1/notes", Some(json!({"title": t})))
            .await;
        ids.push(v["id"].as_str().unwrap().to_owned());
    }
    let (s, v) = h
        .call(Method::POST, "/v1/notes/batch", Some(json!({"ids": [ids[0], ids[1], "missing"], "set": {"color": "red", "pinned": true, "archived": true}, "addLabels": ["Work", "Ideas"]})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let notes = v["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert!(notes.iter().all(|n| n["color"] == "red"
        && n["pinned"] == true
        && n["archived"] == true
        && n["tags"] == json!(["work", "ideas"])));
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes/batch",
            Some(json!({"ids": [ids[0]], "removeLabels": ["WORK"]})),
        )
        .await;
    assert_eq!(v["notes"][0]["tags"], json!(["ideas"]));
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes/batch",
            Some(json!({"ids": [ids[0], ids[2]], "trash": true})),
        )
        .await;
    assert_eq!(v["notes"].as_array().unwrap().len(), 2);
    assert!(v["notes"][0]["deletedAt"].is_string());
    let (_, l) = h.call(Method::GET, "/v1/notes", None).await;
    assert_eq!(l["notes"].as_array().unwrap().len(), 1);
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes/batch",
            Some(json!({"ids": [ids[0]], "restore": true})),
        )
        .await;
    assert!(v["notes"][0].get("deletedAt").is_none());
    let (_, v) = h
        .call(
            Method::POST,
            "/v1/notes/batch",
            Some(json!({"ids": [ids[2], ids[0]], "deleteForever": true})),
        )
        .await;
    assert_eq!(
        (v["notes"].clone(), v["removed"].clone()),
        (json!([]), json!([ids[2]]))
    );
    // Refused.
    for bad in [
        json!({"ids": [ids[0]], "trash": true, "restore": true}),
        json!({"ids": (0..501).map(|i| format!("n{i}")).collect::<Vec<_>>()}),
        json!({"ids": [ids[0]], "set": {"color": "mauve"}}),
        json!({"ids": [ids[0]], "set": {"reminder": {}}}),
        json!({"ids": [ids[0]], "reminder": {"at": "2030-01-01T00:00:00Z"}}),
    ] {
        let (s, v) = h
            .call(Method::POST, "/v1/notes/batch", Some(bad.clone()))
            .await;
        assert!(
            s == StatusCode::BAD_REQUEST || s == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} → {s} {v}"
        );
    }
}

#[tokio::test]
async fn reminders_go_through_lucina() {
    let h = H::with_lucina().await;
    let at = "2030-05-01T09:00:00+02:00";
    // Create with a reminder: Lucina is asked once, the id is stored.
    let (s, n) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Call the bank", "reminder": {"at": at, "recurrence": "weekly"}})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{n}");
    assert_eq!(
        n["reminder"],
        json!({"at": at, "recurrence": "weekly", "lucinaId": "r1"})
    );
    let calls = h.mock.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (calls[0].0.as_str(), calls[0].1.as_str()),
        ("POST", "/api/secretary/reminders")
    );
    assert_eq!(
        calls[0].2,
        json!({"text": "Keep: Call the bank", "dueAt": at})
    );
    let key = n["id"].as_str().unwrap().to_owned();
    let (_, sm) = h.call(Method::GET, "/v1/notes?type=reminder", None).await;
    assert_eq!(sm["notes"][0]["reminderAt"], at);
    h.mock.clear();

    // Editing something else, or the same moment again, does not touch Lucina.
    h.call(
        Method::PATCH,
        &format!("/v1/notes/{key}"),
        Some(json!({"title": "Call the bank!"})),
    )
    .await;
    let (_, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"reminder": {"at": "2030-05-01T07:00:00Z", "recurrence": "daily"}})),
        )
        .await;
    assert!(h.mock.calls().is_empty());
    assert_eq!(v["reminder"]["lucinaId"], "r1");
    assert_eq!(v["reminder"]["recurrence"], "daily");

    // Changing it: a new reminder, the old one cancelled.
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"reminder": {"at": "2030-06-01T09:00:00Z"}})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["reminder"]["lucinaId"], "r2");
    let calls = h.mock.calls();
    let kinds: Vec<(&str, &str)> = calls.iter().map(|c| (c.0.as_str(), c.1.as_str())).collect();
    assert_eq!(
        kinds,
        [
            ("POST", "/api/secretary/reminders"),
            ("DELETE", "/api/secretary/reminders/r1")
        ]
    );
    assert_eq!(calls[0].2["text"], "Keep: Call the bank!");
    h.mock.clear();

    // Lucina failing: 502, and the note (title included) is unchanged.
    h.mock.down.store(true, Ordering::SeqCst);
    let (_, before) = h.call(Method::GET, &format!("/v1/notes/{key}"), None).await;
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"title": "never saved", "reminder": {"at": "2031-01-01T09:00:00Z"}})),
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (StatusCode::BAD_GATEWAY, Some("lucina_error"))
    );
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"reminder": null})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    let (_, after) = h.call(Method::GET, &format!("/v1/notes/{key}"), None).await;
    assert_eq!(before, after);
    let (s, _) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "no note either", "reminder": {"at": at}})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    let (_, l) = h.call(Method::GET, "/v1/notes", None).await;
    assert_eq!(
        l["notes"].as_array().unwrap().len(),
        1,
        "no note was created"
    );
    h.mock.down.store(false, Ordering::SeqCst);
    h.mock.clear();

    // A validation error never reaches Lucina.
    let (s, _) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"color": "mauve", "reminder": {"at": at}})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"reminder": {"at": at}, "baseUpdatedAt": "2000-01-01T00:00:00Z"})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(h.mock.calls().is_empty());

    // Clearing cancels; a reminder Lucina already forgot is fine.
    h.mock.gone.lock().unwrap().insert("r2".into());
    let (s, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{key}"),
            Some(json!({"reminder": null})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.get("reminder").is_none());
    assert_eq!(h.mock.calls().len(), 1);
    h.mock.clear();

    // Trash cancels it too (also when Lucina is down: the note still goes).
    let (_, n2) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Dentist", "reminder": {"at": at}})),
        )
        .await;
    let k2 = n2["id"].as_str().unwrap().to_owned();
    let lid = n2["reminder"]["lucinaId"].as_str().unwrap().to_owned();
    h.mock.clear();
    let (s, _) = h
        .call(Method::DELETE, &format!("/v1/notes/{k2}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.mock
            .calls()
            .iter()
            .map(|c| (c.0.clone(), c.1.clone()))
            .collect::<Vec<_>>(),
        [(
            "DELETE".to_owned(),
            format!("/api/secretary/reminders/{lid}")
        )]
    );
    let (_, t) = h.call(Method::GET, "/v1/notes/trash", None).await;
    assert_eq!(t["notes"][0]["id"], json!(k2));
    let (_, ch) = h.call(Method::GET, "/v1/notes/changes", None).await;
    let trashed = ch["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == json!(k2))
        .unwrap();
    assert_eq!(trashed["reminder"]["at"], at);
    assert!(
        trashed["reminder"].get("lucinaId").is_none(),
        "no live Lucina reminder any more"
    );
    // Batch trash cancels reminders as well; Lucina down does not block it.
    let (_, n3) = h
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "Third", "reminder": {"at": at}})),
        )
        .await;
    h.mock.clear();
    h.mock.down.store(true, Ordering::SeqCst);
    let (s, v) = h
        .call(
            Method::POST,
            "/v1/notes/batch",
            Some(json!({"ids": [n3["id"]], "trash": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h.mock.calls().len(), 1);
    assert!(v["notes"][0]["deletedAt"].is_string());
    h.mock.down.store(false, Ordering::SeqCst);

    // Restored, the reminder is shown without a live id; setting it again re-arms.
    let (_, r) = h
        .call(Method::POST, &format!("/v1/notes/trash/{k2}/restore"), None)
        .await;
    assert!(r["reminder"].get("lucinaId").is_none());
    h.mock.clear();
    let (_, v) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{k2}"),
            Some(json!({"reminder": {"at": at}})),
        )
        .await;
    assert!(v["reminder"]["lucinaId"].is_string());
    assert_eq!(h.mock.calls().len(), 1);

    // Lucina's own refusal passes through (past time).
    // Without a Lucina at all: 503, notes without reminders are unaffected.
    let plain = H::new().await;
    let (s, v) = plain
        .call(
            Method::POST,
            "/v1/notes",
            Some(json!({"title": "x", "reminder": {"at": at}})),
        )
        .await;
    assert_eq!(
        (s, err_code(&v)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("lucina_not_configured")
        )
    );
    let (s, _) = plain
        .call(Method::POST, "/v1/notes", Some(json!({"title": "x"})))
        .await;
    assert_eq!(s, StatusCode::CREATED);
}

#[tokio::test]
async fn service_token_and_csrf_on_the_new_routes() {
    let h = H::new().await;
    let svc = |m: Method, p: &str| h.req(m, p).bearer_auth(&h.service);
    for (m, p) in [
        (Method::GET, "/v1/notes/labels"),
        (Method::GET, "/v1/notes/changes"),
        (Method::GET, "/v1/notes/trash"),
    ] {
        let (s, _) = h.send(svc(m, p)).await;
        assert_eq!(s, StatusCode::OK, "{p}");
        let (s, _) = h.send(h.req(Method::GET, p)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{p}");
    }
    let cookie = h.cookie().await;
    for (m, p, body) in [
        (Method::POST, "/v1/notes/labels", json!({"name": "x"})),
        (Method::POST, "/v1/notes/batch", json!({"ids": []})),
        (Method::DELETE, "/v1/notes/trash", Value::Null),
    ] {
        let rb = h.req(m.clone(), p).header("cookie", &cookie);
        let rb = if body.is_null() { rb } else { rb.json(&body) };
        let (s, v) = h.send(rb).await;
        assert_eq!(
            (s, err_code(&v)),
            (StatusCode::FORBIDDEN, Some("csrf")),
            "{m} {p}"
        );
        let rb = h
            .req(m, p)
            .header("cookie", &cookie)
            .header("x-secretary-csrf", "1");
        let rb = if body.is_null() { rb } else { rb.json(&body) };
        let (s, v) = h.send(rb).await;
        assert!(s.is_success(), "{p}: {s} {v}");
    }
    // Reads with the cookie alone.
    let (s, _) = h
        .send(
            h.req(Method::GET, "/v1/notes/changes")
                .header("cookie", &cookie),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}
