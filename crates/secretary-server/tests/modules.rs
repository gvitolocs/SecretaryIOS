//! End-to-end tests of the hosted engines (`/v1/gym/*`, `/v1/body/*`) and
//! the service-token insights route, on the real router and real files.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Server {
    base: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Server {
    async fn start(cfg: &Config) -> Self {
        // A restarted server must wait until the previous one released the
        // engine file locks.
        let mut state = AppState::new(cfg);
        for _ in 0..100 {
            if state.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            state = AppState::new(cfg);
        }
        let state = state.expect("server state");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            serve_on(listener, app(state), async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Self {
            base,
            stop: Some(tx),
            task,
        }
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

struct Harness {
    server: Option<Server>,
    cfg: Config,
    http: Client,
    token: String,
    service: Option<String>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(with_service: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let service = if with_service {
            let path = dir.path().join("service.token");
            secretary_server::service::write_new(&path, false).unwrap();
            cfg.service_token = Some(
                secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
            );
            Some(std::fs::read_to_string(&path).unwrap().trim().to_owned())
        } else {
            None
        };
        let server = Server::start(&cfg).await;
        Self {
            server: Some(server),
            cfg,
            // no idle keep-alive: a stopped server must drop its state
            http: Client::builder().pool_max_idle_per_host(0).build().unwrap(),
            token,
            service,
            _dir: dir,
        }
    }

    fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }

    async fn restart(&mut self) {
        self.server.take().unwrap().stop().await;
        self.server = Some(Server::start(&self.cfg).await);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.as_ref().unwrap().base)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let resp = rb.send().await.unwrap();
        let status = resp.status();
        (status, resp.json::<Value>().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.http.get(self.url(path)).bearer_auth(&self.token))
            .await
    }

    async fn post(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        self.send(
            self.http
                .post(self.url(path))
                .bearer_auth(&self.token)
                .json(body),
        )
        .await
    }

    async fn revision(&self, module: &str) -> u64 {
        let (s, v) = self.get(&format!("/v1/{module}/state")).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        revision_of(module, &v)
    }

    /// One write with a fresh op_id at the current revision.
    async fn command(&self, module: &str, command: Value) -> (StatusCode, Value) {
        let rev = self.revision(module).await;
        let req = json!({"op_id": uuid(), "expected_revision": rev, "timezone": "Europe/Rome", "command": command});
        self.post(&format!("/v1/{module}/command"), &req).await
    }

    async fn ok(&self, module: &str, command: Value) -> Value {
        let (s, v) = self.command(module, command).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn login(&self) -> String {
        let resp = self
            .http
            .post(self.url("/v1/session"))
            .json(&json!({"token": self.token}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = resp.headers()["set-cookie"].to_str().unwrap().to_owned();
        cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix("sec_session=")
            .unwrap()
            .to_owned()
    }
}

fn uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn revision_of(module: &str, response: &Value) -> u64 {
    let v = if module == "gym" {
        &response["view"]["state"]["revision"]
    } else {
        &response["view"]["revision"]
    };
    v.as_u64()
        .unwrap_or_else(|| panic!("no revision in {response}"))
}

fn mode(path: &PathBuf) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// -- auth -------------------------------------------------------------------

#[tokio::test]
async fn module_routes_need_device_auth() {
    let h = Harness::new(true).await;
    for (method, path) in [
        (Method::GET, "/v1/gym/state"),
        (Method::GET, "/v1/gym/clock"),
        (Method::POST, "/v1/gym/command"),
        (Method::GET, "/v1/body/state"),
        (Method::POST, "/v1/body/command"),
    ] {
        let (s, v) = h
            .send(
                h.http
                    .request(method.clone(), h.url(path))
                    .json(&json!({"command": {"type": "snapshot"}})),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(v["error"]["code"], "unauthorized");
        // the service token is not a device token
        let (s, _) = h
            .send(
                h.http
                    .request(method.clone(), h.url(path))
                    .bearer_auth(h.service.as_ref().unwrap())
                    .json(&json!({"command": {"type": "snapshot"}})),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "svc on {method} {path}");
    }
}

#[tokio::test]
async fn cookie_writes_need_the_csrf_header() {
    let h = Harness::new(false).await;
    let sid = h.login().await;
    let browser = |method: Method, path: &str| {
        h.http
            .request(method, h.url(path))
            .header("cookie", format!("sec_session={sid}"))
    };
    let (s, v) = h.send(browser(Method::GET, "/v1/body/state")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let req = json!({"op_id": "web-1", "expected_revision": 0, "command": {"type": "log_entry", "weight_grams": 80500}});
    let (s, v) = h
        .send(browser(Method::POST, "/v1/body/command").json(&req))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "csrf");
    let (s, v) = h
        .send(
            browser(Method::POST, "/v1/body/command")
                .header("x-secretary-csrf", "1")
                .json(&req),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["view"]["entries"][0]["weight_grams"], 80500);
}

// -- engine protocol over HTTP ----------------------------------------------

#[tokio::test]
async fn gym_state_commands_and_status_mapping() {
    let h = Harness::new(false).await;
    let (s, v) = h.get("/v1/gym/state?timezone=Europe/Rome").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], true);
    assert_eq!(v["view"]["state"]["revision"], 0);
    assert!(v["view"]["state"]["exercises"].as_array().unwrap().len() >= 30);
    assert_eq!(v["view"]["state"]["receipts"], json!({}));

    let start = json!({"op_id": "start-1", "expected_revision": 0, "timezone": "UTC",
                       "command": {"type": "start_workout", "title": "Push"}});
    let (s, v) = h.post("/v1/gym/command", &start).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["replayed"], false);
    assert_eq!(v["view"]["state"]["active"]["title"], "Push");

    // stale revision → 409 with the engine body
    let stale = json!({"op_id": "rename-1", "expected_revision": 0,
                       "command": {"type": "rename_workout", "title": "Pull"}});
    let (s, v) = h.post("/v1/gym/command", &stale).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v,
        json!({"ok": false, "replayed": false, "code": "conflict",
               "error": "State changed. Refresh and retry the edit."})
    );
    // same op_id, different command → 422 op_id_reused
    let mut reused = start.clone();
    reused["command"]["title"] = json!("Other");
    let (s, v) = h.post("/v1/gym/command", &reused).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["code"], "op_id_reused");
    // validation → 422 invalid_command
    let (s, v) = h
        .command("gym", json!({"type": "start_workout", "title": "Second"}))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["code"], "invalid_command");
    assert!(v["error"].as_str().unwrap().contains("current workout"));
    // malformed JSON is the engine's invalid_command too
    let (s, v) = h
        .send(
            h.http
                .post(h.url("/v1/gym/command"))
                .bearer_auth(&h.token)
                .header("content-type", "application/json")
                .body("{not json"),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["code"], "invalid_command");
    // no JSON content type → 415 (server error shape)
    let (s, v) = h
        .send(
            h.http
                .post(h.url("/v1/gym/command"))
                .bearer_auth(&h.token)
                .body(start.to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(v["error"]["code"], "unsupported_media_type");
    // clock: GET route and POST query without op_id
    let (s, v) = h.get("/v1/gym/clock").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["clock"]["active_elapsed_seconds"].is_u64());
    assert!(v.get("view").is_none());
    let (s, v) = h
        .post("/v1/gym/command", &json!({"command": {"type": "clock"}}))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["clock"]["active_duration"].is_string());
    // parse_weight query
    let (s, v) = h
        .post(
            "/v1/gym/command",
            &json!({"command": {"type": "parse_weight", "value": "100", "unit": "lb"}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["weight_grams"], 45359);
    // unknown timezone → 422
    let (s, v) = h.get("/v1/gym/state?timezone=Mars/Olympus").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["code"], "invalid_command");
    assert_eq!(h.revision("gym").await, 1);
}

#[tokio::test]
async fn body_state_and_commands() {
    let h = Harness::new(false).await;
    let (s, v) = h.get("/v1/body/state").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["view"]["revision"], 0);
    assert_eq!(v["view"]["stats"]["count"], 0);
    h.ok("body", json!({"type": "log_entry", "weight": "80.5"}))
        .await;
    let v = h
        .ok(
            "body",
            json!({"type": "update_settings", "unit": "kg", "goal_grams": 78000}),
        )
        .await;
    assert_eq!(v["view"]["stats"]["avg7_grams"], 80500);
    assert_eq!(v["view"]["stats"]["goal_distance_grams"], 2500);
    assert_eq!(v["view"]["stat_labels"]["goal_distance"], "+2.5 kg");
    let (s, v) = h
        .command("body", json!({"type": "log_entry", "weight_grams": 999}))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["code"], "invalid_command");
}

/// A phone replays its offline queue: every resend is a no-op success.
#[tokio::test]
async fn offline_queue_replay_is_idempotent_across_restarts() {
    let mut h = Harness::new(false).await;
    let queue: Vec<Value> = [80_100, 80_200, 80_300]
        .iter()
        .enumerate()
        .map(|(i, g)| {
            json!({"op_id": format!("phone-{i}"), "expected_revision": i, "timezone": "Europe/Rome",
                   "command": {"type": "log_entry", "weight_grams": g}})
        })
        .collect();
    for req in &queue {
        let (s, v) = h.post("/v1/body/command", req).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["replayed"], false);
    }
    let state = |v: &Value| v["view"]["entries"].clone();
    let (_, before) = h.get("/v1/body/state").await;
    for req in queue.iter().chain(queue.iter().rev()) {
        let (s, v) = h.post("/v1/body/command", req).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["replayed"], true);
        assert_eq!(v["view"]["revision"], 3);
        assert_eq!(state(&v), state(&before));
    }

    // gym: start + rename queued, sent twice
    let gym_queue = [
        json!({"op_id": "g-0", "expected_revision": 0, "command": {"type": "start_workout", "title": "Legs"}}),
        json!({"op_id": "g-1", "expected_revision": 1, "command": {"type": "rename_workout", "title": "Leg day"}}),
    ];
    for req in gym_queue.iter().chain(gym_queue.iter()) {
        let (s, v) = h.post("/v1/gym/command", req).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    assert_eq!(h.revision("gym").await, 2);

    h.restart().await;
    let gym_file = h.data_dir().join("gym.json");
    let body_file = h.data_dir().join("body.json");
    assert_eq!(mode(&gym_file), 0o600);
    assert_eq!(mode(&body_file), 0o600);
    let (_, after) = h.get("/v1/body/state").await;
    assert_eq!(state(&after), state(&before));
    for req in &queue {
        let (s, v) = h.post("/v1/body/command", req).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["replayed"], true);
    }
    let (s, v) = h.post("/v1/gym/command", &gym_queue[0]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["replayed"], true);
    assert_eq!(v["view"]["state"]["active"]["title"], "Leg day");
    assert_eq!(h.revision("gym").await, 2);
    assert_eq!(h.revision("body").await, 3);
}

#[tokio::test]
async fn body_limits_per_route() {
    let h = Harness::new(false).await;
    let pad = |n: usize| json!({"command": {"type": "snapshot"}, "pad": "x".repeat(n)});
    let (s, v) = h.post("/v1/body/command", &pad(300 * 1024)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "payload_too_large");
    // the gym route takes restore_json backups up to the engine's 16 MiB
    let (s, v) = h.post("/v1/gym/command", &pad(300 * 1024)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = h.post("/v1/gym/command", &pad(17 * 1024 * 1024)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // other routes keep the global 256 KiB cap
    let (s, _) = h.post("/v1/chat", &pad(300 * 1024)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

// -- insights ---------------------------------------------------------------

async fn finished_workout(h: &Harness, weight: &str, reps: u64) {
    h.ok("gym", json!({"type": "start_workout", "title": "Push"}))
        .await;
    let v = h
        .ok(
            "gym",
            json!({"type": "add_exercise", "exercise_id": "bench-press"}),
        )
        .await;
    let set = v["view"]["state"]["active"]["exercises"][0]["sets"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.ok(
        "gym",
        json!({"type": "edit_set", "set_id": set, "weight": weight, "reps": reps,
               "seconds": 0, "distance_meters": 0, "kind": "normal"}),
    )
    .await;
    h.ok(
        "gym",
        json!({"type": "complete_set", "set_id": set, "completed": true}),
    )
    .await;
    h.ok("gym", json!({"type": "finish_workout"})).await;
    // distinct finish times
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn insights_summary_is_service_only() {
    let h = Harness::new(true).await;
    finished_workout(&h, "80", 5).await;
    finished_workout(&h, "82.5", 5).await;
    h.ok(
        "body",
        json!({"type": "update_settings", "unit": "kg", "goal_grams": 78000}),
    )
    .await;
    h.ok("body", json!({"type": "log_entry", "weight_grams": 80500}))
        .await;

    let svc = h.service.clone().unwrap();
    let (s, v) = h
        .send(
            h.http
                .get(h.url("/v1/insights/summary?timezone=Europe/Rome&workouts=1"))
                .bearer_auth(&svc),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["timezone"], "Europe/Rome");
    assert!(v["generated_ms"].is_u64());
    let gym = &v["gym"];
    assert_eq!(gym["unit"], "kg");
    assert_eq!(gym["recent_workouts"].as_array().unwrap().len(), 1);
    let last = &gym["recent_workouts"][0];
    assert_eq!(last["volume_grams"], 412_500);
    assert_eq!(last["exercises"], json!(["Bench Press"]));
    assert_eq!(last["prs"][0]["weight_grams"], 82_500);
    assert_eq!(last["prs"][0]["label"], "82.5 kg × 5");
    assert_eq!(gym["weekly_volume"].as_array().unwrap().len(), 8);
    assert_eq!(gym["workouts_this_week"], 2);
    assert_eq!(v["body"]["stats"]["avg7_grams"], 80_500);
    assert_eq!(v["body"]["stats"]["goal_distance_grams"], 2_500);
    assert_eq!(v["body"]["stat_labels"]["current"], "80.5 kg");
    let (s, v) = h
        .send(h.http.get(h.url("/v1/insights/summary")).bearer_auth(&svc))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["timezone"], "UTC");
    assert_eq!(v["gym"]["recent_workouts"].as_array().unwrap().len(), 2);

    // device token, cookie, wrong service token → 401
    let (s, _) = h.get("/v1/insights/summary").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let sid = h.login().await;
    let (s, _) = h
        .send(
            h.http
                .get(h.url("/v1/insights/summary"))
                .header("cookie", format!("sec_session={sid}")),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let wrong = secretary_server::service::generate().unwrap();
    let (s, v) = h
        .send(
            h.http
                .get(h.url("/v1/insights/summary"))
                .bearer_auth(&wrong),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    // the service token reads nothing else
    for path in ["/v1/gym/state", "/v1/body/state", "/v1/reminders"] {
        let (s, _) = h.send(h.http.get(h.url(path)).bearer_auth(&svc)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}");
    }
    let (s, v) = h
        .send(
            h.http
                .get(h.url("/v1/insights/summary?timezone=Mars/Olympus"))
                .bearer_auth(&svc),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn insights_without_a_service_token_is_503() {
    let h = Harness::new(false).await;
    let (s, v) = h
        .send(
            h.http
                .get(h.url("/v1/insights/summary"))
                .bearer_auth(secretary_server::service::generate().unwrap()),
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"]["code"], "service_token_not_configured");
}

#[tokio::test]
async fn a_second_server_on_the_same_data_dir_refuses_to_start() {
    let h = Harness::new(false).await;
    let err = AppState::new(&h.cfg).err().expect("engine lock is held");
    assert!(format!("{err:#}").contains("already open"), "{err:#}");
}
