//! The background pipeline against the Meshy mock and a fake gallery:
//! success, polling/timeout, failed tasks, error mapping, budget.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use lucina_meshy::budget::Limits;
use lucina_meshy::key::{Secret, StaticKey};
use lucina_meshy::mock::{self, MockConfig, MockHandle};
use lucina_meshy::{plan, Args, MeshyConfig, MeshyService, Origin};
use lucina_store::{JobStore, TimelineStore};
use serde_json::{json, Value};

const SVC: &str = "svc_test_service_token_0123456789";

/// A minimal `/v1/models` (create, put file, get, list) plus `/v1/notify`.
#[derive(Default)]
struct FakeGallery {
    models: Mutex<HashMap<String, Value>>,
    files: Mutex<HashMap<(String, String), usize>>,
    notified: Mutex<Vec<Value>>,
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {SVC}"))
}

async fn spawn_gallery() -> (String, Arc<FakeGallery>) {
    let st = Arc::new(FakeGallery::default());
    async fn create(
        State(st): State<Arc<FakeGallery>>,
        h: HeaderMap,
        Json(mut b): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        if !authed(&h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        let id = format!(
            "mdl_{}",
            uuid::Uuid::new_v4().simple().to_string().get(..16).unwrap()
        );
        b["id"] = json!(id);
        b["files"] = json!([]);
        st.models.lock().unwrap().insert(id, b.clone());
        (StatusCode::CREATED, Json(json!({"model": b})))
    }
    async fn put_file(
        State(st): State<Arc<FakeGallery>>,
        h: HeaderMap,
        UrlPath((id, name)): UrlPath<(String, String)>,
        body: Bytes,
    ) -> (StatusCode, Json<Value>) {
        if !authed(&h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        let mut models = st.models.lock().unwrap();
        let Some(m) = models.get_mut(&id) else {
            return (StatusCode::NOT_FOUND, Json(json!({})));
        };
        m["files"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": name, "bytes": body.len()}));
        st.files.lock().unwrap().insert((id, name), body.len());
        (StatusCode::OK, Json(json!({"model": m.clone()})))
    }
    async fn get_one(
        State(st): State<Arc<FakeGallery>>,
        UrlPath(id): UrlPath<String>,
    ) -> (StatusCode, Json<Value>) {
        match st.models.lock().unwrap().get(&id) {
            Some(m) => (StatusCode::OK, Json(json!({"model": m}))),
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": {"code": "not_found"}})),
            ),
        }
    }
    async fn list(State(st): State<Arc<FakeGallery>>) -> Json<Value> {
        Json(json!({"models": st.models.lock().unwrap().values().cloned().collect::<Vec<_>>()}))
    }
    async fn notify(State(st): State<Arc<FakeGallery>>, Json(b): Json<Value>) -> Json<Value> {
        st.notified.lock().unwrap().push(b);
        Json(json!({"status": "created"}))
    }
    let app = Router::new()
        .route("/v1/models", post(create).get(list))
        .route("/v1/models/{id}", get(get_one))
        .route("/v1/models/{id}/files/{name}", put(put_file))
        .route("/v1/notify", post(notify))
        .with_state(st.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, st)
}

struct Rig {
    svc: Arc<MeshyService>,
    mock: MockHandle,
    gallery: Arc<FakeGallery>,
    jobs: Arc<JobStore>,
    timeline: Arc<TimelineStore>,
    _dir: tempfile::TempDir,
}

fn fixtures(dir: &Path) -> std::path::PathBuf {
    // Tiny valid GLB headers are enough: the pipeline checks the magic, not the scene.
    let f = dir.join("fixtures");
    std::fs::create_dir_all(&f).unwrap();
    for name in [
        "chest_preview.glb",
        "chest.glb",
        "knight.glb",
        "knight_walk.glb",
        "knight_anim.glb",
    ] {
        let mut b = b"glTF".to_vec();
        b.extend_from_slice(&[2, 0, 0, 0, 20, 0, 0, 0]);
        b.extend_from_slice(name.as_bytes());
        std::fs::write(f.join(name), b).unwrap();
    }
    f
}

async fn rig(limits: Limits, step_timeout: Duration) -> Rig {
    rig_with(limits, step_timeout, 0).await
}

async fn rig_with(limits: Limits, step_timeout: Duration, overcharge: u32) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mock = mock::spawn(MockConfig {
        assets: fixtures(dir.path()),
        polls_to_finish: 2,
        overcharge,
        ..MockConfig::default()
    })
    .await
    .unwrap();
    let (gurl, gallery) = spawn_gallery().await;
    let token = dir.path().join("svc.token");
    std::fs::write(&token, SVC).unwrap();
    let get = |k: &str| (k == "LUCINA_MESHY").then(|| "true".to_owned());
    let mut cfg = MeshyConfig::from_lookup(
        &get,
        dir.path(),
        chrono_tz::Europe::Rome,
        Some(gurl),
        Some(token),
    );
    cfg.api_base = mock.api_base();
    cfg.limits = limits;
    cfg.poll = Duration::from_millis(5);
    cfg.poll_min = Duration::from_millis(1);
    cfg.step_timeout = step_timeout;
    cfg.preview.enabled = false;
    let jobs = Arc::new(JobStore::new(dir.path().join("jobs.json")));
    let timeline = Arc::new(TimelineStore::new(
        dir.path().join("timeline.jsonl"),
        1 << 20,
        true,
    ));
    let svc = Arc::new(MeshyService::new(
        cfg,
        Arc::new(StaticKey(Secret::new("msy_test_key_for_mock_0000"))),
        jobs.clone(),
        timeline.clone(),
    ));
    Rig {
        svc,
        mock,
        gallery,
        jobs,
        timeline,
        _dir: dir,
    }
}

async fn wait_done(r: &Rig, id: &str) -> Value {
    for _ in 0..400 {
        let j = r.svc.job(id).unwrap();
        if j["status"] == "done" || j["status"] == "error" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish: {}", r.svc.job(id).unwrap());
}

fn origin() -> Origin {
    Origin {
        channel: "web".into(),
        session_id: "secretary_app".into(),
        reply_to: None,
        lang: "it".into(),
    }
}

fn args(v: Value) -> Args {
    Args::from_value(&v).unwrap()
}

#[tokio::test]
async fn character_pipeline_end_to_end() {
    let r = rig(Limits::new(100, 60), Duration::from_secs(10)).await;
    let a = args(
        json!({"action": "text_to_3d", "prompt": "a knight in silver armor", "animations": ["walk", "wave"], "formats": ["fbx", "usdz"]}),
    );
    let p = r.svc.plan(&a).await.unwrap();
    assert_eq!(p.total, 41);
    let job = r.svc.start(p, origin()).await.unwrap();
    let j = wait_done(&r, &job.id).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["credits"], json!({"estimate": 41, "consumed": 41}));
    // every step created once, in order, chained by task ids
    for (path, n) in [
        ("/openapi/v2/text-to-3d", 2),
        ("/openapi/v1/rigging", 1),
        ("/openapi/v1/animations", 1),
    ] {
        assert_eq!(r.mock.creates(path), n, "{path}");
    }
    let reqs = r.mock.state.requests.lock().unwrap().clone();
    let anim = reqs
        .iter()
        .find(|q| q["path"] == "/openapi/v1/animations" && q["method"] == "POST")
        .unwrap();
    assert_eq!(anim["body"]["action_ids"], json!([613, 290]));
    let refine = reqs.iter().find(|q| q["body"]["mode"] == "refine").unwrap();
    assert_eq!(
        refine["body"]["target_formats"],
        json!(["glb", "fbx", "usdz"])
    );
    // gallery: GLB (the animated one), thumbnail, extra formats
    let model_id = j["modelId"].as_str().unwrap().to_owned();
    let files = r.gallery.files.lock().unwrap().clone();
    for f in ["model.glb", "thumb.png", "model.fbx", "model.usdz"] {
        assert!(
            files.contains_key(&(model_id.clone(), f.to_owned())),
            "{f}: {files:?}"
        );
    }
    let m = r.gallery.models.lock().unwrap()[&model_id].clone();
    assert_eq!(m["animations"], json!(["Casual Walk", "Wave One Hand"]));
    assert!(m["meshy"]["rigTaskId"].is_string());
    assert_eq!(m["meshy"]["taskKind"], "text-to-3d");
    // timeline: a model3d block; the app was notified; the ledger settled
    let items = r.timeline.read_all().unwrap();
    let last: Value = serde_json::from_str(&items.last().unwrap().raw).unwrap();
    assert_eq!(last["meta"]["format"], "v1");
    assert_eq!(last["meta"]["blocks"][0]["type"], "model3d");
    assert_eq!(last["meta"]["blocks"][0]["id"], json!(model_id));
    assert!(last["text"].as_str().unwrap().contains("Il tuo modello 3D"));
    assert_eq!(r.gallery.notified.lock().unwrap().len(), 1);
    // keyed by the timeline item, so secretary-server's ingester does not notify twice
    assert_eq!(
        r.gallery.notified.lock().unwrap()[0]["timelineId"],
        last["id"]
    );
    assert_eq!(r.svc.ledger.entry(&job.id).unwrap().consumed, Some(41));
    assert_eq!(r.jobs.find(&job.id).unwrap().unwrap().status, "done");
}

#[tokio::test]
async fn polling_times_out_without_recreating_the_task() {
    let r = rig(Limits::new(100, 60), Duration::from_millis(150)).await;
    let p = r
        .svc
        .plan(&args(
            json!({"action": "text_to_3d", "prompt": "a lamp [never]", "texture": false}),
        ))
        .await
        .unwrap();
    let job = r.svc.start(p, origin()).await.unwrap();
    let j = wait_done(&r, &job.id).await;
    assert_eq!(j["status"], "error");
    assert!(
        j["error"].as_str().unwrap().contains("not re-created"),
        "{j}"
    );
    assert_eq!(r.mock.creates("/openapi/v2/text-to-3d"), 1);
    let polls = r
        .mock
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|q| q["method"] == "GET")
        .count();
    assert!(polls >= 2, "it polled ({polls})");
}

#[tokio::test]
async fn failed_tasks_and_create_errors() {
    let r = rig(Limits::new(200, 60), Duration::from_secs(10)).await;
    // FAILED task: Meshy refunds it, the job spends 0
    let job = r
        .svc
        .start(
            r.svc
                .plan(&args(
                    json!({"action": "text_to_3d", "prompt": "a vase [fail]"}),
                ))
                .await
                .unwrap(),
            origin(),
        )
        .await
        .unwrap();
    let j = wait_done(&r, &job.id).await;
    assert_eq!(j["status"], "error");
    assert!(j["error"].as_str().unwrap().contains("FAILED"), "{j}");
    assert_eq!(r.svc.ledger.entry(&job.id).unwrap().consumed, Some(0));
    // 402 on create
    let job = r
        .svc
        .start(
            r.svc
                .plan(&args(
                    json!({"action": "text_to_3d", "prompt": "a cup [402]"}),
                ))
                .await
                .unwrap(),
            origin(),
        )
        .await
        .unwrap();
    let j = wait_done(&r, &job.id).await;
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .contains("Not enough Meshy credits"),
        "{j}"
    );
    // 5xx on create: never resent
    let before = r.mock.creates("/openapi/v2/text-to-3d");
    let job = r
        .svc
        .start(
            r.svc
                .plan(&args(
                    json!({"action": "text_to_3d", "prompt": "a cup [500]"}),
                ))
                .await
                .unwrap(),
            origin(),
        )
        .await
        .unwrap();
    let j = wait_done(&r, &job.id).await;
    assert!(j["error"].as_str().unwrap().contains("not resent"), "{j}");
    assert_eq!(r.mock.creates("/openapi/v2/text-to-3d"), before + 1);
    // 429 queue full
    let job = r
        .svc
        .start(
            r.svc
                .plan(&args(
                    json!({"action": "text_to_3d", "prompt": "a cup [429]"}),
                ))
                .await
                .unwrap(),
            origin(),
        )
        .await
        .unwrap();
    let j = wait_done(&r, &job.id).await;
    assert!(j["error"].as_str().unwrap().contains("queue"), "{j}");
}

#[tokio::test]
async fn budget_and_key_gate_the_start() {
    let r = rig(Limits::new(45, 45), Duration::from_secs(10)).await;
    let p = r
        .svc
        .plan(&args(
            json!({"action": "text_to_3d", "prompt": "knight", "animations": ["walk"]}),
        ))
        .await
        .unwrap();
    assert_eq!(p.total, 38);
    let first = r.svc.start(p.clone(), origin()).await.unwrap();
    let err = r.svc.start(p.clone(), origin()).await.unwrap_err();
    assert!(err.contains("budget"), "{err}");
    wait_done(&r, &first.id).await;
    let big = r.svc.plan(&args(json!({"action": "text_to_3d", "prompt": "knight", "animations": ["walk", "wave", "bow", "dance", "cheer"]}))).await.unwrap();
    assert!(r.svc.check(&big).is_err());
    assert_eq!(
        r.mock.creates("/openapi/v2/text-to-3d"),
        2,
        "the refused plan made no request"
    );
    // post-processing goes through the gallery model
    let model = r
        .gallery
        .models
        .lock()
        .unwrap()
        .keys()
        .next()
        .cloned()
        .unwrap();
    let p = r
        .svc
        .plan(&args(
            json!({"action": "animate", "model_id": model, "animations": ["bow"]}),
        ))
        .await
        .unwrap();
    assert_eq!(p.total, 3, "already rigged");
    assert!(matches!(&p.spec.source, plan::Source::Input(i) if i.rig_task_id.is_some()));
}

/// G4 (docs/verification.md): the daily cap was checked against the plan's
/// estimate, and nothing stopped a job when Meshy charged more than that.
/// With prices 2 credits above the table per task, the knight (estimate 38)
/// would have cost 46 against a 45-credit day. Now the plan reserves every
/// step's worst case (43) and the job stops as soon as a step is charged
/// more than its worst case, so the day stays within its cap.
#[tokio::test]
async fn meshy_charges_stay_within_the_reservation() {
    let r = rig_with(Limits::new(45, 45), Duration::from_secs(10), 2).await;
    let p = r
        .svc
        .plan(&args(
            json!({"action": "text_to_3d", "prompt": "knight", "animations": ["walk"]}),
        ))
        .await
        .unwrap();
    assert_eq!((p.total, p.reservation()), (38, 43));
    let job = r.svc.start(p, origin()).await.unwrap();
    let done = wait_done(&r, &job.id).await;
    assert_eq!(done["status"], "error", "{done}");
    assert!(
        done["error"].as_str().unwrap().contains("charged 12"),
        "{done}"
    );
    assert_eq!(
        r.mock.creates("/openapi/v1/rigging"),
        0,
        "stopped before rigging"
    );
    let charged = r.svc.ledger.entry(&job.id).unwrap().consumed.unwrap();
    assert_eq!(charged, 22 + 12);
    assert!(r.svc.ledger.spent_today(chrono::Utc::now()) <= 45);

    // Within the margin (1 over per task) the same plan completes.
    let r = rig_with(Limits::new(45, 45), Duration::from_secs(10), 1).await;
    let p = r
        .svc
        .plan(&args(
            json!({"action": "text_to_3d", "prompt": "knight", "animations": ["walk"]}),
        ))
        .await
        .unwrap();
    let job = r.svc.start(p, origin()).await.unwrap();
    assert_eq!(wait_done(&r, &job.id).await["status"], "done");
    assert_eq!(r.svc.ledger.entry(&job.id).unwrap().consumed, Some(42));
}
