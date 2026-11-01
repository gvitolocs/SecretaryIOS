//! Contract tests: the clients that talk to Lucina, against the Rust Lucina.
//!
//! 1. `secretary-core`'s typed `LucinaClient` (what secretary-server uses)
//!    decodes every response into its contract models.
//! 2. The real `secretary-server` app in front of the Rust Lucina, driven
//!    through its own `/v1/*` routes with a device token, exactly as the app.

mod common;

use common::{harness, script, TOKEN};
use lucina_server::PlannerMode;
use secretary_core::client::{Call, LucinaClient, Timeouts};
use secretary_core::models::{
    Agenda, AgendaQuery, ChatReply, ChatRequest, CreatedReminder, Health, ReminderList,
    ReminderQuery, TimelinePage, TimelineQuery,
};
use secretary_core::Error;
use serde_json::{json, Value};

fn s() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.9,\"reply_mode\":\"chat\"}"},
        {"kind": "reply", "content": "Ciao!"}
    ]}))
}

#[tokio::test]
async fn secretary_core_client_decodes_every_route() {
    let h = harness(s(), |_| {}).await;
    let client = LucinaClient::new(&h.base, TOKEN, Timeouts::default()).unwrap();
    let health: Health = client.call(&Call::health()).await.unwrap();
    assert_eq!(health.api_version, Some(1));
    let chat: ChatReply = client
        .call(
            &Call::chat(&ChatRequest {
                message: "ciao".into(),
                session_id: Some("iphone".into()),
                client_message_id: Some("m-1".into()),
                channel: Some("ios".into()),
                reply_to: None,
                device: None,
                device_name: None,
                voice_meta: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chat.reply, "Ciao!");
    assert_eq!(chat.session_id.as_deref(), Some("secretary_iphone"));
    let create =
        serde_json::from_value(json!({"text": "dentista", "dueAt": "2099-10-22T09:30:00+02:00"}))
            .unwrap();
    let (status, created): (u16, CreatedReminder) = client
        .call_with_status(&Call::create_reminder(&create).unwrap())
        .await
        .unwrap();
    assert_eq!((status, created.created), (201, true));
    let list: ReminderList = client
        .call(
            &Call::reminders(&ReminderQuery {
                scope: Some("upcoming".into()),
                include_sent: Some("1".into()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.reminders.len(), 1);
    let cancelled: Value = client
        .call(&Call::cancel_reminder(&created.reminder.id).unwrap())
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let agenda: Agenda = client
        .call(
            &Call::agenda(&AgendaQuery {
                date: Some("2099-10-22".into()),
                refresh: Some("1".into()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(agenda.date, "2099-10-22");
    let page: TimelinePage = client
        .call(
            &Call::timeline(&TimelineQuery {
                before: None,
                after: None,
                scope: None,
                conversation: None,
                limit: Some("50".into()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    match client.call::<Value>(&Call::job("missing").unwrap()).await {
        Err(Error::Api {
            status: 404, code, ..
        }) => assert_eq!(code, "job_not_found"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn secretary_server_in_front_of_rust_lucina() {
    let h = harness(s(), |c| c.planner = PlannerMode::Json).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = secretary_server::config::Config::new(h.base.clone(), Some(TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    let store = secretary_server::devices::DeviceStore::new(&cfg.data_dir);
    let (_dev, device_token) = store.issue("contract iPhone").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = secretary_server::AppState::new(&cfg).unwrap();
    tokio::spawn(async move {
        secretary_server::serve_on(
            listener,
            secretary_server::app(state),
            std::future::pending(),
        )
        .await
        .unwrap()
    });
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();
    let call = |m: reqwest::Method, p: &str, body: Option<Value>| {
        let mut rb = http
            .request(m, format!("{base}{p}"))
            .bearer_auth(&device_token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        async move {
            let r = rb.send().await.unwrap();
            let st = r.status().as_u16();
            (st, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (st, b) = call(
        reqwest::Method::POST,
        "/v1/chat",
        Some(json!({"message": "ciao", "sessionId": "iphone", "clientMessageId": "c1"})),
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["reply"], "Ciao!");
    assert_eq!(b["sessionId"], "secretary_iphone");
    let (st, b) = call(
        reqwest::Method::POST,
        "/v1/reminders",
        Some(json!({"text": "call mum", "dueAt": "2099-10-22T09:00:00+02:00"})),
    )
    .await;
    assert_eq!(st, 201, "{b}");
    let id = b["reminder"]["id"].as_str().unwrap().to_owned();
    let (st, b) = call(
        reqwest::Method::GET,
        "/v1/reminders?scope=upcoming&includeSent=1",
        None,
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["reminders"][0]["id"], json!(id));
    let (st, b) = call(
        reqwest::Method::DELETE,
        &format!("/v1/reminders/{id}"),
        None,
    )
    .await;
    assert_eq!((st, b["cancelled"].as_bool()), (200, Some(true)), "{b}");
    let (st, b) = call(reqwest::Method::GET, "/v1/agenda?date=2099-10-22", None).await;
    assert_eq!(st, 200, "{b}");
    let (st, b) = call(reqwest::Method::GET, "/v1/timeline?limit=10", None).await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["items"].as_array().map(Vec::len), Some(2), "{b}");
    let (st, b) = call(reqwest::Method::GET, "/v1/jobs/nope", None).await;
    assert_eq!(st, 404, "{b}");
    // shopping without a grocery DB: Lucina's 503 passes through as a feature 503
    if h.state.grocery.is_none() {
        let (st, b) = call(reqwest::Method::GET, "/v1/shopping", None).await;
        assert_eq!(st, 503, "{b}");
    }
    let ts = lucina_store::timeutil::now_iso();
    let (st, b) = call(reqwest::Method::POST, "/v1/device/events", Some(json!({"events": [{"id": "ev-1", "ts": ts, "type": "focus_change", "source": "ios", "data": {"mode": "Work"}}]}))).await;
    assert!(st == 200 || st == 202, "{st} {b}");
    let lines = h.timeline_lines();
    assert_eq!(lines[0]["clientMessageId"], "c1");
    assert!(
        lines[0]["channel"] == "web" || lines[0]["channel"] == "ios",
        "secretary-server sets the channel"
    );
}
