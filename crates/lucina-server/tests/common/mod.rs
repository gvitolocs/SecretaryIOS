//! Test harness: the real server on an ephemeral port, a temp data dir, the
//! real DeepSeek client pointed at the fake DeepSeek (also on an ephemeral
//! port), memory off, local reminder writer.

#![allow(dead_code)]

use std::time::Duration;

use lucina_llm::{DeepSeekConfig, KeySource};
use lucina_memory::MemoryMode;
use lucina_server::fake::{self, Fake, Script};
use lucina_server::{app, AppState, Config, PlannerMode, WriterKind};
use serde_json::{json, Value};

pub const TOKEN: &str = "test-secretary-token";

pub struct H {
    pub base: String,
    pub http: reqwest::Client,
    pub fake: Fake,
    pub state: AppState,
    pub dir: tempfile::TempDir,
}

pub fn script(v: Value) -> Script {
    serde_json::from_value(v).unwrap()
}

pub async fn harness(s: Script, tweak: impl FnOnce(&mut Config)) -> H {
    harness_with(s, tweak, |st| st).await
}

/// [`harness`] with a last change to the built state (e.g. a fixture deals engine).
pub async fn harness_with(
    s: Script,
    tweak: impl FnOnce(&mut Config),
    finish: impl FnOnce(AppState) -> AppState,
) -> H {
    // Hermetic: never read the live Pokémon game server on this machine.
    std::env::set_var("LUCINA_GAME", "false");
    let dir = tempfile::tempdir().unwrap();
    let fake = fake::spawn("127.0.0.1:0".parse().unwrap(), s, None)
        .await
        .unwrap();
    let mut cfg = Config::for_data_dir(dir.path());
    cfg.secretary_enabled = true;
    cfg.token = Some(TOKEN.into());
    cfg.owner_chat_id = Some("42".into());
    cfg.reminders_writer = WriterKind::Local;
    cfg.memory_mode = MemoryMode::Off;
    cfg.planner = PlannerMode::Json;
    // Hermetic: never read the live AU Find (af-server) on this machine; tests/au.rs turns it on.
    cfg.au_enabled = false;
    // Hermetic: no caller turn for test messages that hold numbers; tests/callers.rs turns it on.
    cfg.callers_enabled = false;
    // Hermetic: never read 180.dk from tests (tests/callers.rs points it at a fake site).
    cfg.callers_public_url = None;
    // The tests' dates and reminders are written for Rome; the production default is Europe/Copenhagen.
    cfg.time_zone = chrono_tz::Europe::Rome;
    cfg.recovery_companion_path = Some(dir.path().join("none.md"));
    cfg.operator_brief_path = Some(dir.path().join("none.md"));
    cfg.timeline_ping = Duration::from_millis(300);
    cfg.deepseek = DeepSeekConfig {
        base_url: fake.base_url.clone(),
        key: KeySource::Inline("fake-key".into()),
        timeout: Duration::from_secs(10),
        max_retries: 0,
        ..DeepSeekConfig::default()
    };
    cfg.grocery_database_url = std::env::var("LUCINA_TEST_DATABASE_URL")
        .ok()
        .filter(|u| u.contains("lucina_test"));
    tweak(&mut cfg);
    let state = finish(AppState::build(cfg, None).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    H {
        base: format!("http://{addr}"),
        http: reqwest::Client::new(),
        fake,
        state,
        dir,
    }
}

impl H {
    pub fn url(&self, p: &str) -> String {
        format!("{}/api/secretary{p}", self.base)
    }

    pub async fn call(
        &self,
        method: reqwest::Method,
        p: &str,
        body: Option<Value>,
    ) -> (u16, Value, reqwest::header::HeaderMap) {
        let mut rb = self.http.request(method, self.url(p)).bearer_auth(TOKEN);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        (status, resp.json().await.unwrap_or(Value::Null), headers)
    }

    pub async fn get(&self, p: &str) -> (u16, Value) {
        let (s, b, _) = self.call(reqwest::Method::GET, p, None).await;
        (s, b)
    }

    pub async fn post(&self, p: &str, body: Value) -> (u16, Value) {
        let (s, b, _) = self.call(reqwest::Method::POST, p, Some(body)).await;
        (s, b)
    }

    pub async fn chat(&self, message: &str) -> Value {
        let (s, b) = self
            .post(
                "/chat",
                json!({"message": message, "sessionId": "test", "channel": "ios"}),
            )
            .await;
        assert_eq!(s, 200, "{b}");
        b["data"].clone()
    }

    pub fn timeline_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("timeline.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
