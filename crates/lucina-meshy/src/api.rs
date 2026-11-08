//! Meshy REST API client (`https://api.meshy.ai/openapi`, OpenAPI 3.1 at
//! docs.meshy.ai/openapi.yaml).
//!
//! - Every call sends `Authorization: Bearer <key>` from the [`KeySource`],
//!   read per call. The key never reaches a log line or an error.
//! - Create requests are **never retried automatically**: a timeout or 5xx
//!   may still have created (and charged) a task (Meshy's "task lifecycle"
//!   rule). Status reads may be retried freely.
//! - Downloads go only to Meshy's asset hosts (`*.meshy.ai`, https) or to the
//!   configured API host itself (the local mock), without the API key.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{clip, from_status, MeshyError};
use crate::key::KeySource;

pub const DEFAULT_BASE: &str = "https://api.meshy.ai/openapi";
/// Largest model download (8K-texture GLBs stay well below).
pub const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// Meshy task families (their URL paths under the base).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    TextTo3d,
    ImageTo3d,
    MultiImageTo3d,
    Remesh,
    Retexture,
    Rigging,
    Animation,
    Convert,
}

impl TaskKind {
    pub fn path(self) -> &'static str {
        match self {
            TaskKind::TextTo3d => "v2/text-to-3d",
            TaskKind::ImageTo3d => "v1/image-to-3d",
            TaskKind::MultiImageTo3d => "v1/multi-image-to-3d",
            TaskKind::Remesh => "v1/remesh",
            TaskKind::Retexture => "v1/retexture",
            TaskKind::Rigging => "v1/rigging",
            TaskKind::Animation => "v1/animations",
            TaskKind::Convert => "v1/convert",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::TextTo3d => "text-to-3d",
            TaskKind::ImageTo3d => "image-to-3d",
            TaskKind::MultiImageTo3d => "multi-image-to-3d",
            TaskKind::Remesh => "remesh",
            TaskKind::Retexture => "retexture",
            TaskKind::Rigging => "rigging",
            TaskKind::Animation => "animation",
            TaskKind::Convert => "convert",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
                "text-to-3d" => TaskKind::TextTo3d,
                "image-to-3d" => TaskKind::ImageTo3d,
                "multi-image-to-3d" => TaskKind::MultiImageTo3d,
                "remesh" => TaskKind::Remesh,
                "retexture" => TaskKind::Retexture,
                "rigging" | "rig" => TaskKind::Rigging,
                "animation" | "animations" | "animate" => TaskKind::Animation,
                "convert" => TaskKind::Convert,
                _ => return None,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pending,
    InProgress,
    Succeeded,
    Failed,
    Canceled,
    Unknown,
}

impl Status {
    fn parse(s: &str) -> Self {
        match s {
            "PENDING" => Status::Pending,
            "IN_PROGRESS" => Status::InProgress,
            "SUCCEEDED" => Status::Succeeded,
            "FAILED" => Status::Failed,
            "CANCELED" => Status::Canceled,
            _ => Status::Unknown,
        }
    }
    pub fn terminal(&self) -> bool {
        matches!(self, Status::Succeeded | Status::Failed | Status::Canceled)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Pending => "PENDING",
            Status::InProgress => "IN_PROGRESS",
            Status::Succeeded => "SUCCEEDED",
            Status::Failed => "FAILED",
            Status::Canceled => "CANCELED",
            Status::Unknown => "UNKNOWN",
        }
    }
}

/// A task as `GET …/{id}` returns it (the raw JSON is kept).
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub status: Status,
    pub progress: u8,
    pub consumed_credits: Option<u32>,
    pub error: Option<String>,
    pub raw: Value,
}

impl Task {
    pub fn from_value(kind: TaskKind, v: Value) -> Result<Self, MeshyError> {
        let id = v["id"]
            .as_str()
            .ok_or_else(|| MeshyError::Decode("task without id".into()))?
            .to_owned();
        let status = Status::parse(v["status"].as_str().unwrap_or(""));
        Ok(Self {
            id,
            kind,
            status,
            progress: v["progress"].as_u64().unwrap_or(0).min(100) as u8,
            consumed_credits: v["consumed_credits"].as_u64().map(|c| c as u32),
            error: v["task_error"]["message"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| clip(s, 300)),
            raw: v,
        })
    }

    /// The model as GLB: an animation clip, else the rigged character, else
    /// the generated model.
    pub fn glb_url(&self) -> Option<&str> {
        let r = &self.raw;
        r["result"]["animation_glb_url"]
            .as_str()
            .or_else(|| r["result"]["rigged_character_glb_url"].as_str())
            .or_else(|| r["model_urls"]["glb"].as_str())
            .filter(|s| !s.is_empty())
    }

    /// URL of one export format (`glb`, `fbx`, `usdz`, `obj`, `stl`, `3mf`).
    pub fn format_url(&self, format: &str) -> Option<&str> {
        let r = &self.raw;
        let from_result = match format {
            "glb" => r["result"]["animation_glb_url"]
                .as_str()
                .or_else(|| r["result"]["rigged_character_glb_url"].as_str()),
            "fbx" => r["result"]["animation_fbx_url"]
                .as_str()
                .or_else(|| r["result"]["rigged_character_fbx_url"].as_str()),
            "usdz" => r["result"]["processed_usdz_url"].as_str(),
            _ => None,
        };
        from_result
            .or_else(|| r["model_urls"][format].as_str())
            .filter(|s| !s.is_empty())
    }

    /// Formats Meshy produced for this task.
    pub fn formats(&self) -> Vec<String> {
        ["glb", "fbx", "usdz", "obj", "stl", "3mf"]
            .iter()
            .filter(|f| self.format_url(f).is_some())
            .map(|f| (*f).to_owned())
            .collect()
    }

    pub fn thumbnail_url(&self) -> Option<&str> {
        self.raw["thumbnail_url"].as_str().filter(|s| !s.is_empty())
    }

    /// The free walking/running clips that come with a rig.
    pub fn basic_animations(&self) -> Vec<(&'static str, String)> {
        let b = &self.raw["result"]["basic_animations"];
        [
            ("Walking", "walking_glb_url"),
            ("Running", "running_glb_url"),
        ]
        .iter()
        .filter_map(|(n, k)| b[*k].as_str().map(|u| (*n, u.to_owned())))
        .collect()
    }
}

/// One entry of `GET /v1/animations/library`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub action_id: i64,
    pub name: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub sub_category: String,
}

pub struct MeshyApi {
    http: reqwest::Client,
    base: String,
    key: Arc<dyn KeySource>,
}

fn retry_after(h: &HeaderMap) -> Option<u64> {
    h.get(RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()
}

fn net(e: reqwest::Error) -> MeshyError {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    MeshyError::Network(kind.into())
}

impl MeshyApi {
    pub fn new(base: &str, key: Arc<dyn KeySource>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-meshy/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            key,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<(Value, Option<u64>), MeshyError> {
        let key = self.key.api_key().await?;
        let resp = rb.bearer_auth(key.expose()).send().await.map_err(net)?;
        let status = resp.status().as_u16();
        let ra = retry_after(resp.headers());
        let text = resp.text().await.map_err(net)?;
        if !(200..300).contains(&status) {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_owned))
                .unwrap_or(text);
            return Err(from_status(status, &msg, ra));
        }
        let v = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text)
                .map_err(|e| MeshyError::Decode(clip(&e.to_string(), 120)))?
        };
        Ok((v, ra))
    }

    /// `POST` a task; returns its id. Not retried (see the module docs).
    pub async fn create(&self, kind: TaskKind, body: &Value) -> Result<String, MeshyError> {
        let (v, _) = self
            .send(self.http.post(self.url(kind.path())).json(body))
            .await?;
        v["result"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| MeshyError::Decode("create answer without result id".into()))
    }

    /// `GET …/{id}`, plus the server's suggested wait (`Retry-After`).
    pub async fn get(&self, kind: TaskKind, id: &str) -> Result<(Task, Option<u64>), MeshyError> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(MeshyError::BadRequest("invalid task id".into()));
        }
        let (v, ra) = self
            .send(self.http.get(self.url(&format!("{}/{id}", kind.path()))))
            .await?;
        Ok((Task::from_value(kind, v)?, ra))
    }

    /// Newest tasks of one kind.
    pub async fn list(&self, kind: TaskKind, page_size: u32) -> Result<Vec<Task>, MeshyError> {
        let (v, _) = self
            .send(self.http.get(self.url(kind.path())).query(&[
                ("page_size", page_size.clamp(1, 50).to_string()),
                ("sort_by", "-created_at".to_owned()),
            ]))
            .await?;
        let items = v.as_array().cloned().unwrap_or_default();
        Ok(items
            .into_iter()
            .filter_map(|t| Task::from_value(kind, t).ok())
            .collect())
    }

    /// `GET /v1/balance` (free).
    pub async fn balance(&self) -> Result<i64, MeshyError> {
        let (v, _) = self.send(self.http.get(self.url("v1/balance"))).await?;
        v["balance"]
            .as_i64()
            .ok_or_else(|| MeshyError::Decode("balance missing".into()))
    }

    /// `GET /v1/animations/library` (free), filtered by `search`.
    pub async fn animation_library(
        &self,
        search: Option<&str>,
    ) -> Result<Vec<LibraryItem>, MeshyError> {
        let mut rb = self.http.get(self.url("v1/animations/library"));
        if let Some(s) = search.map(str::trim).filter(|s| !s.is_empty()) {
            rb = rb.query(&[("search", s)]);
        }
        let (v, _) = self.send(rb).await?;
        serde_json::from_value(v).map_err(|e| MeshyError::Decode(clip(&e.to_string(), 120)))
    }

    /// Is `url` one of Meshy's asset hosts (or the configured API host)?
    pub fn allowed_asset(&self, url: &str) -> bool {
        fn origin(u: &str) -> Option<(String, String)> {
            let (scheme, rest) = u.split_once("://")?;
            let host_port = rest.split(['/', '?', '#']).next()?;
            if host_port.is_empty() || host_port.contains('@') {
                return None;
            }
            Some((scheme.to_ascii_lowercase(), host_port.to_ascii_lowercase()))
        }
        let Some((scheme, host_port)) = origin(url) else {
            return false;
        };
        if origin(&self.base) == Some((scheme.clone(), host_port.clone())) {
            return true;
        }
        let host = host_port.split(':').next().unwrap_or("");
        scheme == "https" && (host == "meshy.ai" || host.ends_with(".meshy.ai"))
    }

    /// Download a result file (no API key: asset URLs are signed).
    pub async fn download(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, MeshyError> {
        if !self.allowed_asset(url) {
            let host = url
                .split_once("://")
                .map(|(_, r)| r.split(['/', '?']).next().unwrap_or(""))
                .unwrap_or("?");
            return Err(MeshyError::AssetHost(host.to_owned()));
        }
        let mut resp = self
            .http
            .get(url)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(net)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(from_status(status, "asset download failed", None));
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(MeshyError::TooLarge(max_bytes / (1024 * 1024)));
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net)? {
            out.extend_from_slice(&chunk);
            if out.len() as u64 > max_bytes {
                return Err(MeshyError::TooLarge(max_bytes / (1024 * 1024)));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{Secret, StaticKey};
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "msy_unit_test_key_0000";

    fn api(base: &str) -> MeshyApi {
        MeshyApi::new(base, Arc::new(StaticKey(Secret::new(KEY))))
    }

    #[tokio::test]
    async fn create_get_and_errors() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/v2/text-to-3d"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .and(body_json(json!({"mode": "preview", "prompt": "chest"})))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({"result": "t-1"})))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/openapi/v2/text-to-3d/t-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Retry-After", "3")
                    .set_body_json(json!({"id": "t-1", "status": "IN_PROGRESS", "progress": 42, "created_at": 1})),
            )
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/openapi/v1/rigging"))
            .respond_with(
                ResponseTemplate::new(402)
                    .set_body_json(json!({"message": "Insufficient credits"})),
            )
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/openapi/v1/remesh"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
            .mount(&s)
            .await;
        let a = api(&format!("{}/openapi", s.uri()));
        let id = a
            .create(
                TaskKind::TextTo3d,
                &json!({"mode": "preview", "prompt": "chest"}),
            )
            .await
            .unwrap();
        assert_eq!(id, "t-1");
        let (t, ra) = a.get(TaskKind::TextTo3d, "t-1").await.unwrap();
        assert_eq!(
            (t.status, t.progress, ra),
            (Status::InProgress, 42, Some(3))
        );
        let e = a.create(TaskKind::Rigging, &json!({})).await.unwrap_err();
        assert_eq!(e.code(), "insufficient_credits");
        assert!(!e.to_string().contains(KEY));
        let e = a.create(TaskKind::Remesh, &json!({})).await.unwrap_err();
        assert!(e.create_outcome_unknown(), "{e:?}");
        assert!(a.get(TaskKind::Remesh, "../x").await.is_err());
    }

    #[test]
    fn asset_hosts() {
        let a = api("https://api.meshy.ai/openapi");
        assert!(a.allowed_asset("https://assets.meshy.ai/abc/model.glb?Expires=1"));
        assert!(!a.allowed_asset("http://assets.meshy.ai/x.glb"));
        assert!(!a.allowed_asset("https://evil.example/x.glb"));
        assert!(!a.allowed_asset("https://assets.meshy.ai.evil.example/x.glb"));
        assert!(!a.allowed_asset("https://user@assets.meshy.ai/x.glb"));
        let m = api("http://127.0.0.1:8797/openapi");
        assert!(m.allowed_asset("http://127.0.0.1:8797/assets/t/model.glb"));
        assert!(!m.allowed_asset("http://127.0.0.1:9999/assets/t/model.glb"));
    }

    #[test]
    fn task_urls() {
        let t = Task::from_value(
            TaskKind::Animation,
            json!({"id": "a", "status": "SUCCEEDED", "progress": 100, "consumed_credits": 6,
                   "result": {"animation_glb_url": "https://assets.meshy.ai/a.glb", "animation_fbx_url": "https://assets.meshy.ai/a.fbx"}}),
        )
        .unwrap();
        assert_eq!(t.glb_url(), Some("https://assets.meshy.ai/a.glb"));
        assert_eq!(t.formats(), vec!["glb", "fbx"]);
        assert_eq!(t.consumed_credits, Some(6));
        let f = Task::from_value(
            TaskKind::TextTo3d,
            json!({"id": "b", "status": "FAILED", "task_error": {"message": "moderation"}}),
        )
        .unwrap();
        assert_eq!(f.error.as_deref(), Some("moderation"));
        assert!(f.status.terminal());
    }
}
