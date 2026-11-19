//! Visual inspection: scene-change keyframes, OCR, Gemini vision with timestamps.
//!
//! 1. One cheap pass over the whole video (downscaled, 3 fps, VAAPI decode on the Raphael
//!    iGPU when it works) finds the scene changes;
//! 2. [`pick_timestamps`] turns them into the frames to look at: one near the start, the
//!    scene changes at least 2 s apart, evenly spaced fillers where nothing changed for a
//!    long time, all under a cap that grows with the length;
//! 3. those frames are cut at full resolution (≤ 1280 px), near-duplicates are dropped by
//!    comparing 16x16 grayscale thumbnails, and the survivors go to
//!    `<folder>/keyframes/<seconds>.jpg`;
//! 4. tesseract reads each frame, Gemini describes them in batches (the local Gemma model
//!    and then the OCR text alone are the fallbacks);
//! 5. the result is `<folder>/visual.json`.
//!
//! Every heavy program runs through [`thermal`]: capped scope, nice 15, and a wait while
//! nezopt is hot.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use futures_util::future::join_all;
use serde_json::Value;

use super::gemini::{self, Part};
use super::{thermal, Frame, VideoConfig, VideoMeta};
use crate::AppState;

/// ffmpeg's scene score above which a cut counts.
const SCENE_THRESHOLD: f64 = 0.12;
/// Two keyframes are never closer than this many seconds.
const MIN_GAP: f64 = 2.0;
/// Frames are cut this long after the detected cut, so they show the new scene.
const NUDGE: f64 = 0.25;
/// Frames per Gemini request.
const BATCH: usize = 10;
/// Most frames the local Gemma fallback is asked about in one run.
const LOCAL_FALLBACK_MAX: usize = 40;
const THUMB: usize = 16;
/// ffmpeg and tesseract runs at a time.
const CONCURRENCY: usize = 3;
const KINDS: [&str; 8] = [
    "slide", "code", "terminal", "diagram", "ui", "chart", "person", "other",
];

const SYSTEM: &str = "You look at keyframes taken from a video for a technical viewer who cannot see it. \
Each frame is preceded by a line `Frame at m:ss (OCR: …)` giving its time and the text an OCR engine read from it (it may be wrong or empty). \
For every frame say what is shown: slides, code, terminals, diagrams, UI, charts, graphs, people and what they are doing. \
Keep technical terms, identifiers, numbers, units and code EXACTLY as they appear on screen; do not paraphrase or translate them. \
Read code and shell commands verbatim into `code`, line by line; use null when the frame has none. \
Do not guess what is not visible and do not repeat the OCR text unless it is what the frame shows. \
Answer with a JSON array with exactly one object per frame, in the order given: \
[{\"t\": <the frame's time in seconds, a number>, \"kind\": \"slide|code|terminal|diagram|ui|chart|person|other\", \"description\": \"…\", \"code\": \"…\" or null}].";

// ------------------------------------------------------------------ entry point

/// Keyframes, OCR and descriptions for `video`; writes `folder/keyframes/*.jpg` and
/// `folder/visual.json` and returns the frames in time order.
pub async fn inspect(
    st: &AppState,
    cfg: &VideoConfig,
    meta: &VideoMeta,
    video: &Path,
    folder: &Path,
    work: &Path,
) -> anyhow::Result<Vec<Frame>> {
    let started = std::time::Instant::now();
    let duration = if meta.duration > 0.0 {
        meta.duration
    } else {
        probe_duration(cfg, video).await?
    };
    if !duration.is_finite() || duration <= 0.0 {
        bail!("the video has no duration");
    }
    let raw = work.join("keyframes-raw");
    let keyframes = folder.join("keyframes");
    for dir in [&raw, &keyframes] {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow!("clearing {}: {e}", dir.display())),
        }
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
    }

    // 1-2. scene changes → timestamps.
    thermal::wait_cool(&cfg.thermal).await;
    let scenes = detect_scenes(cfg, video, duration).await;
    let picks = pick_timestamps(&scenes, duration);
    tracing::info!(
        scenes = scenes.len(),
        picked = picks.len(),
        duration,
        "visual: timestamps chosen"
    );

    // 3. full-resolution frames, then the near-duplicates go.
    thermal::wait_cool(&cfg.thermal).await;
    let ffmpeg = resolve(&cfg.ffmpeg);
    // Plain loops over named async fns (a closure borrowing across an await trips
    // "FnOnce is not general enough" inside tokio::spawn, where the job runs).
    let mut cut: Vec<Cut> = Vec::new();
    for chunk in picks.chunks(CONCURRENCY) {
        let mut jobs = Vec::with_capacity(chunk.len());
        for pick in chunk {
            jobs.push(cut_one(cfg, &ffmpeg, video, &raw, *pick));
        }
        cut.extend(join_all(jobs).await.into_iter().flatten());
    }
    let cut_count = cut.len();
    let kept = drop_duplicates(cut);
    if kept.is_empty() {
        bail!("no frame could be taken from the video");
    }
    tracing::info!(cut = cut_count, kept = kept.len(), "visual: frames cut");

    // 4a. OCR.
    thermal::wait_cool(&cfg.thermal).await;
    let tesseract = find_in_path("tesseract");
    let langs = match &tesseract {
        Some(bin) => ocr_languages(cfg, bin).await,
        None => {
            tracing::warn!("visual: tesseract is not installed, no OCR");
            String::new()
        }
    };
    let mut texts: Vec<String> = Vec::with_capacity(kept.len());
    for chunk in kept.chunks(CONCURRENCY) {
        let mut jobs = Vec::with_capacity(chunk.len());
        for c in chunk {
            jobs.push(ocr_one(cfg, tesseract.as_deref(), &langs, &c.file));
        }
        texts.extend(join_all(jobs).await);
    }

    // 4b. vision.
    let pending: Vec<Pending> = kept
        .iter()
        .zip(texts)
        .map(|(c, ocr)| Pending {
            t: c.pick.t,
            file: c.file.clone(),
            ocr,
        })
        .collect();
    let local_budget = AtomicUsize::new(LOCAL_FALLBACK_MAX);
    let total = pending.len().div_ceil(BATCH);
    let mut described: Vec<Option<Described>> = Vec::with_capacity(pending.len());
    // Two Gemini requests at a time.
    for (round, pair) in pending.chunks(BATCH * 2).enumerate() {
        let mut jobs = Vec::new();
        for (k, batch) in pair.chunks(BATCH).enumerate() {
            jobs.push(describe_batch(
                st,
                cfg,
                meta,
                batch,
                &local_budget,
                (round * 2 + k + 1, total),
            ));
        }
        described.extend(join_all(jobs).await.into_iter().flatten());
    }

    // 5. archive.
    let mut frames = Vec::with_capacity(pending.len());
    for (p, d) in pending.iter().zip(described) {
        let name = format!("{}.jpg", frame_name(p.t));
        tokio::fs::copy(&p.file, keyframes.join(&name))
            .await
            .with_context(|| format!("saving keyframe {name}"))?;
        let d = d.unwrap_or_default();
        frames.push(Frame {
            t: p.t,
            file: format!("keyframes/{name}"),
            ocr: p.ocr.clone(),
            kind: d.kind,
            description: d.description,
            code: d.code,
        });
    }
    frames.sort_by(|a, b| a.t.total_cmp(&b.t));
    let json = serde_json::to_vec_pretty(&frames).context("encoding visual.json")?;
    tokio::fs::write(folder.join("visual.json"), json)
        .await
        .context("writing visual.json")?;
    let _ = tokio::fs::remove_dir_all(&raw).await;
    tracing::info!(
        frames = frames.len(),
        seconds = started.elapsed().as_secs(),
        "visual: done"
    );
    Ok(frames)
}

/// Cuts the frame for `pick` into `raw` and makes its thumbnail.
async fn cut_one(
    cfg: &VideoConfig,
    ffmpeg: &Path,
    video: &Path,
    raw: &Path,
    pick: Pick,
) -> Option<Cut> {
    let file = raw.join(format!("{}.jpg", frame_name(pick.t)));
    match extract_frame(cfg, ffmpeg, video, pick.t, &file).await {
        Ok(()) => {
            let thumb = thumbnail(cfg, ffmpeg, &file).await;
            Some(Cut { pick, file, thumb })
        }
        Err(e) => {
            tracing::warn!(t = pick.t, error = %e, "visual: could not cut a frame");
            None
        }
    }
}

async fn ocr_one(cfg: &VideoConfig, tesseract: Option<&Path>, langs: &str, jpg: &Path) -> String {
    match tesseract {
        Some(bin) => ocr_frame(cfg, bin, langs, jpg).await,
        None => String::new(),
    }
}

/// Takes one from the local-fallback budget, if any is left.
fn take_budget(budget: &AtomicUsize) -> bool {
    let mut n = budget.load(Ordering::SeqCst);
    while n > 0 {
        match budget.compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(now) => n = now,
        }
    }
    false
}

// ------------------------------------------------------------------ timestamps

/// Why a timestamp was picked (decides how strict the duplicate check is).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Why {
    Start,
    /// A scene change with its score.
    Scene(f64),
    /// Evenly spaced filler for a long quiet stretch.
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pick {
    t: f64,
    why: Why,
}

/// Longest stretch without a keyframe.
fn max_gap(duration: f64) -> f64 {
    (duration / 8.0).clamp(10.0, 45.0)
}

/// Most keyframes for a video of `duration` seconds.
fn frame_cap(duration: f64) -> usize {
    ((10.0 + duration / 12.0) as usize).min(120)
}

fn round_ms(t: f64) -> f64 {
    (t * 1000.0).round() / 1000.0
}

/// `n` items spread evenly over `v` (the first and last third of each slice stay open, so
/// the picks sit in the middle of equal parts).
fn thin_evenly<T: Copy>(v: &[T], n: usize) -> Vec<T> {
    if n >= v.len() {
        return v.to_vec();
    }
    (0..n)
        .map(|i| v[(((i as f64 + 0.5) * v.len() as f64) / n as f64) as usize])
        .collect()
}

/// The keyframe times for a video of `duration` seconds given its scene changes
/// (`(time, score)`): one near the start; scene changes at least [`MIN_GAP`] apart (the
/// stronger one when two are close); fillers wherever the gap to the next exceeds
/// [`max_gap`]; at most [`frame_cap`] in all. Sorted, rounded to the millisecond.
fn pick_timestamps(scenes: &[(f64, f64)], duration: f64) -> Vec<Pick> {
    if !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let start = (duration * 0.25).min(1.0);
    let mut cuts: Vec<(f64, f64)> = scenes
        .iter()
        .filter(|(t, s)| t.is_finite() && s.is_finite())
        .map(|&(t, s)| (t + NUDGE, s))
        .filter(|&(t, _)| t > start && t < duration - 0.5)
        .collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut kept = vec![Pick {
        t: start,
        why: Why::Start,
    }];
    for (t, score) in cuts {
        let last = *kept.last().unwrap_or(&kept[0]);
        if t - last.t >= MIN_GAP {
            kept.push(Pick {
                t,
                why: Why::Scene(score),
            });
        } else if let Why::Scene(old) = last.why {
            if score > old {
                if let Some(slot) = kept.last_mut() {
                    *slot = Pick {
                        t,
                        why: Why::Scene(score),
                    };
                }
            }
        }
    }

    let gap = max_gap(duration);
    let mut all: Vec<Pick> = Vec::new();
    for (i, p) in kept.iter().enumerate() {
        all.push(*p);
        let next = kept.get(i + 1).map_or(duration, |n| n.t);
        let span = next - p.t;
        if span > gap {
            let n = (span / gap).ceil() as usize - 1;
            for k in 1..=n {
                all.push(Pick {
                    t: p.t + span * k as f64 / (n + 1) as f64,
                    why: Why::Fill,
                });
            }
        }
    }

    let budget = frame_cap(duration).saturating_sub(1);
    let scene_picks: Vec<Pick> = all
        .iter()
        .copied()
        .filter(|p| matches!(p.why, Why::Scene(_)))
        .collect();
    let fill_picks: Vec<Pick> = all.iter().copied().filter(|p| p.why == Why::Fill).collect();
    if scene_picks.len() + fill_picks.len() > budget {
        let fill_keep = fill_picks
            .len()
            .min((budget / 3).max(budget.saturating_sub(scene_picks.len())));
        let scene_keep = scene_picks.len().min(budget - fill_keep);
        let mut strongest = scene_picks;
        strongest.sort_by(|a, b| score(b).total_cmp(&score(a)));
        strongest.truncate(scene_keep);
        all = std::iter::once(all[0])
            .chain(strongest)
            .chain(thin_evenly(&fill_picks, fill_keep))
            .collect();
        all.sort_by(|a, b| a.t.total_cmp(&b.t));
    }
    for p in &mut all {
        p.t = round_ms(p.t);
    }
    all
}

fn score(p: &Pick) -> f64 {
    match p.why {
        Why::Scene(s) => s,
        _ => 0.0,
    }
}

/// `0012.345`: the keyframe file stem for a time in seconds.
fn frame_name(t: f64) -> String {
    format!("{:08.3}", t.max(0.0))
}

/// `m:ss`, or `h:mm:ss` from an hour on.
fn fmt_ts(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Seconds from `12.5`, `12s`, `1:05` or `1:02:03`.
fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['s', 'S']).trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in s.split(':') {
        let v: f64 = part.trim().parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// The scene changes in `metadata=print` output: `frame:N pts:P pts_time:T` lines, each
/// followed by `lavfi.scene_score=S`.
fn parse_scene_metadata(out: &str) -> Vec<(f64, f64)> {
    let mut time: Option<f64> = None;
    let mut found = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.split("pts_time:").nth(1) {
            time = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(v) = line.trim().strip_prefix("lavfi.scene_score=") {
            if let (Some(t), Ok(s)) = (time.take(), v.trim().parse::<f64>()) {
                found.push((t, s));
            }
        }
    }
    found
}

// ------------------------------------------------------------------ ffmpeg

/// `program` as an absolute path (systemd-run wants one).
fn resolve(program: &Path) -> PathBuf {
    if program.is_absolute() || program.components().count() > 1 {
        return program.to_path_buf();
    }
    find_in_path(&program.to_string_lossy()).unwrap_or_else(|| program.to_path_buf())
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// The iGPU's render node for VAAPI decoding (`LUCINA_VIDEO_VAAPI_DEVICE`, default
/// renderD129; `off` disables), if it exists. renderD128 is the big GPU, others use it.
fn vaapi_device() -> Option<String> {
    let dev =
        std::env::var("LUCINA_VIDEO_VAAPI_DEVICE").unwrap_or_else(|_| "/dev/dri/renderD129".into());
    let dev = dev.trim();
    (!matches!(dev, "" | "off" | "none" | "false" | "0") && Path::new(dev).exists())
        .then(|| dev.to_owned())
}

async fn probe_duration(cfg: &VideoConfig, video: &Path) -> anyhow::Result<f64> {
    let mut cmd = thermal::command(&cfg.thermal, &resolve(&cfg.ffprobe));
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "csv=p=0",
    ])
    .arg(video);
    let out = thermal::output(cmd, Duration::from_secs(60)).await?;
    out.trim()
        .parse()
        .map_err(|_| anyhow!("ffprobe gave no duration"))
}

/// Scene changes `(time, score)`: a downscaled 3 fps pass over the whole video, decoded on
/// the iGPU when that works, else in software. A failure gives no scenes (the picker then
/// just samples evenly).
async fn detect_scenes(cfg: &VideoConfig, video: &Path, duration: f64) -> Vec<(f64, f64)> {
    let filter =
        format!("fps=3,scale=320:-2,select='gt(scene,{SCENE_THRESHOLD})',metadata=print:file=-");
    let ffmpeg = resolve(&cfg.ffmpeg);
    let timeout = Duration::from_secs(600 + duration as u64);
    let mut modes: Vec<Option<String>> = Vec::new();
    if let Some(dev) = vaapi_device() {
        modes.push(Some(dev));
    }
    modes.push(None);
    for device in modes {
        let mut cmd = thermal::command(&cfg.thermal, &ffmpeg);
        cmd.args(["-v", "error", "-nostdin", "-nostats"]);
        if let Some(dev) = &device {
            cmd.args(["-hwaccel", "vaapi", "-hwaccel_device", dev]);
        }
        cmd.arg("-i")
            .arg(video)
            .args(["-an", "-sn", "-vf", &filter, "-f", "null", "-"]);
        match thermal::output(cmd, timeout).await {
            Ok(out) => {
                tracing::info!(vaapi = device.is_some(), "visual: scene pass done");
                return parse_scene_metadata(&out);
            }
            Err(e) => {
                tracing::warn!(vaapi = device.is_some(), error = %e, "visual: scene pass failed");
            }
        }
    }
    Vec::new()
}

/// One frame at `t`, at most 1280 px on the long side, JPEG quality 3.
async fn extract_frame(
    cfg: &VideoConfig,
    ffmpeg: &Path,
    video: &Path,
    t: f64,
    out: &Path,
) -> anyhow::Result<()> {
    let mut cmd = thermal::command(&cfg.thermal, ffmpeg);
    cmd.args([
        "-v",
        "error",
        "-nostdin",
        "-y",
        "-ss",
        &format!("{t:.3}"),
        "-i",
    ])
    .arg(video)
    .args([
        "-an",
        "-sn",
        "-frames:v",
        "1",
        "-vf",
        "scale=w='if(gt(iw,ih),min(1280,iw),-2)':h='if(gt(iw,ih),-2,min(1280,ih))'",
        "-q:v",
        "3",
    ])
    .arg(out);
    thermal::output(cmd, Duration::from_secs(180)).await?;
    match tokio::fs::metadata(out).await {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => bail!("ffmpeg wrote no frame at {t:.3}s"),
    }
}

/// The frame as a 16x16 grayscale thumbnail (256 bytes), if it can be made.
async fn thumbnail(cfg: &VideoConfig, ffmpeg: &Path, jpg: &Path) -> Option<Vec<u8>> {
    let mut cmd = thermal::command(&cfg.thermal, ffmpeg);
    cmd.args(["-v", "error", "-nostdin", "-i"]).arg(jpg).args([
        "-frames:v",
        "1",
        "-vf",
        &format!("scale={THUMB}:{THUMB}:flags=area,format=gray"),
        "-f",
        "rawvideo",
        "-",
    ]);
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .ok()?
        .ok()?;
    (out.status.success() && out.stdout.len() == THUMB * THUMB).then_some(out.stdout)
}

// ------------------------------------------------------------------ duplicates

struct Cut {
    pick: Pick,
    file: PathBuf,
    thumb: Option<Vec<u8>>,
}

/// Mean absolute difference of two equally long thumbnails (0..255).
fn mean_abs_diff(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    Some(sum as f64 / a.len() as f64)
}

/// Is `cur` the same picture as `prev`? Scene changes are trusted (only a nearly identical
/// frame goes); fillers must differ a little more.
fn is_duplicate(prev: &[u8], cur: &[u8], why: Why) -> bool {
    let limit = match why {
        Why::Start => return false,
        Why::Scene(_) => 1.0,
        Why::Fill => 2.5,
    };
    mean_abs_diff(prev, cur).is_some_and(|d| d < limit)
}

/// Drops every frame that looks like the last one kept.
fn drop_duplicates(cut: Vec<Cut>) -> Vec<Cut> {
    let mut kept: Vec<Cut> = Vec::with_capacity(cut.len());
    for c in cut {
        let dup = match (
            kept.last().and_then(|k| k.thumb.as_deref()),
            c.thumb.as_deref(),
        ) {
            (Some(prev), Some(cur)) => is_duplicate(prev, cur, c.pick.why),
            _ => false,
        };
        if dup {
            tracing::debug!(t = c.pick.t, "visual: near-duplicate dropped");
        } else {
            kept.push(c);
        }
    }
    kept
}

// ------------------------------------------------------------------ OCR

/// `eng`, plus Italian and Danish when tesseract has them.
async fn ocr_languages(cfg: &VideoConfig, tesseract: &Path) -> String {
    let mut cmd = thermal::command(&cfg.thermal, tesseract);
    cmd.arg("--list-langs");
    let listed = thermal::output(cmd, Duration::from_secs(30))
        .await
        .unwrap_or_default();
    let mut langs = vec!["eng"];
    for l in ["ita", "dan"] {
        if listed.lines().any(|x| x.trim() == l) {
            langs.push(l);
        }
    }
    langs.join("+")
}

async fn ocr_frame(cfg: &VideoConfig, tesseract: &Path, langs: &str, jpg: &Path) -> String {
    let mut cmd = thermal::command(&cfg.thermal, tesseract);
    cmd.arg(jpg)
        .args(["stdout", "-l", langs])
        .env("OMP_THREAD_LIMIT", "1");
    match thermal::output(cmd, Duration::from_secs(90)).await {
        Ok(raw) => clean_ocr(&raw),
        Err(e) => {
            tracing::debug!(file = %jpg.display(), error = %e, "visual: OCR failed");
            String::new()
        }
    }
}

/// Tesseract's text without the noise: whitespace collapsed, lines with no letter or digit
/// dropped, at most 1500 characters.
fn clean_ocr(raw: &str) -> String {
    let lines: Vec<String> = raw
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| l.chars().any(char::is_alphanumeric))
        .collect();
    lines.join("\n").chars().take(1500).collect()
}

// ------------------------------------------------------------------ vision

struct Pending {
    t: f64,
    file: PathBuf,
    ocr: String,
}

/// What the vision model said about one frame.
#[derive(Debug, Clone, PartialEq)]
struct Described {
    kind: String,
    description: String,
    code: Option<String>,
}

impl Default for Described {
    fn default() -> Self {
        Self {
            kind: "other".into(),
            description: String::new(),
            code: None,
        }
    }
}

fn normalize_kind(v: &Value) -> String {
    let k = v.as_str().unwrap_or_default().trim().to_ascii_lowercase();
    if KINDS.contains(&k.as_str()) {
        k
    } else {
        "other".into()
    }
}

/// The frame objects in a Gemini answer: the array itself, or the first array inside an
/// object (`{"frames": […]}`).
fn answer_items(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o
            .values()
            .find_map(|x| x.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_else(|| {
                if o.contains_key("description") {
                    vec![v]
                } else {
                    vec![]
                }
            }),
        _ => Vec::new(),
    }
}

fn item_time(item: &Value) -> Option<f64> {
    match &item["t"] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_ts(s),
        _ => None,
    }
}

fn item_described(item: &Value) -> Option<Described> {
    let description = item["description"].as_str()?.trim().to_owned();
    if description.is_empty() {
        return None;
    }
    let code = item["code"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("null"))
        .map(str::to_owned);
    Some(Described {
        kind: normalize_kind(&item["kind"]),
        description,
        code,
    })
}

/// Matches the answer's items to the frames of the batch (`times`, seconds): by the
/// timestamp the model echoed (closest first, within 1.5 s), and by position for what is
/// left when it gave one item per frame. Frames without an item stay `None`.
fn map_answers(times: &[f64], answer: &Value) -> Vec<Option<Described>> {
    let items = answer_items(answer);
    let mut out: Vec<Option<Described>> = vec![None; times.len()];
    let mut used = vec![false; items.len()];
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (j, item) in items.iter().enumerate() {
        if let Some(t) = item_time(item) {
            for (i, ft) in times.iter().enumerate() {
                let d = (t - ft).abs();
                if d <= 1.5 {
                    pairs.push((d, i, j));
                }
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, i, j) in pairs {
        if out[i].is_none() && !used[j] {
            if let Some(d) = item_described(items[j]) {
                out[i] = Some(d);
            }
            used[j] = true;
        }
    }
    if items.len() == times.len() {
        let free_items: Vec<usize> = (0..items.len()).filter(|j| !used[*j]).collect();
        let free_frames: Vec<usize> = (0..times.len()).filter(|i| out[*i].is_none()).collect();
        for (i, j) in free_frames.into_iter().zip(free_items) {
            out[i] = item_described(items[j]);
        }
    }
    out
}

/// The request for one batch: an intro, then `Frame at m:ss (OCR: …)` and the picture, per frame.
fn batch_parts(meta: &VideoMeta, batch: &[Pending]) -> Vec<Part> {
    let mut parts = vec![Part::Text(format!(
        "Video: \"{}\"{}. {} frames follow, in time order. Answer with the JSON array.",
        meta.title,
        if meta.channel.is_empty() {
            String::new()
        } else {
            format!(" by {}", meta.channel)
        },
        batch.len()
    ))];
    for p in batch {
        let ocr = if p.ocr.trim().is_empty() {
            "none".to_owned()
        } else {
            p.ocr
                .lines()
                .collect::<Vec<_>>()
                .join(" | ")
                .chars()
                .take(600)
                .collect()
        };
        parts.push(Part::Text(format!("Frame at {} (OCR: {ocr})", fmt_ts(p.t))));
        parts.push(Part::Image(p.file.clone()));
    }
    parts
}

/// One Gemini request (retried once on a rate limit or a server error).
async fn ask_gemini(
    st: &AppState,
    cfg: &VideoConfig,
    meta: &VideoMeta,
    batch: &[Pending],
) -> anyhow::Result<Value> {
    let parts = batch_parts(meta, batch);
    let timeout = Duration::from_secs(240);
    match gemini::generate_json(&st.tool_env.http, cfg, SYSTEM, &parts, timeout).await {
        Err(e)
            if ["HTTP 429", "HTTP 5"]
                .iter()
                .any(|m| format!("{e:#}").contains(m)) =>
        {
            tokio::time::sleep(Duration::from_secs(8)).await;
            gemini::generate_json(&st.tool_env.http, cfg, SYSTEM, &parts, timeout).await
        }
        other => other,
    }
}

/// The local model's answer without its `CATEGORY:` line, and the kind that line implies.
fn local_answer(answer: &str) -> Described {
    let trimmed = answer.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let label = first
        .trim()
        .trim_start_matches(['*', '#', ' '])
        .strip_prefix("CATEGORY:")
        .or_else(|| first.trim().strip_prefix("Category:"))
        .map(|c| c.trim().trim_matches(['*', '.', ' ']).to_ascii_lowercase());
    let (kind, text) = match label {
        Some(l) => (
            match l.as_str() {
                "screenshot_chat" | "screenshot_error" | "screenshot_other" => "ui",
                "document_photo" => "slide",
                "whiteboard" => "diagram",
                _ => "other",
            },
            rest.trim(),
        ),
        None => ("other", trimmed.trim()),
    };
    Described {
        kind: kind.into(),
        description: text.to_owned(),
        code: None,
    }
}

/// Gemini for the batch; the local model for what Gemini missed (at most
/// [`LOCAL_FALLBACK_MAX`] frames a run); `None` for the rest (the OCR text stands alone).
async fn describe_batch(
    st: &AppState,
    cfg: &VideoConfig,
    meta: &VideoMeta,
    batch: &[Pending],
    local_budget: &AtomicUsize,
    progress: (usize, usize),
) -> Vec<Option<Described>> {
    let times: Vec<f64> = batch.iter().map(|p| p.t).collect();
    let mut out = match ask_gemini(st, cfg, meta, batch).await {
        Ok(v) => map_answers(&times, &v),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "visual: Gemini failed for a batch");
            vec![None; batch.len()]
        }
    };
    for (slot, p) in out.iter_mut().zip(batch) {
        if slot.is_some() {
            continue;
        }
        let allowed = take_budget(local_budget);
        if !allowed {
            continue;
        }
        // Not a share: these looks write no timing lines of their own.
        match crate::share::vision::describe(st, &p.file, true, crate::share::ShareTiming::silent())
            .await
        {
            Ok(a) => {
                let d = local_answer(&a);
                if !d.description.is_empty() {
                    *slot = Some(d);
                }
            }
            Err(e) => {
                tracing::warn!(t = p.t, error = %format!("{e:#}"), "visual: local vision failed")
            }
        }
    }
    tracing::info!(
        batch = progress.0,
        of = progress.1,
        described = out.iter().flatten().count(),
        frames = batch.len(),
        "visual: batch described"
    );
    out
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scene_metadata_is_paired_with_its_time() {
        let out = "frame:0    pts:100     pts_time:3.333\n\
                   lavfi.scene_score=0.512345\n\
                   frame:1    pts:900     pts_time:30\n\
                   lavfi.scene_score=0.31\n\
                   noise\n\
                   lavfi.scene_score=0.9\n";
        assert_eq!(
            parse_scene_metadata(out),
            vec![(3.333, 0.512345), (30.0, 0.31)]
        );
        assert!(parse_scene_metadata("").is_empty());
    }

    fn times(p: &[Pick]) -> Vec<f64> {
        p.iter().map(|p| p.t).collect()
    }

    #[test]
    fn the_first_frame_is_near_the_start_and_scenes_keep_their_distance() {
        let scenes = [(0.5, 0.9), (4.0, 0.5), (4.8, 0.7), (9.0, 0.4), (9.5, 0.35)];
        let picks = pick_timestamps(&scenes, 14.0);
        assert_eq!(
            picks[0],
            Pick {
                t: 1.0,
                why: Why::Start
            }
        );
        // 0.5 + nudge is within 2 s of the start; 4.0 and 4.8 collapse into the stronger one.
        assert_eq!(times(&picks), vec![1.0, 5.05, 9.25]);
        assert!(picks.windows(2).all(|w| w[1].t - w[0].t >= MIN_GAP));
        assert_eq!(picks[1].why, Why::Scene(0.7));
    }

    #[test]
    fn long_quiet_stretches_are_filled_evenly() {
        // 100 s, no scene changes: gap is 12.5 s, so the start plus fillers every ~12 s.
        let picks = pick_timestamps(&[], 100.0);
        assert_eq!(picks[0].why, Why::Start);
        assert!(picks.len() >= 8 && picks.len() <= 10, "{picks:?}");
        assert!(picks[1..].iter().all(|p| p.why == Why::Fill));
        let mut prev = 0.0;
        for p in &picks {
            assert!(p.t - prev <= max_gap(100.0) + 0.001, "{picks:?}");
            prev = p.t;
        }
        assert!(100.0 - prev <= max_gap(100.0) + 0.001);
        // Two scene changes in the middle leave the outer stretches to the fillers.
        let with = pick_timestamps(&[(50.0, 0.6), (60.0, 0.5)], 100.0);
        assert!(with.iter().any(|p| p.why == Why::Scene(0.6)));
        assert!(with.iter().any(|p| p.why == Why::Fill && p.t < 50.0));
    }

    #[test]
    fn the_cap_grows_with_the_length_and_keeps_the_strongest_scenes() {
        assert_eq!(frame_cap(98.0), 18);
        assert_eq!(frame_cap(7200.0), 120);
        assert!(max_gap(7200.0) <= 45.0 && max_gap(30.0) >= 10.0);
        // A 2 h talk with a cut every 5 s: 120 at most, the start stays, strong cuts win.
        let scenes: Vec<(f64, f64)> = (1..1400)
            .map(|i| (i as f64 * 5.0, if i % 100 == 0 { 0.95 } else { 0.4 }))
            .collect();
        let picks = pick_timestamps(&scenes, 7200.0);
        assert_eq!(picks.len(), 120, "{}", picks.len());
        assert_eq!(picks[0].why, Why::Start);
        assert!(picks.windows(2).all(|w| w[0].t < w[1].t));
        assert!(picks.iter().filter(|p| p.why == Why::Scene(0.95)).count() >= 13);
        // A quiet 2 h talk is sampled evenly under the cap.
        let quiet = pick_timestamps(&[], 7200.0);
        assert_eq!(quiet.len(), 120);
        assert!(
            quiet.last().is_some_and(|p| p.t > 7000.0),
            "{:?}",
            quiet.last()
        );
    }

    #[test]
    fn degenerate_durations_do_not_panic() {
        assert!(pick_timestamps(&[(1.0, 0.5)], 0.0).is_empty());
        assert!(pick_timestamps(&[], f64::NAN).is_empty());
        let tiny = pick_timestamps(&[(0.1, 0.9), (f64::NAN, 0.5)], 1.0);
        assert_eq!(times(&tiny), vec![0.25]);
    }

    #[test]
    fn names_and_labels() {
        assert_eq!(frame_name(12.345), "0012.345");
        assert_eq!(frame_name(0.0), "0000.000");
        assert_eq!(frame_name(7265.5), "7265.500");
        assert_eq!(fmt_ts(12.4), "0:12");
        assert_eq!(fmt_ts(754.6), "12:35");
        assert_eq!(fmt_ts(3725.0), "1:02:05");
        assert_eq!(parse_ts("1:05"), Some(65.0));
        assert_eq!(parse_ts("1:02:03"), Some(3723.0));
        assert_eq!(parse_ts(" 12.5s "), Some(12.5));
        assert_eq!(parse_ts("abc"), None);
        assert_eq!(parse_ts(""), None);
    }

    #[test]
    fn thinning_spreads_evenly() {
        let v: Vec<u32> = (0..10).collect();
        assert_eq!(thin_evenly(&v, 5), vec![1, 3, 5, 7, 9]);
        assert_eq!(thin_evenly(&v, 20), v);
        assert!(thin_evenly(&v, 0).is_empty());
    }

    #[test]
    fn duplicates_are_judged_by_thumbnail_distance() {
        let a = vec![100u8; 256];
        let mut b = a.clone();
        assert_eq!(mean_abs_diff(&a, &b), Some(0.0));
        b.iter_mut().for_each(|x| *x = 102);
        assert_eq!(mean_abs_diff(&a, &b), Some(2.0));
        assert!(is_duplicate(&a, &b, Why::Fill));
        assert!(!is_duplicate(&a, &b, Why::Scene(0.5)));
        assert!(!is_duplicate(&a, &a, Why::Start));
        assert!(is_duplicate(&a, &a, Why::Scene(0.5)));
        assert_eq!(mean_abs_diff(&a, &b[..10]), None);
        assert!(!is_duplicate(&a, &b[..10], Why::Fill));
    }

    #[test]
    fn duplicates_are_dropped_against_the_last_kept_frame() {
        let cut = |t: f64, why: Why, level: u8| Cut {
            pick: Pick { t, why },
            file: PathBuf::from(format!("{t}.jpg")),
            thumb: Some(vec![level; 256]),
        };
        let kept = drop_duplicates(vec![
            cut(1.0, Why::Start, 100),
            cut(13.0, Why::Fill, 101),
            cut(25.0, Why::Fill, 102),
            cut(37.0, Why::Fill, 104),
            cut(40.0, Why::Scene(0.6), 200),
        ]);
        let kept: Vec<f64> = kept.iter().map(|c| c.pick.t).collect();
        // 13 (diff 1) and 25 (diff 2) look like frame 1; 37 (diff 4) does not.
        assert_eq!(kept, vec![1.0, 37.0, 40.0]);
    }

    #[test]
    fn ocr_text_is_cleaned() {
        let raw = "  io_uring   HTTP server \n\n ~~ -- \n|||\n  p99 =  1.2ms\n\u{c}\n";
        assert_eq!(clean_ocr(raw), "io_uring HTTP server\np99 = 1.2ms");
        assert_eq!(clean_ocr("  \n--\n"), "");
        assert_eq!(clean_ocr(&"a".repeat(3000)).len(), 1500);
    }

    #[test]
    fn gemini_answers_map_back_by_time() {
        let ts = [1.0, 13.2, 25.0, 37.5];
        let answer = json!([
            {"t": 1, "kind": "Person", "description": " A man at a desk. ", "code": null},
            {"t": "0:13", "kind": "terminal", "description": "A shell.", "code": "$ wrk -t4 http://x\n"},
            {"t": 37.4, "kind": "hologram", "description": "A bar chart: 1.2M req/s", "code": "null"},
        ]);
        let got = map_answers(&ts, &answer);
        assert_eq!(
            got[0],
            Some(Described {
                kind: "person".into(),
                description: "A man at a desk.".into(),
                code: None
            })
        );
        assert_eq!(got[1].as_ref().map(|d| d.kind.as_str()), Some("terminal"));
        assert_eq!(
            got[1].as_ref().and_then(|d| d.code.as_deref()),
            Some("$ wrk -t4 http://x")
        );
        assert_eq!(got[2], None, "no item for 0:25");
        assert_eq!(got[3].as_ref().map(|d| d.kind.as_str()), Some("other"));
        assert_eq!(got[3].as_ref().and_then(|d| d.code.as_deref()), None);
    }

    #[test]
    fn gemini_answers_without_usable_times_map_by_position() {
        let ts = [1.0, 13.0];
        let answer = json!({"frames": [
            {"kind": "slide", "description": "Title slide"},
            {"t": "soon", "kind": "code", "description": "A function", "code": "fn main() {}"},
        ]});
        let got = map_answers(&ts, &answer);
        assert_eq!(
            got[0].as_ref().map(|d| d.description.as_str()),
            Some("Title slide")
        );
        assert_eq!(
            got[1].as_ref().and_then(|d| d.code.as_deref()),
            Some("fn main() {}")
        );
        // A different count and no times: nothing can be trusted.
        let short = json!([{"description": "x"}]);
        assert!(map_answers(&ts, &short).iter().all(Option::is_none));
        assert!(map_answers(&ts, &json!("nope")).iter().all(Option::is_none));
        // Empty descriptions are not answers.
        assert!(map_answers(&[1.0], &json!([{"t": 1, "description": "  "}]))[0].is_none());
    }

    #[test]
    fn the_local_models_category_line_becomes_the_kind() {
        let d = local_answer("CATEGORY: screenshot_error\nSegfault in main.rs");
        assert_eq!(
            (d.kind.as_str(), d.description.as_str()),
            ("ui", "Segfault in main.rs")
        );
        let d = local_answer("A plain answer.");
        assert_eq!(
            (d.kind.as_str(), d.description.as_str()),
            ("other", "A plain answer.")
        );
    }

    #[test]
    fn requests_label_every_frame() {
        let meta = VideoMeta {
            title: "Talk".into(),
            channel: "Me".into(),
            ..Default::default()
        };
        let batch = [Pending {
            t: 65.2,
            file: PathBuf::from("a.jpg"),
            ocr: "one\ntwo".into(),
        }];
        let parts = batch_parts(&meta, &batch);
        assert_eq!(parts.len(), 3);
        assert!(
            matches!(&parts[0], Part::Text(t) if t.contains("\"Talk\" by Me") && t.contains("1 frames"))
        );
        assert!(matches!(&parts[1], Part::Text(t) if t == "Frame at 1:05 (OCR: one | two)"));
        assert!(matches!(&parts[2], Part::Image(p) if p == Path::new("a.jpg")));
    }

    /// One real run: `set -a; . ~/.config/lucina-rs/gemini.env; set +a` first, then
    /// `cargo test -p lucina-server --lib video::visual::tests::manual -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs ffmpeg, tesseract, a Gemini key and a video"]
    async fn manual_end_to_end() {
        let home = std::env::var("HOME").unwrap();
        let video = std::env::var("YT_TEST_VIDEO")
            .unwrap_or_else(|_| format!("{home}/scratch/yt-test/FBs--2nSPLQ.mkv"));
        let out = PathBuf::from(format!("{home}/scratch/yt-test/visual-out"));
        let _ = std::fs::remove_dir_all(&out);
        let (folder, work) = (out.join("folder"), out.join("work"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let data = tempfile::tempdir().unwrap();
        let st = AppState::build(crate::Config::for_data_dir(data.path()), None).unwrap();
        let mut cfg = st.cfg.video.clone();
        if let Some(max) = std::env::var("LUCINA_VIDEO_MAX_TCTL")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            cfg.thermal.max_c = max;
        }
        let meta = VideoMeta {
            id: "FBs--2nSPLQ".into(),
            title: "I challenged Rust Developers".into(),
            duration: 98.0,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let frames = inspect(&st, &cfg, &meta, Path::new(&video), &folder, &work)
            .await
            .unwrap();
        println!(
            "{} frames in {:.1}s (model {})",
            frames.len(),
            started.elapsed().as_secs_f64(),
            cfg.gemini_model
        );
        for f in &frames {
            println!(
                "{:>7.3} {} [{}] {}\n    ocr: {:?}",
                f.t, f.file, f.kind, f.description, f.ocr
            );
        }
        assert!(!frames.is_empty());
        assert!(folder.join("visual.json").is_file());
        assert!(frames.iter().all(|f| folder.join(&f.file).is_file()));
    }
}
