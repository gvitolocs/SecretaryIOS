//! The grocery ledger queries behind the chat intents
//! (`src/grocery/repository.js`): store context, pantry events, purchases,
//! the monthly spend summary and historical prices. Same SQL as Node.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::Row;

use crate::products::Product;
use crate::{iso, GroceryError, GroceryRepo};

type Result<T> = std::result::Result<T, GroceryError>;

/// A supermarket (`grocery.stores`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRow {
    pub id: String,
    pub chain_slug: String,
    pub display_name: String,
}

/// A price the purchase is recorded with.
#[derive(Debug, Clone, PartialEq)]
pub struct Price {
    pub price_dkk: f64,
    pub normal_price_dkk: Option<f64>,
    pub offer_price_dkk: Option<f64>,
    pub source: String,
    pub source_url: Option<String>,
    pub verification_status: String,
    pub confidence: f64,
    pub observed_at: Option<DateTime<Utc>>,
}

/// Spend this month (`summary`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub total_dkk: f64,
    pub verified_dkk: f64,
    pub inferred_dkk: f64,
    pub transactions: i64,
}

impl GroceryRepo {
    /// `getStoreContext`: `(store, confidence)`.
    pub async fn store_context(&self, user_id: &str) -> Result<Option<(StoreRow, f64)>> {
        let row = sqlx::query(
            "SELECT c.confidence::float8 AS confidence, s.id, s.chain_slug, s.display_name FROM grocery.store_context c \
             JOIN grocery.stores s ON s.id = c.store_id WHERE c.user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| {
            (
                StoreRow {
                    id: r.get("id"),
                    chain_slug: r.get::<Option<String>, _>("chain_slug").unwrap_or_default(),
                    display_name: r.get("display_name"),
                },
                r.get::<Option<f64>, _>("confidence").unwrap_or(0.7),
            )
        }))
    }

    async fn upsert_store(&self, s: &StoreRow) -> Result<()> {
        sqlx::query(
            "INSERT INTO grocery.stores (id, chain_slug, display_name) VALUES ($1,$2,$3) \
             ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name",
        )
        .bind(&s.id)
        .bind(&s.chain_slug)
        .bind(&s.display_name)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// `saveStoreContext`.
    pub async fn save_store_context(
        &self,
        user_id: &str,
        store: &StoreRow,
        confidence: f64,
        source: &str,
    ) -> Result<()> {
        self.upsert_store(store).await?;
        sqlx::query(
            "INSERT INTO grocery.store_context (user_id, store_id, confidence, source) VALUES ($1,$2,$3::float8::numeric,$4) \
             ON CONFLICT (user_id) DO UPDATE SET store_id=EXCLUDED.store_id, confidence=EXCLUDED.confidence, source=EXCLUDED.source, observed_at=now()",
        )
        .bind(user_id)
        .bind(&store.id)
        .bind(confidence)
        .bind(source)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn ensure_canonical_product(&self, p: &Product) -> Result<()> {
        if p.id.is_empty() {
            return Ok(());
        }
        self.ready().await?;
        let name = if p.name.is_empty() {
            p.id.clone()
        } else {
            p.name.clone()
        };
        let aliases = if p.aliases.is_empty() {
            vec![name.clone()]
        } else {
            p.aliases.clone()
        };
        sqlx::query(
            "INSERT INTO grocery.canonical_products (id, display_name, category, aliases) VALUES ($1, $2, $3, $4::jsonb) \
             ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, \
             category = COALESCE(EXCLUDED.category, grocery.canonical_products.category), aliases = EXCLUDED.aliases",
        )
        .bind(&p.id)
        .bind(&name)
        .bind(if p.category.is_empty() { "other" } else { p.category.as_str() })
        .bind(serde_json::to_string(&aliases).unwrap_or_else(|_| "[]".into()))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// `addPantryEvent` (`consumed`, `finished`, …).
    pub async fn add_pantry_event(
        &self,
        user_id: &str,
        product: &Product,
        event_type: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        occurred_at: DateTime<Utc>,
    ) -> Result<()> {
        self.ensure_canonical_product(product).await?;
        sqlx::query(
            "INSERT INTO grocery.pantry_events (user_id,product_id,event_type,quantity,unit,occurred_at,source,metadata) \
             VALUES ($1,$2,$3,$4::float8::numeric,$5,$6,$7,$8::jsonb)",
        )
        .bind(user_id)
        .bind(&product.id)
        .bind(event_type)
        .bind(quantity)
        .bind(unit)
        .bind(occurred_at)
        .bind(format!("grocery.{event_type}"))
        .bind("{}")
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// `findHistoricalPrice`.
    pub async fn historical_price(
        &self,
        product_id: &str,
        store_id: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<Price>> {
        let row = sqlx::query(
            "SELECT price_dkk::float8 AS price_dkk, normal_price_dkk::float8 AS normal_price_dkk, offer_price_dkk::float8 AS offer_price_dkk, \
             source, source_url, verification_status, observed_at FROM grocery.price_observations \
             WHERE product_id=$1 AND store_id=$2 AND observed_at <= $3 ORDER BY observed_at DESC LIMIT 1",
        )
        .bind(product_id)
        .bind(store_id)
        .bind(at)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| Price {
            price_dkk: r.get::<Option<f64>, _>("price_dkk").unwrap_or(0.0),
            normal_price_dkk: r.get("normal_price_dkk"),
            offer_price_dkk: r.get("offer_price_dkk"),
            source: r.get("source"),
            source_url: r.get("source_url"),
            verification_status: r.get("verification_status"),
            confidence: 0.9,
            observed_at: r.get("observed_at"),
        }))
    }

    /// `recordPurchase` (no catalog product: the Rust Lucina has no catalog
    /// adapters yet, so prices come from history only). Returns the
    /// transaction id.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_purchase(
        &self,
        user_id: &str,
        store: &StoreRow,
        purchased_at: DateTime<Utc>,
        product: &Product,
        raw_product: &str,
        quantity: Option<f64>,
        unit: Option<&str>,
        price: &Price,
        store_resolution: Value,
    ) -> Result<String> {
        self.ready().await?;
        self.upsert_store(store).await?;
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "INSERT INTO grocery.grocery_transactions (user_id,store_id,purchased_at,source,verification_status,confidence,total_dkk,metadata) \
             VALUES ($1,$2,$3,$4,$5,$6::float8::numeric,$7::float8::numeric,$8::jsonb) RETURNING id::text AS id",
        )
        .bind(user_id)
        .bind(&store.id)
        .bind(purchased_at)
        .bind(&price.source)
        .bind(&price.verification_status)
        .bind(price.confidence)
        .bind(price.price_dkk)
        .bind(json!({"storeResolution": store_resolution}).to_string())
        .fetch_one(&mut *tx)
        .await?;
        let id: String = row.get("id");
        sqlx::query(
            "INSERT INTO grocery.grocery_transaction_items (transaction_id,canonical_product_id,raw_product_name,catalog_product_id,quantity,unit,unit_price_dkk,total_price_dkk,category,brand,match_confidence,price_source,verification_status,metadata) \
             VALUES ($1::uuid,$2,$3,NULL,$4::float8::numeric,$5,$6::float8::numeric,$6::float8::numeric,$7,NULL,$8::float8::numeric,$9,$10,$11::jsonb)",
        )
        .bind(&id)
        .bind(&product.id)
        .bind(raw_product)
        .bind(quantity)
        .bind(unit)
        .bind(price.price_dkk)
        .bind(&product.category)
        .bind(product.confidence)
        .bind(&price.source)
        .bind(&price.verification_status)
        .bind(json!({"sourceUrl": price.source_url, "observedAt": price.observed_at.map(iso)}).to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO grocery.pantry_events (user_id,product_id,event_type,quantity,unit,occurred_at,source,metadata) \
             VALUES ($1,$2,'purchased',$3::float8::numeric,$4,$5,'grocery.purchase',$6::jsonb)",
        )
        .bind(user_id)
        .bind(&product.id)
        .bind(quantity)
        .bind(unit)
        .bind(purchased_at)
        .bind(json!({"transactionId": id}).to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// `summary`: this month's spend.
    pub async fn spend_summary(&self, user_id: &str) -> Result<Summary> {
        let r = sqlx::query(
            "SELECT COALESCE(sum(total_dkk),0)::float8 total_dkk, \
             COALESCE(sum(total_dkk) FILTER (WHERE verification_status IN ('receipt_verified','catalog_verified','offer_verified','historical_snapshot')),0)::float8 verified_dkk, \
             COALESCE(sum(total_dkk) FILTER (WHERE verification_status IN ('catalog_inferred','estimated')),0)::float8 inferred_dkk, count(*) transactions \
             FROM grocery.grocery_transactions WHERE user_id=$1 AND purchased_at >= date_trunc('month', now()) AND purchased_at < now() + interval '1 second'",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        Ok(Summary {
            total_dkk: r.get("total_dkk"),
            verified_dkk: r.get("verified_dkk"),
            inferred_dkk: r.get("inferred_dkk"),
            transactions: r.get("transactions"),
        })
    }

    /// `pantry`: `(product, status)` newest first.
    pub async fn pantry(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT p.display_name product, (array_agg(e.event_type ORDER BY e.occurred_at DESC))[1] status \
             FROM grocery.pantry_events e JOIN grocery.canonical_products p ON p.id=e.product_id WHERE e.user_id=$1 \
             GROUP BY p.id,p.display_name,p.category ORDER BY max(e.occurred_at) DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("product"),
                    r.get::<Option<String>, _>("status").unwrap_or_default(),
                )
            })
            .collect())
    }
}
