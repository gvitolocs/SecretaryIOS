//! voice-probe: measures voice latency against Secretary (see README.md).
//!
//! The token, the audio, the transcript and the reply never go to the log:
//! only character counts are printed, and the texts only with --show-text.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use voice_probe::{ima, pace, resample, share, turn, wav, ws};

#[derive(Parser)]
#[command(name = "voice-probe", about = "Measures voice latency against Secretary")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The voice socket (protocol sections 2 and 10).
    Ws {
        /// e.g. wss://getsecretary.tech/v1/voice/ws
        #[arg(long)]
        url: String,
        /// A file holding the Secretary device token (Bearer; never printed).
        #[arg(long)]
        token_file: PathBuf,
        /// Input: s16 mono WAV at any rate, or IMA-ADPCM mono WAV.
        #[arg(long)]
        wav: PathBuf,
        #[arg(long, value_enum, default_value = "command")]
        purpose: Purpose,
        #[arg(long, value_enum, default_value = "pcm16")]
        codec: SendCodec,
        /// Also send the reply as text while it is written (delta/answer).
        #[arg(long)]
        text: bool,
        /// No speech at all; the reply comes as text.
        #[arg(long)]
        no_speak: bool,
        #[arg(long)]
        thread: Option<Thread>,
        #[arg(long)]
        channel: Option<Channel>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        /// Audio per frame, in ms (clients send 20 to 100).
        #[arg(long, default_value_t = 40)]
        frame_ms: u64,
        /// Send all frames at once (no real-time pacing).
        #[arg(long)]
        no_realtime: bool,
        /// Reuse the same socket for N turns (how a client keeps the connection).
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Pause between repeats, in seconds.
        #[arg(long, default_value_t = 3)]
        pause_s: u64,
        /// Write the last reply's s16le PCM as a WAV.
        #[arg(long)]
        out_audio: Option<PathBuf>,
        /// One JSON object per turn, plus a JSON summary.
        #[arg(long)]
        json: bool,
        /// Print the transcript/reply text (default: character counts only).
        #[arg(long)]
        show_text: bool,
    },
    /// The old path (docs/share.md): POST /v1/share, then poll. The "before" number.
    Share {
        /// e.g. https://getsecretary.tech
        #[arg(long)]
        url: String,
        /// A file holding the Secretary device token (Bearer; never printed).
        #[arg(long)]
        token_file: PathBuf,
        /// The recording to upload (s16 or IMA-ADPCM mono WAV).
        #[arg(long)]
        wav: PathBuf,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// One JSON object per turn, plus a JSON summary.
        #[arg(long)]
        json: bool,
        /// Print the reply text (default: character counts only).
        #[arg(long)]
        show_text: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Purpose {
    Command,
    Note,
}

impl Purpose {
    fn as_str(&self) -> &'static str {
        match self {
            Purpose::Command => "command",
            Purpose::Note => "note",
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum SendCodec {
    Pcm16,
    ImaAdpcm,
}

impl SendCodec {
    fn as_str(&self) -> &'static str {
        match self {
            SendCodec::Pcm16 => "pcm16",
            SendCodec::ImaAdpcm => "ima-adpcm",
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Thread {
    Voice,
    Main,
}

impl Thread {
    fn as_str(&self) -> &'static str {
        match self {
            Thread::Voice => "voice",
            Thread::Main => "main",
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Channel {
    Web,
    Ios,
}

impl Channel {
    fn as_str(&self) -> &'static str {
        match self {
            Channel::Web => "web",
            Channel::Ios => "ios",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ws {
            url,
            token_file,
            wav: wav_path,
            purpose,
            codec,
            text,
            no_speak,
            thread,
            channel,
            source,
            lang,
            frame_ms,
            no_realtime,
            repeat,
            pause_s,
            out_audio,
            json,
            show_text,
        } => run_ws(WsArgs {
            url,
            token_file,
            wav_path,
            purpose,
            codec,
            text,
            no_speak,
            thread,
            channel,
            source,
            lang,
            frame_ms,
            no_realtime,
            repeat,
            pause_s,
            out_audio,
            json,
            show_text,
        })
        .await,
        Cmd::Share {
            url,
            token_file,
            wav: wav_path,
            lang,
            source,
            repeat,
            json,
            show_text,
        } => run_share(ShareArgs {
            url,
            token_file,
            wav_path,
            lang,
            source,
            repeat,
            json,
            show_text,
        })
        .await,
    }
}

#[derive(Debug)]
struct WsArgs {
    url: String,
    token_file: std::path::PathBuf,
    wav_path: std::path::PathBuf,
    purpose: Purpose,
    codec: SendCodec,
    text: bool,
    no_speak: bool,
    thread: Option<Thread>,
    channel: Option<Channel>,
    source: Option<String>,
    lang: Option<String>,
    frame_ms: u64,
    no_realtime: bool,
    repeat: u32,
    pause_s: u64,
    out_audio: Option<std::path::PathBuf>,
    json: bool,
    show_text: bool,
}

#[derive(Debug)]
struct ShareArgs {
    url: String,
    token_file: std::path::PathBuf,
    wav_path: std::path::PathBuf,
    lang: Option<String>,
    source: Option<String>,
    repeat: u32,
    json: bool,
    show_text: bool,
}

async fn run_ws(a: WsArgs) -> Result<()> {
    if a.frame_ms < 1 || a.frame_ms > 256 {
        bail!("--frame-ms must be 1..=256 (frames are at most 8 KiB)");
    }
    let token = read_token_file(&a.token_file)?;
    let buf = std::fs::read(&a.wav_path)
        .with_context(|| format!("reading {}", a.wav_path.display()))?;
    let wav = wav::parse(&buf).with_context(|| format!("parsing {}", a.wav_path.display()))?;

    // Prepare the frames to send.
    let (frames, speech_ms, adpcm_rate, adpcm_block, frame_bytes) = match a.codec {
        SendCodec::Pcm16 => {
            let samples = if wav.codec == wav::Codec::Pcm16 {
                wav.pcm.clone()
            } else {
                ima::decode(&wav.data, wav.block)
            };
            let resampled = resample::linear(&samples, wav.rate, 16000);
            let frame_bytes = pace::pcm16_frame_bytes(a.frame_ms);
            let mut bytes = Vec::with_capacity(resampled.len() * 2);
            for s in &resampled {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let frames = cut_frames(&bytes, frame_bytes);
            let speech_ms = resampled.len() as u64 * 1000 / 16000;
            (frames, speech_ms, None, None, frame_bytes)
        }
        SendCodec::ImaAdpcm => {
            if wav.codec != wav::Codec::ImaAdpcm {
                bail!("--codec ima-adpcm needs an IMA-ADPCM WAV input");
            }
            let frame_bytes = pace::adpcm_frame_bytes(a.frame_ms, wav.rate, wav.block);
            let frames = cut_frames(&wav.data, frame_bytes);
            (frames, wav.duration_ms(), Some(wav.rate), Some(wav.block), frame_bytes)
        }
    };
    if frame_bytes > 8192 {
        bail!("frame of {frame_bytes} bytes exceeds the 8 KiB frame cap");
    }

    let cfg = ws::WsConfig {
        purpose: a.purpose.as_str().to_string(),
        codec: a.codec.as_str().to_string(),
        adpcm_rate,
        adpcm_block,
        lang: a.lang.clone(),
        thread: a.thread.map(|t| t.as_str().to_string()),
        channel: a.channel.map(|c| c.as_str().to_string()),
        source: a.source.clone(),
        text: a.text,
        no_speak: a.no_speak,
        frame_ms: a.frame_ms,
        realtime: !a.no_realtime,
        frames,
        speech_ms,
    };

    let (connect_ms, mut stream) = ws::connect(&a.url, &token)
        .await
        .with_context(|| format!("connecting to {}", a.url))?;

    let mut turns: Vec<turn::TurnResult> = Vec::new();
    let mut last_audio: Option<(Vec<u8>, u32)> = None;
    for i in 1..=a.repeat {
        let (mut r, fatal) = ws::turn(&mut stream, &cfg).await?;
        // The socket is reused: only the first turn pays the connect cost.
        r.connect_ms = if i == 1 { Some(connect_ms) } else { None };
        print_turn(i as usize, a.repeat as usize, &r, a.json, a.show_text);
        turns.push(r.clone());
        if !r.audio.is_empty() {
            last_audio = Some((r.audio.clone(), r.audio_rate));
        }
        if fatal {
            eprintln!("turn {i} failed at the socket level; stopping the repeats");
            break;
        }
        if i < a.repeat {
            tokio::time::sleep(Duration::from_secs(a.pause_s)).await;
        }
    }

    if let Some(path) = &a.out_audio {
        match last_audio {
            Some((audio, rate)) => {
                if audio.len() % 2 != 0 {
                    bail!("reply audio has an odd number of bytes");
                }
                let pcm: Vec<i16> = audio
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c))
                    .collect();
                std::fs::write(path, wav::write_wav(&pcm, rate))
                    .with_context(|| format!("writing {}", path.display()))?;
                eprintln!("wrote {} ({} ms at {rate} Hz)", path.display(), pcm.len() * 1000 / rate as usize);
            }
            None => eprintln!("no audio received; not writing {}", path.display()),
        }
    }

    print_summary(&turns, a.json);
    Ok(())
}

async fn run_share(a: ShareArgs) -> Result<()> {
    let token = read_token_file(&a.token_file)?;
    let buf = std::fs::read(&a.wav_path)
        .with_context(|| format!("reading {}", a.wav_path.display()))?;
    let wav = wav::parse(&buf).with_context(|| format!("parsing {}", a.wav_path.display()))?;

    let cfg = share::ShareConfig {
        base_url: a.url.clone(),
        token,
        wav: buf,
        duration_ms: wav.duration_ms(),
        lang: a.lang.clone(),
        source: a.source.clone(),
    };

    let mut turns: Vec<turn::TurnResult> = Vec::new();
    for i in 1..=a.repeat {
        let r = share::run(&cfg).await?;
        print_turn(i as usize, a.repeat as usize, &r, a.json, a.show_text);
        turns.push(r);
    }
    print_summary(&turns, a.json);
    Ok(())
}

/// A file holding a Secretary device token. It is never printed.
fn read_token_file(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading token file {}", path.display()))?;
    let token = raw.trim();
    if token.is_empty() {
        bail!("token file {} is empty", path.display());
    }
    Ok(token.to_string())
}

/// Cut `total` bytes into frames of at most `frame_bytes` (the last may be shorter).
fn cut_frames(total: &[u8], frame_bytes: usize) -> Vec<Message> {
    (0..total.len())
        .step_by(frame_bytes)
        .map(|off| Message::Binary(total[off..(off + frame_bytes).min(total.len())].to_vec().into()))
        .collect()
}

fn print_turn(i: usize, n: usize, r: &turn::TurnResult, json: bool, show_text: bool) {
    if json {
        println!("{}", r.json());
        return;
    }
    let mut s = format!("turn {i}/{n}: {}", r.mode);
    if let Some(p) = &r.purpose {
        s.push_str(&format!(" {p}"));
    }
    if let Some(c) = &r.codec {
        s.push_str(&format!(" {c}"));
    }
    if let Some(cm) = r.connect_ms {
        s.push_str(&format!(" | connect {cm}ms"));
    }
    s.push_str(&format!(" | speech {}ms", r.speech_ms));
    for (k, v) in &r.t {
        s.push_str(&format!(" | {k} {v}ms"));
    }
    if let Some(sm) = &r.server_ms {
        s.push_str(&format!(" | server_ms {sm}"));
    }
    s.push_str(&format!(
        " | up {}B | down {}B",
        r.up_bytes, r.down_bytes
    ));
    s.push_str(&format!(
        " | transcript {}ch | reply {}ch",
        r.transcript_chars, r.reply_chars
    ));
    if let Some(sec) = r.server_section10 {
        s.push_str(&format!(" | section10 {sec}"));
    }
    if let Some(e) = &r.error {
        s.push_str(&format!(" | error: {e}"));
    }
    if show_text {
        if let Some(t) = &r.transcript {
            s.push_str(&format!(" | transcript \"{t}\""));
        }
        if let Some(reply) = &r.reply {
            s.push_str(&format!(" | reply \"{reply}\""));
        }
    }
    println!("{s}");
}

fn print_summary(turns: &[turn::TurnResult], json: bool) {
    let s = turn::summary(turns);
    if json {
        let mut t = serde_json::Map::new();
        for (k, (min, median, max)) in &s {
            t.insert(k.clone(), json!({ "min": min, "median": median, "max": max }));
        }
        println!("{}", json!({ "summary": { "turns": turns.len(), "t": t } }));
    } else if s.is_empty() {
        println!("summary: no turns completed");
    } else {
        println!("summary over {} turn(s):", turns.len());
        for (k, (min, median, max)) in &s {
            println!("  {k:<15} min {min:>5}  median {median:>5}  max {max:>5}  (ms)");
        }
    }
}
