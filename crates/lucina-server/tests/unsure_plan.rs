//! CO-C005 (TURN-02): of the turns Jev could not place, only the owner's own go to the JSON
//! planner — one plan, then the one action that plan names. The same words from an agent, from a
//! surface that is not his own (`Direct`, a group, a delegate, background work), or with
//! `LUCINA_UNSURE_PLAN` off are answered with words, as before.
//!
//! The other planner mode — the native loop, the one `LUCINA_PLANNER=tools` selects — is not what
//! an unsure turn can be given: it works the registry over as many calls as it likes, where the
//! finding asks for the turn to be planned once and that plan's one action to run, and it never
//! asks the classifier at all. A model request of the JSON planner also carries no action list to
//! read back. So a turn is checked twice: in the classification `POST /jev/route` returns, and in
//! the model calls a real turn makes — a plan, or a plain chat answer.

mod common;

use common::{harness, script, H};
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A turn Jev cannot place: no tier-0 rule takes it, its words are not actionable on their own, and
/// Jev's answer is `unknown` at a confidence no threshold accepts.
const SAYING: &str = "move the seed trays from the porch into the greenhouse";

const BOT_TOKEN: &str = "555:TEST_BOT_SECRET";
/// The owner's Telegram chat, and another person's private one: [`lucina_memory::Surface::Direct`].
const OWNER_CHAT: &str = "42";
const STRANGER_CHAT: &str = "77";

/// Jev, with no idea what the message is about. Its client reads its settings from the environment
/// once, so this is the whole binary's Jev.
async fn jev_has_no_idea() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": {
                "intent": {"type": "choice", "choice": "unknown", "confidence": 0.2,
                           "probabilities": {"unknown": 0.2, "chat": 0.2}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.5,
                          "probabilities": {"all": 0.5, "today": 0.5}},
                "needs_memory": {"type": "noul", "noul": 0.2},
                "needs_tools": {"type": "noul", "noul": 0.2},
            }
        })))
        .mount(&server)
        .await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", server.uri()));
    std::env::set_var("LUCINA_JEV_FIRST", "1");
    assert!(
        lucina_jev::shared().available(),
        "the stub Jev is not in use"
    );
    server
}

/// The Telegram bot API: every send answers, so a turn finishes and its model calls can be read.
async fn bot_api() -> MockServer {
    let s = MockServer::start().await;
    for path in [
        "/sendMessage$",
        "/(sendChatAction|getWebhookInfo|deleteMessage)$",
    ] {
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path_regex(path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": true, "result": {"message_id": 1}})),
            )
            .mount(&s)
            .await;
    }
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path_regex("/getMe$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"ok": true, "result": {"id": 555, "is_bot": true, "username": "Lucina00_bot"}}),
        ))
        .mount(&s)
        .await;
    s
}

/// A private Telegram message from `chat` to the bot.
fn text_update(id: u64, chat: &str, text: &str) -> Value {
    let chat: i64 = chat.parse().unwrap();
    json!({"update_id": id, "message": {
        "message_id": id, "date": 1_790_000_000,
        "chat": {"id": chat, "type": "private", "title": Value::Null},
        "from": {"id": chat, "is_bot": false, "first_name": "Person", "username": "person"},
        "text": text
    }})
}

/// The model calls a turn made, from request number `from` on.
fn kinds(h: &H, from: usize) -> Vec<&'static str> {
    h.fake
        .requests()
        .iter()
        .skip(from)
        .map(lucina_server::fake::kind_of)
        .collect()
}

/// How many of them were of kind `kind`.
fn count(ks: &[&'static str], kind: &str) -> usize {
    ks.iter().filter(|k| **k == kind).count()
}

/// What the classifier makes of [`SAYING`]: an agent's turn or the owner's (`mcp`), on his own
/// surface or not (`owner_turn`) — the two facts `prepare_keyed` reads off a real turn.
async fn routed(h: &H, mcp: bool, owner_turn: bool) -> Value {
    let (s, b) = h
        .post(
            "/jev/route",
            json!({"text": SAYING, "mcp": mcp, "ownerTurn": owner_turn}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"]["routed"].clone()
}

#[tokio::test]
async fn only_the_owners_own_unsure_turn_goes_to_the_planner() {
    // The shape every case below rests on: nothing else takes this sentence, so a plain chat
    // classification answers it with words alone.
    assert!(
        lucina_intent::tier0::classify_tier0(
            SAYING,
            "",
            &lucina_intent::heuristics::Settings::default()
        )
        .is_none(),
        "a tier-0 rule takes the sentence"
    );
    assert!(
        !lucina_intent::heuristics::looks_like_actionable(SAYING),
        "the sentence reads as actionable on its own"
    );

    let _jev = jev_has_no_idea().await;
    // The switch is read from the environment by `context::unsure_plan`; this binary names it.
    std::env::set_var("LUCINA_UNSURE_PLAN", "on");
    let h = harness(
        script(json!({"rules": [
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"I will sort out the trays.\"}"},
            {"kind": "reply", "content": "Nice plan."}
        ]})),
        |c| {
            c.telegram.enabled = true;
            c.telegram.allowed_chat_ids = vec![OWNER_CHAT.into(), STRANGER_CHAT.into()];
            c.telegram.operator_chat_id = Some(OWNER_CHAT.into());
            c.telegram.decline_unauthorized = true;
            c.telegram_token = Some(BOT_TOKEN.into());
            c.telegram.lock_dir = Some(c.data_dir.join("locks"));
        },
    )
    .await;
    let api = bot_api().await;
    let bot = lucina_server::telegram::Bot::new(
        h.state.clone(),
        lucina_telegram::BotApi::with_base_url(BOT_TOKEN.into(), &api.uri()),
    );

    // 1. His own turn: the `chat` label stands, but the planner's path takes it.
    let r = routed(&h, false, true).await;
    assert_eq!(r["intent"], "chat", "{r}");
    assert_eq!(r["replyMode"], "full_plan", "{r}");

    // 2. An agent's words, and a turn that is not on his own surface: words, as before.
    for (mcp, owner_turn) in [(true, true), (false, false), (true, false)] {
        let r = routed(&h, mcp, owner_turn).await;
        assert_eq!(
            r["replyMode"], "chat",
            "mcp={mcp} owner_turn={owner_turn} {r}"
        );
    }

    // 3. A real turn of his own, through the app: one plan call, and no plain chat answer.
    let n = h.fake.requests().len();
    let d = h.chat(SAYING).await;
    let ks = kinds(&h, n);
    assert_eq!(
        count(&ks, "plan"),
        1,
        "the planner never saw the turn: {ks:?} {d}"
    );
    assert_eq!(
        count(&ks, "reply"),
        0,
        "the turn was answered as a plain chat: {ks:?}"
    );

    // 4. The same two shapes on a channel, where the turn brings its own surface: the owner's
    //    private chat goes to the planner like his app does.
    let n = h.fake.requests().len();
    bot.clone().handle(text_update(5, OWNER_CHAT, SAYING)).await;
    let ks = kinds(&h, n);
    assert_eq!(
        count(&ks, "plan"),
        1,
        "the owner's own channel turn did not reach the planner once: {ks:?}"
    );
    assert_eq!(
        count(&ks, "reply"),
        0,
        "the owner's channel turn was answered as a plain chat: {ks:?}"
    );

    //    Another person's private chat — a `Direct` surface, no rule, Jev as unsure as ever — is
    //    answered with words: the one chat call it always made, and no plan call.
    let n = h.fake.requests().len();
    bot.clone()
        .handle(text_update(6, STRANGER_CHAT, SAYING))
        .await;
    let ks = kinds(&h, n);
    assert_eq!(
        count(&ks, "plan"),
        0,
        "another person's turn went to the planner: {ks:?}"
    );
    assert_eq!(
        count(&ks, "reply"),
        1,
        "the turn did not make its one plain chat call: {ks:?}"
    );

    // 5. `LUCINA_UNSURE_PLAN` off: his own turn is answered as before.
    std::env::set_var("LUCINA_UNSURE_PLAN", "off");
    let r = routed(&h, false, true).await;
    assert_eq!(r["replyMode"], "chat", "{r}");
    std::env::remove_var("LUCINA_UNSURE_PLAN");
}
