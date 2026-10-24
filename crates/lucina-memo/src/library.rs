//! Registers memos in the media library (lucina-media on nezopt, branch `media-library`), so
//! they show in the gallery's "Voice memos" album and in Lucina's media search. The library
//! keeps no copy: it points at our audio file. Best effort; idempotent by memo id.
//!
//! `PUT <LUCINA_MEMO_LIBRARY_URL>/internal/v1/external/voice-memo/{id}` (default
//! `http://127.0.0.1:8830`, `off` disables); `DELETE` on the same URL when a memo is trashed.
use std::time::Duration;

use serde_json::json;

use crate::model::Memo;
use crate::App;

fn base() -> Option<String> {
    let v = std::env::var("LUCINA_MEMO_LIBRARY_URL").unwrap_or_else(|_| "http://127.0.0.1:8830".into());
    let v = v.trim().trim_end_matches('/').to_owned();
    (!v.is_empty() && v != "off").then_some(v)
}

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(5)).build().ok()
}

pub async fn register(app: &App, m: &Memo) {
    let (Some(base), Some(http)) = (base(), client()) else { return };
    let id = &m.id;
    let body = json!({
        "kind": "audio",
        "title": m.title,
        "created_at": m.recorded_at,
        "duration_ms": (m.duration_sec * 1000.0).round() as u64,
        "mime": m.mime,
        "path": app.store.audio_path(m),
        "links": {
            "memo": format!("/v1/memos/{id}"),
            "transcript": m.steps.transcribed_at.map(|_| format!("/v1/memos/{id}/transcript")),
            "summary": m.steps.summarized_at.map(|_| format!("/v1/memos/{id}")),
            "pdf": m.steps.rendered_at.map(|_| format!("/v1/memos/{id}/pdf")),
            "obsidian": m.obsidian_note,
            "app": app.cfg.memo_url(id),
        },
    });
    match http.put(format!("{base}/internal/v1/external/voice-memo/{id}")).json(&body).send().await {
        Ok(r) if r.status().is_success() => tracing::info!(%id, "registered in the media library"),
        Ok(r) => tracing::info!(%id, status = r.status().as_u16(), "media library refused the memo"),
        Err(e) => tracing::debug!(%id, error = %e.without_url(), "media library not reachable"),
    }
}

pub async fn unregister(id: &str) {
    let (Some(base), Some(http)) = (base(), client()) else { return };
    let _ = http.delete(format!("{base}/internal/v1/external/voice-memo/{id}")).send().await;
}
