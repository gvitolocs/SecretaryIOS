//! Shopping v2: the static catalog (ETag / 304) and the list + ops routes
//! against a mock Lucina that keeps op-id idempotency like the real one.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const U1: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const U2: &str = "7f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const U3: &str = "8f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<(String, String, Value)>>,
    queries: Mutex<Vec<String>>,
    applied: Mutex<HashSet<String>>,
    revision: Mutex<u64>,
}

fn ok(status: StatusCode, data: Value) -> Response {
    (status, Json(json!({"ok": true, "data": data}))).into_response()
}

fn fail(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": code, "message": format!("lucina says {code}")})),
    )
        .into_response()
}

fn banana() -> Value {
    json!({"id": "it-1", "itemId": "banana", "catalogId": "banana", "name": "Banane", "typedName": "banana",
           "item": {"id": "banana", "kind": "catalog", "names": {"it": "Banane", "en": "Bananas", "da": "Bananer"},
                    "section": "produce", "icon": "🍌"},
           "spec": "6", "quantity": null,
           "unit": null, "section": "produce", "icon": "🍌", "status": "needed", "position": 0,
           "updatedAt": "2026-10-21T10:00:00.000Z", "updatedBy": "web", "priceHint": 1.2})
}

fn summary(id: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "theme": "home", "needed": 1, "recent": 4,
           "updatedAt": "2026-10-21T10:00:00.000Z", "owner": "giuseppe"})
}

async fn mock_handler(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_owned();
    m.queries
        .lock()
        .unwrap()
        .push(uri.query().unwrap_or("").to_owned());
    m.seen
        .lock()
        .unwrap()
        .push((method.to_string(), path.clone(), body.clone()));
    if path == "/api/secretary/shopping/items" {
        return ok(
            StatusCode::OK,
            json!({"items": [{"id": "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A", "kind": "custom",
                              "names": {"it": "Spugne", "en": "Sponges", "da": "Svampe"},
                              "section": "household", "icon": "🧽", "aliases": ["spugne"], "needsTranslation": false}]}),
        );
    }
    let p = path
        .strip_prefix("/api/secretary/shopping/lists")
        .unwrap_or("?");
    match (method.as_str(), p) {
        // Lucina wraps the array: the server unwraps it.
        ("GET", "") => ok(
            StatusCode::OK,
            json!({"lists": [summary("default", "Casa"), summary("l2", "Ufficio")]}),
        ),
        ("POST", "") => ok(
            StatusCode::CREATED,
            json!({"list": {"id": "l3", "name": body["name"], "theme": body["theme"]}}),
        ),
        ("GET", "/missing") | ("POST", "/missing/ops") => {
            fail(StatusCode::NOT_FOUND, "list_not_found")
        }
        ("GET", "/default") => ok(
            StatusCode::OK,
            json!({"list": {"id": "default", "name": "Casa", "theme": "home"},
                   "revision": *m.revision.lock().unwrap(), "items": [banana()]}),
        ),
        ("PATCH", "/default") => ok(
            StatusCode::OK,
            json!({"id": "default", "name": body["name"], "theme": "home"}),
        ),
        ("DELETE", "/default") => fail(StatusCode::UNPROCESSABLE_ENTITY, "default_list_protected"),
        ("DELETE", "/l2") => ok(StatusCode::OK, json!({"deleted": true})),
        ("POST", "/default/ops") => {
            let ops = body["ops"].as_array().cloned().unwrap_or_default();
            let mut rev = m.revision.lock().unwrap();
            let stale = body["expectedRevision"].as_u64() != Some(*rev);
            if stale && ops.iter().any(|o| o["type"] == "rename") {
                return fail(StatusCode::CONFLICT, "stale_revision");
            }
            let mut applied_set = m.applied.lock().unwrap();
            let (mut applied, mut dups) = (vec![], vec![]);
            for op in &ops {
                let id = op["opId"].as_str().unwrap().to_owned();
                if applied_set.insert(id.clone()) {
                    applied.push(id);
                } else {
                    dups.push(id);
                }
            }
            if !applied.is_empty() {
                *rev += 1;
            }
            ok(
                StatusCode::OK,
                json!({"revision": *rev, "applied": applied, "duplicates": dups,
                       "items": [banana()]}),
            )
        }
        _ => fail(StatusCode::NOT_FOUND, "not_found"),
    }
}

struct H {
    base: String,
    http: reqwest::Client,
    token: String,
    mock: Arc<Mock>,
    catalog: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let mock = Arc::new(Mock::default());
    *mock.revision.lock().unwrap() = 42;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .fallback(mock_handler)
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    cfg.catalog_dir = dir.path().join("catalog");
    std::fs::create_dir_all(&cfg.catalog_dir).unwrap();
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(listener, app(state), std::future::pending())
            .await
            .unwrap()
    });
    H {
        base,
        http: reqwest::Client::new(),
        token,
        mock,
        catalog: cfg.catalog_dir,
        _dir: dir,
    }
}

impl H {
    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (u16, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    fn lucina_calls(&self) -> usize {
        self.mock.seen.lock().unwrap().len()
    }

    fn last_body(&self) -> Value {
        self.mock.seen.lock().unwrap().last().unwrap().2.clone()
    }
}

fn code(b: &Value) -> &str {
    b["error"]["code"].as_str().unwrap_or("<none>")
}

// -- catalog ---------------------------------------------------------------------

#[tokio::test]
async fn catalog_is_served_with_a_strong_etag_and_304() {
    let h = harness().await;
    let it = r#"{"version":1,"lang":"it","sections":[],"items":[{"id":"banana","name":"Banane"}]}"#;
    std::fs::write(h.catalog.join("catalog.it.json"), it).unwrap();
    std::fs::write(
        h.catalog.join("catalog.en.json"),
        r#"{"version":1,"lang":"en"}"#,
    )
    .unwrap();

    let r = h
        .req(Method::GET, "/v1/shopping/catalog?lang=it")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert!(etag.starts_with('"') && !etag.starts_with("W/"), "{etag}");
    assert_eq!(r.headers()["cache-control"], "public, max-age=3600");
    assert_eq!(
        r.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    assert_eq!(r.text().await.unwrap(), it, "bytes served unchanged");

    // default language is it
    let r = h
        .req(Method::GET, "/v1/shopping/catalog")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["etag"].to_str().unwrap(), etag);

    // revalidation
    let r = h
        .req(Method::GET, "/v1/shopping/catalog?lang=it")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    assert_eq!(r.headers()["etag"].to_str().unwrap(), etag);
    assert_eq!(r.headers()["cache-control"], "public, max-age=3600");
    assert!(r.bytes().await.unwrap().is_empty());

    // other language: other tag, so the old one does not match
    let r = h
        .req(Method::GET, "/v1/shopping/catalog?lang=en")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_ne!(r.headers()["etag"].to_str().unwrap(), etag);

    // a changed file changes the tag
    std::fs::write(h.catalog.join("catalog.it.json"), r#"{"version":2}"#).unwrap();
    let r = h
        .req(Method::GET, "/v1/shopping/catalog?lang=it")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // validation, missing file, auth; Lucina never called
    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/catalog?lang=fr"))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/catalog?lang=da"))
        .await;
    assert_eq!((s, code(&b)), (503, "catalog_unavailable"));
    let r = h
        .http
        .get(format!("{}/v1/shopping/catalog", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(h.lucina_calls(), 0);
}

// -- lists -------------------------------------------------------------------------

#[tokio::test]
async fn list_routes_forward_and_keep_unknown_fields() {
    let h = harness().await;
    let (s, b) = h.send(h.req(Method::GET, "/v1/shopping/lists")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b[0]["id"], "default");
    assert_eq!(b[0]["needed"], 1);
    assert_eq!(b[0]["owner"], "giuseppe", "unknown fields pass");
    assert_eq!(b[1]["name"], "Ufficio");

    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists")
                .json(&json!({"name": "Giardino", "theme": "garden"})),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(
        b,
        json!({"id": "l3", "name": "Giardino", "theme": "garden"})
    );
    assert_eq!(
        h.last_body(),
        json!({"name": "Giardino", "theme": "garden"})
    );

    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/lists/default"))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["revision"], 42);
    assert_eq!(b["list"]["theme"], "home");
    assert_eq!(b["items"][0]["catalogId"], "banana");
    assert_eq!(b["items"][0]["priceHint"], 1.2);
    assert_eq!(b["items"][0]["quantity"], Value::Null);

    let (s, b) = h
        .send(
            h.req(Method::PATCH, "/v1/shopping/lists/default")
                .json(&json!({"name": "Casa mia"})),
        )
        .await;
    assert_eq!((s, b["name"].as_str()), (200, Some("Casa mia")));

    let (s, b) = h.send(h.req(Method::DELETE, "/v1/shopping/lists/l2")).await;
    assert_eq!((s, b), (200, json!({"deleted": true})));

    // Lucina 404 / 422 codes pass through
    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/lists/missing"))
        .await;
    assert_eq!((s, code(&b)), (404, "list_not_found"));
    let (s, b) = h
        .send(h.req(Method::DELETE, "/v1/shopping/lists/default"))
        .await;
    assert_eq!((s, code(&b)), (422, "default_list_protected"));

    // a list id must stay one path segment
    let (s, _) = h.send(h.req(Method::GET, "/v1/shopping/lists/a%2Fb")).await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn list_input_is_validated_before_lucina() {
    let h = harness().await;
    let before = h.lucina_calls();
    for (method, path, body) in [
        (Method::POST, "/v1/shopping/lists", json!({"name": " "})),
        (
            Method::POST,
            "/v1/shopping/lists",
            json!({"name": "x".repeat(81)}),
        ),
        (
            Method::POST,
            "/v1/shopping/lists",
            json!({"name": "Casa", "theme": "pool"}),
        ),
        (Method::POST, "/v1/shopping/lists", json!({"theme": "home"})),
        (Method::PATCH, "/v1/shopping/lists/default", json!({})),
        (Method::PATCH, "/v1/shopping/lists/..", json!({"name": "x"})),
    ] {
        let (s, b) = h.send(h.req(method.clone(), path).json(&body)).await;
        assert!(s == 400 || s == 404, "{method} {path} {body}: {s} {b}");
    }
    assert_eq!(h.lucina_calls(), before);
}

// -- ops ---------------------------------------------------------------------------

#[tokio::test]
async fn ops_are_idempotent_end_to_end() {
    let h = harness().await;
    let batch = json!({"expectedRevision": 42, "ops": [
        {"opId": U1, "type": "add", "catalogId": "banana", "name": "Banane", "spec": "6"},
        {"opId": U2, "type": "purchase", "itemId": "it-1"},
    ]});
    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/default/ops")
                .json(&batch),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["revision"], 43);
    assert_eq!(b["applied"], json!([U1, U2]));
    assert_eq!(b["duplicates"], json!([]));
    assert_eq!(b["items"][0]["id"], "it-1");
    let sent = h.last_body();
    assert_eq!(sent["expectedRevision"], 42);
    assert_eq!(sent["ops"][0]["type"], "add");
    assert_eq!(
        sent["ops"][1],
        json!({"type": "purchase", "opId": U2, "itemId": "it-1"})
    );

    // the outbox replays the same batch: nothing changes, both reported as duplicates
    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/default/ops")
                .json(&batch),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["revision"], 43);
    assert_eq!(b["applied"], json!([]));
    assert_eq!(b["duplicates"], json!([U1, U2]));

    // stale revision with a non-conflicting op: Lucina applies it
    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/default/ops").json(
                &json!({"expectedRevision": 1, "ops": [{"opId": U3, "type": "clear_recent"}]}),
            ),
        )
        .await;
    assert_eq!((s, b["applied"].clone()), (200, json!([U3])));
}

#[tokio::test]
async fn conflicts_and_lucina_errors_are_mapped() {
    let h = harness().await;
    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/default/ops")
                .json(&json!({
                    "expectedRevision": 1,
                    "ops": [{"opId": U1, "type": "rename", "itemId": "it-1", "name": "Banane bio"}]
                })),
        )
        .await;
    assert_eq!((s, code(&b)), (409, "revision_conflict"));
    assert_eq!(b["error"]["message"], "lucina says stale_revision");

    let (s, b) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/missing/ops")
                .json(&json!({"ops": [{"opId": U1, "type": "clear_recent"}]})),
        )
        .await;
    assert_eq!((s, code(&b)), (404, "list_not_found"));
}

#[tokio::test]
async fn ops_are_validated_before_lucina() {
    let h = harness().await;
    let many: Vec<Value> = (0..201)
        .map(
            |i| json!({"opId": format!("6f1c2a3b-4d5e-4f60-8a7b-{i:012}"), "type": "clear_recent"}),
        )
        .collect();
    let cases = [
        json!({"ops": []}),
        json!({"ops": many}),
        json!({"ops": [{"opId": "not-a-uuid", "type": "clear_recent"}]}),
        json!({"ops": [{"type": "clear_recent"}]}),
        json!({"ops": [{"opId": U1, "type": "teleport", "itemId": "x"}]}),
        json!({"ops": [{"opId": U1, "type": "add"}]}),
        json!({"ops": [{"opId": U1, "type": "add", "name": "x".repeat(201)}]}),
        json!({"ops": [{"opId": U1, "type": "add", "name": "latte", "spec": "s".repeat(201)}]}),
        json!({"ops": [{"opId": U1, "type": "add", "name": "latte", "unit": "u".repeat(33)}]}),
        json!({"ops": [{"opId": U1, "type": "add", "name": "a\u{0}b"}]}),
        json!({"ops": [{"opId": U1, "type": "purchase", "itemId": "../etc"}]}),
        json!({"ops": [{"opId": U1, "type": "purchase"}]}),
        json!({"ops": [{"opId": U1, "type": "set_quantity", "itemId": "i", "quantity": 0}]}),
        json!({"ops": [{"opId": U1, "type": "reorder", "itemId": "i", "position": -1}]}),
        json!({"ops": [{"opId": U1, "type": "reorder", "itemId": "i", "position": 100001}]}),
        json!({"ops": [{"opId": U1, "type": "move", "itemId": "i", "toListId": "a b"}]}),
        json!({"ops": [{"opId": U1, "type": "clear_recent"}, {"opId": U1, "type": "clear_recent"}]}),
        json!({"expectedRevision": -1, "ops": [{"opId": U1, "type": "clear_recent"}]}),
    ];
    for body in cases {
        let (s, b) = h
            .send(
                h.req(Method::POST, "/v1/shopping/lists/default/ops")
                    .json(&body),
            )
            .await;
        assert_eq!((s, code(&b)), (400, "bad_request"), "{body}: {b}");
    }
    assert_eq!(h.lucina_calls(), 0);

    // cookie writes need the CSRF header like every other write
    let r = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (s, b) = h
        .send(
            h.http
                .post(format!("{}/v1/shopping/lists/default/ops", h.base))
                .header("cookie", &cookie)
                .json(&json!({"ops": [{"opId": U1, "type": "clear_recent"}]})),
        )
        .await;
    assert_eq!((s, code(&b)), (403, "csrf"));
}

#[tokio::test]
async fn item_objects_lang_and_custom_items_pass_through() {
    let h = harness().await;
    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/lists/default?lang=da"))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(h.mock.queries.lock().unwrap().last().unwrap(), "lang=da");
    assert_eq!(b["items"][0]["itemId"], "banana");
    assert_eq!(b["items"][0]["typedName"], "banana");
    assert_eq!(b["items"][0]["item"]["names"]["da"], "Bananer");
    assert_eq!(b["items"][0]["item"]["kind"], "catalog");

    let calls = h.lucina_calls();
    let (s, b) = h
        .send(h.req(Method::GET, "/v1/shopping/lists/default?lang=fr"))
        .await;
    assert_eq!((s, code(&b)), (400, "bad_request"));
    assert_eq!(h.lucina_calls(), calls, "refused before Lucina");

    let (s, _) = h.send(h.req(Method::GET, "/v1/shopping?lang=en")).await;
    assert_ne!(s, 400);
    assert_eq!(h.mock.queries.lock().unwrap().last().unwrap(), "lang=en");

    let (s, b) = h
        .send(h.req(Method::POST, "/v1/shopping/lists/default/ops?lang=en").json(&json!({
            "ops": [{"opId": U1, "type": "add", "text": "spugne"},
                    {"opId": U2, "type": "edit_item", "itemId": "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A",
                     "names": {"en": "Sponges"}, "icon": "🧽"}]
        })))
        .await;
    assert_eq!(s, 200, "{b}");
    let body = h.last_body();
    assert_eq!(body["lang"], "en", "the query lang goes into the body");
    assert_eq!(body["ops"][0]["text"], "spugne");
    assert_eq!(body["ops"][1]["type"], "edit_item");
    assert_eq!(body["ops"][1]["names"]["en"], "Sponges");

    let (s, _) = h
        .send(
            h.req(Method::POST, "/v1/shopping/lists/default/ops?lang=en")
                .json(&json!({
                    "lang": "da", "ops": [{"opId": U3, "type": "add", "itemId": "milk"}]
                })),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(h.last_body()["lang"], "da", "the body lang wins");

    let (s, b) = h.send(h.req(Method::GET, "/v1/shopping/items")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b[0]["names"]["en"], "Sponges");
    assert_eq!(b[0]["aliases"][0], "spugne", "unknown fields pass");
}
