//! Differential test: `GroceryRepo::apply_ops` on Postgres against a
//! plain-Rust copy of the Verus model `verification/verus/shopping_ops.rs`
//! (branch verus-proofs). Random op batches (re-used and repeated op ids,
//! fresh / current / stale `expectedRevision`) go to both; the 409s, the
//! revision, applied / duplicate / rejected op ids, the row each add lands on
//! and the active rows of the list must agree, and the kernel's invariants
//! (one active row per key, removed stays removed) are checked on the way.
//!
//! Item objects (lucina-items, after the Verus model was written): a row's
//! key is its item object. Typed text finds an object by any of its names
//! (it/en/da; without the translator every language starts as the typed
//! text), a rename edits the object's Italian name (or points the row at
//! another object that has the typed name). The kernel copy below models
//! that with a small object table; the Verus model on verus-proofs still has
//! text keys (to port there).
//!
//! Needs `LUCINA_TEST_DATABASE_URL` like tests/shopping.rs (the URL must
//! contain `55432` or `lucina_test`; the grocery schema is recreated with
//! migrations 001, 002 and 003).

use std::collections::{BTreeMap, HashMap, HashSet};

use lucina_grocery::{Catalog, GroceryError, GroceryRepo};
use proptest::prelude::*;
use proptest::test_runner::{Config as PtConfig, TestCaseError, TestRunner};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// Plain-Rust copy of the Verus model.
mod kernel {
    use std::collections::{BTreeMap, HashSet};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Needed,
        Recent,
        Removed,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Row {
        /// The item object (index into `St::objects`).
        pub key: usize,
        pub status: Status,
        pub rev: i64,
    }

    /// A custom item object: its Italian name and every term that finds it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Obj {
        pub it: String,
        pub others: Vec<String>,
    }

    impl Obj {
        fn finds(&self, text: &str) -> bool {
            self.it == text || self.others.iter().any(|o| o == text)
        }
    }

    #[derive(Debug, Clone)]
    pub enum Op {
        Add { key: String, overwrites: bool },
        Need(u64),
        Purchase(u64),
        Remove(u64),
        Edit(u64),
        Rename(u64, String),
        Move(u64, bool),
        ClearRecent,
    }

    #[derive(Debug, Clone)]
    pub struct TaggedOp {
        pub id: String,
        pub op: Op,
    }

    #[derive(Debug, Clone, Default)]
    pub struct St {
        pub rows: BTreeMap<u64, Row>,
        pub next_row: u64,
        pub objects: Vec<Obj>,
    }

    impl St {
        /// The object typed text names (first created wins).
        pub fn lookup(&self, text: &str) -> Option<usize> {
            self.objects.iter().position(|o| o.finds(text))
        }

        /// The object for an add: found, or a new one named `text` in every
        /// language.
        fn object_for(&mut self, text: &str) -> usize {
            self.lookup(text).unwrap_or_else(|| {
                self.objects.push(Obj {
                    it: text.to_owned(),
                    others: vec![text.to_owned(), text.to_owned()],
                });
                self.objects.len() - 1
            })
        }

        /// The visible name of a row.
        pub fn name(&self, r: &Row) -> String {
            self.objects[r.key].it.clone()
        }
    }

    #[derive(Debug, Clone, Default)]
    pub struct ListState {
        pub st: St,
        pub revision: i64,
        pub receipts: HashSet<String>,
    }

    pub fn active(r: &Row) -> bool {
        r.status != Status::Removed
    }

    pub fn the_active(rows: &BTreeMap<u64, Row>, key: usize) -> Option<u64> {
        rows.iter()
            .find(|(_, r)| active(r) && r.key == key)
            .map(|(k, _)| *k)
    }

    fn touch_status(s: &mut St, r: u64, rev: i64, status: Status) -> bool {
        match s.rows.get_mut(&r) {
            None => false,
            Some(row) if !active(row) => true,
            Some(row) => {
                row.status = status;
                row.rev = rev;
                true
            }
        }
    }

    /// apply_one; returns (applied, the row an add landed on).
    pub fn step(s: &mut St, rev: i64, op: &Op) -> (bool, Option<u64>) {
        match op {
            Op::Add { key, .. } => {
                let key = s.object_for(key);
                if let Some(r) = the_active(&s.rows, key) {
                    s.rows.insert(
                        r,
                        Row {
                            key,
                            status: Status::Needed,
                            rev,
                        },
                    );
                    (true, Some(r))
                } else {
                    let n = s.next_row;
                    s.rows.insert(
                        n,
                        Row {
                            key,
                            status: Status::Needed,
                            rev,
                        },
                    );
                    s.next_row += 1;
                    (true, Some(n))
                }
            }
            Op::Need(r) => (touch_status(s, *r, rev, Status::Needed), None),
            Op::Purchase(r) => (touch_status(s, *r, rev, Status::Recent), None),
            Op::Remove(r) => (touch_status(s, *r, rev, Status::Removed), None),
            Op::Edit(r) => match s.rows.get_mut(r) {
                None => (false, None),
                Some(row) if !active(row) => (true, None),
                Some(row) => {
                    row.rev = rev;
                    (true, None)
                }
            },
            Op::Rename(r, text) => {
                let Some(row) = s.rows.get(r) else {
                    return (false, None);
                };
                if !active(row) {
                    return (true, None);
                }
                let current = row.key;
                let hit = s.lookup(text);
                if hit.is_none() || hit == Some(current) {
                    // An edit of the object's Italian name.
                    s.objects[current].it = text.clone();
                    s.rows.get_mut(r).unwrap().rev = rev;
                    return (true, None);
                }
                // Another object's name: point the row there.
                let other = hit.unwrap();
                if the_active(&s.rows, other).is_some_and(|a| a != *r) {
                    return (false, None);
                }
                let row = s.rows.get_mut(r).unwrap();
                row.key = other;
                row.rev = rev;
                (true, None)
            }
            Op::Move(r, merge) => match s.rows.get(r) {
                None => (false, None),
                Some(row) if !active(row) => (true, None),
                Some(_) if *merge => (touch_status(s, *r, rev, Status::Removed), None),
                Some(_) => {
                    s.rows.remove(r);
                    (true, None)
                }
            },
            Op::ClearRecent => {
                for row in s.rows.values_mut() {
                    if row.status == Status::Recent {
                        row.status = Status::Removed;
                        row.rev = rev;
                    }
                }
                (true, None)
            }
        }
    }

    pub fn touched_rev(s: &St, op: &Op) -> Option<i64> {
        match op {
            Op::Add { key, overwrites } => {
                if *overwrites {
                    s.lookup(key)
                        .and_then(|o| the_active(&s.rows, o))
                        .map(|r| s.rows[&r].rev)
                } else {
                    None
                }
            }
            Op::ClearRecent => None,
            Op::Need(r)
            | Op::Purchase(r)
            | Op::Remove(r)
            | Op::Edit(r)
            | Op::Rename(r, _)
            | Op::Move(r, _) => s.rows.get(r).map(|row| row.rev),
        }
    }

    pub fn fresh_ops(ops: &[TaggedOp], receipts: &HashSet<String>) -> (Vec<TaggedOp>, Vec<String>) {
        let mut seen = receipts.clone();
        let mut fresh = Vec::new();
        let mut dups = Vec::new();
        for o in ops {
            if seen.contains(&o.id) {
                dups.push(o.id.clone());
            } else {
                seen.insert(o.id.clone());
                fresh.push(o.clone());
            }
        }
        (fresh, dups)
    }

    #[derive(Debug, Default)]
    pub struct Done {
        pub applied: Vec<String>,
        pub duplicates: Vec<String>,
        pub rejected: Vec<String>,
        /// opId -> row, for adds.
        pub landed: Vec<(String, u64)>,
    }

    /// apply_batch. `merge_of(st, row)` resolves a move's merge flag when it runs
    /// (the harness tracks the target list).
    pub fn apply_batch(
        l: &mut ListState,
        ops: &[TaggedOp],
        expected: Option<i64>,
        mut merge_of: impl FnMut(&St, u64) -> bool,
    ) -> Option<Done> {
        let (fresh, duplicates) = fresh_ops(ops, &l.receipts);
        if let Some(e) = expected {
            if e < l.revision
                && fresh
                    .iter()
                    .any(|o| touched_rev(&l.st, &o.op).is_some_and(|r| r > e))
            {
                return None;
            }
        }
        let rev = l.revision + 1;
        let mut d = Done {
            duplicates,
            ..Done::default()
        };
        for o in &fresh {
            let op = match &o.op {
                Op::Move(r, _) => Op::Move(*r, merge_of(&l.st, *r)),
                other => other.clone(),
            };
            let (ok, landed) = step(&mut l.st, rev, &op);
            if ok {
                d.applied.push(o.id.clone());
                if let Some(r) = landed {
                    d.landed.push((o.id.clone(), r));
                }
            } else {
                d.rejected.push(o.id.clone());
            }
            l.receipts.insert(o.id.clone());
        }
        if !d.applied.is_empty() {
            l.revision += 1;
        }
        Some(d)
    }

    pub fn unique_active(rows: &BTreeMap<u64, Row>) -> bool {
        let mut keys = HashSet::new();
        rows.values()
            .filter(|r| active(r))
            .all(|r| keys.insert(r.key))
    }
}

// ---------------------------------------------------------------- harness

async fn repo() -> Option<GroceryRepo> {
    let url = std::env::var("LUCINA_TEST_DATABASE_URL").ok()?;
    assert!(
        url.contains("55432") || url.contains("lucina_test"),
        "refusing a non-test database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS grocery CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/001_grocery.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/002_shopping_v2.sql"))
        .execute(&pool)
        .await
        .unwrap();
    // Item objects (lucina-items): rows point to an item object.
    sqlx::raw_sql(include_str!("../migrations/003_shopping_items.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/004_shopping_row_aliases.sql"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    Some(GroceryRepo::with_pool(
        pool,
        Catalog::fixed(&json!({"version": 1, "items": [
            {"id": "milk", "names": {"it": "Latte", "en": "Milk"}, "section": "dairy", "icon": "🥛"}
        ]})),
    ))
}

const NAMES: [&str; 4] = ["xqa", "xqb", "xqc", "xqd"];

/// A generated op: which kind, a name index, a row index (into the rows the
/// kernel knows, or past them = unknown row), a flag, and how its id is picked.
#[derive(Debug, Clone)]
struct GenOp {
    kind: u8,
    name: usize,
    row: usize,
    flag: bool,
    id: u8,
    id_pick: usize,
}

#[derive(Debug, Clone)]
struct GenBatch {
    ops: Vec<GenOp>,
    expected: u8,
}

fn gen_batches() -> impl Strategy<Value = Vec<GenBatch>> {
    let op = (
        0u8..9,
        0usize..4,
        0usize..5,
        any::<bool>(),
        0u8..10,
        any::<usize>(),
    )
        .prop_map(|(kind, name, row, flag, id, id_pick)| GenOp {
            kind,
            name,
            row,
            flag,
            id,
            id_pick,
        });
    let batch = (proptest::collection::vec(op, 1..6), 0u8..5)
        .prop_map(|(ops, expected)| GenBatch { ops, expected });
    proptest::collection::vec(batch, 1..9)
}

async fn run_case(r: &GroceryRepo, batches: Vec<GenBatch>) -> Result<(), TestCaseError> {
    let user = format!("verus-{}", Uuid::new_v4());
    let lists = r.list_lists(&user).await.unwrap();
    let list_id = lists[0]["id"].as_str().unwrap().to_owned();
    let other = r
        .create_list(&user, &json!({"name": "Altro", "theme": "work"}))
        .await
        .unwrap();
    let other_id = other["id"].as_str().unwrap().to_owned();

    let mut model = kernel::ListState::default();
    let mut uuid_of: HashMap<u64, String> = HashMap::new();
    // Objects with an active row in the move target list.
    let mut target_keys: HashSet<usize> = HashSet::new();
    let mut used_ids: Vec<String> = Vec::new();

    for b in batches {
        // Build the batch for both sides.
        let known: Vec<u64> = uuid_of
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut json_ops: Vec<Value> = Vec::new();
        let mut k_ops: Vec<kernel::TaggedOp> = Vec::new();
        for g in &b.ops {
            let id = if g.id < 2 && !used_ids.is_empty() {
                used_ids[g.id_pick % used_ids.len()].clone()
            } else if g.id == 2 && !k_ops.is_empty() {
                k_ops[g.id_pick % k_ops.len()].id.clone()
            } else {
                Uuid::new_v4().to_string()
            };
            let (row, row_uuid) = match known.get(g.row) {
                Some(r) => (*r, uuid_of[r].clone()),
                None => (u64::MAX - g.row as u64, Uuid::new_v4().to_string()),
            };
            let name = NAMES[g.name];
            let (op, j) = match g.kind {
                0 | 1 => (
                    kernel::Op::Add {
                        key: name.into(),
                        overwrites: g.flag,
                    },
                    if g.flag {
                        json!({"type": "add", "name": name, "spec": "2 kg"})
                    } else {
                        json!({"type": "add", "name": name})
                    },
                ),
                2 => (
                    kernel::Op::Need(row),
                    json!({"type": "need", "itemId": row_uuid}),
                ),
                3 => (
                    kernel::Op::Purchase(row),
                    json!({"type": "purchase", "itemId": row_uuid}),
                ),
                4 => (
                    kernel::Op::Remove(row),
                    json!({"type": "remove", "itemId": row_uuid}),
                ),
                5 => (
                    kernel::Op::Edit(row),
                    json!({"type": "set_spec", "itemId": row_uuid, "spec": "bio"}),
                ),
                6 => (
                    kernel::Op::Rename(row, name.into()),
                    json!({"type": "rename", "itemId": row_uuid, "name": name}),
                ),
                7 => (
                    kernel::Op::Move(row, false),
                    json!({"type": "move", "itemId": row_uuid, "toListId": other_id}),
                ),
                _ => (kernel::Op::ClearRecent, json!({"type": "clear_recent"})),
            };
            let mut j = j;
            j["opId"] = json!(id);
            json_ops.push(j);
            k_ops.push(kernel::TaggedOp { id, op });
        }
        let expected = match b.expected {
            0 => None,
            1 => Some(model.revision),
            2 => Some(model.revision - 1),
            3 => Some(model.revision - 2),
            _ => Some(0),
        };

        let before = model.clone();
        let mut tk = target_keys.clone();
        let k = kernel::apply_batch(&mut model, &k_ops, expected, |st, r| {
            let Some(row) = st.rows.get(&r).filter(|row| kernel::active(row)) else {
                return false;
            };
            let key = row.key;
            if tk.contains(&key) {
                true
            } else {
                tk.insert(key);
                false
            }
        });
        let p = r
            .apply_ops(
                &user,
                &list_id,
                &Value::Array(json_ops.clone()),
                expected,
                "web",
            )
            .await;

        match (k, p) {
            (None, Err(e)) => {
                prop_assert_eq!(e.code(), "revision_conflict");
                model = before; // nothing applied
            }
            (Some(d), Ok(out)) => {
                target_keys = tk;
                prop_assert_eq!(out.revision, model.revision);
                prop_assert_eq!(
                    &out.applied,
                    &d.applied,
                    "applied {}",
                    Value::Array(json_ops.clone())
                );
                prop_assert_eq!(&out.duplicates, &d.duplicates);
                let rejected: Vec<String> = out
                    .rejected
                    .iter()
                    .map(|v| v["opId"].as_str().unwrap().to_owned())
                    .collect();
                prop_assert_eq!(&rejected, &d.rejected);
                // Adds land on the same row.
                for (op_id, row) in &d.landed {
                    let u = out.item_ids.get(op_id).cloned();
                    prop_assert!(u.is_some(), "no item id for add {op_id}");
                    let u = u.unwrap();
                    match uuid_of.get(row) {
                        Some(known) => prop_assert_eq!(known, &u, "add merged into another row"),
                        None => {
                            prop_assert!(
                                !uuid_of.values().any(|v| *v == u),
                                "new kernel row, old db row"
                            );
                            uuid_of.insert(*row, u);
                        }
                    }
                }
                // The visible rows: exactly the kernel's active rows.
                let mut db: BTreeMap<String, (String, String)> = BTreeMap::new();
                for it in &out.items {
                    db.insert(
                        it["id"].as_str().unwrap().to_owned(),
                        (
                            it["name"].as_str().unwrap().to_lowercase(),
                            it["status"].as_str().unwrap().to_owned(),
                        ),
                    );
                }
                let mut km: BTreeMap<String, (String, String)> = BTreeMap::new();
                for (rid, row) in &model.st.rows {
                    if kernel::active(row) {
                        let st = if row.status == kernel::Status::Needed {
                            "needed"
                        } else {
                            "recent"
                        };
                        km.insert(uuid_of[rid].clone(), (model.st.name(row), st.to_owned()));
                    }
                }
                prop_assert_eq!(db, km);
                // Every row of the list, removed ones included, with its rev.
                // The key is the item object's name (a rename edits a custom
                // object's name, not the row's legacy name_key).
                let rows = sqlx::query_as::<_, (String, String, i64, String)>(
                    "SELECT i.id::text, i.status, i.rev, lower(COALESCE(c.names->>'it', i.name_key)) \
                       FROM grocery.shopping_list_items i \
                       LEFT JOIN grocery.shopping_custom_items c ON c.id = i.item_id \
                      WHERE i.list_id = $1::uuid",
                )
                .bind(&list_id)
                .fetch_all(r.pool())
                .await
                .unwrap();
                let db_all: BTreeMap<String, (String, String, i64)> = rows
                    .into_iter()
                    .map(|(id, st, rev, k)| (id, (k, st, rev)))
                    .collect();
                let k_all: BTreeMap<String, (String, String, i64)> = model
                    .st
                    .rows
                    .iter()
                    .map(|(rid, row)| {
                        let st = match row.status {
                            kernel::Status::Needed => "needed",
                            kernel::Status::Recent => "recent",
                            kernel::Status::Removed => "removed",
                        };
                        (
                            uuid_of[rid].clone(),
                            (model.st.name(row), st.to_owned(), row.rev),
                        )
                    })
                    .collect();
                prop_assert_eq!(db_all, k_all);
                // Kernel theorems on the way: S3, S4.
                prop_assert!(kernel::unique_active(&model.st.rows));
                for (rid, row) in &before.st.rows {
                    if !kernel::active(row) {
                        prop_assert!(model.st.rows.get(rid).is_some_and(|r| !kernel::active(r)));
                    }
                }
                // S1: the same batch again is all duplicates and changes nothing.
                if b.expected % 2 == 0 {
                    let again = r
                        .apply_ops(
                            &user,
                            &list_id,
                            &Value::Array(json_ops.clone()),
                            Some(0),
                            "web",
                        )
                        .await
                        .unwrap();
                    prop_assert_eq!(again.revision, out.revision);
                    prop_assert!(again.applied.is_empty() && again.rejected.is_empty());
                    prop_assert_eq!(again.duplicates.len(), json_ops.len());
                    prop_assert_eq!(&again.items, &out.items);
                }
            }
            (k, p) => prop_assert!(
                false,
                "kernel {:?} vs production {:?}",
                k.map(|d| d.applied),
                p.map(|o| o.applied).map_err(|e: GroceryError| e.code())
            ),
        }
        used_ids.extend(k_ops.iter().map(|o| o.id.clone()));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_ops_matches_kernel() {
    let Some(r) = repo().await else {
        return eprintln!("LUCINA_TEST_DATABASE_URL unset: skipped");
    };
    let handle = tokio::runtime::Handle::current();
    let cases: u32 = std::env::var("VERUS_OPS_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut runner = TestRunner::new(PtConfig {
        cases,
        ..PtConfig::default()
    });
    let result = tokio::task::block_in_place(|| {
        runner.run(&gen_batches(), |batches| {
            handle.block_on(run_case(&r, batches))
        })
    });
    if let Err(e) = result {
        panic!("{e}");
    }
}
