//! Grocery deals near home, served by this Lucina from `lucina-deals` (the
//! Node deals code is retired; owner decision 2026-10-24):
//!
//! - `GET /api/secretary/grocery/deals/map[?radiusM=500..15000]`: Node's
//!   `handleGroceryDealsMap` (503 `deals_unavailable` without home
//!   coordinates, 400 for a bad `radiusM`);
//! - chat turns classified `grocery.offer.search`, `grocery.deals.cheapest`
//!   or `grocery.deals.lunch`: the deals branch of Node's grocery
//!   `process()` ([`answer`]), else the Rust grocery path as on Node;
//! - the deal watch ([`spawn_watch`]): digest and alerts as app
//!   notifications, with its own switch `GROCERY_OFFER_WATCH_ENABLED`;
//! - `lucina deals …` ([`cli`]): Node's `scripts/grocery-deals.mjs`.
//!
//! Settings are Node's variables (`GROCERY_HOME_*`, `GROCERY_NEARBY_RADIUS_M`,
//! `SALLING_API_*`, `GROCERY_DEALS_*`, `GROCERY_REQUEST_TIMEOUT_MS`), read by
//! [`lucina_deals::DealsConfig::from_lookup`] and [`lucina_deals::WatchConfig`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::Uri;
use axum::response::Response;
use lucina_deals::format::format_map_summary;
use lucina_deals::{
    chain_key, format_lunch, format_matches, AppNotify, DealWatch, DealsConfig, DealsEngine,
    DealsError, DealsHttp, FixedList, FixtureHttp, FormatOptions, MatchOptions, ReqwestHttp,
    Scheduler, ShoppingRow, ShoppingSource, WatchKind,
};
use lucina_grocery::GroceryRepo;
use lucina_intent::grocery::parse_grocery_intent;

use crate::api::{ok, ApiError};
use crate::{AppState, Config};

/// The grocery intents the deal engine answers in chat.
pub const DEALS_INTENTS: &[&str] = &[
    "grocery.offer.search",
    "grocery.deals.cheapest",
    "grocery.deals.lunch",
];

/// The shopping list without a grocery database: every read fails, as the
/// Node repository does without `GROCERY_DATABASE_URL`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoShopping;

#[async_trait]
impl ShoppingSource for NoShopping {
    async fn shopping_list(&self, _user_id: &str) -> Result<Vec<ShoppingRow>, DealsError> {
        Err(DealsError::Shopping(
            "the grocery database is not configured".into(),
        ))
    }
}

fn shopping(grocery: Option<Arc<GroceryRepo>>) -> Arc<dyn ShoppingSource> {
    match grocery {
        Some(repo) => repo,
        None => Arc::new(NoShopping),
    }
}

fn live_http(cfg: &DealsConfig) -> Arc<dyn DealsHttp> {
    Arc::new(ReqwestHttp::new(Duration::from_millis(
        cfg.request_timeout_ms,
    )))
}

/// The engine, only with home coordinates (as Node's `createGroceryService`).
pub fn engine(cfg: &Config, grocery: Option<Arc<GroceryRepo>>) -> Option<Arc<DealsEngine>> {
    if !cfg.deals.has_home() {
        return None;
    }
    Some(Arc::new(DealsEngine::new(
        cfg.deals.clone(),
        shopping(grocery),
        live_http(&cfg.deals),
    )))
}

const RADIUS_MESSAGE: &str = "radiusM must be an integer from 500 to 15000";

/// `radiusM` of the map: absent or empty is the configured radius.
fn map_radius(raw: Option<&str>) -> Result<Option<f64>, ApiError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad_request(RADIUS_MESSAGE));
    }
    let radius = lucina_deals::js::string_to_number(raw);
    if radius.fract() != 0.0 || !(500.0..=15000.0).contains(&radius) {
        return Err(ApiError::bad_request(RADIUS_MESSAGE));
    }
    Ok(Some(radius))
}

/// `GET /grocery/deals/map`.
pub async fn map(st: &AppState, uri: &Uri) -> Result<Response, ApiError> {
    let engine = st.deals.as_ref().ok_or_else(|| {
        ApiError::new(
            503,
            "deals_unavailable",
            "Nearby deals are unavailable without home coordinates",
        )
    })?;
    let radius = map_radius(crate::api::query_param(uri, "radiusM").as_deref())?;
    let data = engine
        .map_data(&st.cfg.grocery_user_id, radius)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "secretary api request failed (deals map)");
            ApiError::internal()
        })?;
    Ok(ok(data.to_json()))
}

/// "anywhere", "ovunque", "overalt"…: offers up to 15 km away.
fn anywhere(text: &str) -> bool {
    lucina_intent::re!(
        r"\b(?:anywhere|everywhere|any store|all stores|ovunque|dappertutto|qualsiasi (?:negozio|supermercato)|overalt|alle butikker)\b",
        "i"
    )
    .test(text)
}

/// The deals branch of Node's grocery `process()`: `(reply, intent)` for a
/// deal question the engine can answer, `None` to continue with the grocery
/// path (a product offer question without a deal goes on to the catalog,
/// a failed shopping-list read too).
pub async fn answer(
    engine: &DealsEngine,
    user_id: &str,
    text: &str,
) -> Option<(String, &'static str)> {
    let parsed = parse_grocery_intent(text)?;
    let intent = parsed.intent;
    if !DEALS_INTENTS.contains(&intent) {
        return None;
    }
    let options = MatchOptions {
        chain_key: parsed.store.as_ref().map(|s| chain_key(&s.chain_slug)),
        radius_m: anywhere(text).then_some(15000.0),
        limit: None,
    };
    if intent == "grocery.deals.lunch" {
        return Some((format_lunch(&engine.lunch(&options).await), intent));
    }
    let result = match &parsed.product {
        Some(product) => {
            let query = if parsed.raw_product.is_empty() {
                product.name.as_str()
            } else {
                parsed.raw_product.as_str()
            };
            engine.cheapest(query, &options).await
        }
        None => match engine.list_matches_with(user_id, &options).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "deal answer: shopping list unavailable");
                return None;
            }
        },
    };
    let any = result.items.iter().any(|i| !i.deals.is_empty());
    if parsed.product.is_none() || any || intent == "grocery.deals.cheapest" {
        return Some((format_matches(&result, &FormatOptions::default()), intent));
    }
    None
}

fn check_channel(channel: &str) -> Result<(), String> {
    match channel {
        "app" => Ok(()),
        "telegram" | "both" => {
            tracing::warn!(
                channel,
                "GROCERY_DEALS_CHANNEL: Telegram delivery belongs to the Node Lucina; deal notifications go to the app"
            );
            Ok(())
        }
        _ => Err("Invalid grocery deals channel".into()),
    }
}

fn app_notify(cfg: &Config) -> AppNotify {
    AppNotify::new(
        cfg.app_notify,
        cfg.secretary_url.clone(),
        cfg.secretary_service_token_file.clone(),
    )
}

/// Start the deal watch when `GROCERY_OFFER_WATCH_ENABLED=true` and the
/// engine exists (independent of `LUCINA_SCHEDULER`).
pub fn spawn_watch(st: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    let wc = &st.cfg.deal_watch;
    if !wc.enabled {
        return None;
    }
    let Some(engine) = st.deals.clone() else {
        tracing::warn!("GROCERY_OFFER_WATCH_ENABLED=true without home coordinates (GROCERY_HOME_LAT/LNG): the deal watch stays off");
        return None;
    };
    if let Err(e) = check_channel(&wc.channel) {
        tracing::error!(channel = %wc.channel, "{e}: the deal watch stays off");
        return None;
    }
    if st.grocery.is_none() {
        tracing::warn!("deal watch: no grocery database, so no shopping list to match");
    }
    if !st.cfg.app_notify {
        tracing::warn!("deal watch: LUCINA_APP_NOTIFY is off, notifications are not delivered");
    }
    let watch = DealWatch::new(
        wc.watch_settings(),
        engine,
        Arc::new(app_notify(&st.cfg)),
        &st.cfg.grocery_user_id,
    );
    Some(Arc::new(Scheduler::new(Arc::new(watch), wc.clone())).spawn())
}

// ---------------------------------------------------------------- CLI

pub const CLI_USAGE: &str = "Usage: lucina deals list|cheapest <query>|lunch|map|digest|alerts [--dry-run|--send] [--channel app|telegram|both] [--user ID] [--fixtures DIR] [--items \"A,B\"] [--radiusM 500-15000]";

fn take_option(args: &mut Vec<String>, name: &str) -> anyhow::Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    let value = args
        .get(i + 1)
        .filter(|v| !v.is_empty() && !v.starts_with("--"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Missing {name} value"))?;
    args.drain(i..i + 2);
    Ok(Some(value))
}

/// `lucina deals …`: the exit code. Errors print one generic line (remote
/// errors may carry URLs with credentials), as Node's script does.
pub async fn cli(args: Vec<String>) -> i32 {
    match cli_run(args).await {
        Ok(code) => code,
        Err(e) => {
            tracing::debug!(error = %e, "deals command failed");
            let missing = e
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound);
            if missing {
                eprintln!("Fixture or state file missing.");
            } else {
                eprintln!("Deal command failed. Check command arguments, configuration and source availability.");
            }
            1
        }
    }
}

async fn cli_run(mut args: Vec<String>) -> anyhow::Result<i32> {
    let command = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };
    let fixtures = take_option(&mut args, "--fixtures")?.map(PathBuf::from);
    let items = take_option(&mut args, "--items")?;
    let user = take_option(&mut args, "--user")?;
    let channel = take_option(&mut args, "--channel")?;
    let radius_option = take_option(&mut args, "--radiusM")?;
    if let Some(c) = &channel {
        if !["app", "telegram", "both"].contains(&c.as_str()) {
            anyhow::bail!("Invalid channel");
        }
    }
    let mut cfg = Config::from_env()?;
    crate::secrets::register_bootstrap(&cfg);
    let user_id = user.unwrap_or_else(|| cfg.grocery_user_id.clone());
    let send = args.iter().any(|a| a == "--send");
    if fixtures.is_some() && send {
        anyhow::bail!("Fixture mode cannot send notifications");
    }
    let mut clock = None;
    if let Some(dir) = &fixtures {
        let context: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("context.json"))?)?;
        cfg.deals.home_lat = context["homeLat"].as_f64();
        cfg.deals.home_lng = context["homeLng"].as_f64();
        cfg.deals.salling_api_key = Some("offline-fixture".into());
        let now = context["now"]
            .as_str()
            .and_then(lucina_deals::js::parse_date)
            .ok_or_else(|| anyhow::anyhow!("context.json has no now"))?;
        clock = Some(lucina_deals::fixed_clock(now));
    }
    if let Some(c) = channel {
        cfg.deal_watch.channel = c;
    }
    if !cfg.deals.has_home() {
        anyhow::bail!("Set grocery home coordinates before querying deals");
    }
    let list: Arc<dyn ShoppingSource> = match &items {
        Some(items) => Arc::new(FixedList(
            items
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(ShoppingRow::needed)
                .collect(),
        )),
        None => shopping(crate::connect_grocery(&cfg)?.map(Arc::new)),
    };
    let http: Arc<dyn DealsHttp> = match &fixtures {
        Some(dir) => Arc::new(FixtureHttp::new(dir.clone())),
        None => live_http(&cfg.deals),
    };
    let mut engine = DealsEngine::new(cfg.deals.clone(), list, http);
    if let Some(c) = &clock {
        engine = engine.with_clock(c.clone());
    }
    match command.as_str() {
        "list" => println!(
            "{}",
            format_matches(
                &engine.list_matches(&user_id).await?,
                &FormatOptions::default()
            )
        ),
        "cheapest" => {
            let query = args
                .iter()
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if query.is_empty() {
                anyhow::bail!("cheapest requires a product");
            }
            println!(
                "{}",
                format_matches(
                    &engine.cheapest(&query, &MatchOptions::default()).await,
                    &FormatOptions::default()
                )
            );
        }
        "lunch" => println!(
            "{}",
            format_lunch(&engine.lunch(&MatchOptions::default()).await)
        ),
        "map" => {
            let radius = match radius_option {
                None => None,
                Some(raw) => {
                    let r = lucina_deals::js::string_to_number(&raw);
                    if r.fract() != 0.0 || !(500.0..=15000.0).contains(&r) {
                        anyhow::bail!("{RADIUS_MESSAGE}");
                    }
                    Some(r)
                }
            };
            let map = engine.map_data(&user_id, radius).await?;
            println!("{}", format_map_summary(&map));
        }
        "digest" | "alerts" => {
            check_channel(&cfg.deal_watch.channel).map_err(|e| anyhow::anyhow!(e))?;
            let mut watch = DealWatch::new(
                cfg.deal_watch.watch_settings(),
                Arc::new(engine),
                Arc::new(app_notify(&cfg)),
                &user_id,
            );
            if let Some(c) = clock {
                watch = watch.with_clock(c);
            }
            let kind = if command == "digest" {
                WatchKind::Digest
            } else {
                WatchKind::Alerts
            };
            let dry_run = !send || args.iter().any(|a| a == "--dry-run");
            let outcome = watch.run(kind, dry_run).await?;
            println!("{}", outcome.display());
            if send
                && !outcome.sent
                && matches!(outcome.reason.as_str(), "send failed" | "send rejected")
            {
                return Ok(1);
            }
        }
        _ => {
            eprintln!("{CLI_USAGE}");
            anyhow::bail!("unknown deals command");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_is_nodes_check() {
        assert_eq!(map_radius(None).unwrap(), None);
        assert_eq!(map_radius(Some("")).unwrap(), None);
        assert_eq!(map_radius(Some("1500")).unwrap(), Some(1500.0));
        assert_eq!(map_radius(Some("0500")).unwrap(), Some(500.0));
        for bad in ["499", "15001", "1e3", "-600", "600.0", " 600", "abc"] {
            let e = map_radius(Some(bad)).unwrap_err();
            assert_eq!(
                (e.status.as_u16(), e.code.as_str(), e.message.as_str()),
                (400, "bad_request", RADIUS_MESSAGE),
                "{bad}"
            );
        }
    }

    #[test]
    fn anywhere_lifts_the_radius() {
        assert!(anywhere("offerte sul latte ovunque?"));
        assert!(anywhere("cheapest milk anywhere"));
        assert!(anywhere("mælk tilbud alle butikker"));
        assert!(!anywhere("cheapest milk nearby"));
    }

    #[test]
    fn cli_options_need_values() {
        let mut a: Vec<String> = ["--items", "A,B", "--send"].map(String::from).to_vec();
        assert_eq!(
            take_option(&mut a, "--items").unwrap().as_deref(),
            Some("A,B")
        );
        assert_eq!(a, vec!["--send".to_owned()]);
        let mut a: Vec<String> = ["--user", "--send"].map(String::from).to_vec();
        assert!(take_option(&mut a, "--user").is_err());
    }
}
