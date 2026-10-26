//! HLS output from ffmpeg: fMP4 segments, streams copied when the client can
//! decode them (remux) and re-encoded only when it cannot.
//!
//! One ffmpeg job per (file, variant) runs to the end and its output stays in
//! the cache, so the second play of a film starts at once. While a job runs
//! the playlist is an EVENT playlist that starts at 0; when it finishes it is
//! served as VOD.
use secretary_player::playback::{Plan, Stream};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{process::Command, sync::Semaphore};

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

/// Segment length in seconds.
const SEGMENT_SECONDS: u32 = 6;

pub fn ffmpeg_args(src: &Path, plan: &Plan, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"].map(String::from).to_vec();
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
            // A keyframe at every segment boundary, so segments cut evenly.
            a.push("-force_key_frames".into());
            a.push(format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})"));
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
            &SEGMENT_SECONDS.to_string(),
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

#[derive(Debug, Clone, PartialEq)]
enum JobState {
    Running,
    Failed(String),
}

/// Runs ffmpeg jobs, at most `limit` at once.
pub struct Jobs {
    ffmpeg: PathBuf,
    slots: Arc<Semaphore>,
    jobs: Mutex<HashMap<PathBuf, JobState>>,
}

pub const DONE_MARKER: &str = "done";

impl Jobs {
    pub fn new(ffmpeg: PathBuf, limit: usize) -> Self {
        Self { ffmpeg, slots: Arc::new(Semaphore::new(limit.max(1))), jobs: Mutex::new(HashMap::new()) }
    }

    /// Starts the job for `out` unless it is finished or running. Returns an
    /// error when an earlier run failed (the next call retries).
    pub fn ensure(self: &Arc<Self>, src: PathBuf, plan: Plan, out: PathBuf) -> Result<(), String> {
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
            let result = this.run(&src, &plan, &out).await;
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

    async fn run(&self, src: &Path, plan: &Plan, out: &Path) -> Result<(), String> {
        let _slot = self.slots.acquire().await.map_err(|e| e.to_string())?;
        // Start clean: a half-written run from before a restart is not resumable.
        let _ = tokio::fs::remove_dir_all(out).await;
        tokio::fs::create_dir_all(out).await.map_err(|e| e.to_string())?;
        let args = ffmpeg_args(src, plan, out);
        tracing::info!(src = %src.display(), variant = %variant_name(plan), "hls job started");
        let output = Command::new(&self.ffmpeg).args(&args).kill_on_drop(true).output().await.map_err(|e| format!("ffmpeg: {e}"))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let tail: String = err.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Err(format!("ffmpeg exited with {}: {tail}", output.status));
        }
        tokio::fs::write(out.join(DONE_MARKER), b"").await.map_err(|e| e.to_string())?;
        tracing::info!(src = %src.display(), variant = %variant_name(plan), "hls job finished");
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

/// Rewrites ffmpeg's playlist for the client: a running job's EVENT playlist
/// starts at 0 (AVPlayer would otherwise join at the live edge); a finished
/// one becomes VOD so the whole film is seekable.
pub fn rewrite_playlist(text: &str, done: bool) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        if line.starts_with("#EXT-X-PLAYLIST-TYPE") {
            out.push_str(if done { "#EXT-X-PLAYLIST-TYPE:VOD" } else { "#EXT-X-PLAYLIST-TYPE:EVENT" });
            out.push('\n');
            if !done {
                out.push_str("#EXT-X-START:TIME-OFFSET=0,PRECISE=YES\n");
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

    #[test]
    fn ffmpeg_args_for_transcode() {
        let p = parse_variant("h264-copy").unwrap();
        let args = ffmpeg_args(Path::new("/m/up.mkv"), &p, Path::new("/c/x"));
        let joined = args.join(" ");
        assert!(joined.contains("-i /m/up.mkv -map 0:v:0 -map 0:a:0 -vf"));
        assert!(joined.contains("-c:v libx264"));
        assert!(joined.contains("-c:a copy"));
        assert!(joined.ends_with("/c/x/seg_%05d.m4s /c/x/index.m3u8"));
    }

    #[test]
    fn playlist_rewrite() {
        let src = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg_00000.m4s\n";
        let running = rewrite_playlist(src, false);
        assert!(running.contains("#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-START:TIME-OFFSET=0,PRECISE=YES\n"));
        assert!(rewrite_playlist(&format!("{src}#EXT-X-ENDLIST\n"), true).contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
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
