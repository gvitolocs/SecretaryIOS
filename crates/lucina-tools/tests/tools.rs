//! Tools and the agent loop with mocked networks (wiremock) and a scripted model.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_llm::{Role, ScriptedModel};
use lucina_store::reminders::ReminderStore;
use lucina_tools::agent::{self, AgentConfig, Stop};
use lucina_tools::pending::{PendingStore, DEFAULT_TTL};
use lucina_tools::reminder_writer::{LocalWriter, NodeForwarder};
use lucina_tools::{Ctx, Effect, Endpoints, Registry, ToolEnv, ToolSelection};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env(dir: &std::path::Path, server: Option<&MockServer>) -> ToolEnv {
    let mut e = ToolEnv::new(dir, "Europe/Rome".parse().unwrap());
    if let Some(s) = server {
        e.endpoints = Endpoints {
            open_meteo_forecast: s.uri(),
            open_meteo_geocoding: s.uri(),
            nominatim: s.uri(),
        };
    }
    e.nominatim_gate = lucina_tools::RateGate::new(Duration::from_millis(300));
    e
}

async fn call(registry: &Registry, ctx: &Ctx, name: &str, args: Value) -> (bool, Value) {
    let (ok, out, _) = agent::execute(registry, name, args, ctx, Duration::from_secs(10)).await;
    (ok, out)
}

#[tokio::test]
async fn weather_geocodes_and_forecasts() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/search")).and(query_param("name", "Copenhagen"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"name": "Copenhagen", "latitude": 55.67594, "longitude": 12.56553, "country": "Denmark", "country_code": "DK", "admin1": "Capital Region"}
        ]})))
        .expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/v1/forecast")).and(query_param("latitude", "55.67594")).and(query_param("forecast_days", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timezone": "Europe/Copenhagen",
            "current": {"time": "2026-10-21T17:00", "temperature_2m": 9.1, "apparent_temperature": 6.0, "weather_code": 3, "wind_speed_10m": 20.5, "relative_humidity_2m": 80, "precipitation": 0.0},
            "daily": {"time": ["2026-10-21", "2026-10-22"], "weather_code": [3, 61], "temperature_2m_max": [11.0, 10.2], "temperature_2m_min": [6.0, 5.5],
                      "precipitation_sum": [0.0, 4.2], "precipitation_probability_max": [10, 85], "wind_speed_10m_max": [25.0, 30.1],
                      "sunrise": ["2026-10-21T07:52", "2026-10-22T07:54"], "sunset": ["2026-10-21T17:41", "2026-10-22T17:39"]}
        })))
        .expect(1).mount(&server).await;
    // default location: no geocoding call
    Mock::given(method("GET"))
        .and(path("/v1/forecast"))
        .and(query_param("latitude", "55.6761"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"current": {}, "daily": {"time": []}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), Some(&server))), "web", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let (ok, out) = call(
        &r,
        &ctx,
        "weather",
        json!({"location": "Copenhagen, Denmark", "days": 2}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["location"]["country"], "Denmark");
    assert_eq!(out["daily"][1]["conditions"], "light rain");
    assert_eq!(out["daily"][1]["precipitationProbability"], 85);
    assert_eq!(out["current"]["conditions"], "overcast");
    let (ok, out) = call(&r, &ctx, "weather", json!({})).await;
    assert!(ok);
    assert_eq!(out["location"]["name"], "Copenhagen, Denmark");
    // schema validation: days out of range never reaches the network
    let (ok, out) = call(&r, &ctx, "weather", json!({"days": 30})).await;
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("invalid arguments"));
}

#[tokio::test]
async fn places_sends_user_agent_and_waits_between_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/search")).and(query_param("q", "pharmacy, Nørrebro"))
        .and(header_regex("user-agent", "^lucina-rs/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"name": "Apotek", "display_name": "Apotek, Nørrebrogade, København", "lat": "55.69", "lon": "12.55", "category": "amenity", "type": "pharmacy"}
        ])))
        .expect(2).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), Some(&server))), "web", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let started = std::time::Instant::now();
    for _ in 0..2 {
        let (ok, out) = call(
            &r,
            &ctx,
            "places",
            json!({"query": "pharmacy", "near": "Nørrebro"}),
        )
        .await;
        assert!(ok, "{out}");
        assert_eq!(out["results"][0]["latitude"], 55.69);
    }
    assert!(started.elapsed() >= Duration::from_millis(300), "rate gate");
}

#[tokio::test]
async fn time_calculator_units_and_notes() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), None)), "ios", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let (ok, out) = call(&r, &ctx, "time", json!({"timezone": "Europe/Copenhagen"})).await;
    assert!(ok);
    assert_eq!(out["timezone"], "Europe/Copenhagen");
    assert!(
        !call(&r, &ctx, "time", json!({"timezone": "Mars/Base"}))
            .await
            .0
    );
    assert_eq!(
        call(&r, &ctx, "calculator", json!({"expression": "17% * 230"}))
            .await
            .1["formatted"],
        "39.1"
    );
    let (ok, out) = call(&r, &ctx, "calculator", json!({"expression": "1/0"})).await;
    assert!(!ok);
    assert_eq!(out["error"], "division by zero");
    assert_eq!(
        call(
            &r,
            &ctx,
            "unit_convert",
            json!({"value": 5, "from": "km", "to": "m"})
        )
        .await
        .1["formatted"],
        "5000 m"
    );
    assert!(
        call(
            &r,
            &ctx,
            "notes",
            json!({"action": "create", "title": "Idee regalo", "content": "- libro"})
        )
        .await
        .0
    );
    assert!(
        !call(
            &r,
            &ctx,
            "notes",
            json!({"action": "create", "title": "Idee regalo", "content": "x"})
        )
        .await
        .0
    );
    assert!(
        call(
            &r,
            &ctx,
            "notes",
            json!({"action": "append", "title": "idee REGALO", "content": "- sciarpa"})
        )
        .await
        .0
    );
    let (_, out) = call(
        &r,
        &ctx,
        "notes",
        json!({"action": "read", "title": "Idee regalo"}),
    )
    .await;
    assert_eq!(out["content"], "# Idee regalo\n\n- libro\n- sciarpa\n");
    let (_, out) = call(
        &r,
        &ctx,
        "notes",
        json!({"action": "search", "query": "sciarpa"}),
    )
    .await;
    assert_eq!(out["results"][0]["title"], "Idee regalo");
    assert_eq!(
        call(&r, &ctx, "notes", json!({"action": "list"})).await.1["notes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !call(&r, &ctx, "notes", json!({"action": "explode"}))
            .await
            .0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.path().join("notes/idee-regalo.md")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn reminders_local_and_forwarded_to_node() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ReminderStore::new(dir.path().join("reminders.json"), 500));
    let mut e = env(dir.path(), None);
    e.reminders = Some(store.clone());
    e.reminder_writer = Some(Arc::new(LocalWriter(store.clone())));
    e.owner_chat_id = Some("42".into());
    let ctx = Ctx::new(Arc::new(e), "web", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let (ok, out) = call(
        &r,
        &ctx,
        "reminders",
        json!({"action": "create", "text": "dentista", "dueAt": "2099-10-22T09:30:00+02:00"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["reminder"]["dueAt"], "2099-10-22T07:30:00.000Z");
    assert_eq!(out["reminder"]["source"], "secretary");
    assert!(matches!(
        ctx.effects()[0],
        Effect::ReminderCreated { created: true, .. }
    ));
    let (ok, out) = call(
        &r,
        &ctx,
        "reminders",
        json!({"action": "create", "naturalText": "remind me in 2 hours to stretch"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["reminder"]["text"], "stretch");
    assert!(
        !call(
            &r,
            &ctx,
            "reminders",
            json!({"action": "create", "text": "x", "dueAt": "2099-10-22T09:30:00"})
        )
        .await
        .0,
        "offset required"
    );
    assert!(
        !call(
            &r,
            &ctx,
            "reminders",
            json!({"action": "create", "text": "x", "dueAt": "2001-10-22T09:30:00Z"})
        )
        .await
        .0,
        "past"
    );
    let (_, out) = call(&r, &ctx, "reminders", json!({"action": "list"})).await;
    assert_eq!(out["reminders"].as_array().unwrap().len(), 2);

    // forwarded: the Node Lucina gets the write, the file is untouched
    let node = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/secretary/reminders")).and(header("authorization", "Bearer node-token"))
        .and(body_json(json!({"text": "call mum", "dueAt": "2099-01-01T08:00:00.000Z"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ok": true, "data": {"created": true, "reminder": {
            "id": "r-node", "text": "call mum", "dueAt": "2099-01-01T08:00:00.000Z", "timeZone": "Europe/Rome", "status": "pending",
            "source": "secretary", "createdAt": "2026-10-21T10:00:00.000Z", "sentAt": null, "cancelledAt": null}}})))
        .expect(1).mount(&node).await;
    Mock::given(method("DELETE"))
        .and(path("/api/secretary/reminders/r-sent"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"ok": false, "error": "already_sent"})),
        )
        .mount(&node)
        .await;
    let before = std::fs::read(store.path()).unwrap();
    let mut e = env(dir.path(), None);
    e.reminders = Some(store.clone());
    e.reminder_writer = Some(Arc::new(NodeForwarder::new(
        &node.uri(),
        Some("node-token".into()),
    )));
    e.owner_chat_id = Some("42".into());
    let ctx = Ctx::new(Arc::new(e), "web", "s");
    let (ok, out) = call(
        &r,
        &ctx,
        "reminders",
        json!({"action": "create", "text": "call mum", "dueAt": "2099-01-01T09:00:00+01:00"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["reminder"]["id"], "r-node");
    let (ok, out) = call(
        &r,
        &ctx,
        "reminders",
        json!({"action": "cancel", "id": "r-sent"}),
    )
    .await;
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().starts_with("already_sent"));
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
    // no token: a clear error, nothing sent
    let mut e = env(dir.path(), None);
    e.reminders = Some(store.clone());
    e.reminder_writer = Some(Arc::new(NodeForwarder::new(&node.uri(), None)));
    e.owner_chat_id = Some("42".into());
    let ctx = Ctx::new(Arc::new(e), "web", "s");
    let (ok, out) = call(
        &r,
        &ctx,
        "reminders",
        json!({"action": "create", "text": "x", "dueAt": "2099-01-01T09:00:00+01:00"}),
    )
    .await;
    assert!(!ok);
    assert!(out["error"]
        .as_str()
        .unwrap()
        .contains("reminders_disabled"));
}

#[tokio::test]
async fn agent_loop_runs_tools_and_returns_errors_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), None)), "web", "secretary_app");
    let r = Registry::builtin(&ToolSelection::All);
    let model = ScriptedModel::new();
    model.push_calls(&[
        ("calculator", json!({"expression": "17% * 230"})),
        ("calculator", json!({"expression": "2 +"})),
    ]);
    model.push_calls(&[("nonexistent", json!({}))]);
    model.push_text("Il 17% di 230 è 39,1.");
    let pending = PendingStore::memory(DEFAULT_TTL);
    let seen = Mutex::new(Vec::new());
    let out = agent::run(
        &model,
        &r,
        vec![lucina_llm::Message::user("quanto fa 17% di 230?")],
        &ctx,
        &pending,
        &AgentConfig::default(),
        &|rec| seen.lock().unwrap().push(rec.name.clone()),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "Il 17% di 230 è 39,1.");
    assert_eq!(out.stop, Stop::Answered);
    assert_eq!(out.rounds, 3);
    assert_eq!(out.records.len(), 3);
    assert!(out.records[0].ok && !out.records[1].ok && !out.records[2].ok);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["calculator", "calculator", "nonexistent"]
    );
    // the model saw the tool outputs, errors included
    let reqs = model.requests.lock().unwrap();
    let second = &reqs[1].messages;
    assert_eq!(second.iter().filter(|m| m.role == Role::Tool).count(), 2);
    assert!(second.iter().any(|m| m.text().contains("39.1")));
    assert!(second.iter().any(|m| m.text().contains("\"error\"")));
    assert!(!reqs[0].tools.is_empty());
    assert_eq!(reqs[0].tool_choice.as_deref(), Some("auto"));
}

#[tokio::test]
async fn agent_stops_after_six_rounds_and_on_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), None)), "web", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let model = ScriptedModel::new();
    for _ in 0..6 {
        model.push_calls(&[("time", json!({}))]);
    }
    let pending = PendingStore::memory(DEFAULT_TTL);
    let out = agent::run(
        &model,
        &r,
        vec![lucina_llm::Message::user("loop")],
        &ctx,
        &pending,
        &AgentConfig::default(),
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(out.rounds, 6);
    assert_eq!(out.stop, Stop::MaxRounds);
    assert_eq!(model.request_count(), 6);
    assert_eq!(
        model.requests.lock().unwrap()[5].tool_choice.as_deref(),
        Some("none"),
        "last round forces text"
    );

    struct Slow;
    #[async_trait::async_trait]
    impl lucina_llm::ChatModel for Slow {
        async fn complete(
            &self,
            _: lucina_llm::ChatRequest,
        ) -> Result<lucina_llm::ChatResponse, lucina_llm::LlmError> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            unreachable!()
        }
    }
    let cfg = AgentConfig {
        deadline: Duration::from_millis(200),
        ..AgentConfig::default()
    };
    let out = agent::run(
        &Slow,
        &r,
        vec![lucina_llm::Message::user("x")],
        &ctx,
        &pending,
        &cfg,
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(out.stop, Stop::Deadline);
}

#[tokio::test]
async fn confirmation_gated_calls_become_pending_actions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ReminderStore::new(dir.path().join("reminders.json"), 500));
    let (_, rem) = store
        .create(lucina_store::CreateReminder {
            text: "x".into(),
            due_at: "2099-01-01T08:00:00.000Z".into(),
            chat_id: "42".into(),
            user_peer_id: "giuseppe".into(),
            user_label: String::new(),
            persona: "Lucina".into(),
            time_zone: "Europe/Rome".into(),
            dedupe_key: None,
            source: "secretary".into(),
        })
        .unwrap();
    let mut e = env(dir.path(), None);
    e.reminders = Some(store.clone());
    e.reminder_writer = Some(Arc::new(LocalWriter(store.clone())));
    e.owner_chat_id = Some("42".into());
    let ctx = Ctx::new(Arc::new(e), "web", "secretary_app");
    let r = Registry::builtin(&ToolSelection::All);
    let model = ScriptedModel::new();
    model.push_calls(&[("reminders", json!({"action": "cancel", "id": rem.id}))]);
    model.push_text("Vuoi che lo cancelli? Rispondi sì.");
    let pending = PendingStore::memory(DEFAULT_TTL);
    let executed = Mutex::new(0);
    let out = agent::run(
        &model,
        &r,
        vec![lucina_llm::Message::user("cancella il promemoria")],
        &ctx,
        &pending,
        &AgentConfig::default(),
        &|_| *executed.lock().unwrap() += 1,
    )
    .await
    .unwrap();
    assert_eq!(
        *executed.lock().unwrap(),
        0,
        "not executed in the same turn"
    );
    let p = out.records[0].pending.clone().unwrap();
    assert_eq!(out.records[0].output["status"], "awaiting_confirmation");
    assert_eq!(store.list(false).unwrap().len(), 1, "still pending");
    // a later turn's explicit yes runs it, once
    let action = pending
        .current("secretary_app", chrono::Utc::now())
        .unwrap();
    assert_eq!(action.id, p.id);
    let action = pending.take(&action.id).unwrap();
    let (ok, out, _) =
        agent::execute(&r, &action.tool, action.args, &ctx, Duration::from_secs(5)).await;
    assert!(ok, "{out}");
    assert_eq!(store.list(false).unwrap().len(), 0);
    assert!(pending.take(&p.id).is_none());
}

#[tokio::test]
async fn tool_call_markup_is_never_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(Arc::new(env(dir.path(), None)), "web", "s");
    let r = Registry::builtin(&ToolSelection::All);
    let dsml = "<\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c} calls> <\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c} invoke name=\"drive\"> </\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c} calls>";
    let pending = PendingStore::memory(DEFAULT_TTL);
    // Markup, then a clean answer on the re-ask.
    let model = ScriptedModel::new();
    model.push_text(dsml);
    model.push_text("Non ho trovato nulla su Drive.");
    let out = agent::run(&model, &r, vec![lucina_llm::Message::user("cerca su drive")], &ctx, &pending, &AgentConfig::default(), &|_| {}).await.unwrap();
    assert_eq!(out.text, "Non ho trovato nulla su Drive.");
    assert!(model.requests.lock().unwrap()[1].tools.is_empty(), "the re-ask has no tools");
    // Markup twice: no text at all, so the caller falls back.
    let model = ScriptedModel::new();
    model.push_text(dsml);
    model.push_text(dsml);
    let out = agent::run(&model, &r, vec![lucina_llm::Message::user("cerca su drive")], &ctx, &pending, &AgentConfig::default(), &|_| {}).await.unwrap();
    assert_eq!(out.text, "");
}
