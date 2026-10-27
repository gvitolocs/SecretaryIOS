//! A small HTTP/1.1 client over the daemon's unix socket, for `lagent` and tests.
//! One request per connection (`Connection: close`); SSE is read line by line.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot reach lucina-agentd at {0}: {1}")]
    Connect(PathBuf, std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad response: {0}")]
    BadResponse(String),
    /// The daemon answered with an error object.
    #[error("{message}")]
    Api { status: u16, code: String, message: String },
}

pub struct Client {
    socket: PathBuf,
}

fn parse_head(head: &str) -> Result<(u16, bool), ClientError> {
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| ClientError::BadResponse(status_line.to_string()))?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    Ok((status, chunked))
}

fn dechunk(body: &[u8]) -> Result<Vec<u8>, ClientError> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let pos = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| ClientError::BadResponse("truncated chunk".into()))?;
        let size_str = std::str::from_utf8(&rest[..pos]).map_err(|e| ClientError::BadResponse(e.to_string()))?;
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|e| ClientError::BadResponse(e.to_string()))?;
        rest = &rest[pos + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err(ClientError::BadResponse("truncated chunk body".into()));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

impl Client {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Client { socket: socket.as_ref().to_path_buf() }
    }

    async fn connect(&self) -> Result<UnixStream, ClientError> {
        UnixStream::connect(&self.socket)
            .await
            .map_err(|e| ClientError::Connect(self.socket.clone(), e))
    }

    fn request_bytes(method: &str, path: &str, body: Option<&Value>) -> Vec<u8> {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: lucina-agentd\r\nConnection: close\r\nAccept: application/json\r\n");
        if !body.is_empty() || method == "POST" {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        req.push_str(&body);
        req.into_bytes()
    }

    /// One JSON request; an error status becomes [`ClientError::Api`].
    pub async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ClientError> {
        let mut stream = self.connect().await?;
        stream.write_all(&Self::request_bytes(method, path, body)).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| ClientError::BadResponse("no header end".into()))?;
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let (status, chunked) = parse_head(&head)?;
        let body = &raw[split + 4..];
        let body = if chunked { dechunk(body)? } else { body.to_vec() };
        let value: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).map_err(|e| ClientError::BadResponse(e.to_string()))?
        };
        if status >= 400 {
            let err = &value["error"];
            return Err(ClientError::Api {
                status,
                code: err["code"].as_str().unwrap_or("ERROR").to_string(),
                message: err["message"].as_str().unwrap_or("request failed").to_string(),
            });
        }
        Ok(value)
    }

    pub async fn get(&self, path: &str) -> Result<Value, ClientError> {
        self.request("GET", path, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, ClientError> {
        self.request("POST", path, Some(body)).await
    }

    /// Follow `/stream`; `on_event` gets each event's JSON and returns `false` to stop.
    pub async fn stream(&self, mut on_event: impl FnMut(Value) -> bool) -> Result<(), ClientError> {
        let mut stream = self.connect().await?;
        stream.write_all(&Self::request_bytes("GET", "/stream", None)).await?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let l = line.trim_end_matches(['\r', '\n']);
            if let Some(data) = l.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim_start()) {
                    if !on_event(v) {
                        return Ok(());
                    }
                }
            }
        }
    }
}
