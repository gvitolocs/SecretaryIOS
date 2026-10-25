//! `tests/grocery-deals.test.mjs` of the Node Lucina, ported test by test
//! (same fixtures, same fixed `now`). Node tests of other modules (intent
//! parser, grocery service, CLI, entrypoint imports, the scheduler tick and
//! the Telegram/app channel routing) belong to their own ports.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use lucina_deals::format::format_map_summary;
use lucina_deals::js::{parse_date, utf16_len};
use lucina_deals::matcher::{match_deal, normalize};
use lucina_deals::sources::{DealsHttp, HttpError, Sources};
use lucina_deals::{
    default_rules, format_deal, format_lunch, format_matches, resolve_rule, validity, Deal,
    DealSource, DealsConfig, FixedList, FixtureHttp, FormatOptions, MatchItem, MatchOptions,
    ShoppingRow, Status, Synonyms,
};
use regex::Regex;
use reqwest::Url;
use serde_json::{json, Value};

fn hit_with(id: &str, name: &str, source: DealSource, category: Option<&str>) -> i32 {
    let rules = default_rules();
    let rule = rules.iter().find(|r| r.id == id).unwrap();
    let mut d = deal("k", source, name, 1.0);
    d.category = category.map(str::to_owned);
    match_deal(rule, &d)
}

fn hit(id: &str, name: &str) -> i32 {
    hit_with(id, name, DealSource::Flyer, None)
}

#[test]
fn head_final_matching_multilingual_aliases_and_phrase_exclusions() {
    for name in [
        "DULANO Parmaskinke",
        "Serranoskinke",
        "DELUXE Jamón Ibérico",
    ] {
        assert!(hit("prosciutto_crudo", name) != 0, "{name}");
    }
    for name in [
        "Skinkeschnitzler",
        "Protein Lab pizza",
        "skinkepizza",
        "BUKO Skinke- eller rejeost",
        "Ribbenssteg eller hel skinke",
    ] {
        assert_eq!(hit("prosciutto_crudo", name), 0, "{name}");
        assert_eq!(hit("ham", name), 0, "{name}");
    }
    for name in [
        "DULANO Hamburgerryg",
        "RØGET HAMBURGERRYG AF DANSK GRIS",
        "Silkeskåret skinke",
        "landskinke",
    ] {
        assert!(hit("ham", name) != 0, "{name}");
    }
    assert_eq!(hit("ham", "DULANO Parmaskinke"), 0);
    assert!(hit("rye_bread", "kernerugbrød") != 0);
    assert_eq!(hit("rye_bread", "rugbrødschips"), 0);
    assert!(hit("milk", "letmælk") != 0);
    for name in [
        "mælkechokolade",
        "kokosmælk",
        "havremælk",
        "kærnemælk",
        "kakaomælk",
        "chokolademælk",
        "tykmælk",
        "MILBONA Kakaomælk",
        "Arla økologisk yoghurt, ymer el. tykmælk",
    ] {
        assert_eq!(hit("milk", name), 0, "{name}");
    }
    assert_eq!(
        hit_with(
            "ham",
            "SKINKE SCHNITZLER",
            DealSource::FoodWaste,
            Some("Grisekød")
        ),
        0
    );
    assert_eq!(
        hit_with("rye_bread", "rugbrød", DealSource::FoodWaste, Some("Chips")),
        0
    );
    let rules = default_rules();
    for (names, id) in [
        (
            &["Prosciutto", "Prosciutto crudo", "Parma ham"][..],
            "prosciutto_crudo",
        ),
        (&["Prosciutto cotto", "Ham", "Skinke"][..], "ham"),
        (&["Latte", "Milk", "Mælk"][..], "milk"),
        (&["Pane di segale", "Rye bread", "Rugbrød"][..], "rye_bread"),
        (&["Yogurt", "Yoghurt", "Skyr"][..], "yogurt"),
    ] {
        for name in names {
            assert_eq!(resolve_rule(name, &rules).rule.id, id, "{name}");
        }
    }
    let mozzarella = resolve_rule("Mozzarella", &rules).rule;
    assert!(
        match_deal(
            &mozzarella,
            &deal("k", DealSource::Flyer, "Galbani mozzarella", 1.0)
        ) != 0
    );
    assert_eq!(normalize("É yoghurt æøå"), "e yogurt æøå");
    assert!(hit("ham", "skinke") > hit("ham", "landskinke"));
}

#[test]
fn fixture_headings_match_rye_bread_yogurt_and_skyr_without_descriptions() {
    for term in ["rugbrød", "yoghurt", "skyr"] {
        let rows = fixture_json(&format!("tjek-offers-{term}.json"));
        let rule = if term == "rugbrød" {
            "rye_bread"
        } else {
            "yogurt"
        };
        assert!(rows
            .as_array()
            .unwrap()
            .iter()
            .any(|r| hit(rule, r["heading"].as_str().unwrap()) != 0));
    }
    // Descriptions are never matched (a deal has no description field).
    assert_eq!(hit("ham", "Laks"), 0);
}

#[test]
fn validity_uses_exclusive_end_and_three_day_upcoming_horizon() {
    let (from, to) = ("2026-10-23T09:30:00Z", "2026-10-24T09:30:00Z");
    assert_eq!(validity(Some(from), Some(to), fixed() - 1), Status::Active);
    assert_eq!(validity(Some(from), Some(to), fixed()), Status::Expired);
    let rows = fixture_json("tjek-offers-skinke.json");
    let lidl = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_from"].as_str().unwrap().starts_with("2026-10-28"))
        .unwrap();
    let (from, to) = (lidl["run_from"].as_str(), lidl["run_till"].as_str());
    assert_ne!(validity(from, to, fixed()), Status::Active);
    assert_eq!(
        validity(from, to, parse_date("2026-10-26T09:30:00Z").unwrap()),
        Status::Upcoming
    );
}

/// The fixture transport, counting requests and failing food waste on demand.
struct CountingHttp {
    inner: FixtureHttp,
    calls: AtomicUsize,
    fail_food_waste: bool,
}

#[async_trait]
impl DealsHttp for CountingHttp {
    async fn get_json(&self, url: &Url, headers: &[(String, String)]) -> Result<Value, HttpError> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail_food_waste && url.path().contains("food-waste") {
            return Err(HttpError("offline".into()));
        }
        self.inner.get_json(url, headers).await
    }
}

fn counting(fail_food_waste: bool) -> Arc<CountingHttp> {
    Arc::new(CountingHttp {
        inner: FixtureHttp::new(fixture_dir()),
        calls: AtomicUsize::new(0),
        fail_food_waste,
    })
}

#[tokio::test]
async fn sources_enforce_radius_and_dealer_filters_cache_requests_and_isolate_failures() {
    let http = counting(false);
    let clock = TestClock::new(fixed());
    let sources = Sources::new(Arc::new(config()), http.clone(), clock.clock());
    let terms = vec!["skinke".to_owned(), "parmaskinke".to_owned()];
    let result = sources.flyers(&terms, 3000.0).await.unwrap();
    assert!(!result.deals.is_empty());
    let banned = Regex::new("(?i)JYSK|Matas|Wolt").unwrap();
    assert!(result.deals.iter().all(|d| d.store.distance_m <= 3000.0
        && !banned.is_match(&lucina_deals::js::display(d.chain.as_ref()))));
    let count = http.calls.load(AtomicOrdering::SeqCst);
    sources.flyers(&terms, 3000.0).await.unwrap();
    assert_eq!(http.calls.load(AtomicOrdering::SeqCst), count);
    clock.set(clock.get() + 1_800_001);
    sources.flyers(&terms, 3000.0).await.unwrap();
    assert_eq!(http.calls.load(AtomicOrdering::SeqCst), count + 2);
    assert!(lucina_deals::sources::haversine(56.19633, 10.20110, 56.19633, 10.20110) < 1.0);

    let broken = engine_with(
        config(),
        Arc::new(FixedList::needed(&ITEMS)),
        counting(true),
    );
    let matches = broken.list_matches("owner").await.unwrap();
    assert_eq!(matches.failures, vec!["food_waste"]);
    assert!(matches.items.iter().any(|i| !i.deals.is_empty()));
    let tiny = engine()
        .cheapest(
            "ham",
            &MatchOptions {
                radius_m: Some(10.0),
                ..MatchOptions::default()
            },
        )
        .await;
    assert!(tiny.items[0].deals.is_empty());
}

#[tokio::test]
async fn sources_share_one_request_in_flight() {
    let http = counting(false);
    let sources = Sources::new(
        Arc::new(config()),
        http.clone(),
        lucina_deals::fixed_clock(fixed()),
    );
    let (a, b, c) = futures_util::join!(
        sources.food_waste(3000.0),
        sources.food_waste_stores(3000.0),
        sources.tjek_stores(3000.0)
    );
    assert!(!a.unwrap().is_empty() && !b.unwrap().is_empty() && !c.unwrap().is_empty());
    assert_eq!(http.calls.load(AtomicOrdering::SeqCst), 2);
    let unconfigured = Sources::new(
        Arc::new(DealsConfig {
            salling_api_key: None,
            ..config()
        }),
        http.clone(),
        lucina_deals::fixed_clock(fixed()),
    );
    assert!(unconfigured.food_waste(3000.0).await.is_err());
}

#[tokio::test]
async fn map_data_merges_food_waste_and_tjek_branches_keeps_coordinates_and_distance_order() {
    let result = engine().map_data("owner", Some(3000.0)).await.unwrap();
    let v = result.to_json();
    assert_eq!(result.radius_m, 3000.0);
    assert_eq!(
        v["home"],
        json!({"lat": 56.19633, "lng": 10.2011, "label": null})
    );
    assert!(result.stores.len() > 5);
    let distances: Vec<i64> = result.stores.iter().map(|s| s.distance_m).collect();
    let mut sorted = distances.clone();
    sorted.sort();
    assert_eq!(distances, sorted);
    for s in &result.stores {
        assert!(s.lat.is_finite() && s.lng.is_finite());
        assert!(s.distance_m <= 3000);
    }
    for s in v["stores"].as_array().unwrap() {
        for k in ["listMatches", "clearances", "flyerOffers"] {
            assert!(s[k].is_array());
        }
    }
    let skejby: Vec<&Value> = v["stores"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["street"] == "Skelagervej 1-9")
        .collect();
    assert_eq!(skejby.len(), 1);
    assert_eq!(
        skejby[0]["id"],
        "salling:0847307a-6cf2-4c96-9d02-03e634dd1608"
    );
    assert_eq!(skejby[0]["chain"], "føtex");
    assert_eq!(skejby[0]["chainKey"], "foetex");
    assert_eq!(skejby[0]["name"], "føtex Skejby");
}

#[tokio::test]
async fn map_flyer_matches_reach_every_branch_of_the_dealer() {
    let result = engine().map_data("owner", None).await.unwrap();
    let netto: Vec<_> = result
        .stores
        .iter()
        .filter(|s| s.chain_key == "netto")
        .collect();
    assert!(netto.len() >= 5);
    let yog = Regex::new("(?i)yoghurt|plantgurt").unwrap();
    for store in netto {
        let name = lucina_deals::js::display(store.name.as_ref());
        assert!(
            store
                .flyer_offers
                .iter()
                .any(|d| yog.is_match(&d.name_text())),
            "{name} flyer offers"
        );
        assert!(
            store
                .list_matches
                .iter()
                .any(|m| m.item == "Yogurt" && m.deal.source == DealSource::Flyer),
            "{name} list matches"
        );
        let keys: HashSet<&str> = store
            .list_matches
            .iter()
            .filter(|m| m.deal.source == DealSource::Flyer)
            .map(|m| m.deal.key.as_str())
            .collect();
        assert!(store
            .flyer_offers
            .iter()
            .all(|d| keys.contains(d.key.as_str())));
    }
}

#[tokio::test]
async fn map_clearances_sorted_by_discount_capped_upcoming_included_expired_excluded() {
    let result = engine().map_data("owner", None).await.unwrap();
    let skejby = result
        .stores
        .iter()
        .find(|s| s.id == "salling:0847307a-6cf2-4c96-9d02-03e634dd1608")
        .unwrap();
    assert_eq!(skejby.clearances.len(), 40);
    for store in &result.stores {
        assert!(store.clearances.len() <= 40);
        assert!(store
            .clearances
            .iter()
            .all(|d| d.source == DealSource::FoodWaste));
        for w in store.clearances.windows(2) {
            assert!(w[0].discount_pct.unwrap_or(-1.0) >= w[1].discount_pct.unwrap_or(-1.0));
        }
    }
    let deals: Vec<&Deal> = result
        .stores
        .iter()
        .flat_map(|s| {
            s.clearances
                .iter()
                .chain(&s.flyer_offers)
                .chain(s.list_matches.iter().map(|m| &m.deal))
        })
        .collect();
    assert!(deals
        .iter()
        .all(|d| matches!(d.status, Status::Active | Status::Upcoming)));
    assert!(deals.iter().any(|d| d.status == Status::Upcoming));
    let banned_key = Regex::new("(?i)^(matas|jysk|normal|stark|silvan|ikea)$").unwrap();
    let banned_chain = Regex::new("Matas|JYSK|Normal|STARK").unwrap();
    assert!(!result
        .stores
        .iter()
        .any(|s| banned_key.is_match(&s.chain_key)
            || banned_chain.is_match(&lucina_deals::js::display(s.chain.as_ref()))));
}

#[tokio::test]
async fn map_summary_prints_one_line_per_nearby_store_with_counts() {
    let map = engine().map_data("owner", None).await.unwrap();
    let text = format_map_summary(&map);
    assert!(Regex::new(r"(?m)^Deals map: \d+ stores within 3000 m of ")
        .unwrap()
        .is_match(&text));
    assert!(
        Regex::new(r"føtex Skejby — \d+ m · \d+ clearances · \d+ matches · \d+ flyer offers")
            .unwrap()
            .is_match(&text)
    );
}

#[tokio::test]
async fn list_dedupes_offers_across_terms_and_items_prices_sorted_output_bounded() {
    let result = engine().list_matches("owner").await.unwrap();
    assert_eq!(result.items.len(), 5);
    let keys: Vec<&str> = result
        .items
        .iter()
        .flat_map(|i| i.deals.iter().map(|d| d.key.as_str()))
        .collect();
    assert_eq!(keys.len(), keys.iter().collect::<HashSet<_>>().len());
    for i in &result.items {
        let prices: Vec<f64> = i.deals.iter().map(|d| d.price_dkk).collect();
        let mut sorted = prices.clone();
        sorted.sort_by(f64::total_cmp);
        assert_eq!(prices, sorted);
    }
    assert!(result
        .items
        .iter()
        .any(|i| i.deals.iter().any(|d| d.more_stores.unwrap_or(0) != 0)));
    assert!(utf16_len(&format_matches(&result, &FormatOptions::default())) <= 3500);
    let bounded = FormatOptions {
        max_length: 1490,
        ..FormatOptions::default()
    };
    assert!(utf16_len(&format_matches(&result, &bounded)) <= 1490);
    let cheap = engine().cheapest("milk", &MatchOptions::default()).await;
    assert!(cheap.items[0].deals.iter().any(|d| d.unit_price.is_some()));
    let scoped = engine()
        .cheapest(
            "ham",
            &MatchOptions {
                chain_key: Some("lidl".into()),
                ..MatchOptions::default()
            },
        )
        .await;
    assert!(scoped.items[0].deals.iter().all(|d| d.chain_key == "lidl"));
}

#[test]
fn synonym_edits_hot_reload_and_can_add_remove_defaults_or_add_products() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rules.json");
    let synonyms = Synonyms::new(&file);
    std::fs::write(
        &file,
        json!([
            {"id": "milk", "add_terms": ["testmælk"], "remove_terms": ["milk"], "add_exclude": ["bad"]},
            {"id": "cheese", "names": ["ost"], "terms": ["ost"], "exclude": []}
        ])
        .to_string(),
    )
    .unwrap();
    let rules = synonyms.rules();
    let latte = resolve_rule("latte", &rules).rule;
    assert!(latte.terms.contains(&"testmælk".to_owned()));
    assert!(!latte.terms.contains(&"milk".to_owned()));
    assert!(latte.exclude.contains(&"bad".to_owned()));
    assert_eq!(resolve_rule("ost", &rules).rule.id, "cheese");
    std::fs::write(&file, "broken json").unwrap();
    assert_eq!(*synonyms.rules(), *rules);
    std::fs::write(
        &file,
        json!([{"id": "milk", "add_terms": ["newterm"]}]).to_string(),
    )
    .unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert!(resolve_rule("latte", &synonyms.rules())
        .rule
        .terms
        .contains(&"newterm".to_owned()));
    std::fs::remove_file(&file).unwrap();
    assert_eq!(*synonyms.rules(), default_rules());
}

#[tokio::test]
async fn lunch_combinations_stay_within_budget_healthy_and_at_one_branch() {
    let result = engine().lunch(&MatchOptions::default()).await;
    assert!(!result.suggestions.is_empty());
    let veto = Regex::new("(?i)chips|pizza|bacon|pølse|burger|russisk salat").unwrap();
    for s in &result.suggestions {
        assert!(s.total <= 50.0);
        assert!(s.items.iter().any(|i| i.protein));
        assert!(s.items.iter().any(|i| i.fibre));
        assert!(s
            .items
            .iter()
            .all(|d| d.deal.store.street == s.store.street && d.deal.status == Status::Active));
        assert!(s.items.iter().all(|d| !veto.is_match(&d.deal.name_text())));
    }
}

#[tokio::test]
async fn tjek_live_unit_objects_and_flattened_units_yield_kg_and_l_prices() {
    let template = fixture_json("tjek-offers-parmaskinke.json")[0].clone();
    let variants = [
        ("g", 100.0, 2.0, "100.00 DKK/kg"),
        ("kg", 0.5, 1.0, "40.00 DKK/kg"),
        ("ml", 250.0, 2.0, "40.00 DKK/l"),
        ("cl", 25.0, 1.0, "80.00 DKK/l"),
        ("l", 1.0, 1.0, "20.00 DKK/l"),
    ];
    let mut expected: HashMap<String, Option<&str>> = HashMap::new();
    let mut rows = Vec::new();
    for (unit, size, pieces, price) in variants {
        for live in [true, false] {
            let id = format!("{unit}-{live}");
            let mut row = template.clone();
            row["id"] = json!(id);
            row["pricing"] = json!({"price": 20});
            row["quantity"] = json!({
                "unit": if live { json!({"symbol": unit, "si": {"factor": 1}}) } else { json!(unit) },
                "size": {"from": size, "to": size},
                "pieces": {"from": pieces, "to": pieces},
            });
            rows.push(row);
            expected.insert(format!("tjek:{id}"), Some(price));
        }
    }
    let mut range = rows[0].clone();
    range["id"] = json!("range");
    range["quantity"]["size"] = json!({"from": 100, "to": 200});
    rows.push(range);
    let rows = Value::Array(rows);
    struct Custom(Value, FixtureHttp);
    #[async_trait]
    impl DealsHttp for Custom {
        async fn get_json(&self, url: &Url, h: &[(String, String)]) -> Result<Value, HttpError> {
            if url.path().contains("offers/search") {
                Ok(self.0.clone())
            } else {
                self.1.get_json(url, h).await
            }
        }
    }
    let sources = Sources::new(
        Arc::new(config()),
        Arc::new(Custom(rows.clone(), FixtureHttp::new(fixture_dir()))),
        lucina_deals::fixed_clock(fixed()),
    );
    let result = sources.flyers(&["test".to_owned()], 3000.0).await.unwrap();
    assert_eq!(result.deals.len(), rows.as_array().unwrap().len());
    for d in &result.deals {
        assert_eq!(
            d.unit_price.as_deref(),
            expected.get(&d.key).copied().flatten(),
            "{}",
            d.key
        );
    }
}

#[tokio::test]
async fn list_displays_localised_names_and_chooses_first_known_rule_across_row_names() {
    let row = |name: &str, raw: &str, product: Option<&str>| ShoppingRow {
        name: Some(name.into()),
        raw_name: Some(raw.into()),
        product: product.map(str::to_owned),
        status: "needed".into(),
    };
    let rows = vec![
        row("Prosciutto crudo", "prosciutto", Some("ham")),
        row("Latte", "milk", None),
        row("Owner custom label", "rye bread", Some("ham")),
        row("Another custom label", "unrecognised", Some("yogurt")),
        row("Mozzarella", "unrecognised", None),
    ];
    let engine = engine_with(config(), Arc::new(FixedList(rows.clone())), fixture_http());
    let result = engine.list_matches("owner").await.unwrap();
    let names: Vec<&str> = result.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(
        names,
        rows.iter()
            .map(|r| r.name.as_deref().unwrap())
            .collect::<Vec<_>>()
    );
    let ids: Vec<&str> = result.items.iter().map(|i| i.rule_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "prosciutto_crudo",
            "milk",
            "rye_bread",
            "yogurt",
            "mozzarella"
        ]
    );
    assert!(result.items[..4].iter().all(|i| !i.deals.is_empty()));
    let text = format_matches(&result, &FormatOptions::default());
    assert!(text.starts_with("Prosciutto crudo:"));
    assert!(text.contains("\nLatte:"));
    assert!(!text.contains("\nmilk:"));
}

#[test]
fn yogurt_rules_omit_yog_abbreviations_and_milk_rejects_flavoured_and_cultured_milk() {
    let rules = default_rules();
    assert_eq!(
        rules.iter().find(|r| r.id == "yogurt").unwrap().terms,
        ["yoghurt", "yogurt", "skyr", "ymer"]
    );
    assert_eq!(hit("yogurt", "DRIK YOG CITRON EGELYKKE"), 0);
    assert!(hit("yogurt", "Arla økologisk yoghurt, ymer el. tykmælk") != 0);
    assert!(hit("milk", "MILBONA Letmælk") != 0);
}

#[tokio::test]
async fn format_uses_branch_names_for_clearances_chain_names_for_flyers_and_middle_dots() {
    let result = engine().list_matches("owner").await.unwrap();
    let all: Vec<&Deal> = result.items.iter().flat_map(|i| &i.deals).collect();
    let waste = all
        .iter()
        .find(|d| d.source == DealSource::FoodWaste)
        .unwrap();
    let text = format_deal(waste, fixed());
    let d = lucina_deals::js::display;
    assert!(text.contains(&format!(
        "— {}, {}",
        d(waste.store.name.as_ref()),
        d(waste.store.street.as_ref())
    )));
    assert!(
        Regex::new(r" · clearance, \d+(?:\.\d+)?(?: kg)? left · until")
            .unwrap()
            .is_match(&text)
    );
    assert_eq!(text.matches("clearance").count(), 1);
    assert!(!text.contains(';') && !text.contains("— netto,"));
    let flyer = all.iter().find(|d| d.source == DealSource::Flyer).unwrap();
    let flyer_text = format_deal(flyer, fixed());
    assert!(flyer_text.contains(&format!(
        "— {}, {}",
        d(flyer.chain.as_ref()),
        d(flyer.store.street.as_ref())
    )));
    assert!(!flyer_text.contains("clearance"));
    let lunch = format_lunch(&engine().lunch(&MatchOptions::default()).await);
    assert!(lunch.contains(" · clearance · until"));
    assert!(!lunch.contains(';'));
}

#[test]
fn explicit_notification_misses_and_omissions_do_not_change_chat_defaults() {
    let missing = MatchItem {
        name: "Missing".into(),
        rule_id: "missing".into(),
        deals: Vec::new(),
    };
    let result = result_of(vec![missing, notification_item(1)]);
    assert!(format_matches(&result, &FormatOptions::default()).contains("No deal: Missing."));
    let notification = format_matches(
        &result,
        &FormatOptions {
            misses: Some(Vec::new()),
            omitted: 2,
            ..FormatOptions::default()
        },
    );
    assert!(!notification.contains("No deal:"));
    assert!(notification.contains("+2 more items"));
    let mut long = result_of(vec![notification_item(1)]);
    long.items[0].deals[0].name = Some(Value::String("Long offer ".repeat(140)));
    assert!(utf16_len(&format_matches(&long, &FormatOptions::default())) > 1490);
}

// ---- watch ----

#[tokio::test]
async fn watch_digest_incremental_alerts_price_drops_failure_caps_and_dry_run_persistence() {
    let mut h = Harness::with_fixture_result().await;
    h.settings.alert_max_per_day = 2.0;
    h.watch = h.restart();
    let dry = h.watch.run_digest(true).await.unwrap();
    assert!(!dry.text.is_empty());
    assert!(!dry.sent);
    assert!(!h.file.exists());
    assert!(h.watch.run_digest(false).await.unwrap().sent);
    assert_eq!(h.deliver.delivered.lock().unwrap().len(), 1);
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "already digested today"
    );
    assert!(!h.watch.run_alerts(false).await.unwrap().sent);
    let base = h.source.result.lock().unwrap().clone();
    let new_deal = |price: f64| Deal {
        key: "tjek:new".into(),
        name: Some(json!("New Parmaskinke")),
        price_dkk: price,
        ..base.items[0].deals[0].clone()
    };
    let with_new = |price: f64| {
        let mut r = base.clone();
        r.items[0].deals.insert(0, new_deal(price));
        r
    };
    h.set_result(with_new(2.0));
    h.set_fail(true);
    let before = h.file_text();
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "send failed"
    );
    assert_eq!(h.file_text(), before);
    h.set_fail(false);
    let alert = h.watch.run_alerts(false).await.unwrap();
    assert!(alert.sent);
    assert!(alert.text.contains("New Parmaskinke"));
    assert!(!alert.text.contains("Latte:"));
    h.set_result(with_new(1.0));
    assert!(h.watch.run_alerts(false).await.unwrap().sent);
    h.set_result(with_new(0.5));
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "daily alert cap"
    );
    h.clock.set(h.clock.get() + 86_400_000);
    assert!(h.watch.run_digest(false).await.unwrap().sent);
    h.clock.set(h.clock.get() + 86_400_000);
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "unchanged digest"
    );
}

#[tokio::test]
async fn watch_is_silent_for_empty_matches_and_records_the_digest_day() {
    let h = Harness::new(result_of(Vec::new()));
    h.clock.set(parse_date("2026-10-24T04:00:00Z").unwrap());
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "no active matches"
    );
    assert_eq!(h.state()["lastDigestDate"], "2026-10-24");
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "already digested today"
    );
    assert_eq!(h.calls(), 1);
}

#[tokio::test]
async fn upcoming_offers_never_notify_and_expired_records_allow_a_fresh_clearance() {
    let mut upcoming = deal("fw:store:ean", DealSource::FoodWaste, "skinke", 5.0);
    upcoming.store.name = Some(json!("Netto"));
    upcoming.store.street = Some(json!("Street"));
    upcoming.valid_to = Some(json!("2026-10-25T22:00:00Z"));
    upcoming.status = Status::Upcoming;
    let item = |d: &Deal| MatchItem {
        name: "Ham".into(),
        rule_id: "ham".into(),
        deals: vec![d.clone()],
    };
    let h = Harness::new(result_of(vec![item(&upcoming)]));
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "no active matches"
    );
    let old =
        json!({"notified": {"ham|fw:store:ean": {"price": 5, "validTo": "2026-10-20T12:00:00Z"}}})
            .to_string();
    std::fs::write(&h.file, &old).unwrap();
    let mut active = upcoming.clone();
    active.status = Status::Active;
    h.set_result(result_of(vec![item(&active)]));
    let watch = h.restart();
    assert_eq!(watch.run_alerts(true).await.unwrap().reason, "dry run");
    assert_eq!(h.file_text(), old);
    assert!(watch.run_alerts(false).await.unwrap().sent);
    assert_eq!(
        h.restart().run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
}

#[tokio::test]
async fn skipped_digests_persist_their_date_across_runs_and_restarts_dry_runs_never_write() {
    let h = Harness::new(result_of(Vec::new()));
    assert_eq!(
        h.watch.run_digest(true).await.unwrap().reason,
        "no active matches"
    );
    assert!(!h.file.exists());
    h.clock.set(parse_date("2026-10-24T06:00:00Z").unwrap());
    h.watch.run_digest(false).await.unwrap();
    assert_eq!(h.calls(), 2);
    h.watch.run_digest(false).await.unwrap();
    assert_eq!(h.calls(), 2);
    assert_eq!(
        h.restart().run_digest(false).await.unwrap().reason,
        "already digested today"
    );
    assert_eq!(h.state()["lastDigestDate"], "2026-10-24");
    assert_eq!(h.state()["notified"], json!({}));

    let unchanged = Harness::with_fixture_result().await;
    assert!(unchanged.watch.run_digest(false).await.unwrap().sent);
    unchanged.clock.set(fixed() + 86_400_000);
    let before = unchanged.file_text();
    assert_eq!(
        unchanged.watch.run_digest(true).await.unwrap().reason,
        "unchanged digest"
    );
    assert_eq!(unchanged.file_text(), before);
    assert_eq!(
        unchanged.watch.run_digest(false).await.unwrap().reason,
        "unchanged digest"
    );
    assert_eq!(unchanged.state()["lastDigestDate"], "2026-10-25");
    let count = unchanged.calls();
    assert_eq!(
        unchanged.watch.run_digest(false).await.unwrap().reason,
        "already digested today"
    );
    assert_eq!(unchanged.calls(), count);
}

#[tokio::test]
async fn failed_digest_leaves_state_untouched_and_retries_only_after_ten_minutes() {
    let h = Harness::with_fixture_result().await;
    h.set_fail(true);
    let initial = json!({"notified": {}, "alertCounts": {}}).to_string();
    std::fs::write(&h.file, &initial).unwrap();
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "send failed"
    );
    assert_eq!(h.file_text(), initial);
    h.clock.set(fixed() + 60_000);
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "digest retry cooldown"
    );
    assert_eq!(h.calls(), 1);
    // A preview neither bypasses delivery safety nor consumes the retry slot.
    assert_eq!(h.watch.run_digest(true).await.unwrap().reason, "dry run");
    h.clock.set(fixed() + 599_999);
    assert_eq!(
        h.watch.run_digest(false).await.unwrap().reason,
        "digest retry cooldown"
    );
    h.clock.set(fixed() + 600_000);
    h.set_fail(false);
    assert!(h.watch.run_digest(false).await.unwrap().sent);
    assert_eq!(h.calls(), 3);
    assert_eq!(h.state()["lastDigestDate"], "2026-10-24");
}

#[tokio::test]
async fn grouped_clearances_retain_notification_identity_across_branch_rotation_and_price_drops() {
    let h = Harness::with_fixture_result().await;
    let base = engine()
        .cheapest("ham", &MatchOptions::default())
        .await
        .items[0]
        .deals
        .iter()
        .find(|d| d.source == DealSource::FoodWaste)
        .unwrap()
        .clone();
    let result = |d: &Deal| {
        result_of(vec![MatchItem {
            name: "Ham".into(),
            rule_id: "ham".into(),
            deals: vec![d.clone()],
        }])
    };
    let mut d = Deal {
        key: "fw:a:ean".into(),
        related_keys: Some(vec!["fw:a:ean".into(), "fw:b:ean".into()]),
        price_dkk: 6.0,
        ..base
    };
    h.set_result(result(&d));
    assert!(h.watch.run_digest(false).await.unwrap().sent);
    assert_eq!(h.state()["notified"]["ham|fw:a:ean"]["price"], 6);
    assert_eq!(h.state()["notified"]["ham|fw:b:ean"]["price"], 6);
    d = Deal {
        key: "fw:b:ean".into(),
        related_keys: Some(vec!["fw:b:ean".into()]),
        price_dkk: 7.0,
        ..d
    };
    h.set_result(result(&d));
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
    // Older persisted state may only contain the previous representative.
    d = Deal {
        key: "fw:c:ean".into(),
        related_keys: Some(vec!["fw:c:ean".into(), "fw:b:ean".into()]),
        ..d
    };
    h.set_result(result(&d));
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
    d.price_dkk = 5.0;
    h.set_result(result(&d));
    assert!(h.watch.run_alerts(false).await.unwrap().sent);
    assert_eq!(h.state()["notified"]["ham|fw:b:ean"]["price"], 5);
    assert_eq!(h.state()["notified"]["ham|fw:c:ean"]["price"], 5);
}

#[tokio::test]
async fn watch_headers_use_home_label_local_date_and_singular_plural_within_limit() {
    let mut h = Harness::with_fixture_result().await;
    let digest = h.watch.run_digest(true).await.unwrap();
    assert_eq!(
        digest.text.split('\n').next().unwrap(),
        "🛒 Deals on your shopping list near home — Sat 24/10"
    );
    assert!(utf16_len(&digest.text) < 1500);
    assert!(digest.text.contains("\nProsciutto crudo:"));
    h.watch.settings_mut().home_label = Some("Risskov".into());
    assert!(h
        .watch
        .run_digest(true)
        .await
        .unwrap()
        .text
        .starts_with("🛒 Deals on your shopping list near Risskov — Sat 24/10\n"));
    let base = engine()
        .cheapest("milk", &MatchOptions::default())
        .await
        .items[0]
        .clone();
    let take = |n: usize| MatchItem {
        deals: base.deals[..n].to_vec(),
        ..base.clone()
    };
    h.set_result(result_of(vec![take(1)]));
    assert!(h
        .watch
        .run_alerts(true)
        .await
        .unwrap()
        .text
        .starts_with("🛒 New deal on your shopping list\n"));
    let ham = MatchItem {
        name: "Ham".into(),
        rule_id: "ham".into(),
        deals: vec![Deal {
            key: "tjek:ham".into(),
            ..base.deals[0].clone()
        }],
    };
    h.set_result(result_of(vec![take(2), ham]));
    let alerts = h.watch.run_alerts(true).await.unwrap();
    assert!(alerts
        .text
        .starts_with("🛒 New deals on your shopping list\n"));
    assert!(utf16_len(&alerts.text) < 1500);
}

fn item_lines(text: &str) -> usize {
    let re = Regex::new(r"^Item \d:").unwrap();
    text.split('\n').filter(|l| re.is_match(l)).count()
}

#[tokio::test]
async fn alerts_send_one_best_deal_per_item_and_defer_the_sixth_without_false_misses() {
    let h = Harness::new(result_of((1..=6).map(notification_item).collect()));
    let items: Vec<MatchItem> = (1..=6).map(notification_item).collect();
    let first = h.watch.run_alerts(false).await.unwrap();
    assert!(first.sent);
    assert_eq!(h.deliver.delivered.lock().unwrap().len(), 1);
    assert_eq!(item_lines(&first.text), 5);
    assert!(!Regex::new("No deal:|Alternative|more item")
        .unwrap()
        .is_match(&first.text));
    assert!(utf16_len(&first.text) <= 1490);
    let state = h.state();
    for item in &items[..5] {
        for d in &item.deals {
            assert_eq!(
                state["notified"][format!("{}|{}", item.rule_id, d.key)]["price"].as_f64(),
                Some(d.price_dkk)
            );
        }
    }
    assert!(state["notified"].get("item_6|tjek:6:best").is_none());
    assert_eq!(state["notified"]["item_6|tjek:6:second"]["price"], 7);
    let second = h.watch.run_alerts(false).await.unwrap();
    assert!(second.sent);
    assert!(second.text.contains("\nItem 6:"));
    assert_eq!(item_lines(&second.text), 1);
    assert!(!Regex::new("No deal:|more item")
        .unwrap()
        .is_match(&second.text));
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
}

#[tokio::test]
async fn known_alternatives_becoming_best_stay_silent_including_omitted_items() {
    let mut items: Vec<MatchItem> = (1..=6).map(notification_item).collect();
    let h = Harness::new(result_of(items.clone()));
    assert!(h.watch.run_alerts(false).await.unwrap().sent);
    for item in &mut items {
        item.deals[0].status = Status::Expired;
    }
    h.set_result(result_of(items));
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "no new or cheaper matches"
    );
    assert_eq!(h.deliver.delivered.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn digest_distinguishes_a_real_miss_from_a_size_trimmed_item() {
    let shown = notification_item(1);
    let mut trimmed = notification_item(2);
    trimmed.deals[0].name = Some(Value::String("Long offer ".repeat(130)));
    let miss = MatchItem {
        name: "Real miss".into(),
        rule_id: "miss".into(),
        deals: Vec::new(),
    };
    let h = Harness::new(result_of(vec![
        shown.clone(),
        miss.clone(),
        trimmed.clone(),
    ]));
    let digest = h.watch.run_digest(false).await.unwrap();
    assert!(digest.sent);
    assert!(digest.text.contains("No deal: Real miss."));
    assert!(Regex::new(r"\+1 more item(?:\n|$)")
        .unwrap()
        .is_match(&digest.text));
    assert!(!Regex::new(r"No deal:.*Item 2|\nItem 2:")
        .unwrap()
        .is_match(&digest.text));
    assert!(utf16_len(&digest.text) <= 1490);
    let state = h.state();
    assert!(state["notified"].get("item_2|tjek:2:best").is_none());
    assert_eq!(state["notified"]["item_2|tjek:2:second"]["price"], 7);
    trimmed.deals[0].name = Some(json!("Short offer"));
    h.set_result(result_of(vec![shown, miss, trimmed]));
    let alert = h.watch.run_alerts(false).await.unwrap();
    assert!(alert.sent);
    assert!(alert.text.contains("\nItem 2:"));
    assert!(!alert.text.contains("No deal:"));
}

#[tokio::test]
async fn app_delivery_splits_shopping_title_and_body_with_a_digest_dedup_key() {
    let mut h = Harness::with_fixture_result().await;
    h.watch.settings_mut().home_label = Some("Risskov".into());
    let result = h.watch.run_digest(false).await.unwrap();
    assert!(result.sent);
    let sent = h.deliver.delivered.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let (title, body) = result.text.split_once('\n').unwrap();
    assert_eq!(
        sent[0].title,
        "🛒 Deals on your shopping list near Risskov — Sat 24/10"
    );
    assert_eq!(sent[0].title, title);
    assert_eq!(sent[0].body, body);
    assert!(!sent[0].body.contains('🛒'));
    assert!(
        Regex::new(r"^grocery-deals:digest:2026-10-24:[a-f0-9]{24}$")
            .unwrap()
            .is_match(&sent[0].dedup_key)
    );
    assert_eq!(h.state()["lastDigestDate"], "2026-10-24");
}

#[tokio::test]
async fn failed_app_sends_preserve_state_and_dedup_keys_follow_delivered_offers() {
    let mut items: Vec<MatchItem> = (1..=6).map(notification_item).collect();
    let h = Harness::new(result_of(items.clone()));
    h.set_fail(true);
    let initial = json!({"notified": {"expired": {"price": 1, "validTo": "2026-10-01T00:00:00Z"}}, "alertCounts": {}}).to_string();
    std::fs::write(&h.file, &initial).unwrap();
    assert_eq!(h.watch.run_alerts(true).await.unwrap().reason, "dry run");
    assert!(h.deliver.attempts.lock().unwrap().is_empty());
    assert_eq!(
        h.watch.run_alerts(false).await.unwrap().reason,
        "send failed"
    );
    assert_eq!(h.file_text(), initial);
    // An undelivered offer's price must not alter the dedup identity.
    items[5].deals[0].price_dkk = 4.0;
    h.set_result(result_of(items.clone()));
    assert!(!h.watch.run_alerts(false).await.unwrap().sent);
    let attempts = || h.deliver.attempts.lock().unwrap().clone();
    assert_eq!(attempts()[0].dedup_key, attempts()[1].dedup_key);
    items[0].deals[0].price_dkk = 3.0;
    h.set_result(result_of(items.clone()));
    h.set_fail(false);
    assert!(h.watch.run_alerts(false).await.unwrap().sent);
    assert_ne!(attempts()[1].dedup_key, attempts()[2].dedup_key);
    assert!(Regex::new(r"^grocery-deals:alert:2026-10-24:[a-f0-9]{24}$")
        .unwrap()
        .is_match(&attempts()[2].dedup_key));
    assert!(h.watch.run_alerts(false).await.unwrap().sent);
    assert_ne!(attempts()[2].dedup_key, attempts()[3].dedup_key);
    assert_eq!(attempts()[3].title, "🛒 New deal on your shopping list");
    assert!(attempts()[3].body.starts_with("Item 6:"));
}

#[tokio::test]
async fn unconfigured_app_delivery_does_not_persist() {
    let h = Harness::with_fixture_result().await;
    h.deliver
        .unconfigured
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = h.watch.run_digest(false).await.unwrap();
    assert!(!outcome.sent);
    assert_eq!(outcome.reason, "app notifications not configured");
    assert!(outcome.text.is_empty());
    assert!(!h.file.exists());
}

#[test]
fn watch_state_round_trips_node_files_byte_for_byte() {
    use lucina_deals::WatchState;
    let node = "{\n  \"notified\": {\n    \"ham|fw:a:ean\": {\n      \"price\": 6,\n      \"validTo\": \"2026-10-24T21:59:59.000Z\"\n    },\n    \"milk|tjek:x\": {\n      \"price\": 8.99\n    }\n  },\n  \"lastDigestDate\": \"2026-10-24\",\n  \"digestFingerprint\": \"[[\\\"fw:a:ean\\\",6]]\",\n  \"alertCounts\": {\n    \"2026-10-24\": 1\n  },\n  \"extra\": [\n    1,\n    {}\n  ]\n}";
    let state = WatchState::parse(node).unwrap();
    assert_eq!(state.to_json(), node);
    assert_eq!(state.notified_price("ham|fw:a:ean"), Some(6.0));
    assert_eq!(state.last_digest_date(), Some("2026-10-24"));
    // `{ ...empty(), ...file }`: the four keys first, in Node's order.
    let partial = WatchState::parse("{\"alertCounts\":{},\"notified\":{}}").unwrap();
    assert_eq!(
        partial.to_json(),
        "{\n  \"notified\": {},\n  \"lastDigestDate\": null,\n  \"digestFingerprint\": null,\n  \"alertCounts\": {}\n}"
    );
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested/deal-watch.json");
    state.save(&file).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), node);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
