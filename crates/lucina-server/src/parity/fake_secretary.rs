//! `lucina fake-secretary`: a stand-in for the secretary-server routes the
//! Lucina bridges call (`/v1/notes`, `/v1/calendar/events`, `/v1/avatar`,
//! `/v1/notify`), for parity runs. Every bearer token gets its own copy of
//! the seed state, so the Node and Rust instances under test cannot see each
//! other's writes; every request is recorded (`--record`, JSONL with the
//! token's namespace, never the token).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde_json::{json, Value};

const WARDROBE: [&str; 5] = ["uniform", "gym", "chef", "maid", "casual"];
const HAIRSTYLES: [&str; 5] = ["natural", "ponytail", "bun", "twintails", "braid"];

#[derive(Default)]
struct Space {
    notes: Vec<Value>,
    events: Vec<Value>,
    avatar: Value,
}

pub struct Fake {
    spaces: Mutex<HashMap<String, Space>>,
    record: Option<PathBuf>,
    tz: chrono_tz::Tz,
    /// Read-through mode (shadow replays): GETs go to the real
    /// secretary-server with its token; writes stay here.
    upstream: Option<(String, String)>,
    http: reqwest::Client,
}

fn seed(tz: chrono_tz::Tz) -> Space {
    let today = Utc::now().with_timezone(&tz).date_naive();
    let at = |days: i64, h: u32, m: u32| {
        let d = today + Duration::days(days);
        local(d, h, m, tz)
    };
    let note = |slug: &str, title: &str, content: &str, source: &str, pinned: bool| {
        json!({"slug": slug, "title": title, "content": content, "source": source, "pinned": pinned,
               "tags": ["work"], "createdAt": "2026-10-20T08:00:00.000Z", "updatedAt": "2026-10-21T09:30:00.000Z",
               "excerpt": content.chars().take(80).collect::<String>()})
    };
    Space {
        notes: vec![
            note("craftit-meeting", "CRAFT-IT4SD meeting", "# CRAFT-IT4SD meeting\n- kickoff on Monday\n- budget review\n- send the slides to Anna", "chatgpt", true),
            note("spesa-casa", "Nota spesa-casa", "latte, pane, uova", "owner", false),
        ],
        events: vec![
            json!({"id": "google:ev1", "title": "Dentista", "start": at(1, 9, 30), "end": at(1, 10, 30), "allDay": false, "calendar": "Personale", "location": "Via Roma 1"}),
            json!({"id": "ics:ev2", "title": "Standup", "start": at(0, 15, 0), "end": at(0, 15, 15), "allDay": false, "calendar": "Work"}),
            json!({"id": "google:ev3", "title": "Compleanno Marco", "start": (today + Duration::days(2)).format("%Y-%m-%d").to_string(), "end": (today + Duration::days(3)).format("%Y-%m-%d").to_string(), "allDay": true}),
        ],
        avatar: json!({"outfit": null, "hair": null, "auto": true}),
    }
}

/// A wall time as RFC 3339 with the zone's offset.
pub fn local(d: chrono::NaiveDate, h: u32, m: u32, tz: chrono_tz::Tz) -> String {
    use chrono::TimeZone;
    let naive = d.and_hms_opt(h, m, 0).expect("valid time");
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
        .unwrap_or_default()
}

/// The namespace of a service token: a short digest, so the record never
/// holds the token.
pub fn space_for(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(token.trim().as_bytes());
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn space_of(headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim()
        .to_owned();
    Some(space_for(&token))
}

fn err(status: u16, code: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(json!({"error": {"code": code, "message": code}})),
    )
        .into_response()
}

impl Fake {
    fn with<R>(&self, space: &str, f: impl FnOnce(&mut Space) -> R) -> R {
        let mut m = self.spaces.lock().unwrap_or_else(|e| e.into_inner());
        // Read-through: the real notes and events come from upstream; local
        // writes start from nothing.
        let read_through = self.upstream.is_some();
        let s = m.entry(space.to_owned()).or_insert_with(|| {
            if read_through {
                Space {
                    avatar: json!({"outfit": null, "hair": null, "auto": true}),
                    ..Space::default()
                }
            } else {
                seed(self.tz)
            }
        });
        f(s)
    }

    fn rec(&self, space: &str, method: &Method, uri: &Uri, body: &Value, status: u16) {
        if let Some(p) = &self.record {
            use std::io::Write;
            let line = json!({"space": space, "method": method.as_str(), "path": uri.path(), "query": uri.query(), "body": body, "status": status, "at": Utc::now().to_rfc3339()});
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

async fn handle(
    State(st): State<Arc<Fake>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(space) = space_of(&headers) else {
        return err(401, "unauthorized");
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if let (Some((url, token)), true) = (&st.upstream, method == Method::GET) {
        let target = format!(
            "{}{}",
            url.trim_end_matches('/'),
            uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
        );
        let resp = match st
            .http
            .get(target)
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => {
                let status =
                    StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let v: Value = r.json().await.unwrap_or(Value::Null);
                (status, Json(v)).into_response()
            }
            Err(_) => err(502, "upstream_unreachable"),
        };
        st.rec(
            &space,
            &method,
            &uri,
            &json!({"proxied": true}),
            resp.status().as_u16(),
        );
        return resp;
    }
    let resp = route(&st, &space, &method, &uri, &body);
    st.rec(&space, &method, &uri, &body, resp.status().as_u16());
    resp
}

fn query(uri: &Uri) -> HashMap<String, String> {
    uri.query()
        .unwrap_or("")
        .split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_owned(), urlish_decode(v)))
        .collect()
}

fn urlish_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) =
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn route(st: &Fake, space: &str, method: &Method, uri: &Uri, body: &Value) -> Response {
    let parts: Vec<&str> = uri
        .path()
        .trim_start_matches("/v1/")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["notes"]) => {
            let q = query(uri);
            let needle = q.get("q").map(|s| s.to_lowercase()).unwrap_or_default();
            let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
            st.with(space, |s| {
                let notes: Vec<Value> = s
                    .notes
                    .iter()
                    .filter(|n| needle.is_empty() || n.to_string().to_lowercase().contains(&needle))
                    .take(limit)
                    .map(|n| json!({"slug": n["slug"], "title": n["title"], "pinned": n["pinned"], "source": n["source"], "tags": n["tags"], "updatedAt": n["updatedAt"], "excerpt": n["excerpt"]}))
                    .collect();
                Json(json!({"notes": notes})).into_response()
            })
        }
        ("GET", ["notes", slug]) => st.with(space, |s| match s.notes.iter().find(|n| n["slug"] == *slug) {
            Some(n) => Json(n.clone()).into_response(),
            None => err(404, "note_not_found"),
        }),
        ("POST", ["notes"]) => st.with(space, |s| {
            let content = body["content"].as_str().unwrap_or("");
            if content.len() > 256 * 1024 {
                return err(413, "note_too_large");
            }
            let title = body["title"].as_str().unwrap_or("Nota");
            let slug = body["slug"].as_str().map(str::to_owned).unwrap_or_else(|| lucina_intent::notes::slugify(title));
            if s.notes.iter().any(|n| n["slug"] == slug.as_str()) {
                return err(409, "slug_taken");
            }
            let note = json!({"slug": slug, "title": title, "content": content, "source": body["source"], "pinned": false, "tags": body["tags"].as_array().cloned().unwrap_or_default(), "createdAt": now, "updatedAt": now, "excerpt": content.chars().take(80).collect::<String>()});
            s.notes.push(note.clone());
            (StatusCode::CREATED, Json(note)).into_response()
        }),
        ("POST", ["notes", slug, "append"]) => st.with(space, |s| match s.notes.iter_mut().find(|n| n["slug"] == *slug) {
            Some(n) => {
                let sep = body["separator"].as_str().unwrap_or("\n\n");
                let content = format!("{}{sep}{}", n["content"].as_str().unwrap_or(""), body["content"].as_str().unwrap_or(""));
                n["content"] = json!(content);
                n["updatedAt"] = json!(now);
                Json(n.clone()).into_response()
            }
            None => err(404, "note_not_found"),
        }),
        ("GET", ["calendar", "events"]) => {
            let q = query(uri);
            let (from, to) = (q.get("from").cloned().unwrap_or_default(), q.get("to").cloned().unwrap_or_default());
            let tz = st.tz;
            st.with(space, |s| {
                let events: Vec<Value> = s
                    .events
                    .iter()
                    .filter(|e| {
                        let start = e["start"].as_str().unwrap_or("");
                        let day = if e["allDay"].as_bool().unwrap_or(false) {
                            start.chars().take(10).collect::<String>()
                        } else {
                            chrono::DateTime::parse_from_rfc3339(start).map(|d| d.with_timezone(&tz).format("%Y-%m-%d").to_string()).unwrap_or_default()
                        };
                        day >= from && day <= to
                    })
                    .cloned()
                    .collect();
                Json(json!({"events": events, "sources": {"google": {"status": "ok"}, "device": {"status": "ok"}}})).into_response()
            })
        }
        ("POST", ["calendar", "events"]) => st.with(space, |s| {
            let mut ev = body.clone();
            ev["id"] = json!(format!("google:new{}", s.events.len() + 1));
            ev["allDay"] = json!(body["end"].is_null());
            s.events.push(ev.clone());
            (StatusCode::CREATED, Json(json!({"event": ev}))).into_response()
        }),
        ("GET", ["avatar"]) => st.with(space, |s| Json(s.avatar.clone()).into_response()),
        ("GET", ["avatar", "wardrobe"]) => Json(json!({"outfits": WARDROBE.iter().map(|i| json!({"id": i, "name": i})).collect::<Vec<_>>(), "hairstyles": HAIRSTYLES.iter().map(|i| json!({"id": i, "name": i})).collect::<Vec<_>>(), "colors": []})).into_response(),
        ("PUT", ["avatar"]) => st.with(space, |s| {
            if let Some(o) = body["outfit"].as_str() {
                if !WARDROBE.contains(&o) {
                    return err(400, "unknown_outfit");
                }
            }
            if let Some(h) = body["hair"].as_str() {
                if !HAIRSTYLES.contains(&h) {
                    return err(400, "unknown_hairstyle");
                }
            }
            for k in ["outfit", "hair", "auto"] {
                if !body[k].is_null() || body.get(k).is_some() {
                    s.avatar[k] = body[k].clone();
                }
            }
            Json(s.avatar.clone()).into_response()
        }),
        ("POST", ["notify"]) => Json(json!({"status": "created"})).into_response(),
        _ => err(404, "not_found"),
    }
}

/// Serve the fake.
pub async fn serve(
    bind: SocketAddr,
    record: Option<PathBuf>,
    tz: chrono_tz::Tz,
    upstream: Option<(String, String)>,
) -> anyhow::Result<()> {
    let st = Arc::new(Fake {
        spaces: Mutex::new(HashMap::new()),
        record,
        tz,
        upstream,
        http: reqwest::Client::new(),
    });
    let app = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "fake": true})) }),
        )
        .route("/v1/{*rest}", any(handle))
        .route(
            "/reset",
            post(|State(st): State<Arc<Fake>>| async move {
                st.spaces.lock().unwrap_or_else(|e| e.into_inner()).clear();
                Json(json!({"ok": true}))
            }),
        )
        .with_state(st);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
