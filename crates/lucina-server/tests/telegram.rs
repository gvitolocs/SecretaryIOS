//! The Lucina Telegram bot against a fake Bot API (wiremock) and the fake
//! DeepSeek: allow-list and decline, commands, an owner text turn (one
//! `sendMessage`, timeline items, recent turns, chat registry), splitting,
//! the voice limit, reminders for the chat, and the poller end to end.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{harness, script, H};
use lucina_server::telegram::Bot;
use lucina_telegram::BotApi;
use serde_json::{json, Value};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BOT_TOKEN: &str = "555:TEST_BOT_SECRET";

async fn bot_api() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"/sendMessage$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "result": {"message_id": 1}})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"/(sendChatAction|getWebhookInfo|deleteMessage)$",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": {"url": ""}})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"/getMe$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "result": {"id": 555, "is_bot": true, "username": "Lucina00_bot"}}),
        ))
        .mount(&s)
        .await;
    s
}

fn telegram_config(c: &mut lucina_server::Config) {
    c.telegram.enabled = true;
    c.telegram.allowed_chat_ids = vec!["42".into()];
    c.telegram.operator_chat_id = Some("42".into());
    c.telegram.decline_unauthorized = true;
    c.telegram_token = Some(BOT_TOKEN.into());
    c.telegram.lock_dir = Some(c.data_dir.join("locks"));
}

async fn setup(
    reply: &str,
    tweak: impl FnOnce(&mut lucina_server::Config),
) -> (H, MockServer, Arc<Bot>) {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "contains": "ricordami", "content": "{\"intent\":\"schedule_reminder\",\"confidence\":0.95,\"reply_mode\":\"direct\"}"},
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "reply", "content": reply}
        ]})),
        |c| {
            telegram_config(c);
            tweak(c);
        },
    )
    .await;
    let api = bot_api().await;
    let bot = Bot::new(
        h.state.clone(),
        BotApi::with_base_url(BOT_TOKEN.into(), &api.uri()),
    );
    (h, api, bot)
}

fn text_update(id: u64, chat: i64, chat_type: &str, from: i64, text: &str) -> Value {
    json!({"update_id": id, "message": {
        "message_id": id, "date": 1_790_000_000,
        "chat": {"id": chat, "type": chat_type, "title": if chat_type == "private" { Value::Null } else { json!("Famiglia") }},
        "from": {"id": from, "is_bot": false, "first_name": "Giuseppe", "username": "giuseppe_v"},
        "text": text
    }})
}

async fn sent(api: &MockServer) -> Vec<Value> {
    api.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/sendMessage"))
        .map(|r| r.body_json::<Value>().unwrap())
        .collect()
}

#[tokio::test]
async fn unauthorized_dms_are_declined_once_a_day_and_groups_stay_silent() {
    let (h, api, bot) = setup("ciao", |_| {}).await;
    bot.clone()
        .handle(text_update(1, 77, "private", 77, "ciao"))
        .await;
    bot.clone()
        .handle(text_update(2, 77, "private", 77, "ci sei?"))
        .await;
    bot.clone()
        .handle(text_update(3, -100, "supergroup", 78, "ciao"))
        .await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0]["chat_id"], "77");
    assert_eq!(
        s[0]["text"],
        "Sorry, Lucina is a private assistant and can't chat with you."
    );
    assert!(s[0].get("parse_mode").is_none());
    assert!(h.timeline_lines().is_empty());
    assert!(
        h.fake.state.requests.lock().unwrap().is_empty(),
        "never reaches the model"
    );
    assert!(!h.dir.path().join("recent-telegram-chat.json").exists());
}

#[tokio::test]
async fn start_and_status() {
    let (h, api, bot) = setup("ciao", |_| {}).await;
    bot.clone()
        .handle(text_update(1, 42, "private", 42, "/start"))
        .await;
    bot.clone()
        .handle(text_update(2, 42, "private", 42, "/status full"))
        .await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 2);
    assert_eq!(s[0]["text"], "Lucina is online.");
    assert_eq!(s[0]["parse_mode"], "HTML");
    let status = s[1]["text"].as_str().unwrap();
    assert!(status.starts_with("Lucina status: ok\n• up "), "{status}");
    assert!(status.contains("runtime lucina-rs"));
    assert!(status.contains("• telegram lock: none held"));
    assert!(status.contains("• unauthorized DMs: decline"));
    assert!(status.contains("• next reminder: none"));
    assert_eq!(s[1]["disable_web_page_preview"], true);
    let items = h.timeline_lines();
    let texts: Vec<_> = items
        .iter()
        .map(|i| (i["role"].as_str().unwrap(), i["text"].as_str().unwrap()))
        .collect();
    assert_eq!(texts[0], ("user", "/start"));
    assert_eq!(texts[1], ("lucina", "Lucina is online."));
    assert_eq!(texts[2], ("user", "/status full"));
    assert_eq!(items[1]["replyTo"], items[0]["id"]);
    assert_eq!(items[0]["channel"], "telegram");
    assert_eq!(items[0]["meta"], json!({"isDm": true}));
    assert!(h.fake.state.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_owner_text_turn_replies_once_records_and_remembers() {
    let (h, api, bot) = setup("Ciao **Giuseppe** <3", |_| {}).await;
    bot.clone()
        .handle(text_update(10, 42, "private", 42, "ciao Lucina"))
        .await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0]["chat_id"], "42");
    assert_eq!(s[0]["text"], "Ciao <b>Giuseppe</b> &lt;3");
    assert_eq!(s[0]["parse_mode"], "HTML");
    assert_eq!(s[0]["disable_web_page_preview"], true);
    let items = h.timeline_lines();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(
        (items[0]["role"].as_str(), items[0]["text"].as_str()),
        (Some("user"), Some("ciao Lucina"))
    );
    assert_eq!(
        (items[1]["role"].as_str(), items[1]["text"].as_str()),
        (Some("lucina"), Some("Ciao **Giuseppe** <3"))
    );
    assert_eq!(items[1]["channel"], "telegram");
    assert_eq!(items[1]["replyTo"], items[0]["id"]);
    assert_eq!(items[0]["meta"]["isDm"], true);
    let recent: Value = serde_json::from_slice(
        &std::fs::read(h.dir.path().join("recent-telegram-chat.json")).unwrap(),
    )
    .unwrap();
    let turns = recent["42"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        (turns[0]["role"].as_str(), turns[0]["text"].as_str()),
        (Some("user"), Some("ciao Lucina"))
    );
    assert_eq!(turns[1]["role"], "assistant");
    let registry: Value =
        serde_json::from_slice(&std::fs::read(h.dir.path().join("telegram-chats.json")).unwrap())
            .unwrap();
    assert_eq!(registry["42"]["chatType"], "private");
    assert_eq!(registry["@giuseppe_v"]["chatId"], "42");
    // The planner and the reply saw Telegram, not the Secretary app.
    let reqs = h.fake.state.requests.lock().unwrap().clone();
    let all = serde_json::to_string(&reqs).unwrap();
    assert!(
        all.contains("Telegram") || all.contains("telegram"),
        "{all}"
    );
    assert!(!all.contains("This conversation comes from the Secretary app"));
    assert!(all.contains("Giuseppe (operator) @giuseppe_v"));
    // The secretary buffer is untouched.
    assert!(h.state.recent.turns("telegram_chat_42").is_empty());
    assert!(h.state.recent.turns("42").is_empty());
}

#[tokio::test]
async fn a_group_turn_from_the_owner_carries_the_chat_title_and_others_are_not_recorded() {
    let (h, api, bot) = setup("ok", |c| {
        c.telegram.allowed_chat_ids = vec!["42".into(), "-100".into()];
    })
    .await;
    bot.clone()
        .handle(text_update(1, -100, "supergroup", 42, "ciao a tutti"))
        .await;
    bot.clone()
        .handle(text_update(2, -100, "supergroup", 99, "ciao Lucina"))
        .await;
    assert_eq!(sent(&api).await.len(), 2);
    let items = h.timeline_lines();
    assert_eq!(items.len(), 2, "only the owner's turn: {items:?}");
    assert_eq!(
        items[0]["meta"],
        json!({"isDm": false, "chatTitle": "Famiglia"})
    );
    assert_eq!(items[0]["text"], "ciao a tutti");
}

#[tokio::test]
async fn a_long_reply_is_split() {
    let long = format!("{}\n{}", "a".repeat(4000), "b".repeat(300));
    let (_h, api, bot) = setup(&long, |_| {}).await;
    bot.clone()
        .handle(text_update(1, 42, "private", 42, "scrivi tanto"))
        .await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 2, "{}", s.len());
    assert_eq!(s[0]["text"], format!("{}\n", "a".repeat(4000)));
    assert_eq!(s[1]["text"], "b".repeat(300));
}

#[tokio::test]
async fn voice_over_the_limit_gets_nodes_refusal() {
    let (h, api, bot) = setup("ok", |_| {}).await;
    let update = json!({"update_id": 5, "message": {
        "message_id": 5, "date": 1_790_000_000,
        "chat": {"id": 42, "type": "private"},
        "from": {"id": 42, "is_bot": false, "first_name": "Giuseppe"},
        "voice": {"file_id": "F1", "file_unique_id": "U1", "duration": 1000, "mime_type": "audio/ogg"}
    }});
    bot.clone().handle(update).await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 1);
    assert_eq!(
        s[0]["text"],
        "Sono Lucina: questo audio è troppo lungo (1000s). Limite attuale: 900s."
    );
    let reqs = api.received_requests().await.unwrap();
    assert!(!reqs.iter().any(|r| r.url.path().ends_with("/getFile")));
    let items = h.timeline_lines();
    assert_eq!(items[0]["attachments"], json!([{"kind": "voice"}]));
    assert_eq!(items[1]["role"], "lucina");
}

#[tokio::test]
async fn a_reminder_from_telegram_is_delivered_to_that_chat() {
    let (h, api, bot) = setup("ok", |_| {}).await;
    bot.clone()
        .handle(text_update(
            1,
            42,
            "private",
            42,
            "ricordami tra 2 ore di chiamare la mamma",
        ))
        .await;
    let s = sent(&api).await;
    assert!(
        s[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Reminder set for "),
        "{s:?}"
    );
    let all = h.state.reminders.list(true).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].chat_id, "42");
    assert_eq!(all[0].source, "telegram");
    assert_eq!(all[0].user_peer_id, "giuseppe");
}

#[tokio::test]
async fn the_poller_hands_owner_updates_to_the_bot_and_reports_health() {
    let (h, api, _bot) = setup("Eccomi", |_| {}).await;
    Mock::given(method("POST"))
        .and(path_regex(r"/getUpdates$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "result": [text_update(77, 42, "private", 42, "ci sei?")]}),
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"/getUpdates$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"ok": true, "result": []})),
        )
        .with_priority(10)
        .mount(&api)
        .await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = lucina_server::telegram::spawn(
        &h.state,
        BotApi::with_base_url(BOT_TOKEN.into(), &api.uri()),
        rx,
    )
    .unwrap();
    for _ in 0..200 {
        if !sent(&api).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let s = sent(&api).await;
    assert_eq!(s.len(), 1);
    assert_eq!(s[0]["text"], "Eccomi");
    let (status, health) = {
        let r = reqwest::get(format!("{}/health", h.base)).await.unwrap();
        (r.status().as_u16(), r.json::<Value>().await.unwrap())
    };
    assert_eq!(status, 200);
    assert_eq!(health["channels"]["telegram"], true);
    assert_eq!(health["telegram"]["leaseHeld"], true);
    assert_eq!(health["telegram"]["claimedUpdateId"], 77);
    assert_eq!(health["telegram"]["botUsername"], "Lucina00_bot");
    assert!(!health.to_string().contains("TEST_BOT_SECRET"));
    let _ = stop.send(true);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    let r: Value = reqwest::get(format!("{}/health", h.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["telegram"]["leaseHeld"], false);
    assert_eq!(r["telegram"]["state"], "stopped");
    let claim: Value = serde_json::from_slice(
        &std::fs::read(h.dir.path().join("lucina-rs/telegram-555-claim.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(claim["claimedUpdateId"], 77);
}

#[tokio::test]
async fn a_job_started_on_telegram_reports_back_to_the_chat() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\"}"},
            {"kind": "plan", "content": "{\"action\":\"generate_image\",\"prompt\":\"a cat\"}"}
        ]})),
        |c| {
            telegram_config(c);
            c.image_jobs = true;
        },
    )
    .await;
    let api = bot_api().await;
    let bot = Bot::new(
        h.state.clone(),
        BotApi::with_base_url(BOT_TOKEN.into(), &api.uri()),
    );
    bot.clone()
        .handle(text_update(1, 42, "private", 42, "disegnami un gatto"))
        .await;
    for _ in 0..200 {
        if sent(&api).await.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let s = sent(&api).await;
    assert_eq!(s.len(), 2, "{s:?}");
    assert!(
        s[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("On it: I started the"),
        "{s:?}"
    );
    assert!(
        s[1]["text"]
            .as_str()
            .unwrap()
            .contains("failed: image generation (ComfyUI) is not available"),
        "{s:?}"
    );
    assert_eq!(s[1]["chat_id"], "42");
    let items = h.timeline_lines();
    assert_eq!(items.len(), 3, "{items:?}");
    assert_eq!(items[2]["replyTo"], items[0]["id"]);
    assert_eq!(items[2]["meta"]["isDm"], true);
    assert!(items[2]["meta"]["jobId"].is_string());
}

#[tokio::test]
async fn a_document_goes_through_the_share_pipeline() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "content": "È la bolletta: totale 84,30 euro."},
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\"}"}
        ]})),
        telegram_config,
    )
    .await;
    let api = bot_api().await;
    Mock::given(method("POST"))
        .and(path_regex(r"/getFile$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "result": {"file_id": "D1", "file_size": 40, "file_path": "documents/file_1.txt"}}),
        ))
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/file/bot.*/documents/file_1\.txt$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("Bolletta luce\nTotale da pagare 84,30 EUR\n"),
        )
        .mount(&api)
        .await;
    let bot = Bot::new(
        h.state.clone(),
        BotApi::with_base_url(BOT_TOKEN.into(), &api.uri()),
    );
    bot.clone()
        .handle(json!({"update_id": 9, "message": {
            "message_id": 9, "date": 1_790_000_000,
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 42, "is_bot": false, "first_name": "Giuseppe", "language_code": "it"},
            "caption": "quanto devo pagare?",
            "document": {"file_id": "D1", "file_unique_id": "DU1", "file_name": "bolletta.txt", "mime_type": "text/plain", "file_size": 40}
        }}))
        .await;
    let s = sent(&api).await;
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0]["text"], "È la bolletta: totale 84,30 euro.");
    let reqs = serde_json::to_string(&*h.fake.state.requests.lock().unwrap()).unwrap();
    assert!(reqs.contains("Totale da pagare 84,30 EUR"), "{reqs}");
    assert!(reqs.contains("owner share on Telegram"));
    assert!(reqs.contains("quanto devo pagare?"));
    let items = h.timeline_lines();
    assert_eq!(items[0]["text"], "quanto devo pagare?");
    assert_eq!(
        items[0]["attachments"],
        json!([{"kind": "file", "name": "bolletta.txt"}])
    );
    assert_eq!(items[1]["text"], "È la bolletta: totale 84,30 euro.");
}
