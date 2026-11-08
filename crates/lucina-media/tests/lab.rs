//! The AI Lab end to end through the HTTP router, with a fake runner command (a shell script that copies the
//! input to the output) and synthetic pictures only: queueing and validation, the worker (one job at a time,
//! progress, timeouts, requeue after a restart), accept (a new linked item, the original untouched), discard,
//! retry, cancel, and the owner's notification through mock secretary-server and Telegram endpoints.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::lab;
use lucina_media::lab_run::{self, LabConfig, NotifyConfig};
use lucina_media::meta::ClientMeta;
use lucina_media::{http, store};
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

struct Env {
    tmp: tempfile::TempDir,
    lib: Library,
    app: Router,
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None);
    Env { tmp, lib, app }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

async fn json_of(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, _, bytes) = call(app, method, uri, body).await;
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([shade, 90, 160]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn add_sized(env: &Env, name: &str, w: u32, h: u32, shade: u8) -> String {
    let src = env.tmp.path().join(format!("src-{shade}-{name}"));
    std::fs::write(&src, png(w, h, shade)).unwrap();
    let client = ClientMeta {
        filename: Some(name.into()),
        source: Some("phone-test".into()),
        device: Some("iphone-test".into()),
        asset_id: Some(format!("iphone-test/{name}")),
        taken_at: Some("2024-05-01T10:00:00Z".into()),
        ..Default::default()
    };
    ingest::ingest_file(&env.lib, &src, None, &client, None)
        .unwrap()
        .item_id
}

fn add(env: &Env, name: &str, shade: u8) -> String {
    add_sized(env, name, 8, 8, shade)
}

/// A runner that behaves like `lab-edit`: it announces itself, prints progress, and copies the input to the
/// output with the prompt appended (a decodable PNG that differs from every library picture). It writes its
/// arguments one per line to `args.txt`, and fails while `fail.flag` exists next to it.
fn fake_runner(dir: &Path, extra: &str) -> PathBuf {
    let path = dir.join("fake-lab");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/bash
here=$(dirname "$0")
printf '%s\n' "$@" > "$here/args.txt"
while [ $# -gt 0 ]; do
  case "$1" in
    --input) in=$2; shift 2;; --output) out=$2; shift 2;; --prompt) prompt=$2; shift 2;;
    *) shift;;
  esac
done
echo "MODEL fake"; echo STARTED; echo "PROGRESS 40"; echo "PROGRESS 90"
{extra}
if [ -f "$here/fail.flag" ]; then echo "boom: out of memory" >&2; exit 3; fi
cp "$in" "$out"; printf 'lab:%s' "$prompt" >> "$out"
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn cfg_for(runner: &Path) -> LabConfig {
    LabConfig::for_command(vec![runner.to_string_lossy().into_owned()])
}

async fn queue(env: &Env, item: &str, prompt: &str) -> Value {
    let (s, v) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/items/{item}/lab"),
        Some(json!({ "prompt": prompt })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn job(env: &Env, id: &str) -> Value {
    let (s, v) = json_of(&env.app, "GET", &format!("/v1/media/lab/{id}"), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

#[tokio::test]
async fn queueing_validates_the_request() {
    let env = setup();
    let item = add(&env, "a.png", 10);
    let other = add(&env, "b.png", 20);

    let v = queue(&env, &item, "  make the sky red  ").await;
    assert_eq!(v["status"], "queued");
    assert_eq!(v["prompt"], "make the sky red", "trimmed");
    assert_eq!(v["queue_position"], 1);
    assert_eq!(v["item_id"], item);
    assert_eq!(v["original"]["id"], item);
    assert!(v["urls"]["before"].as_str().unwrap().contains(&item), "{v}");
    assert!(v["urls"]["after"].is_null());
    let first = v["id"].as_str().unwrap().to_string();
    assert!(first.starts_with("lab_"));

    let second = queue(&env, &other, "b&w").await;
    assert_eq!(second["queue_position"], 2);

    let post = |item: &str, body: Value| {
        let app = env.app.clone();
        let uri = format!("/v1/media/items/{item}/lab");
        async move { json_of(&app, "POST", &uri, Some(body)).await }
    };
    assert_eq!(
        post(&item, json!({"prompt": "   "})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&item, json!({"prompt": "x".repeat(1001)})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&item, json!({"prompt": "x", "steps": 0})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&item, json!({"prompt": "x", "reference_item_id": item}))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "an item cannot be its own reference"
    );
    assert_eq!(
        post(
            &item,
            json!({"prompt": "x", "reference_item_id": "itm_nope"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post("itm_nope", json!({"prompt": "x"})).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(
        json_of(
            &env.app,
            "POST",
            &format!("/v1/media/items/{item}/lab"),
            None
        )
        .await
        .0
        .is_client_error(),
        "a body is required"
    );

    // Only pictures that are not in the Trash.
    env.lib
        .db
        .with(|c| {
            c.execute(
                "INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at)
                 SELECT 'itm_video', 'video', 'v', hash, 'video/mp4', 1, sort_at, source, added_at, updated_at FROM items WHERE id = ?1",
                [&item],
            )
        })
        .unwrap();
    assert_eq!(
        post("itm_video", json!({"prompt": "x"})).await.0,
        StatusCode::BAD_REQUEST
    );
    let (s, _) = json_of(
        &env.app,
        "DELETE",
        &format!("/v1/media/items/{other}"),
        None,
    )
    .await;
    assert!(s.is_success());
    assert_eq!(
        post(&other, json!({"prompt": "x"})).await.0,
        StatusCode::BAD_REQUEST
    );

    // The list: the queue in order, filters, and the status check.
    let (s, list) = json_of(&env.app, "GET", "/v1/media/lab", None).await;
    assert_eq!(s, StatusCode::OK);
    let jobs = list["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["id"], first, "the queue runs oldest first");
    let (_, only) = json_of(
        &env.app,
        "GET",
        &format!("/v1/media/lab?item_id={item}"),
        None,
    )
    .await;
    assert_eq!(only["jobs"].as_array().unwrap().len(), 1);
    let (_, none) = json_of(&env.app, "GET", "/v1/media/lab?status=done,failed", None).await;
    assert!(none["jobs"].as_array().unwrap().is_empty());
    assert_eq!(
        json_of(&env.app, "GET", "/v1/media/lab?status=bogus", None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // A job id is checked before it reaches a path.
    assert_eq!(
        json_of(&env.app, "GET", "/v1/media/lab/lab_..%2F..%2Fetc", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        json_of(
            &env.app,
            "GET",
            &format!("/v1/media/lab/lab_{}", "0".repeat(32)),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_worker_runs_a_job_and_serves_before_and_after() {
    let env = setup();
    let item = add_sized(&env, "wide.png", 200, 100, 30);
    let other = add(&env, "ref.png", 40);
    let runner = fake_runner(env.tmp.path(), "");
    let mut cfg = cfg_for(&runner);
    cfg.max_side = 100;

    let (s, v) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/items/{item}/lab"),
        Some(json!({"prompt": "add a red hat, keep the face", "reference_item_id": other, "steps": 6, "seed": 99})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = v["id"].as_str().unwrap().to_string();

    assert!(lab_run::run_next(&env.lib, &cfg).await.unwrap());
    assert!(
        !lab_run::run_next(&env.lib, &cfg).await.unwrap(),
        "nothing left to run"
    );

    let done = job(&env, &id).await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["progress"], 100);
    assert_eq!(done["model"], "fake");
    assert!(done["error"].is_null());
    assert_eq!(
        done["result"]["width"], 100,
        "the input was scaled to the model's size"
    );
    assert_eq!(done["result"]["height"], 50);
    assert_eq!(done["attempts"], 1);
    assert!(done["finished_at"].is_string());
    assert_eq!(done["urls"]["after"], format!("/v1/media/lab/{id}/result"));
    assert_eq!(done["urls"]["before"], format!("/v1/media/lab/{id}/before"));
    assert!(done["urls"]["reference"].as_str().unwrap().contains(&other));

    // What the runner was given: its arguments, one per line (the prompt stays one argument).
    let args = std::fs::read_to_string(env.tmp.path().join("args.txt")).unwrap();
    let lines: Vec<&str> = args.lines().collect();
    let at = |flag: &str| {
        lines
            .iter()
            .position(|l| *l == flag)
            .unwrap_or_else(|| panic!("{flag} missing in {lines:?}"))
    };
    assert_eq!(lines[at("--prompt") + 1], "add a red hat, keep the face");
    assert_eq!(lines[at("--steps") + 1], "6");
    assert_eq!(lines[at("--seed") + 1], "99");
    assert!(lines[at("--input") + 1].ends_with("input.png"));
    assert!(lines[at("--reference") + 1].ends_with("reference.png"));
    assert!(lines[at("--output") + 1].ends_with("run-output.png"));

    // Before: the picture the model was given; after: the result. Both PNG.
    let (s, h, bytes) = call(&env.app, "GET", &format!("/v1/media/lab/{id}/before"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "image/png");
    let dims = image::load_from_memory(&bytes).unwrap();
    assert_eq!((dims.width(), dims.height()), (100, 50));
    let (s, h, bytes) = call(&env.app, "GET", &format!("/v1/media/lab/{id}/result"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "image/png");
    assert_eq!(&bytes[..4], b"\x89PNG");
    assert!(bytes.ends_with(b"lab:add a red hat, keep the face"));
    assert_eq!(
        done["result"]["hash"],
        lucina_media::store::hash_file(&lab::result_path(&env.lib.cfg, &id)).unwrap()
    );

    // The library is untouched until Accept: still two items.
    let (_, items) = json_of(
        &env.app,
        "GET",
        "/v1/media/items?scope=gallery&limit=50",
        None,
    )
    .await;
    assert_eq!(items["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_input_is_the_picture_as_the_owner_sees_it() {
    let env = setup();
    // 200x100 turned 90 degrees clockwise shows as 100x200; the model gets it at most 100 px on the long side.
    let item = add_sized(&env, "turned.png", 200, 100, 31);
    let (s, _) = json_of(
        &env.app,
        "PATCH",
        &format!("/v1/media/items/{item}"),
        Some(json!({"rotation": 90})),
    )
    .await;
    assert!(s.is_success());
    let created = queue(&env, &item, "x").await;
    let id = created["id"].as_str().unwrap();
    let j = env.lib.db.with(|c| lab::get_job(c, id)).unwrap().unwrap();
    let (input, reference) = lab::prepare_inputs(&env.lib, &j, 100).unwrap();
    assert!(reference.is_none());
    let img = image::open(&input).unwrap();
    assert_eq!((img.width(), img.height()), (50, 100));
}

#[tokio::test]
async fn accepting_adds_a_linked_item_and_leaves_the_original_alone() {
    let env = setup();
    let item = add(&env, "orig.png", 50);
    let before = {
        let (_, v) = json_of(&env.app, "GET", &format!("/v1/media/items/{item}"), None).await;
        v
    };
    let original_path = ingest::original_path(&env.lib, &item).unwrap().unwrap().0;
    let original_bytes = std::fs::read(&original_path).unwrap();

    let runner = fake_runner(env.tmp.path(), "");
    let cfg = cfg_for(&runner);
    let id = queue(&env, &item, "paint it blue").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Not done yet: nothing to accept.
    assert_eq!(
        json_of(
            &env.app,
            "POST",
            &format!("/v1/media/lab/{id}/accept"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    lab_run::run_next(&env.lib, &cfg).await.unwrap();

    let (s, out) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/lab/{id}/accept"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["job"]["status"], "accepted");
    assert_eq!(out["duplicate"], false);
    let new_id = out["item"]["id"].as_str().unwrap().to_string();
    assert_ne!(new_id, item);
    assert_eq!(out["job"]["accepted_item_id"], new_id);
    assert_eq!(out["item"]["source"], "lucina-lab");
    assert_eq!(out["item"]["caption"], "paint it blue");
    assert_eq!(out["item"]["title"], "orig (Lucina edit)");
    assert_eq!(out["item"]["links"]["derived_from"], item, "{out}");
    assert_eq!(out["item"]["links"]["lab_job"], id);
    assert_eq!(
        out["item"]["taken_at"], before["taken_at"],
        "it sits next to the original"
    );
    assert!(
        out["item"]["device"].is_null(),
        "not a device asset: no device deletion can ever target it"
    );

    // The original: same bytes, same hash, same version, same device asset; only `links` knows about the edit.
    let (_, after) = json_of(&env.app, "GET", &format!("/v1/media/items/{item}"), None).await;
    assert_eq!(after["hash"], before["hash"]);
    assert_eq!(after["version"], before["version"]);
    assert_eq!(after["device"], before["device"]);
    assert_eq!(after["title"], before["title"]);
    assert_eq!(after["links"]["lab_edits"], json!([new_id]));
    assert_eq!(std::fs::read(&original_path).unwrap(), original_bytes);

    // The edit is in the library (the Lucina group of the Gallery) with its own bytes.
    let (_, lucina) = json_of(
        &env.app,
        "GET",
        "/v1/media/items?scope=gallery&origin=lucina",
        None,
    )
    .await;
    let ids: Vec<&str> = lucina["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [new_id.as_str()]);
    let (s, _, bytes) = call(
        &env.app,
        "GET",
        &format!("/v1/media/items/{new_id}/original"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(bytes.ends_with(b"lab:paint it blue"));

    // The job's scratch files are gone, the job shows the new picture as its "after", and it cannot be accepted
    // or discarded twice.
    assert!(!lab::job_dir(&env.lib.cfg, &id).exists());
    let j = job(&env, &id).await;
    assert!(j["urls"]["after"].as_str().unwrap().contains(&new_id));
    for what in ["accept", "discard", "retry"] {
        assert_eq!(
            json_of(
                &env.app,
                "POST",
                &format!("/v1/media/lab/{id}/{what}"),
                None
            )
            .await
            .0,
            StatusCode::CONFLICT,
            "{what}"
        );
    }
    assert_eq!(
        json_of(&env.app, "DELETE", &format!("/v1/media/lab/{id}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );

    // A second edit of the same original adds to its list of edits.
    let id2 = queue(&env, &item, "and green").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    let (_, out2) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/lab/{id2}/accept"),
        None,
    )
    .await;
    let new2 = out2["item"]["id"].as_str().unwrap();
    let (_, after) = json_of(&env.app, "GET", &format!("/v1/media/items/{item}"), None).await;
    assert_eq!(after["links"]["lab_edits"], json!([new_id, new2]));
}

#[tokio::test]
async fn discarding_deletes_the_result() {
    let env = setup();
    let item = add(&env, "d.png", 60);
    let runner = fake_runner(env.tmp.path(), "");
    let cfg = cfg_for(&runner);
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    let dir = lab::job_dir(&env.lib.cfg, &id);
    assert!(dir.join("result.png").is_file());

    let (s, v) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/lab/{id}/discard"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "discarded");
    assert!(
        !dir.exists(),
        "the result and its scratch files are deleted"
    );
    assert_eq!(
        call(&env.app, "GET", &format!("/v1/media/lab/{id}/result"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Gone from the Lab view, still no way to accept it, and the library has no new item.
    let (_, list) = json_of(&env.app, "GET", "/v1/media/lab", None).await;
    assert!(list["jobs"].as_array().unwrap().is_empty());
    assert_eq!(
        json_of(
            &env.app,
            "POST",
            &format!("/v1/media/lab/{id}/accept"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, items) = json_of(&env.app, "GET", "/v1/media/items?scope=gallery", None).await;
    assert_eq!(items["items"].as_array().unwrap().len(), 1);
    // The record can still be asked for explicitly, and removed.
    let (_, listed) = json_of(&env.app, "GET", "/v1/media/lab?status=discarded", None).await;
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(
        json_of(&env.app, "DELETE", &format!("/v1/media/lab/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_failed_job_says_why_and_can_be_retried() {
    let env = setup();
    let item = add(&env, "f.png", 70);
    let runner = fake_runner(env.tmp.path(), "");
    let cfg = cfg_for(&runner);
    std::fs::write(env.tmp.path().join("fail.flag"), "").unwrap();
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();

    let v = job(&env, &id).await;
    assert_eq!(v["status"], "failed");
    let err = v["error"].as_str().unwrap();
    assert!(
        err.contains("exit 3") && err.contains("boom: out of memory"),
        "{err}"
    );
    assert!(v["urls"]["after"].is_null());
    assert_eq!(v["model"], "fake");

    // Retry puts it back in the queue with the same request; once the runner works it finishes.
    std::fs::remove_file(env.tmp.path().join("fail.flag")).unwrap();
    let (s, r) = json_of(&env.app, "POST", &format!("/v1/media/lab/{id}/retry"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["status"], "queued");
    assert!(r["error"].is_null());
    assert_eq!(
        json_of(&env.app, "POST", &format!("/v1/media/lab/{id}/retry"), None)
            .await
            .0,
        StatusCode::CONFLICT,
        "only a failed job can be retried"
    );
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    let v = job(&env, &id).await;
    assert_eq!(v["status"], "done");
    assert_eq!(v["attempts"], 2);

    // A runner that produces something that is not a picture fails the job too.
    let junk = env.tmp.path().join("junk-lab");
    std::fs::write(
        &junk,
        "#!/bin/bash\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output ] && out=$2; shift; done\necho not an image > \"$out\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&junk, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id3 = queue(&env, &item, "y").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg_for(&junk)).await.unwrap();
    let v = job(&env, &id3).await;
    assert_eq!(v["status"], "failed");
    assert!(
        v["error"].as_str().unwrap().contains("no usable picture"),
        "{v}"
    );

    // A runner that does not exist.
    let id4 = queue(&env, &item, "z").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg_for(&env.tmp.path().join("missing")))
        .await
        .unwrap();
    assert_eq!(job(&env, &id4).await["status"], "failed");
}

#[tokio::test]
async fn cancel_removes_only_idle_jobs() {
    let env = setup();
    let item = add(&env, "c.png", 80);
    let a = queue(&env, &item, "a").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = queue(&env, &item, "b").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        json_of(&env.app, "DELETE", &format!("/v1/media/lab/{a}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        json_of(&env.app, "GET", &format!("/v1/media/lab/{a}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // b moves up, then starts running: a running job cannot be cancelled.
    assert_eq!(job(&env, &b).await["queue_position"], 1);
    let claimed = env.lib.db.with(lab::claim_next).unwrap().unwrap();
    assert_eq!(claimed.id, b);
    assert_eq!(
        json_of(&env.app, "DELETE", &format!("/v1/media/lab/{b}"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let v = job(&env, &b).await;
    assert_eq!(v["status"], "running");
    assert_eq!(v["phase"], "waiting");
}

#[tokio::test]
async fn a_hung_runner_is_stopped() {
    let env = setup();
    let item = add(&env, "h.png", 90);
    // Prints STARTED, then hangs: the run clock (1 s) applies.
    let hang = env.tmp.path().join("hang-lab");
    std::fs::write(&hang, "#!/bin/bash\necho STARTED\nsleep 60\n").unwrap();
    std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = cfg_for(&hang);
    cfg.timeout = Duration::from_secs(1);
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let t = std::time::Instant::now();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
    let v = job(&env, &id).await;
    assert_eq!(v["status"], "failed");
    assert!(v["error"].as_str().unwrap().contains("ran too long"), "{v}");

    // Never prints STARTED: the wait clock plus the run clock apply, and the message says it waited.
    let silent = env.tmp.path().join("silent-lab");
    std::fs::write(&silent, "#!/bin/bash\nsleep 60\n").unwrap();
    std::fs::set_permissions(&silent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = cfg_for(&silent);
    cfg.timeout = Duration::from_millis(500);
    cfg.wait_timeout = Duration::from_millis(500);
    let id = queue(&env, &item, "y").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    let v = job(&env, &id).await;
    assert_eq!(v["status"], "failed");
    assert!(
        v["error"].as_str().unwrap().contains("waited too long"),
        "{v}"
    );
}

#[tokio::test]
async fn the_worker_runs_jobs_one_at_a_time_in_order_and_resumes_after_a_restart() {
    let env = setup();
    let item = add(&env, "w.png", 100);
    let log = env.tmp.path().join("order.log");
    // Each run logs when it starts and ends, with a pause in between: two jobs overlapping would interleave.
    let runner = fake_runner(
        env.tmp.path(),
        &format!(
            "echo \"start $prompt\" >> {l}; sleep 0.4; echo \"end $prompt\" >> {l}",
            l = log.display()
        ),
    );
    let a = queue(&env, &item, "first").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = queue(&env, &item, "second").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // A job a crashed process left running goes back to the queue when the worker starts.
    let c = queue(&env, &item, "third").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    env.lib
        .db
        .with(|conn| conn.execute("UPDATE lab_jobs SET status = 'running' WHERE id = ?1", [&c]))
        .unwrap();

    let worker = tokio::spawn(lab_run::worker(env.lib.clone(), cfg_for(&runner)));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (_, list) = json_of(&env.app, "GET", "/v1/media/lab?status=done", None).await;
        if list["jobs"].as_array().unwrap().len() == 3 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "jobs did not finish: {list}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    worker.abort();

    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "start first",
            "end first",
            "start second",
            "end second",
            "start third",
            "end third"
        ],
        "one at a time, oldest first"
    );
    for id in [&a, &b, &c] {
        assert_eq!(job(&env, id).await["status"], "done");
    }
}

#[tokio::test]
async fn a_queued_edit_waits_when_no_runner_is_configured() {
    let env = setup();
    let item = add(&env, "n.png", 110);
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let cfg = LabConfig {
        cmd: None,
        ..cfg_for(Path::new("/nonexistent"))
    };
    assert!(!lab_run::run_next(&env.lib, &cfg).await.unwrap());
    assert_eq!(job(&env, &id).await["status"], "queued");
}

#[tokio::test]
async fn purging_the_original_does_not_strand_its_jobs() {
    let env = setup();
    let item = add(&env, "p.png", 120);
    let runner = fake_runner(env.tmp.path(), "");
    let cfg = cfg_for(&runner);
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();

    let (s, _) = json_of(
        &env.app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "trash", "ids": [item]})),
    )
    .await;
    assert!(s.is_success());
    let (s, v) = json_of(
        &env.app,
        "POST",
        "/v1/media/items/batch",
        Some(json!({"op": "purge", "ids": [item]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["done"], json!([item]), "{v}");

    let j = job(&env, &id).await;
    assert_eq!(j["status"], "done");
    assert!(j["original"].is_null());
    // The result can still be accepted; it is a plain new item with no original to link back to.
    let (s, out) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/lab/{id}/accept"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["item"]["title"], "Picture (Lucina edit)");
}

// ---- telling the owner ------------------------------------------------------------------------------------------

type Seen = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;

/// A mock secretary-server and Telegram on one port: records every POST (path, bearer token, JSON body).
async fn mock_notifier() -> (String, Seen) {
    async fn record(
        State(seen): State<Seen>,
        uri: Uri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        seen.lock()
            .unwrap()
            .push((uri.path().to_owned(), auth, body));
        Json(json!({"status": "created", "ok": true}))
    }
    let seen: Seen = Arc::default();
    let app = Router::new()
        .fallback(post(record))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test]
async fn the_owner_is_told_when_a_picture_is_ready_or_failed() {
    let env = setup();
    let item = add(&env, "t.png", 130);
    let runner = fake_runner(env.tmp.path(), "");
    let (base, seen) = mock_notifier().await;
    let token = env.tmp.path().join("svc.token");
    std::fs::write(&token, "svc_secret_token\n").unwrap();
    let mut cfg = cfg_for(&runner);
    cfg.notify = NotifyConfig {
        secretary_url: Some(base.clone()),
        service_token_file: Some(token),
        telegram_token: Some("123:bot".into()),
        telegram_chat: Some("42".into()),
        telegram_api: base,
        link_base: "https://secretary.example".into(),
        lang: "en".into(),
    };

    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        let (path, auth, body) = &seen[0];
        assert_eq!(path, "/v1/notify");
        assert_eq!(auth.as_deref(), Some("Bearer svc_secret_token"));
        assert_eq!(body["category"], "lucina_message");
        assert_eq!(body["body"], "Your picture is ready");
        assert_eq!(body["route"], "/gallery/lab");
        assert_eq!(body["channel"], "lab");
        assert_eq!(body["dedupKey"], format!("lab:{id}"));
        assert_eq!(body["data"]["labJob"], id);
        let (path, _, body) = &seen[1];
        assert_eq!(path, "/bot123:bot/sendMessage");
        assert_eq!(body["chat_id"], "42");
        let text = body["text"].as_str().unwrap();
        assert!(
            text.contains("Your picture is ready")
                && text.contains("https://secretary.example/gallery/lab"),
            "{text}"
        );
    }
    // It is announced once: marked in the job, so a second pass has nothing to say.
    let marked = env.lib.db.with(|c| lab::get_job(c, &id)).unwrap().unwrap();
    assert!(marked.notified_at.is_some());
    assert!(!env.lib.db.with(|c| lab::mark_notified(c, &id)).unwrap());

    // A failure is announced too (Italian here).
    std::fs::write(env.tmp.path().join("fail.flag"), "").unwrap();
    cfg.notify.lang = "it".into();
    let id2 = queue(&env, &item, "y").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert_eq!(
        seen[2].2["body"],
        "Non sono riuscita a creare la tua immagine"
    );
    assert_eq!(seen[2].2["dedupKey"], format!("lab:{id2}"));
}

#[tokio::test]
async fn a_notification_failure_does_not_fail_the_job() {
    let env = setup();
    let item = add(&env, "u.png", 140);
    let runner = fake_runner(env.tmp.path(), "");
    let mut cfg = cfg_for(&runner);
    let token = env.tmp.path().join("svc.token");
    std::fs::write(&token, "svc_x").unwrap();
    cfg.notify.secretary_url = Some("http://127.0.0.1:1".into());
    cfg.notify.service_token_file = Some(token);
    let id = queue(&env, &item, "x").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(&env.lib, &cfg).await.unwrap();
    assert_eq!(job(&env, &id).await["status"], "done");
    assert!(lab_run::notify(&cfg.notify, &id, true).await.is_err());
    // Nothing configured is not an error.
    assert!(lab_run::notify(&NotifyConfig::default(), &id, true)
        .await
        .unwrap()
        .is_empty());
}

/// The real model, end to end: only with `LUCINA_MEDIA_LAB_REAL=1` (it needs nezopt's `~/models/lab` and the GPU,
/// about 20 s). It reads a synthetic picture from `~/models/lab/tests/src-portrait.png`, never a library photo.
#[tokio::test]
async fn the_real_model_edits_a_synthetic_picture() {
    if std::env::var_os("LUCINA_MEDIA_LAB_REAL").is_none() {
        eprintln!("not run: set LUCINA_MEDIA_LAB_REAL=1 to run the local model");
        return;
    }
    let home = std::env::var("HOME").unwrap();
    let source = PathBuf::from(&home).join("models/lab/tests/src-portrait.png");
    let env = setup();
    let item = {
        let client = ClientMeta {
            filename: Some("portrait.png".into()),
            source: Some("upload".into()),
            ..Default::default()
        };
        let src = env.tmp.path().join("portrait.png");
        std::fs::copy(&source, &src).unwrap();
        ingest::ingest_file(&env.lib, &src, None, &client, None)
            .unwrap()
            .item_id
    };
    let cfg = cfg_for(&PathBuf::from(&home).join("models/lab/lab-edit"));
    let id = queue(
        &env,
        &item,
        "Add round black-rimmed glasses. Keep the face, hair and sweater exactly the same.",
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    lab_run::run_next(
        &env.lib,
        &LabConfig {
            timeout: Duration::from_secs(900),
            ..cfg
        },
    )
    .await
    .unwrap();
    let v = job(&env, &id).await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["model"], "klein4b");
    assert_eq!(
        (
            v["result"]["width"].as_i64(),
            v["result"]["height"].as_i64()
        ),
        (Some(1024), Some(768))
    );
    let (s, _, bytes) = call(&env.app, "GET", &format!("/v1/media/lab/{id}/result"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(bytes.len() > 100_000);
}
