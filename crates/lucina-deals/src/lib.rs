//! Grocery deals near home, ported 1:1 from the Node Lucina's
//! `src/grocery/deals/` (the Node code is retired with this port):
//!
//! - [`sources`]: Salling food-waste clearances and Tjek flyer offers and
//!   stores, grocery chains only, cached with in-flight request sharing;
//! - [`synonyms`] and [`matcher`]: shopping-list item → search terms and
//!   heading matching (Danish head-final compounds, excludes, categories);
//! - [`engine`]: list matches, cheapest offers, healthy lunch, map data;
//! - [`format`]: the English chat and notification text;
//! - [`watch`]: the digest/alert run and its `deal-watch.json` state.
//!
//! Output parity with Node (text byte for byte, JSON value for value) is
//! checked against recorded fixtures in `tests/`. [`js`] holds the
//! JavaScript number, date, collation and JSON semantics this relies on.

pub mod engine;
pub mod format;
pub mod js;
pub mod matcher;
pub mod sources;
pub mod synonyms;
pub mod validity;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;

pub use engine::{
    DealsEngine, FixedList, LunchItem, LunchResult, LunchSuggestion, MapData, MapStore, MatchItem,
    MatchOptions, MatchResult, ShoppingRow, ShoppingSource,
};
pub use format::{format_deal, format_lunch, format_matches, FormatOptions};
pub use sources::{Deal, DealSource, DealsHttp, FixtureHttp, FnHttp, HttpError, ReqwestHttp};
pub use synonyms::{default_rules, resolve_rule, Rule, Synonyms};
pub use validity::{validity, Status};
pub use watch::{
    compose, Composed, DealWatch, Deliver, Delivery, MatchSource, Notification, RunOutcome,
    WatchKind, WatchSettings, WatchState,
};

/// Epoch milliseconds (injectable for fixtures and tests).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| chrono::Utc::now().timestamp_millis())
}

/// A fixed clock.
pub fn fixed_clock(ms: i64) -> Clock {
    Arc::new(move || ms)
}

#[derive(Debug, thiserror::Error)]
pub enum DealsError {
    #[error("shopping list unavailable: {0}")]
    Shopping(String),
    #[error("deal watch state: {0}")]
    State(String),
}

/// The Node `config.grocery` fields the engine reads.
#[derive(Clone)]
pub struct DealsConfig {
    /// `GROCERY_HOME_LAT` / `GROCERY_HOME_LNG`.
    pub home_lat: Option<f64>,
    pub home_lng: Option<f64>,
    /// `GROCERY_HOME_LABEL` (Node default `home`).
    pub home_label: Option<String>,
    /// `SALLING_API_KEY`; without it food waste reports a failure.
    pub salling_api_key: Option<String>,
    /// `SALLING_API_BASE_URL`.
    pub salling_base_url: String,
    /// `GROCERY_DEALS_TJEK_BASE_URL`.
    pub tjek_base_url: String,
    /// `GROCERY_DEALS_RADIUS_M`, else `GROCERY_NEARBY_RADIUS_M`, else 3000.
    pub radius_m: f64,
    /// `GROCERY_DEALS_LUNCH_BUDGET_DKK`.
    pub lunch_budget_dkk: f64,
    /// `GROCERY_REQUEST_TIMEOUT_MS` (the live transport's timeout).
    pub request_timeout_ms: u64,
    /// `GROCERY_DEALS_SYNONYMS_PATH`.
    pub synonyms_path: PathBuf,
}

impl Default for DealsConfig {
    fn default() -> Self {
        Self {
            home_lat: None,
            home_lng: None,
            home_label: None,
            salling_api_key: None,
            salling_base_url: "https://api.sallinggroup.com".into(),
            tjek_base_url: "https://squid-api.tjek.com".into(),
            radius_m: 3000.0,
            lunch_budget_dkk: 50.0,
            request_timeout_ms: 8000,
            synonyms_path: PathBuf::from("data/grocery/deal-synonyms.json"),
        }
    }
}

impl std::fmt::Debug for DealsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DealsConfig")
            .field("home_lat", &self.home_lat)
            .field("home_lng", &self.home_lng)
            .field("home_label", &self.home_label)
            .field(
                "salling_api_key",
                &self.salling_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("salling_base_url", &self.salling_base_url)
            .field("tjek_base_url", &self.tjek_base_url)
            .field("radius_m", &self.radius_m)
            .field("lunch_budget_dkk", &self.lunch_budget_dkk)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("synonyms_path", &self.synonyms_path)
            .finish()
    }
}
