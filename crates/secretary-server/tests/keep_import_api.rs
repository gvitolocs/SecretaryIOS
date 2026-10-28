//! The Google Keep importer against the real note store and router: dry run
//! writes nothing, a real import maps every field, re-imports are
//! idempotent, local edits and deletions win, and the endpoint streams a zip
//! (devices only, one at a time).

#[path = "common/keep_fixture.rs"]
mod keep_fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use keep_fixture::*;
use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::keep_import::{import_takeout, ImportReport};
use secretary_server::notes::{Caller, NoteStore};
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct H {
    base: String,
    _stop: oneshot::Sender<()>,
    http: Client,
    data_dir: PathBuf,
    token: String,
    service: String,
    dir: tempfile::TempDir,
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
        let state = AppState::new(&cfg).expect("state");
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
        Self {
            base,
            _stop: tx,
            http: Client::builder().pool_max_idle_per_host(0).build().unwrap(),
            data_dir: cfg.data_dir.clone(),
            token,
            service,
            dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut rb = self
            .http
            .request(m, self.url(path))
            .bearer_auth(&self.token)
            .header("x-secretary-client", "web");
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.call(Method::GET, path, None).await;
        assert_eq!(s, StatusCode::OK, "{path}: {v}");
        v
    }

    /// Import through the library (what the CLI does), as a separate store.
    fn import(&self, zips: &[PathBuf], dry_run: bool) -> ImportReport {
        let store = NoteStore::new(&self.data_dir);
        import_takeout(&store, zips, dry_run, Caller("owner")).unwrap()
    }

    /// title → note (live and trashed), full notes.
    async fn notes_by_title(&self) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        let list = self.get("/v1/notes?limit=500").await;
        let mut found: Vec<Value> = Vec::new();
        for n in list["notes"].as_array().unwrap() {
            found.push(
                self.get(&format!("/v1/notes/{}", n["id"].as_str().unwrap()))
                    .await,
            );
        }
        // Trashed notes cannot be fetched by id: their summaries stand in.
        let trash = self.get("/v1/notes/trash").await;
        found.extend(trash["notes"].as_array().unwrap().iter().cloned());
        for full in found {
            let mut title = full["title"].as_str().unwrap().to_owned();
            if title.is_empty() {
                title = format!("<untitled:{}>", full["content"].as_str().unwrap_or(""));
            }
            if title.chars().count() == 200 {
                title = "<long title>".into();
            }
            out.insert(title, full);
        }
        out
    }

    async fn attachment(&self, file: &str) -> (String, Vec<u8>) {
        let r = self
            .http
            .get(self.url(&format!("/v1/notes/attachments/{file}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let ct = r.headers()["content-type"].to_str().unwrap().to_owned();
        (ct, r.bytes().await.unwrap().to_vec())
    }
}

/// Every file under `dir` with its bytes (to prove a run wrote nothing).
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            // Opening the store takes its lock file; that is not a write.
            if rel.ends_with(".lock") {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn one_zip(dir: &Path, name: &str, edit: impl FnOnce(&mut Files)) -> PathBuf {
    let (mut a, b) = parts();
    edit(&mut a);
    a.extend(b);
    let p = dir.join(name);
    write_zip(&p, &a);
    p
}

fn fixture_attachment_bytes() -> u64 {
    // img1, pic3, pic2, pic4, rec.3gp, second.png (clip.amr is refused)
    (png(1).len() + png(3).len() + jpeg(2).len() + jpeg(4).len() + three_gp().len() + png(20).len())
        as u64
}

// ---------------------------------------------------------------- dry run

#[tokio::test]
async fn dry_run_writes_nothing_and_reports_everything() {
    let h = H::new().await;
    let zips = write_takeout(&h.dir.path().join("takeout"));
    let before = snapshot(&h.data_dir);

    let r = h.import(&zips, true);

    assert_eq!(snapshot(&h.data_dir), before, "the data dir is untouched");
    assert!(r.dry_run);
    assert_eq!(r.files.json, 24);
    assert_eq!(r.files.html, 2);
    assert_eq!(r.files.attachments, 7);
    assert_eq!(r.files.ignored, 3);
    assert_eq!(r.notes.total, 22);
    assert_eq!(r.notes.new, 22);
    assert_eq!(
        (
            r.notes.updated,
            r.notes.unchanged,
            r.notes.kept_local_edits,
            r.notes.skipped_deleted
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        (
            r.notes.trashed,
            r.notes.archived,
            r.notes.pinned,
            r.notes.lists
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(r.labels.total, 4);
    assert_eq!(
        r.labels.new,
        ["GodLevel", "Fisica 2", "health tips", "Unused label"]
    );
    assert_eq!(r.attachments.total, 6);
    assert_eq!(r.attachments.new, 6);
    assert_eq!(r.attachments.bytes, fixture_attachment_bytes());
    assert_eq!(r.attachments.missing, ["gone.png"]);
    assert_eq!(r.sample.len(), 20);
    // File-name order: the localized root sorts first.
    assert_eq!(r.sample[0].title, "Second part");
    assert_eq!(r.sample[0].kind, "image");
    assert!(r
        .sample
        .iter()
        .any(|x| x.kind == "list" && x.labels == ["health tips"]));
    assert!(r.sample.iter().any(|x| x.color == "cerulean"));
    // corrupt json, empty note, refused AMR clip
    assert_eq!(r.errors.len(), 3, "{:?}", r.errors);
    assert!(r
        .errors
        .iter()
        .any(|e| e.error.contains("unsupported attachment type")));

    // ...and the live API still sees an empty store.
    assert_eq!(h.get("/v1/notes").await, json!({"notes": []}));
}

// ---------------------------------------------------------------- real import

#[tokio::test]
async fn real_import_maps_every_field() {
    let h = H::new().await;
    let zips = write_takeout(&h.dir.path().join("takeout"));
    let r = h.import(&zips, false);
    assert!(!r.dry_run);
    assert_eq!(r.notes.new, 22);
    assert_eq!(r.attachments.new, 6);
    let n = h.notes_by_title().await;
    assert_eq!(n.len(), 22);

    let live = h.get("/v1/notes?limit=500").await;
    assert_eq!(live["notes"].as_array().unwrap().len(), 21);
    assert_eq!(
        h.get("/v1/notes/trash").await["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // text, dates, source, import bookkeeping
    let w = &n["Weekend plan"];
    assert_eq!(w["content"], "Buy tea\nCall Marta");
    assert_eq!(w["tags"], json!(["godlevel"]));
    assert_eq!(w["color"], "default");
    assert_eq!(w["createdAt"], "2023-11-14T22:13:21Z");
    assert_eq!(w["updatedAt"], "2023-11-14T22:14:21Z");
    assert_eq!(w["source"], "google-keep");
    assert_eq!(w["import"]["source"], "google-keep");
    assert_eq!(w["import"]["key"], format!("gkeep:{}", usec(1)));
    assert_eq!(w["import"]["editedUsec"], usec(1) + 60_000_000);
    assert_eq!(w["import"]["importedAt"], w["updatedAt"]);
    assert!(w["rev"].as_u64().unwrap() > 0);

    // rich text
    assert_eq!(
        n["Rich note"]["content"],
        "**Bold** and *italic* and <u>under</u>\n# Head\n## Sub\nVisit [the site](https://example.org/x?a=1&b=2) & bad"
    );

    // checklist
    let l = &n["Shopping"];
    let items: Vec<(String, bool)> = l["items"]
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
        items,
        [
            ("milk".into(), false),
            ("eggs".into(), true),
            ("bread".into(), false)
        ]
    );
    assert_eq!(l["content"], "- [ ] milk\n- [x] eggs\n- [ ] bread");
    assert_eq!(l["tags"], json!(["health tips"]));

    // flags and colours
    assert_eq!(n["Pinned one"]["pinned"], true);
    assert_eq!(n["Pinned one"]["color"], "cerulean");
    assert_eq!(n["Old stuff"]["archived"], true);
    assert_eq!(n["Old stuff"]["color"], "teal");
    assert_eq!(n["Old stuff"]["tags"], json!(["fisica 2"]));
    assert_eq!(n["Trashed"]["color"], "default");
    assert!(n["Trashed"]["deletedAt"].is_string());
    assert!(n["Weekend plan"]
        .get("deletedAt")
        .is_none_or(Value::is_null));
    for (t, c) in [
        ("Red", "red"),
        ("Yellow", "yellow"),
        ("Purple", "purple"),
        ("Gray", "gray"),
    ] {
        assert_eq!(n[t]["color"], c);
    }

    // labels registry keeps display names
    let labels = h.get("/v1/notes/labels").await;
    let names: Vec<(&str, u64)> = labels["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["name"].as_str().unwrap(), l["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        names,
        [
            ("Fisica 2", 1),
            ("GodLevel", 1),
            ("health tips", 1),
            ("Unused label", 0)
        ]
    );

    // attachments: bytes readable, wrong declared type fixed, AMR dropped
    let photo = n["Photo"]["attachments"].as_array().unwrap();
    assert_eq!(photo.len(), 2);
    assert_eq!(photo[0]["name"], "img1.png");
    assert_eq!(photo[0]["mimetype"], "image/png");
    assert_eq!(
        photo[1]["mimetype"], "image/png",
        "pic3 was declared image/jpeg"
    );
    let (ct, bytes) = h.attachment(photo[0]["file"].as_str().unwrap()).await;
    assert_eq!((ct.as_str(), bytes), ("image/png", png(1)));
    let (_, bytes) = h.attachment(photo[1]["file"].as_str().unwrap()).await;
    assert_eq!(bytes, png(3));
    let j = n["Jpeg mismatch"]["attachments"].as_array().unwrap();
    assert_eq!(j.len(), 2);
    assert!(j.iter().all(|a| a["mimetype"] == "image/jpeg"));
    let (_, bytes) = h.attachment(j[0]["file"].as_str().unwrap()).await;
    assert_eq!(bytes, jpeg(2));
    assert_eq!(n["Missing attachment"]["attachments"], json!([]));
    let v = n["Voice"]["attachments"].as_array().unwrap();
    assert_eq!(v.len(), 1, "the AMR clip is not an allowed type");
    assert_eq!(v[0]["mimetype"], "audio/3gpp");
    let (_, bytes) = h.attachment(v[0]["file"].as_str().unwrap()).await;
    assert_eq!(bytes, three_gp());
    let (_, bytes) = h
        .attachment(n["Second part"]["attachments"][0]["file"].as_str().unwrap())
        .await;
    assert_eq!(bytes, png(20));

    // links, sharees, untitled, long title, duplicate keys
    assert_eq!(
        n["Link"]["annotations"],
        json!([{"url": "https://example.org/page", "title": "Example",
                "description": "An example page", "source": "WEBLINK"}])
    );
    assert_eq!(n["Shared"]["collaborators"], json!(["friend@example.org"]));
    assert_eq!(n["<untitled:no title here>"]["title"], "");
    let long = &n["<long title>"];
    assert_eq!(long["title"].as_str().unwrap().chars().count(), 200);
    assert!(long["content"]
        .as_str()
        .unwrap()
        .ends_with("\n\nshort body"));
    assert_eq!(n["Dup A"]["import"]["key"], format!("gkeep:{}", usec(19)));
    assert_eq!(n["Dup B"]["import"]["key"], format!("gkeep:{}-2", usec(19)));
    assert_eq!(n["Orphan"]["content"], "found by content");
}

// ---------------------------------------------------------------- idempotency

#[tokio::test]
async fn reimport_changes_nothing() {
    let h = H::new().await;
    let zips = write_takeout(&h.dir.path().join("takeout"));
    h.import(&zips, false);
    let before = h.notes_by_title().await;

    let r = h.import(&zips, false);
    assert_eq!(r.notes.new, 0);
    assert_eq!(r.notes.unchanged, 21);
    assert_eq!(
        r.notes.kept_local_edits, 1,
        "the note in the trash is left alone"
    );
    assert_eq!(r.attachments.new, 0);
    assert_eq!(r.attachments.bytes, 0);
    assert!(r.labels.new.is_empty());
    assert!(r.sample.is_empty());
    assert_eq!(h.notes_by_title().await, before, "no note was touched");
    assert_eq!(
        h.get("/v1/notes?limit=500").await["notes"]
            .as_array()
            .unwrap()
            .len(),
        21
    );

    // A dry run over the imported store says the same, and writes nothing.
    let snap = snapshot(&h.data_dir);
    let d = h.import(&zips, true);
    assert_eq!((d.notes.new, d.notes.unchanged), (0, 21));
    assert_eq!(snapshot(&h.data_dir), snap);
}

#[tokio::test]
async fn local_edits_win_and_untouched_notes_follow_the_export() {
    let h = H::new().await;
    let dir = h.dir.path().join("t");
    std::fs::create_dir_all(&dir).unwrap();
    let first = one_zip(&dir, "v1.zip", |_| {});
    h.import(&[first], false);
    let n = h.notes_by_title().await;
    let id_plan = n["Weekend plan"]["id"].as_str().unwrap().to_owned();
    let id_red = n["Red"]["id"].as_str().unwrap().to_owned();

    // Edited here.
    let (s, _) = h
        .call(
            Method::PATCH,
            &format!("/v1/notes/{id_plan}"),
            Some(json!({"content": "edited on the phone"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);

    // A newer export edits both notes.
    let second = one_zip(&dir, "v2.zip", |a| {
        for (suffix, text) in [("Weekend plan.json", "plan v2"), ("Red.json", "red v2")] {
            edit_json(a, suffix, |v| {
                v["userEditedTimestampUsec"] = json!(T0 + 900_000_000_000_i64);
                v["textContent"] = json!(text);
                v.as_object_mut().unwrap().remove("textContentHtml");
            });
        }
    });
    let r = h.import(std::slice::from_ref(&second), false);
    assert_eq!(r.notes.updated, 1);
    assert_eq!(
        r.notes.kept_local_edits, 2,
        "the edited note and the trashed one"
    );
    assert_eq!(r.notes.unchanged, 19);
    assert_eq!(r.notes.new, 0);

    let plan = h.get(&format!("/v1/notes/{id_plan}")).await;
    assert_eq!(plan["content"], "edited on the phone");
    let red = h.get(&format!("/v1/notes/{id_red}")).await;
    assert_eq!(red["content"], "red v2", "updated in place, same id");
    assert_eq!(red["import"]["editedUsec"], T0 + 900_000_000_000_i64);
    assert_eq!(
        h.get("/v1/notes?limit=500").await["notes"]
            .as_array()
            .unwrap()
            .len(),
        21
    );

    // Importing the same export again changes nothing more.
    let r = h.import(&[second], false);
    assert_eq!(
        (r.notes.updated, r.notes.unchanged, r.notes.kept_local_edits),
        (0, 20, 2)
    );
}

#[tokio::test]
async fn deleted_forever_is_never_resurrected() {
    let h = H::new().await;
    let zips = write_takeout(&h.dir.path().join("takeout"));
    h.import(&zips, false);
    let n = h.notes_by_title().await;
    let id = n["Gray"]["id"].as_str().unwrap().to_owned();

    let (s, _) = h
        .call(Method::DELETE, &format!("/v1/notes/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .call(Method::DELETE, &format!("/v1/notes/trash/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!h.notes_by_title().await.contains_key("Gray"));

    let r = h.import(&zips, false);
    assert_eq!(r.notes.skipped_deleted, 1);
    assert_eq!(r.notes.new, 0);
    assert!(!h.notes_by_title().await.contains_key("Gray"));
    // Same through a dry run.
    assert_eq!(h.import(&zips, true).notes.skipped_deleted, 1);
}

// ---------------------------------------------------------------- endpoint

async fn post_zip(h: &H, query: &str, token: Option<&str>, body: Vec<u8>) -> (StatusCode, Value) {
    let mut rb = h
        .http
        .post(h.url(&format!("/v1/notes/import/google-keep{query}")))
        .header("content-type", "application/zip")
        .body(body);
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    let r = rb.send().await.unwrap();
    let s = r.status();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn endpoint_dry_run_import_and_cleanup() {
    let h = H::new().await;
    let zip = std::fs::read(one_zip(h.dir.path(), "all.zip", |_| {})).unwrap();
    let before = snapshot(&h.data_dir);

    let (s, v) = post_zip(&h, "?dryRun=1", Some(&h.token), zip.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["dryRun"], true);
    assert_eq!(v["notes"]["total"], 22);
    assert_eq!(v["notes"]["new"], 22);
    assert_eq!(v["attachments"]["missing"], json!(["gone.png"]));
    assert!(v["files"]["json"].is_number());
    assert_eq!(h.get("/v1/notes").await, json!({"notes": []}));
    let after = snapshot(&h.data_dir);
    let new_files: Vec<_> = after
        .keys()
        .filter(|k| !before.contains_key(*k) && !k.ends_with(".tmp"))
        .collect();
    assert!(
        new_files.iter().all(|k| k.starts_with("tmp")),
        "a dry run writes no notes, only the tmp dir exists: {new_files:?}"
    );
    assert!(
        !after.keys().any(|k| k.contains("keep-import-")),
        "the temp zip is deleted"
    );

    let (s, v) = post_zip(&h, "", Some(&h.token), zip.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["dryRun"], false);
    assert_eq!(v["notes"]["new"], 22);
    assert_eq!(
        h.get("/v1/notes?limit=500").await["notes"]
            .as_array()
            .unwrap()
            .len(),
        21
    );
    assert!(!snapshot(&h.data_dir)
        .keys()
        .any(|k| k.contains("keep-import-")));

    // Again: nothing new; ?dryRun=0 is a real run.
    let (s, v) = post_zip(&h, "?dryRun=0", Some(&h.token), zip).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["notes"]["new"], 0);
    assert_eq!(v["notes"]["unchanged"], 21);

    // The tmp dir is private.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(h.data_dir.join("tmp"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    // `import` is a reserved slug: it is no note.
    let (s, _) = h.call(Method::GET, "/v1/notes/import", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn endpoint_refuses_the_service_token_bad_zips_and_strangers() {
    let h = H::new().await;
    let zip = std::fs::read(one_zip(h.dir.path(), "all.zip", |_| {})).unwrap();

    let (s, v) = post_zip(&h, "?dryRun=1", Some(&h.service), zip.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "forbidden");
    assert_eq!(h.get("/v1/notes").await, json!({"notes": []}));

    let (s, _) = post_zip(&h, "", None, zip).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, v) = post_zip(&h, "", Some(&h.token), b"definitely not a zip".to_vec()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "invalid_zip");
    let (s, v) = post_zip(&h, "", Some(&h.token), vec![]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "empty_body");
    assert!(
        !snapshot(&h.data_dir)
            .keys()
            .any(|k| k.contains("keep-import-")),
        "failed uploads leave no temp file"
    );
}

#[tokio::test]
async fn endpoint_runs_one_import_at_a_time() {
    let h = H::new().await;
    let zip = std::fs::read(one_zip(h.dir.path(), "all.zip", |_| {})).unwrap();
    let (half, rest) = zip.split_at(zip.len() / 2);

    // A slow upload holds the import slot.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
    let slow = h
        .http
        .post(h.url("/v1/notes/import/google-keep"))
        .bearer_auth(&h.token)
        .body(reqwest::Body::wrap_stream(stream))
        .send();
    let slow = tokio::spawn(slow);
    tx.send(Ok(half.to_vec())).await.unwrap();

    let mut got = StatusCode::OK;
    for _ in 0..100 {
        let (s, v) = post_zip(&h, "?dryRun=1", Some(&h.token), zip.clone()).await;
        got = s;
        if s == StatusCode::CONFLICT {
            assert_eq!(v["error"]["code"], "import_running");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        got,
        StatusCode::CONFLICT,
        "a second import is refused while one runs"
    );

    tx.send(Ok(rest.to_vec())).await.unwrap();
    drop(tx);
    let r = slow.await.unwrap().unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["notes"]["new"], 22);

    // The slot is free again.
    let (s, _) = post_zip(&h, "?dryRun=1", Some(&h.token), zip).await;
    assert_eq!(s, StatusCode::OK);
}

// ---------------------------------------------------------------- CLI

#[test]
fn cli_import_keep_prints_text_and_json() {
    use secretary_server::cli::{run_notes, NotesAction};
    let dir = tempfile::tempdir().unwrap();
    let zips = write_takeout(&dir.path().join("t"));
    let data = dir.path().join("data");
    let run = |dry_run: bool, json: bool| {
        let mut out = Vec::new();
        run_notes(
            Some(data.clone()),
            NotesAction::ImportKeep {
                zips: zips.clone(),
                dry_run,
                json,
                label: None,
            },
            &mut out,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    };

    let text = run(true, false);
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("22 total: 22 new"), "{text}");
    assert!(text.contains("missing: gone.png"), "{text}");
    assert!(
        !data.join("notes").exists(),
        "a dry run creates no notes dir"
    );

    let v: Value = serde_json::from_str(&run(false, true)).unwrap();
    assert_eq!(v["dryRun"], false);
    assert_eq!(v["notes"]["new"], 22);
    assert_eq!(v["labels"]["new"].as_array().unwrap().len(), 4);
    let again: Value = serde_json::from_str(&run(false, true)).unwrap();
    assert_eq!(again["notes"]["new"], 0);
    assert_eq!(again["notes"]["unchanged"], 21);

    let bad = dir.path().join("bad.zip");
    std::fs::write(&bad, b"nope").unwrap();
    let mut out = Vec::new();
    assert!(run_notes(
        Some(data),
        NotesAction::ImportKeep {
            zips: vec![bad],
            dry_run: true,
            json: false,
            label: None,
        },
        &mut out
    )
    .is_err());
}

#[tokio::test]
async fn endpoint_label_marks_a_second_accounts_notes() {
    let h = H::new().await;
    let zip = std::fs::read(one_zip(h.dir.path(), "all.zip", |_| {})).unwrap();

    let (s, v) = post_zip(&h, "?label=a,b", Some(&h.token), zip.clone()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    let (s, v) = post_zip(&h, "?label=nesaggezza", Some(&h.token), zip.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["notes"]["new"], 22);
    for (title, n) in &h.notes_by_title().await {
        let tags: Vec<&str> = n["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"nesaggezza"), "{title}: {tags:?}");
    }
    let labels = h.get("/v1/notes/labels").await;
    assert!(
        labels["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "nesaggezza"),
        "{labels}"
    );

    // Same export, same label: nothing changes and nothing doubles.
    let before = h.notes_by_title().await.len();
    let (s, v) = post_zip(&h, "?label=nesaggezza", Some(&h.token), zip).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["notes"]["new"], 0);
    assert_eq!(v["notes"]["unchanged"], 21);
    assert_eq!(h.notes_by_title().await.len(), before);
}
