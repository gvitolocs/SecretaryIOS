//! The Lucina Discord handler against a recording REST stub and the fake
//! DeepSeek: the disabled answer outside the admin channels, owner and
//! stranger turns (replies, timeline, splitting), reminders for the owner
//! only (on his Telegram chat), and `discord_admin_task` owner-only.

mod common;

use std::sync::Arc;

use common::{harness, script, H};
use lucina_discord::cache::SharedCache;
use lucina_discord::rest::{perm, ChannelKind, DiscordRest};
use lucina_discord::routing::Inbound;
use lucina_server::discord::testing::RecordingRest;
use lucina_server::discord::Bot;
use serde_json::{json, Value};

const BOT: u64 = 900;
const OWNER: u64 = 4242;
const GUILD: u64 = 5;

async fn setup(rules: Vec<Value>) -> (H, Arc<RecordingRest>, Arc<Bot>) {
    let mut all = rules;
    all.push(json!({"kind": "intent", "contains": "ricordami", "content": "{\"intent\":\"schedule_reminder\",\"confidence\":0.95,\"reply_mode\":\"direct\"}"}));
    all.push(json!({"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"}));
    all.push(json!({"kind": "reply", "content": "Eccomi"}));
    let h = harness(script(json!({ "rules": all })), |c| {
        c.discord.enabled = true;
        c.owner_discord_user_id = Some(OWNER.to_string());
        c.telegram.operator_chat_id = Some("42".into());
    })
    .await;
    let rest = Arc::new(RecordingRest::new(GUILD, "Casa", 1));
    rest.add_role(GUILD, "@everyone", perm::VIEW_CHANNEL);
    rest.add_role(500, "Bots", perm::MANAGE_CHANNELS);
    rest.add_member(BOT, "Lucina", vec![500]);
    rest.add_member(OWNER, "Giuseppe", vec![]);
    rest.add_channel(20, "Ops", ChannelKind::Category, None);
    rest.add_channel(10, "admin", ChannelKind::Text, Some(20));
    rest.add_channel(11, "general", ChannelKind::Text, None);
    let bot = Bot::new(
        h.state.clone(),
        rest.clone() as Arc<dyn DiscordRest>,
        SharedCache::default(),
    );
    (h, rest, bot)
}

fn dm(id: u64, author: u64, text: &str) -> Inbound {
    Inbound {
        message_id: id,
        channel_id: 70_000 + author,
        guild_id: None,
        author_id: author,
        author_username: "pino".into(),
        content: text.into(),
        bot_id: BOT,
        channel_kind: ChannelKind::Dm,
        ..Inbound::default()
    }
}

fn in_guild(id: u64, channel: u64, author: u64, text: &str) -> Inbound {
    Inbound {
        message_id: id,
        channel_id: channel,
        guild_id: Some(GUILD),
        author_id: author,
        author_username: "pino".into(),
        content: text.into(),
        mentions: if text.contains(&format!("<@{BOT}>")) {
            vec![BOT]
        } else {
            vec![]
        },
        bot_id: BOT,
        ..Inbound::default()
    }
}

#[tokio::test]
async fn outside_admin_channels_a_mention_gets_the_disabled_text_and_the_rest_silence() {
    let (h, rest, bot) = setup(vec![]).await;
    bot.clone()
        .handle(in_guild(1, 11, 77, &format!("<@{BOT}> ciao")))
        .await;
    bot.clone()
        .handle(in_guild(2, 11, 77, "ciao a tutti"))
        .await;
    // A bot (Poko) mentioning Lucina: nothing.
    let mut poko = in_guild(3, 11, 800, &format!("<@{BOT}> ehi"));
    poko.author_bot = true;
    bot.clone().handle(poko).await;
    // In the admin channel without a mention: nothing.
    bot.clone().handle(in_guild(4, 10, 77, "ciao")).await;
    let sent = rest.sent();
    assert_eq!(
        sent,
        [(
            11,
            "I'm currently disabled, ask Poko instead ✨".to_owned(),
            Some(1)
        )]
    );
    assert!(h.fake.requests().is_empty(), "no model call");
    assert!(h.timeline_lines().is_empty());
}

#[tokio::test]
async fn the_owner_dm_turn_replies_and_records_the_timeline() {
    let (h, rest, bot) = setup(vec![]).await;
    bot.clone().handle(dm(9, OWNER, "ci sei?")).await;
    let sent = rest.sent();
    assert_eq!(sent, [(70_000 + OWNER, "Eccomi".to_owned(), Some(9))]);
    assert!(rest.calls().iter().any(|c| c.starts_with("typing ")));
    let items = h.timeline_lines();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["channel"], "discord");
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[0]["text"], "ci sei?");
    assert_eq!(items[0]["meta"]["isDm"], true);
    assert_eq!(items[1]["role"], "lucina");
    assert_eq!(items[1]["text"], "Eccomi");
    assert_eq!(items[1]["replyTo"], items[0]["id"]);
}

#[tokio::test]
async fn the_owner_in_an_admin_channel_gets_guild_and_channel_on_the_timeline() {
    let (h, rest, bot) = setup(vec![]).await;
    bot.clone()
        .handle(in_guild(5, 10, OWNER, &format!("<@{BOT}> stato?")))
        .await;
    assert_eq!(rest.sent(), [(10, "Eccomi".to_owned(), Some(5))]);
    let items = h.timeline_lines();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["text"], "stato?");
    assert_eq!(
        items[0]["meta"],
        json!({"isDm": false, "guildName": "Casa", "channelName": "admin"})
    );
}

#[tokio::test]
async fn strangers_get_replies_but_never_reach_the_timeline() {
    let (h, rest, bot) = setup(vec![]).await;
    bot.clone().handle(dm(6, 77, "ciao")).await;
    bot.clone()
        .handle(in_guild(7, 10, 78, &format!("<@{BOT}> ciao")))
        .await;
    assert_eq!(rest.sent().len(), 2);
    assert!(h.timeline_lines().is_empty());
}

#[tokio::test]
async fn long_replies_are_split_at_2000_characters() {
    let long = "parola ".repeat(700);
    let (_h, rest, bot) = setup(vec![
        json!({"kind": "reply", "contains": "lungo", "content": long}),
    ])
    .await;
    bot.clone()
        .handle(dm(8, OWNER, "scrivi un testo lungo"))
        .await;
    let sent = rest.sent();
    assert_eq!(sent.len(), 3, "{}", sent.len());
    assert!(sent
        .iter()
        .all(|(_, t, r)| t.chars().count() <= 2000 && *r == Some(8)));
}

#[tokio::test]
async fn reminders_go_to_the_owner_telegram_chat_and_are_refused_for_others() {
    let (h, rest, bot) = setup(vec![]).await;
    bot.clone()
        .handle(dm(10, OWNER, "ricordami tra 2 ore di chiamare la mamma"))
        .await;
    let sent = rest.sent();
    assert!(sent[0].1.starts_with("Reminder set for "), "{sent:?}");
    let all = h.state.reminders.list(true).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].chat_id, "42");
    assert_eq!(all[0].source, "discord");
    assert_eq!(all[0].user_peer_id, "giuseppe");

    bot.clone()
        .handle(dm(11, 77, "ricordami tra 2 ore di chiamare la mamma"))
        .await;
    let sent = rest.sent();
    assert_eq!(
        sent[1].1,
        "Lucina: on Discord, reminders are only for my owner."
    );
    assert_eq!(h.state.reminders.list(true).unwrap().len(), 1);
}

fn admin_rules() -> Vec<Value> {
    vec![
        json!({"kind": "intent", "contains": "riassunto", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\"}"}),
        json!({"kind": "plan", "contains": "riassunto", "content": "{\"action\":\"discord_admin_task\",\"operation\":\"server_summary\",\"ack\":\"Controllo il server.\"}"}),
        json!({"kind": "reply", "contains": "skill/tool has already run", "content": "Il server Casa ha 2 membri."}),
    ]
}

#[tokio::test]
async fn the_owner_runs_discord_admin_tasks() {
    let (h, rest, bot) = setup(admin_rules()).await;
    bot.clone()
        .handle(in_guild(
            12,
            10,
            OWNER,
            &format!("<@{BOT}> fammi un riassunto del server"),
        ))
        .await;
    let sent = rest.sent();
    assert_eq!(
        sent,
        [
            (10, "Controllo il server.".to_owned(), Some(12)),
            (10, "Il server Casa ha 2 membri.".to_owned(), Some(12)),
        ],
        "{:?}",
        rest.calls()
    );
    assert!(rest.calls().iter().any(|c| c == "guild_channels 5"));
    assert!(rest.writes().is_empty());
    // The skill reply saw Node's result text.
    let reqs = h.fake.requests();
    let skill = reqs
        .iter()
        .map(|r| r.to_string())
        .find(|r| r.contains("Internal skill result"))
        .expect("skill reply request");
    assert!(skill.contains("Text channels: 2"), "{skill}");
    // Ack and reply on the owner's timeline.
    let texts: Vec<String> = h
        .timeline_lines()
        .iter()
        .map(|i| i["text"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(
        texts.contains(&"Controllo il server.".to_owned()),
        "{texts:?}"
    );
    assert!(texts.contains(&"Il server Casa ha 2 membri.".to_owned()));
}

#[tokio::test]
async fn anyone_else_gets_the_refusal_and_no_discord_call() {
    let (_h, rest, bot) = setup(admin_rules()).await;
    bot.clone()
        .handle(in_guild(
            13,
            10,
            77,
            &format!("<@{BOT}> fammi un riassunto del server"),
        ))
        .await;
    assert_eq!(
        rest.sent(),
        [(
            10,
            "Lucina: non sono riuscito ad aggiornare il server Discord: only the owner can ask me to change a Discord server.".to_owned(),
            Some(13)
        )]
    );
    assert!(!rest
        .calls()
        .iter()
        .any(|c| c.starts_with("guild_channels") || c.starts_with("roles")));
    assert!(rest.writes().is_empty());
}
