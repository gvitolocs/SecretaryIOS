//! Lucina's tools over the attachment memory: find what the owner shared
//! earlier and read it again. Owner turns only (never MCP registries).

use std::sync::Arc;

use lucina_tools::{Ctx, SideEffect, Tool, ToolError, ToolResult};
use serde_json::{json, Value};

use super::memory::Memory;

const GET_MAX_CHARS: usize = 20_000;

/// Both tools over one memory.
pub fn all(memory: Arc<Memory>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(AttachmentsSearch(memory.clone())),
        Arc::new(AttachmentsGet(memory)),
    ]
}

pub struct AttachmentsSearch(pub Arc<Memory>);
pub struct AttachmentsGet(pub Arc<Memory>);

fn since_ms(since: &str) -> Result<i64, ToolError> {
    if let Ok(d) = chrono::NaiveDate::parse_from_str(since, "%Y-%m-%d") {
        return Ok(d
            .and_hms_opt(0, 0, 0)
            .map(|t| t.and_utc().timestamp_millis())
            .unwrap_or(0));
    }
    chrono::DateTime::parse_from_rfc3339(since)
        .map(|t| t.timestamp_millis())
        .map_err(|_| ToolError::new("since must be YYYY-MM-DD or an RFC 3339 time"))
}

#[async_trait::async_trait]
impl Tool for AttachmentsSearch {
    fn name(&self) -> &'static str {
        "attachments_search"
    }
    fn description(&self) -> &'static str {
        "Search the files, photos, links and documents the owner shared with you earlier (what you read from them: text, OCR, transcripts, summaries). Use it when the owner refers to something they sent before (\"the PDF from yesterday\"). Returns ids, names, dates and summaries; read one with attachments_get."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Words to look for; empty lists the newest."},
                "kind": {"type": "string", "description": "Optional: image, video, audio, pdf, document, slides, sheet, text, code, archive, calendar, contact, location, link, other."},
                "since": {"type": "string", "description": "Optional: only items shared since this date (YYYY-MM-DD)."}
            },
            "required": ["query"]
        })
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let query = args["query"].as_str().unwrap_or("").to_owned();
        let kind = args["kind"].as_str().filter(|k| !k.is_empty()).map(str::to_owned);
        let since = match args["since"].as_str().filter(|s| !s.is_empty()) {
            Some(s) => Some(since_ms(s)?),
            None => None,
        };
        let memory = self.0.clone();
        let found = tokio::task::spawn_blocking(move || memory.search(&query, kind.as_deref(), since, 8))
            .await
            .map_err(|e| ToolError::new(e.to_string()))?
            .map_err(|e| ToolError::new(e.to_string()))?;
        let items: Vec<Value> = found
            .into_iter()
            .map(|r| {
                let summary = r
                    .summary
                    .clone()
                    .unwrap_or_else(|| r.text.chars().take(400).collect());
                json!({
                    "id": r.id, "name": r.name, "kind": r.kind, "subtype": r.subtype,
                    "shared_at": chrono::DateTime::from_timestamp_millis(r.created_at).map(|t| t.to_rfc3339()),
                    "media_id": r.media_id, "summary": summary,
                })
            })
            .collect();
        Ok(json!({ "count": items.len(), "items": items }))
    }
}

#[async_trait::async_trait]
impl Tool for AttachmentsGet {
    fn name(&self) -> &'static str {
        "attachments_get"
    }
    fn description(&self) -> &'static str {
        "Read one earlier attachment in full (its extracted text, summary and parsed data such as invoice amounts, tables, events or contacts) by the id attachments_search returned."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"id": {"type": "string"}},
            "required": ["id"]
        })
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let id = args["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::new("id is required"))?
            .to_owned();
        let memory = self.0.clone();
        let record = tokio::task::spawn_blocking(move || memory.get(&id))
            .await
            .map_err(|e| ToolError::new(e.to_string()))?
            .map_err(|e| ToolError::new(e.to_string()))?
            .ok_or_else(|| ToolError::new("no attachment with that id"))?;
        let truncated = record.text.chars().count() > GET_MAX_CHARS;
        Ok(json!({
            "id": record.id, "name": record.name, "kind": record.kind, "subtype": record.subtype,
            "shared_at": chrono::DateTime::from_timestamp_millis(record.created_at).map(|t| t.to_rfc3339()),
            "summary": record.summary, "data": record.structured,
            "text": record.text.chars().take(GET_MAX_CHARS).collect::<String>(),
            "truncated": truncated,
        }))
    }
}
