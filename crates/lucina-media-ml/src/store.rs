//! The vectors in the catalog: `ml_embeddings`, one row per (item, model), the vector as f16.
//!
//! Pictures that could not be embedded (unreadable rendition, model error) go to
//! `ml_embed_failures` so the worker does not retry them on every pass; a new rendition or a model
//! change clears them. No foreign key to `items`: this crate does not know that table, and the
//! caller drops rows of purged items ([`prune`]).

use rusqlite::{params, Connection, OptionalExtension};

use crate::index::VectorIndex;
use crate::vector::{from_f16_bytes, to_f16_bytes};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ml_embeddings (
    item_id    TEXT NOT NULL,
    model      TEXT NOT NULL,
    dim        INTEGER NOT NULL,
    vec        BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (item_id, model)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ml_embed_failures (
    item_id   TEXT NOT NULL,
    model     TEXT NOT NULL,
    error     TEXT NOT NULL,
    failed_at TEXT NOT NULL,
    PRIMARY KEY (item_id, model)
) WITHOUT ROWID;
";

/// Idempotent: safe on every start.
pub fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(SCHEMA)
}

/// Stores `item_id`'s vector for `model` (replacing an older one) and clears a recorded failure.
pub fn put(
    c: &Connection,
    item_id: &str,
    model: &str,
    v: &[f32],
    now: &str,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO ml_embeddings (item_id, model, dim, vec, created_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(item_id, model) DO UPDATE SET dim = excluded.dim, vec = excluded.vec, created_at = excluded.created_at",
        params![item_id, model, v.len() as i64, to_f16_bytes(v), now],
    )?;
    c.execute(
        "DELETE FROM ml_embed_failures WHERE item_id = ?1 AND model = ?2",
        params![item_id, model],
    )?;
    Ok(())
}

pub fn put_failure(
    c: &Connection,
    item_id: &str,
    model: &str,
    error: &str,
    now: &str,
) -> rusqlite::Result<()> {
    let error: String = error.chars().take(300).collect();
    c.execute(
        "INSERT INTO ml_embed_failures (item_id, model, error, failed_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(item_id, model) DO UPDATE SET error = excluded.error, failed_at = excluded.failed_at",
        params![item_id, model, error, now],
    )?;
    Ok(())
}

pub fn get(c: &Connection, item_id: &str, model: &str) -> rusqlite::Result<Option<Vec<f32>>> {
    let b: Option<Vec<u8>> = c
        .query_row(
            "SELECT vec FROM ml_embeddings WHERE item_id = ?1 AND model = ?2",
            params![item_id, model],
            |r| r.get(0),
        )
        .optional()?;
    Ok(b.and_then(|b| from_f16_bytes(&b)))
}

/// Forgets `item_id`'s vectors and failures (every model): its picture changed.
pub fn forget(c: &Connection, item_id: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM ml_embeddings WHERE item_id = ?1", [item_id])?;
    c.execute(
        "DELETE FROM ml_embed_failures WHERE item_id = ?1",
        [item_id],
    )?;
    Ok(())
}

/// Every vector of `model` with `dim` values, in an index. Rows of another length are skipped.
pub fn load(c: &Connection, model: &str, dim: usize) -> rusqlite::Result<VectorIndex> {
    let mut ix = VectorIndex::new(dim);
    let mut st =
        c.prepare("SELECT item_id, vec FROM ml_embeddings WHERE model = ?1 AND dim = ?2")?;
    let mut rows = st.query(params![model, dim as i64])?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let b: Vec<u8> = r.get(1)?;
        if let Some(v) = from_f16_bytes(&b) {
            ix.upsert(&id, &v);
        }
    }
    Ok(ix)
}

pub fn count(c: &Connection, model: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM ml_embeddings WHERE model = ?1",
        [model],
        |r| r.get(0),
    )
}

/// How many vectors each model has stored, by model id.
pub fn counts_by_model(
    c: &Connection,
) -> rusqlite::Result<std::collections::BTreeMap<String, i64>> {
    let mut st = c.prepare("SELECT model, COUNT(*) FROM ml_embeddings GROUP BY model")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// The model other than `current` with the most stored vectors: the one that answered searches
/// before `current` was chosen. Ties go to the smaller id so the answer is stable. `None` when no
/// other model has a vector.
pub fn previous_model(c: &Connection, current: &str) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT model FROM ml_embeddings WHERE model != ?1 \
         GROUP BY model ORDER BY COUNT(*) DESC, model ASC LIMIT 1",
        [current],
        |r| r.get(0),
    )
    .optional()
}

pub fn failures(c: &Connection, model: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM ml_embed_failures WHERE model = ?1",
        [model],
        |r| r.get(0),
    )
}

/// Drops rows whose item is gone, given the SQL of the live item ids (e.g. `SELECT id FROM items`).
pub fn prune(c: &Connection, live_ids_sql: &str) -> rusqlite::Result<usize> {
    let a = c.execute(
        &format!("DELETE FROM ml_embeddings WHERE item_id NOT IN ({live_ids_sql})"),
        [],
    )?;
    let b = c.execute(
        &format!("DELETE FROM ml_embed_failures WHERE item_id NOT IN ({live_ids_sql})"),
        [],
    )?;
    Ok(a + b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    #[test]
    fn put_load_and_replace() {
        let c = db();
        put(&c, "it_1", "m", &[1.0, 0.0], "t0").unwrap();
        put(&c, "it_2", "m", &[0.0, 2.0], "t0").unwrap();
        put(&c, "it_1", "m", &[0.0, 1.0], "t1").unwrap();
        put(&c, "it_3", "other", &[1.0, 0.0], "t0").unwrap();
        assert_eq!(count(&c, "m").unwrap(), 2);
        let ix = load(&c, "m", 2).unwrap();
        assert_eq!(ix.len(), 2);
        assert_eq!(ix.get("it_1").unwrap(), &[0.0, 1.0]);
        assert!(!ix.contains("it_3"));
        assert!(load(&c, "m", 3).unwrap().is_empty());
    }

    #[test]
    fn a_success_clears_the_failure() {
        let c = db();
        put_failure(&c, "it_1", "m", &"x".repeat(1000), "t0").unwrap();
        assert_eq!(failures(&c, "m").unwrap(), 1);
        let err: String = c
            .query_row("SELECT error FROM ml_embed_failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(err.len(), 300);
        put(&c, "it_1", "m", &[1.0], "t1").unwrap();
        assert_eq!(failures(&c, "m").unwrap(), 0);
        assert_eq!(get(&c, "it_1", "m").unwrap(), Some(vec![1.0]));
    }

    #[test]
    fn counts_and_previous_model() {
        let c = db();
        assert_eq!(previous_model(&c, "new").unwrap(), None);
        for id in ["a", "b", "c"] {
            put(&c, id, "old", &[1.0], "t").unwrap();
        }
        put(&c, "a", "older", &[1.0], "t").unwrap();
        put(&c, "a", "new", &[1.0], "t").unwrap();
        let counts = counts_by_model(&c).unwrap();
        assert_eq!(counts["old"], 3);
        assert_eq!(counts["older"], 1);
        assert_eq!(counts["new"], 1);
        // The most rows other than the current one; the current model never counts.
        assert_eq!(previous_model(&c, "new").unwrap().as_deref(), Some("old"));
        assert_eq!(previous_model(&c, "old").unwrap().as_deref(), Some("new"));
        // A tie goes to the smaller id.
        put(&c, "b", "older", &[1.0], "t").unwrap();
        put(&c, "c", "older", &[1.0], "t").unwrap();
        assert_eq!(previous_model(&c, "new").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn forget_and_prune() {
        let c = db();
        c.execute_batch(
            "CREATE TABLE items (id TEXT PRIMARY KEY); INSERT INTO items VALUES ('it_1');",
        )
        .unwrap();
        put(&c, "it_1", "m", &[1.0], "t").unwrap();
        put(&c, "it_gone", "m", &[1.0], "t").unwrap();
        put_failure(&c, "it_gone2", "m", "bad", "t").unwrap();
        assert_eq!(prune(&c, "SELECT id FROM items").unwrap(), 2);
        assert_eq!(count(&c, "m").unwrap(), 1);
        forget(&c, "it_1").unwrap();
        assert_eq!(count(&c, "m").unwrap(), 0);
    }
}
