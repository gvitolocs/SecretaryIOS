//! The live speech-to-text engine: one `whisper-server` (whisper.cpp) with a
//! small model, kept loaded while somebody is on the prompter.
//!
//! - GPU first: `gpu-lease run --wait 0` reserves a little VRAM next to Qwen
//!   and the memo worker, on the RX 7900 XTX only (`GGML_VK_VISIBLE_DEVICES=1`).
//! - No lease right now (exit 75): the same server on the CPU, in a user scope
//!   capped at [`EngineConfig::cpu_quota`].
//! - Started by the first session, stopped [`IDLE_STOP`] after the last one
//!   left, so nothing stays loaded when the prompter is closed.
//!
//! Requests are one window of 16 kHz PCM each (`POST /inference`, a WAV in a
//! multipart form); the server keeps the model warm between them.
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::segmenter::{clean, RATE};

/// Stop the server this long after the last session closed.
pub const IDLE_STOP: Duration = Duration::from_secs(90);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// `gpu-lease`: not admissible right now.
const EX_TEMPFAIL: i32 = 75;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub server: PathBuf,
    pub model: PathBuf,
    pub gpu_lease: PathBuf,
    pub port: u16,
    pub gpu_mb: u32,
    pub threads: u32,
    pub cpu_quota: String,
}

impl EngineConfig {
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        whisper: &crate::transcribe::WhisperConfig,
    ) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        let bin = whisper.cli.parent().map(PathBuf::from).unwrap_or_default();
        let num = |k: &str, d: u32| get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        Self {
            server: get("LUCINA_PROMPTER_WHISPER_SERVER")
                .map(PathBuf::from)
                .unwrap_or_else(|| bin.join("whisper-server")),
            model: get("LUCINA_PROMPTER_MODEL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{home}/models/whisper/ggml-small.bin"))),
            gpu_lease: whisper.gpu_lease.clone(),
            port: num("LUCINA_PROMPTER_STT_PORT", 28796) as u16,
            gpu_mb: num("LUCINA_PROMPTER_GPU_MB", 1200),
            threads: num("LUCINA_PROMPTER_THREADS", 4),
            cpu_quota: get("LUCINA_PROMPTER_CPU_QUOTA").unwrap_or_else(|| "400%".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Gpu,
    Cpu,
}

/// One transcription.
#[derive(Debug, Clone, Default)]
pub struct SttOut {
    pub text: String,
    pub language: String,
    pub ms: u64,
}

struct Running {
    child: Child,
    mode: Mode,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    sessions: usize,
    idle_since: Option<Instant>,
    reaper: bool,
}

pub struct Engine {
    cfg: EngineConfig,
    http: reqwest::Client,
    inner: Mutex<Inner>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client"),
            inner: Mutex::default(),
        })
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.cfg.port)
    }

    /// Join as a session: the server is started (or reused) and ready on return.
    pub async fn acquire(self: &Arc<Self>) -> Result<Mode, String> {
        let mut g = self.inner.lock().await;
        g.sessions += 1;
        g.idle_since = None;
        if !g.reaper {
            g.reaper = true;
            let me = self.clone();
            tokio::spawn(async move { me.reap().await });
        }
        if let Some(r) = g.running.as_mut() {
            if matches!(r.child.try_wait(), Ok(None)) {
                return Ok(r.mode);
            }
            g.running = None;
        }
        match self.start(Mode::Gpu).await {
            Ok(r) => {
                let m = r.mode;
                g.running = Some(r);
                Ok(m)
            }
            Err(gpu_err) => {
                tracing::warn!(error = %gpu_err, "prompter: no GPU for live speech-to-text, using the CPU");
                match self.start(Mode::Cpu).await {
                    Ok(r) => {
                        g.running = Some(r);
                        Ok(Mode::Cpu)
                    }
                    Err(e) => {
                        g.sessions -= 1;
                        Err(format!("speech-to-text did not start: {e}"))
                    }
                }
            }
        }
    }

    /// Leave as a session; the reaper stops the server once idle.
    pub async fn release(&self) {
        let mut g = self.inner.lock().await;
        g.sessions = g.sessions.saturating_sub(1);
        if g.sessions == 0 {
            g.idle_since = Some(Instant::now());
        }
    }

    pub async fn status(&self) -> Value {
        let g = self.inner.lock().await;
        serde_json::json!({
            "running": g.running.as_ref().map(|r| r.mode),
            "sessions": g.sessions,
            "model": self.cfg.model.file_name().map(|n| n.to_string_lossy().into_owned()),
        })
    }

    /// Stop the server now (tests, and a CLI run that started its own).
    pub async fn shutdown(&self) {
        let mut g = self.inner.lock().await;
        if let Some(r) = g.running.take() {
            stop(r.child).await;
        }
    }

    async fn reap(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let mut g = self.inner.lock().await;
            let idle = g.idle_since.is_some_and(|t| t.elapsed() >= IDLE_STOP);
            if g.sessions == 0 && idle {
                if let Some(r) = g.running.take() {
                    tracing::info!(mode = ?r.mode, "prompter: stopping idle speech-to-text");
                    stop(r.child).await;
                }
                g.idle_since = None;
            }
        }
    }

    async fn start(&self, mode: Mode) -> Result<Running, String> {
        // A server left over from a crash of this service would hold the port without a lease.
        let _ = Command::new("pkill")
            .args([
                "-TERM",
                "-f",
                &format!("whisper-server .*--port {} ", self.cfg.port),
            ])
            .status()
            .await;
        let c = &self.cfg;
        let server_args = |cmd: &mut Command| {
            cmd.arg(&c.server)
                .arg("-m")
                .arg(&c.model)
                .args([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &c.port.to_string(),
                    "-l",
                    "auto",
                ])
                .args(["-t", &c.threads.to_string(), "-nt"]);
        };
        let mut cmd = match mode {
            Mode::Gpu => {
                let mut cmd = Command::new(&c.gpu_lease);
                cmd.args([
                    "run",
                    "--name",
                    "prompter",
                    "--mb",
                    &c.gpu_mb.to_string(),
                    "--wait",
                    "0",
                    "--linger",
                    "0",
                    "--",
                ]);
                server_args(&mut cmd);
                cmd.args(["-dev", "0"]).env("GGML_VK_VISIBLE_DEVICES", "1");
                cmd
            }
            Mode::Cpu => {
                let mut cmd = Command::new("systemd-run");
                cmd.args([
                    "--user",
                    "--scope",
                    "--quiet",
                    "-p",
                    &format!("CPUQuota={}", c.cpu_quota),
                    "--",
                ]);
                server_args(&mut cmd);
                cmd.arg("-ng");
                cmd
            }
        };
        if let Some(bin) = c.server.parent() {
            cmd.env("LD_LIBRARY_PATH", bin);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
        let started = Instant::now();
        loop {
            if let Ok(Some(st)) = child.try_wait() {
                return Err(match st.code() {
                    Some(EX_TEMPFAIL) => "no VRAM lease right now".into(),
                    _ => format!("whisper-server exited ({st})"),
                });
            }
            if let Ok(r) = self
                .http
                .get(self.base())
                .timeout(Duration::from_secs(1))
                .send()
                .await
            {
                if r.status().is_success() {
                    tracing::info!(mode = ?mode, ms = started.elapsed().as_millis() as u64, "prompter: speech-to-text ready");
                    return Ok(Running { child, mode });
                }
            }
            if started.elapsed() > START_TIMEOUT {
                stop(child).await;
                return Err("whisper-server did not answer in time".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Transcribe one window. `language` is `auto` or a code; `prompt` biases spelling.
    pub async fn transcribe(
        &self,
        pcm: &[i16],
        language: &str,
        prompt: &str,
    ) -> Result<SttOut, String> {
        let started = Instant::now();
        let boundary = format!(
            "prompter{:016x}",
            started.elapsed().as_nanos() as u64 ^ pcm.len() as u64
        );
        let mut body = Vec::with_capacity(pcm.len() * 2 + 1024);
        let field = |body: &mut Vec<u8>, name: &str, value: &str| {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
            );
        };
        field(&mut body, "response_format", "verbose_json");
        field(&mut body, "temperature", "0");
        field(&mut body, "language", language);
        if !prompt.is_empty() {
            field(&mut body, "prompt", prompt);
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"w.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&wav(pcm));
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let r = self
            .http
            .post(format!("{}/inference", self.base()))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !r.status().is_success() {
            return Err(format!("whisper-server answered {}", r.status()));
        }
        let v: Value = r.json().await.map_err(|e| e.without_url().to_string())?;
        Ok(SttOut {
            text: clean(v["text"].as_str().unwrap_or_default()),
            language: v["language"].as_str().unwrap_or_default().to_owned(),
            ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// SIGTERM (gpu-lease forwards it and frees the lease), then SIGKILL after 5 s.
async fn stop(mut child: Child) {
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await;
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

/// 16 kHz mono 16-bit WAV, padded to one second (shorter input confuses whisper).
pub fn wav(pcm: &[i16]) -> Vec<u8> {
    let n = pcm.len().max(RATE);
    let data = (n * 2) as u32;
    let mut out = Vec::with_capacity(44 + n * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(RATE as u32).to_le_bytes());
    out.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for &s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.resize(44 + n * 2, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_padding() {
        let w = wav(&[1, -1, 2]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(
            u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize,
            RATE * 2
        );
        assert_eq!(w.len(), 44 + RATE * 2);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), -1);
        let long = wav(&vec![7; RATE * 3]);
        assert_eq!(long.len(), 44 + RATE * 6);
    }
}
