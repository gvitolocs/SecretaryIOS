//! The category decisions in the catalog: `ml_categories`, one row per item.
//!
//! A row says which category the photo is in, who decided (`rules`, `jev`, or `owner`), and what
//! the decision was made from: the embedding model, the [`FEATURES_VERSION`] of the record, the
//! decider's id, and the record itself as JSON (no file names, see `categories::Features`). A row
//! made by a decider is made again when any of the three changes; a row made by the owner is
//! never touched by a decider: [`put_auto`] refuses to replace it in SQL, not only in code.
//!
//! The owner's corrections are labelled examples for a better decider: [`examples`] lists them
//! with the record they were made on and what the machine had said. No foreign key to `items`:
//! this crate does not know that table, and [`forget`] drops a purged item's row.

use rusqlite::{params, Connection, OptionalExtension};

use crate::categories::{Category, DeciderKind, Features};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ml_categories (
    item_id          TEXT PRIMARY KEY,
    category         TEXT NOT NULL,
    confidence       REAL NOT NULL,
    decided_by       TEXT NOT NULL CHECK (decided_by IN ('rules', 'jev', 'owner')),
    decider_id       TEXT NOT NULL,
    model            TEXT NOT NULL,
    features_version INTEGER NOT NULL,
    features_json    TEXT,
    reason           TEXT,
    auto_category    TEXT,
    decided_at       TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ml_categories_category ON ml_categories(category);
";

/// Idempotent: safe on every start.
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(SCHEMA)
}

/// Who made a stored decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecidedBy {
    Rules,
    Jev,
    Owner,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Rules => "rules",
            DecidedBy::Jev => "jev",
            DecidedBy::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<DecidedBy> {
        match s {
            "rules" => Some(DecidedBy::Rules),
            "jev" => Some(DecidedBy::Jev),
            "owner" => Some(DecidedBy::Owner),
            _ => None,
        }
    }
}

impl From<DeciderKind> for DecidedBy {
    fn from(k: DeciderKind) -> Self {
        match k {
            DeciderKind::Rules => DecidedBy::Rules,
            DeciderKind::Jev => DecidedBy::Jev,
        }
    }
}

/// A stored decision.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub item_id: String,
    pub category: Category,
    pub confidence: f32,
    pub decided_by: DecidedBy,
    pub decider_id: String,
    pub model: String,
    pub features_version: u32,
    pub features: Option<Features>,
    pub reason: Option<String>,
    /// For an owner's correction: what the machine had decided (`None` if it had not decided).
    pub auto_category: Option<Category>,
    pub decided_at: String,
}

const COLS: &str =
    "item_id, category, confidence, decided_by, decider_id, model, features_version, \
                    features_json, reason, auto_category, decided_at";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let cat = |s: String| Category::parse(&s);
    let features: Option<String> = r.get(7)?;
    Ok(Row {
        item_id: r.get(0)?,
        category: cat(r.get(1)?).unwrap_or(Category::Photo),
        confidence: r.get::<_, f64>(2)? as f32,
        decided_by: DecidedBy::parse(&r.get::<_, String>(3)?).unwrap_or(DecidedBy::Rules),
        decider_id: r.get(4)?,
        model: r.get(5)?,
        features_version: r.get::<_, i64>(6)? as u32,
        features: features.and_then(|j| serde_json::from_str(&j).ok()),
        reason: r.get(8)?,
        auto_category: r.get::<_, Option<String>>(9)?.and_then(cat),
        decided_at: r.get(10)?,
    })
}

pub fn get(c: &Connection, item_id: &str) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        &format!("SELECT {COLS} FROM ml_categories WHERE item_id = ?1"),
        [item_id],
        from_row,
    )
    .optional()
}

/// What a decider decided about one item.
#[derive(Clone, Debug)]
pub struct NewDecision<'a> {
    pub item_id: &'a str,
    pub category: Category,
    pub confidence: f32,
    pub kind: DeciderKind,
    pub decider_id: &'a str,
    pub model: &'a str,
    pub features: &'a Features,
    pub reason: &'a str,
}

/// Stores an automatic decision, replacing an older one. An owner's row is never replaced: the
/// guard is in the statement. `false` when the item has an owner's row (nothing was written).
pub fn put_auto(c: &Connection, d: &NewDecision, now: &str) -> rusqlite::Result<bool> {
    let json = serde_json::to_string(d.features).unwrap_or_default();
    let n = c.execute(
        "INSERT INTO ml_categories (item_id, category, confidence, decided_by, decider_id, model, \
             features_version, features_json, reason, auto_category, decided_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10) \
         ON CONFLICT(item_id) DO UPDATE SET category = excluded.category, \
             confidence = excluded.confidence, decided_by = excluded.decided_by, \
             decider_id = excluded.decider_id, model = excluded.model, \
             features_version = excluded.features_version, features_json = excluded.features_json, \
             reason = excluded.reason, auto_category = NULL, decided_at = excluded.decided_at \
         WHERE ml_categories.decided_by != 'owner'",
        params![
            d.item_id,
            d.category.as_str(),
            d.confidence as f64,
            DecidedBy::from(d.kind).as_str(),
            d.decider_id,
            d.model,
            d.features.version as i64,
            json,
            d.reason,
            now,
        ],
    )?;
    Ok(n > 0)
}

/// The owner says what the item is. The row keeps the record the machine decided from, and what
/// it said, as a labelled example; `features` fills the record when the machine had not decided
/// yet. Choosing again replaces the owner's earlier choice and keeps the machine's first answer.
pub fn set_owner(
    c: &Connection,
    item_id: &str,
    category: Category,
    features: Option<&Features>,
    now: &str,
) -> rusqlite::Result<Row> {
    let prior = get(c, item_id)?;
    let (model, version, json, auto) = match &prior {
        Some(p) => (
            p.model.clone(),
            p.features_version,
            p.features
                .as_ref()
                .or(features)
                .and_then(|f| serde_json::to_string(f).ok()),
            match p.decided_by {
                DecidedBy::Owner => p.auto_category,
                _ => Some(p.category),
            },
        ),
        None => (
            String::new(),
            features.map_or(0, |f| f.version),
            features.and_then(|f| serde_json::to_string(f).ok()),
            None,
        ),
    };
    c.execute(
        "INSERT OR REPLACE INTO ml_categories (item_id, category, confidence, decided_by, decider_id, \
             model, features_version, features_json, reason, auto_category, decided_at) \
         VALUES (?1, ?2, 1.0, 'owner', 'owner', ?3, ?4, ?5, 'owner', ?6, ?7)",
        params![
            item_id,
            category.as_str(),
            model,
            version as i64,
            json,
            auto.map(Category::as_str),
            now
        ],
    )?;
    Ok(get(c, item_id)?.unwrap_or_else(|| unreachable_row(item_id)))
}

fn unreachable_row(item_id: &str) -> Row {
    Row {
        item_id: item_id.to_string(),
        category: Category::Photo,
        confidence: 1.0,
        decided_by: DecidedBy::Owner,
        decider_id: "owner".into(),
        model: String::new(),
        features_version: 0,
        features: None,
        reason: None,
        auto_category: None,
        decided_at: String::new(),
    }
}

/// Takes the owner's choice back: the item is decided automatically again. `true` if there was one.
pub fn clear_owner(c: &Connection, item_id: &str) -> rusqlite::Result<bool> {
    Ok(c.execute(
        "DELETE FROM ml_categories WHERE item_id = ?1 AND decided_by = 'owner'",
        [item_id],
    )? > 0)
}

/// Drops `item_id`'s decision (it was purged).
pub fn forget(c: &Connection, item_id: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM ml_categories WHERE item_id = ?1", [item_id])?;
    Ok(())
}

/// Drops rows whose item is gone, given the SQL of the live item ids (e.g. `SELECT id FROM items`).
pub fn prune(c: &Connection, live_ids_sql: &str) -> rusqlite::Result<usize> {
    c.execute(
        &format!("DELETE FROM ml_categories WHERE item_id NOT IN ({live_ids_sql})"),
        [],
    )
}

/// One labelled example: the owner's choice, what the machine had said, and the record.
#[derive(Clone, Debug, PartialEq)]
pub struct Example {
    pub item_id: String,
    pub category: Category,
    pub auto_category: Option<Category>,
    pub features: Option<Features>,
    pub model: String,
    pub labelled_at: String,
}

/// The owner's corrections, oldest first, `limit` at a time after the cursor `(labelled_at, item_id)`
/// of the last one seen. The record is the one the machine decided from.
pub fn examples(
    c: &Connection,
    after: Option<(&str, &str)>,
    limit: usize,
) -> rusqlite::Result<Vec<Example>> {
    let (at, id) = after.unwrap_or(("", ""));
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM ml_categories WHERE decided_by = 'owner' \
         AND (decided_at > ?1 OR (decided_at = ?1 AND item_id > ?2)) \
         ORDER BY decided_at, item_id LIMIT ?3"
    ))?;
    let rows = st.query_map(params![at, id, limit as i64], from_row)?;
    rows.map(|r| {
        r.map(|r| Example {
            item_id: r.item_id,
            category: r.category,
            auto_category: r.auto_category,
            features: r.features,
            model: r.model,
            labelled_at: r.decided_at,
        })
    })
    .collect()
}

/// How many rows each decider has made (`rules`, `jev`, `owner`), over the given item ids' SQL
/// when `live_ids_sql` is set, else all rows.
pub fn count_by(
    c: &Connection,
    live_ids_sql: Option<&str>,
) -> rusqlite::Result<Vec<(DecidedBy, i64)>> {
    let filter = live_ids_sql
        .map(|s| format!(" WHERE item_id IN ({s})"))
        .unwrap_or_default();
    let mut st = c.prepare(&format!(
        "SELECT decided_by, COUNT(*) FROM ml_categories{filter} GROUP BY decided_by"
    ))?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for r in rows {
        let (by, n) = r?;
        if let Some(by) = DecidedBy::parse(&by) {
            out.push((by, n));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::categories::{Facts, SourceKind, ZeroShot, ZERO_SHOT_SCALE};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn features() -> Features {
        let zs = ZeroShot::from_cosines([0.1; 10], ZERO_SHOT_SCALE);
        let facts = Facts {
            kind: "photo",
            width: Some(100),
            height: Some(200),
            mime: "image/png",
            source_kind: SourceKind::Chat,
            name: None,
            camera: None,
            has_gps: false,
        };
        Features::build(&facts, &zs)
    }

    fn auto<'a>(id: &'a str, cat: Category, f: &'a Features) -> NewDecision<'a> {
        NewDecision {
            item_id: id,
            category: cat,
            confidence: 0.8,
            kind: DeciderKind::Rules,
            decider_id: "rules-1",
            model: "m",
            features: f,
            reason: "zero-shot",
        }
    }

    #[test]
    fn an_automatic_decision_is_stored_and_replaced() {
        let c = db();
        let f = features();
        assert!(put_auto(&c, &auto("it_1", Category::Receipt, &f), "t0").unwrap());
        let r = get(&c, "it_1").unwrap().unwrap();
        assert_eq!(r.category, Category::Receipt);
        assert_eq!(r.decided_by, DecidedBy::Rules);
        assert_eq!((r.decider_id.as_str(), r.model.as_str()), ("rules-1", "m"));
        assert_eq!(r.features.as_ref(), Some(&f));
        assert!((r.confidence - 0.8).abs() < 1e-6);
        assert!(put_auto(&c, &auto("it_1", Category::Food, &f), "t1").unwrap());
        let r = get(&c, "it_1").unwrap().unwrap();
        assert_eq!((r.category, r.decided_at.as_str()), (Category::Food, "t1"));
        assert!(get(&c, "nope").unwrap().is_none());
    }

    #[test]
    fn the_owner_is_never_overwritten() {
        let c = db();
        let f = features();
        put_auto(&c, &auto("it_1", Category::Receipt, &f), "t0").unwrap();
        let r = set_owner(&c, "it_1", Category::Photo, None, "t1").unwrap();
        assert_eq!(r.decided_by, DecidedBy::Owner);
        assert_eq!(r.category, Category::Photo);
        assert_eq!(r.auto_category, Some(Category::Receipt));
        assert_eq!(r.features.as_ref(), Some(&f));
        assert_eq!(r.confidence, 1.0);
        // A decider cannot replace it, whatever it says.
        assert!(!put_auto(&c, &auto("it_1", Category::Meme, &f), "t2").unwrap());
        let r = get(&c, "it_1").unwrap().unwrap();
        assert_eq!(
            (r.category, r.decided_by),
            (Category::Photo, DecidedBy::Owner)
        );
        // Choosing again keeps the machine's first answer.
        let r = set_owner(&c, "it_1", Category::Document, None, "t3").unwrap();
        assert_eq!(r.auto_category, Some(Category::Receipt));
        assert_eq!(r.category, Category::Document);
        // Taking it back lets the deciders in again.
        assert!(clear_owner(&c, "it_1").unwrap());
        assert!(!clear_owner(&c, "it_1").unwrap());
        assert!(put_auto(&c, &auto("it_1", Category::Meme, &f), "t4").unwrap());
        assert_eq!(get(&c, "it_1").unwrap().unwrap().auto_category, None);
        // clear_owner never removes a machine's decision.
        assert!(!clear_owner(&c, "it_1").unwrap());
        assert!(get(&c, "it_1").unwrap().is_some());
    }

    #[test]
    fn an_owner_label_without_a_prior_decision_takes_the_given_record() {
        let c = db();
        let f = features();
        let r = set_owner(&c, "it_9", Category::Pet, Some(&f), "t").unwrap();
        assert_eq!(r.auto_category, None);
        assert_eq!(r.features.as_ref(), Some(&f));
        let none = set_owner(&c, "it_8", Category::Pet, None, "t").unwrap();
        assert_eq!((none.features, none.features_version), (None, 0));
    }

    #[test]
    fn labelled_examples_page_in_order() {
        let c = db();
        let f = features();
        put_auto(&c, &auto("it_a", Category::Receipt, &f), "t0").unwrap();
        put_auto(&c, &auto("it_b", Category::Photo, &f), "t0").unwrap();
        set_owner(&c, "it_a", Category::Photo, None, "t1").unwrap();
        set_owner(&c, "it_b", Category::Document, None, "t2").unwrap();
        set_owner(&c, "it_c", Category::Pet, Some(&f), "t2").unwrap();
        let all = examples(&c, None, 10).unwrap();
        let ids: Vec<&str> = all.iter().map(|e| e.item_id.as_str()).collect();
        assert_eq!(ids, ["it_a", "it_b", "it_c"]);
        assert_eq!(all[0].auto_category, Some(Category::Receipt));
        assert_eq!(all[0].category, Category::Photo);
        assert_eq!(all[2].auto_category, None);
        assert_eq!(all[2].features.as_ref(), Some(&f));
        let page = examples(&c, None, 2).unwrap();
        assert_eq!(page.len(), 2);
        let last = page.last().unwrap();
        let next = examples(&c, Some((&last.labelled_at, &last.item_id)), 2).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].item_id, "it_c");
        // Automatic rows are not examples.
        put_auto(&c, &auto("it_d", Category::Food, &f), "t3").unwrap();
        assert_eq!(examples(&c, None, 10).unwrap().len(), 3);
    }

    #[test]
    fn forget_prune_and_counts() {
        let c = db();
        c.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY); INSERT INTO items VALUES ('it_1'), ('it_2');",
        )
        .unwrap();
        let f = features();
        put_auto(&c, &auto("it_1", Category::Food, &f), "t").unwrap();
        put_auto(&c, &auto("it_2", Category::Pet, &f), "t").unwrap();
        put_auto(&c, &auto("it_gone", Category::Pet, &f), "t").unwrap();
        set_owner(&c, "it_2", Category::Photo, None, "t").unwrap();
        let mut by = count_by(&c, None).unwrap();
        by.sort_by_key(|(b, _)| b.as_str());
        assert_eq!(by, [(DecidedBy::Owner, 1), (DecidedBy::Rules, 2)]);
        assert_eq!(prune(&c, "SELECT id FROM items").unwrap(), 1);
        forget(&c, "it_1").unwrap();
        assert!(get(&c, "it_1").unwrap().is_none());
        assert_eq!(
            count_by(&c, Some("SELECT id FROM items")).unwrap(),
            [(DecidedBy::Owner, 1)]
        );
    }

    #[test]
    fn the_crate_forgets_a_purged_item() {
        let c = db();
        crate::store::migrate(&c).unwrap();
        crate::faces_store::migrate(&c).unwrap();
        let f = features();
        put_auto(&c, &auto("it_1", Category::Food, &f), "t").unwrap();
        crate::forget_item(&c, "it_1").unwrap();
        assert!(get(&c, "it_1").unwrap().is_none());
    }
}
