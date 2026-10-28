//! The ZCode pass: GLM through Z.ai's vision MCP transcribes and describes the video.
//!
//! The vision server (`zai-mcp-server`, tool `analyze_video`) takes MP4 files up to 8 MiB,
//! so the video is first re-encoded small: one file when the bitrate budget allows, else
//! chunks of a few minutes ([`plan`]). ZCode (`node zcode.cjs`, the owner's Z.ai Coding
//! Plan) is then asked to run the tool on every file and write `zcode-result.json`
//! ([`prompt`]); [`parse_result`] reads it back tolerantly. The node process runs in the
//! capped scope of [`thermal`], so it stays out of lucina-rs.service's memory cap.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::Value;

use super::{thermal, Seg, VideoConfig, VideoMeta, ZcodeResult, ZcodeVisual};

/// The most one file may weigh (the tool refuses 8 MiB; base64 adds a third on the wire).
const BUDGET_BYTES: u64 = 7_500_000;
/// Share of the budget the bitrate plan aims at (the encoder overshoots a little).
const TARGET_SHARE: f64 = 0.9;
const MIN_VIDEO_KBPS: f64 = 200.0;
const MAX_VIDEO_KBPS: f64 = 1500.0;
const AUDIO_KBPS_LOW: f64 = 48.0;
const AUDIO_KBPS_HIGH: f64 = 64.0;
/// The Raphael iGPU.
const VAAPI_DEVICE: &str = "/dev/dri/renderD129";
const ENCODE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Re-encodes allowed when a file comes out over the budget.
const REENCODE_TRIES: usize = 3;

// ---------------------------------------------------------------- planning

/// A piece of the video: where it starts and how long it is (seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chunk {
    pub start: f64,
    pub duration: f64,
}

/// How to encode the video for the vision tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub chunks: Vec<Chunk>,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    /// The long side of the picture is at most this many pixels.
    pub long_side: u32,
}

/// Bitrates and chunks for a video of `duration` seconds: one file when the video gets at
/// least 200 kbps within the budget, else equal chunks that each do.
pub fn plan(duration: f64) -> Plan {
    let duration = if duration.is_finite() {
        duration.max(1.0)
    } else {
        1.0
    };
    let bits = BUDGET_BYTES as f64 * 8.0 * TARGET_SHARE;
    let total_kbps = bits / duration / 1000.0;
    let audio = if total_kbps >= 600.0 {
        AUDIO_KBPS_HIGH
    } else {
        AUDIO_KBPS_LOW
    };
    let single = total_kbps - audio;
    let (chunks, video) = if single >= MIN_VIDEO_KBPS {
        (
            vec![Chunk {
                start: 0.0,
                duration,
            }],
            single.min(MAX_VIDEO_KBPS),
        )
    } else {
        let max_len = bits / 1000.0 / (MIN_VIDEO_KBPS + AUDIO_KBPS_LOW);
        let n = (duration / max_len).ceil().max(1.0) as usize;
        let len = duration / n as f64;
        let video = (bits / 1000.0 / len - AUDIO_KBPS_LOW).max(MIN_VIDEO_KBPS);
        let chunks = (0..n)
            .map(|i| Chunk {
                start: len * i as f64,
                duration: len,
            })
            .collect();
        (chunks, video.min(MAX_VIDEO_KBPS))
    };
    let audio = if chunks.len() > 1 {
        AUDIO_KBPS_LOW
    } else {
        audio
    };
    Plan {
        chunks,
        video_kbps: video.floor() as u32,
        audio_kbps: audio as u32,
        long_side: if video >= 300.0 { 720 } else { 480 },
    }
}

/// The picture size that fits `long_side`, never larger than the source, both even.
pub fn fit_dims(w: u32, h: u32, long_side: u32) -> (u32, u32) {
    let long = w.max(h).max(1);
    let scale = (long_side as f64 / long as f64).min(1.0);
    let even = |v: u32| (((v as f64 * scale).round() as u32) & !1).max(2);
    (even(w), even(h))
}

// ---------------------------------------------------------------- encoding

async fn probe_dims(cfg: &VideoConfig, video: &Path) -> anyhow::Result<(u32, u32)> {
    let mut cmd = thermal::command(&cfg.thermal, &cfg.ffprobe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "csv=p=0:s=x",
    ])
    .arg(video);
    let out = thermal::output(cmd, Duration::from_secs(120))
        .await
        .context("ffprobe")?;
    parse_dims(&out).ok_or_else(|| anyhow!("ffprobe gave no picture size: {}", out.trim()))
}

fn parse_dims(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.lines().next()?.trim().split_once('x')?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// The ffmpeg arguments for one output file.
fn encode_args(
    video: &Path,
    out: &Path,
    chunk: Chunk,
    dims: (u32, u32),
    video_kbps: u32,
    audio_kbps: u32,
    vaapi: bool,
) -> Vec<String> {
    let mut a: Vec<String> = ["-nostdin", "-v", "error", "-y"].map(String::from).into();
    if vaapi {
        a.extend(["-vaapi_device".into(), VAAPI_DEVICE.into()]);
    }
    a.extend([
        "-ss".into(),
        format!("{:.3}", chunk.start),
        "-t".into(),
        format!("{:.3}", chunk.duration),
        "-i".into(),
        video.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
    ]);
    let (w, h) = dims;
    if vaapi {
        a.extend([
            "-vf".into(),
            format!("format=nv12,hwupload,scale_vaapi=w={w}:h={h}"),
            "-c:v".into(),
            "h264_vaapi".into(),
        ]);
    } else {
        a.extend([
            "-vf".into(),
            format!("scale={w}:{h},format=yuv420p"),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
        ]);
    }
    a.extend([
        "-b:v".into(),
        format!("{video_kbps}k"),
        "-maxrate".into(),
        format!("{}k", video_kbps + video_kbps / 4),
        "-bufsize".into(),
        format!("{}k", video_kbps * 2),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        format!("{audio_kbps}k"),
        "-ac".into(),
        "1".into(),
        "-movflags".into(),
        "+faststart".into(),
        out.display().to_string(),
    ]);
    a
}

async fn run_ffmpeg(cfg: &VideoConfig, args: &[String]) -> anyhow::Result<()> {
    let mut cmd = thermal::command(&cfg.thermal, &cfg.ffmpeg);
    cmd.args(args);
    thermal::output(cmd, ENCODE_TIMEOUT).await.map(|_| ())
}

/// Encodes `chunk` of `video` into `out` and returns its size. VAAPI first, libx264 when
/// that fails; a file over the budget is encoded again with a lower bitrate.
#[allow(clippy::too_many_arguments)]
async fn encode_chunk(
    cfg: &VideoConfig,
    video: &Path,
    out: &Path,
    chunk: Chunk,
    dims: (u32, u32),
    mut video_kbps: u32,
    audio_kbps: u32,
    vaapi: &mut bool,
) -> anyhow::Result<u64> {
    let mut size = 0;
    for _ in 0..REENCODE_TRIES {
        if *vaapi {
            let args = encode_args(video, out, chunk, dims, video_kbps, audio_kbps, true);
            if let Err(e) = run_ffmpeg(cfg, &args).await {
                tracing::warn!(error = %format!("{e:#}"), "VAAPI encode failed: using libx264");
                *vaapi = false;
            }
        }
        if !*vaapi {
            let args = encode_args(video, out, chunk, dims, video_kbps, audio_kbps, false);
            run_ffmpeg(cfg, &args).await.context("ffmpeg (libx264)")?;
        }
        size = tokio::fs::metadata(out)
            .await
            .context("encoded file")?
            .len();
        if size <= BUDGET_BYTES {
            return Ok(size);
        }
        video_kbps = (video_kbps as f64 * (BUDGET_BYTES as f64 / size as f64) * 0.85) as u32;
        if video_kbps < 50 {
            break;
        }
    }
    bail!(
        "could not get {} under {} bytes (last size {size})",
        out.display(),
        BUDGET_BYTES
    )
}

/// The small copies of the video in `dir`, with each one's start offset.
async fn encode_all(
    cfg: &VideoConfig,
    meta: &VideoMeta,
    video: &Path,
    dir: &Path,
) -> anyhow::Result<Vec<(PathBuf, f64)>> {
    let plan = plan(meta.duration);
    let (w, h) = probe_dims(cfg, video).await?;
    let dims = fit_dims(w, h, plan.long_side);
    let mut vaapi = Path::new(VAAPI_DEVICE).exists();
    let many = plan.chunks.len() > 1;
    let mut files = Vec::new();
    for (i, chunk) in plan.chunks.iter().enumerate() {
        thermal::wait_cool(&cfg.thermal).await;
        let name = if many {
            format!("chunk-{:03}.mp4", i + 1)
        } else {
            "video.mp4".to_owned()
        };
        let out = dir.join(name);
        let size = encode_chunk(
            cfg,
            video,
            &out,
            *chunk,
            dims,
            plan.video_kbps,
            plan.audio_kbps,
            &mut vaapi,
        )
        .await?;
        tracing::info!(file = %out.display(), size, start = chunk.start, "ZCode copy encoded");
        files.push((out, chunk.start));
    }
    Ok(files)
}

// ---------------------------------------------------------------- the prompt

/// What ZCode is told. `files` are absolute paths with their start offsets (seconds);
/// `result_file` is the JSON file to write; `part` is (this group, groups) when the video
/// is split over several ZCode runs.
pub fn prompt(
    meta: &VideoMeta,
    files: &[(PathBuf, f64)],
    result_file: &str,
    part: Option<(usize, usize)>,
) -> String {
    let mut p = String::new();
    p.push_str(
        "You are the video-analysis step of a report pipeline. Use ONLY the `analyze_video` tool of the \
         `zai-mcp-server` MCP server (Z.ai's vision server; it may be listed as \
         mcp__zai-mcp-server__analyze_video) with its `video_source` set to the absolute file path. \
         Do not use any other MCP server and do not edit any file except the result file named below.\n\n",
    );
    p.push_str("Video context (to spell names and jargon right):\n");
    p.push_str(&format!("- Title: {}\n", one_line(&meta.title, 300)));
    if !meta.channel.trim().is_empty() {
        p.push_str(&format!("- Channel: {}\n", one_line(&meta.channel, 120)));
    }
    p.push_str(&format!("- Length: {:.0} seconds\n", meta.duration));
    if let Some(l) = meta.language.as_deref().filter(|l| !l.is_empty()) {
        p.push_str(&format!("- Language hint: {l}\n"));
    }
    let desc = one_line(&meta.description, 1500);
    if !desc.is_empty() {
        p.push_str(&format!("- Description: {desc}\n"));
    }
    if let Some((g, n)) = part {
        p.push_str(&format!(
            "\nThis is part {g} of {n}: other runs handle the other parts of the video, so cover only the files below.\n"
        ));
    }
    p.push_str("\nVideo files, in order (each is a piece of the same video; the offset is where it starts in the whole video):\n");
    for (i, (path, start)) in files.iter().enumerate() {
        p.push_str(&format!(
            "{}. {} (offset {:.0} seconds)\n",
            i + 1,
            path.display(),
            start
        ));
    }
    p.push_str(
        "\nFor each file in order, call `analyze_video` once (again for a part you need to recheck) with a \
         detailed prompt that asks for: (a) every spoken word transcribed verbatim in the language spoken, \
         with a timestamp in seconds from the start of THAT file; (b) the technical terms, names, jargon, \
         numbers, code and commands exactly as spoken or shown; (c) what is on screen, a short description \
         every few seconds or at each change, with any text visible on screen copied exactly; (d) a statement \
         of whether the audio could be heard at all. Never invent speech: if the tool cannot hear audio, leave \
         the transcript empty and say so in the summary.\n\n",
    );
    p.push_str(
        &"Then write the file `@RESULT@` in the current directory: one JSON object and nothing else:\n\
         {\"transcript\":[{\"start\":0.0,\"end\":4.2,\"text\":\"...\"}],\
         \"visual\":[{\"t\":0.0,\"description\":\"...\",\"on_screen_text\":\"...\"}],\
         \"terms\":[\"...\"],\"summary\":\"...\"}\n\
         Rules: all times are numbers in seconds from the start of the WHOLE video, so add each file's offset \
         to the times the tool gave for that file; transcript segments are sentence-sized, in time order, with \
         the words verbatim in the original language (no translation, no cleaning up); `terms` lists the \
         technical terms, names and code identifiers exactly as spoken or shown; `summary` is two to four \
         sentences. After writing the file, reply with the word done.\n"
            .replace("@RESULT@", result_file),
    );
    p
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut cut: String = flat.chars().take(max).collect();
        cut.push('…');
        cut
    }
}

// ---------------------------------------------------------------- parsing

/// A JSON object in `text`: the whole text, a fenced block, or the outermost braces.
fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    if let Some(start) = t.find("```") {
        let rest = &t[start + 3..];
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("JSON"))
            .unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(rest[..end].trim()) {
                return Some(v);
            }
        }
    }
    let (a, b) = (t.find('{')?, t.rfind('}')?);
    if a < b {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&t[a..=b]) {
            return Some(v);
        }
    }
    None
}

fn has_result_keys(v: &Value) -> bool {
    ["transcript", "visual", "terms", "summary"]
        .iter()
        .any(|k| v.get(k).is_some())
}

/// Seconds from a number, a numeric string, or `mm:ss` / `h:mm:ss`.
fn seconds(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let s = s.trim().trim_end_matches('s').trim();
            if s.contains(':') {
                s.split(':').try_fold(0.0, |acc, part| {
                    part.trim().parse::<f64>().ok().map(|p| acc * 60.0 + p)
                })
            } else {
                s.parse().ok()
            }
        }
        _ => None,
    }
    .filter(|t| t.is_finite() && *t >= 0.0)
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| text_of(Some(x)))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("; "),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Reads the JSON ZCode wrote into a [`ZcodeResult`] (no session). Segments are cleaned and
/// sorted; missing or odd fields are tolerated.
pub fn parse_result(text: &str) -> Option<ZcodeResult> {
    let v = extract_json(text)?;
    let v = if has_result_keys(&v) {
        v
    } else {
        v.get("result").filter(|r| has_result_keys(r))?.clone()
    };
    let mut transcript: Vec<Seg> = Vec::new();
    for s in v
        .get("transcript")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = text_of(s.get("text"));
        let Some(start) = s.get("start").and_then(seconds) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let end = s.get("end").and_then(seconds).unwrap_or(start).max(start);
        transcript.push(Seg { start, end, text });
    }
    transcript.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut visual: Vec<ZcodeVisual> = Vec::new();
    for s in v
        .get("visual")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(t) = s.get("t").or_else(|| s.get("start")).and_then(seconds) else {
            continue;
        };
        let description = text_of(s.get("description"));
        let on_screen_text = text_of(s.get("on_screen_text").or_else(|| s.get("text")));
        if description.is_empty() && on_screen_text.is_empty() {
            continue;
        }
        visual.push(ZcodeVisual {
            t,
            description,
            on_screen_text,
        });
    }
    visual.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut terms: Vec<String> = Vec::new();
    for t in v
        .get("terms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let t = text_of(Some(t));
        if !t.is_empty() && !terms.contains(&t) {
            terms.push(t);
        }
    }
    Some(ZcodeResult {
        transcript,
        visual,
        terms,
        summary: text_of(v.get("summary")),
        session: None,
    })
}

/// Every string inside a JSON value (the result text may sit under any key).
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// The result when ZCode did not write the file: from the stdout JSON itself or from any
/// JSON object inside its strings.
pub fn result_from_stdout(stdout: &str) -> Option<ZcodeResult> {
    if let Some(v) = extract_json(stdout) {
        if has_result_keys(&v) {
            return parse_result(stdout);
        }
        let mut all = Vec::new();
        strings(&v, &mut all);
        all.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for s in all {
            if let Some(r) = extract_json(&s)
                .filter(has_result_keys)
                .and_then(|_| parse_result(&s))
            {
                return Some(r);
            }
        }
    }
    None
}

/// The `sess_…` id in ZCode's stdout JSON.
pub fn session_id(stdout: &str) -> Option<String> {
    let re = regex::Regex::new(r"sess_[A-Za-z0-9_-]+").ok()?;
    re.find(stdout).map(|m| m.as_str().to_owned())
}

// ---------------------------------------------------------------- errors

/// A clear reason for a failed ZCode run, from its stderr (and stdout), or `None` when
/// nothing recognisable is there.
pub fn classify(stderr: &str, stdout: &str) -> Option<String> {
    let both = format!("{stderr}\n{stdout}");
    if both.contains("[1308]") || both.contains("Usage limit reached") {
        let reset = regex::Regex::new(r"reset at ([^\]\n]+)")
            .ok()
            .and_then(|re| re.captures(&both).map(|c| c[1].trim().to_owned()));
        return Some(match reset {
            Some(r) => {
                format!(
                "Z.ai usage limit reached (5-hour quota); it resets at {r} (Z.ai time, UTC+8{})",
                utc_of(&r).map(|u| format!(" = {u} UTC")).unwrap_or_default()
            )
            }
            None => "Z.ai usage limit reached (5-hour quota)".to_owned(),
        });
    }
    if both.contains("project_token_login_failed")
        || both.contains("Z.ai user information is invalid")
        || both.contains("Select a model")
    {
        return Some("ZCode sign-in expired; re-login needed".to_owned());
    }
    None
}

/// `2026-10-28 14:00:00` in Z.ai time → the same moment in UTC.
fn utc_of(s: &str) -> Option<String> {
    let t = chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    Some(
        (t - chrono::Duration::hours(8))
            .format("%Y-%m-%d %H:%M")
            .to_string(),
    )
}

fn tail(s: &str, lines: usize, max: usize) -> String {
    let v: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let skip = v.len().saturating_sub(lines);
    let joined = v[skip..].join(" | ");
    let n = joined.chars().count();
    if n > max {
        joined.chars().skip(n - max).collect()
    } else {
        joined
    }
}

// ---------------------------------------------------------------- the run

/// Chunks per ZCode run: more files per run make the run long and its result file big.
pub const GROUP_SIZE: usize = 6;
/// All ZCode runs of one video together should not take longer than this.
const TOTAL_BUDGET: Duration = Duration::from_secs(2 * 3600);
const MIN_GROUP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Index ranges of the groups of at most [`GROUP_SIZE`] chunks, in order.
pub fn groups(chunks: usize) -> Vec<std::ops::Range<usize>> {
    (0..chunks)
        .step_by(GROUP_SIZE)
        .map(|a| a..(a + GROUP_SIZE).min(chunks))
        .collect()
}

/// The time limit of one run: `limit` (cfg.zcode_timeout) when there is one run, else the
/// total budget shared by the runs, never more than `limit` and never under five minutes.
pub fn group_timeout(limit: Duration, groups: usize) -> Duration {
    if groups <= 1 {
        return limit;
    }
    (TOTAL_BUDGET / groups as u32)
        .max(MIN_GROUP_TIMEOUT)
        .min(limit)
}

/// The file names of one run: plain for a single run, numbered (`zcode-result-2.json`) for
/// several. `group` counts from 1.
fn file_names(group: usize, total: usize) -> (String, String, String) {
    if total <= 1 {
        (
            "zcode-result.json".into(),
            "zcode.out.json".into(),
            "zcode.err".into(),
        )
    } else {
        (
            format!("zcode-result-{group}.json"),
            format!("zcode.out-{group}.json"),
            format!("zcode.err-{group}"),
        )
    }
}

/// The groups' results as one: transcript and visual notes in time order (offsets are
/// already applied), terms without repeats, the summaries one after another, the first
/// session.
pub fn merge_results(parts: Vec<ZcodeResult>) -> ZcodeResult {
    let mut all = ZcodeResult::default();
    let mut summaries: Vec<String> = Vec::new();
    for p in parts {
        all.transcript.extend(p.transcript);
        all.visual.extend(p.visual);
        for t in p.terms {
            if !all.terms.contains(&t) {
                all.terms.push(t);
            }
        }
        if !p.summary.trim().is_empty() {
            summaries.push(p.summary.trim().to_owned());
        }
        if all.session.is_none() {
            all.session = p.session;
        }
    }
    all.transcript.sort_by(|a, b| a.start.total_cmp(&b.start));
    all.visual.sort_by(|a, b| a.t.total_cmp(&b.t));
    all.summary = summaries.join(" ");
    all
}

/// Kills a process group (ZCode starts its MCP servers as children).
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Runs ZCode in `cwd`, stdout and stderr to the named files, and returns its exit code
/// (None = killed by a signal).
async fn run_zcode(
    cfg: &VideoConfig,
    cwd: &Path,
    prompt: &str,
    out_name: &str,
    err_name: &str,
    timeout: Duration,
) -> anyhow::Result<Option<i32>> {
    let out = std::fs::File::create(cwd.join(out_name)).context("ZCode stdout file")?;
    let err = std::fs::File::create(cwd.join(err_name)).context("ZCode stderr file")?;
    let mut cmd = thermal::command(&cfg.thermal, &cfg.node);
    cmd.arg("zcode.cjs")
        .arg("--cwd")
        .arg(cwd)
        .args(["--json", "--no-color", "-p"])
        .arg(prompt)
        .current_dir(&cfg.zcode_dir)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .process_group(0);
    if let Some(dir) = cfg.node.parent().filter(|d| !d.as_os_str().is_empty()) {
        // npx (the vision MCP server) lives next to node.
        let old = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{old}", dir.display()));
    }
    let mut child = cmd.spawn().context("starting ZCode (node zcode.cjs)")?;
    let pid = child.id();
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => Ok(status.context("waiting for ZCode")?.code()),
        Err(_) => {
            if let Some(pid) = pid {
                kill_group(pid);
            }
            let _ = child.kill().await;
            bail!("ZCode timed out after {} minutes", timeout.as_secs() / 60)
        }
    }
}

/// Everything ZCode wrote that is worth keeping (not the video copies).
fn copy_outputs(cwd: &Path, dest: &Path) {
    let Ok(dir) = std::fs::read_dir(cwd) else {
        return;
    };
    if std::fs::create_dir_all(dest).is_err() {
        return;
    }
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("zcode")
            && e.metadata()
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
        {
            let _ = std::fs::copy(e.path(), dest.join(&name));
        }
    }
}

/// A failed ZCode run. `fatal` means the next runs would fail the same way (quota, sign-in).
struct GroupFailure {
    fatal: bool,
    error: anyhow::Error,
}

/// One ZCode run over `files`: its result, or why there is none.
async fn run_group(
    cfg: &VideoConfig,
    meta: &VideoMeta,
    cwd: &Path,
    files: &[(PathBuf, f64)],
    group: usize,
    total: usize,
    timeout: Duration,
) -> Result<ZcodeResult, GroupFailure> {
    let (result_name, out_name, err_name) = file_names(group, total);
    let part = (total > 1).then_some((group, total));
    let text = prompt(meta, files, &result_name, part);
    let code = run_zcode(cfg, cwd, &text, &out_name, &err_name, timeout)
        .await
        .map_err(|error| GroupFailure {
            fatal: false,
            error,
        })?;
    let stdout = tokio::fs::read_to_string(cwd.join(&out_name))
        .await
        .unwrap_or_default();
    let stderr = tokio::fs::read_to_string(cwd.join(&err_name))
        .await
        .unwrap_or_default();
    let written = tokio::fs::read_to_string(cwd.join(&result_name)).await.ok();
    let parsed = written
        .as_deref()
        .and_then(parse_result)
        .or_else(|| result_from_stdout(&stdout));
    if let Some(mut r) = parsed {
        r.session = session_id(&stdout);
        return Ok(r);
    }
    if let Some(reason) = classify(&stderr, &stdout) {
        return Err(GroupFailure {
            fatal: true,
            error: anyhow!("ZCode: {reason}"),
        });
    }
    let error = if code != Some(0) {
        anyhow!("ZCode failed (exit {code:?}): {}", tail(&stderr, 4, 500))
    } else {
        anyhow!(
            "ZCode finished without a result file ({})",
            if written.is_some() {
                format!("{result_name} is not valid")
            } else {
                "none written".to_owned()
            }
        )
    };
    Err(GroupFailure {
        fatal: false,
        error,
    })
}

/// The ZCode pass for `video`: small copies in `work/zcode`, ZCode runs over groups of at
/// most [`GROUP_SIZE`] of them, one after another; the outputs are copied to
/// `folder/zcode/`. Groups that fail are logged and left out; it is an error only when no
/// group gave a result.
pub async fn run(
    cfg: &VideoConfig,
    meta: &VideoMeta,
    video: &Path,
    work: &Path,
    folder: &Path,
) -> anyhow::Result<ZcodeResult> {
    let cwd = work.join("zcode");
    tokio::fs::create_dir_all(&cwd)
        .await
        .context("work/zcode")?;
    let cwd = std::path::absolute(&cwd).unwrap_or(cwd);
    if let Ok(mut dir) = tokio::fs::read_dir(&cwd).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            if e.file_name().to_string_lossy().starts_with("zcode") {
                let _ = tokio::fs::remove_file(e.path()).await;
            }
        }
    }
    let files = encode_all(cfg, meta, video, &cwd)
        .await
        .context("preparing the video for ZCode")?;
    let ranges = groups(files.len());
    let timeout = group_timeout(cfg.zcode_timeout, ranges.len());
    let total = ranges.len();
    let mut done: Vec<ZcodeResult> = Vec::new();
    let mut failed: Vec<(usize, String)> = Vec::new();
    for (i, range) in ranges.into_iter().enumerate() {
        let group = i + 1;
        match run_group(cfg, meta, &cwd, &files[range], group, total, timeout).await {
            Ok(r) => done.push(r),
            Err(f) => {
                tracing::warn!(group, total, error = %format!("{:#}", f.error), "ZCode group failed");
                failed.push((group, format!("{:#}", f.error)));
                if f.fatal {
                    break;
                }
            }
        }
    }
    copy_outputs(&cwd, &folder.join("zcode"));
    if done.is_empty() {
        let first = failed
            .first()
            .map(|(_, e)| e.as_str())
            .unwrap_or("no ZCode run");
        if total > 1 {
            bail!("{first} (no part of {total} gave a result)");
        }
        bail!("{first}");
    }
    if !failed.is_empty() {
        let list: Vec<String> = failed
            .iter()
            .map(|(g, e)| format!("part {g}: {e}"))
            .collect();
        tracing::warn!(
            ok = done.len(),
            total,
            "ZCode result is partial; failed: {}",
            list.join("; ")
        );
    }
    Ok(merge_results(done))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> VideoMeta {
        VideoMeta {
            id: "FBs--2nSPLQ".into(),
            title: "I challenged Rust Developers".into(),
            channel: "Arjay McCandless".into(),
            duration: 98.0,
            description: "#coding\n#programming".into(),
            ..Default::default()
        }
    }

    fn total_bytes(p: &Plan, c: &Chunk) -> f64 {
        (p.video_kbps + p.audio_kbps) as f64 * 1000.0 / 8.0 * c.duration
    }

    #[test]
    fn plans_a_short_as_one_file() {
        let p = plan(98.248);
        assert_eq!(p.chunks.len(), 1);
        assert_eq!(p.chunks[0].start, 0.0);
        assert_eq!(p.long_side, 720);
        assert_eq!(p.audio_kbps, 48);
        assert!((450..=560).contains(&p.video_kbps), "{p:?}");
        assert!(total_bytes(&p, &p.chunks[0]) <= BUDGET_BYTES as f64);
    }

    #[test]
    fn plans_a_very_short_clip_with_capped_bitrate() {
        let p = plan(10.0);
        assert_eq!(p.chunks.len(), 1);
        assert_eq!(p.video_kbps, 1500);
        assert_eq!(p.audio_kbps, 64);
    }

    #[test]
    fn plans_a_half_hour_talk_in_chunks() {
        let p = plan(1800.0);
        assert!(p.chunks.len() > 1, "{p:?}");
        assert_eq!(p.long_side, 480);
        assert!(p.video_kbps >= 200, "{p:?}");
        assert_eq!(p.chunks[0].start, 0.0);
        let mut end = 0.0;
        for c in &p.chunks {
            assert!((c.start - end).abs() < 1e-6, "chunks are contiguous");
            assert!(total_bytes(&p, c) <= BUDGET_BYTES as f64, "{c:?}");
            end = c.start + c.duration;
        }
        assert!((end - 1800.0).abs() < 1e-6);
        assert_eq!(p.chunks.len(), 9);
    }

    #[test]
    fn plans_three_hours() {
        let p = plan(3.0 * 3600.0);
        assert_eq!(p.chunks.len(), 50);
        assert!(p.video_kbps >= 200);
        let last = p.chunks.last().unwrap();
        assert!((last.start + last.duration - 10800.0).abs() < 1e-6);
        assert!(total_bytes(&p, last) <= BUDGET_BYTES as f64);
    }

    #[test]
    fn plan_survives_odd_durations() {
        assert_eq!(plan(0.0).chunks.len(), 1);
        assert_eq!(plan(f64::NAN).chunks.len(), 1);
    }

    #[test]
    fn fits_pictures() {
        assert_eq!(fit_dims(1080, 1920, 720), (404, 720));
        assert_eq!(fit_dims(1920, 1080, 480), (480, 270));
        assert_eq!(fit_dims(320, 240, 720), (320, 240), "never upscales");
        assert_eq!(parse_dims("1080x1920\n"), Some((1080, 1920)));
        assert_eq!(parse_dims(""), None);
    }

    #[test]
    fn encode_args_pick_the_encoder() {
        let c = Chunk {
            start: 217.5,
            duration: 200.0,
        };
        let v = encode_args(
            Path::new("/in.mkv"),
            Path::new("/o.mp4"),
            c,
            (406, 720),
            500,
            48,
            true,
        );
        let j = v.join(" ");
        assert!(
            j.contains("h264_vaapi")
                && j.contains("-vaapi_device")
                && j.contains("scale_vaapi=w=406:h=720")
        );
        assert!(j.contains("-ss 217.500 -t 200.000 -i /in.mkv") && j.contains("-b:a 48k -ac 1"));
        let x = encode_args(
            Path::new("/in.mkv"),
            Path::new("/o.mp4"),
            c,
            (406, 720),
            500,
            48,
            false,
        )
        .join(" ");
        assert!(x.contains("libx264 -preset veryfast") && !x.contains("vaapi"));
    }

    #[test]
    fn prompt_names_tool_files_offsets_and_context() {
        let files = vec![
            (PathBuf::from("/w/zcode/chunk-001.mp4"), 0.0),
            (PathBuf::from("/w/zcode/chunk-002.mp4"), 217.0),
        ];
        let p = prompt(&meta(), &files, "zcode-result-2.json", Some((2, 9)));
        assert!(p.contains("analyze_video") && p.contains("zai-mcp-server"));
        assert!(p.contains("1. /w/zcode/chunk-001.mp4 (offset 0 seconds)"));
        assert!(p.contains("2. /w/zcode/chunk-002.mp4 (offset 217 seconds)"));
        assert!(p.contains("Title: I challenged Rust Developers"));
        assert!(p.contains("Channel: Arjay McCandless"));
        assert!(p.contains("Description: #coding #programming"));
        assert!(p.contains("`zcode-result-2.json`") && p.contains("\"on_screen_text\""));
        assert!(p.contains("part 2 of 9"));
        assert!(!prompt(&meta(), &files, "zcode-result.json", None).contains("part 1 of"));
        assert!(p.contains("verbatim") && p.contains("Never invent speech"));
        assert!(p.contains("WHOLE video"));
    }

    #[test]
    fn prompt_truncates_a_long_description() {
        let mut m = meta();
        m.description = "word ".repeat(2000);
        let p = prompt(
            &m,
            &[(PathBuf::from("/a.mp4"), 0.0)],
            "zcode-result.json",
            None,
        );
        assert!(p.len() < 6000, "{}", p.len());
    }

    #[test]
    fn groups_chunks_by_six() {
        assert!(groups(0).is_empty());
        assert_eq!(groups(1), vec![0..1]);
        assert_eq!(groups(6), vec![0..6]);
        assert_eq!(groups(7), vec![0..6, 6..7]);
        assert_eq!(groups(9), vec![0..6, 6..9]);
        let g = groups(plan(3.0 * 3600.0).chunks.len());
        assert_eq!(g.len(), 9);
        assert_eq!(g.last().unwrap().end, 50);
        assert!(g.iter().all(|r| r.len() <= GROUP_SIZE));
        assert!(g.windows(2).all(|w| w[0].end == w[1].start));
    }

    #[test]
    fn group_timeouts_fit_the_budget() {
        let limit = Duration::from_secs(40 * 60);
        assert_eq!(
            group_timeout(limit, 1),
            limit,
            "one run gets the whole limit"
        );
        // 30 minutes: two runs, each up to the limit (80 minutes at most).
        assert_eq!(group_timeout(limit, 2), limit);
        // 3 hours: nine runs share two hours.
        let t = group_timeout(limit, 9);
        assert!(
            t * 9 <= Duration::from_secs(2 * 3600) && t >= Duration::from_secs(13 * 60),
            "{t:?}"
        );
        assert!(group_timeout(limit, 100) >= MIN_GROUP_TIMEOUT);
        assert_eq!(
            group_timeout(Duration::from_secs(60), 3),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn names_files_per_group() {
        assert_eq!(file_names(1, 1).0, "zcode-result.json");
        assert_eq!(
            file_names(2, 3),
            (
                "zcode-result-2.json".into(),
                "zcode.out-2.json".into(),
                "zcode.err-2".into()
            )
        );
    }

    #[test]
    fn merges_groups_in_time_order() {
        let seg = |s: f64, t: &str| Seg {
            start: s,
            end: s + 1.0,
            text: t.into(),
        };
        let a = ZcodeResult {
            transcript: vec![seg(0.0, "a"), seg(10.0, "b")],
            visual: vec![ZcodeVisual {
                t: 5.0,
                description: "x".into(),
                on_screen_text: String::new(),
            }],
            terms: vec!["Rust".into(), "SQLite".into()],
            summary: "first.".into(),
            session: Some("sess_1".into()),
        };
        let b = ZcodeResult {
            transcript: vec![seg(1300.0, "d"), seg(1290.0, "c")],
            visual: vec![ZcodeVisual {
                t: 1295.0,
                description: "y".into(),
                on_screen_text: "z".into(),
            }],
            terms: vec!["SQLite".into(), "io_uring".into()],
            summary: " second. ".into(),
            session: Some("sess_2".into()),
        };
        let m = merge_results(vec![a.clone(), b.clone()]);
        let texts: Vec<&str> = m.transcript.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "d"]);
        assert_eq!(
            m.visual.iter().map(|v| v.t).collect::<Vec<_>>(),
            [5.0, 1295.0]
        );
        assert_eq!(m.terms, ["Rust", "SQLite", "io_uring"]);
        assert_eq!(m.summary, "first. second.");
        assert_eq!(
            m.session.as_deref(),
            Some("sess_1"),
            "the first successful session"
        );
        assert_eq!(
            merge_results(vec![b, a]).transcript[0].text,
            "a",
            "sorted whatever the order"
        );
        assert_eq!(merge_results(vec![]), ZcodeResult::default());
    }

    #[test]
    fn parses_a_clean_result() {
        let r = parse_result(
            r#"{"transcript":[{"start":4.5,"end":9,"text":"b"},{"start":0,"end":4,"text":" a "}],
                "visual":[{"t":2,"description":"a terminal","on_screen_text":"cargo run"}],
                "terms":["io_uring","io_uring","SQLite"],"summary":"s"}"#,
        )
        .unwrap();
        assert_eq!(r.transcript[0].text, "a");
        assert_eq!(r.transcript[1].start, 4.5);
        assert_eq!(r.visual[0].on_screen_text, "cargo run");
        assert_eq!(r.terms, vec!["io_uring", "SQLite"]);
        assert_eq!(r.summary, "s");
    }

    #[test]
    fn parses_fenced_json_with_string_numbers_and_extra_keys() {
        let r = parse_result(
            "Here you go:\n```json\n{\"transcript\":[{\"start\":\"219.5\",\"end\":\"3:42\",\"text\":\"later\"},\
             {\"start\":\"1:05\",\"text\":\"\"},{\"text\":\"no time\"},{\"start\":\"217\",\"text\":\"first\"}],\
             \"visual\":[{\"t\":\"230\",\"description\":\"slide\"},{\"t\":\"x\",\"description\":\"bad\"}],\
             \"extra\":1}\n```\n",
        )
        .unwrap();
        assert_eq!(r.transcript.len(), 2, "empty and untimed segments dropped");
        assert_eq!(r.transcript[0].text, "first");
        assert_eq!(r.transcript[0].end, 217.0, "a missing end is the start");
        assert_eq!(r.transcript[1].start, 219.5);
        assert_eq!(r.transcript[1].end, 222.0);
        assert_eq!(r.visual.len(), 1);
        assert_eq!(r.visual[0].t, 230.0);
        assert!(r.summary.is_empty() && r.terms.is_empty());
    }

    #[test]
    fn chunk_offsets_already_applied_stay_as_they_are() {
        let r = parse_result(
            r#"{"transcript":[{"start":0,"end":3,"text":"x"},{"start":217,"end":220,"text":"y"}]}"#,
        )
        .unwrap();
        assert_eq!(r.transcript[1].start, 217.0);
        assert_eq!(r.transcript[1].end, 220.0);
    }

    #[test]
    fn rejects_text_without_a_result() {
        assert!(parse_result("done").is_none());
        assert!(parse_result(r#"{"foo":1}"#).is_none());
    }

    #[test]
    fn finds_the_result_in_zcode_stdout() {
        let inner = "```json\n{\"transcript\":[{\"start\":1,\"end\":2,\"text\":\"hi\"}],\"summary\":\"s\"}\n```";
        let stdout =
            serde_json::json!({"type":"result","session_id":"sess_abc-123","result":inner})
                .to_string();
        let r = result_from_stdout(&stdout).unwrap();
        assert_eq!(r.transcript[0].text, "hi");
        assert_eq!(session_id(&stdout).as_deref(), Some("sess_abc-123"));
        assert!(result_from_stdout(r#"{"result":"nothing here"}"#).is_none());
        assert!(result_from_stdout("").is_none());
    }

    #[test]
    fn classifies_quota() {
        let e = classify(
            "x\nProviderBusinessError: [1308][Usage limit reached for 5 hour. Your limit will reset at 2026-10-28 14:00:00]\n",
            "",
        )
        .unwrap();
        assert!(
            e.contains("usage limit") && e.contains("2026-10-28 14:00:00"),
            "{e}"
        );
        assert!(e.contains("2026-10-28 06:00 UTC"), "{e}");
    }

    #[test]
    fn classifies_expired_sign_in() {
        let e = classify(
            "[project-access-token] failed: project_token_login_failed",
            "",
        )
        .unwrap();
        assert_eq!(e, "ZCode sign-in expired; re-login needed");
        assert!(classify("", "Select a model").unwrap().contains("re-login"));
    }

    #[test]
    fn leaves_other_errors_unclassified() {
        assert!(classify("ENOENT: no such file", "").is_none());
        assert_eq!(tail("a\n\nb\nc\nd\n", 2, 100), "c | d");
    }

    #[test]
    fn utc_conversion_needs_the_plain_format() {
        assert_eq!(
            utc_of("2026-10-28 03:00:00").as_deref(),
            Some("2026-10-27 19:00")
        );
        assert!(utc_of("tomorrow").is_none());
    }

    /// The real thing: `cargo test ... video::zcode::tests::real_run -- --ignored --nocapture`.
    /// Uses one run of the owner's Z.ai quota; writes to temp dirs.
    #[tokio::test]
    #[ignore]
    async fn real_run() {
        let home = std::env::var("HOME").unwrap();
        let lookup = |k: &str| std::env::var(k).ok();
        let whisper = lucina_memo::transcribe::WhisperConfig::from_lookup(lookup);
        let data = tempfile::tempdir().unwrap();
        let cfg = VideoConfig::from_lookup(&lookup, data.path(), &whisper);
        let video = PathBuf::from(format!("{home}/scratch/yt-test/FBs--2nSPLQ.mkv"));
        let mut m = meta();
        m.description =
            std::fs::read_to_string(format!("{home}/scratch/yt-test/FBs--2nSPLQ.description"))
                .unwrap_or_default();
        let work = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        let r = run(&cfg, &m, &video, work.path(), folder.path()).await;
        println!("took {:?}", t.elapsed());
        println!(
            "zcode dir: {:?}",
            std::fs::read_dir(folder.path().join("zcode"))
                .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
        for f in ["zcode.out.json", "zcode-result.json", "zcode.err"] {
            if let Ok(s) = std::fs::read_to_string(folder.path().join("zcode").join(f)) {
                println!("--- {f}\n{}", s.chars().take(6000).collect::<String>());
            }
        }
        let r = r.unwrap();
        println!("{r:#?}");
        assert!(r.session.is_some());
    }
}
