//! Speech-to-text with the local whisper.cpp on nezopt.
//!
//! 1. `ffmpeg` decodes the upload to 16 kHz mono PCM WAV; `ffprobe` measures it.
//! 2. `whisper-cli` (the `gpu-lease` shim in `~/Projects/whisper.cpp/build/bin`)
//!    runs large-v3 on the RX 7900 XTX with `-l auto`, writing JSON with
//!    segment offsets. The shim waits for a VRAM lease (FIFO); if none is
//!    granted within `gpu_wait` it exits 75 and we fall back to the CPU model
//!    (`-ng`, which the shim passes straight to the real binary).
//! 3. Known silence hallucinations ("Sottotitoli creati dalla comunità
//!    Amara.org", "Teksting av ...") are dropped; repeated identical lines
//!    are collapsed.
//!
//! Audio never leaves this host.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use tokio::process::Command;

use crate::model::{Segment, Transcript, Word};

#[derive(Debug, Clone)]
pub struct WhisperConfig {
    /// The `whisper-cli` shim (holds a gpu-lease) or the real binary.
    pub cli: PathBuf,
    /// GPU model (large-v3).
    pub model: PathBuf,
    /// CPU fallback model; None = wait for the GPU without a deadline.
    pub cpu_model: Option<PathBuf>,
    /// Silero VAD model for `--vad`, if present.
    pub vad_model: Option<PathBuf>,
    /// How long to wait for a VRAM lease before the CPU fallback.
    pub gpu_wait: Duration,
    pub gpu_mb: u32,
    pub threads: u32,
    /// Forced language (`it`, `en`, `da`); None = auto-detect.
    pub language: Option<String>,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// `gpu-lease` (for `check`: is the GPU free right now).
    pub gpu_lease: PathBuf,
    /// Hard cap on one whisper run.
    pub timeout: Duration,
}

impl WhisperConfig {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        let path = |k: &str, d: String| PathBuf::from(get(k).unwrap_or(d));
        let opt_path = |k: &str, d: String| {
            let p = PathBuf::from(get(k).unwrap_or(d));
            (!p.as_os_str().is_empty() && p.exists()).then_some(p)
        };
        let models = format!("{home}/models/whisper");
        Self {
            cli: path(
                "LUCINA_MEMO_WHISPER_CLI",
                format!("{home}/Projects/whisper.cpp/build/bin/whisper-cli"),
            ),
            model: path(
                "LUCINA_MEMO_WHISPER_MODEL",
                format!("{models}/ggml-large-v3.bin"),
            ),
            cpu_model: opt_path(
                "LUCINA_MEMO_WHISPER_CPU_MODEL",
                format!("{models}/ggml-large-v3-turbo-q8_0.bin"),
            ),
            vad_model: opt_path(
                "LUCINA_MEMO_WHISPER_VAD_MODEL",
                format!("{models}/ggml-silero-v5.1.2.bin"),
            ),
            gpu_wait: Duration::from_secs(
                get("LUCINA_MEMO_GPU_WAIT_SEC")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(900),
            ),
            gpu_mb: get("LUCINA_MEMO_GPU_MB")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(4500),
            threads: get("LUCINA_MEMO_WHISPER_THREADS")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(8),
            language: get("LUCINA_MEMO_LANGUAGE")
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty() && v != "auto"),
            ffmpeg: path("LUCINA_MEMO_FFMPEG", "/usr/bin/ffmpeg".into()),
            gpu_lease: path("LUCINA_MEMO_GPU_LEASE", format!("{home}/bin/gpu-lease")),
            ffprobe: path("LUCINA_MEMO_FFPROBE", "/usr/bin/ffprobe".into()),
            timeout: Duration::from_secs(
                get("LUCINA_MEMO_WHISPER_TIMEOUT_SEC")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(4 * 3600),
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("audio decode failed: {0}")]
    Decode(String),
    #[error("whisper failed: {0}")]
    Whisper(String),
    /// No VRAM lease within the wait and no CPU model.
    #[error("GPU busy (no gpu-lease within {0:?}) and no CPU model configured")]
    GpuBusy(Duration),
    /// The model file is not there (it lives on the MyBook, locked after a reboot).
    #[error("Whisper model not available ({0}); is the MyBook unlocked?")]
    ModelMissing(std::path::PathBuf),
}

impl SttError {
    /// Worth waiting for (GPU or model back later), not a failure of this memo.
    pub fn is_transient(&self) -> bool {
        matches!(self, SttError::GpuBusy(_) | SttError::ModelMissing(_))
    }
}

/// Speech to text as the meeting pipeline sees it (tests use a fake).
#[async_trait::async_trait]
pub trait Stt: Send + Sync {
    /// `input` → 16 kHz mono WAV at `wav`; the duration in seconds.
    async fn decode(&self, input: &Path, wav: &Path) -> Result<f64, SttError>;
    /// Whisper with word timings, waiting up to `gpu_wait` for a lease.
    async fn transcribe_words(
        &self,
        wav: &Path,
        work: &Path,
        duration_sec: f64,
        gpu_wait: Duration,
    ) -> Result<Transcript, SttError>;
    async fn gpu_free(&self) -> bool;
}

/// The real thing: ffmpeg and whisper.cpp under `gpu-lease`.
pub struct Whisper(pub WhisperConfig);

#[async_trait::async_trait]
impl Stt for Whisper {
    async fn decode(&self, input: &Path, wav: &Path) -> Result<f64, SttError> {
        decode(&self.0, input, wav).await
    }
    async fn transcribe_words(
        &self,
        wav: &Path,
        work: &Path,
        duration_sec: f64,
        gpu_wait: Duration,
    ) -> Result<Transcript, SttError> {
        transcribe_words(&self.0, wav, work, duration_sec, gpu_wait).await
    }
    async fn gpu_free(&self) -> bool {
        gpu_free(&self.0).await
    }
}

/// Rewraps an ADTS `.aac` upload (the iPhone's crash-safe recording) as `.m4a` without
/// re-encoding; a file cut off mid-frame is re-encoded instead. Returns the new file.
pub async fn remux_adts(cfg: &WhisperConfig, input: &Path, output: &Path) -> Result<(), SttError> {
    let tmp = output.with_extension("m4a.tmp");
    for codec in [&["-c:a", "copy"][..], &["-c:a", "aac", "-b:a", "96k"][..]] {
        let out = Command::new(&cfg.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(input)
            .args(["-vn"])
            .args(codec)
            .args(["-movflags", "+faststart", "-f", "mp4"])
            .arg(&tmp)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| SttError::Decode(format!("ffmpeg: {e}")))?;
        if out.status.success() && tokio::fs::metadata(&tmp).await.is_ok_and(|m| m.len() > 0) {
            tokio::fs::rename(&tmp, output)
                .await
                .map_err(|e| SttError::Decode(e.to_string()))?;
            return Ok(());
        }
        tracing::info!(error = %tail(&out.stderr), "ADTS remux with {:?} failed", codec);
    }
    let _ = tokio::fs::remove_file(&tmp).await;
    Err(SttError::Decode(
        "the ADTS recording could not be read".into(),
    ))
}

/// Decodes `input` to `wav` (16 kHz mono s16) and returns its duration.
pub async fn decode(cfg: &WhisperConfig, input: &Path, wav: &Path) -> Result<f64, SttError> {
    let out = Command::new(&cfg.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(wav)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| SttError::Decode(format!("ffmpeg: {e}")))?;
    if !out.status.success() {
        return Err(SttError::Decode(tail(&out.stderr)));
    }
    duration(cfg, wav).await
}

pub async fn duration(cfg: &WhisperConfig, file: &Path) -> Result<f64, SttError> {
    let out = Command::new(&cfg.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| SttError::Decode(format!("ffprobe: {e}")))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|_| SttError::Decode(format!("ffprobe gave no duration: {}", tail(&out.stderr))))
}

fn tail(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let s = s.trim();
    let start = s.len().saturating_sub(600);
    let start = (start..=s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s[start..].to_owned()
}

/// True when a whisper lease would be granted now (`gpu-lease check`).
pub async fn gpu_free(cfg: &WhisperConfig) -> bool {
    Command::new(&cfg.gpu_lease)
        .args([
            "check",
            "--name",
            "whisper",
            "--mb",
            &cfg.gpu_mb.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Transcribes a 16 kHz WAV in `work`: GPU (waiting up to `gpu_wait` for a lease), then
/// the CPU model if one is configured.
pub async fn transcribe(
    cfg: &WhisperConfig,
    wav: &Path,
    work: &Path,
    duration_sec: f64,
    gpu_wait: Duration,
) -> Result<Transcript, SttError> {
    run(cfg, wav, work, duration_sec, gpu_wait, false).await
}

/// [`transcribe`] with word timings (`-ojf`): every segment carries its words (meetings, where
/// a segment may span two speakers).
pub async fn transcribe_words(
    cfg: &WhisperConfig,
    wav: &Path,
    work: &Path,
    duration_sec: f64,
    gpu_wait: Duration,
) -> Result<Transcript, SttError> {
    run(cfg, wav, work, duration_sec, gpu_wait, true).await
}

async fn run(
    cfg: &WhisperConfig,
    wav: &Path,
    work: &Path,
    duration_sec: f64,
    gpu_wait: Duration,
    words: bool,
) -> Result<Transcript, SttError> {
    if !cfg.model.is_file() {
        tracing::warn!(model = %cfg.model.display(), "Whisper model missing");
        return Err(SttError::ModelMissing(cfg.model.clone()));
    }
    let prefix = work.join("whisper");
    let gpu = run_whisper(cfg, wav, &prefix, &cfg.model, true, gpu_wait, words).await;
    let (out, engine) = match gpu {
        Ok(o) => (o, format!("whisper.cpp {} (GPU)", model_name(&cfg.model))),
        Err(RunError::NoLease) => {
            let Some(cpu) = &cfg.cpu_model else {
                return Err(SttError::GpuBusy(gpu_wait));
            };
            tracing::info!("no GPU lease within {gpu_wait:?}; transcribing on the CPU");
            let o = run_whisper(cfg, wav, &prefix, cpu, false, gpu_wait, words)
                .await
                .map_err(|e| SttError::Whisper(e.to_string()))?;
            (o, format!("whisper.cpp {} (CPU)", model_name(cpu)))
        }
        Err(e) => return Err(SttError::Whisper(e.to_string())),
    };
    parse_output(&out, engine, duration_sec)
}

fn model_name(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model")
        .trim_start_matches("ggml-")
        .to_owned()
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("no gpu-lease")]
    NoLease,
    #[error("{0}")]
    Failed(String),
}

async fn run_whisper(
    cfg: &WhisperConfig,
    wav: &Path,
    prefix: &Path,
    model: &Path,
    gpu: bool,
    gpu_wait: Duration,
    words: bool,
) -> Result<String, RunError> {
    let json = prefix.with_extension("json");
    let _ = tokio::fs::remove_file(&json).await;
    let mut cmd = Command::new(&cfg.cli);
    if let Some(bin) = cfg.cli.parent() {
        cmd.env("LD_LIBRARY_PATH", bin);
    }
    cmd.arg("-m").arg(model);
    cmd.arg("-l").arg(cfg.language.as_deref().unwrap_or("auto"));
    cmd.args([
        "-t",
        &cfg.threads.to_string(),
        if words { "-ojf" } else { "-oj" },
        "-mc",
        "0",
        "-sns",
    ]);
    if gpu {
        // RX 7900 XTX only (skip the Raphael iGPU), like ~/bin/whisper-gpu.
        cmd.env("GGML_VK_VISIBLE_DEVICES", "1")
            .env("GPU_LEASE_WAIT", gpu_wait.as_secs().to_string())
            .env("GPU_LEASE_LINGER", "0")
            .env("WHISPER_GPU_MB", cfg.gpu_mb.to_string())
            .args(["-fa", "-dev", "0"]);
    } else {
        cmd.arg("-ng");
    }
    if let Some(vad) = &cfg.vad_model {
        cmd.arg("--vad").arg("-vm").arg(vad);
    }
    cmd.arg("-of").arg(prefix).arg("-f").arg(wav);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| RunError::Failed(format!("spawn {}: {e}", cfg.cli.display())))?;
    let out = tokio::time::timeout(cfg.timeout, child.wait_with_output())
        .await
        .map_err(|_| RunError::Failed(format!("timed out after {:?}", cfg.timeout)))?
        .map_err(|e| RunError::Failed(e.to_string()))?;
    match out.status.code() {
        Some(0) => {}
        Some(75) if gpu => return Err(RunError::NoLease),
        _ => {
            return Err(RunError::Failed(format!(
                "exit {:?}: {}",
                out.status.code(),
                tail(&out.stderr)
            )))
        }
    }
    tokio::fs::read_to_string(&json)
        .await
        .map_err(|e| RunError::Failed(format!("no JSON output ({e}): {}", tail(&out.stderr))))
}

#[derive(Deserialize)]
struct WhisperJson {
    #[serde(default)]
    result: Option<WhisperResult>,
    #[serde(default)]
    transcription: Vec<WhisperSegment>,
}
#[derive(Deserialize)]
struct WhisperResult {
    #[serde(default)]
    language: Option<String>,
}
#[derive(Deserialize)]
struct WhisperSegment {
    offsets: Offsets,
    text: String,
    /// `[SPEAKER_TURN]` when tinydiarize is on (not used with large-v3).
    #[serde(default)]
    speaker_turn_next: bool,
    /// `-ojf` only: the segment's tokens with their offsets.
    #[serde(default)]
    tokens: Vec<WhisperToken>,
}
#[derive(Deserialize)]
struct WhisperToken {
    text: String,
    offsets: Offsets,
}

/// Tokens → words: a token starting with a space starts a word; special tokens (`[_BEG_]`,
/// `[_TT_150]`, `<|en|>`) are skipped.
fn words_of(tokens: &[WhisperToken]) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for t in tokens {
        let text = t.text.as_str();
        if text.starts_with("[_") || text.starts_with("<|") || text.trim().is_empty() {
            continue;
        }
        let (start, end) = (
            t.offsets.from.max(0) as f64 / 1000.0,
            t.offsets.to.max(0) as f64 / 1000.0,
        );
        match out.last_mut() {
            Some(w) if !text.starts_with(' ') => {
                w.text.push_str(text);
                w.end = w.end.max(end);
            }
            _ => out.push(Word {
                start,
                end: end.max(start),
                text: text.trim().to_owned(),
            }),
        }
    }
    out.retain(|w| !w.text.is_empty());
    out
}
/// With `--vad`, whisper.cpp maps segment offsets back to the original audio but leaves token
/// offsets relative to the VAD-trimmed audio; shift such words to start at the segment's start.
fn align_words(mut words: Vec<Word>, s0: f64, s1: f64) -> Vec<Word> {
    let Some(first) = words.first() else {
        return words;
    };
    if (first.start - s0).abs() <= 0.5 {
        return words;
    }
    let (delta, hi) = (s0 - first.start, s1.max(s0));
    for w in &mut words {
        w.start = (w.start + delta).clamp(s0, hi);
        w.end = (w.end + delta).clamp(w.start, hi);
    }
    words
}
#[derive(Deserialize)]
struct Offsets {
    from: i64,
    to: i64,
}

/// whisper.cpp JSON → [`Transcript`], with hallucination filtering.
pub fn parse_output(json: &str, engine: String, duration_sec: f64) -> Result<Transcript, SttError> {
    let w: WhisperJson =
        serde_json::from_str(json).map_err(|e| SttError::Whisper(format!("bad JSON: {e}")))?;
    let language = w
        .result
        .and_then(|r| r.language)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "und".into());
    let mut segments: Vec<Segment> = Vec::new();
    for s in w.transcription {
        let _ = s.speaker_turn_next;
        let text = s.text.trim().to_owned();
        if text.is_empty() || is_hallucination(&text) {
            continue;
        }
        if segments.last().is_some_and(|p| p.text == text) {
            // whisper loops repeat a line verbatim; keep the first.
            if let Some(p) = segments.last_mut() {
                p.end = s.offsets.to as f64 / 1000.0;
            }
            continue;
        }
        let (start, end) = (
            s.offsets.from.max(0) as f64 / 1000.0,
            s.offsets.to.max(0) as f64 / 1000.0,
        );
        segments.push(Segment {
            start,
            end,
            text,
            speaker: None,
            words: align_words(words_of(&s.tokens), start, end),
        });
    }
    Ok(Transcript {
        language,
        engine,
        duration_sec,
        segments,
        speakers: false,
        created_at: Utc::now(),
    })
}

/// Lines whisper invents on silence or music (subtitle credits, sign-offs).
pub fn is_hallucination(text: &str) -> bool {
    let t = text.to_lowercase();
    let t = t.trim_matches(|c: char| !c.is_alphanumeric());
    const EXACT: &[&str] = &[
        "thank you",
        "thanks for watching",
        "thank you for watching",
        "grazie per la visione",
        "grazie a tutti",
        "tak fordi du så med",
        "tak for nu",
        "you",
        "bye",
    ];
    const CONTAINS: &[&str] = &[
        "amara.org",
        "sottotitoli creati",
        "sottotitoli a cura",
        "teksting av",
        "tekster af",
        "undertekster af",
        "subtitles by",
        "transcribed by",
        "www.",
        "dog barks",
        "musica",
        "[music]",
        "♪",
    ];
    // A short line that is only a credit or a sign-off; real speech that
    // happens to say "thank you" inside a sentence is kept.
    EXACT.contains(&t) || (t.chars().count() < 80 && CONTAINS.iter().any(|c| t.contains(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whisper_json() {
        let json = r#"{"result":{"language":"it"},"transcription":[
          {"timestamps":{"from":"00:00:00,000","to":"00:00:04,000"},"offsets":{"from":0,"to":4000},"text":" Domani chiamo Marco per il preventivo."},
          {"offsets":{"from":4000,"to":6000},"text":" Domani chiamo Marco per il preventivo."},
          {"offsets":{"from":6000,"to":9000},"text":" Sottotitoli creati dalla comunità Amara.org"},
          {"offsets":{"from":9000,"to":12500},"text":" Poi la palestra alle sette."}]}"#;
        let t = parse_output(json, "test".into(), 12.5).unwrap();
        assert_eq!(t.language, "it");
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0].end, 6.0);
        assert_eq!(t.segments[1].start, 9.0);
        assert_eq!(
            t.text(),
            "Domani chiamo Marco per il preventivo. Poi la palestra alle sette."
        );
    }

    #[test]
    fn word_timings_from_full_json() {
        let json = r#"{"result":{"language":"en"},"transcription":[
          {"offsets":{"from":0,"to":2000},"text":" Hello there, Anna.","tokens":[
            {"text":"[_BEG_]","offsets":{"from":0,"to":0}},
            {"text":" Hello","offsets":{"from":0,"to":400}},
            {"text":" there","offsets":{"from":400,"to":800}},
            {"text":",","offsets":{"from":800,"to":820}},
            {"text":" An","offsets":{"from":900,"to":1200}},
            {"text":"na.","offsets":{"from":1200,"to":1500}},
            {"text":"[_TT_100]","offsets":{"from":2000,"to":2000}}]}]}"#;
        let t = parse_output(json, "test".into(), 2.0).unwrap();
        let w = &t.segments[0].words;
        assert_eq!(
            w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["Hello", "there,", "Anna."]
        );
        assert_eq!((w[2].start, w[2].end), (0.9, 1.5));
    }

    #[test]
    fn vad_word_timings_are_moved_into_their_segment() {
        // whisper.cpp --vad: segment offsets are on the original audio, token offsets on the
        // trimmed one.
        let json = r#"{"result":{"language":"en"},"transcription":[
          {"offsets":{"from":4930,"to":9470},"text":" Hello there, Anna.","tokens":[
            {"text":"[_BEG_]","offsets":{"from":0,"to":0}},
            {"text":" Hello","offsets":{"from":20,"to":460}},
            {"text":" there","offsets":{"from":460,"to":900}},
            {"text":",","offsets":{"from":900,"to":920}},
            {"text":" An","offsets":{"from":1000,"to":1300}},
            {"text":"na.","offsets":{"from":1300,"to":1600}}]},
          {"offsets":{"from":14220,"to":18580},"text":" How are you?","tokens":[
            {"text":" How","offsets":{"from":4880,"to":5200}},
            {"text":" are","offsets":{"from":5200,"to":5500}},
            {"text":" you?","offsets":{"from":5500,"to":9900}}]}]}"#;
        let t = parse_output(json, "test".into(), 20.0).unwrap();
        let (a, b) = (&t.segments[0], &t.segments[1]);
        assert!((a.words[0].start - 4.93).abs() < 1e-9);
        assert!((b.words[0].start - 14.22).abs() < 1e-9);
        for s in [a, b] {
            assert!(!s.words.is_empty());
            for w in &s.words {
                assert!(
                    s.start <= w.start && w.start <= w.end && w.end <= s.end,
                    "{w:?} in {s:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_missing_model_is_transient() {
        let mut cfg = WhisperConfig::from_lookup(|_| None);
        cfg.model = "/nonexistent/ggml-large-v3.bin".into();
        let dir = std::env::temp_dir();
        let e = transcribe(&cfg, &dir.join("x.wav"), &dir, 1.0, Duration::ZERO)
            .await
            .unwrap_err();
        assert!(
            matches!(e, SttError::ModelMissing(_)) && e.is_transient(),
            "{e}"
        );
    }

    /// A recording cut off mid-frame (iOS killed the app) still becomes a playable .m4a.
    #[tokio::test]
    async fn cut_off_adts_is_recovered() {
        let cfg = WhisperConfig::from_lookup(|_| None);
        if !cfg.ffmpeg.exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full.aac");
        let st = std::process::Command::new(&cfg.ffmpeg)
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=4",
                "-c:a",
                "aac",
                "-f",
                "adts",
            ])
            .arg(&full)
            .status()
            .unwrap();
        assert!(st.success());
        let bytes = std::fs::read(&full).unwrap();
        let cut = dir.path().join("cut.aac");
        std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3 + 7]).unwrap();
        let out = dir.path().join("audio.m4a");
        remux_adts(&cfg, &cut, &out).await.unwrap();
        let d = duration(&cfg, &out).await.unwrap();
        assert!(d > 2.0 && d < 4.0, "duration {d}");
    }

    #[test]
    fn hallucinations() {
        assert!(is_hallucination("Thank you."));
        assert!(is_hallucination("Teksting av Nicolai Winther"));
        assert!(!is_hallucination(
            "I wanted to say thank you to Anna for the help with the move."
        ));
        assert!(!is_hallucination(
            "Okay, so the plan for tomorrow is simple."
        ));
    }
}
