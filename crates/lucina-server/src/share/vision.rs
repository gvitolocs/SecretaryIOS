use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use base64::Engine;
use serde_json::{json, Value};
use tokio::process::{Child, Command};

use crate::AppState;

use super::timing::{Outcome, ShareTiming, Sizes};

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

const READ_TEXT_PROMPT: &str = "Transcribe all the text in this image exactly as written, line by line, keeping each label with its value. Digits and letters matter: do not guess, and write [?] for a character you cannot read. Nothing else.";

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
    /// The server's (and gpu-lease's) output of this start: why it exited, when it does.
    log: std::path::PathBuf,
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
/// How long a picture waits for another's start of the server.
const START_WAIT: Duration = Duration::from_secs(GPU_READY_SECS + 30);

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
    for pid in dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
    {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<_> = raw
            .split(|b| *b == 0)
            .map(String::from_utf8_lossy)
            .collect();
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
        && args
            .windows(2)
            .any(|w| w[0] == "--mmproj" && w[1] == mmproj)
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
    // Its output goes to a file, rewritten at each start: a deny from gpu-lease ("budget: held 21500 …") or a
    // crash used to be invisible ("exited before becoming ready", 5 Nov 2026).
    let log = st.cfg.data_dir.join("vision-server.log");
    let out = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&log)?
    };
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
        .stdout(out.try_clone()?)
        .stderr(out)
        .kill_on_drop(true)
        .spawn()?;
    Ok(Server {
        process_group: child.id().map(|id| id as i32),
        child,
        url: format!("http://127.0.0.1:{port}"),
        last_use: Instant::now(),
        log,
    })
}
/// Waiting for the server to answer `/health`: the lease handing over the GPU and the cold load. Its
/// line says where a picture that never got a description spent its time.
async fn ready(server: &mut Server, seconds: u64, timing: &ShareTiming) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let phase = timing.begin("vision_ready", true);
    while Instant::now() < deadline {
        if let Some(status) = server.child.try_wait()? {
            phase.finish(Outcome::Error, Sizes::NONE);
            bail!(
                "vision process exited before becoming ready ({status}): {}",
                last_lines(&server.log, 3)
            );
        }
        if http
            .get(format!("{}/health", server.url))
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            phase.finish(Outcome::Ok, Sizes::NONE);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    phase.finish(Outcome::Timeout, Sizes::NONE);
    bail!("vision startup timeout")
}
/// The last `n` non-empty lines of a log file, on one line (empty when there is none).
fn last_lines(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..]
        .join(" | ")
        .chars()
        .take(400)
        .collect()
}

async fn completion(url: &str, path: &Path, prompt: &str) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let v: Value = reqwest::Client::new().post(format!("{}/v1/chat/completions", url.trim_end_matches('/')))
        .timeout(Duration::from_secs(120)).json(&json!({"model":"gemma-4", "max_tokens":2048, "chat_template_kwargs":{"enable_thinking":false},
            "messages":[{"role":"user","content":[{"type":"text","text":prompt},{"type":"image_url","image_url":{"url":format!("data:image/jpeg;base64,{data}")}}]}]}))
        .send().await?.error_for_status()?.json().await?;
    let message = &v["choices"][0]["message"];
    message["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| message["reasoning_content"].as_str())
        .map(str::to_owned)
        .context("vision returned no description")
}
/// One request to the server, with the line for the wait it is: this is the seconds the model
/// spends looking at the picture.
async fn completion_timed(
    url: &str,
    path: &Path,
    prompt: &str,
    timing: &ShareTiming,
) -> anyhow::Result<String> {
    let phase = timing.begin("vision_request", true);
    let out = completion(url, path, prompt).await;
    phase.finish(
        Outcome::of(&out),
        Sizes::chars(out.as_ref().ok().map_or(0, |s| s.chars().count())),
    );
    out
}

/// The running server's address, marking it used. The lock is held only for this: a turn that stalls
/// while starting the server or waiting for an answer must not stop every later picture (2 Nov 2026).
async fn running(st: &AppState) -> Option<String> {
    let mut slot = st.share.vision.lock().await;
    if slot
        .as_mut()
        .is_some_and(|s| s.child.try_wait().ok().flatten().is_some())
    {
        *slot = None;
    }
    slot.as_mut().map(|s| {
        s.last_use = Instant::now();
        s.url.clone()
    })
}

/// The server's address, starting it when none runs. One start at a time; a wait for another's start is bounded.
async fn server_url(st: &AppState, cpu_too: bool, timing: &ShareTiming) -> anyhow::Result<String> {
    if let Some(url) = running(st).await {
        return Ok(url);
    }
    // A picture queued behind another's start waits here, up to START_WAIT.
    let lock_phase = timing.begin("vision_start_lock", true);
    let start_lock = tokio::time::timeout(START_WAIT, st.share.vision_start.lock()).await;
    lock_phase.finish(Outcome::of(&start_lock), Sizes::NONE);
    let _start = start_lock.context("another start of the vision server is taking too long")?;
    if let Some(url) = running(st).await {
        return Ok(url);
    }
    let spawn = timing.begin("vision_spawn", false);
    let started = start(st, true).await;
    spawn.finish(Outcome::of(&started), Sizes::NONE);
    let mut server = started?;
    if let Err(e) = ready(&mut server, GPU_READY_SECS, timing).await {
        drop(server);
        if !cpu_too {
            tracing::info!(error = %e, "vision did not start on the GPU; the OCR text will do");
            bail!("the GPU is busy");
        }
        tracing::warn!(error = %e, "vision did not start on the GPU; trying the CPU");
        let spawn = timing.begin("vision_spawn", false);
        let started = start(st, false).await;
        spawn.finish(Outcome::of(&started), Sizes::NONE);
        server = started?;
        ready(&mut server, 100, timing).await?;
    }
    let url = server.url.clone();
    *st.share.vision.lock().await = Some(server);
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
    Ok(url)
}

/// `cpu_too`: fall back to the CPU (minutes) when the GPU is taken. Not worth it once OCR has the text.
pub(crate) async fn describe(
    st: &AppState,
    path: &Path,
    cpu_too: bool,
    timing: &ShareTiming,
) -> anyhow::Result<String> {
    if let Some(url) = &st.cfg.share.vision_url {
        return completion_timed(url, path, PROMPT, timing).await;
    }
    let url = server_url(st, cpu_too, timing).await?;
    let result = completion_timed(&url, path, PROMPT, timing).await;
    // Its idle time counts from the answer.
    let _ = running(st).await;
    result
}

/// Every piece of text in a picture, as written. Only with the server already up: it follows a description.
pub(crate) async fn read_text(
    st: &AppState,
    path: &Path,
    timing: &ShareTiming,
) -> anyhow::Result<String> {
    let url = match &st.cfg.share.vision_url {
        Some(url) => url.clone(),
        None => running(st)
            .await
            .context("the vision server is not running")?,
    };
    let result = completion_timed(&url, path, READ_TEXT_PROMPT, timing).await;
    let _ = running(st).await;
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
            &cow(&[
                "/home/nez/llama.cpp/build/bin/llama-server",
                "-m",
                "x",
                "--mmproj",
                m
            ]),
            m
        ));
        // Its scope and lease wrappers carry the same arguments; Qwen has no such mmproj.
        assert!(!is_vision_server(&cow(&["systemd-run", "--mmproj", m]), m));
        assert!(!is_vision_server(
            &cow(&["/bin/llama-server", "-m", "qwen.gguf"]),
            m
        ));
        assert!(!is_vision_server(
            &cow(&["/bin/llama-server", "--mmproj", "other"]),
            m
        ));
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
