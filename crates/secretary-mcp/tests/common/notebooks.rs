//! A fake of secretary-server's `/v1/notebooks/*` (docs/design/notebooks-api.md): the
//! server forwards to Lucina's `/api/secretary/notebooks/*` unchanged, so the shapes
//! below are the contract's. It answers the device token only.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{device_ok, MockState};

pub const NB: &str = "nb4f1c09a2e7";
pub const SRC: &str = "s0a9be3c112";
pub const CHAT: &str = "c4f1c09a2e";
pub const PODCAST: &str = "p4f1c09a2e7";

fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
        .into_response()
}

fn notebook() -> Value {
    json!({"id": NB, "name": "CNS", "description": "Course notes", "archived": false,
        "created_at": "2026-10-29T13:00:00Z", "updated_at": "2026-10-29T13:05:00Z",
        "counts": {"sources": 2, "notes": 1, "chats": 1, "podcasts": 1}})
}

fn source(id: &str, kind: &str, title: &str, status: &str) -> Value {
    json!({"id": id, "kind": kind, "title": title, "origin": "upload:RM Class 2.pdf", "status": status,
        "error": null, "chars": 48211, "chunks": 37, "pages": 22, "duration_s": null, "has_file": true,
        "created_at": "2026-10-29T13:00:00Z", "updated_at": "2026-10-29T13:00:00Z", "notebooks": [NB], "context": "full"})
}

fn podcast(status: &str) -> Value {
    json!({"id": PODCAST, "notebook_id": NB, "title": "CNS in ten minutes", "episode_profile": "two-hosts",
        "speaker_profile": "lucina-duo", "briefing": "", "status": status, "error": null,
        "outline": [{"name": "Neurons", "description": "", "size": "short"}, {"name": "Synapses", "description": "", "size": "short"}],
        "transcript": [{"speaker": "Lucina", "text": "Hello"}], "duration_s": if status == "ready" { json!(312.4) } else { Value::Null },
        "audio": status == "ready", "created_at": "2026-10-29T13:10:00Z"})
}

fn reply(text: &str) -> Value {
    json!({"id": "m4f1c09a2e7", "role": "assistant", "text": "A ring is a set with two operations [source:s0a9be3c112-14] and more [source:s0a9be3c112-15].",
        "citations": [
            {"ref": "source:s0a9be3c112-14", "kind": "source", "source_id": SRC, "chunk": 14, "title": "RM Class 2",
             "excerpt": "A ring is a set R with\n two binary operations", "page": 7, "start_s": null},
            {"ref": "source:s0a9be3c112-15", "kind": "source", "source_id": SRC, "chunk": 15, "title": "Lecture video",
             "excerpt": "and more", "page": null, "start_s": 754.0}],
        "model": if text.contains("local") { "qwen-local" } else { "deepseek-flash" }, "created_at": "2026-10-29T13:06:00Z"})
}

pub fn route(
    _st: &mut MockState,
    method: &str,
    rest: &[&str],
    _query: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Response {
    if !device_ok(headers) {
        return err(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "device token required",
        );
    }
    if rest.first().is_some_and(|r| *r == "nb0000000000") {
        return err(StatusCode::NOT_FOUND, "not_found", "no such notebook");
    }
    match (method, rest) {
        ("GET", []) => Json(json!({"notebooks": [notebook()]})).into_response(),
        ("POST", []) => {
            let mut n = notebook();
            n["name"] = body["name"].clone();
            (StatusCode::CREATED, Json(n)).into_response()
        }
        ("GET", [NB]) => {
            let mut n = notebook();
            n["sources"] = json!([source(SRC, "pdf", "RM Class 2", "ready"), source("s1111111111", "web", "Wiki: Ring", "extracting")]);
            n["notes"] = json!([{"slug": "rm-class-2-summary", "title": "RM Class 2 summary", "excerpt": "…", "updated_at": "2026-10-29T13:00:00Z", "indexed": true}]);
            n["chats"] = json!([{"id": CHAT, "notebook_id": NB, "title": "Exam prep", "messages": 4}]);
            n["podcasts"] = json!([podcast("ready")]);
            Json(n).into_response()
        }
        ("POST", [NB, "sources"]) => {
            let kind = body["kind"].as_str().unwrap_or("");
            (StatusCode::ACCEPTED, Json(source("s2222222222", if kind == "url" { "web" } else { kind }, body["title"].as_str().unwrap_or("Added"), "queued")))
                .into_response()
        }
        ("POST", [NB, "chats"]) => (StatusCode::CREATED, Json(json!({"id": CHAT, "notebook_id": NB, "title": body["title"], "messages": 0}))).into_response(),
        ("POST", ["chats", CHAT, "messages"]) => {
            let text = body["text"].as_str().unwrap_or("");
            if text.contains("broken") {
                return err(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "deepseek is down");
            }
            Json(json!({"user": {"id": "m0", "role": "user", "text": text, "citations": []}, "reply": reply(text)})).into_response()
        }
        ("GET", [NB, "search"]) | ("GET", ["search"]) => {
            let mut hit = json!({"ref": "source:s0a9be3c112-14", "kind": "source", "source_id": SRC, "title": "RM Class 2",
                "excerpt": "A ring is a set\nR with two operations", "score": 0.8312});
            if rest.len() == 1 {
                hit["notebook_id"] = json!(NB);
            }
            Json(json!({"hits": [hit, {"ref": "note:rm-class-2-summary", "kind": "note", "slug": "rm-class-2-summary", "title": "RM Class 2 summary", "excerpt": "", "score": 0.5}]}))
                .into_response()
        }
        ("GET", ["transformations"]) => Json(json!({"transformations": [
            {"id": "t-dense-summary", "name": "Dense Summary", "description": "A tight summary", "prompt": "…", "apply_default": true, "builtin": true},
            {"id": "t-key-insights", "name": "Key Insights", "description": "", "prompt": "…", "apply_default": false, "builtin": true}]}))
        .into_response(),
        ("GET", ["sources", SRC, "insights"]) => Json(json!({"insights": [
            {"id": "i4f1c09a2e7", "source_id": SRC, "transformation_id": "t-dense-summary", "title": "Dense Summary", "status": "ready", "text": "Rings are sets.", "created_at": "2026-10-29T13:01:00Z"}]}))
        .into_response(),
        ("POST", ["sources", SRC, "insights"]) => (
            StatusCode::ACCEPTED,
            Json(json!({"id": "i5555555555", "source_id": SRC, "transformation_id": body["transformation_id"], "title": "Dense Summary", "status": "queued", "text": null, "created_at": "2026-10-29T13:02:00Z"})),
        )
            .into_response(),
        ("POST", [NB, "podcasts"]) => (StatusCode::ACCEPTED, Json(podcast("queued"))).into_response(),
        ("GET", ["podcasts", PODCAST]) => Json(podcast("ready")).into_response(),
        _ => err(StatusCode::NOT_FOUND, "not_found", "no such route"),
    }
}
