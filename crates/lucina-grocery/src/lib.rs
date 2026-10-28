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
//! - deleted stays deleted; one active row per (list, item object).
//!
//! Item objects (`migrations/003_shopping_items.sql`, [`items`]): every row
//! points to a catalog id or a custom object (`u_<ULID>`) with names in
//! it/en/da; reads take a `lang` for each row's `name`.

pub mod catalog;
pub mod items;
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
pub use items::{message_language, ItemObject, Namer};
use items::{ItemEdit, ItemError, ItemNamer, Translation};
pub use ledger::{Price, StoreRow, Summary};
pub use products::{resolve_canonical_product, Product};

pub const THEMES: [&str; 8] = [
    "home", "grocery", "work", "holiday", "garden", "hardware", "baby", "other",
];
pub const MODIFIERS: [&str; 6] = ["telegram", "discord", "web", "ios", "alexa", "lucina"];
pub const OP_TYPES: [&str; 12] = [
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
    "edit_item",
];
pub const MAX_OPS_PER_BATCH: usize = 200;
pub const DEFAULT_LIST_NAME: &str = "Casa";
const MAX_NAME: usize = 200;
const MAX_LIST_NAME: usize = 100;
const MAX_SPEC: usize = 200;
const MAX_UNIT: usize = 20;
const MAX_SECTION: usize = 50;
const MAX_CATALOG_ID: usize = 100;

const ITEM_COLUMNS: &str = "i.id, i.list_id, i.raw_name, i.status, i.item_id, i.catalog_item_id, i.specification, \
    i.quantity::float8 AS quantity, i.unit, i.section_override, i.icon_key, i.position, i.purchased_at, \
    i.modified_by, i.rev, i.created_at, i.updated_at";

/// A row's own icon as clients get it. An `svg:<key>` icon_key is never sent:
/// installed app builds show the raw string (2026-10-28 regression), so such a
/// row shows its catalog item's emoji, and clients find the art by `catalogId`.
fn client_icon(icon_key: &Option<String>) -> Option<String> {
    icon_key.clone().filter(|i| !i.starts_with("svg:"))
}

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
        /// With `revision_conflict`: the batch's op ids that were applied
        /// before (receipts). The client drops them instead of rebasing them
        /// as new ops (docs/verification.md, S1).
        duplicates: Vec<String>,
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
            duplicates: Vec::new(),
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

impl From<ItemError> for OpError {
    fn from(e: ItemError) -> Self {
        match e {
            ItemError::Code(c) => OpError::Rejected(c),
            ItemError::Db(e) => OpError::Db(e),
        }
    }
}

type OpResult<T> = std::result::Result<T, OpError>;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, sqlx::FromRow)]
struct ListRow {
    id: Uuid,
    user_id: String,
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
    item_id: Option<String>,
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
    /// Add ops that found the object already on the list.
    pub merged: Vec<String>,
    pub lang: &'static str,
}

impl OpsOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "revision": self.revision,
            "applied": self.applied,
            "duplicates": self.duplicates,
            "rejected": self.rejected,
            "merged": self.merged,
            "list": self.list,
            "lang": self.lang,
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
    lang: &'static str,
    targets: HashMap<Uuid, Target>,
    /// Names for new texts, fetched before the transaction (folded text → names).
    translations: HashMap<String, Translation>,
    merged: bool,
    edited: HashSet<String>,
    /// `grocery.shopping_row_aliases` exists (migration 004).
    aliases: bool,
}

pub struct GroceryRepo {
    pool: PgPool,
    catalog: Catalog,
    seeded: tokio::sync::OnceCell<()>,
    aliases: tokio::sync::OnceCell<bool>,
    namer: ItemNamer,
}

/// The client's optimistic row id of an add op: `rowId` (item-objects
/// clients), or a UUID in `itemId` (the first v2 clients). See S2.
pub fn add_row_id(op: &Value) -> Option<Uuid> {
    ["rowId", "itemId"]
        .iter()
        .filter_map(|k| str_field(op, k))
        .find(|s| is_uuid(s))
        .and_then(|s| Uuid::parse_str(s).ok())
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
            aliases: tokio::sync::OnceCell::new(),
            namer: ItemNamer::default(),
        }
    }

    /// The model call that names new custom items in it/en/da (DeepSeek in
    /// lucina-server). Without one, new items keep the typed text everywhere.
    pub fn with_namer(mut self, namer: Namer) -> Self {
        self.namer = ItemNamer::new(Some(namer));
        self
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The current shopping catalog.
    pub fn catalog_index(&self) -> std::sync::Arc<CatalogIndex> {
        self.catalog.index()
    }

    /// Whether migration 004 (`grocery.shopping_row_aliases`) is applied.
    /// Without it an add that merges into an existing row records no alias
    /// (the behaviour before S2's fix); everything else works.
    async fn aliases_ready(&self) -> bool {
        *self
            .aliases
            .get_or_init(|| async {
                let found: std::result::Result<Option<bool>, sqlx::Error> = sqlx::query_scalar(
                    "SELECT to_regclass('grocery.shopping_row_aliases') IS NOT NULL",
                )
                .fetch_optional(&self.pool)
                .await;
                let ok = matches!(found, Ok(Some(true)));
                if !ok {
                    tracing::warn!("grocery.shopping_row_aliases missing: apply migration 004");
                }
                ok
            })
            .await
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

    /// The item object of a row: `item_id` (catalog or custom id) wins; a
    /// row from before item objects is resolved on read through the catalog
    /// (names and aliases in any language); unknown text gets a stand-in.
    fn row_object(
        &self,
        item_id: Option<&str>,
        catalog_item_id: Option<&str>,
        raw_name: &str,
        customs: &HashMap<String, items::CustomRow>,
    ) -> ItemObject {
        let idx = self.catalog.index();
        let id = item_id
            .filter(|s| !s.is_empty())
            .or(catalog_item_id.filter(|s| !s.is_empty()));
        match id {
            Some(id) if items::is_custom_item_id(id) => match customs.get(id) {
                Some(c) => ItemObject::custom(c),
                None => {
                    let mut o = ItemObject::catalog(id, None, raw_name);
                    o.kind = "custom";
                    o.section = Some("other".into());
                    o.needs_translation = true;
                    o
                }
            },
            Some(id) => ItemObject::catalog(id, idx.item(id), raw_name),
            None => items::resolve_catalog_text(&idx, raw_name)
                .map(|(o, _)| o)
                .unwrap_or_else(|| ItemObject::stand_in(raw_name)),
        }
    }

    /// `itemCatalogFields`: `(catalogId, section, icon)` of a row (legacy
    /// shape); a row with no ids is resolved on read like [`Self::row_object`].
    pub fn catalog_fields(
        &self,
        catalog_item_id: &Option<String>,
        raw_name: &str,
        section_override: &Option<String>,
        icon_key: &Option<String>,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let o = self.row_object(None, catalog_item_id.as_deref(), raw_name, &HashMap::new());
        let pick = |own: &Option<String>, from: Option<String>| {
            own.clone().filter(|s| !s.is_empty()).or(from)
        };
        (
            (o.kind == "catalog").then(|| o.id.clone()).flatten(),
            pick(section_override, o.section.clone()),
            pick(&client_icon(icon_key), o.icon.clone()),
        )
    }

    fn public_item(
        &self,
        row: &ItemRow,
        lang: &str,
        customs: &HashMap<String, items::CustomRow>,
    ) -> Value {
        let o = self.row_object(
            row.item_id.as_deref(),
            row.catalog_item_id.as_deref(),
            &row.raw_name,
            customs,
        );
        let pick = |own: &Option<String>, from: &Option<String>| {
            own.clone().filter(|s| !s.is_empty()).or(from.clone())
        };
        json!({
            "id": row.id.to_string(),
            "listId": row.list_id.to_string(),
            "itemId": o.id,
            "catalogId": if o.kind == "catalog" { o.id.clone() } else { None },
            "item": o.to_json(),
            "name": items::local_name(&o.names, items::clean_lang(Some(lang))).unwrap_or_else(|| row.raw_name.clone()),
            "typedName": row.raw_name,
            "spec": opt_str(&row.specification),
            "quantity": row.quantity.map(js_number).unwrap_or(Value::Null),
            "unit": opt_str(&row.unit),
            "section": pick(&row.section_override, &o.section),
            "icon": pick(&client_icon(&row.icon_key), &o.icon),
            "status": row.status,
            "position": row.position,
            "purchasedAt": row.purchased_at.map(iso),
            "createdAt": iso(row.created_at),
            "updatedAt": iso(row.updated_at),
            "updatedBy": opt_str(&row.modified_by),
        })
    }

    /// Public items for rows of one user, custom objects loaded in one query.
    async fn public_items(
        &self,
        conn: &mut PgConnection,
        user_id: &str,
        rows: &[ItemRow],
        lang: &str,
    ) -> Result<Vec<Value>> {
        let ids: Vec<String> = rows.iter().filter_map(|r| r.item_id.clone()).collect();
        let customs = items::load_custom_items(conn, user_id, &ids).await?;
        Ok(rows
            .iter()
            .map(|r| self.public_item(r, lang, &customs))
            .collect())
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
        lang: &str,
    ) -> Result<(Value, i64, Vec<Value>)> {
        let rows = sqlx::query_as::<_, ItemRow>(&format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status IN ('needed', 'recent') \
             ORDER BY (i.status = 'needed') DESC, i.position NULLS LAST, i.created_at, i.id"
        ))
        .bind(list.id)
        .fetch_all(&mut *conn)
        .await?;
        let items = self.public_items(conn, &list.user_id, &rows, lang).await?;
        Ok((Self::short_list(list), revision, items))
    }

    /// Give active rows from before item objects (no `item_id`) their
    /// object: a catalog hit, an existing custom object, or a new custom
    /// object (typed text everywhere, `needsTranslation`; no model call). A
    /// catalog id another active row already has is left alone. In a
    /// transaction each update runs under a savepoint.
    async fn upgrade_legacy_rows(
        &self,
        conn: &mut PgConnection,
        user_id: &str,
        list_id: Uuid,
        in_tx: bool,
    ) -> Result<()> {
        let rows = sqlx::query(
            "SELECT i.id, i.raw_name, i.catalog_item_id FROM grocery.shopping_list_items i \
             WHERE i.list_id = $1 AND i.status <> 'removed' AND i.item_id IS NULL",
        )
        .bind(list_id)
        .fetch_all(&mut *conn)
        .await?;
        let idx = self.catalog.index();
        for r in rows {
            let id: Uuid = r.get("id");
            let raw: String = r.get("raw_name");
            let catalog_id: Option<String> = r.get("catalog_item_id");
            if in_tx {
                sqlx::query("SAVEPOINT shopping_upgrade")
                    .execute(&mut *conn)
                    .await?;
            }
            let step: std::result::Result<(), sqlx::Error> = async {
                let (object_id, is_catalog) = match catalog_id.clone() {
                    Some(c) => (c, true),
                    None => match items::resolve_item_text(conn, &idx, user_id, &raw).await? {
                        Some((o, _)) => (o.id.clone().unwrap_or_default(), o.kind == "catalog"),
                        None => {
                            let o = items::create_custom_item(conn, &idx, user_id, &raw, None, Some("lucina")).await?;
                            (o.id.clone().unwrap_or_default(), false)
                        }
                    },
                };
                sqlx::query(
                    "UPDATE grocery.shopping_list_items SET item_id = $2, catalog_item_id = COALESCE(catalog_item_id, $3) \
                     WHERE id = $1 AND item_id IS NULL",
                )
                .bind(id)
                .bind(&object_id)
                .bind(is_catalog.then_some(&object_id))
                .execute(&mut *conn)
                .await?;
                Ok(())
            }
            .await;
            match step {
                Ok(()) => {
                    if in_tx {
                        sqlx::query("RELEASE SAVEPOINT shopping_upgrade")
                            .execute(&mut *conn)
                            .await?;
                    }
                }
                Err(e) => {
                    if in_tx {
                        sqlx::query("ROLLBACK TO SAVEPOINT shopping_upgrade")
                            .execute(&mut *conn)
                            .await?;
                    }
                    let dup = matches!(&e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"));
                    if !dup {
                        tracing::warn!(error = %e, "shopping row upgrade failed");
                    }
                }
            }
        }
        Ok(())
    }

    /// `{list, revision, lang, items}`: needed items first (by position), then recent; names in the owner's language.
    pub async fn list_state(&self, user_id: &str, list_id: &str) -> Result<Value> {
        self.list_state_in(user_id, list_id, None).await
    }

    /// [`Self::list_state`] with item names in `lang` (it|en|da; default the owner's).
    pub async fn list_state_in(
        &self,
        user_id: &str,
        list_id: &str,
        lang: Option<&str>,
    ) -> Result<Value> {
        self.ready().await?;
        let lang = items::clean_lang(lang);
        let mut conn = self.pool.acquire().await?;
        let list = Self::require_list(&mut conn, user_id, list_id, false).await?;
        if let Err(e) = self
            .upgrade_legacy_rows(&mut conn, user_id, list.id, false)
            .await
        {
            tracing::warn!(error = %e, "shopping row upgrade skipped");
        }
        let (l, revision, items) = self
            .state_for(&mut conn, &list, list.revision, lang)
            .await?;
        Ok(json!({"list": l, "revision": revision, "lang": lang, "items": items}))
    }

    /// The user's custom item objects (search shows them next to the catalog).
    pub async fn custom_items(&self, user_id: &str) -> Result<Vec<Value>> {
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        Ok(items::list_custom_items(&mut conn, user_id).await?)
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

    /// Typed text of an add or rename op (`text`, or the legacy `name`).
    fn op_typed(op: &Value) -> OpResult<Option<String>> {
        match op_text(op.get("text"), MAX_NAME, false)? {
            Some(t) => Ok(Some(t)),
            None => op_text(op.get("name"), MAX_NAME, false),
        }
    }

    /// The object id an add op names (`itemId`, or the legacy `catalogId`);
    /// a UUID in `itemId` is a client's own row id and is ignored.
    fn op_object_id(op: &Value) -> OpResult<Option<String>> {
        let item_id = op_text(op.get("itemId"), MAX_CATALOG_ID, false)?.filter(|s| !is_uuid(s));
        match item_id {
            Some(i) => Ok(Some(i)),
            None => op_text(op.get("catalogId"), MAX_CATALOG_ID, false),
        }
    }

    /// The object an add op means, without creating anything; `None` for
    /// typed text that matches nothing yet.
    async fn find_add_object(
        &self,
        conn: &mut PgConnection,
        user_id: &str,
        op: &Value,
    ) -> OpResult<Option<(ItemObject, Option<String>)>> {
        let typed = Self::op_typed(op)?;
        let explicit = Self::op_object_id(op)?;
        if typed.is_none() && explicit.is_none() {
            return Err(OpError::Rejected("missing_field"));
        }
        let idx = self.catalog.index();
        if let Some(id) = &explicit {
            if let Some(o) = items::object_by_id(conn, &idx, user_id, id).await? {
                return Ok(Some((o, typed)));
            }
            // A catalog id this server does not know: use the text.
            if typed.is_none() {
                return Err(OpError::Rejected("unknown_item"));
            }
        }
        let text = typed.clone().unwrap_or_default();
        Ok(items::resolve_item_text(conn, &idx, user_id, &text)
            .await?
            .map(|(o, _)| (o, typed)))
    }

    /// The active row of a list for an object (item id, or a legacy row by name).
    async fn find_active_by_object(
        conn: &mut PgConnection,
        list_id: Uuid,
        object: &ItemObject,
        typed: &str,
        for_update: bool,
    ) -> std::result::Result<Option<ItemRow>, sqlx::Error> {
        let mut keys: Vec<String> = Vec::new();
        for k in std::iter::once(typed.to_owned())
            .chain(object.id.clone())
            .chain(object.names.values().cloned())
            .map(|s| items::name_key(&s))
        {
            if !k.is_empty() && !keys.contains(&k) {
                keys.push(k);
            }
        }
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status <> 'removed' \
               AND (COALESCE(i.item_id, i.catalog_item_id) = $2 \
                    OR (i.item_id IS NULL AND i.catalog_item_id IS NULL AND i.name_key = ANY($3::text[]))) \
             ORDER BY (i.item_id IS NOT NULL) DESC LIMIT 1{lock}",
            lock = if for_update { " FOR UPDATE" } else { "" }
        );
        sqlx::query_as::<_, ItemRow>(&sql)
            .bind(list_id)
            .bind(&object.id)
            .bind(&keys)
            .fetch_optional(&mut *conn)
            .await
    }

    /// Names for add/rename texts that match no object yet: one model call
    /// per new text, in parallel, BEFORE the transaction.
    async fn prepare_translations(
        &self,
        user_id: &str,
        ops: &[Value],
    ) -> Result<HashMap<String, Translation>> {
        let idx = self.catalog.index();
        let mut texts: Vec<(String, String)> = Vec::new();
        let mut conn = self.pool.acquire().await?;
        for op in ops {
            let kind = str_field(op, "type").unwrap_or("");
            let typed = match kind {
                "add" => Self::op_typed(op).ok().flatten(),
                "rename" => op_text(op.get("name").or(op.get("text")), MAX_NAME, false)
                    .ok()
                    .flatten(),
                _ => None,
            };
            let Some(typed) = typed else { continue };
            let key = catalog::fold(&typed);
            if key.is_empty() || texts.iter().any(|(k, _)| *k == key) {
                continue;
            }
            if kind == "add" {
                if let Ok(Some(id)) = Self::op_object_id(op) {
                    if idx.item(&id).is_some() {
                        continue;
                    }
                }
            }
            if kind == "rename" {
                if let Some(row_id) = str_field(op, "itemId")
                    .filter(|s| is_uuid(s))
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    let current: Option<Option<String>> = sqlx::query_scalar(
                        "SELECT item_id FROM grocery.shopping_list_items WHERE id = $1",
                    )
                    .bind(row_id)
                    .fetch_optional(&mut *conn)
                    .await?;
                    if current
                        .flatten()
                        .is_some_and(|i| items::is_custom_item_id(&i))
                    {
                        continue;
                    }
                }
            }
            if items::resolve_item_text(&mut conn, &idx, user_id, &typed)
                .await?
                .is_some()
            {
                continue;
            }
            texts.push((key, typed));
        }
        drop(conn);
        let mut out = HashMap::new();
        let futures = texts.iter().map(|(key, typed)| {
            let idx = idx.clone();
            async move { (key.clone(), self.namer.translate(&idx, typed).await) }
        });
        for (key, t) in futures_join(futures).await {
            if let Some(t) = t {
                out.insert(key, t);
            }
        }
        Ok(out)
    }

    /// A row or an alias already has this id (any list, any user).
    async fn row_id_taken(
        conn: &mut PgConnection,
        aliases: bool,
        id: Uuid,
    ) -> std::result::Result<bool, sqlx::Error> {
        let sql = if aliases {
            "SELECT EXISTS (SELECT 1 FROM grocery.shopping_list_items WHERE id = $1) \
             OR EXISTS (SELECT 1 FROM grocery.shopping_row_aliases WHERE alias_id = $1)"
        } else {
            "SELECT EXISTS (SELECT 1 FROM grocery.shopping_list_items WHERE id = $1)"
        };
        sqlx::query_scalar(sql).bind(id).fetch_one(&mut *conn).await
    }

    /// The row an op names: its id, or (S2) a client's optimistic row id
    /// recorded as an alias when that client's add merged into another row.
    async fn resolve_row(
        conn: &mut PgConnection,
        aliases: bool,
        id: Uuid,
    ) -> std::result::Result<Uuid, sqlx::Error> {
        if !aliases {
            return Ok(id);
        }
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM grocery.shopping_list_items WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        if exists.is_some() {
            return Ok(id);
        }
        let target: Option<Uuid> = sqlx::query_scalar(
            "SELECT item_id FROM grocery.shopping_row_aliases WHERE alias_id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(target.unwrap_or(id))
    }

    async fn item_in_list(
        conn: &mut PgConnection,
        aliases: bool,
        list_id: Uuid,
        item_id: Option<&str>,
    ) -> OpResult<ItemRow> {
        let Some(id) = item_id
            .filter(|s| is_uuid(s))
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Err(OpError::Rejected("item_not_found"));
        };
        let id = Self::resolve_row(conn, aliases, id).await?;
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
        aliases: bool,
        list: &ListRow,
        ops: &[&Value],
        expected: i64,
    ) -> Result<Vec<String>> {
        let mut conflicts = Vec::new();
        for op in ops {
            let kind = str_field(op, "type").unwrap_or("");
            if kind == "clear_recent" || kind == "edit_item" {
                continue;
            }
            let rev = if kind == "add" {
                let overwrites = ["spec", "quantity", "unit", "section"]
                    .iter()
                    .any(|k| op.get(*k).is_some_and(|v| !v.is_null()));
                if !overwrites {
                    continue;
                }
                let Ok(Some((object, typed))) = self.find_add_object(conn, &list.user_id, op).await
                else {
                    continue;
                };
                Self::find_active_by_object(
                    conn,
                    list.id,
                    &object,
                    typed.as_deref().unwrap_or(""),
                    false,
                )
                .await?
                .map(|r| r.rev)
            } else if let Some(id) = str_field(op, "itemId")
                .filter(|s| is_uuid(s))
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                let id = Self::resolve_row(conn, aliases, id).await?;
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
                let spec = op_text(op.get("spec"), MAX_SPEC, false)?;
                let quantity = op_quantity(op.get("quantity"))?;
                let unit = op_text(op.get("unit"), MAX_UNIT, false)?;
                let section = op_text(op.get("section"), MAX_SECTION, false)?;
                let idx = self.catalog.index();
                // Only `svg:<catalog id>` (an item illustration) is kept; anything else is ignored.
                let icon = op_text(op.get("icon"), MAX_CATALOG_ID + 4, false)?.filter(|i| {
                    i.strip_prefix("svg:")
                        .is_some_and(|k| idx.item(k).is_some())
                });
                let (object, typed_text) = match self.find_add_object(conn, ctx.user_id, op).await?
                {
                    Some(found) => found,
                    None => {
                        let typed = Self::op_typed(op)?.unwrap_or_default();
                        let translation = ctx.translations.get(&catalog::fold(&typed));
                        let (o, _) = items::resolve_or_create_item(
                            conn,
                            &idx,
                            ctx.user_id,
                            &typed,
                            translation,
                            ctx.modified_by,
                        )
                        .await?;
                        (o, Some(typed))
                    }
                };
                let object_id = object.id.clone().unwrap_or_default();
                let typed = typed_text
                    .clone()
                    .or_else(|| items::local_name(&object.names, ctx.lang))
                    .unwrap_or_else(|| object_id.clone());
                let catalog_id = (object.kind == "catalog").then(|| object_id.clone());
                let existing =
                    Self::find_active_by_object(conn, list_id, &object, &typed, true).await?;
                if let Some(t) = &typed_text {
                    items::remember_alias(conn, &idx, ctx.user_id, &object, t).await?;
                }
                if let Some(existing) = existing {
                    // Re-adding moves a recent item back to needed; one active row per object.
                    ctx.merged = true;
                    sqlx::query(
                        "UPDATE grocery.shopping_list_items SET status = 'needed', purchased_at = NULL, \
                         item_id = COALESCE(item_id, $4), catalog_item_id = COALESCE(catalog_item_id, $5), \
                         specification = COALESCE($6, specification), quantity = COALESCE($7::numeric, quantity), \
                         unit = COALESCE($8, unit), section_override = COALESCE($9, section_override), \
                         icon_key = COALESCE($10, icon_key), \
                         rev = $2, modified_by = $3, updated_at = now() WHERE id = $1",
                    )
                    .bind(existing.id)
                    .bind(ctx.rev)
                    .bind(ctx.modified_by)
                    .bind(&object_id)
                    .bind(&catalog_id)
                    .bind(&spec)
                    .bind(quantity)
                    .bind(&unit)
                    .bind(&section)
                    .bind(&icon)
                    .execute(&mut *conn)
                    .await?;
                    // S2: later ops of this client name its optimistic row.
                    if let Some(rid) = add_row_id(op).filter(|r| *r != existing.id) {
                        if ctx.aliases && !Self::row_id_taken(conn, ctx.aliases, rid).await? {
                            sqlx::query(
                                "INSERT INTO grocery.shopping_row_aliases (alias_id, list_id, item_id) \
                                 VALUES ($1, $2, $3) ON CONFLICT (alias_id) DO NOTHING",
                            )
                            .bind(rid)
                            .bind(list_id)
                            .bind(existing.id)
                            .execute(&mut *conn)
                            .await?;
                        }
                    }
                    return Ok(Some(existing.id));
                }
                let product = resolve_canonical_product(&typed);
                sqlx::query(
                    "INSERT INTO grocery.canonical_products (id, display_name, category, aliases) VALUES ($1, $2, $3, $4::jsonb) ON CONFLICT (id) DO NOTHING",
                )
                .bind(&product.id)
                .bind(if product.name.is_empty() { &typed } else { &product.name })
                .bind(&product.category)
                .bind(serde_json::to_string(&product.aliases).unwrap_or_else(|_| "[]".into()))
                .execute(&mut *conn)
                .await?;
                let position = Self::next_position(conn, list_id).await?;
                // S2: the client's fresh row id becomes the row's id, so its
                // later ops (same batch, or after a lost ack) find the row.
                let new_id = match add_row_id(op) {
                    Some(rid) if !Self::row_id_taken(conn, ctx.aliases, rid).await? => rid,
                    _ => Uuid::new_v4(),
                };
                let row = sqlx::query(
                    "INSERT INTO grocery.shopping_list_items (user_id, list_id, product_id, raw_name, status, item_id, catalog_item_id, \
                     specification, quantity, unit, section_override, position, rev, modified_by, id, icon_key) \
                     VALUES ($1, $2, $3, $4, 'needed', $5, $6, $7, $8::numeric, $9, $10, $11, $12, $13, $14, $15) RETURNING id",
                )
                .bind(ctx.user_id)
                .bind(list_id)
                .bind(&product.id)
                .bind(&typed)
                .bind(&object_id)
                .bind(&catalog_id)
                .bind(&spec)
                .bind(quantity)
                .bind(&unit)
                .bind(&section)
                .bind(position)
                .bind(ctx.rev)
                .bind(ctx.modified_by)
                .bind(new_id)
                .bind(&icon)
                .fetch_one(&mut *conn)
                .await?;
                Ok(Some(row.get::<Uuid, _>("id")))
            }
            "edit_item" => {
                // itemId: the object id (item.id); a row id of this list works too.
                let reference =
                    op_text(op.get("itemId"), MAX_CATALOG_ID, true)?.unwrap_or_default();
                let object_id = if is_uuid(&reference) {
                    Self::item_in_list(conn, ctx.aliases, list_id, Some(&reference))
                        .await?
                        .item_id
                        .unwrap_or_default()
                } else {
                    reference
                };
                if !items::is_custom_item_id(&object_id) {
                    let known = self.catalog.index().item(&object_id).is_some();
                    return Err(OpError::Rejected(if known {
                        "not_custom"
                    } else {
                        "item_not_found"
                    }));
                }
                let edit = ItemEdit {
                    names: op.get("names"),
                    section: op.get("section"),
                    icon: op.get("icon"),
                };
                items::edit_custom_item(conn, &self.catalog.index(), ctx.user_id, &object_id, edit)
                    .await?;
                ctx.edited.insert(object_id.clone());
                let row = sqlx::query(
                    "UPDATE grocery.shopping_list_items SET rev = $3, updated_at = now() \
                     WHERE list_id = $1 AND item_id = $2 AND status <> 'removed' RETURNING id",
                )
                .bind(list_id)
                .bind(&object_id)
                .bind(ctx.rev)
                .fetch_optional(&mut *conn)
                .await?;
                Ok(row.map(|r| r.get::<Uuid, _>("id")))
            }
            "rename" => {
                let item =
                    Self::item_in_list(conn, ctx.aliases, list_id, str_field(op, "itemId")).await?;
                if item.status == "removed" {
                    return Ok(Some(item.id));
                }
                let typed =
                    op_text(op.get("name").or(op.get("text")), MAX_NAME, true)?.unwrap_or_default();
                let current = item.item_id.clone().or(item.catalog_item_id.clone());
                let idx = self.catalog.index();
                let hit = items::resolve_item_text(conn, &idx, ctx.user_id, &typed).await?;
                let names_edit = json!({ ctx.lang: typed });
                let same = hit.as_ref().is_some_and(|(o, _)| o.id == current);
                let current_custom = current.as_deref().is_some_and(items::is_custom_item_id);
                if same || (hit.is_none() && current_custom) {
                    // rename = an edit of a custom object's name in the current language.
                    if current_custom {
                        let id = current.clone().unwrap_or_default();
                        items::edit_custom_item(
                            conn,
                            &idx,
                            ctx.user_id,
                            &id,
                            ItemEdit {
                                names: Some(&names_edit),
                                ..Default::default()
                            },
                        )
                        .await?;
                        ctx.edited.insert(id);
                    }
                    Self::touch(conn, ctx, item.id, "", None).await?;
                    return Ok(Some(item.id));
                }
                // Another object's name, or a catalog row renamed to new text: point the row there.
                let object = match hit {
                    Some((o, _)) => o,
                    None => {
                        let translation = ctx.translations.get(&catalog::fold(&typed));
                        items::resolve_or_create_item(
                            conn,
                            &idx,
                            ctx.user_id,
                            &typed,
                            translation,
                            ctx.modified_by,
                        )
                        .await?
                        .0
                    }
                };
                if let Some(clash) =
                    Self::find_active_by_object(conn, list_id, &object, &typed, false).await?
                {
                    if clash.id != item.id {
                        return Err(OpError::Rejected("duplicate_item"));
                    }
                }
                sqlx::query(
                    "UPDATE grocery.shopping_list_items SET item_id = $4, catalog_item_id = $5, raw_name = $6, \
                     rev = $2, modified_by = $3, updated_at = now() WHERE id = $1",
                )
                .bind(item.id)
                .bind(ctx.rev)
                .bind(ctx.modified_by)
                .bind(&object.id)
                .bind((object.kind == "catalog").then(|| object.id.clone()).flatten())
                .bind(&typed)
                .execute(&mut *conn)
                .await?;
                Ok(Some(item.id))
            }
            "need" | "purchase" | "remove" => {
                let item =
                    Self::item_in_list(conn, ctx.aliases, list_id, str_field(op, "itemId")).await?;
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
            "set_spec" | "set_quantity" | "set_section" => {
                let item =
                    Self::item_in_list(conn, ctx.aliases, list_id, str_field(op, "itemId")).await?;
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
                    _ => {}
                }
                Ok(Some(item.id))
            }
            "reorder" => {
                let item =
                    Self::item_in_list(conn, ctx.aliases, list_id, str_field(op, "itemId")).await?;
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
                let item =
                    Self::item_in_list(conn, ctx.aliases, list_id, str_field(op, "itemId")).await?;
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
                let object = ItemObject {
                    id: item.item_id.clone().or(item.catalog_item_id.clone()),
                    names: Default::default(),
                    ..ItemObject::stand_in(&item.raw_name)
                };
                if let Some(clash) =
                    Self::find_active_by_object(conn, target_id, &object, &item.raw_name, true)
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

    /// Apply an op batch to one list in one transaction (see module docs);
    /// item names in the owner's language.
    pub async fn apply_ops(
        &self,
        user_id: &str,
        list_id: &str,
        ops: &Value,
        expected_revision: Option<i64>,
        modified_by: &str,
    ) -> Result<OpsOutcome> {
        self.apply_ops_in(user_id, list_id, ops, expected_revision, modified_by, None)
            .await
    }

    /// [`Self::apply_ops`] with item names (and `rename`) in `lang`.
    pub async fn apply_ops_in(
        &self,
        user_id: &str,
        list_id: &str,
        ops: &Value,
        expected_revision: Option<i64>,
        modified_by: &str,
        lang: Option<&str>,
    ) -> Result<OpsOutcome> {
        let ops = Self::validate_ops(ops)?;
        let modifier = if MODIFIERS.contains(&modified_by) {
            modified_by
        } else {
            "lucina"
        };
        let lang = items::clean_lang(lang);
        self.ready().await?;
        // Model calls for new item names happen here, outside the transaction.
        let translations = self.prepare_translations(user_id, ops).await?;
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
        let mut ctx = Ctx {
            list: &list,
            user_id,
            modified_by: modifier,
            rev: revision + 1,
            lang,
            targets: HashMap::new(),
            translations,
            merged: false,
            edited: HashSet::new(),
            aliases: self.aliases_ready().await,
        };
        if let Some(expected) = expected_revision.filter(|e| *e < revision) {
            let conflicts = self
                .find_conflicts(&mut tx, ctx.aliases, &list, &fresh, expected)
                .await?;
            if !conflicts.is_empty() {
                // Nothing is recorded on a 409, but the ops of this batch that
                // were applied before (a lost ack) are named, so the client
                // drops them instead of rebasing them into new ops (S1).
                return Err(GroceryError::Shopping {
                    code: "revision_conflict",
                    message: "The list changed since expectedRevision".into(),
                    revision: Some(revision),
                    conflicts,
                    duplicates,
                });
            }
        }
        self.upgrade_legacy_rows(&mut tx, user_id, list.id, true)
            .await?;
        let mut applied = Vec::new();
        let mut merged = Vec::new();
        let mut rejected: Vec<(String, &'static str)> = Vec::new();
        let mut item_ids = HashMap::new();
        for op in &fresh {
            let op_id = str_field(op, "opId").unwrap_or("").to_owned();
            sqlx::query("SAVEPOINT shopping_op")
                .execute(&mut *tx)
                .await?;
            ctx.merged = false;
            match self.apply_one(&mut tx, &mut ctx, op).await {
                Ok(item) => {
                    sqlx::query("RELEASE SAVEPOINT shopping_op")
                        .execute(&mut *tx)
                        .await?;
                    if let Some(id) = item {
                        item_ids.insert(op_id.clone(), id.to_string());
                    }
                    if ctx.merged {
                        merged.push(op_id.clone());
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
        if !ctx.edited.is_empty() {
            // An object edit shows on every list that has it: bump those lists too.
            let skip: Vec<Uuid> = std::iter::once(list.id)
                .chain(ctx.targets.keys().copied())
                .collect();
            let edited: Vec<String> = ctx.edited.iter().cloned().collect();
            sqlx::query(
                "WITH bumped AS ( \
                   UPDATE grocery.shopping_lists l SET revision = l.revision + 1, updated_at = now() \
                    WHERE l.user_id = $1 AND l.deleted_at IS NULL AND NOT (l.id = ANY($2::uuid[])) \
                      AND EXISTS (SELECT 1 FROM grocery.shopping_list_items i \
                                   WHERE i.list_id = l.id AND i.status <> 'removed' AND i.item_id = ANY($3::text[])) \
                   RETURNING l.id, l.revision) \
                 UPDATE grocery.shopping_list_items i SET rev = b.revision, updated_at = now() \
                   FROM bumped b WHERE i.list_id = b.id AND i.status <> 'removed' AND i.item_id = ANY($3::text[])",
            )
            .bind(user_id)
            .bind(&skip)
            .bind(&edited)
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
        let (list_json, _, items) = self.state_for(&mut tx, &list, new_revision, lang).await?;
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
            merged,
            lang,
        })
    }

    // ------------------------------------------------------------ legacy (v1) + chat helpers

    /// Legacy add (`POST /shopping`, chat intents): an `add` op on the
    /// default list; `product`/`name` are the item object's name in the
    /// owner's language.
    pub async fn add_item(
        &self,
        user_id: &str,
        product: &Product,
        raw_name: &str,
        modified_by: &str,
    ) -> Result<Value> {
        self.add_item_in(user_id, product, raw_name, modified_by, None)
            .await
    }

    /// [`Self::add_item`] with names in `lang`; `merged` says the object was already listed.
    pub async fn add_item_in(
        &self,
        user_id: &str,
        product: &Product,
        raw_name: &str,
        modified_by: &str,
        lang: Option<&str>,
    ) -> Result<Value> {
        self.add_item_with(user_id, product, raw_name, modified_by, lang, &json!({}))
            .await
    }

    /// [`Self::add_item_in`] with extra `add` op fields (`catalogId`, `section`, `icon`).
    pub async fn add_item_with(
        &self,
        user_id: &str,
        product: &Product,
        raw_name: &str,
        modified_by: &str,
        lang: Option<&str>,
        extra: &Value,
    ) -> Result<Value> {
        let op_id = Uuid::new_v4().to_string();
        let name = if raw_name.trim().is_empty() {
            product.name.clone()
        } else {
            raw_name.to_owned()
        };
        let mut op = json!({"opId": op_id, "type": "add", "text": name});
        for k in ["catalogId", "section", "icon"] {
            if let Some(v) = extra.get(k).filter(|v| v.is_string()) {
                op[k] = v.clone();
            }
        }
        let out = self
            .apply_ops_in(user_id, "default", &json!([op]), None, modified_by, lang)
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
            "product": item["name"],
            "name": item["name"],
            "rawName": item["typedName"],
            "itemId": item["itemId"],
            "item": item["item"],
            "category": product.category,
            "status": legacy_status(item["status"].as_str().unwrap_or("")),
            "createdAt": item["createdAt"],
            "updatedAt": item["updatedAt"],
            "listId": out.list["id"],
            "merged": out.merged.contains(&op_id),
        }))
    }

    /// Legacy remove (chat): `remove` ops for the default list's rows of
    /// the object `raw_name` names (any language), same name or product.
    pub async fn remove_items_named(
        &self,
        user_id: &str,
        raw_name: &str,
        modified_by: &str,
    ) -> Result<(usize, String)> {
        let (n, list, _) = self
            .remove_items_named_in(user_id, raw_name, modified_by, None)
            .await?;
        Ok((n, list))
    }

    /// [`Self::remove_items_named`] plus the removed items' names in `lang`.
    pub async fn remove_items_named_in(
        &self,
        user_id: &str,
        raw_name: &str,
        modified_by: &str,
        lang: Option<&str>,
    ) -> Result<(usize, String, Vec<String>)> {
        self.ready().await?;
        let mut conn = self.pool.acquire().await?;
        let list = Self::ensure_default_list(&mut conn, user_id).await?;
        let idx = self.catalog.index();
        let object_id = items::resolve_item_text(&mut conn, &idx, user_id, raw_name)
            .await?
            .and_then(|(o, _)| o.id);
        let product_id = resolve_canonical_product(raw_name).id;
        let rows = sqlx::query_as::<_, ItemRow>(&format!(
            "SELECT {ITEM_COLUMNS} FROM grocery.shopping_list_items i WHERE i.list_id = $1 AND i.status <> 'removed' \
               AND (i.name_key = {k} OR ($3::text IS NOT NULL AND COALESCE(i.item_id, i.catalog_item_id) = $3) \
                    OR ($4::text IS NOT NULL AND i.product_id = $4))",
            k = name_key_sql("$2")
        ))
        .bind(list.id)
        .bind(raw_name)
        .bind(object_id)
        .bind(product_id)
        .fetch_all(&mut *conn)
        .await?;
        let lang_code = items::clean_lang(lang);
        let mut names: Vec<String> = Vec::new();
        for item in self
            .public_items(&mut conn, user_id, &rows, lang_code)
            .await?
        {
            if let Some(n) = item["name"].as_str() {
                if !names.iter().any(|x| x == n) {
                    names.push(n.to_owned());
                }
            }
        }
        drop(conn);
        if rows.is_empty() {
            return Ok((0, list.id.to_string(), Vec::new()));
        }
        let ops: Vec<Value> = rows
            .iter()
            .map(|r| json!({"opId": Uuid::new_v4().to_string(), "type": "remove", "itemId": r.id.to_string()}))
            .collect();
        let out = self
            .apply_ops_in(
                user_id,
                &list.id.to_string(),
                &Value::Array(ops),
                None,
                modified_by,
                lang,
            )
            .await?;
        Ok((out.applied.len(), list.id.to_string(), names))
    }

    /// Legacy view of the default list: needed first, then recent
    /// ("purchased"); `product`/`name` in the owner's language.
    pub async fn legacy_list(&self, user_id: &str) -> Result<Vec<Value>> {
        self.legacy_list_in(user_id, None).await
    }

    /// [`Self::legacy_list`] with names in `lang`.
    pub async fn legacy_list_in(&self, user_id: &str, lang: Option<&str>) -> Result<Vec<Value>> {
        let state = self.list_state_in(user_id, "default", lang).await?;
        let mut conn = self.pool.acquire().await?;
        let list = Self::ensure_default_list(&mut conn, user_id).await?;
        let rows = sqlx::query(
            "SELECT i.id, p.category FROM grocery.shopping_list_items i LEFT JOIN grocery.canonical_products p ON p.id = i.product_id \
              WHERE i.list_id = $1 AND i.status IN ('needed', 'recent')",
        )
        .bind(list.id)
        .fetch_all(&mut *conn)
        .await?;
        let categories: HashMap<String, String> = rows
            .iter()
            .map(|r| {
                let c: Option<String> = r.get("category");
                (
                    r.get::<Uuid, _>("id").to_string(),
                    c.filter(|c| !c.is_empty())
                        .unwrap_or_else(|| "other".into()),
                )
            })
            .collect();
        Ok(state["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|i| {
                let id = i["id"].as_str().unwrap_or("").to_owned();
                json!({
                    "id": i["id"],
                    "product": i["name"],
                    "name": i["name"],
                    "rawName": i["typedName"],
                    "itemId": i["itemId"],
                    "item": i["item"],
                    "category": categories.get(&id).cloned().unwrap_or_else(|| "other".into()),
                    "catalogId": i["catalogId"],
                    "section": i["section"],
                    "icon": i["icon"],
                    "spec": i["spec"],
                    "status": legacy_status(i["status"].as_str().unwrap_or("")),
                    "createdAt": i["createdAt"],
                    "updatedAt": i["updatedAt"],
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
            json!({"id": item["id"], "product": item["name"], "name": item["name"], "itemId": item["itemId"], "status": legacy_status(item["status"].as_str().unwrap_or(""))}),
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

/// Await futures concurrently (no extra dependency).
async fn futures_join<F, T>(futures: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T>,
{
    let mut pinned: Vec<std::pin::Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut out: Vec<Option<T>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in pinned.iter_mut().enumerate() {
            if out[i].is_some() {
                continue;
            }
            match f.as_mut().poll(cx) {
                std::task::Poll::Ready(v) => out[i] = Some(v),
                std::task::Poll::Pending => pending = true,
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod client_icon_tests {
    #[test]
    fn svg_icon_keys_never_reach_clients() {
        assert_eq!(super::client_icon(&Some("svg:ham".into())), None);
        assert_eq!(super::client_icon(&Some("🍖".into())), Some("🍖".into()));
        assert_eq!(super::client_icon(&None), None);
    }
}
