//! Infisical broker protocol. Neither broker diagnostics nor secrets escape this module.
use crate::oauth::Secret;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Clone)]
pub struct Config {
    pub socket: PathBuf,
    pub token_file: Option<PathBuf>,
    pub token_ref: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            socket: "/run/infisical-broker/broker.sock".into(),
            token_file: None,
            token_ref: "secret://lucina/prod#PIXIV_REFRESH_TOKEN".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("broker unconfigured")]
    Unconfigured,
    #[error("broker unavailable")]
    Unavailable,
    #[error("approval denied")]
    Denied,
    #[error("approval expired")]
    Expired,
    #[error("approval unavailable")]
    ApprovalUnavailable,
    #[error("broker operation failed")]
    Failed(String),
}
fn missing(v: &Value) -> bool {
    v["error"]["code"] == "not_found"
        || (v["error"]["code"] == "upstream"
            && v["error"]["message"].as_str().is_some_and(|s| {
                let s = s.to_lowercase();
                s.contains("not found") || s.contains("404")
            }))
}
pub(crate) fn wipe(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => a.iter_mut().for_each(wipe),
        Value::Object(o) => o.values_mut().for_each(wipe),
        _ => {}
    }
}
impl Config {
    async fn call(&self, request: &mut Value) -> Result<Value, Error> {
        let path = self.token_file.as_ref().ok_or(Error::Unconfigured)?;
        let token = Zeroizing::new(
            tokio::fs::read_to_string(path)
                .await
                .map_err(|_| Error::Unavailable)?,
        );
        request["token"] = json!(token.trim());
        let mut bytes = Zeroizing::new(
            serde_json::to_vec(request).map_err(|_| Error::Failed("protocol".into()))?,
        );
        wipe(&mut request["token"]);
        request.as_object_mut().unwrap().remove("token");
        bytes.push(b'\n');
        let mut stream = timeout(Duration::from_secs(5), UnixStream::connect(&self.socket))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(|_| Error::Unavailable)?;
        timeout(Duration::from_secs(5), stream.write_all(&bytes))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(|_| Error::Unavailable)?;
        let mut line = Zeroizing::new(String::new());
        timeout(
            Duration::from_secs(300),
            BufReader::new(stream).read_line(&mut line),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(|_| Error::Unavailable)?;
        serde_json::from_str(&line).map_err(|_| Error::Failed("protocol".into()))
    }
    pub async fn read(&self) -> Result<Option<Secret>, Error> {
        let mut v = self
            .call(&mut json!({"op":"exec", "env":{"PIXIV_REFRESH_TOKEN":self.token_ref}}))
            .await?;
        let result = if v["ok"] == true {
            Ok(v["result"]["env"]["PIXIV_REFRESH_TOKEN"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(Secret::new))
        } else if missing(&v) {
            Ok(None)
        } else {
            Err(Error::Unavailable)
        };
        wipe(&mut v);
        result
    }
    pub async fn exists(&self) -> Result<bool, Error> {
        let v = self
            .call(&mut json!({"op":"describe","ref":self.token_ref}))
            .await?;
        if v["ok"] == true {
            Ok(true)
        } else if missing(&v) {
            Ok(false)
        } else {
            Err(failed(&v))
        }
    }
    pub async fn mutate(&self, action: &str, value: Option<&Secret>) -> Result<(), Error> {
        let mut request = Sensitive(json!({"op":"mutate","ref":self.token_ref,"action":action}));
        if let Some(s) = value {
            request.0["value"] = json!(s.expose());
        }
        let result = self.mutate_loop(&mut request.0).await;
        wipe(&mut request.0);
        result
    }
    async fn mutate_loop(&self, request: &mut Value) -> Result<(), Error> {
        let mut deadline = None;
        loop {
            if deadline.is_some_and(|d| chrono::Utc::now().timestamp() > d) {
                return Err(Error::Expired);
            }
            let v = self.call(request).await?;
            if deadline.is_some_and(|d| chrono::Utc::now().timestamp() > d) {
                return Err(Error::Expired);
            }
            if v["ok"] == true {
                if v["result"]["ok"] == true {
                    return Ok(());
                }
                let p = &v["result"]["pending"];
                if p["kind"] == "code" {
                    return Err(Error::ApprovalUnavailable);
                }
                if p["kind"] != "approval" {
                    return Err(Error::Failed("protocol".into()));
                }
                let id = p["id"].as_str().ok_or(Error::Failed("protocol".into()))?;
                request["confirm_id"] = json!(id);
                if deadline.is_none() {
                    deadline = p["expires_at"]
                        .as_i64()
                        .or_else(|| {
                            p["expires_at"]
                                .as_str()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .map(|d| d.timestamp())
                        })
                        .map(|n| n + 30);
                    if deadline.is_none() {
                        return Err(Error::Failed("protocol".into()));
                    }
                }
            } else {
                match v["error"]["code"].as_str() {
                    Some("confirm_pending") if request["confirm_id"].is_string() => {}
                    Some("confirm_denied") => return Err(Error::Denied),
                    Some("confirm_expired") => return Err(Error::Expired),
                    _ => return Err(failed(&v)),
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

struct Sensitive(Value);
impl Drop for Sensitive {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

// Preserve short machine codes only; broker prose and unknown payloads never become errors.
fn failed(v: &Value) -> Error {
    let code = v["error"]["code"].as_str().unwrap_or("protocol");
    let safe = if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code
    } else {
        "protocol"
    };
    Error::Failed(safe.into())
}
