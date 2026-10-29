//! Test harness: a mock secretary-server and a running secretary-mcp.
#![allow(dead_code)]

pub mod agents;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub const DEVICE_TOKEN: &str = "sec_test_device_token";
pub const SERVICE_TOKEN: &str = "svc_test_service_token";
pub const OWNER_EMAIL: &str = "owner@example.com";
pub const OWNER_PASSWORD: &str = "correct horse battery";

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Default)]
pub struct MockState {
    pub log: Vec<Recorded>,
    pub v2: bool,
    pub items: Vec<Value>,
    pub legacy: Vec<Value>,
    pub revision: u64,
    pub notes: Vec<Value>,
    /// The label registry (`{tag, name}`), as `GET /v1/notes/labels` keeps it.
    pub labels: Vec<Value>,
    /// Counter for item ids and `updatedAt` stamps.
    pub seq: u64,
    /// Simulated concurrent edits: the next n PATCHes that carry `baseUpdatedAt`
    /// find the note changed by somebody else (409).
    pub race: u32,
    /// Open tasks (task views as `/v1/tasks/state` returns them).
    pub tasks: Vec<Value>,
    /// Lucina Agents snapshots (`common::agents`).
    pub agents: Vec<Value>,
    /// Agent schedules; `None`: the route is not there yet (phase 6).
    pub schedules: Option<Vec<Value>>,
    /// agentd's `/agents/search` answer (phase 6); `None` answers as an older agentd.
    pub search: Option<Value>,
}

#[derive(Clone)]
pub struct Mock {
    pub state: Arc<Mutex<MockState>>,
    pub url: String,
}

impl Mock {
    pub fn requests(&self, path: &str) -> Vec<Recorded> {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }
    pub fn set_v2(&self, on: bool) {
        self.state.lock().unwrap().v2 = on;
    }
}

fn err(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": code}})),
    )
        .into_response()
}

fn device_ok(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {DEVICE_TOKEN}"))
}

// -- notes (Keep) ------------------------------------------------------------------

fn norm_tag(s: &str) -> String {
    s.trim()
        .trim_start_matches('#')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn stamp(seq: &mut u64) -> String {
    *seq += 1;
    format!("2026-10-22T11:30:00.{:03}Z", *seq)
}

fn derive(items: &[Value]) -> String {
    items
        .iter()
        .map(|i| {
            let pad = if i["indent"].as_u64() == Some(1) {
                "  "
            } else {
                ""
            };
            let mark = if i["checked"] == true { "x" } else { " " };
            format!("{pad}- [{mark}] {}", i["text"].as_str().unwrap_or(""))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_lines(content: &str, seq: &mut u64) -> Vec<Value> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let t = l.trim();
            let (checked, text) = if let Some(r) = t.strip_prefix("- [x] ") {
                (true, r)
            } else if let Some(r) = t.strip_prefix("- [ ] ") {
                (false, r)
            } else {
                (false, t)
            };
            *seq += 1;
            json!({"id": format!("i_{:012x}", *seq), "text": text, "checked": checked, "indent": 0})
        })
        .collect()
}

/// What secretary-server does with the fields of a POST/PATCH body.
fn apply_note_body(n: &mut Value, body: &Value, labels: &mut Vec<Value>, seq: &mut u64) {
    for f in ["title", "content", "pinned", "color", "archived", "source"] {
        if let Some(v) = body.get(f) {
            n[f] = v.clone();
        }
    }
    if let Some(r) = body.get("reminder") {
        if r.is_null() {
            n.as_object_mut().unwrap().remove("reminder");
        } else {
            n["reminder"] = json!({"at": r["at"], "recurrence": r.get("recurrence").cloned().unwrap_or(Value::Null), "lucinaId": "r_mock"});
        }
    }
    for f in ["tags", "labels"] {
        if let Some(arr) = body.get(f).and_then(Value::as_array) {
            let mut tags = Vec::new();
            for l in arr.iter().filter_map(Value::as_str) {
                let tag = norm_tag(l);
                if !labels.iter().any(|r| r["tag"] == tag.as_str()) {
                    labels.push(json!({"tag": tag, "name": l.trim().trim_start_matches('#')}));
                }
                tags.push(json!(tag));
            }
            n["tags"] = json!(tags);
        }
    }
    if let Some(items) = body.get("items").and_then(Value::as_array) {
        let items: Vec<Value> = items
            .iter()
            .map(|i| {
                let mut i = i.clone();
                if i.get("id").is_none() {
                    *seq += 1;
                    i["id"] = json!(format!("i_{:012x}", *seq));
                }
                i["checked"] = json!(i["checked"] == true);
                i["indent"] = json!(i["indent"].as_u64().unwrap_or(0));
                i
            })
            .collect();
        n["content"] = json!(derive(&items));
        n["items"] = json!(items);
    } else if body.get("content").is_some() && n.get("items").is_some() {
        let items = parse_lines(body["content"].as_str().unwrap_or(""), seq);
        n["content"] = json!(derive(&items));
        n["items"] = json!(items);
    }
    match body.get("convert").and_then(Value::as_str) {
        Some("list") if n.get("items").is_none() => {
            let items = parse_lines(n["content"].as_str().unwrap_or(""), seq);
            n["content"] = json!(derive(&items));
            n["items"] = json!(items);
        }
        Some("text") if n.get("items").is_some() => {
            let lines: Vec<String> = n["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["text"].as_str().unwrap_or("").to_owned())
                .collect();
            n["content"] = json!(lines.join("\n"));
            n.as_object_mut().unwrap().remove("items");
        }
        _ => {}
    }
}

fn label_name(labels: &[Value], tag: &str) -> String {
    labels
        .iter()
        .find(|l| l["tag"] == tag)
        .and_then(|l| l["name"].as_str())
        .unwrap_or(tag)
        .to_owned()
}

fn query_pairs(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.replace('+', " ").replace("%20", " ")))
        .collect()
}

fn note_summary(n: &Value, labels: &[Value]) -> Value {
    let mut s = n.clone();
    let c = s["content"].as_str().unwrap_or("").to_owned();
    let o = s.as_object_mut().unwrap();
    o.remove("content");
    o.remove("items");
    s["size"] = json!(c.len());
    s["excerpt"] = json!(c.chars().take(220).collect::<String>());
    if let Some(items) = n.get("items").and_then(Value::as_array) {
        s["isList"] = json!(true);
        s["itemsTotal"] = json!(items.len());
        s["itemsDone"] = json!(items.iter().filter(|i| i["checked"] == true).count());
    }
    if let Some(at) = n.get("reminder").and_then(|r| r.get("at")) {
        s["reminderAt"] = at.clone();
    }
    let names: Vec<String> = n["tags"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .map(|t| label_name(labels, t))
                .collect()
        })
        .unwrap_or_default();
    s["labels"] = json!(names);
    s
}

fn library_highlight() -> Value {
    json!({"id":"hl_0123456789abcdef","bookId":"bk_0123456789abcdef","text":"A thought","note":null,"page":"12","location":{"start":170,"end":172},"source":"kindle-clippings"})
}
fn library_brief() -> Value {
    json!({"id":"br_a","kind":"extractive","text":"A thought","basedOn":["hl_0123456789abcdef"]})
}

async fn handle(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or("").to_owned();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.state.lock().unwrap().log.push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        headers: headers.clone(),
        body: body.clone(),
    });
    let mut st = m.state.lock().unwrap();
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), seg.as_slice()) {
        // Lucina Agents (crate::agents), forwarded to agentd by the real server.
        (m, ["v1", "agents", rest @ ..]) => agents::route(&mut st, m, rest, &query, &headers, &body),
        // Lucina's computer (crate::computer): a job that finishes on the first poll.
        ("POST", ["v1", "computer", "task"]) if device_ok(&headers) => (
            StatusCode::ACCEPTED,
            Json(json!({"jobId": "job0123abc", "status": "running", "elapsedMs": 0})),
        )
            .into_response(),
        ("GET", ["v1", "computer", "task", "job0123abc"]) if device_ok(&headers) => Json(json!({
            "jobId": "job0123abc", "status": "done", "elapsedMs": 12000,
            "result": {"status": "completed", "reply": "The title is Example Domain.", "executedTools": ["terminal"]},
        }))
        .into_response(),
        // AU Find (crate::au): a person, a building, a map; "down" answers as AU Find unreachable.
        ("GET", ["v1", "au", "people"]) if device_ok(&headers) => {
            if query.contains("q=down") {
                return err(StatusCode::SERVICE_UNAVAILABLE, "au_unavailable");
            }
            Json(json!({"source": "aufind", "status": "found", "query": "Jens Hansen", "total": 1,
                "tried": [{"q": "Jens Hansen", "total": 1}],
                "people": [{"uuid": "u1", "name": "Jens Hansen", "position": "Lektor", "units": ["Datalogi"],
                    "email": "jh@cs.au.dk", "phone": "+45 8715 0000", "au_room": "1532-123",
                    "profile_url": "https://www.au.dk/en/jh@cs", "source": "aufind"}]}))
            .into_response()
        }
        ("GET", ["v1", "au", "buildings"]) if device_ok(&headers) => Json(json!({"source": "aufind", "status": "found", "query": "1532",
            "buildings": [{"id": "1532", "names": ["Nygaard"], "address": ["Åbogade 34"], "latitude": 56.17, "longitude": 10.19}]}))
        .into_response(),
        ("GET", ["v1", "au", "map.png"]) if device_ok(&headers) => (
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            vec![0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        )
            .into_response(),
        ("POST", ["v1", "computer", "call"]) if device_ok(&headers) => match body["op"].as_str() {
            Some("screenshot") => Json(json!({"text": "Visible windows: pid=1 window_id=2", "image": {"mimeType": "image/png", "data": "iVBORw0KGgo="}})).into_response(),
            Some("bash") if body["sandbox"] == "android" => err(StatusCode::CONFLICT, "owner_driving"),
            Some("bash") => Json(json!({"text": "hi\n", "image": null})).into_response(),
            _ => Json(json!({"text": "ok", "image": null})).into_response(),
        },
        ("GET", ["v1", "health"]) => {
            Json(json!({"ok": true, "version": "0.1.0", "lucina": {"reachable": true, "latencyMs": 12, "apiVersion": 1}}))
                .into_response()
        }
        ("POST", ["v1", "session"]) => {
            if body["email"] == OWNER_EMAIL && body["password"] == OWNER_PASSWORD {
                (
                    [("set-cookie", "sec_session=sess123; HttpOnly; Secure; SameSite=Strict; Path=/")],
                    Json(json!({"authenticated": true, "deviceName": "web"})),
                )
                    .into_response()
            } else {
                err(StatusCode::UNAUTHORIZED, "invalid_credentials")
            }
        }
        ("DELETE", ["v1", "session"]) => Json(json!({"authenticated": false, "deviceName": null})).into_response(),
        ("GET", ["v1", "insights", "summary"]) => {
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {SERVICE_TOKEN}")) {
                return err(StatusCode::UNAUTHORIZED, "unauthorized");
            }
            Json(json!({"generated_ms": 1, "timezone": "Europe/Rome", "gym": {
                "unit": "kg", "weekly_goal": 3, "workouts_this_week": 2,
                "recent_workouts": [{"id": "w1", "title": "Push", "date": "2026-10-21", "duration_min": 55,
                    "completed_sets": 18, "volume_label": "4,120 kg", "exercises": ["Bench Press", "Dips"],
                    "prs": [{"exercise_id": "bench-press", "name": "Bench Press", "label": "82.5 kg × 5"}]}],
                "weekly_volume": [{"week_start": "2026-10-19", "workouts": 2, "volume_label": "8,000 kg"}]},
                "body": {}}))
            .into_response()
        }
        _ if !device_ok(&headers) => err(StatusCode::UNAUTHORIZED, "unauthorized"),
        ("GET", ["v1", "library", "books"]) => Json(json!({"books":[{"id":"bk_0123456789abcdef","title":"Book","authors":["Writer"],"status":"unknown"}]})).into_response(),
        ("GET", ["v1", "library", "search"]) => Json(json!({"results":[{"bookId":"bk_0123456789abcdef","title":"Book","authors":["Writer"],"highlightId":"hl_0123456789abcdef","snippet":"A thought","page":"12","location":{"start":170,"end":172},"source":"kindle-clippings"}]})).into_response(),
        ("GET", ["v1", "library", "recent"]) => Json(json!({"highlights":[library_highlight()],"changes":[],"sessions":[],"readingSessions":"unavailable_on_stock_kindle"})).into_response(),
        ("GET", ["v1", "library", "sync"]) => Json(json!({"imports":[],"books":1,"highlights":1})).into_response(),
        ("GET", ["v1", "library", "books", id]) => Json(json!({"book":{"id":id,"title":"Book","authors":["Writer"],"status":"unknown","progress":null,"relatedNotes":[],"briefs":[library_brief()]},"highlights":[library_highlight()],"sessions":[]})).into_response(),
        ("GET", ["v1", "library", "books", id, "notes"]) => Json(if *id == "bk_0123456789abcdef" {
            json!({"bookId":id,"total":1,"notes":[{"vault":"Vault","bookId":id,"notePath":"Reading/Book.md","noteTitle":"Book","how":"frontmatter","excerpt":"title: Book","line":2}],"indexedVaults":[{"vault":"Vault","generatedAt":"2026-10-24T10:00:00Z"}]})
        } else {
            json!({"bookId":id,"total":0,"notes":[],"indexedVaults":[]})
        }).into_response(),
        ("GET", ["v1", "library", "books", _, "highlights"]) => Json(json!({"highlights":[library_highlight()]})).into_response(),
        ("POST", ["v1", "library", "books", _, "brief"]) => Json(library_brief()).into_response(),
        ("GET", ["v1", "session"]) => Json(json!({"authenticated": true, "deviceName": "lucina-mcp"})).into_response(),
        ("POST", ["v1", "decisions"]) => Json(json!({"decision_id":"dc-012345abcdef", "status":"pending"})).into_response(),
        ("GET", ["v1", "decisions", id]) => Json(json!({"decision_id":id, "status":"decided", "decision":"go", "chosen_option":"Ship", "reason":"Small fix", "decided_by":"lucina", "confidence":0.86})).into_response(),
        ("DELETE", ["v1", "decisions", id]) => Json(json!({"decision_id":id, "status":"withdrawn", "reason":"Withdrawn by the asker: settled elsewhere"})).into_response(),
        ("GET", ["v1", "jobs", id]) => Json(json!({"id":id,"status":"done","action":"research","result":"Found it","error":null,"createdAt":"t0","startedAt":"t1","finishedAt":"t2"})).into_response(),
        ("POST", ["v1", "chat"]) => {
            let msg = body["message"].as_str().unwrap_or("");
            Json(json!({"reply": format!("Ciao! You said: {msg}"), "action": "reply", "job": null,
                "sessionId": format!("secretary_{}", body["sessionId"].as_str().unwrap_or("app")),
                "clientMessageId": body["clientMessageId"], "userItemId": "tl_u", "lucinaItemId": "tl_l"}))
            .into_response()
        }
        ("GET", ["v1", "timeline"]) => Json(json!({"items": [
            {"id": "tl_3", "ts": 1_792_602_906_000_i64, "channel": "telegram", "role": "lucina", "text": "Fatto, latte aggiunto."},
            {"id": "tl_2", "ts": 1_792_602_900_000_i64, "channel": "telegram", "role": "user", "text": "Aggiungi il latte"},
            {"id": "tl_1", "ts": 1_792_602_000_000_i64, "channel": "web", "role": "user", "text": "Ciao"}
        ], "nextBefore": "tl_1"}))
        .into_response(),
        ("GET", ["v1", "shopping", "lists"]) => {
            if !st.v2 {
                return err(StatusCode::NOT_FOUND, "not_found");
            }
            Json(json!([{"id": "default", "name": "Casa", "theme": "home", "needed": 2, "recent": 1},
                        {"id": "work", "name": "Ufficio", "theme": "work", "needed": 0, "recent": 0}]))
            .into_response()
        }
        ("GET", ["v1", "shopping", "lists", id]) => {
            if !st.v2 {
                return err(StatusCode::BAD_GATEWAY, "lucina_error");
            }
            if *id != "default" && *id != "work" {
                return err(StatusCode::NOT_FOUND, "list_not_found");
            }
            let items = if *id == "default" { st.items.clone() } else { vec![] };
            Json(json!({"list": {"id": id, "name": if *id == "default" {"Casa"} else {"Ufficio"}, "theme": "home"},
                "revision": st.revision, "items": items}))
            .into_response()
        }
        ("POST", ["v1", "shopping", "lists", _id, "ops"]) => {
            if !st.v2 {
                return err(StatusCode::BAD_GATEWAY, "lucina_error");
            }
            let (mut applied, mut rejected) = (vec![], vec![]);
            for op in body["ops"].as_array().cloned().unwrap_or_default() {
                // Lucina's Jev gate on MCP batches: not a shopping item, ask instead
                if body["via"] == "mcp" && op["name"] == "lavatrici" {
                    rejected.push(json!({"opId": op["opId"], "error": "needs_confirmation",
                        "text": "lavatrici", "question": "Intendevi Detersivo per lavatrice?",
                        "suggestion": "Detersivo per lavatrice"}));
                    continue;
                }
                applied.push(op["opId"].clone());
                match op["type"].as_str() {
                    Some("add") => {
                        let n = st.items.len();
                        st.items.push(json!({"id": format!("item{n}"), "name": op["name"], "spec": op.get("spec").cloned().unwrap_or(Value::Null), "status": "needed"}));
                    }
                    Some("purchase") => {
                        for it in st.items.iter_mut() {
                            if it["id"] == op["itemId"] {
                                it["status"] = json!("recent");
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !applied.is_empty() {
                st.revision += 1;
            }
            Json(json!({"revision": st.revision, "applied": applied, "duplicates": [],
                "rejected": rejected, "items": st.items}))
            .into_response()
        }
        ("GET", ["v1", "shopping"]) => Json(Value::Array(st.legacy.clone())).into_response(),
        ("POST", ["v1", "shopping"]) => {
            let n = st.legacy.len();
            let item = json!({"id": format!("l{n}"), "rawName": body["rawName"], "product": body["rawName"], "status": "needed"});
            st.legacy.push(item.clone());
            (StatusCode::CREATED, Json(item)).into_response()
        }
        ("PATCH", ["v1", "shopping", id]) => {
            for it in st.legacy.iter_mut() {
                if it["id"] == *id {
                    it["status"] = body["status"].clone();
                }
            }
            Json(json!({"id": id, "product": "x", "status": body["status"]})).into_response()
        }
        ("GET", ["v1", "reminders"]) => Json(json!({"scope": "upcoming", "includeSent": false, "timeZone": "Europe/Rome",
            "reminders": [{"id": "r1", "text": "Call the dentist", "dueAt": "2026-10-22T07:30:00.000Z", "timeZone": "Europe/Rome",
                           "status": "pending", "source": "secretary"}]}))
        .into_response(),
        ("POST", ["v1", "reminders"]) => {
            if body.get("naturalText").is_some_and(|t| t.as_str().unwrap_or("").contains("never")) {
                return err(StatusCode::BAD_REQUEST, "unparseable_reminder");
            }
            let text = body.get("text").or(body.get("naturalText")).cloned().unwrap_or(Value::Null);
            (StatusCode::CREATED, Json(json!({"created": true, "reminder": {"id": "r2", "text": text,
                "dueAt": body.get("dueAt").cloned().unwrap_or(json!("2026-10-22T07:30:00.000Z")), "timeZone": "Europe/Rome", "status": "pending"}})))
            .into_response()
        }
        ("DELETE", ["v1", "reminders", id]) => {
            if *id != "r1" {
                return err(StatusCode::NOT_FOUND, "reminder_not_found");
            }
            Json(json!({"cancelled": true, "reminder": {"id": "r1", "text": "Call the dentist", "status": "cancelled"}})).into_response()
        }
        ("GET", ["v1", "agenda"]) => Json(json!({"date": "2026-10-22", "timeZone": "Europe/Rome",
            "reminders": [{"id": "r1", "text": "Call the dentist", "dueAt": "2026-10-22T07:30:00.000Z", "status": "pending"}],
            "briefing": "Tomorrow starts at 9:30 with the dentist call.", "briefingCached": false}))
        .into_response(),
        ("GET", ["v1", "calendar", "events"]) => Json(json!({"from": "2026-10-21T22:00:00.000Z", "to": "2026-10-28T23:00:00.000Z",
            "timeZone": "Europe/Rome", "events": [
                {"id": "google:abc", "source": "google", "sourceId": "abc", "calendar": "Google", "title": "Dentist",
                 "start": "2026-10-22T07:30:00.000Z", "end": "2026-10-22T08:00:00.000Z", "allDay": false, "location": "Via Roma 1",
                 "startMs": 1, "endMs": 2, "recurring": false, "editable": true},
                {"id": "iphone:EK1", "source": "iphone", "sourceId": "EK1", "calendar": "Casa", "title": "Birthday",
                 "start": "2026-10-23", "end": "2026-10-24", "allDay": true, "startMs": 3, "endMs": 4, "recurring": false, "editable": false}],
            "sources": {"google": {"status": "reconnect"}}}))
        .into_response(),
        ("POST", ["v1", "calendar", "proposals"]) => {
            if body["start"] == "bad" {
                return err(StatusCode::BAD_REQUEST, "bad_request");
            }
            (StatusCode::CREATED, Json(json!({"id": "prop_1", "event": body, "from": body["from"]}))).into_response()
        }
        ("POST", ["v1", "calendar", "events"]) => panic!("the MCP must never create events directly"),
        ("GET", ["v1", "gym", "state"]) => Json(json!({"ok": true, "replayed": false, "view": {"summary": {
            "workouts": 10, "completed_sets": 200, "weekly_workouts": 1, "weekly_goal": 3, "volume": "40,000 kg"},
            "records": [{"name": "Squat", "weight": "100 kg", "reps": 5, "estimated_1rm": "116.7 kg"}], "state": {"active": null}}}))
        .into_response(),
        ("GET", ["v1", "body", "state"]) => Json(json!({"ok": true, "replayed": false, "view": {
            "stats": {"count": 2, "goal_reached": false},
            "stat_labels": {"current": "80.5 kg", "avg7": "80.6 kg", "goal": "78.0 kg", "goal_distance": "+2.6 kg"},
            "entries": [{"id": "e1", "weight_grams": 80500, "measured_ms": 1_792_602_906_883_i64, "note": "after run"}],
            "labels": {"e1": "80.5 kg"}}}))
        .into_response(),
        ("GET", ["v1", "notes"]) => {
            let st = &mut *st;
            let pairs = query_pairs(&query.to_lowercase());
            let get = |k: &str| pairs.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
            let hits: Vec<Value> = st
                .notes
                .iter()
                .filter(|n| {
                    pairs.iter().filter(|(k, _)| k == "q").all(|(_, w)| {
                        n["title"].as_str().unwrap_or("").to_lowercase().contains(w.as_str())
                            || n["content"].as_str().unwrap_or("").to_lowercase().contains(w.as_str())
                            || n["slug"].as_str().unwrap_or("").contains(w.as_str())
                    })
                })
                .filter(|n| match get("tag") {
                    Some(t) => n["tags"].as_array().is_some_and(|a| a.iter().any(|x| x == t.as_str())),
                    None => true,
                })
                .filter(|n| get("color").is_none_or(|c| n["color"].as_str().unwrap_or("default") == c))
                .filter(|n| match get("type").as_deref() {
                    Some("list") => n.get("items").is_some(),
                    Some("text") => n.get("items").is_none(),
                    Some("reminder") => n.get("reminder").is_some(),
                    _ => true,
                })
                .filter(|n| match get("archived").as_deref() {
                    Some("exclude") => n["archived"] != true,
                    Some("only") => n["archived"] == true,
                    _ => true,
                })
                .map(|n| note_summary(n, &st.labels))
                .collect();
            Json(json!({"notes": hits})).into_response()
        }
        ("GET", ["v1", "notes", "labels"]) => {
            let mut all: Vec<Value> = st.labels.clone();
            for n in &st.notes {
                for t in n["tags"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    if !all.iter().any(|l| l["tag"] == t) {
                        all.push(json!({"tag": t, "name": t}));
                    }
                }
            }
            let labels: Vec<Value> = all
                .iter()
                .map(|l| {
                    let count = st.notes.iter().filter(|n| n["tags"].as_array().is_some_and(|a| a.iter().any(|x| x == &l["tag"]))).count();
                    json!({"tag": l["tag"], "name": l["name"], "count": count})
                })
                .collect();
            Json(json!({"labels": labels})).into_response()
        }
        ("POST", ["v1", "notes"]) => {
            let st = &mut *st;
            let slug = body["slug"].as_str().map(str::to_owned).unwrap_or_else(|| {
                body["title"].as_str().unwrap_or("note").to_lowercase().replace(' ', "-")
            });
            if st.notes.iter().any(|n| n["slug"] == slug.as_str()) {
                return err(StatusCode::CONFLICT, "slug_taken");
            }
            let n = st.notes.len();
            let at = stamp(&mut st.seq);
            let mut note = json!({"id": format!("n_{n:020}"), "slug": slug, "title": "",
                "content": "", "tags": [], "source": "owner", "pinned": false, "color": "default", "archived": false,
                "createdAt": "2026-10-22T11:30:00.000Z", "updatedAt": at});
            apply_note_body(&mut note, &body, &mut st.labels, &mut st.seq);
            st.notes.push(note.clone());
            (StatusCode::CREATED, Json(note)).into_response()
        }
        ("GET", ["v1", "notes", key]) => match st.notes.iter().find(|n| n["slug"] == *key || n["id"] == *key) {
            Some(n) => Json(n.clone()).into_response(),
            None => err(StatusCode::NOT_FOUND, "note_not_found"),
        },
        ("PATCH", ["v1", "notes", key]) => {
            let st = &mut *st;
            let race = st.race > 0 && body.get("baseUpdatedAt").is_some();
            if race {
                st.race -= 1;
            }
            let at = stamp(&mut st.seq);
            match st.notes.iter_mut().find(|n| n["slug"] == *key || n["id"] == *key) {
                Some(n) => {
                    if race {
                        // Somebody else edited the note: a new item appears and updatedAt moves on.
                        st.seq += 1;
                        if let Some(items) = n.get_mut("items").and_then(Value::as_array_mut) {
                            items.push(json!({"id": format!("i_{:012x}", st.seq), "text": "Added elsewhere", "checked": false, "indent": 0}));
                        }
                        n["updatedAt"] = json!(at);
                        return err(StatusCode::CONFLICT, "conflict");
                    }
                    if let Some(base) = body.get("baseUpdatedAt") {
                        if *base != n["updatedAt"] {
                            return err(StatusCode::CONFLICT, "conflict");
                        }
                    }
                    if body.get("reminder").is_some_and(|r| !r.is_null()) && n["slug"] == "no-lucina" {
                        return err(StatusCode::BAD_GATEWAY, "lucina_unreachable");
                    }
                    apply_note_body(n, &body, &mut st.labels, &mut st.seq);
                    n["updatedAt"] = json!(at);
                    Json(n.clone()).into_response()
                }
                None => err(StatusCode::NOT_FOUND, "note_not_found"),
            }
        }
        ("POST", ["v1", "notes", key, "append"]) => match st.notes.iter_mut().find(|n| n["slug"] == *key || n["id"] == *key) {
            Some(n) => {
                let sep = body["separator"].as_str().unwrap_or("");
                let c = format!("{}{sep}{}", n["content"].as_str().unwrap_or(""), body["content"].as_str().unwrap_or(""));
                if c.len() > 256 * 1024 {
                    return err(StatusCode::PAYLOAD_TOO_LARGE, "note_too_large");
                }
                n["content"] = json!(c);
                Json(n.clone()).into_response()
            }
            None => err(StatusCode::NOT_FOUND, "note_not_found"),
        },
        ("GET", ["v1", "tasks", "state"]) => {
            if st.tasks.is_empty() {
                st.tasks = seed_tasks();
            }
            let q = query.to_lowercase().replace('+', " ").replace("%20", " ");
            let search = q.split('&').find_map(|kv| kv.strip_prefix("query=")).map(str::to_owned);
            Json(tasks_view(&st.tasks, search.as_deref())).into_response()
        }
        ("POST", ["v1", "tasks", "command"]) => {
            if st.tasks.is_empty() {
                st.tasks = seed_tasks();
            }
            let c = &body["command"];
            match c["type"].as_str() {
                Some("quick_add") => {
                    let text = c["text"].as_str().unwrap_or("");
                    let t = json!({"id": "tmp-1", "content": text, "project_name": "Inbox", "priority": 4, "labels": [], "pending": true});
                    st.tasks.push(t);
                    let mut v = tasks_view(&st.tasks, None);
                    v["task_id"] = json!("tmp-1");
                    v["queued"] = json!(true);
                    v["parsed"] = json!({"content": text, "due_string": "domani", "due_date": "2026-10-23", "is_recurring": false, "lang": "it", "labels": [], "spans": []});
                    Json(v).into_response()
                }
                Some("complete") => {
                    let id = c["task_id"].as_str().unwrap_or("");
                    let Some(pos) = st.tasks.iter().position(|t| t["id"] == id) else {
                        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "replayed": false, "code": "invalid_command", "error": "Task not found"}))).into_response();
                    };
                    let t = st.tasks.remove(pos);
                    if t["due"]["is_recurring"] == true {
                        let mut t = t;
                        t["due"]["rel_days"] = json!(1);
                        t["due"]["date"] = json!("2026-10-23");
                        st.tasks.push(t);
                    }
                    Json(tasks_view(&st.tasks, None)).into_response()
                }
                _ => (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"ok": false, "code": "invalid_command", "error": "unknown"}))).into_response(),
            }
        }
        _ => err(StatusCode::NOT_FOUND, "not_found"),
    }
}

fn seed_tasks() -> Vec<Value> {
    let due = |rel: i64, string: &str, rec: bool| json!({"date": format!("2026-10-{}", 22 + rel), "string": string, "is_recurring": rec, "rel_days": rel, "overdue": rel < 0});
    vec![
        json!({"id": "a2", "content": "10 min anki/memrise", "project_name": "Inbox", "priority": 4, "labels": ["lingue"], "due": due(-2, "every day", true), "pending": false}),
        json!({"id": "a1", "content": "15 min shadowing", "project_name": "Inbox", "priority": 4, "labels": [], "due": due(0, "every day", true), "pending": false}),
        json!({"id": "a5", "content": "20min speaking", "project_name": "Lingue", "priority": 1, "labels": [], "due": due(0, "today", false), "pending": false}),
        json!({"id": "a6", "content": "speaking club", "project_name": "Lingue", "priority": 4, "labels": [], "pending": false}),
    ]
}

fn tasks_view(tasks: &[Value], search: Option<&str>) -> Value {
    let pick = |f: &dyn Fn(&Value) -> bool| -> Vec<Value> {
        tasks.iter().filter(|t| f(t)).cloned().collect()
    };
    let groups = match search {
        Some(q) => vec![
            json!({"key": "none", "kind": "none", "title": "", "tasks": pick(&|t| t["content"].as_str().unwrap_or("").to_lowercase().contains(q))}),
        ],
        None => vec![
            json!({"key": "overdue", "kind": "overdue", "title": "Overdue", "tasks": pick(&|t| t["due"]["rel_days"].as_i64().is_some_and(|r| r < 0))}),
            json!({"key": "2026-10-22", "kind": "date", "title": "", "date": "2026-10-22", "rel_days": 0, "tasks": pick(&|t| t["due"]["rel_days"] == 0 || t["id"] == "tmp-1")}),
        ],
    };
    json!({"ok": true, "replayed": false, "view": {"revision": 3, "today": "2026-10-22", "timezone": "Europe/Copenhagen",
        "goal": {"daily_goal": 5, "completed_today": 1, "reached": false},
        "sync": {"status": "ok", "connected": true, "pending": 0},
        "list": {"kind": if search.is_some() { "search" } else { "today" }, "title": if search.is_some() { "Search" } else { "Today" }, "groups": groups, "count": 0}}})
}

/// CIMD document served by the mock (for the CIMD test).
async fn cimd(State(m): State<Mock>) -> Json<Value> {
    Json(json!({
        "client_id": format!("{}/oauth/client.json", m.url),
        "client_name": "ChatGPT",
        "redirect_uris": ["https://chatgpt.com/connector_platform_oauth_redirect"],
        "token_endpoint_auth_method": "private_key_jwt",
        "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"]
    }))
}

pub async fn start_mock() -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let mock = Mock {
        state: Arc::new(Mutex::new(MockState {
            v2: true,
            items: vec![
                json!({"id": "i1", "name": "Latte", "status": "needed"}),
                json!({"id": "i2", "name": "Banane", "spec": "6, mature", "status": "needed"}),
                json!({"id": "i3", "name": "Pane", "status": "recent"}),
            ],
            legacy: vec![
                json!({"id": "l0", "rawName": "latte", "product": "milk", "status": "needed"}),
            ],
            revision: 42,
            ..Default::default()
        })),
        url: format!("http://{addr}"),
    };
    let app = Router::new()
        .route("/oauth/client.json", get(cimd))
        .fallback(any(handle))
        .with_state(mock.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    mock
}

pub struct Harness {
    pub mock: Mock,
    pub base: String,
    pub dir: tempfile::TempDir,
    pub ct: CancellationToken,
    pub http: reqwest::Client,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.ct.cancel();
    }
}

pub async fn start_with(f: impl FnOnce(&mut secretary_mcp::Config, &Mock)) -> Harness {
    let mock = start_mock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg =
        secretary_mcp::Config::new("https://getsecretary.tech", dir.path().join("data")).unwrap();
    cfg.bind = "127.0.0.1:0".parse().unwrap();
    cfg.backend_url = mock.url.clone();
    cfg.backend_token = Some(DEVICE_TOKEN.into());
    cfg.service_token = Some(SERVICE_TOKEN.into());
    cfg.agents_token = Some(agents::AGENTS_TOKEN.into());
    f(&mut cfg, &mock);
    let (addr, ct, _h) = secretary_mcp::spawn(cfg).await.unwrap();
    Harness {
        mock,
        base: format!("http://{addr}"),
        dir,
        ct,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    }
}

pub async fn start() -> Harness {
    start_with(|_, _| {}).await
}

impl Harness {
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("data")
    }

    pub fn issue(&self, name: &str, scopes: &str) -> String {
        let store = secretary_mcp::tokens::TokenStore::new(self.data_dir());
        store
            .issue(
                name,
                secretary_mcp::scope::Scopes::parse_list(scopes).unwrap(),
            )
            .unwrap()
            .1
    }

    /// A token at a delegation depth; returns (token id, secret).
    pub fn issue_at(&self, name: &str, scopes: &str, depth: u8) -> (String, String) {
        let store = secretary_mcp::tokens::TokenStore::new(self.data_dir());
        let (rec, secret) = store
            .issue_at(
                name,
                secretary_mcp::scope::Scopes::parse_list(scopes).unwrap(),
                depth,
            )
            .unwrap();
        (rec.id, secret)
    }

    /// The lines of `agents-audit.jsonl`.
    pub fn agents_audit(&self) -> Vec<Value> {
        std::fs::read_to_string(self.data_dir().join("agents-audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// One JSON-RPC request to /mcp; returns (HTTP status, JSON-RPC message or Null).
    pub async fn rpc_raw(
        &self,
        token: Option<&str>,
        method: &str,
        params: Value,
    ) -> (u16, HeaderMap, Value) {
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base))
            .header(CONTENT_TYPE, "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let ct = headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let text = resp.text().await.unwrap();
        let v = if ct.contains("text/event-stream") {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                .rfind(|m| m.get("id").is_some())
                .unwrap_or(Value::Null)
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        (status, headers, v)
    }

    pub async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let (status, _, v) = self.rpc_raw(Some(token), method, params).await;
        assert_eq!(status, 200, "{method}: {v}");
        assert!(v.get("error").is_none(), "{method}: {v}");
        v["result"].clone()
    }

    /// tools/call → (is_error, text).
    pub async fn call(&self, token: &str, tool: &str, args: Value) -> (bool, String) {
        let r = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": args}),
            )
            .await;
        let text = r["content"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        (r["isError"].as_bool().unwrap_or(false), text)
    }

    pub async fn tool_names(&self, token: &str) -> Vec<String> {
        let r = self.rpc(token, "tools/list", json!({})).await;
        r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// Small helper for query strings in Location headers.
pub fn query_param(url: &str, key: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}
