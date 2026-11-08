//! Output parity with the Node engine on the recorded fixtures. The goldens
//! in `fixtures/golden/` come from the Node Lucina (`secretary-live`):
//! `node scripts/grocery-deals.mjs digest|alerts|list|lunch|map --dry-run
//! --fixtures tests/fixtures/grocery-deals --items "Prosciutto crudo,Latte,
//! Prosciutto cotto,Pane di segale,Yogurt"` (stdout, empty watch state,
//! default settings) and `engine.mapData/listMatches/lunch/cheapest` as JSON.

mod common;

use std::cmp::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use lucina_deals::format::format_map_summary;
use lucina_deals::js::{self, locale_compare, parse_date};
use lucina_deals::{
    format_lunch, format_matches, DealWatch, Deliver, Delivery, FixedList, FormatOptions,
    MatchOptions, Notification, WatchKind, WatchSettings,
};

fn parity_config() -> lucina_deals::DealsConfig {
    lucina_deals::DealsConfig {
        home_label: Some("home".into()),
        salling_api_key: Some("offline-fixture".into()),
        ..config()
    }
}

fn parity_engine() -> lucina_deals::DealsEngine {
    engine_with(
        parity_config(),
        Arc::new(FixedList::needed(&ITEMS)),
        fixture_http(),
    )
}

struct NoDelivery;

#[async_trait]
impl Deliver for NoDelivery {
    async fn deliver(&self, _: &Notification) -> Delivery {
        panic!("dry runs never deliver")
    }
}

async fn watch_text(kind: WatchKind) -> String {
    let dir = tempfile::tempdir().unwrap();
    let watch = DealWatch::new(
        WatchSettings {
            state_path: dir.path().join("deal-watch.json"),
            home_label: Some("home".into()),
            ..WatchSettings::default()
        },
        Arc::new(parity_engine()),
        Arc::new(NoDelivery),
        "owner",
    )
    .with_clock(lucina_deals::fixed_clock(fixed()));
    let outcome = watch.run(kind, true).await.unwrap();
    assert!(!dir.path().join("deal-watch.json").exists());
    format!("{}\n", outcome.display())
}

#[tokio::test]
async fn digest_text_is_byte_identical() {
    assert_eq!(watch_text(WatchKind::Digest).await, golden("digest.txt"));
}

#[tokio::test]
async fn alerts_text_is_byte_identical() {
    assert_eq!(watch_text(WatchKind::Alerts).await, golden("alerts.txt"));
}

#[tokio::test]
async fn list_text_is_byte_identical() {
    let result = parity_engine().list_matches("owner").await.unwrap();
    assert_eq!(
        format!("{}\n", format_matches(&result, &FormatOptions::default())),
        golden("list.txt")
    );
    let bounded = FormatOptions {
        max_length: 1490,
        ..FormatOptions::default()
    };
    assert_eq!(format_matches(&result, &bounded), golden("list-1490.txt"));
}

#[tokio::test]
async fn lunch_text_is_byte_identical() {
    let result = parity_engine().lunch(&MatchOptions::default()).await;
    assert_eq!(format!("{}\n", format_lunch(&result)), golden("lunch.txt"));
    let tight = engine_with(
        lucina_deals::DealsConfig {
            lunch_budget_dkk: 15.0,
            ..parity_config()
        },
        Arc::new(FixedList::needed(&ITEMS)),
        fixture_http(),
    );
    let result = tight.lunch(&MatchOptions::default()).await;
    assert_eq!(format_lunch(&result), golden("lunch-15.txt"));
}

#[tokio::test]
async fn map_summary_is_byte_identical() {
    let map = parity_engine().map_data("owner", None).await.unwrap();
    assert_eq!(format!("{}\n", format_map_summary(&map)), golden("map.txt"));
}

#[tokio::test]
async fn map_json_is_identical() {
    let map = parity_engine().map_data("owner", None).await.unwrap();
    assert_json_eq(&map.to_json(), &golden_json("map.json"), "map");
    let map = parity_engine()
        .map_data("owner", Some(1500.0))
        .await
        .unwrap();
    assert_json_eq(&map.to_json(), &golden_json("map-1500.json"), "map1500");
}

#[tokio::test]
async fn list_and_lunch_results_are_identical() {
    let list = parity_engine().list_matches("owner").await.unwrap();
    assert_json_eq(&list.to_json(), &golden_json("list.json"), "list");
    let lunch = parity_engine().lunch(&MatchOptions::default()).await;
    assert_json_eq(&lunch.to_json(), &golden_json("lunch.json"), "lunch");
}

#[tokio::test]
async fn cheapest_results_and_text_are_identical() {
    let golden = golden_json("cheapest.json");
    for query in [
        "milk",
        "ham",
        "Prosciutto",
        "rugbrød",
        "skyr",
        "Mozzarella",
        "yoghurt",
    ] {
        let result = parity_engine()
            .cheapest(query, &MatchOptions::default())
            .await;
        assert_json_eq(&result.to_json(), &golden[query]["result"], query);
        assert_eq!(
            format_matches(&result, &FormatOptions::default()),
            golden[query]["text"].as_str().unwrap(),
            "{query}"
        );
    }
    let lidl = parity_engine()
        .cheapest(
            "ham",
            &MatchOptions {
                chain_key: Some("lidl".into()),
                ..MatchOptions::default()
            },
        )
        .await;
    assert_json_eq(&lidl.to_json(), &golden["ham@lidl"]["result"], "ham@lidl");
    let active = parity_engine().list_matches("owner").await.unwrap();
    assert_eq!(
        format_matches(
            &active,
            &FormatOptions {
                active_only: true,
                ..FormatOptions::default()
            }
        ),
        golden["list-active"]["text"].as_str().unwrap()
    );
}

#[test]
fn js_number_formatting_matches_node() {
    let golden = golden_json("js.json");
    for row in golden["numbers"].as_array().unwrap() {
        let x = row[0].as_f64().unwrap();
        assert_eq!(
            js::to_fixed(x, 2),
            row[1].as_str().unwrap(),
            "{x}.toFixed(2)"
        );
        assert_eq!(
            js::to_fixed(x, 1),
            row[2].as_str().unwrap(),
            "{x}.toFixed(1)"
        );
        assert_eq!(
            js::to_fixed(x, 0),
            row[3].as_str().unwrap(),
            "{x}.toFixed(0)"
        );
        assert_eq!(
            js::num_to_string(x),
            row[4].as_str().unwrap(),
            "String({x})"
        );
        assert_eq!(js::round(x), row[5].as_f64().unwrap(), "Math.round({x})");
    }
    for row in golden["dates"].as_array().unwrap() {
        assert_eq!(
            parse_date(row[0].as_str().unwrap()),
            row[1].as_i64(),
            "{}",
            row[0]
        );
    }
}

#[test]
fn locale_compare_orders_fixture_keys_like_node() {
    let golden = golden_json("js.json");
    let expected: Vec<&str> = golden["collation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let mut actual = expected.clone();
    actual.reverse();
    actual.sort_by(|a, b| locale_compare(a, b));
    for (i, (a, e)) in actual.iter().zip(&expected).enumerate() {
        // Node sorts equal-collating strings stably; only distinct ones must agree.
        if a != e {
            assert_eq!(
                locale_compare(a, e),
                Ordering::Equal,
                "position {i}: {a:?} vs node {e:?}"
            );
        }
    }
}
