//! A small in-memory Todoist API v1 for local development and screenshots:
//! `/api/v1/sync` (full and incremental sync tokens, item commands with
//! temp ids), `/api/v1/user`, `/api/v1/tasks/completed/by_completion_date`.
//! Any Bearer token is accepted. Seeded with recurring language routines.
//!
//!   cargo run -p secretary-server --example mock_todoist -- 127.0.0.1:8898
//!
//! Then run secretary-server with
//! `SECRETARY_TODOIST_API_URL=http://127.0.0.1:8898` and connect any token
//! of 20+ characters in Links.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, NaiveDate, Utc};
use serde_json::{json, Value};

#[derive(Default)]
struct Db {
    version: u64,
    items: BTreeMap<String, (Value, u64)>,
    done: Vec<Value>,
    seen: std::collections::HashSet<String>,
    next: u64,
}

type S = Arc<Mutex<Db>>;

fn today() -> NaiveDate {
    Utc::now()
        .with_timezone(&chrono_tz::Europe::Copenhagen)
        .date_naive()
}

fn day(n: i64) -> String {
    (today() + Duration::days(n)).format("%Y-%m-%d").to_string()
}

impl Db {
    fn put(&mut self, mut item: Value) {
        self.version += 1;
        item["updated_at"] = json!(format!(
            "2026-10-22T08:00:{:02}.{:06}Z",
            self.version % 60,
            self.version
        ));
        let id = item["id"].as_str().unwrap_or_default().to_owned();
        self.items.insert(id, (item, self.version));
    }

    fn get(&self, id: &str) -> Option<Value> {
        self.items.get(id).map(|(v, _)| v.clone())
    }
}

#[allow(clippy::too_many_arguments)]
fn item(
    id: &str,
    content: &str,
    project: &str,
    section: Option<&str>,
    parent: Option<&str>,
    prio: u8,
    due: Value,
    labels: &[&str],
    desc: &str,
) -> Value {
    json!({"id": id, "content": content, "description": desc, "project_id": project, "section_id": section,
           "parent_id": parent, "priority": prio, "due": due, "labels": labels, "child_order": id.trim_start_matches(|c: char| c.is_alphabetic()).parse::<i64>().unwrap_or(1),
           "day_order": -1, "checked": false, "is_deleted": false, "added_at": "2026-10-01T08:00:00Z", "completed_at": null})
}

fn due(date: String, string: &str, recurring: bool) -> Value {
    json!({"date": date, "string": string, "lang": "en", "is_recurring": recurring, "timezone": null})
}

fn seed(db: &mut Db) {
    let rows = vec![
        item(
            "r1",
            "15 min shadowing",
            "lang",
            Some("s_speak"),
            None,
            3,
            due(day(0), "every day", true),
            &["lingue", "routine"],
            "",
        ),
        item(
            "r2",
            "10 min anki/memrise",
            "lang",
            Some("s_rip"),
            None,
            2,
            due(day(-2), "every day", true),
            &["lingue", "routine"],
            "Danish deck first, then Italian idioms",
        ),
        item(
            "r3",
            "20min speaking",
            "lang",
            Some("s_speak"),
            None,
            4,
            due(format!("{}T18:30:00", day(0)), "every day at 18:30", true),
            &["lingue"],
            "Italki or a voice note to Marco",
        ),
        item(
            "r4",
            "10 min duolingo",
            "lang",
            None,
            None,
            1,
            due(day(0), "every day", true),
            &["routine"],
            "",
        ),
        item(
            "r5",
            "Brain Training",
            "inbox1",
            None,
            None,
            1,
            due(day(-1), "every day", true),
            &["routine"],
            "",
        ),
        item(
            "t1",
            "Call Marco about the CRAFT-IT meeting",
            "work",
            None,
            None,
            4,
            due(format!("{}T10:00:00", day(1)), "tomorrow at 10", false),
            &[],
            "Paid hours, Excel survey",
        ),
        item(
            "t2",
            "Pay rent",
            "home",
            None,
            None,
            3,
            due(day(9), "every 1", true),
            &["casa"],
            "",
        ),
        item(
            "t3",
            "Buy a birthday gift for mum",
            "home",
            None,
            None,
            1,
            due(day(3), "in 3 days", false),
            &["casa"],
            "",
        ),
        item(
            "t4",
            "Review Danish verb groups",
            "lang",
            Some("s_rip"),
            None,
            1,
            due(day(4), "monday", false),
            &["lingue"],
            "",
        ),
        item(
            "t5",
            "Group 1: -e verbs",
            "lang",
            Some("s_rip"),
            Some("t4"),
            1,
            Value::Null,
            &[],
            "",
        ),
        item(
            "t6",
            "Group 2: irregular",
            "lang",
            Some("s_rip"),
            Some("t4"),
            1,
            Value::Null,
            &[],
            "",
        ),
        item(
            "t7",
            "Book the padel court",
            "inbox1",
            None,
            None,
            1,
            Value::Null,
            &[],
            "",
        ),
        item(
            "t8",
            "Renew the residence card",
            "inbox1",
            None,
            None,
            3,
            due(day(6), "next week", false),
            &[],
            "Bring the passport and two photos",
        ),
        item(
            "t9",
            "Plan Q4 goals",
            "work",
            None,
            None,
            2,
            due(day(2), "saturday", false),
            &[],
            "",
        ),
    ];
    for r in rows {
        db.put(r);
    }
    let now = Utc::now();
    for (i, (id, content)) in [
        ("r1", "15 min shadowing"),
        ("r4", "10 min duolingo"),
        ("r1", "15 min shadowing"),
        ("r3", "20min speaking"),
        ("r1", "15 min shadowing"),
    ]
    .iter()
    .enumerate()
    {
        let at = now - Duration::days(i as i64 + 1) - Duration::hours(2);
        db.done.push(json!({"id": id, "content": content, "project_id": "lang", "completed_at": at.to_rfc3339(), "due": {"is_recurring": true}}));
    }
    db.done.push(json!({"id": "x1", "content": "Send the CV to Aarhus University", "project_id": "work", "completed_at": (now - Duration::hours(3)).to_rfc3339()}));
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

async fn sync(State(s): State<S>, body: String) -> Response {
    let f = form(&body);
    let mut db = s.lock().unwrap_or_else(|e| e.into_inner());
    let token = f.get("sync_token").cloned().unwrap_or_else(|| "*".into());
    let since: u64 = token.trim_start_matches('v').parse().unwrap_or(0);
    let mut status = serde_json::Map::new();
    let mut mapping = serde_json::Map::new();
    if let Some(cmds) = f.get("commands") {
        let cmds: Vec<Value> = serde_json::from_str(cmds).unwrap_or_default();
        for c in cmds {
            let uuid = c["uuid"].as_str().unwrap_or_default().to_owned();
            if !db.seen.insert(uuid.clone()) {
                status.insert(uuid, json!("ok"));
                continue;
            }
            let a = &c["args"];
            let resolve = |m: &serde_json::Map<String, Value>, id: &str| {
                m.get(id).and_then(Value::as_str).unwrap_or(id).to_owned()
            };
            let id = a["id"].as_str().map(|x| resolve(&mapping, x));
            let ok = match c["type"].as_str().unwrap_or_default() {
                "item_add" => {
                    db.next += 1;
                    let new = format!("n{}", db.next);
                    let string = a["due"]["string"].as_str().unwrap_or("");
                    let d = if a["due"].is_null() {
                        Value::Null
                    } else if let Some(date) = a["due"]["date"].as_str() {
                        due(date.to_owned(), date, false)
                    } else {
                        let rec = string.starts_with("every")
                            || string.starts_with("ogni")
                            || string.starts_with("hver");
                        let date = if string.contains("domani") || string.contains("tomorrow") {
                            day(1)
                        } else {
                            day(0)
                        };
                        due(date, string, rec)
                    };
                    let project = a["project_id"].as_str().unwrap_or("inbox1").to_owned();
                    let mut it = item(
                        &new,
                        a["content"].as_str().unwrap_or(""),
                        &project,
                        a["section_id"].as_str(),
                        a["parent_id"].as_str(),
                        a["priority"].as_u64().unwrap_or(1) as u8,
                        d,
                        &[],
                        a["description"].as_str().unwrap_or(""),
                    );
                    it["labels"] = a["labels"].clone();
                    if it["labels"].is_null() {
                        it["labels"] = json!([]);
                    }
                    db.put(it);
                    if let Some(t) = c["temp_id"].as_str() {
                        mapping.insert(t.to_owned(), json!(new));
                    }
                    true
                }
                "item_update"
                | "item_close"
                | "item_uncomplete"
                | "item_delete"
                | "item_move"
                | "item_update_date_complete" => {
                    let Some(mut it) = id.as_deref().and_then(|i| db.get(i)) else {
                        status.insert(uuid, json!({"error_tag": "ITEM_NOT_FOUND", "error": "Item not found", "http_code": 400}));
                        continue;
                    };
                    match c["type"].as_str().unwrap_or_default() {
                        "item_update" => {
                            for (k, v) in a.as_object().into_iter().flatten() {
                                if k == "due" && !v.is_null() {
                                    let rec = v["is_recurring"].as_bool().unwrap_or(false);
                                    let date = v["date"]
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| day(1));
                                    let string = v["string"].as_str().unwrap_or(&date).to_owned();
                                    it["due"] = due(date, &string, rec);
                                } else if k != "id" {
                                    it[k] = v.clone();
                                }
                            }
                        }
                        "item_close" => {
                            db.done.push(json!({"id": it["id"], "content": it["content"], "project_id": it["project_id"], "completed_at": Utc::now().to_rfc3339(), "due": it["due"]}));
                            if it["due"]["is_recurring"].as_bool().unwrap_or(false) {
                                let cur = it["due"]["date"].as_str().unwrap_or_default().to_owned();
                                let (d, t) = cur.split_at(10.min(cur.len()));
                                let next = NaiveDate::parse_from_str(d, "%Y-%m-%d")
                                    .map(|x| x.max(today()) + Duration::days(1))
                                    .unwrap_or(today());
                                it["due"]["date"] =
                                    json!(format!("{}{t}", next.format("%Y-%m-%d")));
                            } else {
                                it["checked"] = json!(true);
                                it["completed_at"] = json!(Utc::now().to_rfc3339());
                            }
                        }
                        "item_uncomplete" => it["checked"] = json!(false),
                        "item_delete" => it["is_deleted"] = json!(true),
                        "item_move" => {
                            for k in ["project_id", "section_id", "parent_id"] {
                                if let Some(v) = a.get(k) {
                                    it[k] = v.clone();
                                }
                            }
                        }
                        _ => {
                            if let Some(d) = a.get("due") {
                                it["due"] = d.clone();
                            }
                        }
                    }
                    db.put(it);
                    true
                }
                _ => true,
            };
            if ok {
                status.insert(uuid, json!("ok"));
            }
        }
    }
    let full = since == 0;
    let items: Vec<Value> = db
        .items
        .values()
        .filter(|(it, v)| {
            if full {
                it["checked"] != true && it["is_deleted"] != true
            } else {
                *v > since
            }
        })
        .map(|(it, _)| it.clone())
        .collect();
    let today_done = db
        .done
        .iter()
        .filter(|d| {
            d["completed_at"]
                .as_str()
                .is_some_and(|t| t.starts_with(&day(0)))
        })
        .count();
    let mut out = json!({
        "sync_token": format!("v{}", db.version),
        "full_sync": full,
        "items": items,
        "user": {"email": "owner@example.com", "full_name": "Owner", "daily_goal": 6, "weekly_goal": 30, "lang": "it", "start_day": 1, "next_week": 1, "tz_info": {"timezone": "Europe/Copenhagen"}},
        "stats": {"days_items": [{"date": day(0), "total_completed": today_done}]},
    });
    if full {
        out["projects"] = json!([
            {"id": "inbox1", "name": "Inbox", "inbox_project": true, "child_order": 0, "color": "charcoal"},
            {"id": "lang", "name": "Lingue", "child_order": 1, "color": "sky_blue", "is_favorite": true},
            {"id": "work", "name": "Work", "child_order": 2, "color": "violet"},
            {"id": "home", "name": "Home", "child_order": 3, "color": "lime_green"}
        ]);
        out["sections"] = json!([
            {"id": "s_speak", "project_id": "lang", "name": "Speaking", "section_order": 1},
            {"id": "s_rip", "project_id": "lang", "name": "Ripasso", "section_order": 2}
        ]);
        out["labels"] = json!([
            {"id": "l1", "name": "lingue", "color": "sky_blue", "item_order": 1},
            {"id": "l2", "name": "routine", "color": "orange", "item_order": 2},
            {"id": "l3", "name": "casa", "color": "lime_green", "item_order": 3}
        ]);
        out["filters"] = json!([
            {"id": "f1", "name": "Lingue oggi", "query": "#Lingue & (today | overdue)", "color": "sky_blue", "item_order": 1},
            {"id": "f2", "name": "Urgent", "query": "p1 | p2", "color": "red", "item_order": 2}
        ]);
    }
    if !status.is_empty() {
        out["sync_status"] = Value::Object(status);
        out["temp_id_mapping"] = Value::Object(mapping);
    }
    Json(out).into_response()
}

async fn user() -> Json<Value> {
    Json(json!({"id": "1", "email": "owner@example.com", "full_name": "Owner", "daily_goal": 6}))
}

async fn completed(State(s): State<S>) -> Json<Value> {
    let db = s.lock().unwrap_or_else(|e| e.into_inner());
    Json(json!({"items": db.done, "next_cursor": null}))
}

#[tokio::main]
async fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8898".into());
    let mut db = Db::default();
    seed(&mut db);
    let app = Router::new()
        .route("/api/v1/sync", post(sync))
        .route("/api/v1/user", get(user))
        .route("/api/v1/tasks/completed/by_completion_date", get(completed))
        .route("/api/v1/revoke", post(|| async { StatusCode::OK }))
        .with_state(Arc::new(Mutex::new(db)));
    let l = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    println!("mock Todoist on http://{addr}");
    axum::serve(l, app).await.expect("serve");
}
