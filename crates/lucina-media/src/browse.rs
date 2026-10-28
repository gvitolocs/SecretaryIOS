//! Browsing the Gallery beyond the timeline (Google Photos parity, part 2): the Places map
//! (`GET items/places`), the memories strip (`GET items/on-this-day`, `GET items/recent`) and the
//! archive (`items.archived`: hidden from the main grid, never deleted; the batch ops `archive` and
//! `unarchive` are in `batch.rs`, the `archived` and `bbox` filters in `db::ListQuery`).
//!
//! Everything decided here is a pure function with its own tests: the grid clustering of map points,
//! the choice of the best few pictures of a past day, the date arithmetic. The routes only fetch
//! rows through the Gallery's own filters ([`db::list_filters`]), so Places, memories and the grid
//! always agree on what is visible (not trashed, not archived, the chosen origin and kinds).

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{self, ListQuery};
use crate::gallery;
use crate::http::{bad, item_out, ApiResult};
use crate::ingest::Library;

/// The archive flag, added to databases that predate it. Idempotent; called from `db::migrate`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let mut st =
        conn.prepare("SELECT 1 FROM pragma_table_info('items') WHERE name = 'archived'")?;
    let has = st.exists([])?;
    drop(st);
    if !has {
        conn.execute(
            "ALTER TABLE items ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS items_archived ON items(archived) WHERE archived != 0",
        [],
    )?;
    Ok(())
}

pub fn routes() -> Router<Library> {
    Router::new()
        .route("/v1/media/items/places", get(places))
        .route("/v1/media/items/on-this-day", get(on_this_day))
        .route("/v1/media/items/recent", get(recent))
}

// ---- places ------------------------------------------------------------------------------------

/// `west,south,east,north` in degrees. `west > east` is a box across the date line; the latitudes
/// must be ordered. `None` for anything else.
pub fn parse_bbox(s: &str) -> Option<(f64, f64, f64, f64)> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>().ok().filter(|x| x.is_finite()))
        .collect::<Option<_>>()?;
    let [w, s, e, n] = <[f64; 4]>::try_from(v).ok()?;
    let lon = |x: f64| (-180.0..=180.0).contains(&x);
    let lat = |x: f64| (-90.0..=90.0).contains(&x);
    (lon(w) && lon(e) && lat(s) && lat(n) && s <= n).then_some((w, s, e, n))
}

/// A cluster cell is this many map pixels wide (tiles are 256 px).
pub const CELL_PX: f64 = 72.0;
/// Zoom levels the map can ask for.
pub const MAX_ZOOM: f64 = 24.0;

fn merc_y(lat: f64) -> f64 {
    let l = lat.clamp(-85.0511, 85.0511).to_radians();
    (std::f64::consts::FRAC_PI_4 + l / 2.0).tan().ln()
}

/// The grid cell of a point at a map zoom: square on screen (Web Mercator), `CELL_PX` wide.
pub fn cell_of(lat: f64, lon: f64, zoom: f64) -> (i64, i64) {
    let world = 256.0 * 2f64.powf(zoom.clamp(0.0, MAX_ZOOM));
    let lon_cell = 360.0 * CELL_PX / world;
    let y_cell = std::f64::consts::TAU * CELL_PX / world;
    (
        ((lon + 180.0) / lon_cell).floor() as i64,
        (merc_y(lat) / y_cell).floor() as i64,
    )
}

/// One picture with a position.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub favorite: bool,
    pub sort_at: String,
}

/// A group of nearby pictures: how many, where their centre is, the box around them (to list
/// exactly these), and the one that stands for them (a favorite first, then the newest).
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    /// `"<x>:<y>"` of the grid cell at this zoom: stable while the map is not zoomed.
    pub key: String,
    pub count: usize,
    pub lat: f64,
    pub lon: f64,
    /// `west, south, east, north`
    pub bbox: [f64; 4],
    pub cover: String,
}

struct Acc {
    count: usize,
    sum_lat: f64,
    sum_lon: f64,
    bbox: [f64; 4],
    cover: (bool, String, String),
}

/// Group `points` into the grid cells of `zoom`; biggest clusters first, ties by key.
pub fn cluster(points: &[Point], zoom: f64) -> Vec<Cluster> {
    let mut cells: HashMap<(i64, i64), Acc> = HashMap::new();
    for p in points {
        let rank = (p.favorite, p.sort_at.clone(), p.id.clone());
        let a = cells
            .entry(cell_of(p.lat, p.lon, zoom))
            .or_insert_with(|| Acc {
                count: 0,
                sum_lat: 0.0,
                sum_lon: 0.0,
                bbox: [p.lon, p.lat, p.lon, p.lat],
                cover: rank.clone(),
            });
        a.count += 1;
        a.sum_lat += p.lat;
        a.sum_lon += p.lon;
        a.bbox = [
            a.bbox[0].min(p.lon),
            a.bbox[1].min(p.lat),
            a.bbox[2].max(p.lon),
            a.bbox[3].max(p.lat),
        ];
        if rank > a.cover {
            a.cover = rank;
        }
    }
    let mut out: Vec<Cluster> = cells
        .into_iter()
        .map(|((x, y), a)| Cluster {
            key: format!("{x}:{y}"),
            count: a.count,
            lat: a.sum_lat / a.count as f64,
            lon: a.sum_lon / a.count as f64,
            bbox: a.bbox,
            cover: a.cover.2,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    out
}

#[derive(Deserialize)]
struct ZoomQuery {
    zoom: Option<f64>,
}

/// The listing filters every browse route takes: the origin defaults to the whole Gallery.
fn gallery_query(lib: &Library, mut q: ListQuery) -> ApiResult<ListQuery> {
    q.film_folders = lib.cfg.film_folders.clone();
    q.origin = q
        .origin
        .take()
        .filter(|o| !o.is_empty())
        .or_else(|| Some("all".into()));
    if q.origin
        .as_deref()
        .is_some_and(|o| gallery::Origin::parse(o).is_none())
    {
        return Err(bad("origin must be all, device:<id>, chat or lucina"));
    }
    if q.bbox.as_deref().is_some_and(|b| parse_bbox(b).is_none()) {
        return Err(bad("bbox must be west,south,east,north in degrees"));
    }
    if q.kind.as_deref().is_none_or(|k| k.trim().is_empty()) {
        q.kind = Some("photo,video".into());
    }
    Ok(q)
}

/// The visible pictures with a position under `q`'s filters (`bbox` included).
fn points(c: &Connection, q: &ListQuery) -> rusqlite::Result<Vec<Point>> {
    let (mut wh, args) = db::list_filters(q);
    wh.push("i.lat IS NOT NULL AND i.lon IS NOT NULL".into());
    let mut st = c.prepare(&format!(
        "SELECT i.id, i.lat, i.lon, i.favorite, i.sort_at FROM items i WHERE {}",
        wh.join(" AND ")
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter()), |r| {
        Ok(Point {
            id: r.get(0)?,
            lat: r.get(1)?,
            lon: r.get(2)?,
            favorite: r.get::<_, i64>(3)? != 0,
            sort_at: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// `GET items/places?origin=&kind=&bbox=&zoom=&limit=`: the pictures that have a position, grouped
/// into map clusters. `bbox` is the visible map (`west,south,east,north`), `zoom` the map's zoom
/// (default 3, 0 to 24). Answers `{ zoom, total, clusters: [{ id, count, lat, lon, bbox, cover }] }`
/// biggest first, at most `limit` (default 300, up to 500); `cover` is the item JSON of the picture
/// that stands for the cluster, `bbox` the box to pass to `items?bbox=` to list its pictures.
async fn places(
    State(lib): State<Library>,
    Query(q): Query<ListQuery>,
    Query(z): Query<ZoomQuery>,
) -> ApiResult<Json<Value>> {
    let q = gallery_query(&lib, q)?;
    let zoom = z.zoom.unwrap_or(3.0);
    if !(0.0..=MAX_ZOOM).contains(&zoom) {
        return Err(bad("zoom must be between 0 and 24"));
    }
    let limit = q.limit.unwrap_or(300).clamp(1, 500) as usize;
    let (total, clusters, mut covers) = lib
        .db
        .call(move |c| {
            let pts = points(c, &q)?;
            let mut clusters = cluster(&pts, zoom);
            clusters.truncate(limit);
            let mut items = HashMap::new();
            for cl in &clusters {
                if let Some(it) = db::get_item(c, &cl.cover)? {
                    items.insert(cl.cover.clone(), it);
                }
            }
            Ok((pts.len(), clusters, items))
        })
        .await?;
    let out: Vec<Value> = clusters
        .into_iter()
        .filter_map(|cl| {
            let cover = covers.remove(&cl.cover)?;
            Some(json!({
                "id": cl.key,
                "count": cl.count,
                "lat": cl.lat,
                "lon": cl.lon,
                "bbox": cl.bbox,
                "cover": item_out(&lib, cover, None),
            }))
        })
        .collect();
    Ok(Json(
        json!({ "ok": true, "zoom": zoom, "total": total, "clusters": out }),
    ))
}

// ---- memories ----------------------------------------------------------------------------------

/// Pictures of one day closer together than this are one burst: only the best of them is shown.
pub const BURST_GAP_SECS: i64 = 30;
/// How many pictures a year of "on this day" shows, by default and at most.
pub const PER_YEAR: usize = 6;
pub const MAX_PER_YEAR: usize = 20;

/// A picture taken on the day, with what makes one nicer than another.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: String,
    /// Seconds since the epoch.
    pub at: i64,
    pub favorite: bool,
    /// Width times height: bigger is a better picture, a thumbnail or screenshot is not.
    pub pixels: i64,
}

/// The best `n` of a day's pictures, in time order: favorites first, then the most pixels, then the
/// earliest; a picture within `gap` seconds of one already chosen is skipped (a burst counts once).
pub fn pick_best(cands: &[Candidate], n: usize, gap: i64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&cands[a], &cands[b]);
        y.favorite
            .cmp(&x.favorite)
            .then(y.pixels.cmp(&x.pixels))
            .then(x.at.cmp(&y.at))
            .then(x.id.cmp(&y.id))
    });
    let mut chosen: Vec<usize> = Vec::new();
    for i in order {
        if chosen.len() == n {
            break;
        }
        if chosen
            .iter()
            .all(|&j| (cands[i].at - cands[j].at).abs() >= gap)
        {
            chosen.push(i);
        }
    }
    chosen.sort_by(|&a, &b| {
        cands[a]
            .at
            .cmp(&cands[b].at)
            .then(cands[a].id.cmp(&cands[b].id))
    });
    chosen
}

/// `"MM-DD"` as `(month, day)`; Feb 29 is a real day. `None` for anything else.
pub fn parse_month_day(s: &str) -> Option<(u32, u32)> {
    let (m, d) = s.split_once('-')?;
    if m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (m, d): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
    // 2024 is a leap year: it accepts 02-29 and rejects 04-31.
    NaiveDate::from_ymd_opt(2024, m, d).map(|_| (m, d))
}

/// Today in the library's time zone, as `("MM-DD", year)`.
fn today(lib: &Library) -> (String, i32) {
    let now = Utc::now().with_timezone(&lib.cfg.tz);
    (now.format("%m-%d").to_string(), now.year())
}

#[derive(Deserialize)]
struct DayQuery {
    /// `MM-DD`; today (library time zone) by default.
    date: Option<String>,
    /// The year that does not count as "previous": this year by default.
    year: Option<i32>,
    per_year: Option<usize>,
}

/// What the day query reads: the day's pictures that have a real taken date.
fn day_rows(
    c: &Connection,
    q: &ListQuery,
    month_day: &str,
    before_year: i32,
) -> rusqlite::Result<Vec<(i32, Candidate)>> {
    let (mut wh, mut args) = db::list_filters(q);
    wh.push("i.taken_at IS NOT NULL".into());
    args.push(month_day.to_string().into());
    wh.push(format!("substr(i.taken_at, 6, 5) = ?{}", args.len()));
    args.push(format!("{before_year:04}").into());
    wh.push(format!("substr(i.taken_at, 1, 4) < ?{}", args.len()));
    let mut st = c.prepare(&format!(
        "SELECT i.id, i.taken_at, i.favorite, COALESCE(i.width, 0) * COALESCE(i.height, 0) \
         FROM items i WHERE {} ORDER BY i.taken_at LIMIT 20000",
        wh.join(" AND ")
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter()), |r| {
        let taken: String = r.get(1)?;
        Ok((
            r.get::<_, String>(0)?,
            taken,
            r.get::<_, i64>(2)? != 0,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, taken, favorite, pixels) = row?;
        let (Some(year), Ok(at)) = (
            taken.get(..4).and_then(|y| y.parse::<i32>().ok()),
            DateTime::parse_from_rfc3339(&taken),
        ) else {
            continue;
        };
        out.push((
            year,
            Candidate {
                id,
                at: at.timestamp(),
                favorite,
                pixels,
            },
        ));
    }
    Ok(out)
}

/// `GET items/on-this-day?date=MM-DD&year=&per_year=&origin=&kind=`: the pictures taken on this
/// calendar day in previous years, a few of the best per year, newest year first:
/// `{ date, year, years: [{ year, years_ago, total, items: [item JSON] }] }`. `total` counts all
/// of the year's pictures that day; `items` are at most `per_year` (default 6, up to 20), in time
/// order. Pictures without a taken date never appear.
async fn on_this_day(
    State(lib): State<Library>,
    Query(q): Query<ListQuery>,
    Query(d): Query<DayQuery>,
) -> ApiResult<Json<Value>> {
    let q = gallery_query(&lib, q)?;
    let (today_md, today_year) = today(&lib);
    let date = d.date.unwrap_or(today_md);
    let Some((m, day)) = parse_month_day(&date) else {
        return Err(bad("date must be MM-DD"));
    };
    let date = format!("{m:02}-{day:02}");
    let year = d.year.unwrap_or(today_year);
    if !(1..=9999).contains(&year) {
        return Err(bad("year out of range"));
    }
    let per_year = d.per_year.unwrap_or(PER_YEAR).clamp(1, MAX_PER_YEAR);
    let md = date.clone();
    let (groups, mut items) = lib
        .db
        .call(move |c| {
            let rows = day_rows(c, &q, &md, year)?;
            let mut by_year: BTreeMap<i32, Vec<Candidate>> = BTreeMap::new();
            for (y, cand) in rows {
                by_year.entry(y).or_default().push(cand);
            }
            let mut groups = Vec::new();
            let mut items = HashMap::new();
            for (y, cands) in by_year.into_iter().rev() {
                let picks = pick_best(&cands, per_year, BURST_GAP_SECS);
                let mut ids = Vec::new();
                for &i in &picks {
                    if let Some(it) = db::get_item(c, &cands[i].id)? {
                        ids.push(it.id.clone());
                        items.insert(it.id.clone(), it);
                    }
                }
                groups.push((y, cands.len(), ids));
            }
            Ok((groups, items))
        })
        .await?;
    let years: Vec<Value> = groups
        .into_iter()
        .filter(|(_, _, ids)| !ids.is_empty())
        .map(|(y, total, ids)| {
            json!({
                "year": y,
                "years_ago": year - y,
                "total": total,
                "items": ids.iter().filter_map(|id| items.remove(id)).map(|it| item_out(&lib, it, None)).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(
        json!({ "ok": true, "date": date, "year": year, "years": years }),
    ))
}

#[derive(Deserialize)]
struct RecentQuery {
    /// Only what arrived in the last this many days.
    days: Option<i64>,
}

/// `GET items/recent?origin=&kind=&days=&limit=`: the newest arrivals (by the time they were added to
/// the library, not when they were taken), `{ items, total }` with `total` the count within `days`.
/// `limit` defaults to 30, up to 200.
async fn recent(
    State(lib): State<Library>,
    Query(q): Query<ListQuery>,
    Query(r): Query<RecentQuery>,
) -> ApiResult<Json<Value>> {
    let q = gallery_query(&lib, q)?;
    let limit = q.limit.unwrap_or(30).clamp(1, 200) as i64;
    let since = match r.days {
        Some(d) if !(1..=36_500).contains(&d) => {
            return Err(bad("days must be between 1 and 36500"))
        }
        Some(d) => Some(
            (Utc::now() - Duration::days(d)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ),
        None => None,
    };
    let (total, items) = lib
        .db
        .call(move |c| {
            let (mut wh, mut args) = db::list_filters(&q);
            if let Some(s) = since {
                args.push(s.into());
                wh.push(format!("i.added_at >= ?{}", args.len()));
            }
            let cond = wh.join(" AND ");
            let total: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM items i WHERE {cond}"),
                rusqlite::params_from_iter(args.iter()),
                |r| r.get(0),
            )?;
            let mut st = c.prepare(&format!(
                "SELECT i.id FROM items i WHERE {cond} ORDER BY i.added_at DESC, i.id DESC LIMIT {limit}"
            ))?;
            let ids: Vec<String> = st
                .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut items = Vec::new();
            for id in ids {
                if let Some(it) = db::get_item(c, &id)? {
                    items.push(it);
                }
            }
            Ok((total, items))
        })
        .await?;
    Ok(Json(json!({
        "ok": true,
        "total": total,
        "items": items.into_iter().map(|it| item_out(&lib, it, None)).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(id: &str, lat: f64, lon: f64, favorite: bool, sort_at: &str) -> Point {
        Point {
            id: id.into(),
            lat,
            lon,
            favorite,
            sort_at: sort_at.into(),
        }
    }

    #[test]
    fn bbox_needs_four_ordered_numbers() {
        assert_eq!(parse_bbox("-10,40,10,50"), Some((-10.0, 40.0, 10.0, 50.0)));
        assert_eq!(
            parse_bbox(" 170 , -5 , -170 , 5 "),
            Some((170.0, -5.0, -170.0, 5.0))
        );
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "a,b,c,d",
            "0,50,10,40",
            "-181,0,0,1",
            "0,-91,1,0",
            "0,0,181,1",
            "nan,0,1,1",
        ] {
            assert_eq!(parse_bbox(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cells_shrink_as_the_map_zooms_in() {
        // Two points a few hundred metres apart share a cell at city level and split at street level.
        let a = cell_of(55.6761, 12.5683, 8.0);
        let b = cell_of(55.6790, 12.5720, 8.0);
        assert_eq!(a, b);
        assert_ne!(
            cell_of(55.6761, 12.5683, 18.0),
            cell_of(55.6790, 12.5720, 18.0)
        );
        // The date line and the poles do not panic.
        let _ = cell_of(89.9999, 180.0, 0.0);
        let _ = cell_of(-90.0, -180.0, 24.0);
    }

    #[test]
    fn a_cluster_counts_centres_and_boxes_its_pictures() {
        let pts = [
            pt("a", 55.0, 12.0, false, "2020-01-01T00:00:00Z"),
            pt("b", 55.001, 12.001, true, "2019-01-01T00:00:00Z"),
            pt("c", 55.002, 12.002, false, "2021-01-01T00:00:00Z"),
            pt("far", -33.9, 151.2, false, "2022-01-01T00:00:00Z"),
        ];
        let cl = cluster(&pts, 6.0);
        assert_eq!(cl.len(), 2);
        assert_eq!(cl[0].count, 3);
        assert_eq!(cl[1].count, 1);
        assert_eq!(cl[1].cover, "far");
        // The favorite stands for the cluster even though others are newer.
        assert_eq!(cl[0].cover, "b");
        assert!((cl[0].lat - 55.001).abs() < 1e-9 && (cl[0].lon - 12.001).abs() < 1e-9);
        assert_eq!(cl[0].bbox, [12.0, 55.0, 12.002, 55.002]);
        // Zoomed all the way in, every picture is its own cluster.
        assert_eq!(cluster(&pts, 24.0).len(), 4);
        // The total is preserved at every zoom.
        for z in [0.0, 3.0, 9.0, 15.0] {
            assert_eq!(cluster(&pts, z).iter().map(|c| c.count).sum::<usize>(), 4);
        }
    }

    #[test]
    fn without_a_favorite_the_newest_is_the_cover() {
        let pts = [
            pt("old", 1.0, 1.0, false, "2010-01-01T00:00:00Z"),
            pt("new", 1.0, 1.0, false, "2020-01-01T00:00:00Z"),
        ];
        assert_eq!(cluster(&pts, 5.0)[0].cover, "new");
        assert!(cluster(&[], 5.0).is_empty());
    }

    fn cand(id: &str, at: i64, favorite: bool, pixels: i64) -> Candidate {
        Candidate {
            id: id.into(),
            at,
            favorite,
            pixels,
        }
    }

    #[test]
    fn the_best_pictures_come_in_time_order() {
        let c = [
            cand("small", 1000, false, 100),
            cand("big", 2000, false, 9000),
            cand("fav", 3000, true, 10),
            cand("mid", 4000, false, 500),
        ];
        // Favorite first, then the most pixels; shown in the order they were taken.
        let ids = |n| -> Vec<&str> {
            pick_best(&c, n, 30)
                .into_iter()
                .map(|i| c[i].id.as_str())
                .collect()
        };
        assert_eq!(ids(1), ["fav"]);
        assert_eq!(ids(2), ["big", "fav"]);
        assert_eq!(ids(3), ["big", "fav", "mid"]);
        assert_eq!(ids(10), ["small", "big", "fav", "mid"]);
        assert!(pick_best(&[], 3, 30).is_empty());
        assert!(pick_best(&c, 0, 30).is_empty());
    }

    #[test]
    fn a_burst_counts_once() {
        let c = [
            cand("b1", 1000, false, 500),
            cand("b2", 1010, false, 900),
            cand("b3", 1029, false, 800),
            cand("later", 1030, false, 100),
            cand("much-later", 9000, false, 50),
        ];
        let ids: Vec<&str> = pick_best(&c, 5, 30)
            .into_iter()
            .map(|i| c[i].id.as_str())
            .collect();
        // b2 wins its burst (b1 and b3 are within 30 s of it); "later" is 20 s from b2 too, so it goes as well.
        assert_eq!(ids, ["b2", "much-later"]);
        // With no gap everything is a different picture.
        assert_eq!(pick_best(&c, 5, 0).len(), 5);
    }

    #[test]
    fn dates_are_real_days() {
        assert_eq!(parse_month_day("10-28"), Some((10, 28)));
        assert_eq!(parse_month_day("02-29"), Some((2, 29)));
        for bad in [
            "",
            "1-5",
            "13-01",
            "00-10",
            "04-31",
            "02-30",
            "10/28",
            "10-28-1",
            "ab-cd",
            "2026-10-28",
        ] {
            assert_eq!(parse_month_day(bad), None, "{bad:?}");
        }
    }
}
