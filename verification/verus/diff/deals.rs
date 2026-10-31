//! Differential tests of the Verus deals kernel (`verification/verus/deals_kernel.rs`)
//! against the real lucina-deals code at the pinned revision (`deals-revision`), plus
//! real-code witnesses of the DealWatch model findings (docs/verification.md §14).
//!
//! `run-deals.sh` extracts the pinned crates with `git archive` into `target/deals-diff`,
//! installs this file as `crates/lucina-deals/tests/verus_deals.rs` and the plain-Rust
//! kernel copy as `tests/verus/deals_kernel_exec.rs`, and runs them. Inputs are generated
//! by a fixed-seed xorshift, so a failure is reproducible.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lucina_deals::js::{display, iso_string, parse_date};
use lucina_deals::matcher::{match_deal, term_score, tokens, GLOBAL_EXCLUDES};
use lucina_deals::sources::{DealStore, Sources};
use lucina_deals::watch::{acknowledge, deal_keys, Prepared};
use lucina_deals::{
    compose, fixed_clock, validity, Composed, Deal, DealSource, DealWatch, DealsConfig, Deliver,
    Delivery, FixtureHttp, FnHttp, MatchItem, MatchResult, MatchSource, Notification, Rule, Status,
    WatchKind, WatchSettings, WatchState,
};
use regex::Regex;
use serde_json::{json, Value};

#[allow(dead_code, clippy::all)]
mod kernel {
    include!("verus/deals_kernel_exec.rs");
}

/// xorshift64*, fixed seeds.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len() as u64) as usize]
    }
}

/// `2026-10-24T09:30:00Z`, the fixtures' recording time.
fn now() -> i64 {
    parse_date("2026-10-24T09:30:00Z").unwrap()
}

const DAY: i64 = 86_400_000;

fn deal(key: &str, name: &str, price: f64) -> Deal {
    Deal {
        key: key.into(),
        source: DealSource::Flyer,
        name: Some(json!(name)),
        chain: Some(json!("Netto")),
        chain_key: "netto".into(),
        dealer_id: None,
        store: DealStore {
            id: Some(json!("s")),
            name: Some(json!("Netto")),
            street: None,
            lat: None,
            lng: None,
            distance_m: 100.0,
        },
        price_dkk: price,
        normal_price_dkk: None,
        discount_pct: None,
        stock: None,
        stock_unit: None,
        valid_from: Some(json!(iso_string(now() - DAY))),
        valid_to: Some(json!(iso_string(now() + 3 * DAY))),
        ean: None,
        category: None,
        unit_price: None,
        status: Status::Active,
        score: 1000,
        related_keys: None,
        more_stores: None,
    }
}

fn item(name: &str, deals: Vec<Deal>) -> MatchItem {
    MatchItem {
        name: name.into(),
        rule_id: name.to_lowercase(),
        deals,
    }
}

fn result(items: Vec<MatchItem>) -> MatchResult {
    MatchResult {
        items,
        failures: vec![],
        now: iso_string(now()),
    }
}

fn ready(c: Composed) -> Box<Prepared> {
    match c {
        Composed::Ready(p) => p,
        other => panic!("expected a notification, got {other:?}"),
    }
}

fn settings() -> WatchSettings {
    WatchSettings::default()
}

// ------------------------------------------------------------------ (a) validity
#[test]
fn validity_refines_the_kernel() {
    let t = now();
    let edges = [
        -10 * DAY,
        -3 * DAY - 1,
        -3 * DAY,
        -DAY,
        -1,
        0,
        1,
        DAY,
        3 * DAY - 1,
        3 * DAY,
        3 * DAY + 1,
        10 * DAY,
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut n = 0;
    for _ in 0..20_000 {
        let from = t + if rng.below(2) == 0 {
            *rng.pick(&edges)
        } else {
            rng.below(20 * DAY as u64) as i64 - 10 * DAY
        };
        let to = if rng.below(3) == 0 {
            from + *rng.pick(&edges)
        } else {
            t + rng.below(20 * DAY as u64) as i64 - 10 * DAY
        };
        let real = validity(Some(&iso_string(from)), Some(&iso_string(to)), t);
        let k = kernel::validity_exec(from, to, t);
        assert_eq!(code(real), k, "from {from} to {to} now {t}");
        n += 1;
    }
    for (f, to) in [(None, Some(t)), (Some(t), None), (None, None)] {
        let real = validity(
            f.map(iso_string).as_deref(),
            to.map(iso_string).as_deref(),
            t,
        );
        assert_eq!(code(real), kernel::validity_opt_exec(f, to, t));
    }
    assert_eq!(
        code(validity(Some("not a date"), Some(&iso_string(t + DAY)), t)),
        kernel::validity_opt_exec(None, Some(t + DAY), t)
    );
    assert!(n == 20_000);
}

fn code(s: Status) -> u8 {
    match s {
        Status::Active => 0,
        Status::Upcoming => 1,
        Status::Expired => 2,
    }
}

// ------------------------------------------------------------------ (b) keys
fn fw_row(store: &Value, ean: &Value) -> Value {
    json!([{
        "store": {"id": store, "coordinates": [10.0, 56.0], "brand": "netto", "name": "Netto"},
        "clearances": [{"offer": {"ean": ean, "newPrice": 10, "stock": 1}, "product": {"description": "mælk"}}]
    }])
}

async fn food_waste_key(store: Value, ean: Value) -> String {
    let rows = fw_row(&store, &ean);
    let cfg = DealsConfig {
        home_lat: Some(56.0),
        home_lng: Some(10.0),
        salling_api_key: Some("test".into()),
        ..DealsConfig::default()
    };
    let http = FnHttp(move |_: &reqwest::Url| Ok(rows.clone()));
    let sources = Sources::new(Arc::new(cfg), Arc::new(http), fixed_clock(now()));
    let deals = sources.food_waste(3000.0).await.unwrap();
    assert_eq!(deals.len(), 1);
    deals[0].key.clone()
}

#[tokio::test]
async fn food_waste_keys_are_the_kernel_bytes() {
    let mut rng = Rng(7);
    let alphabet: Vec<char> = "abcdef0123456789-æø".chars().collect();
    let mut seen: HashMap<String, (String, String)> = HashMap::new();
    for _ in 0..400 {
        let store: String = (0..1 + rng.below(8))
            .map(|_| *rng.pick(&alphabet))
            .collect();
        let ean = if rng.below(4) == 0 {
            json!(rng.below(100_000))
        } else {
            json!(format!("57{}", rng.below(100_000)))
        };
        let key = food_waste_key(json!(store), ean.clone()).await;
        let expected = kernel::wire_exec(
            true,
            &store.as_bytes().to_vec(),
            &display(Some(&ean)).into_bytes(),
        );
        assert_eq!(key.as_bytes(), &expected[..]);
        // Colon-free store ids: distinct (store, ean) never share a key.
        let id = (store.clone(), display(Some(&ean)));
        if let Some(old) = seen.insert(key.clone(), id.clone()) {
            assert_eq!(old, id, "two identities share {key}");
        }
    }
    // The precondition matters: with ':' in a store id the raw join collides
    // (lemma_wire_collision).
    let a = food_waste_key(json!("a:b"), json!("c")).await;
    let b = food_waste_key(json!("a"), json!("b:c")).await;
    assert_eq!(a, b);
    assert_eq!(
        a.as_bytes(),
        &kernel::wire_exec(true, &b"a".to_vec(), &b"b:c".to_vec())[..]
    );
}

#[tokio::test]
async fn flyer_keys_are_the_kernel_bytes() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/grocery-deals");
    let cfg = DealsConfig {
        home_lat: Some(56.19633),
        home_lng: Some(10.20110),
        salling_api_key: Some("fixture".into()),
        ..DealsConfig::default()
    };
    let terms: Vec<String> = [
        "mælk",
        "parmaskinke",
        "rugbrød",
        "skinke",
        "skyr",
        "yoghurt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut ids = HashSet::new();
    for t in &terms {
        let text = std::fs::read_to_string(dir.join(format!("tjek-offers-{t}.json"))).unwrap();
        let rows: Value = serde_json::from_str(&text).unwrap();
        for o in rows.as_array().unwrap() {
            ids.insert(display(o.get("id")));
        }
    }
    let sources = Sources::new(
        Arc::new(cfg),
        Arc::new(FixtureHttp::new(dir)),
        fixed_clock(now()),
    );
    let flyers = sources.flyers(&terms, 3000.0).await.unwrap();
    assert!(flyers.deals.len() > 10, "fixtures give offers");
    for d in &flyers.deals {
        let id = d.key.strip_prefix("tjek:").expect("flyer key prefix");
        assert!(ids.contains(id));
        assert_eq!(
            d.key.as_bytes(),
            &kernel::wire_exec(false, &id.as_bytes().to_vec(), &Vec::new())[..]
        );
        assert!(!d.key.starts_with("fw:"));
    }
}

#[test]
fn deal_keys_refines_the_kernel() {
    let mut rng = Rng(11);
    for _ in 0..5_000 {
        let key = rng.below(6);
        let related: Vec<u64> = (0..rng.below(6)).map(|_| rng.below(6)).collect();
        let mut d = deal(&format!("k{key}"), "x", 1.0);
        d.related_keys = if related.is_empty() && rng.below(2) == 0 {
            None
        } else {
            Some(related.iter().map(|k| format!("k{k}")).collect())
        };
        let real: Vec<u64> = deal_keys(&d)
            .iter()
            .map(|k| k[1..].parse().unwrap())
            .collect();
        assert_eq!(real, kernel::deal_keys_exec(key, &related));
    }
}

// ------------------------------------------------------------------ (c) new / acknowledge
/// The notified price of `rule|key` as the kernel sees it (prices are integers here).
fn olds(state: &WatchState, rule: &str, d: &Deal) -> Vec<Option<i64>> {
    deal_keys(d)
        .iter()
        .map(|k| {
            state
                .notified_price(&format!("{rule}|{k}"))
                .map(|p| p as i64)
        })
        .collect()
}

#[test]
fn alert_selection_and_acknowledgment_refine_the_kernel() {
    let mut rng = Rng(23);
    let names = ["Vare0", "Vare1", "Vare2", "Vare3"];
    let mut ready_runs = 0;
    for case in 0..3_000 {
        let mut state = WatchState::empty();
        let mut items = Vec::new();
        for (i, name) in names.iter().enumerate().take(1 + rng.below(4) as usize) {
            let rule = name.to_lowercase();
            let mut deals = Vec::new();
            for a in 0..1 + rng.below(3) {
                let key = format!("fw:s{a}:{i}{case}");
                let mut d = deal(&key, name, (1 + rng.below(6)) as f64 + a as f64);
                if rng.below(2) == 0 {
                    d.related_keys = Some(vec![key.clone(), format!("fw:t{a}:{i}{case}")]);
                }
                for k in deal_keys(&d) {
                    if rng.below(3) == 0 {
                        state.set_notified(&format!("{rule}|{k}"), (1 + rng.below(8)) as f64, None);
                    }
                }
                deals.push(d);
            }
            items.push(item(name, deals));
        }
        let r = result(items.clone());
        let all_olds: Vec<Vec<Option<i64>>> = items
            .iter()
            .map(|it| olds(&state, &it.rule_id, &it.deals[0]))
            .collect();
        let prices: Vec<i64> = items
            .iter()
            .map(|it| it.deals[0].price_dkk as i64)
            .collect();
        let selected = kernel::select_exec(&all_olds, &prices);
        match compose(WatchKind::Alerts, &state, &r, now(), &settings()) {
            Composed::Ready(p) => {
                ready_runs += 1;
                for (it, sel) in items.iter().zip(&selected) {
                    assert_eq!(
                        p.notification.body.contains(&format!("{}: ", it.name)),
                        *sel,
                        "case {case}: {}",
                        p.notification.body
                    );
                }
                // acknowledge = spec `ack` of every active deal's keys (overwrite), in order.
                let mut reference: HashMap<String, f64> = HashMap::new();
                for it in &items {
                    for d in &it.deals {
                        for k in deal_keys(d) {
                            reference.insert(format!("{}|{k}", it.rule_id), d.price_dkk);
                        }
                    }
                }
                acknowledge(&mut state, &p);
                for (id, price) in &reference {
                    assert_eq!(state.notified_price(id), Some(*price));
                }
                // Idempotence and strict cheaper (lemma_ack_idempotent / _strict_cheaper_is_new).
                for it in &items {
                    for d in &it.deals {
                        for (q, new) in [
                            (d.price_dkk, false),
                            (d.price_dkk + 1.0, false),
                            (d.price_dkk - 0.5, true),
                        ] {
                            let mut again = d.clone();
                            again.price_dkk = q;
                            let single = result(vec![item(&it.name, vec![again])]);
                            let c = compose(WatchKind::Alerts, &state, &single, now(), &settings());
                            assert_eq!(matches!(c, Composed::Ready(_)), new);
                        }
                    }
                }
            }
            Composed::Skip(reason) => {
                assert!(
                    selected.iter().all(|s| !s),
                    "case {case}: skipped ({reason}) with new offers"
                );
                assert_eq!(reason, "no new or cheaper matches");
            }
            Composed::TooLong => panic!("short bodies"),
        }
    }
    assert!(
        ready_runs > 500 && ready_runs < 3_000,
        "both outcomes: {ready_runs}"
    );
}

// ------------------------------------------------------------------ (d) matcher
fn letter_re() -> Regex {
    Regex::new(r"^\p{L}$").unwrap()
}

fn kernel_words(text: &str, re: &Regex) -> (Vec<Vec<char>>, Vec<Vec<bool>>) {
    let words: Vec<Vec<char>> = tokens(text).iter().map(|w| w.chars().collect()).collect();
    let flags = words
        .iter()
        .map(|w| w.iter().map(|c| re.is_match(&c.to_string())).collect())
        .collect();
    (words, flags)
}

fn kernel_term(term: &str) -> Vec<Vec<char>> {
    tokens(term).iter().map(|w| w.chars().collect()).collect()
}

const FRAGMENTS: &[&str] = &[
    "skinke",
    "pizza",
    "rug",
    "brød",
    "rugbrød",
    "mælk",
    "chokolade",
    "æg",
    "pålæg",
    "yoghurt",
    "yogurt",
    "ost",
    "skinkeost",
    "parma",
    "røget",
    "kyllinge",
    "12",
    "x",
    "Æble",
    "mælke",
    "schnitzler",
    "chips",
    "Kærgården",
    "smør",
    "café",
    "øko",
    "Ⅷ",
    "ß",
    "a",
    "ÆØÅ",
];
const SEPARATORS: &[&str] = &["", "", " ", " ", "-", ", ", "/", "2", " 500 g "];

fn heading(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..1 + rng.below(4) {
        s.push_str(rng.pick(SEPARATORS));
        s.push_str(rng.pick(FRAGMENTS));
    }
    s
}

fn term(rng: &mut Rng) -> String {
    match rng.below(5) {
        0 => format!("{} {}", rng.pick(FRAGMENTS), rng.pick(FRAGMENTS)),
        _ => rng.pick(FRAGMENTS).to_string(),
    }
}

#[test]
fn term_score_refines_the_kernel() {
    let re = letter_re();
    let mut rng = Rng(31);
    let (mut hits, mut heads) = (0, 0);
    for _ in 0..60_000 {
        let text = heading(&mut rng);
        let t = term(&mut rng);
        let (w, f) = kernel_words(&text, &re);
        let k = kernel::term_score_exec(&w, &f, &kernel_term(&t));
        assert_eq!(term_score(&text, &t), k, "{text:?} vs {t:?}");
        hits += usize::from(k >= 600);
        heads += usize::from((1..=500).contains(&k));
    }
    assert!(
        hits > 1_000 && heads > 500,
        "phrase {hits}, head-final {heads}"
    );
}

#[test]
fn owner_examples_never_match() {
    let re = letter_re();
    for (text, t) in [
        ("skinkepizza", "skinke"),
        ("skinkeschnitzler", "skinke"),
        ("rugbrødschips", "rugbrød"),
        ("mælkechokolade", "mælk"),
    ] {
        assert_eq!(term_score(text, t), 0);
        let (w, f) = kernel_words(text, &re);
        assert_eq!(kernel::term_score_exec(&w, &f, &kernel_term(t)), 0);
    }
    // Head-final compounds do match, and global excludes veto (lemma_pizza_is_vetoed).
    assert!(term_score("parmaskinke", "skinke") > 0);
    let rule = Rule {
        id: "skinke".into(),
        label: None,
        names: vec![],
        terms: vec!["skinke".into(), "pizza".into()],
        exclude: vec![],
        categories: None,
    };
    assert_eq!(match_deal(&rule, &deal("tjek:1", "Skinkepizza", 30.0)), 0);
    assert!(match_deal(&rule, &deal("tjek:1", "Parmaskinke", 30.0)) > 0);
}

#[test]
fn match_deal_refines_the_kernel() {
    let re = letter_re();
    let mut rng = Rng(43);
    let (mut vetoed, mut matched) = (0, 0);
    for _ in 0..20_000 {
        let rule = Rule {
            id: "r".into(),
            label: None,
            names: vec![],
            terms: (0..rng.below(3)).map(|_| term(&mut rng)).collect(),
            exclude: (0..rng.below(2)).map(|_| term(&mut rng)).collect(),
            categories: match rng.below(3) {
                0 => None,
                1 => Some(vec![]),
                _ => Some(vec![rng.pick(&["Mejeri", "Pålæg", "brød"]).to_string()]),
            },
        };
        let mut d = deal("fw:s:1", &heading(&mut rng), 10.0);
        if rng.below(2) == 0 {
            d.source = DealSource::FoodWaste;
            d.category = Some(
                rng.pick(&["Mejeri & køl", "Pålæg", "Brød og kager", ""])
                    .to_string(),
            );
        }
        // category_ok: the code's food-waste category filter (normalized substring).
        let category_ok = d.source != DealSource::FoodWaste
            || rule.categories.as_ref().is_none_or(|c| c.is_empty())
            || rule.categories.as_ref().unwrap().iter().any(|c| {
                lucina_deals::matcher::normalize(d.category.as_deref().unwrap_or(""))
                    .contains(&lucina_deals::matcher::normalize(c))
            });
        let excludes: Vec<Vec<Vec<char>>> = GLOBAL_EXCLUDES
            .iter()
            .copied()
            .chain(rule.exclude.iter().map(String::as_str))
            .map(kernel_term)
            .collect();
        let terms: Vec<Vec<Vec<char>>> = rule.terms.iter().map(|t| kernel_term(t)).collect();
        let (w, f) = kernel_words(&d.name_text(), &re);
        let k = kernel::match_deal_exec(&w, &f, &excludes, category_ok, &terms);
        assert_eq!(match_deal(&rule, &d), k, "{:?} / {rule:?}", d.name_text());
        let any_term = terms
            .iter()
            .any(|t| kernel::term_score_exec(&w, &f, t) != 0);
        vetoed += usize::from(k == 0 && any_term);
        matched += usize::from(k != 0);
    }
    assert!(
        vetoed > 300 && matched > 300,
        "vetoed {vetoed}, matched {matched}"
    );
}

// ------------------------------------------------------------------ model witnesses
/// The model's dedup key is (kind, date, delivered best offers): the same batch gives the
/// same key (so the server suppresses a retry), alternatives do not change it, and a new
/// date or a changed batch gives a new key.
#[test]
fn dedup_key_is_kind_date_and_delivered_offers() {
    let state = WatchState::empty();
    let s = settings();
    let one = result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]);
    let with_alt = result(vec![item(
        "Mælk",
        vec![deal("tjek:1", "mælk", 10.0), deal("tjek:2", "mælk", 12.0)],
    )]);
    for kind in [WatchKind::Digest, WatchKind::Alerts] {
        let a = ready(compose(kind, &state, &one, now(), &s));
        let b = ready(compose(kind, &state, &one, now(), &s));
        let alt = ready(compose(kind, &state, &with_alt, now(), &s));
        let tomorrow = ready(compose(kind, &state, &one, now() + DAY, &s));
        assert_eq!(a.notification.dedup_key, b.notification.dedup_key);
        assert_eq!(a.notification.dedup_key, alt.notification.dedup_key);
        assert_ne!(a.notification.dedup_key, tomorrow.notification.dedup_key);
    }
    let a = ready(compose(WatchKind::Alerts, &state, &one, now(), &s));
    let d = ready(compose(WatchKind::Digest, &state, &one, now(), &s));
    assert_ne!(a.notification.dedup_key, d.notification.dedup_key);
}

/// D-a (send/save gap): a run delivers, dies before saving; the retry sees one more
/// offer, so its key is new and the owner gets the first offer again.
#[test]
fn retry_after_unsaved_send_repeats_with_a_new_key() {
    let state = WatchState::empty();
    let s = settings();
    let first = result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]);
    let retry = result(vec![
        item("Mælk", vec![deal("tjek:1", "mælk", 10.0)]),
        item("Brød", vec![deal("tjek:2", "brød", 12.0)]),
    ]);
    for kind in [WatchKind::Digest, WatchKind::Alerts] {
        let a = ready(compose(kind, &state, &first, now(), &s));
        let b = ready(compose(kind, &state, &retry, now(), &s));
        assert_ne!(a.notification.dedup_key, b.notification.dedup_key);
        assert!(a.notification.body.contains("Mælk: ") && b.notification.body.contains("Mælk: "));
    }
}

/// D-b (price bounce): notified at 10, a later delivered run acknowledges it at 12,
/// then the offer at 10 is alerted again (lemma_overwrite_forgets_cheaper).
#[test]
fn price_bounce_alerts_a_received_price_again() {
    let mut state = WatchState::empty();
    let s = settings();
    let p = ready(compose(
        WatchKind::Alerts,
        &state,
        &result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]),
        now(),
        &s,
    ));
    acknowledge(&mut state, &p);
    let up = result(vec![
        item("Mælk", vec![deal("tjek:1", "mælk", 12.0)]),
        item("Brød", vec![deal("tjek:2", "brød", 12.0)]),
    ]);
    let p = ready(compose(WatchKind::Alerts, &state, &up, now(), &s));
    assert!(!p.notification.body.contains("Mælk: "));
    acknowledge(&mut state, &p);
    assert_eq!(state.notified_price("mælk|tjek:1"), Some(12.0));
    let again = ready(compose(
        WatchKind::Alerts,
        &state,
        &result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]),
        now(),
        &s,
    ));
    assert!(again.notification.body.contains("Mælk: "));
}

/// D-c (alternatives): an alert shows the best offer only but acknowledges the
/// alternative too; when the best goes away the alternative is never alerted.
#[test]
fn alternative_acknowledged_without_delivery() {
    let mut state = WatchState::empty();
    let s = settings();
    let p = ready(compose(
        WatchKind::Alerts,
        &state,
        &result(vec![item(
            "Mælk",
            vec![deal("tjek:1", "mælk", 10.0), deal("tjek:2", "mælk", 12.0)],
        )]),
        now(),
        &s,
    ));
    assert!(!p.notification.body.contains("12"));
    acknowledge(&mut state, &p);
    assert_eq!(state.notified_price("mælk|tjek:2"), Some(12.0));
    let later = compose(
        WatchKind::Alerts,
        &state,
        &result(vec![item("Mælk", vec![deal("tjek:2", "mælk", 12.0)])]),
        now(),
        &s,
    );
    assert_eq!(later, Composed::Skip("no new or cheaper matches"));
}

/// The digest is a summary: it lists acknowledged offers (only its fingerprint skips it).
#[test]
fn digest_relists_alerted_offers() {
    let mut state = WatchState::empty();
    let s = settings();
    let r = result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]);
    let p = ready(compose(WatchKind::Alerts, &state, &r, now(), &s));
    acknowledge(&mut state, &p);
    let d = ready(compose(WatchKind::Digest, &state, &r, now(), &s));
    assert!(d.notification.body.contains("Mælk: "));
    acknowledge(&mut state, &d);
    assert_eq!(
        compose(WatchKind::Digest, &state, &r, now() + DAY, &s),
        Composed::Skip("unchanged digest")
    );
}

struct Matches(MatchResult);

#[async_trait::async_trait]
impl MatchSource for Matches {
    async fn list_matches(&self, _: &str) -> Result<MatchResult, lucina_deals::DealsError> {
        Ok(self.0.clone())
    }
}

struct Outcome(Delivery);

#[async_trait::async_trait]
impl Deliver for Outcome {
    async fn deliver(&self, _: &Notification) -> Delivery {
        self.0.clone()
    }
}

/// A failed send (also an admitted send whose response was lost) writes nothing; the
/// daily cap stops a run before polling.
#[tokio::test]
async fn failed_send_writes_nothing_and_the_cap_stops_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deal-watch.json");
    let s = WatchSettings {
        state_path: path.clone(),
        ..settings()
    };
    let r = result(vec![item("Mælk", vec![deal("tjek:1", "mælk", 10.0)])]);
    let failing = DealWatch::new(
        s.clone(),
        Arc::new(Matches(r.clone())),
        Arc::new(Outcome(Delivery::Failed {
            reason: "send failed".into(),
        })),
        "owner",
    )
    .with_clock(fixed_clock(now()));
    assert_eq!(
        failing.run_alerts(false).await.unwrap().reason,
        "send failed"
    );
    assert!(!path.exists());
    let ok = DealWatch::new(
        s.clone(),
        Arc::new(Matches(r.clone())),
        Arc::new(Outcome(Delivery::Delivered)),
        "owner",
    )
    .with_clock(fixed_clock(now()));
    assert!(ok.run_alerts(false).await.unwrap().sent);
    let state = WatchState::load(&path).unwrap();
    assert_eq!(state.notified_price("mælk|tjek:1"), Some(10.0));
    let date = lucina_deals::format::local_date(now(), s.zone);
    std::fs::write(&path, json!({"alertCounts": {date: 4}}).to_string()).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        ok.run_alerts(false).await.unwrap().reason,
        "daily alert cap"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
