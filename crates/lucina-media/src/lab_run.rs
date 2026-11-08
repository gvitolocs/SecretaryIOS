//! The Lab's worker: one background task that runs queued jobs one at a time by invoking the command
//! in `LUCINA_MEDIA_LAB_CMD` (`~/models/lab/lab-edit` on nezopt), then tells the owner.
//!
//! The command is started without a shell, in its own process group, as
//! `CMD --input IN --prompt TEXT --output OUT [--reference REF] [--steps N] [--seed N]`. It owns the GPU
//! lease and exits when done, so the model never stays in VRAM. On stdout it may print
//! `MODEL <name>`, `STARTED` (the GPU lease was granted: the run timeout starts here) and
//! `PROGRESS <0-100>`; anything on stderr is kept (the tail) as the error text of a failed run.
//!
//! Two clocks: `LUCINA_MEDIA_LAB_WAIT_TIMEOUT` (default 2 h) from the start until the command's
//! `STARTED` (it may queue behind the Qwen server or another GPU job), then `LUCINA_MEDIA_LAB_TIMEOUT`
//! (default 30 min) for the run itself. A command that never prints `STARTED` gets the first clock plus
//! the second, so a simple wrapper still works.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::ingest::Library;
use crate::lab::{self, Job};

/// How the owner is told (the same two paths as the voice memos: the app inbox through
/// secretary-server's `POST /v1/notify`, and Lucina's Telegram bot).
#[derive(Clone, Debug, Default)]
pub struct NotifyConfig {
    /// `LUCINA_SECRETARY_SERVER_URL`, e.g. `https://getsecretary.tech`.
    pub secretary_url: Option<String>,
    /// `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`: the `svc_…` token (0600).
    pub service_token_file: Option<PathBuf>,
    /// `TELEGRAM_BOT_TOKEN_LUCINA` or `…_FILE`.
    pub telegram_token: Option<String>,
    /// `LUCINA_MEDIA_LAB_TELEGRAM_CHAT_ID`, `TELEGRAM_OWNER_CHAT_ID`, or the first `TELEGRAM_ALLOWED_CHAT_IDS`.
    pub telegram_chat: Option<String>,
    pub telegram_api: String,
    /// Where `/gallery/lab` lives for the link in a Telegram message.
    pub link_base: String,
    /// `en`, `it` or `da`.
    pub lang: String,
}

impl NotifyConfig {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let on = |k: &str| {
            get(k)
                .map(|v| v.trim().to_ascii_lowercase())
                .is_none_or(|v| !matches!(v.as_str(), "off" | "false" | "0"))
        };
        let nonempty = |v: Option<String>| v.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
        let telegram_token = nonempty(get("TELEGRAM_BOT_TOKEN_LUCINA")).or_else(|| {
            nonempty(
                get("TELEGRAM_BOT_TOKEN_LUCINA_FILE")
                    .and_then(|f| std::fs::read_to_string(f.trim()).ok()),
            )
        });
        let lang = nonempty(get("LUCINA_MEDIA_LAB_LANG")).unwrap_or_else(|| "en".into());
        Self {
            secretary_url: on("LUCINA_MEDIA_LAB_NOTIFY_APP")
                .then(|| nonempty(get("LUCINA_SECRETARY_SERVER_URL")))
                .flatten()
                .map(|u| u.trim_end_matches('/').to_owned()),
            service_token_file: nonempty(get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE"))
                .map(PathBuf::from),
            telegram_token: on("LUCINA_MEDIA_LAB_NOTIFY_TELEGRAM")
                .then_some(telegram_token)
                .flatten(),
            telegram_chat: nonempty(get("LUCINA_MEDIA_LAB_TELEGRAM_CHAT_ID"))
                .or_else(|| nonempty(get("TELEGRAM_OWNER_CHAT_ID")))
                .or_else(|| {
                    get("TELEGRAM_ALLOWED_CHAT_IDS")
                        .and_then(|v| v.split(',').next().map(str::to_owned))
                        .and_then(|v| nonempty(Some(v)))
                }),
            telegram_api: nonempty(get("LUCINA_MEDIA_LAB_TELEGRAM_API"))
                .unwrap_or_else(|| "https://api.telegram.org".into()),
            link_base: nonempty(get("LUCINA_MEDIA_LAB_LINK_BASE"))
                .unwrap_or_else(|| "https://getsecretary.tech".into())
                .trim_end_matches('/')
                .to_owned(),
            lang: if matches!(lang.as_str(), "it" | "da") {
                lang
            } else {
                "en".into()
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct LabConfig {
    /// The edit command and its fixed arguments (`LUCINA_MEDIA_LAB_CMD`, split on spaces; double or
    /// single quotes group). `None` = the Lab queues jobs but nothing runs them.
    pub cmd: Option<Vec<String>>,
    /// Run time after `STARTED`.
    pub timeout: Duration,
    /// Time allowed before `STARTED` (waiting for the GPU).
    pub wait_timeout: Duration,
    /// Longest side of the picture handed to the model.
    pub max_side: u32,
    pub notify: NotifyConfig,
}

impl LabConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String> + Copy) -> Self {
        let secs = |k: &str, default: u64| {
            Duration::from_secs(
                get(k)
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(default),
            )
        };
        Self {
            cmd: get("LUCINA_MEDIA_LAB_CMD")
                .map(|c| split_command(&c))
                .filter(|c| !c.is_empty()),
            timeout: secs("LUCINA_MEDIA_LAB_TIMEOUT", 1800),
            wait_timeout: secs("LUCINA_MEDIA_LAB_WAIT_TIMEOUT", 7200),
            max_side: get("LUCINA_MEDIA_LAB_MAX_SIDE")
                .and_then(|v| v.trim().parse().ok())
                .filter(|v| (256..=4096).contains(v))
                .unwrap_or(1024),
            notify: NotifyConfig::from_lookup(get),
        }
    }

    /// A config for tests: the given command, short clocks, nothing notified.
    pub fn for_command(cmd: Vec<String>) -> Self {
        Self {
            cmd: Some(cmd),
            timeout: Duration::from_secs(20),
            wait_timeout: Duration::from_secs(20),
            max_side: 1024,
            notify: NotifyConfig {
                telegram_api: "https://api.telegram.org".into(),
                link_base: "https://example.test".into(),
                lang: "en".into(),
                ..NotifyConfig::default()
            },
        }
    }
}

/// Split a command line on spaces; `'…'` and `"…"` keep spaces; no escapes, no expansion.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for ch in s.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(ch);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn wake() -> &'static Arc<Notify> {
    static W: OnceLock<Arc<Notify>> = OnceLock::new();
    W.get_or_init(|| Arc::new(Notify::new()))
}

/// Tell the worker there is something to run.
pub fn kick() {
    wake().notify_one();
}

/// The worker loop: put interrupted jobs back, then run the queue forever, one job at a time.
pub async fn worker(lib: Library, cfg: LabConfig) {
    match lib.db.call(|c| lab::requeue_interrupted(c)).await {
        Ok(n) if n > 0 => tracing::info!(jobs = n, "lab: interrupted jobs requeued"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "lab: requeue failed"),
    }
    if cfg.cmd.is_none() {
        tracing::warn!("lab: LUCINA_MEDIA_LAB_CMD is not set, queued edits wait");
    }
    loop {
        match run_next(&lib, &cfg).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "lab: worker step failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        tokio::select! {
            () = wake().notified() => {}
            () = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
    }
}

/// Run the oldest queued job to its end. `Ok(false)` when there was nothing to run.
pub async fn run_next(lib: &Library, cfg: &LabConfig) -> anyhow::Result<bool> {
    let Some(cmd) = cfg.cmd.clone() else {
        return Ok(false);
    };
    let Some(job) = lib.db.call(lab::claim_next).await? else {
        return Ok(false);
    };
    tracing::info!(job = %job.id, attempt = job.attempts, "lab: job started");
    lib.emit("lab.running", &job.id);
    let outcome = run_job(lib, cfg, &cmd, &job).await;
    let jid = job.id.clone();
    let done = match outcome {
        Ok(info) => {
            let id = jid.clone();
            let ok = lib
                .db
                .call(move |c| lab::finish_done(c, &id, &info))
                .await?;
            if ok {
                tracing::info!(job = %jid, "lab: job done");
            }
            ok.then_some(true)
        }
        Err(msg) => {
            tracing::warn!(job = %jid, error = %msg, "lab: job failed");
            let id = jid.clone();
            let ok = lib
                .db
                .call(move |c| lab::finish_failed(c, &id, &msg))
                .await?;
            ok.then_some(false)
        }
    };
    if let Some(success) = done {
        lib.emit(if success { "lab.done" } else { "lab.failed" }, &jid);
        announce(lib, cfg, &jid, success).await;
    }
    Ok(true)
}

async fn run_job(
    lib: &Library,
    cfg: &LabConfig,
    cmd: &[String],
    job: &Job,
) -> Result<lab::ResultInfo, String> {
    let (l2, j2, max_side) = (lib.clone(), job.clone(), cfg.max_side);
    let (input, reference) =
        tokio::task::spawn_blocking(move || lab::prepare_inputs(&l2, &j2, max_side))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
    let output = lab::job_dir(&lib.cfg, &job.id).join("run-output.png");
    let _ = std::fs::remove_file(&output);
    let mut args: Vec<String> = cmd[1..].to_vec();
    args.extend([
        "--input".into(),
        input.to_string_lossy().into_owned(),
        "--prompt".into(),
        job.prompt.clone(),
        "--output".into(),
        output.to_string_lossy().into_owned(),
    ]);
    if let Some(r) = &reference {
        args.extend(["--reference".into(), r.to_string_lossy().into_owned()]);
    }
    if let Some(s) = job.steps {
        args.extend(["--steps".into(), s.to_string()]);
    }
    if let Some(s) = job.seed {
        args.extend(["--seed".into(), s.to_string()]);
    }
    run_command(lib, cfg, &cmd[0], &args, &job.id).await?;
    let result = lab::result_path(&lib.cfg, &job.id);
    let (o2, r2) = (output.clone(), result.clone());
    tokio::task::spawn_blocking(move || -> anyhow::Result<lab::ResultInfo> {
        let info = lab::inspect_result(&o2)?;
        std::fs::rename(&o2, &r2)?;
        Ok(info)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("the model produced no usable picture: {e}"))
}

/// Start the command. A script that was written a moment ago can still be open for writing in another thread's forked
/// child (ETXTBSY): wait a little and try again, as a wrapper being updated would need too.
async fn spawn_with_retry(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    let mut tries = 0;
    loop {
        match command.spawn() {
            Err(e) if e.raw_os_error() == Some(26) && tries < 20 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => return other,
        }
    }
}

/// What the command has said on stdout so far.
#[derive(Default)]
struct Seen {
    /// The GPU lease was granted (`STARTED`, or the first `PROGRESS`).
    running: bool,
    last_progress: Option<i64>,
}

impl Seen {
    /// Record one stdout line in the job. Returns `true` when it was `STARTED`: the run clock starts now.
    async fn apply(&mut self, lib: &Library, job_id: &str, line: &str) -> bool {
        let id = job_id.to_owned();
        if let Some(m) = line.strip_prefix("MODEL ") {
            let m = m.trim().to_owned();
            let _ = lib.db.call(move |c| lab::set_model(c, &id, &m)).await;
        } else if line == "STARTED" {
            self.running = true;
            let _ = lib
                .db
                .call(move |c| lab::set_progress(c, &id, 0, Some("generating")))
                .await;
            return true;
        } else if let Some(p) = line.strip_prefix("PROGRESS ") {
            if let Ok(p) = p.trim().parse::<f64>() {
                let p = p.round() as i64;
                if self.last_progress != Some(p) {
                    self.last_progress = Some(p);
                    let phase = (!self.running).then_some("generating");
                    self.running = true;
                    let _ = lib
                        .db
                        .call(move |c| lab::set_progress(c, &id, p, phase))
                        .await;
                }
            }
        }
        false
    }
}

/// Run the command to its end; `Err` is the text for the owner.
async fn run_command(
    lib: &Library,
    cfg: &LabConfig,
    program: &str,
    args: &[String],
    job_id: &str,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = spawn_with_retry(&mut command)
        .await
        .map_err(|e| format!("cannot start the model runner: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let err_tail = tokio::spawn(async move {
        let mut tail = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tail.push_str(line.trim_end());
            tail.push('\n');
            if tail.len() > 4000 {
                let cut = tail.ceil_char_boundary(tail.len() - 3000);
                tail.drain(..cut);
            }
        }
        tail
    });

    let mut deadline = Instant::now() + cfg.wait_timeout + cfg.timeout;
    let mut seen = Seen::default();
    let mut lines = BufReader::new(stdout).lines();
    let mut stdout_open = true;
    let exit = loop {
        tokio::select! {
            line = lines.next_line(), if stdout_open => match line {
                Ok(Some(line)) => {
                    if seen.apply(lib, job_id, line.trim()).await {
                        deadline = Instant::now() + cfg.timeout;
                    }
                }
                _ => stdout_open = false,
            },
            status = child.wait() => break Some(status),
            () = tokio::time::sleep_until(deadline) => break None,
        }
    };
    // The command may have printed its last lines (MODEL, the final PROGRESS) just before it exited.
    if stdout_open {
        while let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_secs(2), lines.next_line()).await
        {
            seen.apply(lib, job_id, line.trim()).await;
        }
    }
    let running = seen.running;
    let status = match exit {
        Some(s) => s.map_err(|e| format!("the model runner failed: {e}"))?,
        None => {
            terminate_group(pid, &mut child).await;
            err_tail.abort();
            let what = if running {
                "ran too long"
            } else {
                "waited too long for the GPU"
            };
            return Err(format!("the model {what} and was stopped"));
        }
    };
    let tail = tokio::time::timeout(Duration::from_secs(2), err_tail)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let last: Vec<&str> = tail.lines().rev().take(3).collect();
    let detail = last.into_iter().rev().collect::<Vec<_>>().join(" | ");
    let code = status
        .code()
        .map_or_else(|| "a signal".into(), |c| c.to_string());
    Err(if detail.is_empty() {
        format!("the model runner failed (exit {code})")
    } else {
        format!("the model runner failed (exit {code}): {detail}")
    })
}

/// SIGTERM the whole group so the GPU lease is released cleanly, give the command 5 s to exit, then SIGKILL
/// whatever is left of the group.
async fn terminate_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    let Some(pid) = pid else {
        let _ = child.kill().await;
        return;
    };
    let signal = |sig: &'static str| async move {
        let _ = tokio::process::Command::new("kill")
            .args([sig, "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    };
    signal("-TERM").await;
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    signal("-KILL").await;
    let _ = child.wait().await;
}

// ---- telling the owner -----------------------------------------------------------------------

struct Words {
    title: &'static str,
    ready: &'static str,
    failed: &'static str,
}

fn words(lang: &str) -> Words {
    match lang {
        "it" => Words {
            title: "Lucina Lab",
            ready: "La tua immagine è pronta",
            failed: "Non sono riuscita a creare la tua immagine",
        },
        "da" => Words {
            title: "Lucina Lab",
            ready: "Dit billede er klar",
            failed: "Jeg kunne ikke lave dit billede",
        },
        _ => Words {
            title: "Lucina Lab",
            ready: "Your picture is ready",
            failed: "Your picture could not be made",
        },
    }
}

/// The text of the notification: `(title, body)`.
pub fn message(lang: &str, success: bool) -> (&'static str, &'static str) {
    let w = words(lang);
    (w.title, if success { w.ready } else { w.failed })
}

/// Announce a finished job, once: marked first, so a crash or a failed send never repeats it.
async fn announce(lib: &Library, cfg: &LabConfig, job_id: &str, success: bool) {
    let id = job_id.to_owned();
    match lib.db.call(move |c| lab::mark_notified(c, &id)).await {
        Ok(true) => {}
        _ => return,
    }
    match notify(&cfg.notify, job_id, success).await {
        Ok(sent) => tracing::info!(job = %job_id, channels = ?sent, "lab: owner told"),
        Err(e) => tracing::warn!(job = %job_id, error = %e, "lab: could not tell the owner"),
    }
}

/// Send to every configured channel; `Ok` when at least one took it (or none is configured).
pub async fn notify(
    cfg: &NotifyConfig,
    job_id: &str,
    success: bool,
) -> Result<Vec<&'static str>, String> {
    let (title, body) = message(&cfg.lang, success);
    let route = "/gallery/lab";
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut sent = Vec::new();
    let mut errors = Vec::new();
    if let (Some(base), Some(file)) = (&cfg.secretary_url, &cfg.service_token_file) {
        match std::fs::read_to_string(file) {
            Ok(token) => {
                let r = http
                    .post(format!("{base}/v1/notify"))
                    .bearer_auth(token.trim())
                    .json(&json!({
                        "category": "lucina_message",
                        "title": title,
                        "body": body,
                        "route": route,
                        "channel": "lab",
                        "dedupKey": format!("lab:{job_id}"),
                        "data": { "labJob": job_id },
                    }))
                    .send()
                    .await;
                match r {
                    Ok(r) if r.status().is_success() => sent.push("app"),
                    Ok(r) => errors.push(format!("app notify HTTP {}", r.status())),
                    Err(e) => errors.push(format!("app notify: {e}")),
                }
            }
            Err(e) => errors.push(format!("service token: {e}")),
        }
    }
    if let (Some(token), Some(chat)) = (&cfg.telegram_token, &cfg.telegram_chat) {
        let text = format!("🎨 {body}\n\n{}{route}", cfg.link_base);
        let r = http
            .post(format!("{}/bot{token}/sendMessage", cfg.telegram_api))
            .json(&json!({ "chat_id": chat, "text": text, "disable_web_page_preview": true }))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => sent.push("telegram"),
            // Never log the URL: it carries the bot token.
            Ok(r) => errors.push(format!("telegram HTTP {}", r.status())),
            Err(e) => errors.push(format!("telegram: {}", e.without_url())),
        }
    }
    if sent.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_simple_shell() {
        assert_eq!(split_command("a b  c"), ["a", "b", "c"]);
        assert_eq!(
            split_command("run '/x y/z' \"q r\" ''"),
            ["run", "/x y/z", "q r", ""]
        );
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn config_reads_the_environment_names() {
        let env = |k: &str| match k {
            "LUCINA_MEDIA_LAB_CMD" => Some("/bin/lab-edit --model klein4b".to_string()),
            "LUCINA_MEDIA_LAB_TIMEOUT" => Some("60".into()),
            "LUCINA_SECRETARY_SERVER_URL" => Some("https://s.example/".into()),
            "LUCINA_SECRETARY_SERVICE_TOKEN_FILE" => Some("/tok".into()),
            "TELEGRAM_ALLOWED_CHAT_IDS" => Some("42, 43".into()),
            "LUCINA_MEDIA_LAB_LANG" => Some("it".into()),
            _ => None,
        };
        let c = LabConfig::from_lookup(env);
        assert_eq!(c.cmd.unwrap(), ["/bin/lab-edit", "--model", "klein4b"]);
        assert_eq!(c.timeout, Duration::from_secs(60));
        assert_eq!(c.wait_timeout, Duration::from_secs(7200));
        assert_eq!(c.notify.secretary_url.as_deref(), Some("https://s.example"));
        assert_eq!(c.notify.telegram_chat.as_deref(), Some("42"));
        assert_eq!(c.notify.telegram_token, None);
        assert_eq!(message(&c.notify.lang, true).1, "La tua immagine è pronta");
        assert_eq!(message("xx", true).1, "Your picture is ready");
    }
}
