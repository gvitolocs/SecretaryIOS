//! Shopping v2 on Lucina's grocery Postgres: the schema of
//! `migrations/001_grocery.sql` + `002_shopping_v2.sql` (copied from the
//! Lucina shopping-v2 branch) and the op semantics of `src/grocery/shopping.js`
//! (SecretaryIOS `docs/shopping-v2.md`):
//!
//! - every write is an op batch in one transaction; op ids are idempotent
//!   through `grocery.shopping_ops` receipts (applied and rejected);
//! - the list revision goes up by one per batch that applied anything;
//! - a stale `expectedRevision` is a 409 `revision_conflict` only when an op
//!   touches an item changed after it (then nothing is applied);
//! - deleted stays deleted; one active row per (list, catalogId-or-name).

pub mod catalog;
pub mod ledger;
pub mod products;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use sqlx::{Connection, Row};
use uuid::Uuid;

pub use catalog::{Catalog, CatalogIndex};
pub use ledger::{Price, StoreRow, Summary};
pub use products::{resolve_canonical_product, Product};

pub const THEMES: [&str; 8] = [
    "home", "grocery", "work", "holiday", "garden", "hardware", "baby", "other",
];
pub const MODIFIERS: [&str; 6] = ["telegram", "discord", "web", "ios", "alexa", "lucina"];
pub const OP_TYPES: [&str; 11] = [
    "add",
    "need",
    "purchase",
    "remove",
    "set_spec",
    "set_quantity",
    "set_section",
    "rename",
    "move",
    "clear_recent",
    "reorder",
];
pub const MAX_OPS_PER_BATCH: usize = 200;
pub const DEFAULT_LIST_NAME: &str = "Casa";
const MAX_NAME: usize = 200;
const MAX_LIST_NAME: usize = 100;
const MAX_SPEC: usize = 200;
const MAX_UNIT: usize = 20;
const MAX_SECTION: usize = 50;
const MAX_CATALOG_ID: usize = 100;

const ITEM_COLUMNS: &str = "i.id, i.list_id, i.raw_name, i.status, i.catalog_item_id, i.specification, \
    i.quantity::float8 AS quantity, i.unit, i.section_override, i.icon_key, i.position, i.purchased_at, \
    i.modified_by, i.rev, i.created_at, i.updated_at";

fn name_key_sql(param: &str) -> String {
    format!("lower(btrim(regexp_replace({param}, '\\s+', ' ', 'g')))")
}

/// Errors. `Shopping` carries an API error code (`bad_request`,
/// `list_not_found`, `default_list`, `revision_conflict`, `item_not_found`).
#[derive(Debug, thiserror::Error)]
pub enum GroceryError {
    #[error("{message}")]
    Shopping {
        code: &'static str,
        message: String,
        revision: Option<i64>,
        conflicts: Vec<String>,
    },
    #[error("grocery database unreachable: {0}")]
    Unavailable(String),
    #[error("grocery database: {0}")]
    Db(sqlx::Error),
}

impl GroceryError {
    fn shopping(code: &'static str, message: impl Into<String>) -> Self {
        GroceryError::Shopping {
            code,
            message: message.into(),
            revision: None,
            conflicts: Vec::new(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            GroceryError::Shopping { code, .. } => code,
            GroceryError::Unavailable(_) => "grocery_unavailable",
            GroceryError::Db(_) => "internal_error",
        }
    }
}

fn is_unavailable(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Tls(_)
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => {
            matches!(db.code().as_deref(), Some("57P01" | "57P03" | "53300"))
        }
        other => {
            let s = other.to_string().to_lowercase();
            s.contains("timeout") || s.contains("connect") || s.contains("terminat")
        }
    }
}

impl From<sqlx::Error> for GroceryError {
    fn from(e: sqlx::Error) -> Self {
        if is_unavailable(&e) {
            GroceryError::Unavailable(e.to_string())
        } else {
            GroceryError::Db(e)
        }
    }
}

pub type Result<T> = std::result::Result<T, GroceryError>;

/// A per-op failure: the op is rejected (with a receipt), the batch goes on.
#[derive(Debug)]
enum OpError {
    Rejected(&'static str),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for OpError {
    fn from(e: sqlx::Error) -> Self {
        OpError::Db(e)
    }
}

type OpResult<T> = std::result::Result<T, OpError>;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, sqlx::FromRow)]
struct ListRow {
    id: Uuid,
    name: String,
    theme: String,
    position: i32,
    is_default: bool,
    revision: i64,
    updated_at: DateTime<Utc>,
    #[sqlx(default)]
    needed: Option<i64>,
    #[sqlx(default)]
    recent: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    list_id: Uuid,
    raw_name: String,
    status: String,
    catalog_item_id: Option<String>,
    specification: Option<String>,
    quantity: Option<f64>,
    unit: Option<String>,
    section_override: Option<String>,
    icon_key: Option<String>,
    position: Option<i32>,
    purchased_at: Option<DateTime<Utc>>,
    modified_by: Option<String>,
    rev: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `Number(x)` in JSON: integral values without a fraction.
fn js_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

fn opt_str(v: &Option<String>) -> Value {
    match v {
        Some(s) if !s.is_empty() => Value::String(s.clone()),
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------- op field helpers

fn js_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn op_text(v: Option<&Value>, max: usize, required: bool) -> OpResult<Option<String>> {
    match v {
        None | Some(Value::Null) => {
            if required {
                Err(OpError::Rejected("missing_field"))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) => {
            let t = s.trim();
            if required && t.is_empty() {
                return Err(OpError::Rejected("missing_field"));
            }
            if js_len(t) > max {
                return Err(OpError::Rejected("field_too_long"));
            }
            Ok((!t.is_empty()).then(|| t.to_owned()))
        }
        Some(_) => Err(OpError::Rejected("invalid_field")),
    }
}

/// `Number(value)` like JS (numbers, numeric strings, booleans).
fn js_to_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn op_quantity(v: Option<&Value>) -> OpResult<Option<f64>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(v) => {
            let n = js_to_number(v);
            if !n.is_finite() || !(0.0..=1e6).contains(&n) {
                Err(OpError::Rejected("invalid_quantity"))
            } else {
                Ok(Some(n))
            }
        }
    }
}

pub fn is_uuid(s: &str) -> bool {
    s.len() == 36 && Uuid::parse_str(s).is_ok()
}

fn str_field<'a>(op: &'a Value, key: &str) -> Option<&'a str> {
    op.get(key).and_then(Value::as_str)
}

// ---------------------------------------------------------------- repository

/// Result of [`GroceryRepo::apply_ops`].
#[derive(Debug, Clone)]
pub struct OpsOutcome {
    pub revision: i64,
    pub applied: Vec<String>,
    pub duplicates: Vec<String>,
    /// `{opId, error}`
    pub rejected: Vec<Value>,
    pub list: Value,
    pub items: Vec<Value>,
    pub item_ids: HashMap<String, String>,
}

impl OpsOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "revision": self.revision,
            "applied": self.applied,
            "duplicates": self.duplicates,
            "rejected": self.rejected,
            "list": self.list,
            "items": self.items,
        })
    }
}

struct Target {
    id: Uuid,
    next_rev: i64,
}

struct Ctx<'a> {
    list: &'a ListRow,
    user_id: &'a str,
    modified_by: &'a str,
    rev: i64,
    targets: HashMap<Uuid, Target>,
}

pub struct GroceryRepo {
    pool: PgPool,
    catalog: Catalog,
    seeded: tokio::sync::OnceCell<()>,
}

impl std::fmt::Debug for GroceryRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroceryRepo").finish_non_exhaustive()
    }
}

impl GroceryRepo {
    /// Lazy pool (no connection until first use), 4 connections, 8 s
    /// acquire timeout, like the Node pool.
    pub fn connect_lazy(database_url: &str, catalog: Catalog) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(8))
            .idle_timeout(Duration::from_secs(10))
            .connect_lazy(database_url)
            .map_err(GroceryError::from)?;
        Ok(Self::with_pool(pool, catalog))
    }

    pub fn with_pool(pool: PgPool, catalog: Catalog) -> Self {
        Self {
            pool,
            catalog,
            seeded: tokio::sync::OnceCell::new(),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The current shopping catalog.
    pub fn catalog_index(&self) -> std::sync::Arc<CatalogIndex> {
        self.catalog.index()
    }

    /// Seed `grocery.canonical_products` once (as Node's `ready()`).
    async fn ready(&self) -> Result<()> {
        self.seeded
            .get_or_try_init(|| async {
                let mut conn = self.pool.acquire().await?;
                for p in products::known_canonical_products() {
                    sqlx::query(
                        "INSERT INTO grocery.canonical_products (id, display_name, category, aliases) VALUES ($1, $2, $3, $4::jsonb) \
                         ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name, category = EXCLUDED.category, aliases = EXCLUDED.aliases",
                    )
                    .bind(&p.id)
                    .bind(&p.name)
                    .bind(&p.category)
                    .bind(serde_json::to_string(&p.aliases).unwrap_or_else(|_| "[]".into()))
                    .execute(&mut *conn)
                    .await?;
                }
                Ok::<(), GroceryError>(())
            })
            .await?;
        Ok(())
    }

    fn public_item(&self, row: &ItemRow) -> Value {
        let idx = self.catalog.index();
        let entry = row.catalog_item_id.as_deref().and_then(|id| idx.item(id));
        json!({
            "id": row.id.to_string(),
            "listId": row.list_id.to_string(),
            "catalogId": opt_str(&row.catalog_item_id),
            "name": row.raw_name,
            "spec": opt_str(&row.specification),
            "quantity": row.quantity.map(js_number).unwrap_or(Value::Null),
            "unit": opt_str(&row.unit),
            "section": opt_str(&row.section_override.clone().or_else(|| entry.and_then(|e| e.section.clone()))),
            "icon": opt_str(&row.icon_key.clone().or_else(|| entry.and_then(|e| e.icon.clone()))),
            "status": row.status,
            "position": row.position,
            "purchasedAt": row.purchased_at.map(iso),
            "createdAt": iso(row.created_at),
            "updatedAt": iso(row.updated_at),
            "updatedBy": opt_str(&row.modified_by),
        })
    }

    fn public_list(row: &ListRow) -> Value {
        let mut v = json!({
            "id": row.id.to_string(),
            "name": row.name,
            "theme": row.theme,
            "isDefault": row.is_default,
            "position": row.position,
            "revision": row.revision,
        });
        if let (Some(n), Some(r)) = (row.needed, row.recent) {
            v["needed"] = json!(n);
            v["recent"] = json!(r);
        }
        v["updatedAt"] = json!(iso(row.updated_at));
        v
    }

    fn short_list(row: &ListRow) -> Value {
        json!({"id": row.id.to_string(), "name": row.name, "theme": row.theme, "isDefault": row.is_default})
    }

    async fn ensure_default_list(conn: &mut PgConnection, user_id: &str) -> Result<ListRow> {
        sqlx::query(
            "INSERT INTO grocery.shopping_lists (user_id, name, theme, is_default) VALUES ($1, $2, 'home', true) \
             ON CONFLICT (user_id) WHERE is_default DO NOTHING",
        )
        .bind(user_id)
        .bind(DEFAULT_LIST_NAME)
        .execute(&mut *conn)
        .await?;
        Ok(sqlx::query_as::<_, ListRow>(
            "SELECT * FROM grocery.shopping_lists WHERE user_id = $1 AND is_default",
        )
        .bind(user_id)
        .fetch_one(&mut *conn)
        .await?)
    }

    /// A live list by id or `default`; `None` when unknown.
    async fn resolve_list(
        conn: &mut PgConnection,
        user_id: &str,
        list_id: &str,
        for_update: bool,
    ) -> Result<Option<ListRow>> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        if list_id == "default" {
            Self::ensure_default_list(conn, user_id).await?;
            return Ok(sqlx::query_as::<_, ListRow>(&format!(
                "SELECT * FROM grocery.shopping_lists WHERE user_id = $1 AND is_default{lock}"
            ))
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?);
        }
        let Ok(id) = Uuid::parse_str(list_id) else {
            return Ok(None);
        };
        if !is_uuid(list_id) {
            return Ok(None);
        }
        Ok(sqlx::query_as::<_, ListRow>(&format!(
            "SELECT * FROM grocery.shopping_lists WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL{lock}"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?)
    }

    async fn require_list(
        conn: &mut PgConnection,
        user_id: &str,
        list_id: &str,
        for_update: bool,
    ) -> Result<ListRow> {
        Self::resolve_list(conn, user_id, list_id, for_update)
            .await?
            .ok_or_else(|| GroceryError::shopping("list_not_found", "No such shopping list"))
    }

    fn clean_text(
        v: Option<&Value>,
        max: usize,
        field: &str,
        required: bool,
    ) -> Result<Option<String>> {
        match v {
            None | Some(Value::Null) => {
                if required {
                    Err(GroceryError::shopping(
                        "bad_request",
                        format!("{field} is required"),
                    ))
                } else {
                    Ok(None)
                }
            }
            Some(Value::String(s)) => {
                let t = s.trim();
                if required && t.is_empty() {
                    return Err(GroceryError::shopping(
                        "bad_request",
                        format!("{field} is required"),
                    ));
                }
                if js_len(t) > max {
                    return Err(GroceryError::shopping(
                        "bad_request",
                        format!("{field} must be at most {max} characters"),
                    ));
                }
                Ok(Some(t.to_owned()))
            }
            Some(_) => Err(GroceryError::shopping(
                "bad_request",
                format!("{field} must be a string"),
            )),
        }
    }

    fn clean_theme(v: Option<&Value>) -> Result<Option<String>> {
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if THEMES.contains(&s.as_str()) => Ok(Some(s.clone())),
            Some(_) => Err(GroceryError::shopping(
                "bad_request",
                format!("theme must be one of {}", THEMES.join(", ")),
            )),
        }
    }

    // ------------------------------------------------------------ lists

    pub async fn list_lists(&self, user_id: &str) -> Result<Vec<Value>> {
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        Self::ensure_default_list(&mut conn, user_id).await?;
        let rows = sqlx::query_as::<_, ListRow>(
            "SELECT l.*, count(i.id) FILTER (WHERE i.status = 'needed') AS needed, count(i.id) FILTER (WHERE i.status = 'recent') AS recent \
               FROM grocery.shopping_lists l LEFT JOIN grocery.shopping_list_items i ON i.list_id = l.id \
              WHERE l.user_id = $1 AND l.deleted_at IS NULL GROUP BY l.id ORDER BY l.is_default DESC, l.position, l.created_at",
        )
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows.iter().map(Self::public_list).collect())
    }

    /// `body`: `{name, theme?}`.
    pub async fn create_list(&self, user_id: &str, body: &Value) -> Result<Value> {
        let name =
            Self::clean_text(body.get("name"), MAX_LIST_NAME, "name", true)?.unwrap_or_default();
        let theme = Self::clean_theme(body.get("theme"))?.unwrap_or_else(|| "home".into());
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        Self::ensure_default_list(&mut conn, user_id).await?;
        let row = sqlx::query_as::<_, ListRow>(
            "INSERT INTO grocery.shopping_lists (user_id, name, theme, position) \
             VALUES ($1, $2, $3, (SELECT COALESCE(max(position), 0) + 1 FROM grocery.shopping_lists WHERE user_id = $1)) \
             RETURNING *, 0::bigint AS needed, 0::bigint AS recent",
        )
        .bind(user_id)
        .bind(name)
        .bind(theme)
        .fetch_one(&mut *conn)
        .await?;
        Ok(Self::public_list(&row))
    }

    /// `body`: `{name?, theme?}` (bumps the revision).
    pub async fn update_list(&self, user_id: &str, list_id: &str, body: &Value) -> Result<Value> {
        let name = match body.get("name") {
            None => None,
            v => Self::clean_text(v, MAX_LIST_NAME, "name", true)?,
        };
        let theme =
            match body.get("theme") {
                None => None,
                v => Some(Self::clean_theme(v)?.ok_or_else(|| {
                    GroceryError::shopping("bad_request", "theme must be a string")
                })?),
            };
        self.ready().await?;
        let mut tx = self.pool.begin().await?;
        let list = Self::require_list(&mut tx, user_id, list_id, true).await?;
        let row = sqlx::query_as::<_, ListRow>(
            "UPDATE grocery.shopping_lists SET name = COALESCE($2, name), theme = COALESCE($3, theme), \
             revision = revision + 1, updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(list.id)
        .bind(name)
        .bind(theme)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Self::public_list(&row))
    }

    pub async fn delete_list(&self, user_id: &str, list_id: &str) -> Result<Value> {
        self.ready().await?;
        let mut tx = self.pool.begin().await?;
        let list = Self::require_list(&mut tx, user_id, list_id, true).await?;
        if list.is_default {
            return Err(GroceryError::shopping(
                "default_list",
                "The default list cannot be deleted",
            ));
        }
        let revision = list.revision + 1;
        sqlx::query("UPDATE grocery.shopping_list_items SET status = 'removed', rev = $2, updated_at = now() WHERE list_id = $1 AND status <> 'removed'")
            .bind(list.id)
            .bind(revision)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE grocery.shopping_lists SET deleted_at = now(), revision = $2, updated_at = now() WHERE id = $1")
            .bind(list.id)
            .bind(revision)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(json!({"id": list.id.to_string(), "deleted": true}))
    }

    async fn state_for(
        &self,
        conn: &mut PgConnection,
        list: &ListRow,
        revision: i64,
    ) -> Result<(Value, i64, Vec<Value>)> {
        let rows = sqlx::query_as::<_, ItemRow>(&format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status IN ('needed', 'recent') \
             ORDER BY (i.status = 'needed') DESC, i.position NULLS LAST, i.created_at, i.id"
        ))
        .bind(list.id)
        .fetch_all(&mut *conn)
        .await?;
        Ok((
            Self::short_list(list),
            revision,
            rows.iter().map(|r| self.public_item(r)).collect(),
        ))
    }

    /// `{list, revision, items}`: needed items first (by position), then recent.
    pub async fn list_state(&self, user_id: &str, list_id: &str) -> Result<Value> {
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        let list = Self::require_list(&mut conn, user_id, list_id, false).await?;
        let (l, revision, items) = self.state_for(&mut conn, &list, list.revision).await?;
        Ok(json!({"list": l, "revision": revision, "items": items}))
    }

    // ------------------------------------------------------------ ops

    fn validate_ops(ops: &Value) -> Result<&Vec<Value>> {
        let arr = ops.as_array().filter(|a| !a.is_empty()).ok_or_else(|| {
            GroceryError::shopping("bad_request", "ops must be a non-empty array")
        })?;
        if arr.len() > MAX_OPS_PER_BATCH {
            return Err(GroceryError::shopping(
                "bad_request",
                format!("at most {MAX_OPS_PER_BATCH} ops per batch"),
            ));
        }
        for op in arr {
            if !op.is_object() {
                return Err(GroceryError::shopping(
                    "bad_request",
                    "each op must be an object",
                ));
            }
            if !str_field(op, "opId").is_some_and(is_uuid) {
                return Err(GroceryError::shopping(
                    "bad_request",
                    "each op needs a uuid opId",
                ));
            }
            match str_field(op, "type") {
                Some(t) if OP_TYPES.contains(&t) => {}
                _ => {
                    return Err(GroceryError::shopping(
                        "bad_request",
                        format!("unknown op type {}", op.get("type").unwrap_or(&Value::Null)),
                    ))
                }
            }
        }
        Ok(arr)
    }

    /// `(catalogId, displayName)` an add op would match.
    fn add_target(&self, op: &Value) -> OpResult<(Option<String>, String)> {
        let name = op_text(op.get("name"), MAX_NAME, false)?;
        let explicit = op_text(op.get("catalogId"), MAX_CATALOG_ID, false)?;
        if name.is_none() && explicit.is_none() {
            return Err(OpError::Rejected("missing_field"));
        }
        let idx = self.catalog.index();
        let entry = match &explicit {
            Some(id) => idx.item(id),
            None => idx.find(name.as_deref().unwrap_or("")),
        };
        let catalog_id = explicit.clone().or_else(|| entry.map(|e| e.id.clone()));
        let display = name
            .or_else(|| entry.and_then(|e| e.display_name()))
            .or(explicit)
            .unwrap_or_default();
        Ok((catalog_id, display))
    }

    async fn find_active_by_key(
        conn: &mut PgConnection,
        list_id: Uuid,
        catalog_id: Option<&str>,
        display: &str,
        for_update: bool,
    ) -> std::result::Result<Option<ItemRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status <> 'removed' \
               AND (COALESCE(i.catalog_item_id, i.name_key) = COALESCE($2, {k}) OR (i.catalog_item_id IS NULL AND i.name_key = {k})) \
             ORDER BY (i.catalog_item_id IS NOT NULL) DESC LIMIT 1{lock}",
            k = name_key_sql("$3"),
            lock = if for_update { " FOR UPDATE" } else { "" }
        );
        sqlx::query_as::<_, ItemRow>(&sql)
            .bind(list_id)
            .bind(catalog_id)
            .bind(display)
            .fetch_optional(&mut *conn)
            .await
    }

    async fn item_in_list(
        conn: &mut PgConnection,
        list_id: Uuid,
        item_id: Option<&str>,
    ) -> OpResult<ItemRow> {
        let Some(id) = item_id
            .filter(|s| is_uuid(s))
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Err(OpError::Rejected("item_not_found"));
        };
        sqlx::query_as::<_, ItemRow>(&format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.id = $1 AND i.list_id = $2 FOR UPDATE"
        ))
        .bind(id)
        .bind(list_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(OpError::Rejected("item_not_found"))
    }

    async fn next_position(
        conn: &mut PgConnection,
        list_id: Uuid,
    ) -> std::result::Result<i32, sqlx::Error> {
        let row = sqlx::query("SELECT (COALESCE(max(position), 0) + 1)::int AS position FROM grocery.shopping_list_items WHERE list_id = $1")
            .bind(list_id)
            .fetch_one(&mut *conn)
            .await?;
        row.try_get::<i32, _>("position")
    }

    /// Ops that would overwrite changes made after a stale `expectedRevision`.
    async fn find_conflicts(
        &self,
        conn: &mut PgConnection,
        list: &ListRow,
        ops: &[&Value],
        expected: i64,
    ) -> Result<Vec<String>> {
        let mut conflicts = Vec::new();
        for op in ops {
            let kind = str_field(op, "type").unwrap_or("");
            if kind == "clear_recent" {
                continue;
            }
            let rev = if kind == "add" {
                let overwrites = ["spec", "quantity", "unit", "section"]
                    .iter()
                    .any(|k| op.get(*k).is_some_and(|v| !v.is_null()));
                if !overwrites {
                    continue;
                }
                let Ok((cid, display)) = self.add_target(op) else {
                    continue;
                };
                Self::find_active_by_key(conn, list.id, cid.as_deref(), &display, false)
                    .await?
                    .map(|r| r.rev)
            } else if let Some(id) = str_field(op, "itemId")
                .filter(|s| is_uuid(s))
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                sqlx::query(
                    "SELECT rev FROM grocery.shopping_list_items WHERE id = $1 AND list_id = $2",
                )
                .bind(id)
                .bind(list.id)
                .fetch_optional(&mut *conn)
                .await?
                .map(|r| r.get::<i64, _>("rev"))
            } else {
                None
            };
            if rev.is_some_and(|r| r > expected) {
                conflicts.push(str_field(op, "opId").unwrap_or("").to_owned());
            }
        }
        Ok(conflicts)
    }

    async fn touch(
        conn: &mut PgConnection,
        ctx: &Ctx<'_>,
        item_id: Uuid,
        assignments: &str,
        rev: Option<i64>,
    ) -> std::result::Result<(), sqlx::Error> {
        let sets = if assignments.is_empty() {
            String::new()
        } else {
            format!("{assignments}, ")
        };
        sqlx::query(&format!(
            "UPDATE grocery.shopping_list_items SET {sets}rev = $2, modified_by = $3, updated_at = now() WHERE id = $1"
        ))
        .bind(item_id)
        .bind(rev.unwrap_or(ctx.rev))
        .bind(ctx.modified_by)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    async fn target_list(
        conn: &mut PgConnection,
        ctx: &mut Ctx<'_>,
        to: Option<&str>,
    ) -> OpResult<(Uuid, i64)> {
        let Some(to) = to else {
            return Err(OpError::Rejected("list_not_found"));
        };
        let target = Self::resolve_list(conn, ctx.user_id, to, true)
            .await
            .map_err(|e| match e {
                GroceryError::Db(e) => OpError::Db(e),
                GroceryError::Unavailable(m) => OpError::Db(sqlx::Error::Protocol(m)),
                GroceryError::Shopping { .. } => OpError::Rejected("list_not_found"),
            })?
            .ok_or(OpError::Rejected("list_not_found"))?;
        let entry = ctx.targets.entry(target.id).or_insert(Target {
            id: target.id,
            next_rev: target.revision + 1,
        });
        Ok((entry.id, entry.next_rev))
    }

    async fn apply_one(
        &self,
        conn: &mut PgConnection,
        ctx: &mut Ctx<'_>,
        op: &Value,
    ) -> OpResult<Option<Uuid>> {
        let list_id = ctx.list.id;
        let kind = str_field(op, "type").unwrap_or("");
        match kind {
            "add" => {
                let (catalog_id, display) = self.add_target(op)?;
                let spec = op_text(op.get("spec"), MAX_SPEC, false)?;
                let quantity = op_quantity(op.get("quantity"))?;
                let unit = op_text(op.get("unit"), MAX_UNIT, false)?;
                let section = op_text(op.get("section"), MAX_SECTION, false)?;
                if let Some(existing) =
                    Self::find_active_by_key(conn, list_id, catalog_id.as_deref(), &display, true)
                        .await?
                {
                    // Re-adding moves a recent item back to needed; one active row per key.
                    sqlx::query(
                        "UPDATE grocery.shopping_list_items SET status = 'needed', purchased_at = NULL, \
                         catalog_item_id = COALESCE(catalog_item_id, $4), specification = COALESCE($5, specification), \
                         quantity = COALESCE($6::numeric, quantity), unit = COALESCE($7, unit), \
                         section_override = COALESCE($8, section_override), rev = $2, modified_by = $3, updated_at = now() WHERE id = $1",
                    )
                    .bind(existing.id)
                    .bind(ctx.rev)
                    .bind(ctx.modified_by)
                    .bind(&catalog_id)
                    .bind(&spec)
                    .bind(quantity)
                    .bind(&unit)
                    .bind(&section)
                    .execute(&mut *conn)
                    .await?;
                    return Ok(Some(existing.id));
                }
                let product = resolve_canonical_product(&display);
                sqlx::query(
                    "INSERT INTO grocery.canonical_products (id, display_name, category, aliases) VALUES ($1, $2, $3, $4::jsonb) ON CONFLICT (id) DO NOTHING",
                )
                .bind(&product.id)
                .bind(if product.name.is_empty() { &display } else { &product.name })
                .bind(&product.category)
                .bind(serde_json::to_string(&product.aliases).unwrap_or_else(|_| "[]".into()))
                .execute(&mut *conn)
                .await?;
                let position = Self::next_position(conn, list_id).await?;
                let row = sqlx::query(
                    "INSERT INTO grocery.shopping_list_items (user_id, list_id, product_id, raw_name, status, catalog_item_id, specification, \
                     quantity, unit, section_override, position, rev, modified_by) \
                     VALUES ($1, $2, $3, $4, 'needed', $5, $6, $7::numeric, $8, $9, $10, $11, $12) RETURNING id",
                )
                .bind(ctx.user_id)
                .bind(list_id)
                .bind(&product.id)
                .bind(&display)
                .bind(&catalog_id)
                .bind(&spec)
                .bind(quantity)
                .bind(&unit)
                .bind(&section)
                .bind(position)
                .bind(ctx.rev)
                .bind(ctx.modified_by)
                .fetch_one(&mut *conn)
                .await?;
                Ok(Some(row.get::<Uuid, _>("id")))
            }
            "need" | "purchase" | "remove" => {
                let item = Self::item_in_list(conn, list_id, str_field(op, "itemId")).await?;
                // Deleted stays deleted.
                if item.status == "removed" {
                    return Ok(Some(item.id));
                }
                let sets = match kind {
                    "need" => "status = 'needed', purchased_at = NULL",
                    "purchase" => "status = 'recent', purchased_at = now()",
                    _ => "status = 'removed'",
                };
                Self::touch(conn, ctx, item.id, sets, None).await?;
                Ok(Some(item.id))
            }
            "set_spec" | "set_quantity" | "set_section" | "rename" => {
                let item = Self::item_in_list(conn, list_id, str_field(op, "itemId")).await?;
                if item.status == "removed" {
                    return Ok(Some(item.id));
                }
                let base = "rev = $2, modified_by = $3, updated_at = now() WHERE id = $1";
                match kind {
                    "set_spec" => {
                        let spec = op_text(op.get("spec"), MAX_SPEC, false)?;
                        sqlx::query(&format!(
                            "UPDATE grocery.shopping_list_items SET specification = $4, {base}"
                        ))
                        .bind(item.id)
                        .bind(ctx.rev)
                        .bind(ctx.modified_by)
                        .bind(spec)
                        .execute(&mut *conn)
                        .await?;
                    }
                    "set_quantity" => {
                        let q = op_quantity(op.get("quantity"))?;
                        let unit = op_text(op.get("unit"), MAX_UNIT, false)?;
                        sqlx::query(&format!("UPDATE grocery.shopping_list_items SET quantity = $4::numeric, unit = $5, {base}"))
                            .bind(item.id).bind(ctx.rev).bind(ctx.modified_by).bind(q).bind(unit)
                            .execute(&mut *conn).await?;
                    }
                    "set_section" => {
                        let section = op_text(op.get("section"), MAX_SECTION, false)?;
                        sqlx::query(&format!(
                            "UPDATE grocery.shopping_list_items SET section_override = $4, {base}"
                        ))
                        .bind(item.id)
                        .bind(ctx.rev)
                        .bind(ctx.modified_by)
                        .bind(section)
                        .execute(&mut *conn)
                        .await?;
                    }
                    _ => {
                        let name = op_text(op.get("name"), MAX_NAME, true)?;
                        sqlx::query(&format!(
                            "UPDATE grocery.shopping_list_items SET raw_name = $4, {base}"
                        ))
                        .bind(item.id)
                        .bind(ctx.rev)
                        .bind(ctx.modified_by)
                        .bind(name)
                        .execute(&mut *conn)
                        .await?;
                    }
                }
                Ok(Some(item.id))
            }
            "reorder" => {
                let item = Self::item_in_list(conn, list_id, str_field(op, "itemId")).await?;
                let position = op.get("position").map(js_to_number).unwrap_or(f64::NAN);
                if !position.is_finite()
                    || position.fract() != 0.0
                    || !(0.0..=1e6).contains(&position)
                {
                    return Err(OpError::Rejected("invalid_position"));
                }
                if item.status == "removed" {
                    return Ok(Some(item.id));
                }
                let position = position as i32;
                sqlx::query(
                    "UPDATE grocery.shopping_list_items SET position = position + 1, rev = $4, updated_at = now() \
                     WHERE list_id = $1 AND id <> $2 AND status <> 'removed' AND position >= $3",
                )
                .bind(list_id)
                .bind(item.id)
                .bind(position)
                .bind(ctx.rev)
                .execute(&mut *conn)
                .await?;
                sqlx::query("UPDATE grocery.shopping_list_items SET position = $4, rev = $2, modified_by = $3, updated_at = now() WHERE id = $1")
                    .bind(item.id).bind(ctx.rev).bind(ctx.modified_by).bind(position)
                    .execute(&mut *conn).await?;
                Ok(Some(item.id))
            }
            "move" => {
                let item = Self::item_in_list(conn, list_id, str_field(op, "itemId")).await?;
                if item.status == "removed" {
                    return Ok(Some(item.id));
                }
                let to = str_field(op, "toListId");
                let to_self = to == Some(list_id.to_string().as_str())
                    || to.is_some_and(|t| Uuid::parse_str(t).ok() == Some(list_id) && is_uuid(t))
                    || (to == Some("default") && ctx.list.is_default);
                if to_self {
                    return Err(OpError::Rejected("same_list"));
                }
                let (target_id, target_rev) = Self::target_list(conn, ctx, to).await?;
                if let Some(clash) = Self::find_active_by_key(
                    conn,
                    target_id,
                    item.catalog_item_id.as_deref(),
                    &item.raw_name,
                    true,
                )
                .await?
                {
                    // Already on the target list: merge into that row.
                    if item.status == "needed" {
                        Self::touch(
                            conn,
                            ctx,
                            clash.id,
                            "status = 'needed', purchased_at = NULL",
                            Some(target_rev),
                        )
                        .await?;
                    }
                    Self::touch(conn, ctx, item.id, "status = 'removed'", None).await?;
                    return Ok(Some(clash.id));
                }
                let position = Self::next_position(conn, target_id).await?;
                sqlx::query("UPDATE grocery.shopping_list_items SET list_id = $4, position = $5, rev = $2, modified_by = $3, updated_at = now() WHERE id = $1")
                    .bind(item.id).bind(target_rev).bind(ctx.modified_by).bind(target_id).bind(position)
                    .execute(&mut *conn).await?;
                Ok(Some(item.id))
            }
            "clear_recent" => {
                sqlx::query(
                    "UPDATE grocery.shopping_list_items SET status = 'removed', rev = $2, modified_by = $3, updated_at = now() \
                     WHERE list_id = $1 AND status = 'recent'",
                )
                .bind(list_id)
                .bind(ctx.rev)
                .bind(ctx.modified_by)
                .execute(&mut *conn)
                .await?;
                Ok(None)
            }
            _ => Err(OpError::Rejected("unknown_type")),
        }
    }

    /// Apply an op batch to one list in one transaction (see module docs).
    pub async fn apply_ops(
        &self,
        user_id: &str,
        list_id: &str,
        ops: &Value,
        expected_revision: Option<i64>,
        modified_by: &str,
    ) -> Result<OpsOutcome> {
        let ops = Self::validate_ops(ops)?;
        let modifier = if MODIFIERS.contains(&modified_by) {
            modified_by
        } else {
            "lucina"
        };
        self.ready().await?;
        let mut tx = self.pool.begin().await?;
        let list = Self::require_list(&mut tx, user_id, list_id, true).await?;
        let ids: Vec<Uuid> = ops
            .iter()
            .filter_map(|o| str_field(o, "opId").and_then(|s| Uuid::parse_str(s).ok()))
            .collect();
        let seen_rows =
            sqlx::query("SELECT op_id FROM grocery.shopping_ops WHERE op_id = ANY($1::uuid[])")
                .bind(&ids)
                .fetch_all(&mut *tx)
                .await?;
        let mut seen: HashSet<String> = seen_rows
            .iter()
            .map(|r| r.get::<Uuid, _>("op_id").to_string())
            .collect();
        let mut duplicates = Vec::new();
        let mut fresh: Vec<&Value> = Vec::new();
        for op in ops {
            let raw = str_field(op, "opId").unwrap_or("");
            let key = raw.to_lowercase();
            if seen.contains(&key) {
                duplicates.push(raw.to_owned());
            } else {
                seen.insert(key);
                fresh.push(op);
            }
        }
        let revision = list.revision;
        if let Some(expected) = expected_revision.filter(|e| *e < revision) {
            let conflicts = self
                .find_conflicts(&mut tx, &list, &fresh, expected)
                .await?;
            if !conflicts.is_empty() {
                return Err(GroceryError::Shopping {
                    code: "revision_conflict",
                    message: "The list changed since expectedRevision".into(),
                    revision: Some(revision),
                    conflicts,
                });
            }
        }
        let mut ctx = Ctx {
            list: &list,
            user_id,
            modified_by: modifier,
            rev: revision + 1,
            targets: HashMap::new(),
        };
        let mut applied = Vec::new();
        let mut rejected: Vec<(String, &'static str)> = Vec::new();
        let mut item_ids = HashMap::new();
        for op in &fresh {
            let op_id = str_field(op, "opId").unwrap_or("").to_owned();
            sqlx::query("SAVEPOINT shopping_op")
                .execute(&mut *tx)
                .await?;
            match self.apply_one(&mut tx, &mut ctx, op).await {
                Ok(item) => {
                    sqlx::query("RELEASE SAVEPOINT shopping_op")
                        .execute(&mut *tx)
                        .await?;
                    if let Some(id) = item {
                        item_ids.insert(op_id.clone(), id.to_string());
                    }
                    applied.push(op_id);
                }
                Err(err) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT shopping_op")
                        .execute(&mut *tx)
                        .await?;
                    match err {
                        OpError::Rejected(code) => rejected.push((op_id, code)),
                        OpError::Db(sqlx::Error::Database(db))
                            if db.code().as_deref() == Some("23505") =>
                        {
                            rejected.push((op_id, "duplicate_item"))
                        }
                        OpError::Db(e) => return Err(e.into()),
                    }
                }
            }
        }
        let new_revision = if applied.is_empty() {
            revision
        } else {
            revision + 1
        };
        if !applied.is_empty() {
            sqlx::query(
                "UPDATE grocery.shopping_lists SET revision = $2, updated_at = now() WHERE id = $1",
            )
            .bind(list.id)
            .bind(new_revision)
            .execute(&mut *tx)
            .await?;
        }
        for target in ctx.targets.values() {
            sqlx::query(
                "UPDATE grocery.shopping_lists SET revision = $2, updated_at = now() WHERE id = $1",
            )
            .bind(target.id)
            .bind(target.next_rev)
            .execute(&mut *tx)
            .await?;
        }
        let type_of: HashMap<&str, &str> = fresh
            .iter()
            .map(|o| {
                (
                    str_field(o, "opId").unwrap_or(""),
                    str_field(o, "type").unwrap_or(""),
                )
            })
            .collect();
        let receipts = applied
            .iter()
            .map(|id| (id.as_str(), "applied", None))
            .chain(
                rejected
                    .iter()
                    .map(|(id, e)| (id.as_str(), "rejected", Some(*e))),
            );
        for (op_id, outcome, error) in receipts {
            sqlx::query(
                "INSERT INTO grocery.shopping_ops (op_id, list_id, user_id, op_type, outcome, error, revision, modified_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::parse_str(op_id).unwrap_or_default())
            .bind(list.id)
            .bind(user_id)
            .bind(type_of.get(op_id).copied().unwrap_or(""))
            .bind(outcome)
            .bind(error)
            .bind(new_revision)
            .bind(modifier)
            .execute(&mut *tx)
            .await?;
        }
        let (list_json, _, items) = self.state_for(&mut tx, &list, new_revision).await?;
        tx.commit().await?;
        Ok(OpsOutcome {
            revision: new_revision,
            applied,
            duplicates,
            rejected: rejected
                .into_iter()
                .map(|(op_id, error)| json!({"opId": op_id, "error": error}))
                .collect(),
            list: list_json,
            items,
            item_ids,
        })
    }

    // ------------------------------------------------------------ legacy (v1) + chat helpers

    /// Legacy add (`POST /shopping`, chat intents): an `add` op on the default list.
    pub async fn add_item(
        &self,
        user_id: &str,
        product: &Product,
        raw_name: &str,
        modified_by: &str,
    ) -> Result<Value> {
        let op_id = Uuid::new_v4().to_string();
        let name = if raw_name.trim().is_empty() {
            product.name.clone()
        } else {
            raw_name.to_owned()
        };
        let out = self
            .apply_ops(
                user_id,
                "default",
                &json!([{"opId": op_id, "type": "add", "name": name}]),
                None,
                modified_by,
            )
            .await?;
        if let Some(r) = out.rejected.first() {
            return Err(GroceryError::shopping(
                "bad_request",
                format!(
                    "Shopping-list add rejected: {}",
                    r["error"].as_str().unwrap_or("")
                ),
            ));
        }
        let item_id = out.item_ids.get(&op_id).cloned().unwrap_or_default();
        let item = out
            .items
            .iter()
            .find(|i| i["id"] == json!(item_id))
            .cloned()
            .ok_or_else(|| {
                GroceryError::shopping("item_not_found", "Shopping-list item not found")
            })?;
        Ok(json!({
            "id": item["id"],
            "product": product.name,
            "rawName": item["name"],
            "category": product.category,
            "status": legacy_status(item["status"].as_str().unwrap_or("")),
            "createdAt": item["createdAt"],
            "updatedAt": item["updatedAt"],
            "listId": out.list["id"],
        }))
    }

    /// Legacy remove (chat): `remove` ops for matching items on the default list.
    pub async fn remove_items_named(
        &self,
        user_id: &str,
        raw_name: &str,
        modified_by: &str,
    ) -> Result<(usize, String)> {
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        let list = Self::ensure_default_list(&mut conn, user_id).await?;
        let idx = self.catalog.index();
        let entry_id = idx.find(raw_name).map(|e| e.id.clone());
        let product_id = resolve_canonical_product(raw_name).id;
        let rows = sqlx::query(&format!(
            "SELECT i.id FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status <> 'removed' \
               AND (i.name_key = {k} OR ($3::text IS NOT NULL AND i.catalog_item_id = $3) OR ($4::text IS NOT NULL AND i.product_id = $4))",
            k = name_key_sql("$2")
        ))
        .bind(list.id)
        .bind(raw_name)
        .bind(entry_id)
        .bind(product_id)
        .fetch_all(&mut *conn)
        .await?;
        drop(conn);
        if rows.is_empty() {
            return Ok((0, list.id.to_string()));
        }
        let ops: Vec<Value> = rows
            .iter()
            .map(|r| json!({"opId": Uuid::new_v4().to_string(), "type": "remove", "itemId": r.get::<Uuid, _>("id").to_string()}))
            .collect();
        let out = self
            .apply_ops(
                user_id,
                &list.id.to_string(),
                &Value::Array(ops),
                None,
                modified_by,
            )
            .await?;
        Ok((out.applied.len(), list.id.to_string()))
    }

    /// Legacy view of the default list: needed first, then recent ("purchased").
    pub async fn legacy_list(&self, user_id: &str) -> Result<Vec<Value>> {
        let mut conn = self.pool.acquire().await?;
        let list = Self::ensure_default_list(&mut conn, user_id).await?;
        let rows = sqlx::query(
            "SELECT i.id, i.raw_name, i.status, i.created_at, i.updated_at, p.display_name AS product, p.category \
               FROM grocery.shopping_list_items i LEFT JOIN grocery.canonical_products p ON p.id = i.product_id \
              WHERE i.list_id = $1 AND i.status IN ('needed', 'recent') \
              ORDER BY CASE WHEN i.status = 'needed' THEN 0 ELSE 1 END, i.position NULLS LAST, i.created_at ASC",
        )
        .bind(list.id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let raw: String = r.get("raw_name");
                let product: Option<String> = r.get("product");
                let category: Option<String> = r.get("category");
                json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "product": product.filter(|p| !p.is_empty()).unwrap_or_else(|| raw.clone()),
                    "rawName": raw,
                    "category": category.filter(|c| !c.is_empty()).unwrap_or_else(|| "other".into()),
                    "status": legacy_status(&r.get::<String, _>("status")),
                    "createdAt": iso(r.get("created_at")),
                    "updatedAt": iso(r.get("updated_at")),
                })
            })
            .collect())
    }

    /// Legacy `PATCH /shopping/:id` (`needed` | `purchased`): `{id, product, status}`.
    pub async fn legacy_set_status(
        &self,
        user_id: &str,
        item_id: &str,
        status: &str,
        modified_by: &str,
    ) -> Result<Value> {
        if status != "needed" && status != "purchased" {
            return Err(GroceryError::shopping(
                "invalid_status",
                "status must be needed or purchased",
            ));
        }
        self.ready().await?;
        let not_found = || GroceryError::shopping("item_not_found", "No such shopping-list item");
        let mut conn = self.pool.acquire().await?;
        let row = sqlx::query(
            "SELECT i.list_id FROM grocery.shopping_list_items i JOIN grocery.shopping_lists l ON l.id = i.list_id \
              WHERE i.id::text = $1 AND l.user_id = $2 AND l.deleted_at IS NULL AND i.status <> 'removed'",
        )
        .bind(item_id.to_lowercase())
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(not_found)?;
        drop(conn);
        let list_id: Uuid = row.get("list_id");
        let op = json!([{"opId": Uuid::new_v4().to_string(), "type": if status == "needed" { "need" } else { "purchase" }, "itemId": item_id.to_lowercase()}]);
        let out = self
            .apply_ops(user_id, &list_id.to_string(), &op, None, modified_by)
            .await?;
        let item = out
            .items
            .iter()
            .find(|i| i["id"] == json!(item_id.to_lowercase()))
            .ok_or_else(not_found)?;
        Ok(
            json!({"id": item["id"], "product": item["name"], "status": legacy_status(item["status"].as_str().unwrap_or(""))}),
        )
    }

    /// Is the database reachable?
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        conn.ping().await?;
        Ok(())
    }
}

/// v2 `recent` is v1 `purchased`.
pub fn legacy_status(status: &str) -> &str {
    if status == "recent" {
        "purchased"
    } else {
        status
    }
}
