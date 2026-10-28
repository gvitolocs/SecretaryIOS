use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use base64::Engine;
use serde_json::{json, Value};
use tokio::process::{Child, Command};

use crate::AppState;

/// Image subtypes the vision model picks from (first line of its answer).
pub(super) const CATEGORIES: &[&str] = &[
    "photo",
    "screenshot_chat",
    "screenshot_error",
    "screenshot_other",
    "receipt",
    "document_photo",
    "whiteboard",
    "meme",
    "other",
];
const PROMPT: &str = "First line exactly: CATEGORY: <one of photo, screenshot_chat, screenshot_error, screenshot_other, receipt, document_photo, whiteboard, meme, other>. Then describe this image for an assistant who cannot see it: what it shows, people/objects/places, and transcribe any visible text exactly (for an error screenshot, the full error message; for a chat screenshot, who wrote each message).";

/// Split the model's answer into (category, description).
pub(super) fn split_category(answer: &str) -> (Option<String>, String) {
    let trimmed = answer.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let label = first
        .trim()
        .trim_start_matches(['*', '#', ' '])
        .strip_prefix("CATEGORY:")
        .or_else(|| first.trim().strip_prefix("Category:"))
        .map(|c| c.trim().trim_matches(['*', '.', ' ']).to_ascii_lowercase());
    match label {
        Some(c) => {
            let c = CATEGORIES.contains(&c.as_str()).then_some(c);
            (c, rest.trim().to_owned())
        }
        None => (None, trimmed.trim().to_owned()),
    }
}

pub(super) struct Server {
    child: Child,
    process_group: Option<i32>,
    url: String,
    last_use: Instant,
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(group) = self.process_group.take() {
            // SAFETY: a negative pid addresses only the new process group
            // created for this server and its lease wrapper/descendants.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}
async fn start(st: &AppState, gpu: bool) -> anyhow::Result<Server> {
    let cfg = &st.cfg.share;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port().to_string();
    drop(listener);
    let mut cmd = if gpu {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
        let mut cmd = Command::new(format!("{home}/bin/gpu-lease"));
        cmd.args([
            "run",
            "--name",
            "lucina-vision",
            "--mb",
            "11000",
            "--wait",
            "90",
            "--",
        ])
        .arg(&cfg.vision_bin);
        cmd
    } else {
        Command::new(&cfg.vision_bin)
    };
    cmd.process_group(0);
    let child = cmd
        .arg("-m")
        .arg(&cfg.vision_model)
        .arg("--mmproj")
        .arg(&cfg.vision_mmproj)
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "-ngl",
            if gpu { "99" } else { "0" },
            "-c",
            "8192",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    Ok(Server {
        process_group: child.id().map(|id| id as i32),
        child,
        url: format!("http://127.0.0.1:{port}"),
        last_use: Instant::now(),
    })
}
async fn ready(server: &mut Server, seconds: u64) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if server.child.try_wait()?.is_some() {
            bail!("vision process exited before becoming ready");
        }
        if http
            .get(format!("{}/health", server.url))
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bail!("vision startup timeout")
}
async fn completion(url: &str, path: &Path) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let v: Value = reqwest::Client::new().post(format!("{}/v1/chat/completions", url.trim_end_matches('/')))
        .timeout(Duration::from_secs(120)).json(&json!({"model":"gemma-4", "max_tokens":2048, "chat_template_kwargs":{"enable_thinking":false},
            "messages":[{"role":"user","content":[{"type":"text","text":PROMPT},{"type":"image_url","image_url":{"url":format!("data:image/jpeg;base64,{data}")}}]}]}))
        .send().await?.error_for_status()?.json().await?;
    let message = &v["choices"][0]["message"];
    message["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| message["reasoning_content"].as_str())
        .map(str::to_owned)
        .context("vision returned no description")
}
pub(crate) async fn describe(st: &AppState, path: &Path) -> anyhow::Result<String> {
    if let Some(url) = &st.cfg.share.vision_url {
        return completion(url, path).await;
    }
    let mut slot = st.share.vision.lock().await;
    if slot
        .as_mut()
        .is_some_and(|s| s.child.try_wait().ok().flatten().is_some())
    {
        *slot = None;
    }
    if slot.is_none() {
        let mut server = start(st, true).await?;
        if ready(&mut server, 120).await.is_err() {
            drop(server);
            server = start(st, false).await?;
            ready(&mut server, 100).await?;
        }
        *slot = Some(server);
        let weak = std::sync::Arc::downgrade(&st.0);
        tokio::spawn(async move {
            loop {
                let delay = {
                    let Some(inner) = weak.upgrade() else { return };
                    let mut slot = inner.share.vision.lock().await;
                    match slot.as_ref() {
                        Some(server) => {
                            let remaining =
                                Duration::from_secs(300).saturating_sub(server.last_use.elapsed());
                            if remaining.is_zero() {
                                slot.take();
                                return;
                            }
                            remaining
                        }
                        None => return,
                    }
                };
                tokio::time::sleep(delay).await;
            }
        });
    }
    let server = slot.as_mut().expect("vision was started");
    server.last_use = Instant::now();
    let result = completion(&server.url, path).await;
    server.last_use = Instant::now();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        let (c, d) =
            split_category("CATEGORY: screenshot_error\nA terminal showing `error[E0425]`.");
        assert_eq!(c.as_deref(), Some("screenshot_error"));
        assert_eq!(d, "A terminal showing `error[E0425]`.");
        let (c, d) = split_category("**CATEGORY: Receipt.**\nA Netto receipt.");
        assert_eq!(c.as_deref(), Some("receipt"));
        assert_eq!(d, "A Netto receipt.");
        let (c, d) = split_category("CATEGORY: spaceship\nSomething.");
        assert_eq!((c, d.as_str()), (None, "Something."));
        let (c, d) = split_category("A cat on a sofa.");
        assert_eq!((c, d.as_str()), (None, "A cat on a sofa."));
    }
}
