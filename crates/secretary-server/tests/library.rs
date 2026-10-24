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
            kind: ink_library::model::BookKind::Book,
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

#[tokio::test]
async fn dictionaries_import_but_default_lists_and_book_stats_exclude_them() {
    let h = H::new().await;
    let mut b = batch();
    let original = b["books"][0].clone();
    let mut dictionary = original.clone();
    dictionary["kind"] = json!("dictionary");
    dictionary["metadata"]["title"] = json!("Synthetic glossary");
    dictionary["fingerprint"] = json!("b".repeat(64));
    dictionary["path"] = json!("documents/dictionaries/test.mobi");
    dictionary["format"] = json!("mobi");
    let mut document = original;
    document["kind"] = json!("document");
    document["metadata"]["title"] = json!("Synthetic document");
    document["fingerprint"] = json!("c".repeat(64));
    document["path"] = json!("documents/test.txt");
    document["format"] = json!("txt");
    b["books"]
        .as_array_mut()
        .unwrap()
        .extend([dictionary, document]);
    let (status, o) = h
        .call(Method::POST, "/v1/library/import", Some(b.clone()))
        .await;
    assert_eq!(status, 200, "{o}");
    assert_eq!(o["booksAdded"], 3);
    let (_, list) = h.call(Method::GET, "/v1/library/books", None).await;
    let books = list["books"].as_array().unwrap();
    assert_eq!(books.len(), 2);
    assert!(books.iter().all(|b| b["kind"] != "dictionary"));
    for (filter, count) in [
        ("dictionary", 1),
        ("book", 1),
        ("document", 1),
        ("book,document,dictionary", 3),
    ] {
        let (status, list) = h
            .call(
                Method::GET,
                &format!("/v1/library/books?kind={filter}"),
                None,
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(list["books"].as_array().unwrap().len(), count);
    }
    let (_, list) = h
        .call(Method::GET, "/v1/library/books?kind=dictionary", None)
        .await;
    let id = list["books"][0]["id"].as_str().unwrap();
    let (_, detail) = h
        .call(Method::GET, &format!("/v1/library/books/{id}"), None)
        .await;
    assert_eq!(detail["book"]["kind"], "dictionary");
    let (status, hits) = h
        .call(
            Method::GET,
            "/v1/library/search?q=synthetic&kind=dictionary",
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(hits["results"].as_array().unwrap().len(), 1);
    assert_eq!(hits["results"][0]["bookId"], id);

    let (_, stats) = h.call(Method::GET, "/v1/library/stats", None).await;
    assert_eq!(stats["books"], 2);
    assert_eq!(stats["dictionaries"], 1);
    assert_eq!(stats["booksOnKindle"], 2);
    assert!(stats["byFormat"]["mobi"].is_null());
    assert_eq!(stats["byDrm"]["protected"], 2);
    assert_eq!(
        h.call(Method::GET, "/v1/library/books?kind=invalid", None)
            .await
            .0,
        400
    );
    let (_, outcome) = h.call(Method::POST, "/v1/library/import", Some(b)).await;
    assert_eq!(outcome["booksUnchanged"], 3);
}
#[tokio::test]
async fn obsidian_notes_index_replaces_per_vault_idempotently() {
    let mut h = H::new().await;
    let (id, _) = h.import().await;
    let notes = format!("/v1/library/books/{id}/notes");
    let book = format!("/v1/library/books/{id}");
    let (status, empty) = h.call(Method::GET, &notes, None).await;
    assert_eq!(status, 200, "{empty}");
    assert_eq!(empty["notes"], json!([]));
    assert_eq!(empty["indexedVaults"], json!([]));
    let r = |path: &str, how: &str, line: u32| json!({"bookId":id,"notePath":path,"noteTitle":"Synthetic","how":how,"excerpt":"about Café Book","line":line});
    let mut body = json!({"vault":"Vault","generatedAt":"2026-10-24T10:00:00Z","refs":[
        r("Reading/Cafe.md","frontmatter",2), r("Ideas/Talk.md","mention",7), r("Reading/Cafe.md","mention",9),
        {"bookId":"bk_ffffffffffffffff","notePath":"x.md","noteTitle":"x","how":"wikilink","excerpt":"","line":1}]});
    let (status, o) = h
        .call(Method::POST, "/v1/library/notes-index", Some(body.clone()))
        .await;
    assert_eq!(status, 200, "{o}");
    assert_eq!(
        (
            o["refsStored"].clone(),
            o["booksLinked"].clone(),
            o["unknownBooks"].clone(),
            o["changed"].clone()
        ),
        (json!(3), json!(1), json!(1), json!(true))
    );
    let (_, got) = h.call(Method::GET, &notes, None).await;
    assert_eq!(got["total"], 3);
    assert_eq!(got["notes"][0]["vault"], "Vault");
    assert_eq!(got["notes"][0]["line"], 7);
    assert_eq!(got["indexedVaults"][0]["refs"], 3);
    let (_, b1) = h.call(Method::GET, &book, None).await;
    let related = &b1["book"]["relatedNotes"];
    assert_eq!(related.as_array().unwrap().len(), 2, "{related}");
    assert_eq!(related[0]["path"], "Ideas/Talk.md");
    assert_eq!(related[0]["how"], "mention");
    assert_eq!(related[1]["path"], "Reading/Cafe.md");
    assert_eq!(related[1]["how"], "frontmatter");
    assert_eq!(related[1]["linkedAt"], "2026-10-24T10:00:00Z");
    // A later index with the same references changes nothing about the book.
    body["generatedAt"] = json!("2026-10-25T10:00:00Z");
    let (_, o) = h
        .call(Method::POST, "/v1/library/notes-index", Some(body.clone()))
        .await;
    assert_eq!(o["changed"], false);
    let (_, b2) = h.call(Method::GET, &book, None).await;
    assert_eq!(b2["book"], b1["book"]);
    // Vaults are independent; an empty index clears only its own vault.
    let work = json!({"vault":"Work notes","generatedAt":"2026-10-25T10:00:00Z","refs":[r("Work/Note.md","wikilink",3)]});
    assert_eq!(
        h.call(Method::POST, "/v1/library/notes-index", Some(work))
            .await
            .0,
        200
    );
    assert_eq!(h.call(Method::GET, &notes, None).await.1["total"], 4);
    let clear = json!({"vault":"Vault","generatedAt":"2026-10-26T10:00:00Z","refs":[]});
    let (_, o) = h
        .call(Method::POST, "/v1/library/notes-index", Some(clear))
        .await;
    assert_eq!(o["booksUpdated"], 1);
    let (_, after) = h.call(Method::GET, &notes, None).await;
    assert_eq!(after["total"], 1);
    assert_eq!(after["notes"][0]["vault"], "Work notes");
    assert_eq!(after["indexedVaults"].as_array().unwrap().len(), 2);
    let (_, b3) = h.call(Method::GET, &book, None).await;
    assert_eq!(b3["book"]["relatedNotes"].as_array().unwrap().len(), 1);
    for (field, value) in [
        ("notePath", json!("../x.md")),
        ("notePath", json!("x.txt")),
        ("notePath", json!("/abs.md")),
        ("how", json!("bogus")),
        ("excerpt", json!("x".repeat(241))),
        ("line", json!(0)),
        ("bookId", json!("bk_x")),
        ("noteTitle", json!("")),
    ] {
        let mut bad = r("a.md", "mention", 1);
        bad[field] = value;
        let body = json!({"vault":"Vault","generatedAt":"2026-10-24T10:00:00Z","refs":[bad]});
        assert_eq!(
            h.call(Method::POST, "/v1/library/notes-index", Some(body))
                .await
                .0,
            400,
            "{field}"
        );
    }
    for (field, value) in [
        ("vault", json!("../v")),
        ("vault", json!("")),
        ("generatedAt", json!("yesterday")),
    ] {
        let mut body = json!({"vault":"Vault","generatedAt":"2026-10-24T10:00:00Z","refs":[]});
        body[field] = value;
        assert_eq!(
            h.call(Method::POST, "/v1/library/notes-index", Some(body))
                .await
                .0,
            400,
            "{field}"
        );
    }
    assert_eq!(
        h.call(
            Method::GET,
            "/v1/library/books/bk_ffffffffffffffff/notes",
            None
        )
        .await
        .0,
        404
    );
    let r = h
        .http
        .post(format!("{}/v1/library/notes-index", h.base))
        .json(&json!({"vault":"Vault","generatedAt":"2026-10-24T10:00:00Z","refs":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert!(sync["imports"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["source"] == "obsidian"));
    assert!(!sync.to_string().contains("Reading/Cafe.md"));
    let _ = h.stop.take().unwrap().send(());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (base, stop) = start(&h.cfg).await;
    h.base = base;
    h.stop = Some(stop);
    assert_eq!(h.call(Method::GET, &notes, None).await.1, after);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [
            format!("library/notes/{id}.json"),
            "library/notes-index.json".into(),
        ] {
            let mode = std::fs::metadata(h.cfg.data_dir.join(p))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}

// ---- POST /v1/library/sync (ink-sync) ----------------------------------------------

fn op(device: &str, n: u64, secs: i64, body: Value) -> Value {
    let mut v = body;
    v["opId"] = json!(format!("{device}:{n}"));
    v["at"] = json!(chrono::DateTime::from_timestamp(1_790_000_000 + secs, 0)
        .unwrap()
        .to_rfc3339());
    v
}
fn sync_body(device: &str, ops: Vec<Value>) -> Value {
    json!({"device": device, "ops": ops})
}
impl H {
    async fn restart(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (base, stop) = start(&self.cfg).await;
        self.base = base;
        self.stop = Some(stop);
    }
}

#[tokio::test]
async fn sync_requires_auth_and_replays_are_noops() {
    let mut h = H::new().await;
    let (id, _) = h.import().await;
    let body = sync_body(
        "kobo-1",
        vec![
            op(
                "kobo-1",
                1,
                10,
                json!({"type":"setProgress","bookId":id,"anchor":"epubcfi(/6/4!/4/2)","percent":40.0}),
            ),
            op(
                "kobo-1",
                2,
                11,
                json!({"type":"addHighlight","bookId":id,"text":"A synced passage.","anchor":"epubcfi(/6/4!/4/8)","note":"mine"}),
            ),
            op(
                "kobo-1",
                3,
                12,
                json!({"type":"setStatus","bookId":id,"status":"reading"}),
            ),
            op(
                "kobo-1",
                4,
                13,
                json!({"type":"startSession","bookId":id,"sessionId":"kobo-1-s1","location":3}),
            ),
        ],
    );
    // Auth required.
    let r = h
        .http
        .post(format!("{}/v1/library/sync", h.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let (status, first) = h
        .call(Method::POST, "/v1/library/sync", Some(body.clone()))
        .await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(
        first["acked"],
        json!(["kobo-1:1", "kobo-1:2", "kobo-1:3", "kobo-1:4"])
    );
    assert_eq!(first["serverClock"], 4);
    let path = format!("/v1/library/books/{id}");
    let (_, book) = h.call(Method::GET, &path, None).await;
    assert_eq!(book["book"]["progress"]["percent"], 40.0);
    assert_eq!(book["book"]["progress"]["anchor"], "epubcfi(/6/4!/4/2)");
    assert_eq!(book["book"]["progress"]["source"], "device:kobo-1");
    assert_eq!(book["book"]["status"], "reading");
    assert_eq!(book["book"]["statusSource"], "device:kobo-1");
    let synced: Vec<_> = book["highlights"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["source"] == "device-sync")
        .collect();
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0]["note"], "mine");
    assert_eq!(book["sessions"][0]["id"], "kobo-1-s1");
    // A replay (lost response) is acked again and changes nothing.
    let (status, again) = h
        .call(Method::POST, "/v1/library/sync", Some(body.clone()))
        .await;
    assert_eq!(status, 200);
    assert_eq!(again["acked"], first["acked"]);
    assert_eq!(again["serverClock"], 4);
    assert_eq!(h.call(Method::GET, &path, None).await.1, book);
    // Durable across a restart, and a replay after it is still a no-op.
    h.restart().await;
    assert_eq!(h.call(Method::GET, &path, None).await.1, book);
    let (_, again) = h.call(Method::POST, "/v1/library/sync", Some(body)).await;
    assert_eq!(again["serverClock"], 4);
    assert_eq!(h.call(Method::GET, &path, None).await.1, book);
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert_eq!(sync["serverClock"], 4);
    assert_eq!(sync["devices"]["kobo-1"], 4);
    // The envelope is checked: ops must belong to the request's device.
    let bad = sync_body(
        "kobo-1",
        vec![op(
            "kobo-2",
            1,
            1,
            json!({"type":"setStatus","bookId":id,"status":"reading"}),
        )],
    );
    assert_eq!(
        h.call(Method::POST, "/v1/library/sync", Some(bad)).await.0,
        400
    );
    assert_eq!(
        h.call(
            Method::POST,
            "/v1/library/sync",
            Some(json!({"device":"x","ops":"no"}))
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn sync_out_of_order_and_two_devices_converge() {
    let h = H::new().await;
    let (id, _) = h.import().await;
    let p = |d: &str, n, secs, pct: f64| {
        op(
            d,
            n,
            secs,
            json!({"type":"setProgress","bookId":id,"percent":pct}),
        )
    };
    // kobo-1's ops arrive 3, 1, 2; boox-2's older position arrives last.
    for (dev, ops) in [
        ("kobo-1", vec![p("kobo-1", 3, 30, 30.0)]),
        (
            "kobo-1",
            vec![p("kobo-1", 1, 10, 10.0), p("kobo-1", 2, 20, 20.0)],
        ),
        ("boox-2", vec![p("boox-2", 1, 25, 25.0)]),
    ] {
        let (status, r) = h
            .call(Method::POST, "/v1/library/sync", Some(sync_body(dev, ops)))
            .await;
        assert_eq!(status, 200, "{r}");
    }
    let (_, book) = h
        .call(Method::GET, &format!("/v1/library/books/{id}"), None)
        .await;
    assert_eq!(book["book"]["progress"]["percent"], 30.0);
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert_eq!(sync["devices"], json!({"boox-2":1,"kobo-1":3}));
    assert_eq!(sync["recentConflicts"][0]["opId"], "boox-2:1");
    assert_eq!(sync["recentConflicts"][0]["winner"], "kobo-1:3");
}

#[tokio::test]
async fn owner_completed_status_is_protected_and_deletions_need_the_owner() {
    let h = H::new().await;
    let (id, _) = h.import().await;
    let path = format!("/v1/library/books/{id}");
    let (status, _) = h
        .call(Method::PATCH, &path, Some(json!({"status":"completed"})))
        .await;
    assert_eq!(status, 200);
    // A device op stamped after the owner's edit still cannot revert it.
    let future = chrono::Utc::now() + chrono::Duration::days(1);
    let mut revert = json!({"type":"setStatus","bookId":id,"status":"reading","opId":"kobo-1:1"});
    revert["at"] = json!(future.to_rfc3339());
    let (status, r) = h
        .call(
            Method::POST,
            "/v1/library/sync",
            Some(sync_body("kobo-1", vec![revert])),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(r["acked"], json!(["kobo-1:1"]));
    assert_eq!(r["conflicts"][0]["kind"], "protected");
    let (_, book) = h.call(Method::GET, &path, None).await;
    assert_eq!(book["book"]["status"], "completed");
    assert_eq!(book["book"]["statusSource"], "owner");
    // A deletion request is recorded, not applied.
    let hl = book["highlights"][0]["id"].as_str().unwrap().to_owned();
    let del = op(
        "kobo-1",
        2,
        50,
        json!({"type":"deleteHighlight","bookId":id,"highlightId":hl}),
    );
    let (_, r) = h
        .call(
            Method::POST,
            "/v1/library/sync",
            Some(sync_body("kobo-1", vec![del])),
        )
        .await;
    assert_eq!(r["acked"], json!(["kobo-1:2"]));
    assert_eq!(
        h.call(Method::GET, &path, None).await.1["highlights"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (_, sync) = h.call(Method::GET, "/v1/library/sync", None).await;
    assert_eq!(sync["pendingTombstones"][0]["highlightId"], hl.as_str());
    // Lucina cannot confirm; the owner's device can.
    let confirm = format!("{}/v1/library/sync/tombstones/kobo-1:2/confirm", h.base);
    let r = h
        .http
        .post(&confirm)
        .bearer_auth(&h.service)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let (status, _) = h
        .call(
            Method::POST,
            "/v1/library/sync/tombstones/kobo-1:9/confirm",
            None,
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = h
        .call(
            Method::POST,
            "/v1/library/sync/tombstones/kobo-1:2/confirm",
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert!(h.call(Method::GET, &path, None).await.1["highlights"]
        .as_array()
        .unwrap()
        .is_empty());
    // A re-import of the same clipping does not bring it back.
    h.call(Method::POST, "/v1/library/import", Some(batch()))
        .await;
    assert!(h.call(Method::GET, &path, None).await.1["highlights"]
        .as_array()
        .unwrap()
        .is_empty());
}
