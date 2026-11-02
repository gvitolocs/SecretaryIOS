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
/// The lease wait (`--wait 90` below) plus a cold load. On 2 Nov 2026 a 120 s limit, counting the 51 s the
/// lease waited for Qwen to unload, killed a load that needed longer; the slow CPU start then ate the read's
/// whole limit, and the screenshot "stayed unread".
const GPU_READY_SECS: u64 = 240;

/// A vision server outlives a restart of lucina-rs: it runs in its own scope (`scoped-llama-server`) and its
/// `Drop` never ran. It then holds VRAM the lease no longer accounts for. Stop any left over at startup.
pub fn stop_orphans(cfg: &super::ShareConfig) {
    if cfg.vision_url.is_some() {
        return;
    }
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return;
    };
    let mmproj = cfg.vision_mmproj.to_string_lossy();
    for pid in dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok()) {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<_> = raw.split(|b| *b == 0).map(String::from_utf8_lossy).collect();
        if is_vision_server(&args, &mmproj) {
            tracing::info!(pid, "stopping a vision server left by an earlier lucina-rs");
            // SAFETY: a signal to the process whose command line was just read as ours.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
}

/// Lucina's vision server, and not the `systemd-run` or lease wrappers holding the same arguments.
fn is_vision_server(args: &[std::borrow::Cow<'_, str>], mmproj: &str) -> bool {
    args.first().is_some_and(|a| a.ends_with("llama-server"))
        && args.windows(2).any(|w| w[0] == "--mmproj" && w[1] == mmproj)
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
            // An image goes to the encoder in one batch, and a landscape picture is more tokens than the
            // default 512: the server aborts on it (GGML_ASSERT n_ubatch >= n_tokens, 2 Nov 2026).
            "-b",
            "4096",
            "-ub",
            "4096",
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
/// `cpu_too`: fall back to the CPU (minutes) when the GPU is taken. Not worth it once OCR has the text.
pub(crate) async fn describe(st: &AppState, path: &Path, cpu_too: bool) -> anyhow::Result<String> {
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
        if let Err(e) = ready(&mut server, GPU_READY_SECS).await {
            drop(server);
            if !cpu_too {
                tracing::info!(error = %e, "vision did not start on the GPU; the OCR text will do");
                bail!("the GPU is busy");
            }
            tracing::warn!(error = %e, "vision did not start on the GPU; trying the CPU");
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
    fn only_the_vision_server_counts_as_an_orphan() {
        let cow = |a: &[&'static str]| -> Vec<std::borrow::Cow<'_, str>> {
            a.iter().map(|s| std::borrow::Cow::Borrowed(*s)).collect()
        };
        let m = "/models/mmproj.gguf";
        assert!(is_vision_server(
            &cow(&["/home/nez/llama.cpp/build/bin/llama-server", "-m", "x", "--mmproj", m]),
            m
        ));
        // Its scope and lease wrappers carry the same arguments; Qwen has no such mmproj.
        assert!(!is_vision_server(&cow(&["systemd-run", "--mmproj", m]), m));
        assert!(!is_vision_server(&cow(&["/bin/llama-server", "-m", "qwen.gguf"]), m));
        assert!(!is_vision_server(&cow(&["/bin/llama-server", "--mmproj", "other"]), m));
    }

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
