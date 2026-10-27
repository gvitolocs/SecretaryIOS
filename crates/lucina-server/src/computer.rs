//! Lucina's own computer for the apps and the MCP (`/api/secretary/computer/*`).
//!
//! Two isolated Cua sandboxes on nezopt: the Linux desktop `lucina-workspace` and the
//! Android `lucina-android` (crate `lucina_computer_use`). Nothing here runs a host
//! command: tasks go to the Hermes engine pinned to the workspace, direct calls go to
//! the Cua MCP proxy, and guest paths that name host locations are refused.
//!
//! - `GET  /computer`            status (task engine ready, sandboxes)
//! - `POST /computer/task`       `{task}`: Lucina's engine does the whole task, returns receipts
//! - `POST /computer/call`       `{sandbox?, op, ...}`: `spaces`, `bash`, `write`, `tools`, `tool`, `screenshot`
//! - `GET  /computer/screenshot?sandbox=` the current screen as PNG (the apps' live view)

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use base64::Engine as _;
use lucina_computer_use::{image_of, text_of, Cancel, Client, ComputerUseConfig, Sandbox};
use serde_json::{json, Value};

use crate::api::{ok, read_json, ApiError};

type ApiResult = Result<axum::response::Response, ApiError>;

const BODY_LIMIT: usize = 512 * 1024;

fn config() -> ComputerUseConfig {
    ComputerUseConfig::from_lookup(&|k: &str| std::env::var(k).ok())
}

fn sandbox(name: Option<&str>) -> Result<Sandbox, ApiError> {
    match name.unwrap_or("workspace") {
        "workspace" | "linux" | "desktop" => Ok(Sandbox::workspace()),
        "android" | "phone" => Ok(Sandbox::android()),
        other => Err(ApiError::bad_request(format!(
            "unknown sandbox {other:?} (use \"workspace\" or \"android\")"
        ))),
    }
}

fn cu_error(e: lucina_computer_use::Error) -> ApiError {
    let status = match e.code() {
        "not_enabled" | "launcher_unavailable" | "sandbox_unavailable" | "recovery_required" => 503,
        "bad_request" | "output_limit" => 400,
        "busy" => 409,
        "quota_exhausted" => 429,
        "deadline_exceeded" => 504,
        _ => 502,
    };
    ApiError::new(status, e.code(), e.to_string())
}

/// The screen of a sandbox through the driver's `get_desktop_state`.
async fn screen(sb: &Sandbox) -> Result<(Option<(String, String)>, String), ApiError> {
    let r = sb
        .call_tool("get_desktop_state", json!({}))
        .await
        .map_err(cu_error)?;
    Ok((image_of(&r), text_of(&r)))
}

pub async fn computer(method: &Method, uri: &Uri, what: Option<&str>, body: Body) -> ApiResult {
    match what {
        None => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let cfg = config();
            Ok(ok(json!({
                "taskEngine": {"enabled": cfg.enabled, "ready": cfg.ready(), "timeoutSecs": cfg.timeout.as_secs()},
                "sandboxes": [
                    {"name": "workspace", "space": lucina_computer_use::WORKSPACE_SPACE, "kind": "linux-desktop"},
                    {"name": "android", "space": lucina_computer_use::ANDROID_SPACE, "kind": "android"},
                ],
            })))
        }
        Some("task") => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let v = read_json(body, BODY_LIMIT).await?;
            let task = v
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ApiError::bad_request("task is required"))?;
            let client = Client::new(config());
            if !client.ready() {
                return Err(ApiError::new(
                    503,
                    "computer_use_disabled",
                    "Lucina's computer-use engine is not enabled (LUCINA_COMPUTER_USE_ENABLED) or its launcher is missing",
                ));
            }
            tracing::info!(chars = task.chars().count(), "computer task started");
            let result = client.run(task, &Cancel::new()).await.map_err(cu_error)?;
            let out = result.to_json();
            let status = out.get("status").and_then(Value::as_str).unwrap_or("").to_owned();
            tracing::info!(status = %status, "computer task finished");
            Ok(ok(out))
        }
        Some("call") => {
            if *method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let v = read_json(body, BODY_LIMIT).await?;
            let sb = sandbox(v.get("sandbox").and_then(Value::as_str))?;
            let op = v.get("op").and_then(Value::as_str).unwrap_or("");
            let s = |k: &str| v.get(k).and_then(Value::as_str);
            tracing::info!(sandbox = %sb.space(), op, tool = s("tool").unwrap_or(""), "computer call");
            let r = match op {
                "spaces" => sb.spaces().await,
                "bash" => {
                    let cmd = s("command").ok_or_else(|| ApiError::bad_request("command is required"))?;
                    sb.bash(cmd, v.get("timeoutSecs").and_then(Value::as_u64)).await
                }
                "write" => {
                    let path = s("path").ok_or_else(|| ApiError::bad_request("path is required"))?;
                    sb.write(path, s("content").unwrap_or("")).await
                }
                "tools" => sb.tools(s("tool")).await,
                "tool" => {
                    let tool = s("tool").ok_or_else(|| ApiError::bad_request("tool is required"))?;
                    sb.call_tool(tool, v.get("arguments").cloned().unwrap_or_else(|| json!({}))).await
                }
                "screenshot" => {
                    let (img, text) = screen(&sb).await?;
                    return Ok(ok(json!({
                        "text": text,
                        "image": img.map(|(mime, data)| json!({"mimeType": mime, "data": data})),
                    })));
                }
                _ => {
                    return Err(ApiError::bad_request(
                        "op must be one of spaces, bash, write, tools, tool, screenshot",
                    ))
                }
            }
            .map_err(cu_error)?;
            Ok(ok(json!({
                "text": text_of(&r),
                "image": image_of(&r).map(|(mime, data)| json!({"mimeType": mime, "data": data})),
            })))
        }
        Some("screenshot") => {
            if *method != Method::GET {
                return Err(ApiError::method_not_allowed(&["GET"]));
            }
            let q = uri.query().unwrap_or("");
            let name = q
                .split('&')
                .find_map(|kv| kv.strip_prefix("sandbox="));
            let sb = sandbox(name)?;
            let (img, _) = tokio::time::timeout(Duration::from_secs(30), screen(&sb))
                .await
                .map_err(|_| ApiError::new(504, "timeout", "screenshot timed out"))??;
            let (mime, data) = img.ok_or_else(|| ApiError::new(502, "no_image", "the sandbox returned no screen"))?;
            let png = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| ApiError::new(502, "bad_image", e.to_string()))?;
            let mut resp = (StatusCode::OK, png).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("image/png")),
            );
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(resp)
        }
        Some(_) => Err(ApiError::not_found()),
    }
}
