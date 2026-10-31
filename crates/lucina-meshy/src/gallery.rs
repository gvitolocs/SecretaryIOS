//! The model gallery on secretary-server (`/v1/models`, docs/server-api.md
//! "Models"), with Lucina's service token (`svc_…`, read per call):
//!
//! - `POST /v1/models` → 201 `{model}`: a new entry (title, prompt, Meshy
//!   task ids, credits, clips, formats).
//! - `PUT /v1/models/{id}/files/{name}`: `model.glb`, `thumb.png|jpg|webp`,
//!   `preview.mp4`, `model.fbx|usdz|obj|stl|3mf` (raw body).
//! - `GET /v1/models?limit=&q=`, `GET /v1/models/{id}`, `DELETE /v1/models/{id}`.
//!
//! Files live under the server's data dir and are only served with auth.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GalleryError {
    #[error("the model gallery is not configured (LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE)")]
    NotConfigured,
    #[error("secretary-server unreachable")]
    Unreachable,
    #[error("secretary-server answered {status} {code}")]
    Status { status: u16, code: String },
}

#[derive(Debug, Clone)]
pub struct Gallery {
    http: reqwest::Client,
    url: Option<String>,
    token_file: Option<PathBuf>,
}

/// `mdl_` + 8-40 lowercase letters/digits.
pub fn valid_model_id(id: &str) -> bool {
    id.strip_prefix("mdl_").is_some_and(|r| {
        (8..=40).contains(&r.len())
            && r.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

pub fn content_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "model.glb" => "model/gltf-binary",
        "thumb.png" => "image/png",
        "thumb.jpg" => "image/jpeg",
        "thumb.webp" => "image/webp",
        "preview.mp4" => "video/mp4",
        "model.fbx" | "model.obj" | "model.3mf" => "application/octet-stream",
        "model.usdz" => "model/vnd.usdz+zip",
        "model.stl" => "model/stl",
        _ => return None,
    })
}

impl Gallery {
    pub fn new(url: Option<String>, token_file: Option<PathBuf>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-meshy/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("http client");
        Self {
            http,
            url: url
                .map(|u| u.trim().trim_end_matches('/').to_owned())
                .filter(|u| !u.is_empty()),
            token_file,
        }
    }

    pub fn configured(&self) -> bool {
        self.url.is_some() && self.token_file.is_some()
    }

    fn auth(&self) -> Result<(String, String), GalleryError> {
        let url = self.url.clone().ok_or(GalleryError::NotConfigured)?;
        let token = self
            .token_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.trim().to_owned())
            .filter(|t| t.starts_with("svc_"))
            .ok_or(GalleryError::NotConfigured)?;
        Ok((url, token))
    }

    async fn answer(resp: reqwest::Response) -> Result<Value, GalleryError> {
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return Err(GalleryError::Status {
                status,
                code: body["error"]["code"].as_str().unwrap_or("").to_owned(),
            });
        }
        Ok(body)
    }

    /// Create an entry; returns the model object (with its `id`).
    pub async fn create(&self, meta: &Value) -> Result<Value, GalleryError> {
        let (url, token) = self.auth()?;
        let resp = self
            .http
            .post(format!("{url}/v1/models"))
            .bearer_auth(token)
            .json(meta)
            .send()
            .await
            .map_err(|_| GalleryError::Unreachable)?;
        Ok(Self::answer(resp).await?["model"].clone())
    }

    pub async fn put_file(
        &self,
        id: &str,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, GalleryError> {
        let ct = content_type(name).ok_or(GalleryError::Status {
            status: 400,
            code: "bad_file_name".into(),
        })?;
        if !valid_model_id(id) {
            return Err(GalleryError::Status {
                status: 400,
                code: "bad_model_id".into(),
            });
        }
        let (url, token) = self.auth()?;
        let resp = self
            .http
            .put(format!("{url}/v1/models/{id}/files/{name}"))
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, ct)
            .timeout(Duration::from_secs(300))
            .body(bytes)
            .send()
            .await
            .map_err(|_| GalleryError::Unreachable)?;
        Ok(Self::answer(resp).await?["model"].clone())
    }

    pub async fn get(&self, id: &str) -> Result<Value, GalleryError> {
        if !valid_model_id(id) {
            return Err(GalleryError::Status {
                status: 404,
                code: "not_found".into(),
            });
        }
        let (url, token) = self.auth()?;
        let resp = self
            .http
            .get(format!("{url}/v1/models/{id}"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| GalleryError::Unreachable)?;
        Ok(Self::answer(resp).await?["model"].clone())
    }

    pub async fn list(&self, limit: u32) -> Result<Vec<Value>, GalleryError> {
        let (url, token) = self.auth()?;
        let resp = self
            .http
            .get(format!("{url}/v1/models"))
            .query(&[("limit", limit.clamp(1, 50).to_string())])
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| GalleryError::Unreachable)?;
        Ok(Self::answer(resp).await?["models"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }
}

/// The compact view of a gallery entry for the model (no URLs).
pub fn summary(m: &Value) -> Value {
    json!({
        "id": m["id"],
        "title": m["title"],
        "prompt": m["prompt"],
        "createdAt": m["createdAt"],
        "credits": m["credits"],
        "animations": m["animations"],
        "formats": m["formats"],
        "rigged": m["meshy"]["rigTaskId"].is_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        assert!(valid_model_id("mdl_0123456789abcdef"));
        assert!(!valid_model_id("mdl_ABC12345"));
        assert!(!valid_model_id("mdl_short"));
        assert!(!valid_model_id("../etc/passwd"));
        assert_eq!(content_type("model.glb"), Some("model/gltf-binary"));
        assert_eq!(content_type("preview.mp4"), Some("video/mp4"));
        assert_eq!(content_type("../model.glb"), None);
    }
}
