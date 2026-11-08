//! Deterministic, read-only deal queries shared by every channel and the
//! watcher (`engine.js`): shopping-list matches, the cheapest offers for one
//! product, healthy lunch combinations and the nearby-stores map.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::js::{self, cmp_num, display, locale_compare, parse_date, truthy};
use crate::matcher::{match_deal, term_score, tokens};
use crate::sources::{
    haversine, id_key, Branch, BranchSource, Deal, DealSource, DealStore, DealsHttp, Sources,
};
use crate::synonyms::{resolve_rule, Rule, Synonyms};
use crate::validity::{validity, Status};
use crate::{system_clock, Clock, DealsConfig, DealsError};

/// One shopping-list row, as the Node repository's `shoppingList` gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShoppingRow {
    pub name: Option<String>,
    pub raw_name: Option<String>,
    pub product: Option<String>,
    pub status: String,
}

impl ShoppingRow {
    pub fn needed(name: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            status: "needed".into(),
            ..Self::default()
        }
    }

    /// The non-empty names in Node's candidate order.
    fn candidates(&self) -> Vec<String> {
        [&self.name, &self.raw_name, &self.product]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect()
    }
}

/// Where the engine reads the owner's shopping list.
#[async_trait]
pub trait ShoppingSource: Send + Sync {
    async fn shopping_list(&self, user_id: &str) -> Result<Vec<ShoppingRow>, DealsError>;
}

/// A fixed list (CLI `--items`, tests).
#[derive(Debug, Clone, Default)]
pub struct FixedList(pub Vec<ShoppingRow>);

impl FixedList {
    pub fn needed(names: &[&str]) -> Self {
        Self(names.iter().map(|n| ShoppingRow::needed(n)).collect())
    }
}

#[async_trait]
impl ShoppingSource for FixedList {
    async fn shopping_list(&self, _user_id: &str) -> Result<Vec<ShoppingRow>, DealsError> {
        Ok(self.0.clone())
    }
}

#[async_trait]
impl ShoppingSource for lucina_grocery::GroceryRepo {
    async fn shopping_list(&self, user_id: &str) -> Result<Vec<ShoppingRow>, DealsError> {
        let rows = self
            .legacy_list(user_id)
            .await
            .map_err(|e| DealsError::Shopping(e.to_string()))?;
        let text = |v: &Value| v.as_str().map(str::to_owned);
        Ok(rows
            .iter()
            .map(|r| ShoppingRow {
                name: text(&r["name"]),
                raw_name: text(&r["rawName"]),
                product: text(&r["product"]),
                status: r["status"].as_str().unwrap_or("").to_owned(),
            })
            .collect())
    }
}

/// Query options (`radiusM`, `chainKey`, `limit` on Node).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchOptions {
    pub radius_m: Option<f64>,
    pub chain_key: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchItem {
    /// The list's own name for the item (the first non-empty row name).
    pub name: String,
    pub rule_id: String,
    pub deals: Vec<Deal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    pub items: Vec<MatchItem>,
    /// `food_waste` and/or `flyer` when that source failed.
    pub failures: Vec<String>,
    /// ISO time of the query.
    pub now: String,
}

impl MatchResult {
    pub fn to_json(&self) -> Value {
        json!({
            "items": self.items.iter().map(|i| json!({
                "name": i.name,
                "ruleId": i.rule_id,
                "deals": i.deals.iter().map(Deal::to_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "failures": self.failures,
            "now": self.now,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LunchItem {
    pub deal: Deal,
    pub protein: bool,
    pub fibre: bool,
    pub veg: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LunchSuggestion {
    pub items: Vec<LunchItem>,
    pub total: f64,
    pub saving: f64,
    pub store: DealStore,
    pub valid_to: Option<Value>,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LunchResult {
    pub suggestions: Vec<LunchSuggestion>,
    pub budget: f64,
    pub failures: Vec<String>,
    pub now: String,
}

impl LunchResult {
    pub fn to_json(&self) -> Value {
        json!({
            "suggestions": self.suggestions.iter().map(|s| {
                let mut v = json!({
                    "items": s.items.iter().map(|i| {
                        let mut d = i.deal.to_json();
                        d["protein"] = json!(i.protein);
                        d["fibre"] = json!(i.fibre);
                        d["veg"] = json!(i.veg);
                        d
                    }).collect::<Vec<_>>(),
                    "total": js::num_value(s.total),
                    "saving": js::num_value(s.saving),
                    "store": s.store.to_json(),
                    "rank": js::num_value(s.rank),
                });
                if let Some(t) = &s.valid_to {
                    v["validTo"] = t.clone();
                }
                v
            }).collect::<Vec<_>>(),
            "budget": js::num_value(self.budget),
            "failures": self.failures,
            "now": self.now,
        })
    }
}

/// A shopping-list item an offer at this store serves.
#[derive(Debug, Clone, PartialEq)]
pub struct ListMatch {
    pub item: String,
    pub rule_id: String,
    pub deal: Deal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapStore {
    pub id: String,
    pub chain: Option<Value>,
    pub chain_key: String,
    pub name: Option<Value>,
    pub street: Option<String>,
    pub lat: f64,
    pub lng: f64,
    /// Rounded metres.
    pub distance_m: i64,
    pub list_matches: Vec<ListMatch>,
    pub clearances: Vec<Deal>,
    pub flyer_offers: Vec<Deal>,
}

/// `GET /api/secretary/grocery/deals/map` data.
#[derive(Debug, Clone, PartialEq)]
pub struct MapData {
    pub generated_at: String,
    pub home_lat: Option<f64>,
    pub home_lng: Option<f64>,
    pub home_label: Option<String>,
    pub radius_m: f64,
    pub failures: Vec<String>,
    pub stores: Vec<MapStore>,
}

impl MapData {
    /// The contract JSON (`docs/secretary-api.md`, deals map).
    pub fn to_json(&self) -> Value {
        let public = |d: &Vec<Deal>| d.iter().map(Deal::public_json).collect::<Vec<_>>();
        json!({
            "generatedAt": self.generated_at,
            "home": {
                "lat": js::opt_num_value(self.home_lat),
                "lng": js::opt_num_value(self.home_lng),
                "label": self.home_label,
            },
            "radiusM": js::num_value(self.radius_m),
            "failures": self.failures,
            "stores": self.stores.iter().map(|s| {
                let mut v = json!({
                    "id": s.id,
                    "chainKey": s.chain_key,
                    "street": s.street,
                    "lat": js::num_value(s.lat),
                    "lng": js::num_value(s.lng),
                    "distanceM": s.distance_m,
                    "listMatches": s.list_matches.iter().map(|m| json!({
                        "item": m.item,
                        "ruleId": m.rule_id,
                        "deal": m.deal.public_json(),
                    })).collect::<Vec<_>>(),
                    "clearances": public(&s.clearances),
                    "flyerOffers": public(&s.flyer_offers),
                });
                if let Some(c) = &s.chain {
                    v["chain"] = c.clone();
                }
                if let Some(n) = &s.name {
                    v["name"] = n.clone();
                }
                v
            }).collect::<Vec<_>>(),
        })
    }
}

const LUNCH_TERMS: &[&str] = &[
    "rugbrød", "skyr", "salat", "kylling", "laks", "tun", "æg", "hummus", "frugt", "grønt",
];
const VETO: &[&str] = &[
    "slik",
    "chips",
    "sodavand",
    "kage",
    "chokolade",
    "pizza",
    "is",
    "wienerbrød",
    "energidrik",
    "øl",
    "vin",
    "bacon",
    "pølse",
    "burger",
    "russisk salat",
    "italiensk salat",
    "pålægssalater",
    "juice",
    "sandwich",
    "panini",
];
const PROTEIN: &[&str] = &[
    "kylling", "fisk", "laks", "tun", "æg", "skyr", "bønner", "linser", "hummus", "cottage",
    "hytteost",
];
const FIBRE: &[&str] = &["rugbrød", "fuldkorn", "bønner", "linser"];
const VEG: &[&str] = &[
    "frugt", "grønt", "salat", "æble", "banan", "gulerod", "tomat",
];

fn has(text: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| {
        if *term == "æg" {
            tokens(text)
                .iter()
                .any(|w| w.ends_with("æg") && !w.ends_with("pålæg"))
        } else {
            term_score(text, term) != 0
        }
    })
}

pub(crate) fn by_price(a: &Deal, b: &Deal) -> Ordering {
    cmp_num(a.price_dkk - b.price_dkk)
        .or_else(|| cmp_num(f64::from(b.score - a.score)))
        .or_else(|| cmp_num(a.store.distance_m - b.store.distance_m))
        .unwrap_or_else(|| locale_compare(&a.key, &b.key))
}

pub(crate) fn by_discount(a: &Deal, b: &Deal) -> Ordering {
    cmp_num(b.discount_pct.unwrap_or(-1.0) - a.discount_pct.unwrap_or(-1.0))
        .or_else(|| cmp_num(a.price_dkk - b.price_dkk))
        .unwrap_or_else(|| locale_compare(&a.key, &b.key))
}

/// An insertion-ordered map (JS `Map`): a repeated key keeps its position.
struct Ordered<T> {
    index: HashMap<String, usize>,
    values: Vec<T>,
}

impl<T> Ordered<T> {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            values: Vec::new(),
        }
    }

    fn set(&mut self, key: String, value: T) {
        match self.index.get(&key) {
            Some(&i) => self.values[i] = value,
            None => {
                self.index.insert(key, self.values.len());
                self.values.push(value);
            }
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut T> {
        self.index.get(key).map(|&i| &mut self.values[i])
    }
}

/// Group identical food waste across branches, retaining the cheapest then nearest.
fn group_deals(mut deals: Vec<Deal>) -> Vec<Deal> {
    deals.sort_by(by_price);
    let mut groups: Ordered<Deal> = Ordered::new();
    for deal in deals {
        let key = if deal.source == DealSource::FoodWaste {
            let ean = deal.ean.as_ref();
            let id = if truthy(ean) {
                display(ean)
            } else {
                display(deal.name.as_ref())
            };
            format!("fw:{id}")
        } else {
            deal.key.clone()
        };
        match groups.get_mut(&key) {
            None => {
                let mut first = deal;
                first.related_keys = Some(vec![first.key.clone()]);
                first.more_stores = Some(0);
                groups.set(key, first);
            }
            Some(old) if deal.key != old.key => {
                old.related_keys.get_or_insert_with(Vec::new).push(deal.key);
                *old.more_stores.get_or_insert(0) += 1;
            }
            Some(_) => {}
        }
    }
    let mut out = groups.values;
    out.sort_by(by_price);
    out
}

/// The same physical branch can be listed by Salling and Tjek; Salling keeps the identity.
struct MergedStore {
    branch: Branch,
    dealer_ids: Vec<String>,
}

fn merge_map_stores(food_waste: Vec<Branch>, tjek: Vec<Branch>) -> Vec<MergedStore> {
    let mut stores: Vec<MergedStore> = Vec::new();
    for store in food_waste.into_iter().chain(tjek) {
        let dealer = store
            .dealer_id
            .as_ref()
            .filter(|d| truthy(Some(*d)))
            .map(|d| id_key(Some(d)));
        let old = stores.iter_mut().find(|s| {
            s.branch.chain_key == store.chain_key
                && haversine(s.branch.lat, s.branch.lng, store.lat, store.lng) <= 80.0
        });
        let Some(old) = old else {
            stores.push(MergedStore {
                branch: store,
                dealer_ids: dealer.into_iter().collect(),
            });
            continue;
        };
        old.branch.distance_m = old.branch.distance_m.min(store.distance_m);
        if let Some(d) = dealer {
            if !old.dealer_ids.contains(&d) {
                old.dealer_ids.push(d);
            }
        }
        if store.source == BranchSource::FoodWaste && old.branch.source != BranchSource::FoodWaste {
            let distance_m = old.branch.distance_m.min(store.distance_m);
            old.branch = Branch {
                distance_m,
                ..store
            };
        } else if store.source == BranchSource::Tjek {
            if !truthy(old.branch.name.as_ref()) {
                old.branch.name = store.name.clone();
            }
            if old.branch.street.as_deref().is_none_or(str::is_empty) {
                old.branch.street = store.street.clone();
            }
            if truthy(store.chain.as_ref()) {
                old.branch.chain = store.chain.clone();
            }
        }
    }
    stores
}

struct Collected {
    deals: Vec<Deal>,
    failures: Vec<String>,
    now: String,
}

/// The deals engine of one Lucina.
pub struct DealsEngine {
    cfg: Arc<DealsConfig>,
    shopping: Arc<dyn ShoppingSource>,
    sources: Sources,
    synonyms: Synonyms,
    clock: Clock,
}

impl std::fmt::Debug for DealsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DealsEngine")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl DealsEngine {
    pub fn new(
        config: DealsConfig,
        shopping: Arc<dyn ShoppingSource>,
        http: Arc<dyn DealsHttp>,
    ) -> Self {
        let cfg = Arc::new(config);
        let clock = system_clock();
        Self {
            synonyms: Synonyms::new(cfg.synonyms_path.clone()),
            sources: Sources::new(cfg.clone(), http, clock.clone()),
            cfg,
            shopping,
            clock,
        }
    }

    /// A fixed or test clock (epoch milliseconds).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.sources.set_clock(clock.clone());
        self.clock = clock;
        self
    }

    pub fn config(&self) -> &DealsConfig {
        &self.cfg
    }

    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// The current rule set (default rules + owner overrides).
    pub fn rules(&self) -> Arc<Vec<Rule>> {
        self.synonyms.rules()
    }

    fn radius(&self, requested: Option<f64>) -> f64 {
        [requested, Some(self.cfg.radius_m)]
            .into_iter()
            .flatten()
            .find(|r| *r != 0.0 && !r.is_nan())
            .unwrap_or(3000.0)
    }

    async fn collect(&self, terms: &[String], radius: f64, chain: Option<&str>) -> Collected {
        let mut unique_terms: Vec<String> = Vec::new();
        for t in terms {
            if !unique_terms.contains(t) {
                unique_terms.push(t.clone());
            }
        }
        let (fw, flyers) = futures_util::join!(
            self.sources.food_waste(radius),
            self.sources.flyers(&unique_terms, radius)
        );
        let mut failures = Vec::new();
        if fw.is_err() {
            failures.push("food_waste".to_owned());
        }
        if flyers.as_ref().map_or(true, |f| f.failed) {
            failures.push("flyer".to_owned());
        }
        let now_ms = (self.clock)();
        let mut unique: Ordered<Deal> = Ordered::new();
        let all = fw
            .unwrap_or_default()
            .into_iter()
            .chain(flyers.map(|f| f.deals).unwrap_or_default());
        for mut deal in all {
            let status = validity(deal.valid_from_str(), deal.valid_to_str(), now_ms);
            if status == Status::Expired
                || chain
                    .filter(|c| !c.is_empty())
                    .is_some_and(|c| c != deal.chain_key)
            {
                continue;
            }
            deal.status = status;
            deal.score = 0;
            unique.set(deal.key.clone(), deal);
        }
        Collected {
            deals: unique.values,
            failures,
            now: js::iso_string(now_ms),
        }
    }

    /// Each wanted item's display name and rule: the first row name that a
    /// rule knows, else the first name's canonical-product fallback.
    fn resolve_wanted(&self, rows: &[Vec<String>]) -> Vec<(String, Rule)> {
        let rules = self.synonyms.rules();
        rows.iter()
            .filter(|c| !c.is_empty())
            .map(|candidates| {
                let resolved: Vec<_> = candidates.iter().map(|n| resolve_rule(n, &rules)).collect();
                let rule = resolved
                    .iter()
                    .find(|r| r.known)
                    .unwrap_or(&resolved[0])
                    .rule
                    .clone();
                (candidates[0].clone(), rule)
            })
            .collect()
    }

    fn search_terms(wanted: &[(String, Rule)]) -> Vec<String> {
        wanted
            .iter()
            .flat_map(|(_, rule)| rule.terms.iter().take(3).cloned())
            .collect()
    }

    async fn match_names(&self, rows: &[Vec<String>], options: &MatchOptions) -> MatchResult {
        let wanted = self.resolve_wanted(rows);
        let result = self
            .collect(
                &Self::search_terms(&wanted),
                self.radius(options.radius_m),
                options.chain_key.as_deref(),
            )
            .await;
        let limit = options.limit.filter(|l| *l != 0).unwrap_or(3);
        let mut assigned: HashSet<String> = HashSet::new();
        let items = wanted
            .into_iter()
            .map(|(name, rule)| {
                let hits: Vec<Deal> = result
                    .deals
                    .iter()
                    .filter(|d| !assigned.contains(&d.key))
                    .filter_map(|d| {
                        let score = match_deal(&rule, d);
                        (score != 0).then(|| Deal { score, ..d.clone() })
                    })
                    .collect();
                let deals: Vec<Deal> = group_deals(hits).into_iter().take(limit).collect();
                for d in &deals {
                    assigned.extend(d.related_keys.iter().flatten().cloned());
                }
                MatchItem {
                    name,
                    rule_id: rule.id,
                    deals,
                }
            })
            .collect();
        MatchResult {
            items,
            failures: result.failures,
            now: result.now,
        }
    }

    /// Up to three offers for each needed shopping-list item.
    pub async fn list_matches(&self, user_id: &str) -> Result<MatchResult, DealsError> {
        self.list_matches_with(user_id, &MatchOptions::default())
            .await
    }

    pub async fn list_matches_with(
        &self,
        user_id: &str,
        options: &MatchOptions,
    ) -> Result<MatchResult, DealsError> {
        let list = self.shopping.shopping_list(user_id).await?;
        let rows: Vec<Vec<String>> = list
            .iter()
            .filter(|r| r.status == "needed")
            .map(ShoppingRow::candidates)
            .filter(|c| !c.is_empty())
            .collect();
        Ok(self.match_names(&rows, options).await)
    }

    /// The five cheapest offers for one product (`chainKey` scopes it to a chain).
    pub async fn cheapest(&self, query: &str, options: &MatchOptions) -> MatchResult {
        let options = MatchOptions {
            limit: Some(5),
            ..options.clone()
        };
        let rows = if query.is_empty() {
            Vec::new()
        } else {
            vec![vec![query.to_owned()]]
        };
        self.match_names(&rows, &options).await
    }

    /// Up to two healthy lunch combinations within budget, each at one branch.
    pub async fn lunch(&self, options: &MatchOptions) -> LunchResult {
        let terms: Vec<String> = LUNCH_TERMS.iter().map(|t| (*t).to_owned()).collect();
        let result = self
            .collect(
                &terms,
                self.radius(options.radius_m),
                options.chain_key.as_deref(),
            )
            .await;
        let budget = Some(self.cfg.lunch_budget_dkk)
            .filter(|b| *b != 0.0 && !b.is_nan())
            .unwrap_or(50.0);
        let mut branches: Ordered<Vec<LunchItem>> = Ordered::new();
        for deal in &result.deals {
            let name = deal.name_text();
            let category = deal.category.clone().unwrap_or_default();
            if deal.status != Status::Active || has(&name, VETO) || has(&category, VETO) {
                continue;
            }
            // Broad categories such as 'Kød & fisk' and 'Pesto & hummus' cannot
            // establish a component on their own. Fruit-flavoured yogurt is not fruit.
            let leaf = category.rsplit('>').next().unwrap_or("");
            let text = format!("{name} {}", if leaf.contains('&') { "" } else { leaf });
            let protein = has(&text, PROTEIN);
            let fibre = has(&text, FIBRE);
            let veg = !has(leaf, &["brød", "kager", "yoghurt", "skyr"]) && has(&text, VEG);
            if !protein && !fibre && !veg {
                continue;
            }
            let key = format!("{}:{}", deal.chain_key, display(deal.store.street.as_ref()));
            let item = LunchItem {
                deal: deal.clone(),
                protein,
                fibre,
                veg,
            };
            match branches.get_mut(&key) {
                Some(list) => list.push(item),
                None => branches.set(key, vec![item]),
            }
        }
        let mut candidates: Vec<LunchSuggestion> = Vec::new();
        for deals in &branches.values {
            for p in deals.iter().filter(|d| d.protein) {
                for c in deals.iter().filter(|d| d.fibre && d.deal.key != p.deal.key) {
                    let pc = p.deal.price_dkk + c.deal.price_dkk;
                    if pc > budget {
                        continue;
                    }
                    let mut extra: Option<&LunchItem> = None;
                    for d in deals.iter().filter(|d| {
                        d.veg
                            && d.deal.key != p.deal.key
                            && d.deal.key != c.deal.key
                            && pc + d.deal.price_dkk <= budget
                    }) {
                        if extra.is_none_or(|e| by_price(&d.deal, &e.deal) == Ordering::Less) {
                            extra = Some(d);
                        }
                    }
                    let mut items = vec![p.clone(), c.clone()];
                    items.extend(extra.cloned());
                    let total = items.iter().fold(0.0, |sum, d| sum + d.deal.price_dkk);
                    let saving = items.iter().fold(0.0, |sum, d| {
                        let normal = d
                            .deal
                            .normal_price_dkk
                            .filter(|n| *n != 0.0)
                            .unwrap_or(d.deal.price_dkk);
                        sum + (normal - d.deal.price_dkk).max(0.0)
                    });
                    let mut valid_tos: Vec<Option<Value>> =
                        items.iter().map(|d| d.deal.valid_to.clone()).collect();
                    valid_tos.sort_by(|a, b| {
                        let t = |v: &Option<Value>| {
                            v.as_ref()
                                .and_then(Value::as_str)
                                .and_then(parse_date)
                                .map_or(f64::NAN, |ms| ms as f64)
                        };
                        cmp_num(t(a) - t(b)).unwrap_or(Ordering::Equal)
                    });
                    let food_waste = items
                        .iter()
                        .filter(|d| d.deal.source == DealSource::FoodWaste)
                        .count() as f64;
                    let rank = food_waste * 100.0 + if extra.is_some() { 20.0 } else { 0.0 }
                        - p.deal.store.distance_m / 1000.0
                        - total / 50.0;
                    candidates.push(LunchSuggestion {
                        items,
                        total,
                        saving,
                        store: p.deal.store.clone(),
                        valid_to: valid_tos.into_iter().next().flatten(),
                        rank,
                    });
                }
            }
        }
        candidates.sort_by(|a, b| {
            cmp_num(b.rank - a.rank)
                .or_else(|| cmp_num(a.total - b.total))
                .unwrap_or(Ordering::Equal)
        });
        let mut seen = HashSet::new();
        let suggestions = candidates
            .into_iter()
            .filter(|s| {
                seen.insert(format!(
                    "{}:{}",
                    display(s.store.name.as_ref()),
                    display(s.store.street.as_ref())
                ))
            })
            .take(2)
            .collect();
        LunchResult {
            suggestions,
            budget,
            failures: result.failures,
            now: result.now,
        }
    }

    /// Every nearby grocery branch with its clearances and the list matches it can serve.
    pub async fn map_data(
        &self,
        user_id: &str,
        radius_m: Option<f64>,
    ) -> Result<MapData, DealsError> {
        let radius = self.radius(radius_m);
        let list = self.shopping.shopping_list(user_id).await?;
        let rows: Vec<Vec<String>> = list
            .iter()
            .filter(|r| r.status == "needed")
            .filter_map(|r| r.candidates().into_iter().next().map(|n| vec![n]))
            .collect();
        let wanted = self.resolve_wanted(&rows);
        let terms = Self::search_terms(&wanted);
        let (collected, fw_stores, tjek_stores) = futures_util::join!(
            self.collect(&terms, radius, None),
            self.sources.food_waste_stores(radius),
            self.sources.tjek_stores(radius)
        );
        let stores = merge_map_stores(
            fw_stores.unwrap_or_default(),
            tjek_stores.unwrap_or_default(),
        );
        let mut by_salling: HashMap<String, usize> = HashMap::new();
        let mut by_dealer: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, store) in stores.iter().enumerate() {
            if store.branch.source == BranchSource::FoodWaste {
                by_salling.insert(id_key(store.branch.source_id.as_ref()), i);
            }
            for dealer in &store.dealer_ids {
                by_dealer.entry(dealer.clone()).or_default().push(i);
            }
        }
        let mut list_matches: Vec<Ordered<ListMatch>> =
            stores.iter().map(|_| Ordered::new()).collect();
        let mut flyer_keys: Vec<Vec<String>> = stores.iter().map(|_| Vec::new()).collect();
        let mut assigned: HashSet<String> = HashSet::new();
        for (name, rule) in &wanted {
            let mut hits: Vec<Deal> = collected
                .deals
                .iter()
                .filter(|d| !assigned.contains(&d.key))
                .filter_map(|d| {
                    let score = match_deal(rule, d);
                    (score != 0).then(|| Deal { score, ..d.clone() })
                })
                .collect();
            hits.sort_by(by_price);
            for hit in hits {
                assigned.insert(hit.key.clone());
                let targets: Vec<usize> = if hit.source == DealSource::FoodWaste {
                    by_salling
                        .get(&id_key(hit.store.id.as_ref()))
                        .copied()
                        .into_iter()
                        .collect()
                } else {
                    by_dealer
                        .get(&id_key(hit.dealer_id.as_ref()))
                        .cloned()
                        .unwrap_or_default()
                };
                for i in targets {
                    list_matches[i].set(
                        format!("{name}|{}", hit.key),
                        ListMatch {
                            item: name.clone(),
                            rule_id: rule.id.clone(),
                            deal: hit.clone(),
                        },
                    );
                    if hit.source == DealSource::Flyer && !flyer_keys[i].contains(&hit.key) {
                        flyer_keys[i].push(hit.key.clone());
                    }
                }
            }
        }
        let mut clearances: Vec<&Deal> = collected
            .deals
            .iter()
            .filter(|d| d.source == DealSource::FoodWaste)
            .collect();
        clearances.sort_by(|a, b| by_discount(a, b));
        let deal_by_key: HashMap<&str, &Deal> = collected
            .deals
            .iter()
            .map(|d| (d.key.as_str(), d))
            .collect();
        let mut order: Vec<usize> = (0..stores.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&stores[a].branch, &stores[b].branch);
            cmp_num(a.distance_m - b.distance_m)
                .or_else(|| Some(locale_compare(&a.chain_key, &b.chain_key)).filter(|o| o.is_ne()))
                .or_else(|| {
                    Some(locale_compare(
                        &display(a.name.as_ref()),
                        &display(b.name.as_ref()),
                    ))
                    .filter(|o| o.is_ne())
                })
                .unwrap_or_else(|| locale_compare(&a.id, &b.id))
        });
        let mut out = Vec::new();
        let mut list_matches: Vec<Option<Vec<ListMatch>>> =
            list_matches.into_iter().map(|m| Some(m.values)).collect();
        for i in order {
            let b = &stores[i].branch;
            let source_key = id_key(b.source_id.as_ref());
            let mut flyer_offers: Vec<&Deal> = flyer_keys[i]
                .iter()
                .filter_map(|k| deal_by_key.get(k.as_str()).copied())
                .collect();
            flyer_offers.sort_by(|a, b| by_discount(a, b));
            out.push(MapStore {
                id: b.id.clone(),
                chain: b.chain.clone(),
                chain_key: b.chain_key.clone(),
                name: if truthy(b.name.as_ref()) {
                    b.name.clone()
                } else {
                    b.chain.clone()
                },
                street: b.street.clone().filter(|s| !s.is_empty()),
                lat: b.lat,
                lng: b.lng,
                distance_m: js::round(b.distance_m) as i64,
                list_matches: list_matches[i].take().unwrap_or_default(),
                clearances: if b.source == BranchSource::FoodWaste {
                    clearances
                        .iter()
                        .filter(|d| id_key(d.store.id.as_ref()) == source_key)
                        .take(40)
                        .map(|d| (*d).clone())
                        .collect()
                } else {
                    Vec::new()
                },
                flyer_offers: flyer_offers.into_iter().cloned().collect(),
            });
        }
        Ok(MapData {
            generated_at: collected.now,
            home_lat: self.cfg.home_lat,
            home_lng: self.cfg.home_lng,
            home_label: self.cfg.home_label.clone().filter(|l| !l.is_empty()),
            radius_m: radius,
            failures: collected.failures,
            stores: out,
        })
    }
}
