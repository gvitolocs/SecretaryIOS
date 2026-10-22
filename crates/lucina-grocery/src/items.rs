//! Shopping item objects (Node `src/grocery/shopping-items.js`; SecretaryIOS
//! `docs/shopping-v2.md`, "Item objects"; schema `migrations/003_shopping_items.sql`).
//!
//! Every list row points to an object: a master catalog item (`milk`) or a
//! custom object (`u_<ULID>`) owned by one user, with names in it, en and da.
//! Typed text in any language (catalog names and aliases, custom names and
//! aliases, folded as in `docs/catalog.md`) resolves to the same object.
//! Text that matches nothing becomes a custom object whose other languages
//! come from ONE model call (DeepSeek, cached); without a model the typed
//! text fills every language and the object is marked `needsTranslation`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::postgres::PgConnection;
use uuid::Uuid;

use crate::catalog::{fold, CatalogIndex, CatalogItem};

pub const LANGS: [&str; 3] = ["it", "en", "da"];
pub const MAX_ITEM_NAME: usize = 100;
const MAX_ALIASES: usize = 40;
const MAX_ICON: usize = 16;
const MAX_CACHE: usize = 500;
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CUSTOM_COLUMNS: &str =
    "id, user_id, names::text AS names, aliases::text AS aliases, terms, section, icon, \
    needs_translation, created_by, created_at, updated_at";

/// Per-op failure code, or a database error.
#[derive(Debug)]
pub enum ItemError {
    Code(&'static str),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ItemError {
    fn from(e: sqlx::Error) -> Self {
        ItemError::Db(e)
    }
}

pub type ItemResult<T> = std::result::Result<T, ItemError>;

/// The owner's language: `LUCINA_OWNER_LANG` (it|en|da), default it.
pub fn owner_language() -> &'static str {
    let v = std::env::var("LUCINA_OWNER_LANG").unwrap_or_default();
    LANGS
        .iter()
        .copied()
        .find(|l| *l == v.trim().to_lowercase())
        .unwrap_or("it")
}

/// `value` when it is it|en|da, else the owner's language.
pub fn clean_lang(value: Option<&str>) -> &'static str {
    let v = value.unwrap_or("").trim().to_lowercase();
    LANGS
        .iter()
        .copied()
        .find(|l| *l == v)
        .unwrap_or_else(owner_language)
}

pub fn is_custom_item_id(s: &str) -> bool {
    s.len() == 28 && s.starts_with("u_") && s.as_bytes()[2..].iter().all(|b| CROCKFORD.contains(b))
}

/// `u_` + a ULID (48-bit ms timestamp, 80 random bits, Crockford base32).
pub fn new_custom_item_id() -> String {
    let mut ms = Utc::now().timestamp_millis().max(0) as u64;
    let mut time = [0u8; 10];
    for slot in time.iter_mut().rev() {
        *slot = CROCKFORD[(ms % 32) as usize];
        ms /= 32;
    }
    let bytes = *Uuid::new_v4().as_bytes();
    let mut out = String::from("u_");
    out.push_str(std::str::from_utf8(&time).unwrap_or("0000000000"));
    for b in bytes {
        out.push(CROCKFORD[(b % 32) as usize] as char);
    }
    out
}

/// The 002 `name_key`: lowercase, whitespace runs collapsed, trimmed.
pub fn name_key(s: &str) -> String {
    clean_item_text(s).to_lowercase()
}

pub fn clean_item_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn js_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if js_len(&out) + c.len_utf16() > max {
            break;
        }
        out.push(c);
    }
    out
}

const ARTICLES: [&str; 25] = [
    "the", "a", "an", "some", "il", "lo", "la", "l", "i", "gli", "le", "un", "uno", "una", "del",
    "dello", "della", "dei", "degli", "delle", "dell", "di", "en", "et", "nogle",
];

/// Folded lookup keys, best first: the text, the text without a leading
/// article ("the milk", "l'olio"), and each without a plural `s`.
pub fn lookup_keys(text: &str) -> Vec<String> {
    let folded = fold(text);
    if folded.is_empty() {
        return Vec::new();
    }
    let mut keys = vec![folded.clone()];
    if let Some((first, rest)) = folded.split_once(' ') {
        if (ARTICLES.contains(&first) || first == "noget") && !rest.is_empty() {
            keys.push(rest.to_owned());
        }
    }
    for k in keys.clone() {
        if k.chars().count() > 3 && k.ends_with('s') {
            keys.push(k[..k.len() - 1].to_owned());
        }
    }
    let mut seen = Vec::new();
    for k in keys {
        if !seen.contains(&k) {
            seen.push(k);
        }
    }
    seen
}

/// Every folded term of a custom object: its names and aliases.
pub fn custom_terms(names: &BTreeMap<String, String>, aliases: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in names.values().chain(aliases.iter()) {
        let k = fold(v);
        if !k.is_empty() && !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// `names[lang]`, then the owner's language, then it, en, da.
pub fn local_name(names: &BTreeMap<String, String>, lang: &str) -> Option<String> {
    [lang, owner_language(), "it", "en", "da"]
        .iter()
        .find_map(|l| names.get(*l).filter(|s| !s.is_empty()).cloned())
}

fn same_names(text: &str) -> BTreeMap<String, String> {
    let name = clip(&clean_item_text(text), MAX_ITEM_NAME);
    LANGS
        .iter()
        .map(|l| ((*l).to_owned(), name.clone()))
        .collect()
}

// ---------------------------------------------------------------- objects

/// A grocery.shopping_custom_items row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CustomRow {
    pub id: String,
    pub user_id: String,
    pub names: String,
    pub aliases: String,
    pub terms: Vec<String>,
    pub section: String,
    pub icon: Option<String>,
    pub needs_translation: bool,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CustomRow {
    pub fn names_map(&self) -> BTreeMap<String, String> {
        serde_json::from_str::<BTreeMap<String, Value>>(&self.names)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
            .collect()
    }

    pub fn alias_list(&self) -> Vec<String> {
        serde_json::from_str::<Vec<Value>>(&self.aliases)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    }
}

fn iso(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// A resolved item object (public JSON via [`ItemObject::to_json`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemObject {
    /// `None` only for a stand-in (a row with no object yet).
    pub id: Option<String>,
    pub kind: &'static str,
    pub names: BTreeMap<String, String>,
    pub section: Option<String>,
    pub icon: Option<String>,
    pub aliases: Vec<String>,
    pub needs_translation: bool,
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl ItemObject {
    pub fn is_custom(&self) -> bool {
        self.kind == "custom"
    }

    pub fn catalog(id: &str, entry: Option<&CatalogItem>, fallback: &str) -> Self {
        let names: BTreeMap<String, String> = match entry {
            Some(e) if !e.names.is_empty() => LANGS
                .iter()
                .map(|l| {
                    let v = e
                        .names
                        .get(*l)
                        .cloned()
                        .or_else(|| e.name.clone())
                        .unwrap_or_else(|| {
                            if fallback.is_empty() {
                                id.to_owned()
                            } else {
                                fallback.to_owned()
                            }
                        });
                    ((*l).to_owned(), v)
                })
                .collect(),
            Some(e) => same_names(e.name.as_deref().unwrap_or(if fallback.is_empty() {
                id
            } else {
                fallback
            })),
            None => same_names(if fallback.is_empty() { id } else { fallback }),
        };
        Self {
            id: Some(id.to_owned()),
            kind: "catalog",
            names,
            section: entry.and_then(|e| e.section.clone()),
            icon: entry.and_then(|e| e.icon.clone()),
            aliases: Vec::new(),
            needs_translation: false,
            created_by: None,
            created_at: None,
            updated_at: None,
        }
    }

    pub fn custom(row: &CustomRow) -> Self {
        let raw = row.names_map();
        let names = LANGS
            .iter()
            .map(|l| {
                let v = raw
                    .get(*l)
                    .cloned()
                    .or_else(|| local_name(&raw, l))
                    .unwrap_or_else(|| row.id.clone());
                ((*l).to_owned(), v)
            })
            .collect();
        Self {
            id: Some(row.id.clone()),
            kind: "custom",
            names,
            section: Some(if row.section.is_empty() {
                "other".into()
            } else {
                row.section.clone()
            }),
            icon: row.icon.clone().filter(|s| !s.is_empty()),
            aliases: row.alias_list(),
            needs_translation: row.needs_translation,
            created_by: row.created_by.clone(),
            created_at: Some(iso(row.created_at)),
            updated_at: Some(iso(row.updated_at)),
        }
    }

    /// A custom-shaped stand-in for a row with no object (id null).
    pub fn stand_in(raw_name: &str) -> Self {
        Self {
            id: None,
            kind: "custom",
            names: same_names(raw_name),
            section: None,
            icon: None,
            aliases: Vec::new(),
            needs_translation: true,
            created_by: None,
            created_at: None,
            updated_at: None,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "kind": self.kind,
            "names": self.names,
            "section": self.section,
            "icon": self.icon,
        });
        if self.is_custom() && self.id.is_some() {
            v["aliases"] = json!(self.aliases);
            v["needsTranslation"] = json!(self.needs_translation);
            v["createdBy"] = json!(self.created_by);
            v["createdAt"] = json!(self.created_at);
            v["updatedAt"] = json!(self.updated_at);
        } else if self.is_custom() {
            v["needsTranslation"] = json!(true);
        }
        v
    }
}

/// Custom objects by id (only the user's).
pub async fn load_custom_items(
    conn: &mut PgConnection,
    user_id: &str,
    ids: &[String],
) -> Result<HashMap<String, CustomRow>, sqlx::Error> {
    let wanted: Vec<String> = ids
        .iter()
        .filter(|i| is_custom_item_id(i))
        .cloned()
        .collect();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, CustomRow>(&format!(
        "SELECT {CUSTOM_COLUMNS} FROM grocery.shopping_custom_items WHERE user_id = $1 AND id = ANY($2::text[])"
    ))
    .bind(user_id)
    .bind(&wanted)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id.clone(), r)).collect())
}

/// All of a user's custom objects, oldest first (public JSON).
pub async fn list_custom_items(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query_as::<_, CustomRow>(&format!(
        "SELECT {CUSTOM_COLUMNS} FROM grocery.shopping_custom_items WHERE user_id = $1 ORDER BY created_at, id"
    ))
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| ItemObject::custom(r).to_json())
        .collect())
}

/// The object for an id: a catalog item or one of the user's custom objects.
pub async fn object_by_id(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    id: &str,
) -> Result<Option<ItemObject>, sqlx::Error> {
    if id.is_empty() {
        return Ok(None);
    }
    if is_custom_item_id(id) {
        let map = load_custom_items(conn, user_id, &[id.to_owned()]).await?;
        return Ok(map.get(id).map(ItemObject::custom));
    }
    Ok(idx.item(id).map(|e| ItemObject::catalog(id, Some(e), "")))
}

/// Catalog-only resolution: `(object, via)` with via `name` | `alias`.
pub fn resolve_catalog_text(idx: &CatalogIndex, text: &str) -> Option<(ItemObject, &'static str)> {
    lookup_keys(text).into_iter().find_map(|k| {
        idx.lookup(&k)
            .map(|(e, via)| (ItemObject::catalog(&e.id, Some(e), ""), via))
    })
}

/// Typed text to an object: catalog names and aliases first, then the user's
/// custom objects, for each lookup key in turn.
pub async fn resolve_item_text(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    text: &str,
) -> Result<Option<(ItemObject, &'static str)>, sqlx::Error> {
    let keys = lookup_keys(text);
    if keys.is_empty() {
        return Ok(None);
    }
    let rows = sqlx::query_as::<_, CustomRow>(&format!(
        "SELECT {CUSTOM_COLUMNS} FROM grocery.shopping_custom_items WHERE user_id = $1 AND terms && $2::text[] ORDER BY created_at, id"
    ))
    .bind(user_id)
    .bind(&keys)
    .fetch_all(&mut *conn)
    .await?;
    for k in &keys {
        if let Some((e, via)) = idx.lookup(k) {
            return Ok(Some((ItemObject::catalog(&e.id, Some(e), ""), via)));
        }
        if let Some(r) = rows.iter().find(|r| r.terms.contains(k)) {
            return Ok(Some((ItemObject::custom(r), "custom")));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- names for new objects

/// Section and icon from the words of the text (a pair of words or a word
/// that is a catalog term lends its section and icon); else `other`.
pub fn guess_section_and_icon(idx: &CatalogIndex, text: &str) -> (String, Option<String>) {
    let folded = fold(text);
    let words: Vec<&str> = folded.split(' ').filter(|w| !w.is_empty()).collect();
    let mut candidates: Vec<String> = words.windows(2).map(|w| w.join(" ")).collect();
    candidates.extend(words.iter().map(|w| (*w).to_owned()));
    for k in candidates {
        let hit = idx.lookup(&k).or_else(|| {
            (k.chars().count() > 3 && k.ends_with('s'))
                .then(|| idx.lookup(&k[..k.len() - 1]))
                .flatten()
        });
        if let Some((e, _)) = hit {
            if let Some(section) = &e.section {
                let icon = e
                    .icon
                    .clone()
                    .or_else(|| idx.section(section).and_then(|s| s.icon.clone()));
                return (section.clone(), icon);
            }
        }
    }
    (
        "other".into(),
        idx.section("other").and_then(|s| s.icon.clone()),
    )
}

fn valid_icon(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    if s.is_empty()
        || js_len(s) > MAX_ICON
        || s.chars()
            .any(|c| c.is_alphanumeric() || c.is_whitespace() || c.is_control())
        || !s.chars().any(|c| (c as u32) >= 0x2000)
    {
        return None;
    }
    Some(s.to_owned())
}

fn valid_section(idx: &CatalogIndex, v: Option<&Value>) -> Option<String> {
    let id = v?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    if idx.section(id).is_some() || idx.sections().is_empty() {
        Some(id.to_owned())
    } else {
        None
    }
}

/// Names, section and icon for a new custom object.
#[derive(Debug, Clone, PartialEq)]
pub struct Translation {
    pub names: BTreeMap<String, String>,
    pub section: String,
    pub icon: Option<String>,
    /// A language was missing (filled with the typed text).
    pub partial: bool,
}

/// Check a model's answer; `None` when it has no usable name.
pub fn normalize_translation(idx: &CatalogIndex, raw: &Value, text: &str) -> Option<Translation> {
    if !raw.is_object() {
        return None;
    }
    let typed = clip(&clean_item_text(text), MAX_ITEM_NAME);
    let mut names = BTreeMap::new();
    let mut missing = 0;
    for l in LANGS {
        let v = raw
            .get("names")
            .and_then(|n| n.get(l))
            .and_then(Value::as_str)
            .map(clean_item_text)
            .unwrap_or_default();
        if !v.is_empty() && js_len(&v) <= MAX_ITEM_NAME && !v.contains(['<', '>', '{', '}', '\n']) {
            names.insert(l.to_owned(), v);
        } else {
            names.insert(l.to_owned(), typed.clone());
            missing += 1;
        }
    }
    if missing == LANGS.len() {
        return None;
    }
    let (guess_section, guess_icon) = guess_section_and_icon(idx, text);
    let section = valid_section(idx, raw.get("section")).unwrap_or_else(|| guess_section.clone());
    let icon = valid_icon(raw.get("icon")).or_else(|| {
        if section == guess_section {
            guess_icon.clone()
        } else {
            idx.section(&section)
                .and_then(|s| s.icon.clone())
                .or(guess_icon.clone())
        }
    });
    Some(Translation {
        names,
        section,
        icon,
        partial: missing > 0,
    })
}

/// The `(system, user)` prompt of the naming call.
pub fn translation_prompt(idx: &CatalogIndex, text: &str) -> (String, String) {
    let sections = idx
        .sections()
        .iter()
        .map(|s| {
            format!(
                "{} {} ({})",
                s.id,
                s.icon.clone().unwrap_or_default(),
                s.names
                    .get("en")
                    .cloned()
                    .or_else(|| s.name.clone())
                    .unwrap_or_else(|| s.id.clone())
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let system = "You name items on a household shopping list (groceries and household goods) in Italian, English and Danish. Reply with JSON only.".to_owned();
    let user = [
        format!(
            "Someone typed this item on their shopping list: {}",
            Value::String(clean_item_text(text))
        ),
        "Give its name as a shopping list would show it in each language: capitalised first letter, plural for countable things".into(),
        "(\"Mele\", \"Apples\", \"Æbler\"), a mass noun otherwise (\"Latte\", \"Milk\", \"Mælk\"). Keep brand names as they are. If it is".into(),
        "already a name in one of the languages, keep that wording for that language.".into(),
        "Pick the section id from this list and ONE emoji for the item (Unicode Emoji 15.0 or older; a specific food or object".into(),
        "emoji when one fits, else the section emoji):".into(),
        sections,
        "JSON shape: {\"names\":{\"it\":\"…\",\"en\":\"…\",\"da\":\"…\"},\"section\":\"<section id>\",\"icon\":\"<emoji>\"}".into(),
    ]
    .join("\n");
    (system, user)
}

/// The model call: `(system, user)` → the answer's text, or `None`.
pub type Namer = Arc<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync,
>;

/// The first JSON object in a model answer (code fences tolerated).
fn parse_json_answer(content: &str) -> Option<Value> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    serde_json::from_str(content.get(start..=end)?).ok()
}

fn translation_enabled() -> bool {
    !matches!(
        std::env::var("LUCINA_SHOPPING_TRANSLATE")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "off" | "false" | "0" | "no"
    )
}

/// Names new custom objects: one model call per new text, cached by folded text.
#[derive(Default)]
pub struct ItemNamer {
    namer: Option<Namer>,
    cache: Mutex<HashMap<String, Translation>>,
}

impl std::fmt::Debug for ItemNamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemNamer")
            .field("namer", &self.namer.is_some())
            .finish()
    }
}

impl ItemNamer {
    pub fn new(namer: Option<Namer>) -> Self {
        Self {
            namer,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// `None` when there is no model, it is off (`LUCINA_SHOPPING_TRANSLATE=off`),
    /// times out (`LUCINA_SHOPPING_TRANSLATE_TIMEOUT_MS`, default 12 s) or
    /// answers nothing usable.
    pub async fn translate(&self, idx: &CatalogIndex, text: &str) -> Option<Translation> {
        let key = fold(text);
        if key.is_empty() {
            return None;
        }
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Some(hit.clone());
        }
        let namer = self.namer.as_ref()?;
        if !translation_enabled() {
            return None;
        }
        let timeout = std::env::var("LUCINA_SHOPPING_TRANSLATE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(12_000);
        let (system, user) = translation_prompt(idx, text);
        let answer =
            match tokio::time::timeout(Duration::from_millis(timeout), namer(system, user)).await {
                Ok(a) => a,
                Err(_) => {
                    tracing::warn!("shopping item translation timed out");
                    None
                }
            }?;
        let result = normalize_translation(idx, &parse_json_answer(&answer)?, text)?;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAX_CACHE {
            cache.clear();
        }
        cache.insert(key, result.clone());
        Some(result)
    }
}

// ---------------------------------------------------------------- custom objects

/// The object for typed text, creating a custom one when nothing matches.
/// Runs in a transaction (advisory lock per user and text).
pub async fn resolve_or_create_item(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    text: &str,
    translation: Option<&Translation>,
    created_by: &str,
) -> ItemResult<(ItemObject, bool)> {
    let typed = clip(&clean_item_text(text), MAX_ITEM_NAME);
    let key = fold(&typed);
    if key.is_empty() {
        return Err(ItemError::Code("missing_field"));
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("shopping-item:{user_id}:{key}"))
        .execute(&mut *conn)
        .await?;
    if let Some((object, _)) = resolve_item_text(conn, idx, user_id, &typed).await? {
        return Ok((object, false));
    }
    Ok((
        create_custom_item(conn, idx, user_id, &typed, translation, Some(created_by)).await?,
        true,
    ))
}

pub async fn create_custom_item(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    typed: &str,
    translation: Option<&Translation>,
    created_by: Option<&str>,
) -> Result<ItemObject, sqlx::Error> {
    let (guess_section, guess_icon) = guess_section_and_icon(idx, typed);
    let names = translation
        .map(|t| t.names.clone())
        .unwrap_or_else(|| same_names(typed));
    // The typed text is always kept as an alias, so it still finds the object after a rename.
    let aliases = vec![typed.to_owned()];
    let row = sqlx::query_as::<_, CustomRow>(&format!(
        "INSERT INTO grocery.shopping_custom_items (id, user_id, names, aliases, terms, section, icon, needs_translation, created_by) \
         VALUES ($1, $2, $3::jsonb, $4::jsonb, $5::text[], $6, $7, $8, $9) RETURNING {CUSTOM_COLUMNS}"
    ))
    .bind(new_custom_item_id())
    .bind(user_id)
    .bind(serde_json::to_string(&names).unwrap_or_else(|_| "{}".into()))
    .bind(serde_json::to_string(&aliases).unwrap_or_else(|_| "[]".into()))
    .bind(custom_terms(&names, &aliases))
    .bind(translation.map(|t| t.section.clone()).unwrap_or(guess_section))
    .bind(translation.map(|t| t.icon.clone()).unwrap_or(guess_icon))
    .bind(translation.is_none_or(|t| t.partial))
    .bind(created_by)
    .fetch_one(&mut *conn)
    .await?;
    Ok(ItemObject::custom(&row))
}

/// Remember typed text as an alias of a custom object.
pub async fn remember_alias(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    object: &ItemObject,
    typed: &str,
) -> Result<(), sqlx::Error> {
    let Some(id) = object.id.as_deref().filter(|_| object.is_custom()) else {
        return Ok(());
    };
    let text = clip(&clean_item_text(typed), MAX_ITEM_NAME);
    let key = fold(&text);
    if key.is_empty()
        || custom_terms(&object.names, &object.aliases).contains(&key)
        || object.aliases.len() >= MAX_ALIASES
        || idx.lookup(&key).is_some()
    {
        return Ok(());
    }
    sqlx::query(
        "UPDATE grocery.shopping_custom_items SET aliases = aliases || $3::jsonb, \
         terms = CASE WHEN $4 = ANY(terms) THEN terms ELSE array_append(terms, $4) END, updated_at = now() \
         WHERE id = $1 AND user_id = $2 AND NOT (aliases @> $3::jsonb)",
    )
    .bind(id)
    .bind(user_id)
    .bind(serde_json::to_string(&[&text]).unwrap_or_else(|_| "[]".into()))
    .bind(key)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn edited_names(v: Option<&Value>) -> ItemResult<Option<BTreeMap<String, String>>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let obj = v.as_object().ok_or(ItemError::Code("invalid_field"))?;
    let mut out = BTreeMap::new();
    for (lang, value) in obj {
        if !LANGS.contains(&lang.as_str()) {
            return Err(ItemError::Code("invalid_field"));
        }
        let s = value.as_str().ok_or(ItemError::Code("invalid_field"))?;
        let t = clean_item_text(s);
        if t.is_empty() {
            return Err(ItemError::Code("missing_field"));
        }
        if js_len(&t) > MAX_ITEM_NAME {
            return Err(ItemError::Code("field_too_long"));
        }
        out.insert(lang.clone(), t);
    }
    Ok((!out.is_empty()).then_some(out))
}

/// What an `edit_item` changes; `icon: Some(Value::Null)` or `""` clears it.
#[derive(Debug, Default, Clone)]
pub struct ItemEdit<'a> {
    pub names: Option<&'a Value>,
    pub section: Option<&'a Value>,
    pub icon: Option<&'a Value>,
}

/// Edit a custom object (names subset, catalog section id, one emoji icon).
pub async fn edit_custom_item(
    conn: &mut PgConnection,
    idx: &CatalogIndex,
    user_id: &str,
    id: &str,
    edit: ItemEdit<'_>,
) -> ItemResult<ItemObject> {
    if !is_custom_item_id(id) {
        return Err(ItemError::Code("not_custom"));
    }
    let row = sqlx::query_as::<_, CustomRow>(&format!(
        "SELECT {CUSTOM_COLUMNS} FROM grocery.shopping_custom_items WHERE id = $1 AND user_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ItemError::Code("item_not_found"))?;
    let next_names = edited_names(edit.names)?;
    let next_section = match edit.section.filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(valid_section(idx, Some(v)).ok_or(ItemError::Code("invalid_section"))?),
    };
    let mut next_icon = row.icon.clone();
    match edit.icon {
        None => {}
        Some(Value::Null) => next_icon = None,
        Some(Value::String(s)) if s.is_empty() => next_icon = None,
        Some(v) => next_icon = Some(valid_icon(Some(v)).ok_or(ItemError::Code("invalid_icon"))?),
    }
    if next_names.is_none() && next_section.is_none() && edit.icon.is_none() {
        return Err(ItemError::Code("missing_field"));
    }
    let mut merged = row.names_map();
    if let Some(n) = &next_names {
        let keys: Vec<String> = n
            .values()
            .map(|v| fold(v))
            .filter(|k| !k.is_empty())
            .collect();
        if keys.iter().any(|k| idx.lookup(k).is_some()) {
            return Err(ItemError::Code("name_taken"));
        }
        let clash = sqlx::query(
            "SELECT id FROM grocery.shopping_custom_items WHERE user_id = $1 AND id <> $2 AND terms && $3::text[] LIMIT 1",
        )
        .bind(user_id)
        .bind(id)
        .bind(&keys)
        .fetch_optional(&mut *conn)
        .await?;
        if clash.is_some() {
            return Err(ItemError::Code("name_taken"));
        }
        merged.extend(n.clone());
    }
    let aliases = row.alias_list();
    let updated = sqlx::query_as::<_, CustomRow>(&format!(
        "UPDATE grocery.shopping_custom_items SET names = $3::jsonb, terms = $4::text[], section = COALESCE($5, section), icon = $6, \
         needs_translation = needs_translation AND NOT $7, updated_at = now() WHERE id = $1 AND user_id = $2 RETURNING {CUSTOM_COLUMNS}"
    ))
    .bind(id)
    .bind(user_id)
    .bind(serde_json::to_string(&merged).unwrap_or_else(|_| "{}".into()))
    .bind(custom_terms(&merged, &aliases))
    .bind(next_section)
    .bind(next_icon)
    .bind(next_names.is_some())
    .fetch_one(&mut *conn)
    .await?;
    Ok(ItemObject::custom(&updated))
}

// ---------------------------------------------------------------- message language

const DA_WORDS: [&str; 20] = [
    "tilføj",
    "tilfoej",
    "indkøbsliste",
    "indkøbslisten",
    "listen",
    "mangler",
    "købe",
    "koebe",
    "skal",
    "fjern",
    "hvad",
    "står",
    "jeg",
    "vi",
    "og",
    "på",
    "har",
    "til",
    "fra",
    "ikke",
];
const IT_WORDS: [&str; 32] = [
    "aggiungi", "metti", "togli", "rimuovi", "lista", "spesa", "serve", "servono", "comprare",
    "compra", "prendi", "finito", "finita", "finiti", "ho", "abbiamo", "il", "lo", "gli", "della",
    "alla", "nella", "dalla", "cosa", "mostra", "dimmi", "anche", "per", "con", "del", "dei",
    "delle",
];
const EN_WORDS: [&str; 20] = [
    "add", "put", "remove", "take", "shopping", "grocery", "list", "need", "buy", "the", "my",
    "we", "out", "of", "show", "what", "on", "to", "from", "please",
];
const IT_MARKERS: [&str; 30] = [
    "che", "non", "per", "con", "sono", "sei", "hai", "hanno", "ciao", "grazie", "anche", "questo",
    "questa", "della", "delle", "degli", "dei", "nel", "nella", "alla", "una", "uno", "gli",
    "puoi", "posso", "voglio", "vorrei", "mio", "mia", "cosa",
];
const EN_MARKERS: [&str; 30] = [
    "the", "and", "you", "your", "my", "that", "this", "with", "have", "has", "can", "could",
    "would", "what", "when", "where", "why", "how", "there", "here", "about", "from", "for", "not",
    "but", "is", "are", "was", "want", "need",
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Node `detectReplyLanguage` (stopwords, accents break near-ties): it|en or None.
fn detect_it_en(text: &str) -> Option<&'static str> {
    let w = words(text);
    if text.trim().chars().count() < 3 {
        return None;
    }
    let mut it = w
        .iter()
        .filter(|x| IT_MARKERS.contains(&x.as_str()))
        .count();
    let en = w
        .iter()
        .filter(|x| EN_MARKERS.contains(&x.as_str()))
        .count();
    if text.chars().any(|c| "àèéìòùÀÈÉÌÒÙ".contains(c)) {
        it += 1;
    }
    match it.cmp(&en) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some("it"),
        std::cmp::Ordering::Less => Some("en"),
    }
}

/// The language of a short shopping message (it|en|da): keyword hits per
/// language, then the general it/en detector, then `fallback`.
pub fn message_language(text: &str, fallback: Option<&str>) -> &'static str {
    let w = words(text);
    let count = |set: &[&str]| w.iter().filter(|x| set.contains(&x.as_str())).count();
    let mut scores = [
        (
            "da",
            count(&DA_WORDS) + usize::from(text.to_lowercase().contains(['æ', 'ø', 'å'])),
        ),
        (
            "it",
            count(&IT_WORDS) + text.to_lowercase().matches("c'è").count(),
        ),
        ("en", count(&EN_WORDS)),
    ];
    scores.sort_by_key(|s| std::cmp::Reverse(s.1));
    if scores[0].1 > 0 && scores[0].1 > scores[1].1 {
        return scores[0].0;
    }
    detect_it_en(text).unwrap_or_else(|| clean_lang(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keys_and_language() {
        let id = new_custom_item_id();
        assert!(is_custom_item_id(&id), "{id}");
        assert!(!is_custom_item_id("milk"));
        assert_eq!(lookup_keys("L'olio"), vec!["l olio", "olio"]);
        assert_eq!(
            lookup_keys("the apples"),
            vec!["the apples", "apples", "the apple", "apple"]
        );
        assert_eq!(message_language("aggiungi latte alla lista", None), "it");
        assert_eq!(message_language("add milk to my shopping list", None), "en");
        assert_eq!(
            message_language("tilføj mælk til indkøbslisten", None),
            "da"
        );
        assert_eq!(message_language("vi mangler kaffe", None), "da");
        assert_eq!(message_language("🙂", Some("en")), "en");
        assert_eq!(name_key("  Detersivo   PIATTI "), "detersivo piatti");
    }

    #[test]
    fn translations_are_checked() {
        let idx = CatalogIndex::build(&json!({
            "sections": [{"id": "dairy-eggs", "names": {"en": "Dairy"}, "icon": "🧀", "order": 1},
                         {"id": "household", "names": {"en": "Household"}, "icon": "🧽", "order": 2}],
            "items": [{"id": "milk", "names": {"it": "Latte", "en": "Milk", "da": "Mælk"}, "section": "dairy-eggs", "icon": "🥛"}]
        }));
        assert_eq!(
            guess_section_and_icon(&idx, "latte di avena"),
            ("dairy-eggs".into(), Some("🥛".into()))
        );
        assert!(normalize_translation(&idx, &json!({"names": {}}), "x").is_none());
        let t = normalize_translation(
            &idx,
            &json!({"names": {"it": "Spugne", "en": "Sponges"}, "section": "space", "icon": "sponge"}),
            "spugne",
        )
        .unwrap();
        assert_eq!(t.names["da"], "spugne");
        assert!(t.partial);
        assert_eq!(t.section, "other");
        assert_eq!(
            parse_json_answer("```json\n{\"a\":1}\n```"),
            Some(json!({"a": 1}))
        );
    }
}
