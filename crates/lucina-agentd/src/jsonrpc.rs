//! JSON-RPC 2.0 over a child's stdio, one JSON object per line: the transport of
//! Codex's app-server and of ACP agents (Paseo `jsonl-rpc-process.js`).
//!
//! [`Rpc::request`] waits for the matching response. Notifications and requests
//! from the child arrive on the [`Incoming`] channel; a request is answered with
//! [`Rpc::respond`] / [`Rpc::respond_error`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    #[error("{method}: {message}")]
    Remote { method: String, code: i64, message: String, data: Option<Value> },
    #[error("{0}: no response within the timeout")]
    Timeout(String),
    #[error("the agent process closed its connection")]
    Closed,
    #[error("write failed: {0}")]
    Write(String),
}

/// A message the child sent us.
#[derive(Debug, Clone)]
pub enum Incoming {
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
    /// stdout closed (the child exited or crashed).
    Closed,
}

type Pending = Arc<Mutex<HashMap<u64, (String, oneshot::Sender<Result<Value, RpcError>>)>>>;

pub struct Rpc {
    writer: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
}

impl Rpc {
    /// Start reading `stdout`; returns the client and the incoming channel.
    pub fn start(stdin: ChildStdin, stdout: ChildStdout) -> (Arc<Rpc>, mpsc::UnboundedReceiver<Incoming>) {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let rpc = Arc::new(Rpc { writer: tokio::sync::Mutex::new(stdin), pending: pending.clone(), next_id: AtomicU64::new(1) });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(line) else {
                    tracing::debug!(line, "non-JSON line from agent process");
                    continue;
                };
                let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
                let id = msg.get("id").cloned().filter(|v| !v.is_null());
                match (method, id) {
                    (Some(method), Some(id)) => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        let _ = tx.send(Incoming::Request { id, method, params });
                    }
                    (Some(method), None) => {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        let _ = tx.send(Incoming::Notification { method, params });
                    }
                    (None, Some(id)) => {
                        let Some(n) = id.as_u64() else { continue };
                        let entry = pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&n);
                        if let Some((method, reply)) = entry {
                            let result = match msg.get("error") {
                                Some(err) if !err.is_null() => Err(RpcError::Remote {
                                    method,
                                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                                    message: err.get("message").and_then(Value::as_str).unwrap_or("error").to_string(),
                                    data: err.get("data").cloned(),
                                }),
                                _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = reply.send(result);
                        }
                    }
                    (None, None) => {}
                }
            }
            for (_, (_, reply)) in pending.lock().unwrap_or_else(|p| p.into_inner()).drain() {
                let _ = reply.send(Err(RpcError::Closed));
            }
            let _ = tx.send(Incoming::Closed);
        });
        (rpc, rx)
    }

    async fn write(&self, msg: &Value) -> Result<(), RpcError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes()).await.map_err(|e| RpcError::Write(e.to_string()))?;
        w.flush().await.map_err(|e| RpcError::Write(e.to_string()))
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id, (method.to_string(), tx));
        if let Err(e) = self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await {
            self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed),
            Err(_) => {
                self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
                Err(RpcError::Timeout(method.to_string()))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params})).await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.write(&json!({"jsonrpc": "2.0", "id": id, "result": result})).await
    }

    pub async fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.write(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})).await
    }
}
