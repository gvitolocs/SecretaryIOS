//! Grocery deals served natively (crate::deals) from the recorded fixtures:
//! the map route against Node's goldens, deal questions in chat (MCP too)
//! with Node's texts, the fallbacks, and when the deal watch starts.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{harness, harness_with, script};
use lucina_deals::{
    fixed_clock, format_matches, js::parse_date, DealsConfig, DealsEngine, FixedList, FixtureHttp,
    FormatOptions, MatchOptions,
};
use serde_json::{json, Value};

const ITEMS: [&str; 5] = [
    "Prosciutto crudo",
    "Latte",
    "Prosciutto cotto",
    "Pane di segale",
    "Yogurt",
];

fn deals_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../lucina-deals/fixtures")
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(deals_dir().join("golden").join(name)).expect(name)
}

fn golden_json(name: &str) -> Value {
    serde_json::from_str(&golden(name)).expect(name)
}

/// Node's line (`console.log` added the newline).
fn line(name: &str) -> String {
    golden(name).trim_end_matches('\n').to_owned()
}

fn fixture_engine() -> Arc<DealsEngine> {
    let cfg = DealsConfig {
        home_lat: Some(56.19633),
        home_lng: Some(10.20110),
        home_label: Some("home".into()),
        salling_api_key: Some("offline-fixture".into()),
        synonyms_path: PathBuf::from("/nonexistent/deal-synonyms.json"),
        ..DealsConfig::default()
    };
    Arc::new(
        DealsEngine::new(
            cfg,
            Arc::new(FixedList::needed(&ITEMS)),
            Arc::new(FixtureHttp::new(deals_dir().join("grocery-deals"))),
        )
        .with_clock(fixed_clock(parse_date("2026-10-24T09:30:00Z").unwrap())),
    )
}

fn chat_script() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "planner reply"}
    ]}))
}

async fn deals_harness() -> common::H {
    harness_with(
        chat_script(),
        |_| {},
        |st| st.with_deals(Some(fixture_engine())),
    )
    .await
}

#[tokio::test]
async fn the_deals_map_is_nodes_map() {
    let h = deals_harness().await;
    let (s, b, hd) = h
        .call(reqwest::Method::GET, "/grocery/deals/map", None)
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["ok"], true);
    assert_eq!(b["data"], golden_json("map.json"));
    assert_eq!(hd["cache-control"], "no-store");
    let (s, b) = h.get("/grocery/deals/map?radiusM=1500").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"], golden_json("map-1500.json"));
    // Empty is the configured radius.
    let (s, b) = h.get("/grocery/deals/map?radiusM=").await;
    assert_eq!((s, &b["data"]), (200, &golden_json("map.json")));

    for bad in ["10", "15001", "abc", "1500.5", "-600"] {
        let (s, b) = h.get(&format!("/grocery/deals/map?radiusM={bad}")).await;
        assert_eq!(
            (s, b),
            (
                400,
                json!({"ok": false, "error": "bad_request", "message": "radiusM must be an integer from 500 to 15000"})
            ),
            "{bad}"
        );
    }
    let (s, b, hd) = h
        .call(reqwest::Method::POST, "/grocery/deals/map", Some(json!({})))
        .await;
    assert_eq!((s, b["error"].as_str()), (405, Some("method_not_allowed")));
    assert_eq!(hd["allow"], "GET");
    assert_eq!(h.get("/grocery/deals").await.0, 404);
    assert_eq!(h.get("/grocery/deals/map/x").await.0, 404);
    assert_eq!(h.get("/grocery/deals/answer").await.0, 404);
    let r = h
        .http
        .get(h.url("/grocery/deals/map"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[tokio::test]
async fn without_home_coordinates_the_map_is_unavailable() {
    let h = harness(chat_script(), |_| {}).await;
    assert!(h.state.deals.is_none());
    let (s, b) = h.get("/grocery/deals/map").await;
    assert_eq!(
        (s, b),
        (
            503,
            json!({"ok": false, "error": "deals_unavailable", "message": "Nearby deals are unavailable without home coordinates"})
        )
    );
    // Configured coordinates build the engine.
    let h = harness(chat_script(), |c| {
        c.deals.home_lat = Some(56.19633);
        c.deals.home_lng = Some(10.2011);
    })
    .await;
    assert!(h.state.deals.is_some());
}

#[tokio::test]
async fn deal_questions_in_chat_get_the_engines_reply() {
    let h = deals_harness().await;

    // Browse: the shopping list's matches (Node's `list` text).
    let d = h.chat("any good deals?").await;
    assert_eq!(d["reply"], line("list.txt"));
    assert_eq!(d["action"], "grocery");
    assert!(h
        .timeline_lines()
        .iter()
        .any(|l| l["text"] == line("list.txt")));

    let d = h.chat("cheap healthy lunch nearby?").await;
    assert_eq!(d["reply"], line("lunch.txt"));
    assert_eq!(d["action"], "grocery");

    // Cheapest: the engine's five cheapest, Node's text.
    let d = h.chat("Where is milk cheapest?").await;
    let golden = golden_json("cheapest.json");
    assert_eq!(d["reply"], golden["milk"]["text"]);
    let d = h.chat("Where is skyr cheapest?").await;
    assert_eq!(d["reply"], golden["skyr"]["text"]);

    // An explicit chain scopes the offers.
    let engine = fixture_engine();
    let lidl = engine
        .cheapest(
            "ham",
            &MatchOptions {
                chain_key: Some("lidl".into()),
                ..MatchOptions::default()
            },
        )
        .await;
    let d = h.chat("Where is ham cheapest at Lidl?").await;
    assert_eq!(d["reply"], format_matches(&lidl, &FormatOptions::default()));

    // An MCP turn too (the allowlist admits the deal intents); the prefix is stripped.
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from claude-code] cheap healthy lunch nearby?", "sessionId": "mcp-claude-code", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["reply"], line("lunch.txt"));
}

#[tokio::test]
async fn questions_the_engine_cannot_answer_go_on_to_the_grocery_path() {
    // A product offer question without a deal: the catalog path (here: no grocery database).
    let h = deals_harness().await;
    let d = h.chat("any offers on zucchine?").await;
    assert_eq!(d["action"], "grocery");
    assert_eq!(
        d["reply"],
        "Grocery accounting is not configured yet. Set GROCERY_DATABASE_URL and run `npm run grocery:migrate` once."
    );
    // Not a deal question: the planner answers.
    let d = h.chat("ciao come stai").await;
    assert_ne!(d["action"], "grocery");

    // No engine (no home coordinates): Node's reply for deal intents.
    let h = harness(chat_script(), |_| {}).await;
    let d = h.chat("Where is prosciutto crudo cheapest?").await;
    assert_eq!(
        d["reply"],
        "Nearby deal lookup is unavailable. Check home coordinates and deal sources."
    );
    let d = h.chat("any good deals?").await;
    assert_eq!(d["action"], "grocery");
}

#[tokio::test]
async fn the_deal_watch_has_its_own_switch() {
    // Off by default, whatever LUCINA_SCHEDULER says.
    let h = deals_harness().await;
    assert!(!h.state.cfg.deal_watch.enabled);
    assert!(lucina_server::deals::spawn_watch(&h.state).is_none());

    let on = |c: &mut lucina_server::Config| {
        c.deal_watch.enabled = true;
        c.scheduler = false;
    };
    // No engine: stays off.
    let h = harness(chat_script(), on).await;
    assert!(lucina_server::deals::spawn_watch(&h.state).is_none());
    // An unknown channel: stays off (Node refuses to start it).
    let h = harness_with(
        chat_script(),
        |c| {
            on(c);
            c.deal_watch.channel = "sms".into();
        },
        |st| st.with_deals(Some(fixture_engine())),
    )
    .await;
    assert!(lucina_server::deals::spawn_watch(&h.state).is_none());
    // Enabled with an engine; Telegram channels are delivered to the app.
    for channel in ["app", "both"] {
        let h = harness_with(
            chat_script(),
            |c| {
                on(c);
                c.deal_watch.channel = channel.into();
            },
            |st| st.with_deals(Some(fixture_engine())),
        )
        .await;
        let task = lucina_server::deals::spawn_watch(&h.state).expect(channel);
        task.abort();
    }
}

#[test]
fn the_server_reads_nodes_deal_variables() {
    let env = |k: &str| {
        match k {
            "LUCINA_DATA_DIR" => Some("/srv/data"),
            "GROCERY_HOME_LAT" => Some("56.19633"),
            "GROCERY_HOME_LNG" => Some("10.2011"),
            "GROCERY_HOME_LABEL" => Some("Risskov"),
            "GROCERY_NEARBY_RADIUS_M" => Some("2500"),
            "SALLING_API_KEY" => Some("salling-secret"),
            "GROCERY_OFFER_WATCH_ENABLED" => Some("true"),
            "LUCINA_REMINDER_TIMEZONE" => Some("Europe/Copenhagen"),
            _ => None,
        }
        .map(str::to_owned)
    };
    let c = lucina_server::Config::from_lookup(env).unwrap();
    assert!(c.deals.has_home());
    assert_eq!(c.deals.home_label.as_deref(), Some("Risskov"));
    assert_eq!(c.deals.radius_m, 2500.0);
    assert_eq!(c.deals.salling_api_key.as_deref(), Some("salling-secret"));
    assert!(c.deal_watch.enabled);
    assert!(!c.scheduler);
    assert_eq!(
        c.deal_watch.state_path,
        PathBuf::from("/srv/data/grocery/deal-watch.json")
    );
    assert_eq!(c.deal_watch.home_label, "Risskov");
    // The key is never in the debug output.
    assert!(!format!("{c:?}").contains("salling-secret"));
}
