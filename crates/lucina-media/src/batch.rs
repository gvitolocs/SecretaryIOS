//! `POST /v1/media/items/batch`: one operation on up to 500 items, for the Gallery's selection bar.
//!
//! The request is checked as a whole by [`plan`] (a pure function: known op, 1 to 500 ids, an album
//! where the op needs one, `also_from_device` only on `trash`), then [`apply`] runs it in one
//! transaction and reports each id as done or failed. A failing id never stops the others.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{self, now_str, BlobLoc};
use crate::{deletions, trash};

/// The most ids one call takes.
pub const MAX_BATCH: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchOp {
    Trash,
    Restore,
    Purge,
    Favorite,
    Unfavorite,
    AlbumAdd,
    AlbumRemove,
}

impl BatchOp {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "trash" => Self::Trash,
            "restore" => Self::Restore,
            "purge" => Self::Purge,
            "favorite" => Self::Favorite,
            "unfavorite" => Self::Unfavorite,
            "album_add" => Self::AlbumAdd,
            "album_remove" => Self::AlbumRemove,
            _ => return None,
        })
    }

    fn needs_album(self) -> bool {
        matches!(self, Self::AlbumAdd | Self::AlbumRemove)
    }

    /// The event the clients hear for each item the op changed.
    pub fn event(self) -> &'static str {
        match self {
            Self::Trash => "item.trashed",
            Self::Purge => "item.purged",
            _ => "item.updated",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BatchReq {
    pub op: String,
    #[serde(default)]
    pub ids: Vec<String>,
    pub album: Option<String>,
    pub also_from_device: Option<bool>,
}

/// A checked request.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub op: BatchOp,
    /// The ids, first occurrence order, duplicates dropped.
    pub ids: Vec<String>,
    pub album: Option<String>,
    pub also_from_device: bool,
}

/// Check a request as a whole; the message is the 400 body.
pub fn plan(req: BatchReq) -> Result<Plan, String> {
    let op = BatchOp::parse(&req.op).ok_or_else(|| {
        "op must be one of trash, restore, purge, favorite, unfavorite, album_add, album_remove"
            .to_string()
    })?;
    if req.ids.is_empty() {
        return Err("ids must not be empty".into());
    }
    if req.ids.len() > MAX_BATCH {
        return Err(format!("at most {MAX_BATCH} ids per call"));
    }
    let album = req.album.filter(|a| !a.trim().is_empty());
    if op.needs_album() && album.is_none() {
        return Err("album is required for album_add and album_remove".into());
    }
    let also = req.also_from_device.unwrap_or(false);
    if also && op != BatchOp::Trash {
        return Err("also_from_device only goes with trash".into());
    }
    let mut seen = std::collections::HashSet::new();
    let ids = req
        .ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    Ok(Plan {
        op,
        ids,
        album,
        also_from_device: also,
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Failed {
    pub id: String,
    pub error: &'static str,
}

#[derive(Debug, Default)]
pub struct BatchResult {
    pub done: Vec<String>,
    pub failed: Vec<Failed>,
    /// Device deletions queued by `trash` with `also_from_device`.
    pub queued: usize,
    /// Originals a purge left unreferenced; [`trash::finish_purge`] removes them after the commit.
    pub orphans: Vec<BlobLoc>,
}

pub enum Outcome {
    /// The album the op names does not exist.
    NoAlbum,
    Applied(BatchResult),
}

/// An id the catalog could hold (the same shape the item routes accept).
pub fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct Row {
    trashed: bool,
    device: Option<String>,
    asset_id: Option<String>,
}

fn row(c: &Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        "SELECT trashed_at IS NOT NULL, device, asset_id FROM items WHERE id = ?1",
        [id],
        |r| {
            Ok(Row {
                trashed: r.get(0)?,
                device: r.get(1)?,
                asset_id: r.get(2)?,
            })
        },
    )
    .optional()
}

/// Run a checked request in one transaction. A database error rolls the whole call back.
pub fn apply(c: &mut Connection, plan: &Plan) -> rusqlite::Result<Outcome> {
    let tx = c.transaction()?;
    let album = match &plan.album {
        Some(a) if plan.op.needs_album() => match db::get_album(&tx, a)? {
            Some(al) => Some(al.id),
            None => return Ok(Outcome::NoAlbum),
        },
        _ => None,
    };
    let mut res = BatchResult::default();
    if plan.op == BatchOp::Purge {
        let valid: Vec<String> = plan.ids.iter().filter(|i| id_ok(i)).cloned().collect();
        let report = trash::purge_in(&tx, &valid)?;
        res.failed = report
            .failed
            .into_iter()
            .map(|(id, error)| Failed { id, error })
            .collect();
        res.done = report.done;
        res.orphans = report.orphans;
        for id in plan.ids.iter().filter(|i| !id_ok(i)) {
            res.failed.push(Failed {
                id: id.clone(),
                error: "invalid_id",
            });
        }
        tx.commit()?;
        return Ok(Outcome::Applied(res));
    }
    let now = now_str();
    for id in &plan.ids {
        if !id_ok(id) {
            res.failed.push(Failed {
                id: id.clone(),
                error: "invalid_id",
            });
            continue;
        }
        let Some(item) = row(&tx, id)? else {
            res.failed.push(Failed {
                id: id.clone(),
                error: "not_found",
            });
            continue;
        };
        let error = match plan.op {
            BatchOp::Trash if item.trashed => Some("already_trashed"),
            BatchOp::Restore if !item.trashed => Some("not_trashed"),
            _ => None,
        };
        if let Some(error) = error {
            res.failed.push(Failed {
                id: id.clone(),
                error,
            });
            continue;
        }
        match plan.op {
            BatchOp::Trash => {
                tx.execute(
                    "UPDATE items SET trashed_at = ?2, updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                if plan.also_from_device {
                    if let (Some(dev), Some(asset)) = (&item.device, &item.asset_id) {
                        deletions::queue(&tx, dev, asset, id)?;
                        res.queued += 1;
                    }
                }
            }
            BatchOp::Restore => {
                tx.execute(
                    "UPDATE items SET trashed_at = NULL, updated_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
            }
            BatchOp::Favorite | BatchOp::Unfavorite => {
                db::set_favorite(&tx, id, plan.op == BatchOp::Favorite)?;
            }
            BatchOp::AlbumAdd => {
                let aid = album.as_deref().expect("album op has an album");
                if db::add_to_album(&tx, aid, id)? {
                    db::reindex(&tx, id)?;
                }
            }
            BatchOp::AlbumRemove => {
                let aid = album.as_deref().expect("album op has an album");
                tx.execute(
                    "DELETE FROM album_items WHERE album_id = ?1 AND item_id = ?2",
                    params![aid, id],
                )?;
                db::reindex(&tx, id)?;
            }
            BatchOp::Purge => unreachable!("handled above"),
        }
        res.done.push(id.clone());
    }
    tx.commit()?;
    Ok(Outcome::Applied(res))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(op: &str, ids: &[&str]) -> BatchReq {
        BatchReq {
            op: op.into(),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            album: None,
            also_from_device: None,
        }
    }

    #[test]
    fn every_op_parses_and_others_do_not() {
        for (s, op) in [
            ("trash", BatchOp::Trash),
            ("restore", BatchOp::Restore),
            ("purge", BatchOp::Purge),
            ("favorite", BatchOp::Favorite),
            ("unfavorite", BatchOp::Unfavorite),
            ("album_add", BatchOp::AlbumAdd),
            ("album_remove", BatchOp::AlbumRemove),
        ] {
            assert_eq!(BatchOp::parse(s), Some(op));
        }
        for s in ["", "delete", "Trash", "purge ", "empty"] {
            assert_eq!(BatchOp::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn a_plan_needs_ids_within_the_limit() {
        assert!(plan(req("trash", &[])).is_err());
        let many: Vec<String> = (0..=MAX_BATCH).map(|i| format!("itm_{i}")).collect();
        let big = BatchReq {
            ids: many.clone(),
            ..req("trash", &[])
        };
        assert!(plan(big).is_err());
        let exact = BatchReq {
            ids: many[..MAX_BATCH].to_vec(),
            ..req("trash", &[])
        };
        assert_eq!(plan(exact).unwrap().ids.len(), MAX_BATCH);
        assert!(plan(req("nope", &["a"])).is_err());
    }

    #[test]
    fn album_ops_need_an_album_and_only_trash_may_ask_the_device() {
        assert!(plan(req("album_add", &["a"])).is_err());
        assert!(plan(req("album_remove", &["a"])).is_err());
        let ok = BatchReq {
            album: Some("alb_1".into()),
            ..req("album_add", &["a"])
        };
        assert_eq!(plan(ok).unwrap().album.as_deref(), Some("alb_1"));
        let blank = BatchReq {
            album: Some("  ".into()),
            ..req("album_add", &["a"])
        };
        assert!(plan(blank).is_err());
        let also = |op: &str| BatchReq {
            also_from_device: Some(true),
            ..req(op, &["a"])
        };
        assert!(plan(also("trash")).unwrap().also_from_device);
        for op in ["restore", "purge", "favorite", "unfavorite"] {
            assert!(plan(also(op)).is_err(), "{op}");
        }
        // Explicitly false is fine anywhere.
        let no = BatchReq {
            also_from_device: Some(false),
            ..req("purge", &["a"])
        };
        assert!(!plan(no).unwrap().also_from_device);
    }

    #[test]
    fn duplicate_ids_count_once_in_order() {
        let p = plan(req("favorite", &["b", "a", "b", "c", "a"])).unwrap();
        assert_eq!(p.ids, vec!["b", "a", "c"]);
    }

    #[test]
    fn ids_have_the_shape_of_catalog_ids() {
        assert!(id_ok("itm_0123abcd-EF"));
        for id in ["", "a b", "a/b", "../x", "é", &"x".repeat(129)] {
            assert!(!id_ok(id), "{id:?}");
        }
    }
}
