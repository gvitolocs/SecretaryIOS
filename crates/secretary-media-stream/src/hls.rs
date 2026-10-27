//! HLS output from ffmpeg: fMP4 segments, streams copied when the client can
//! decode them (remux) and re-encoded only when it cannot.
//!
//! One ffmpeg job per (file, variant) runs to the end and its output stays in
//! the cache, so the second play of a film starts at once. While a job runs
//! the playlist is an EVENT playlist that starts at 0; when it finishes it is
//! served as VOD, starting where the viewer resumes.
//!
//! Jobs come from a viewer (`/play`, the playlist) or from the background
//! preparation (`prepare`). The two have separate slot pools, and background
//! ffmpeg runs niced with fewer threads, so a viewer never waits behind it.
use crate::gate::{Gate, Watchdog};
use secretary_player::playback::{Plan, Stream};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore};

/// The variant name in URLs and cache paths: `<video>-<audio>[-hvc1][-tm]`,
/// e.g. `copy-aac-hvc1`. Parsed back strictly, so a URL cannot inject flags.
pub fn variant_name(plan: &Plan) -> String {
    let s = |s: Stream| match s {
        Stream::None => "none",
        Stream::Copy => "copy",
        Stream::H264 => "h264",
        Stream::Aac => "aac",
    };
    let mut name = format!("{}-{}", s(plan.video), s(plan.audio));
    if plan.hvc1_tag {
        name.push_str("-hvc1");
    }
    if plan.tonemap {
        name.push_str("-tm");
    }
    name
}

pub fn parse_variant(name: &str) -> Option<Plan> {
    let mut parts = name.split('-');
    let stream = |p: Option<&str>| match p? {
        "none" => Some(Stream::None),
        "copy" => Some(Stream::Copy),
        "h264" => Some(Stream::H264),
        "aac" => Some(Stream::Aac),
        _ => None,
    };
    let video = stream(parts.next())?;
    let audio = stream(parts.next())?;
    if !matches!(video, Stream::None | Stream::Copy | Stream::H264) || !matches!(audio, Stream::None | Stream::Copy | Stream::Aac) {
        return None;
    }
    let (mut hvc1_tag, mut tonemap) = (false, false);
    for p in parts {
        match p {
            "hvc1" if !hvc1_tag => hvc1_tag = true,
            "tm" if !tonemap => tonemap = true,
            _ => return None,
        }
    }
    let mode = if video == Stream::H264 || audio == Stream::Aac {
        secretary_player::playback::Mode::Transcode
    } else {
        secretary_player::playback::Mode::Remux
    };
    Some(Plan { mode, video, audio, hvc1_tag, tonemap })
}

/// Segment length in seconds. Short, so a resume loads a small segment first
/// and a fresh transcode has its first segment within a second or two. Outputs
/// cached with the earlier 6 s segments stay valid: the playlist says how long
/// each segment is.
pub const SEGMENT_SECONDS: u32 = 2;

/// The segment length for one file. ffmpeg writes the fMP4 init when it cuts the first segment,
/// and a copied AC-3/E-AC-3 track needs one of its packets by then, or the init gets an empty
/// sample description that no player reads ("Up": audio from 5.8 s). So with copied audio the
/// first cut must come after the audio starts: longer segments when it starts late, the old 6 s
/// when the lead is unknown. Re-encoded (AAC) or no audio: the short default.
pub fn segment_seconds(plan: &Plan, audio_lead_ms: Option<u64>) -> u32 {
    if plan.audio != Stream::Copy {
        return SEGMENT_SECONDS;
    }
    match audio_lead_ms {
        Some(lead) if lead <= 1000 => SEGMENT_SECONDS,
        Some(lead) => (lead.div_ceil(1000) as u32 + 2).max(SEGMENT_SECONDS),
        None => 6,
    }
}

/// Whether an fMP4 init is usable: it has a `moov`, and every `stsd` (one per track) holds a
/// sample entry. ffmpeg exits 0 even when it wrote an empty one (see `segment_seconds`).
pub fn init_is_complete(bytes: &[u8]) -> bool {
    fn walk(b: &[u8], moov: &mut bool, ok: &mut bool) {
        let mut i = 0usize;
        while i + 8 <= b.len() {
            let size = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
            let kind = &b[i + 4..i + 8];
            if size < 8 || i + size > b.len() {
                *ok = false;
                return;
            }
            let body = &b[i + 8..i + size];
            match kind {
                b"moov" => {
                    *moov = true;
                    walk(body, moov, ok);
                }
                b"trak" | b"mdia" | b"minf" | b"stbl" => walk(body, moov, ok),
                // Full box: version/flags (4), entry_count (4), then entries.
                b"stsd" => {
                    let count = body.get(4..8).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).unwrap_or(0);
                    if count == 0 || body.len() < 8 + 8 {
                        *ok = false;
                    }
                }
                _ => {}
            }
            i += size;
        }
    }
    let (mut moov, mut ok) = (false, true);
    walk(bytes, &mut moov, &mut ok);
    moov && ok
}

/// Who asked for a job: a viewer now, or the background preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Interactive,
    Background,
}

/// Background ffmpeg: the lowest CPU priority and a small thread pool, so a
/// viewer's own transcode (and everything else on the box) keeps the cores.
pub const BACKGROUND_NICE: &str = "19";
pub const BACKGROUND_THREADS: u32 = 2;

/// The wrappers a background job runs under. The defaults are all off, so the
/// library (or tests) opt in; lucina-media detects them at startup.
#[derive(Debug, Clone)]
pub struct BackgroundConfig {
    /// Run in `systemd-run --user --scope` so the CPU quota can be enforced.
    pub scope: bool,
    /// The scope's CPU quota in percent (100 = one core).
    pub cpu_quota: u32,
    /// VAAPI render node; None keeps every encode on libx264.
    pub vaapi_device: Option<PathBuf>,
    /// The GPU VRAM semaphore VAAPI jobs run under.
    pub gpu_lease: Option<PathBuf>,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self { scope: false, cpu_quota: 100, vaapi_device: None, gpu_lease: None }
    }
}

impl From<&crate::StreamConfig> for BackgroundConfig {
    fn from(cfg: &crate::StreamConfig) -> Self {
        Self {
            scope: cfg.background_scope,
            cpu_quota: cfg.background_cpu_quota,
            vaapi_device: cfg.vaapi_device.clone(),
            gpu_lease: cfg.gpu_lease.clone(),
        }
    }
}

/// Whether this background job can use the VAAPI encoder. Tone-mapping needs
/// zscale on the CPU, so it stays on libx264 (and so does everything when no
/// render node is configured, or after a VAAPI failure).
pub fn uses_vaapi(plan: &Plan, background: &BackgroundConfig, allow: bool) -> bool {
    allow && plan.video == Stream::H264 && !plan.tonemap && background.vaapi_device.is_some()
}

/// Whether a background job is on the CPU path: libx264 or tone-mapping. Only
/// these wait for the night window; VAAPI and remux jobs may start any hour.
pub fn is_cpu_path(plan: &Plan, background: &BackgroundConfig, allow_vaapi: bool) -> bool {
    plan.video == Stream::H264 && !uses_vaapi(plan, background, allow_vaapi)
}

pub fn ffmpeg_args(
    src: &Path,
    plan: &Plan,
    out: &Path,
    priority: Priority,
    segment: u32,
    background: &BackgroundConfig,
    use_vaapi: bool,
) -> Vec<String> {
    let is_background = priority == Priority::Background;
    let vaapi = use_vaapi && is_background && plan.video == Stream::H264 && !plan.tonemap && background.vaapi_device.is_some();
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"].map(String::from).to_vec();
    // Software decode with a small pool: the RX 7900 XTX has no MPEG-2 decoder,
    // and the CPU budget belongs to the rest of the box.
    if is_background && plan.video == Stream::H264 {
        a.extend(["-threads".to_string(), BACKGROUND_THREADS.to_string()]);
    }
    a.push("-i".into());
    a.push(src.display().to_string());
    if plan.video != Stream::None {
        a.extend(["-map", "0:v:0"].map(String::from));
    }
    if plan.audio != Stream::None {
        a.extend(["-map", "0:a:0"].map(String::from));
    }
    match plan.video {
        Stream::Copy => {
            a.extend(["-c:v", "copy"].map(String::from));
            if plan.hvc1_tag {
                a.extend(["-tag:v", "hvc1"].map(String::from));
            }
        }
        Stream::H264 if vaapi => {
            let device = background.vaapi_device.as_ref().expect("vaapi device checked above");
            a.push("-vaapi_device".into());
            a.push(device.display().to_string());
            a.extend(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=nv12,hwupload"].map(String::from));
            a.extend(["-c:v", "h264_vaapi", "-qp", "21", "-profile:v", "high", "-bf", "0"].map(String::from));
            // A keyframe at every segment boundary, so segments cut evenly.
            a.push("-force_key_frames".into());
            a.push(format!("expr:gte(t,n_forced*{segment})"));
        }
        Stream::H264 => {
            if plan.tonemap {
                a.push("-vf".into());
                a.push("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p".into());
            } else {
                // Even dimensions and 8-bit 4:2:0: what every H.264 decoder takes.
                a.extend(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p"].map(String::from));
            }
            a.extend(
                ["-c:v", "libx264", "-preset", "veryfast", "-crf", "21", "-profile:v", "high", "-level:v", "4.1"].map(String::from),
            );
            if is_background {
                a.extend(["-threads".to_string(), BACKGROUND_THREADS.to_string()]);
            }
            // A keyframe at every segment boundary, so segments cut evenly.
            a.push("-force_key_frames".into());
            a.push(format!("expr:gte(t,n_forced*{segment})"));
        }
        _ => {}
    }
    match plan.audio {
        Stream::Copy => a.extend(["-c:a", "copy"].map(String::from)),
        Stream::Aac => a.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2"].map(String::from)),
        _ => {}
    }
    a.extend(
        [
            "-f",
            "hls",
            "-hls_time",
            &segment.to_string(),
            "-hls_playlist_type",
            "event",
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init.mp4",
            "-hls_flags",
            "independent_segments+temp_file",
            "-hls_segment_filename",
        ]
        .map(String::from),
    );
    a.push(out.join("seg_%05d.m4s").display().to_string());
    a.push(out.join("index.m3u8").display().to_string());
    a
}

/// The program and arguments to run. Interactive jobs are untouched. Background
/// jobs are niced, optionally under `gpu-lease` (VAAPI) and always in the CPU
/// scope when one is configured.
pub fn command_line(
    ffmpeg: &Path,
    args: Vec<String>,
    priority: Priority,
    background: &BackgroundConfig,
    use_vaapi: bool,
) -> (PathBuf, Vec<String>) {
    if priority == Priority::Interactive {
        return (ffmpeg.to_path_buf(), args);
    }
    let mut nice: Vec<String> = ["-n", BACKGROUND_NICE, &ffmpeg.display().to_string()].map(String::from).to_vec();
    nice.extend(args);
    let (mut program, mut program_args) = if use_vaapi {
        match &background.gpu_lease {
            Some(lease) => {
                let mut a: Vec<String> =
                    ["run".into(), "--name".into(), "media-prepare".into(), "--mb".into(), "512".into(), "--".into(), "nice".into()]
                        .to_vec();
                a.extend(nice);
                (lease.clone(), a)
            }
            None => (PathBuf::from("nice"), nice),
        }
    } else {
        (PathBuf::from("nice"), nice)
    };
    if background.scope {
        let mut a: Vec<String> = vec![
            "--user".into(),
            "--scope".into(),
            "--quiet".into(),
            "-p".into(),
            format!("CPUQuota={}%", background.cpu_quota),
            "--".into(),
            program.display().to_string(),
        ];
        a.extend(program_args);
        program = PathBuf::from("systemd-run");
        program_args = a;
    }
    (program, program_args)
}

#[derive(Debug, Clone, PartialEq)]
enum JobState {
    Running,
    Failed(String),
}

/// A background job that did not finish: a failure is remembered, while a
/// watchdog stop is a pause and the output goes back in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Failed(String),
    Stopped(String),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(e) | Self::Stopped(e) => f.write_str(e),
        }
    }
}

/// Runs ffmpeg jobs: at most `limit` a viewer asked for at once, and one
/// background job beside them.
pub struct Jobs {
    ffmpeg: PathBuf,
    /// The wrappers background jobs use (scope, VAAPI, gpu-lease).
    background: BackgroundConfig,
    slots: Arc<Semaphore>,
    /// Background preparation's own pool, so a viewer's job never queues behind it.
    prepare_slot: Arc<Semaphore>,
    jobs: Mutex<HashMap<PathBuf, JobState>>,
}

pub const DONE_MARKER: &str = "done";

impl Jobs {
    pub fn new(ffmpeg: PathBuf, limit: usize, background: BackgroundConfig) -> Self {
        Self {
            ffmpeg,
            background,
            slots: Arc::new(Semaphore::new(limit.max(1))),
            prepare_slot: Arc::new(Semaphore::new(1)),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a job for `out` is running (or waiting for its slot).
    pub fn is_running(&self, out: &Path) -> bool {
        matches!(self.jobs.lock().expect("jobs lock").get(out), Some(JobState::Running))
    }

    /// Starts the job for `out` unless it is finished or running. Returns an
    /// error when an earlier run failed (the next call retries).
    pub fn ensure(self: &Arc<Self>, src: PathBuf, plan: Plan, out: PathBuf, segment: u32) -> Result<(), String> {
        if out.join(DONE_MARKER).exists() {
            return Ok(());
        }
        let mut jobs = self.jobs.lock().expect("jobs lock");
        match jobs.get(&out) {
            Some(JobState::Running) => return Ok(()),
            Some(JobState::Failed(e)) => {
                let e = e.clone();
                jobs.remove(&out);
                return Err(e);
            }
            None => {}
        }
        jobs.insert(out.clone(), JobState::Running);
        drop(jobs);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.run(&src, &plan, &out, segment).await;
            let mut jobs = this.jobs.lock().expect("jobs lock");
            match result {
                Ok(()) => {
                    jobs.remove(&out);
                }
                Err(e) => {
                    tracing::warn!(out = %out.display(), "hls job failed: {e}");
                    let _ = std::fs::remove_dir_all(&out);
                    jobs.insert(out, JobState::Failed(e));
                }
            }
        });
        Ok(())
    }

    /// Runs the background job for `out` to the end, one at a time, unless it
    /// is finished or already running (a viewer's job is left to finish on its
    /// own). Returns whether it ran; an error when it, or a viewer's run, failed. A viewer who asks for the same variant
    /// meanwhile is served from this job, as from their own.
    pub async fn prepare(&self, src: &Path, plan: &Plan, out: &Path, segment: u32) -> Result<bool, JobError> {
        self.prepare_with(src, plan, out, segment, true, None).await
    }

    /// Like [`prepare`](Self::prepare), with the temperature/load watchdog.
    pub async fn prepare_watched(
        &self,
        src: &Path,
        plan: &Plan,
        out: &Path,
        segment: u32,
        allow_vaapi: bool,
        gate: &Gate,
    ) -> Result<bool, JobError> {
        self.prepare_with(src, plan, out, segment, allow_vaapi, Some(gate)).await
    }

    async fn prepare_with(
        &self,
        src: &Path,
        plan: &Plan,
        out: &Path,
        segment: u32,
        allow_vaapi: bool,
        gate: Option<&Gate>,
    ) -> Result<bool, JobError> {
        // The slot first, then the claim: a viewer never finds a Running job
        // that is still waiting for a background slot.
        let _slot = self.prepare_slot.acquire().await.map_err(|e| JobError::Failed(e.to_string()))?;
        if out.join(DONE_MARKER).exists() {
            return Ok(false);
        }
        {
            let mut jobs = self.jobs.lock().expect("jobs lock");
            match jobs.get(out) {
                Some(JobState::Running) => return Ok(false),
                // A viewer's run failed: left for their retry, and the worker moves on.
                Some(JobState::Failed(e)) => return Err(JobError::Failed(e.clone())),
                None => {}
            }
            jobs.insert(out.to_path_buf(), JobState::Running);
        }
        let result = self
            .exec(src, plan, out, Priority::Background, segment, &self.background, allow_vaapi, gate)
            .await;
        // A failure is not recorded for the viewer: their next play retries at full speed.
        self.jobs.lock().expect("jobs lock").remove(out);
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(out).await;
        }
        result.map(|()| true)
    }

    async fn run(&self, src: &Path, plan: &Plan, out: &Path, segment: u32) -> Result<(), String> {
        let _slot = self.slots.acquire().await.map_err(|e| e.to_string())?;
        self.exec(src, plan, out, Priority::Interactive, segment, &self.background, false, None)
            .await
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn exec(
        &self,
        src: &Path,
        plan: &Plan,
        out: &Path,
        priority: Priority,
        segment: u32,
        background: &BackgroundConfig,
        allow_vaapi: bool,
        gate: Option<&Gate>,
    ) -> Result<(), JobError> {
        // Start clean: a half-written run from before a restart is not resumable.
        let _ = tokio::fs::remove_dir_all(out).await;
        tokio::fs::create_dir_all(out).await.map_err(|e| JobError::Failed(e.to_string()))?;
        let use_vaapi = priority == Priority::Background && uses_vaapi(plan, background, allow_vaapi);
        let args = ffmpeg_args(src, plan, out, priority, segment, background, use_vaapi);
        let (program, args) = command_line(&self.ffmpeg, args, priority, background, use_vaapi);
        tracing::info!(src = %src.display(), variant = %variant_name(plan), ?priority, ?use_vaapi, "hls job started");
        let output = run_command(&program, &args, gate).await?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let tail: String = err.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Err(JobError::Failed(format!("ffmpeg exited with {}: {tail}", output.status)));
        }
        // A broken init plays nowhere: not done, so the output is removed and made again.
        let init = tokio::fs::read(out.join("init.mp4")).await.unwrap_or_default();
        if !init_is_complete(&init) {
            return Err(JobError::Failed(format!(
                "ffmpeg wrote an incomplete init.mp4 ({} bytes, segments of {segment} s)",
                init.len()
            )));
        }
        tokio::fs::write(out.join(DONE_MARKER), b"").await.map_err(|e| JobError::Failed(e.to_string()))?;
        tracing::info!(src = %src.display(), variant = %variant_name(plan), ?priority, segment, "hls job finished");
        Ok(())
    }

    /// Waits until the playlist has its first segment (or the job fails).
    pub async fn wait_ready(&self, out: &Path, timeout: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(text) = tokio::fs::read_to_string(out.join("index.m3u8")).await {
                if text.contains("#EXTINF") {
                    return Ok(());
                }
            }
            if let Some(JobState::Failed(e)) = self.jobs.lock().expect("jobs lock").get(out) {
                return Err(e.clone());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("timed out waiting for the first segment".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

struct CommandOutput {
    status: ExitStatus,
    stderr: Vec<u8>,
}

/// Runs one ffmpeg command. A background command is put in its own process group
/// and watched: three over-limit samples kill the whole group (systemd-run,
/// gpu-lease, nice, ffmpeg), not just the first process.
async fn run_command(program: &Path, args: &[String], gate: Option<&Gate>) -> Result<CommandOutput, JobError> {
    let mut cmd = Command::new(program);
    cmd.args(args).kill_on_drop(true);
    let Some(gate) = gate else {
        let output = cmd.output().await.map_err(|e| JobError::Failed(format!("ffmpeg: {e}")))?;
        return Ok(CommandOutput { status: output.status, stderr: output.stderr });
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).process_group(0);
    let mut child = cmd.spawn().map_err(|e| JobError::Failed(format!("ffmpeg: {e}")))?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });
    let mut watchdog = Watchdog::default();
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(10), Duration::from_secs(10));
    let status = loop {
        enum Event {
            Done(std::io::Result<ExitStatus>),
            Stop(crate::gate::Reading),
        }
        let event = tokio::select! {
            status = child.wait() => Event::Done(status),
            _ = ticker.tick() => {
                let reading = gate.reading();
                if watchdog.sample(gate.over(reading)) {
                    Event::Stop(reading)
                } else {
                    continue;
                }
            }
        };
        match event {
            Event::Done(status) => break status.map_err(|e| JobError::Failed(format!("ffmpeg: {e}")))?,
            Event::Stop(reading) => {
                if let Some(pid) = child.id() {
                    // SAFETY: the child leads the process group made by
                    // `process_group(0)`; a negative pid signals the group.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.start_kill();
                let _ = child.wait().await;
                reader.abort();
                return Err(JobError::Stopped(gate.stop_reason(reading)));
            }
        }
    };
    let stderr = reader.await.unwrap_or_default();
    Ok(CommandOutput { status, stderr })
}

/// `start_ms` from the playlist URL: an integer (sign and digits only), else
/// nothing, so a malformed value plays from the start as before.
pub fn parse_start_ms(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Where a finished playlist of `duration_ms` starts for a viewer resuming at
/// `start_ms`: inside [0, duration − 1 s]. No start when the length is unknown.
pub fn start_offset_ms(start_ms: i64, duration_ms: Option<u64>) -> Option<u64> {
    duration_ms.map(|d| crate::kernel::clamp_start_ms(start_ms, d))
}

/// The length of a playlist: the sum of its segments, or None without any.
fn playlist_duration_ms(text: &str) -> Option<u64> {
    let mut total = 0.0f64;
    let mut any = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let secs = rest.split(',').next().unwrap_or("").trim();
            total += secs.parse::<f64>().ok()?;
            any = true;
        }
    }
    any.then(|| (total * 1000.0).round() as u64)
}

/// Milliseconds as playlist seconds: `0`, `754.5`, `1.25`.
fn seconds(ms: u64) -> String {
    let frac = format!("{:03}", ms % 1000);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        (ms / 1000).to_string()
    } else {
        format!("{}.{frac}", ms / 1000)
    }
}

/// Rewrites ffmpeg's playlist for the client: a running job's EVENT playlist
/// starts at 0 (AVPlayer would otherwise join at the live edge); a finished
/// one becomes VOD so the whole film is seekable, and starts at `start_ms`
/// when the viewer resumes, so AVPlayer loads that segment first instead of
/// segment 0.
pub fn rewrite_playlist(text: &str, done: bool, start_ms: Option<i64>) -> String {
    let start = if done { start_ms.and_then(|s| start_offset_ms(s, playlist_duration_ms(text))) } else { Some(0) };
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        if line.starts_with("#EXT-X-PLAYLIST-TYPE") {
            out.push_str(if done { "#EXT-X-PLAYLIST-TYPE:VOD" } else { "#EXT-X-PLAYLIST-TYPE:EVENT" });
            out.push('\n');
            if let Some(ms) = start {
                out.push_str(&format!("#EXT-X-START:TIME-OFFSET={},PRECISE=YES\n", seconds(ms)));
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// File names a client may request from a variant directory.
pub fn valid_segment_name(name: &str) -> bool {
    name == "init.mp4"
        || (name.starts_with("seg_")
            && name.ends_with(".m4s")
            && name.len() > "seg_.m4s".len()
            && name.len() <= 20
            && name["seg_".len()..name.len() - 4].bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secretary_player::playback::{plan, Client, Probe};

    #[test]
    fn variant_names_round_trip_and_reject_junk() {
        let probe = Probe { container: "matroska".into(), video_codec: Some("hevc".into()), audio_codec: Some("dts".into()), ..Default::default() };
        let p = plan(&probe, Client::Ios);
        let name = variant_name(&p);
        assert_eq!(name, "copy-aac-hvc1");
        assert_eq!(parse_variant(&name), Some(p));
        for bad in ["copy", "copy-copy-x", "h264-h264", "aac-copy", "copy-aac-hvc1-hvc1", "../x", ""] {
            assert_eq!(parse_variant(bad), None, "{bad}");
        }
    }

    fn bg(scope: bool, vaapi: Option<&str>) -> BackgroundConfig {
        BackgroundConfig {
            scope,
            cpu_quota: 100,
            vaapi_device: vaapi.map(PathBuf::from),
            gpu_lease: vaapi.map(|_| PathBuf::from("/usr/bin/gpu-lease")),
        }
    }

    #[test]
    fn ffmpeg_args_for_transcode() {
        let p = parse_variant("h264-copy").unwrap();
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Interactive,
            SEGMENT_SECONDS,
            &BackgroundConfig::default(),
            false,
        );
        let joined = args.join(" ");
        assert!(joined.contains("-i /m/up.mkv -map 0:v:0 -map 0:a:0 -vf"));
        assert!(joined.contains("-c:v libx264"));
        assert!(joined.contains("-force_key_frames expr:gte(t,n_forced*2)"));
        assert!(joined.contains("-hls_time 2 "));
        assert!(joined.contains("-c:a copy"));
        assert!(joined.ends_with("/c/x/seg_%05d.m4s /c/x/index.m3u8"));
    }

    #[test]
    fn interactive_jobs_are_untouched() {
        let p = parse_variant("h264-aac").unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let fg = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Interactive,
            SEGMENT_SECONDS,
            &bg(true, Some("/dev/dri/renderD128")),
            false,
        );
        assert!(!fg.iter().any(|a| a == "-threads"));
        assert!(!fg.iter().any(|a| a == "-vaapi_device"));
        let (program, args) = command_line(ffmpeg, fg.clone(), Priority::Interactive, &bg(true, None), false);
        assert_eq!((program.as_path(), args), (ffmpeg, fg));
    }

    #[test]
    fn vaapi_background_jobs_run_in_a_scope_under_a_lease() {
        let p = parse_variant("h264-aac").unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let background = bg(true, Some("/dev/dri/renderD128"));
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Background,
            SEGMENT_SECONDS,
            &background,
            true,
        );
        let joined = args.join(" ");
        assert!(joined.contains("-threads 2 -i /m/up.mkv"), "{joined}");
        assert!(joined.contains("-vaapi_device /dev/dri/renderD128"), "{joined}");
        assert!(joined.contains("-vf scale=trunc(iw/2)*2:trunc(ih/2)*2,format=nv12,hwupload"), "{joined}");
        assert!(joined.contains("-c:v h264_vaapi -qp 21 -profile:v high -bf 0"), "{joined}");
        assert!(!joined.contains("libx264"), "{joined}");
        let (program, command) = command_line(ffmpeg, args, Priority::Background, &background, true);
        assert_eq!(program, Path::new("systemd-run"));
        assert_eq!(command[..7], ["--user", "--scope", "--quiet", "-p", "CPUQuota=100%", "--", "/usr/bin/gpu-lease"]);
        assert!(
            command.join(" ").contains("/usr/bin/gpu-lease run --name media-prepare --mb 512 -- nice -n 19 /usr/bin/ffmpeg"),
            "{command:?}"
        );
    }

    #[test]
    fn cpu_background_jobs_cap_threads_and_tone_map() {
        let p = parse_variant("h264-copy-tm").unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let background = bg(true, Some("/dev/dri/renderD128"));
        // A tone-mapping job cannot use VAAPI, so it stays on libx264 in the scope.
        assert!(!uses_vaapi(&p, &background, true));
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Background,
            SEGMENT_SECONDS,
            &background,
            false,
        );
        let joined = args.join(" ");
        assert!(joined.contains("-threads 2 -i /m/up.mkv"), "{joined}");
        assert!(joined.contains("-c:v libx264"), "{joined}");
        assert_eq!(joined.matches("-threads 2").count(), 2, "{joined}");
        assert!(joined.contains("zscale=t=linear"), "{joined}");
        let (program, command) = command_line(ffmpeg, args, Priority::Background, &background, false);
        assert_eq!(program, Path::new("systemd-run"));
        assert!(command.join(" ").contains("nice -n 19 /usr/bin/ffmpeg"), "{command:?}");
        assert!(!command.join(" ").contains("gpu-lease"), "{command:?}");
    }

    #[test]
    fn remux_background_jobs_run_in_the_scope_without_an_encoder() {
        let p = parse_variant("copy-copy").unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let background = bg(true, None);
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Background,
            SEGMENT_SECONDS,
            &background,
            false,
        );
        let joined = args.join(" ");
        assert!(joined.contains("-c:v copy"), "{joined}");
        assert!(!joined.contains("-threads"), "{joined}");
        assert!(!joined.contains("libx264"), "{joined}");
        let (program, command) = command_line(ffmpeg, args, Priority::Background, &background, false);
        assert_eq!(program, Path::new("systemd-run"));
        assert_eq!(command[..7], ["--user", "--scope", "--quiet", "-p", "CPUQuota=100%", "--", "nice"]);
    }

    #[test]
    fn without_a_scope_a_background_job_is_only_niced() {
        let p = parse_variant("h264-aac").unwrap();
        let ffmpeg = Path::new("/usr/bin/ffmpeg");
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &p,
            Path::new("/c/x"),
            Priority::Background,
            SEGMENT_SECONDS,
            &BackgroundConfig::default(),
            false,
        );
        let (program, command) = command_line(ffmpeg, args, Priority::Background, &BackgroundConfig::default(), false);
        assert_eq!(program, Path::new("nice"));
        assert_eq!(command[..3], ["-n", "19", "/usr/bin/ffmpeg"]);
    }

    const RUNNING: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2.000000,\nseg_00000.m4s\n#EXTINF:2.000000,\nseg_00001.m4s\n#EXTINF:1.500000,\nseg_00002.m4s\n";

    #[test]
    fn playlist_rewrite() {
        let running = rewrite_playlist(RUNNING, false, None);
        assert!(running.contains("#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-START:TIME-OFFSET=0,PRECISE=YES\n"));
        let done = format!("{RUNNING}#EXT-X-ENDLIST\n");
        let vod = rewrite_playlist(&done, true, None);
        assert!(vod.contains("#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP"));
        assert!(!vod.contains("EXT-X-START"));
        assert!(vod.ends_with("seg_00002.m4s\n#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn playlist_rewrite_starts_where_the_viewer_resumes() {
        let done = format!("{RUNNING}#EXT-X-ENDLIST\n");
        let start = |ms| {
            let text = rewrite_playlist(&done, true, Some(ms));
            text.lines().find(|l| l.starts_with("#EXT-X-START")).map(str::to_string)
        };
        assert_eq!(start(2_500).as_deref(), Some("#EXT-X-START:TIME-OFFSET=2.5,PRECISE=YES"));
        assert_eq!(start(1_250).as_deref(), Some("#EXT-X-START:TIME-OFFSET=1.25,PRECISE=YES"));
        // The playlist is 5.5 s long: never in its last second, never before 0.
        assert_eq!(start(60_000).as_deref(), Some("#EXT-X-START:TIME-OFFSET=4.5,PRECISE=YES"));
        assert_eq!(start(-5).as_deref(), Some("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES"));
        // A running job keeps starting at 0: the resume point may not be written yet.
        assert!(rewrite_playlist(RUNNING, false, Some(2_500)).contains("TIME-OFFSET=0,PRECISE=YES"));
        // No segments, no length: no start.
        let empty = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-ENDLIST\n";
        assert!(!rewrite_playlist(empty, true, Some(2_500)).contains("EXT-X-START"));
    }

    #[test]
    fn start_offset_is_clamped() {
        assert_eq!(start_offset_ms(0, Some(100_000)), Some(0));
        assert_eq!(start_offset_ms(-1, Some(100_000)), Some(0));
        assert_eq!(start_offset_ms(i64::MIN, Some(100_000)), Some(0));
        assert_eq!(start_offset_ms(42_000, Some(100_000)), Some(42_000));
        assert_eq!(start_offset_ms(99_000, Some(100_000)), Some(99_000));
        assert_eq!(start_offset_ms(99_001, Some(100_000)), Some(99_000));
        assert_eq!(start_offset_ms(i64::MAX, Some(100_000)), Some(99_000));
        // Shorter than a second: only the start.
        assert_eq!(start_offset_ms(500, Some(800)), Some(0));
        assert_eq!(start_offset_ms(500, Some(0)), Some(0));
        assert_eq!(start_offset_ms(42_000, None), None);
    }

    #[test]
    fn start_ms_is_parsed_strictly() {
        assert_eq!(parse_start_ms("754000"), Some(754_000));
        assert_eq!(parse_start_ms("0"), Some(0));
        assert_eq!(parse_start_ms("-20"), Some(-20));
        for bad in ["", "-", "+5", "1.5", "1e3", " 5", "5s", "99999999999999999999"] {
            assert_eq!(parse_start_ms(bad), None, "{bad}");
        }
    }

    /// MP4 boxes: `size kind body`.
    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    /// An init with one track whose `stsd` holds `entries` sample entries.
    fn init_with(entries: u32) -> Vec<u8> {
        let mut stsd = vec![0, 0, 0, 0];
        stsd.extend_from_slice(&entries.to_be_bytes());
        for _ in 0..entries {
            stsd.extend_from_slice(&mp4_box(b"avc1", &[0; 8]));
        }
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd));
        let trak = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &stbl)));
        let mut init = mp4_box(b"ftyp", b"iso5");
        init.extend(mp4_box(b"moov", &trak));
        init
    }

    #[test]
    fn an_init_needs_a_sample_entry_in_every_track() {
        assert!(init_is_complete(&init_with(1)));
        // What ffmpeg wrote for "Up" with 2 s segments: an stsd with no entry.
        assert!(!init_is_complete(&init_with(0)));
        assert!(!init_is_complete(&mp4_box(b"ftyp", b"iso5")), "no moov");
        assert!(!init_is_complete(&[0, 0, 0, 4]), "a truncated box");
        assert!(!init_is_complete(b""));
        let mut cut = init_with(1);
        cut.truncate(cut.len() - 3);
        assert!(!init_is_complete(&cut));
    }

    #[test]
    fn copied_audio_that_starts_late_gets_longer_segments() {
        let copy = parse_variant("h264-copy").unwrap();
        let aac = parse_variant("h264-aac").unwrap();
        assert_eq!(segment_seconds(&copy, Some(0)), 2);
        assert_eq!(segment_seconds(&copy, Some(1000)), 2);
        // "Up": the AC-3 starts 5.8 s in; the first cut has to come after it.
        assert_eq!(segment_seconds(&copy, Some(5800)), 8);
        assert_eq!(segment_seconds(&copy, Some(1001)), 4);
        // Unknown: the 6 s every earlier output used.
        assert_eq!(segment_seconds(&copy, None), 6);
        // Re-encoded audio carries its own setup: short segments whatever the lead.
        assert_eq!(segment_seconds(&aac, Some(5800)), 2);
        assert_eq!(segment_seconds(&aac, None), 2);
        let args = ffmpeg_args(
            Path::new("/m/up.mkv"),
            &copy,
            Path::new("/c/x"),
            Priority::Interactive,
            8,
            &BackgroundConfig::default(),
            false,
        )
        .join(" ");
        assert!(args.contains("-hls_time 8 ") && args.contains("n_forced*8"), "{args}");
    }

    /// A fake ffmpeg: a background run (it has `-threads`) marks itself started
    /// and runs for a while; a viewer's run writes a complete init and its first segment at once.
    fn fake_ffmpeg(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("init.ok"), init_with(1)).unwrap();
        let path = dir.join("ffmpeg");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nfor a; do last=$a; case $a in -threads) bg=1;; esac; done\n\
                 if [ -n \"$bg\" ]; then touch \"$(dirname \"$last\")/started\"; exec sleep 30; fi\n\
                 cp {} \"$(dirname \"$last\")/init.mp4\"\n\
                 printf '#EXTM3U\\n#EXTINF:2.0,\\nseg_00000.m4s\\n' > \"$last\"\n",
                dir.join("init.ok").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_background_job_never_holds_up_a_viewer() {
        let dir = tempfile::tempdir().unwrap();
        // One viewer slot: if the background job took it, the viewer would time out.
        let jobs = Arc::new(Jobs::new(fake_ffmpeg(dir.path()), 1, BackgroundConfig::default()));
        let plan = parse_variant("h264-aac").unwrap();
        let (bg_out, fg_out) = (dir.path().join("bg"), dir.path().join("fg"));
        let src = PathBuf::from("/m/a.mkv");
        let bg = {
            let (jobs, plan, src, out) = (Arc::clone(&jobs), plan.clone(), src.clone(), bg_out.clone());
            tokio::spawn(async move { jobs.prepare(&src, &plan, &out, SEGMENT_SECONDS).await })
        };
        for _ in 0..100 {
            if bg_out.join("started").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(bg_out.join("started").exists(), "background job did not start");
        assert!(jobs.is_running(&bg_out));

        jobs.ensure(src.clone(), plan.clone(), fg_out.clone(), SEGMENT_SECONDS).unwrap();
        jobs.wait_ready(&fg_out, Duration::from_secs(5)).await.unwrap();

        // The same variant as the background job: served from it, not started again.
        jobs.ensure(src, plan, bg_out.clone(), SEGMENT_SECONDS).unwrap();
        assert!(jobs.is_running(&bg_out));
        assert!(!bg.is_finished());
        bg.abort();
    }

    #[tokio::test]
    async fn a_viewers_failed_run_is_a_failure_for_the_worker_too() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = Arc::new(Jobs::new(dir.path().join("no-ffmpeg"), 1, BackgroundConfig::default()));
        let (plan, out, src) = (parse_variant("h264-aac").unwrap(), dir.path().join("x"), PathBuf::from("/m/a.mkv"));
        jobs.ensure(src.clone(), plan.clone(), out.clone(), SEGMENT_SECONDS).unwrap();
        assert!(jobs.wait_ready(&out, Duration::from_secs(5)).await.is_err());
        // Not "nothing to do": the worker would pick the same output again at once.
        assert!(jobs.prepare(&src, &plan, &out, SEGMENT_SECONDS).await.is_err());
        // The viewer still gets their failure once, then a retry.
        assert!(jobs.ensure(src.clone(), plan.clone(), out.clone(), SEGMENT_SECONDS).is_err());
    }

    #[test]
    fn segment_names() {
        assert!(valid_segment_name("init.mp4"));
        assert!(valid_segment_name("seg_00042.m4s"));
        for bad in ["seg_.m4s", "seg_1a.m4s", "../init.mp4", "index.m3u8", "seg_0000000000000001.m4s"] {
            assert!(!valid_segment_name(bad), "{bad}");
        }
    }
}
