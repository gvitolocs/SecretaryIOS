//! Library endpoints on the real router, storage, auth and pure import merge.
use reqwest::{Client, Method, StatusCode};
use secretary_server::{app, config::Config, devices::DeviceStore, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;
struct H {
    base: String,
    token: String,
    service: String,
    http: Client,
    cfg: Config,
    stop: Option<oneshot::Sender<()>>,
    _dir: tempfile::TempDir,
}
async fn start(cfg: &Config) -> (String, oneshot::Sender<()>) {
    let state = AppState::new(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel();
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
        cfg.data_dir = dir.path().join("data");
        cfg.rate_limit_per_minute = 10_000;
        let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test").unwrap();
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let service = std::fs::read_to_string(path).unwrap().trim().to_owned();
        let (base, stop) = start(&cfg).await;
        Self {
            base,
            token,
            service,
            http: Client::new(),
            cfg,
            stop: Some(stop),
            _dir: dir,
        }
    }
    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap())
    }
    async fn import(&self) -> (String, Value) {
        let (status, v) = self
            .call(Method::POST, "/v1/library/import", Some(batch()))
            .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let (_, list) = self.call(Method::GET, "/v1/library/books", None).await;
        (list["books"][0]["id"].as_str().unwrap().into(), v)
    }
}
impl Drop for H {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}
fn batch() -> Value {
    use ink_library::{
        import::{ImportBatch, ScannedBook},
        model::{DrmState, Format, Metadata},
    };
    let timestamp = chrono::Utc::now()
        .format("%A, %-d %B %Y %H:%M:%S")
        .to_string();
    let text=format!("Café Book (Writer)\n- Your Highlight on page 12 | Location 170-172 | Added on {timestamp}\n\nMemorable café thought.\n==========\n");
    let batch = ImportBatch {
        source: "kindle-usb".into(),
        device: Some("Paperwhite".into()),
        scanned_at: chrono::Utc::now().to_rfc3339(),
        books: vec![ScannedBook {
            path: "documents/Book.azw3".into(),
            size_bytes: 20,
            fingerprint: "a".repeat(64),
            format: Format::Azw3,
            metadata: Metadata {
                title: Some("Café Book".into()),
                authors: vec!["Writer".into()],
                drm: DrmState::Protected,
                ..Default::default()
            },
            sidecar_present: false,
            sidecar_extensions: vec![],
        }],
        clippings: ink_library::clippings::parse(text.as_bytes())
            .unwrap()
            .entries,
    };
    serde_json::to_value(batch).unwrap()
}

#[tokio::test]
async fn auth_service_cookie_csrf_and_malformed() {
    let h = H::new().await;
    let r = h
        .http
        .get(format!("{}/v1/library/books", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = h
        .http
        .get(format!("{}/v1/library/books", h.base))
        .bearer_auth(&h.service)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token":h.token}))
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
    let r = h
        .http
        .post(format!("{}/v1/library/import", h.base))
        .header("cookie", &cookie)
        .json(&batch())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = h
        .http
        .post(format!("{}/v1/library/import", h.base))
        .header("cookie", cookie)
        .header("x-secretary-csrf", "1")
        .json(&batch())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        h.call(
            Method::POST,
            "/v1/library/import",
            Some(json!({"books":"bad"}))
        )
        .await
        .0,
        400
    );
    let mut b = batch();
    b["books"][0]["path"] = json!("../outside");
    assert_eq!(
        h.call(Method::POST, "/v1/library/import", Some(b)).await.0,
        400
    );
    let mut b = batch();
    b["books"][0]["metadata"]["title"] = json!("x".repeat(1001));
    assert_eq!(
        h.call(Method::POST, "/v1/library/import", Some(b)).await.0,
        400
    );
}
#[tokio::test]
async fn idempotent_owner_edits_removal_and_persistence() {
    let mut h = H::new().await;
    let b = batch();
    let (status, o) = h
        .call(Method::POST, "/v1/library/import", Some(b.clone()))
        .await;
    assert_eq!(status, 200);
    assert_eq!(o["booksAdded"], 1);
    let (_, list) = h.call(Method::GET, "/v1/library/books", None).await;
    let id = list["books"][0]["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/library/books/{id}");
    let (_, first) = h.call(Method::GET, &path, None).await;
    assert_eq!(first["book"]["status"], "unknown");
    let (_, o) = h
        .call(Method::POST, "/v1/library/import", Some(b.clone()))
        .await;
    assert_eq!(o["booksUnchanged"], 1);
    assert_eq!(o["booksUpdated"], 0);
    assert_eq!(o["highlightsSkipped"], 1);
    assert_eq!(h.call(Method::GET, &path, None).await.1, first);
    let (status,edited)=h.call(Method::PATCH,&path,Some(json!({"status":"reading","tags":["mine"],"progress":{"percent":12.0,"location":172,"page":12,"source":"client","at":"client"},"baseUpdatedAt":first["book"]["updatedAt"]}))).await;
    assert_eq!(status, 200);
    assert_eq!(edited["progress"]["source"], "owner");
    assert_eq!(
        h.call(
            Method::PATCH,
            &path,
            Some(json!({"status":"completed","baseUpdatedAt":first["book"]["updatedAt"]}))
        )
        .await
        .0,
        409
    );
    h.call(Method::POST, "/v1/library/import", Some(b.clone()))
        .await;
    let (_, book) = h.call(Method::GET, &path, None).await;
    assert_eq!(book["book"]["status"], "reading");
    assert_eq!(book["book"]["tags"], json!(["mine"]));
    let mut removed = b;
    removed["books"] = json!([]);
    removed["scannedAt"] = json!(chrono::Utc::now().to_rfc3339());
    assert_eq!(
        h.call(Method::POST, "/v1/library/import", Some(removed))
            .await
            .1["removedFromKindle"],
        1
    );
    let (_, after) = h.call(Method::GET, &path, None).await;
    assert!(after["book"]["onKindle"].is_null());
    assert!(after["book"]["removedFromKindleAt"].is_string());
    assert_eq!(after["highlights"].as_array().unwrap().len(), 1);
    let _ = h.stop.take().unwrap().send(());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (base, stop) = start(&h.cfg).await;
    h.base = base;
    h.stop = Some(stop);
    assert_eq!(h.call(Method::GET, &path, None).await.1, after);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [
            format!("library/books/{id}.json"),
            format!("library/highlights/{id}.json"),
            "library/imports.jsonl".into(),
            "library/activity.json".into(),
        ] {
            assert_eq!(
                std::fs::metadata(h.cfg.data_dir.join(p))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
#[tokio::test]
async fn search_recent_stats_sync_and_generated_brief() {
    let h = H::new().await;
    let (id, _) = h.import().await;
    let (_, v) = h
        .call(Method::GET, "/v1/library/search?q=memorable", None)
        .await;
    assert_eq!(v["results"][0]["bookId"], id);
    assert!(v["results"][0]["highlightId"].is_string());
    assert_eq!(v["results"][0]["location"]["end"], 172);
    assert_eq!(v["results"][0]["page"], "12");
    assert_eq!(v["results"][0]["source"], "kindle-clippings");
    let (_, recent) = h.call(Method::GET, "/v1/library/recent?days=7", None).await;
    assert_eq!(recent["readingSessions"], "unavailable_on_stock_kindle");
    assert_eq!(recent["highlights"].as_array().unwrap().len(), 1);
    assert_eq!(recent["changes"], json!([]));
    h.call(
        Method::PATCH,
        &format!("/v1/library/books/{id}"),
        Some(json!({"status":"completed"})),
    )
    .await;
    let (_, recent) = h.call(Method::GET, "/v1/library/recent", None).await;
    assert_eq!(recent["changes"][0]["bookId"], id);
    let (_, stats) = h.call(Method::GET, "/v1/library/stats", None).await;
    assert_eq!(stats["byStatus"]["completed"], 1);
    assert_eq!(stats["byDrm"]["protected"], 1);
    assert_eq!(stats["highlights"], 1);
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert_eq!(sync["imports"][0]["caller"], "device");
    let log = std::fs::read_to_string(h.cfg.data_dir.join("library/imports.jsonl")).unwrap();
    assert!(!log.contains("Memorable"));
    assert!(!log.contains("Café"));
    let (status, brief) = h
        .call(
            Method::POST,
            &format!("/v1/library/books/{id}/brief"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(brief["kind"], "extractive");
    assert_eq!(brief["basedOn"].as_array().unwrap().len(), 1);
    assert!(brief["model"].is_null());
    assert_eq!(
        h.call(
            Method::POST,
            &format!("/v1/library/books/{id}/brief"),
            Some(json!({}))
        )
        .await
        .1,
        brief
    );
    assert_eq!(
        h.call(
            Method::GET,
            "/v1/library/books?status=completed&onKindle=true",
            None
        )
        .await
        .1["books"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        h.call(Method::GET, "/v1/library/books?status=nonsense", None)
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn library_body_limit_overrides_default_and_logs_keep_twenty() {
    let h = H::new().await;
    let mut b = batch();
    let original = b["clippings"][0].clone();
    for n in 0..3 {
        let mut c = original.clone();
        c["highlight"]["text"] = json!(format!("{n}{}", "x".repeat(200_000)));
        b["clippings"].as_array_mut().unwrap().push(c);
    }
    let (status, v) = h.call(Method::POST, "/v1/library/import", Some(b)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["highlightsAdded"], 4);
    let mut empty = batch();
    empty["books"] = json!([]);
    empty["clippings"] = json!([]);
    for _ in 0..21 {
        assert_eq!(
            h.call(Method::POST, "/v1/library/import", Some(empty.clone()))
                .await
                .0,
            200
        );
    }
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert_eq!(sync["imports"].as_array().unwrap().len(), 20);
    let (status,_)=h.call(Method::PATCH,"/v1/library/books/bk_0000000000000000",Some(json!({"progress":{"percent":101.0,"location":null,"page":null,"source":"owner","at":"now"}}))).await;
    assert_eq!(status, 400);
}
