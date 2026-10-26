//! Background preparation: every film gets a finished HLS output for the
//! variants the apps play, so it starts at once, and a resume starts at its own
//! segment, instead of waiting for ffmpeg to get there.
//!
//! Only the film folders (`scope=films`) are prepared: phone videos are not.
//! One job at a time, in its own slot pool, so a viewer never waits behind it.
//! The queue is read again after every job: a film the owner starts watching,
//! or one just added, moves to the front.
//!
//! Where the CPU is protected, in order:
//! - `systemd-run --user --scope -p CPUQuota=…` caps a background job at one core
//!   (config: `background_scope`, `background_cpu_quota`); viewers' jobs are untouched.
//! - Re-encodes use VAAPI under `gpu-lease` when a render node is configured;
//!   otherwise libx264 with a two-thread decode and encode.
//! - libx264 and tone-mapping wait for the night window (`prepare_window_*`).
//!   VAAPI and remux jobs may start any hour.
//! - Nothing starts at or above `prepare_max_temp_c` (k10temp Tctl) or
//!   `prepare_max_load`. A running job is stopped after three over-limit samples
//!   (`max_temp_c + 5` or max load), its output removed, and it goes back in the
//!   queue as paused, not failed.
use crate::{
    gate::{self, Gate},
    hls, kernel, probe, App, MediaSource, Video,
};
use secretary_player::{
    playback::{self, Client, Mode, Plan, Probe},
    progress::{resume_point, Entry},
};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

/// The players whose variants are prepared: the iPhone and the web (Chrome).
pub const CLIENTS: [Client; 2] = [Client::Ios, Client::WebChrome];

/// A timed scan, for anything the library does not announce.
const RESCAN: Duration = Duration::from_secs(5 * 60);

/// After the library announces an item: let its ingest settle (move, probe,
/// thumbnails) before reading the file again.
const SETTLE: Duration = Duration::from_secs(10);

/// The distinct HLS variants the clients play for a file. Direct play needs none.
pub fn variants(probe: &Probe) -> Vec<Plan> {
    let mut out: Vec<Plan> = Vec::new();
    for client in CLIENTS {
        let plan = playback::plan(probe, client);
        if plan.mode != Mode::Direct && !out.contains(&plan) {
            out.push(plan);
        }
    }
    out
}

/// Where a video goes in the queue (see `kernel::prepare_before`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rank {
    /// The owner is part way through it: they are likely to come back to it next.
    pub unfinished: bool,
    /// The position's `updated_at` when unfinished, else the item's `added_at`.
    pub recency: u64,
}

pub fn rank(video: &Video, progress: Option<&Entry>) -> Rank {
    match progress {
        // The same rule /play uses to resume: a position it would not resume from is no reason to hurry.
        Some(e) if resume_point(Some(e), None) > 0 => Rank { unfinished: true, recency: e.updated_at },
        _ => Rank { unfinished: false, recency: video.added_at },
    }
}

/// The verified preorder as an `Ordering` for `sort_by`.
pub fn compare(a: Rank, b: Rank) -> Ordering {
    let ab = kernel::prepare_before(a.unfinished, a.recency, b.unfinished, b.recency);
    let ba = kernel::prepare_before(b.unfinished, b.recency, a.unfinished, a.recency);
    match (ab, ba) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        _ => Ordering::Greater,
    }
}

/// Videos in preparation order: unfinished positions first (the latest
/// watched first), then the newest added. The sort is stable.
pub fn order(videos: Vec<Video>, progress: &HashMap<String, Entry>) -> Vec<Video> {
    let mut ranked: Vec<(Rank, Video)> = videos.into_iter().map(|v| (rank(&v, progress.get(&v.item.id)), v)).collect();
    ranked.sort_by(|a, b| compare(a.0, b.0));
    ranked.into_iter().map(|(_, v)| v).collect()
}

struct Job {
    src: PathBuf,
    plan: Plan,
    out: PathBuf,
    segment: u32,
    /// Its VAAPI run failed: retry once on the CPU path, in the night window.
    cpu_only: bool,
}

/// Outputs still to make, in order, and how many are already finished.
async fn pending<S: MediaSource>(app: &App<S>, failed: &HashSet<PathBuf>, cpu_only: &HashSet<PathBuf>) -> (Vec<Job>, usize) {
    let progress: HashMap<String, Entry> = app.progress.since(0).await.into_iter().map(|e| (e.id.clone(), e)).collect();
    let (mut jobs, mut done) = (Vec::new(), 0);
    for video in order(app.src.videos().await, &progress) {
        let info = match probe::probe(&app.cfg.ffprobe, &app.cfg.cache_dir, &video.item.path).await {
            Ok(info) => info,
            Err(e) => {
                tracing::debug!(id = %video.item.id, "prepare: probe failed: {e}");
                continue;
            }
        };
        for plan in variants(&info.probe) {
            let Ok(out) = app.hls_dir(&video.item, &hls::variant_name(&plan)) else { continue };
            if out.join(hls::DONE_MARKER).exists() {
                done += 1;
            } else if !failed.contains(&out) && !app.jobs.is_running(&out) {
                let segment = hls::segment_seconds(&plan, info.audio_lead_ms);
                let cpu_only = cpu_only.contains(&out);
                jobs.push(Job { src: video.item.path.clone(), plan, out, segment, cpu_only });
            }
        }
    }
    (jobs, done)
}

/// The worker: one job, then a fresh look at the queue; a nap when it is empty,
/// and a minute's wait while the gate is closed.
pub(crate) async fn run<S: MediaSource>(app: Arc<App<S>>) {
    // Outputs that failed since the start: tried again after a restart (or when the file changes,
    // which changes its cache key), not every few minutes.
    let mut failed: HashSet<PathBuf> = HashSet::new();
    // Outputs whose VAAPI run failed: once on the CPU path, never VAAPI again.
    let mut cpu_only: HashSet<PathBuf> = HashSet::new();
    let background = hls::BackgroundConfig::from(&app.cfg);
    let gate = Gate::from(&app.cfg);
    gate.log_missing();
    // Log a pause when it starts (or its reason changes), not every minute.
    let mut paused: Option<String> = None;
    loop {
        let (jobs, done) = pending(&app, &failed, &cpu_only).await;
        tracing::info!("prepare queued={} done={done}", jobs.len());
        if jobs.is_empty() {
            paused = None;
            tokio::select! {
                _ = tokio::time::sleep(RESCAN) => {}
                _ = app.src.changed() => tokio::time::sleep(SETTLE).await,
            }
            continue;
        }
        let cpu_path = |job: &Job| hls::is_cpu_path(&job.plan, &background, !job.cpu_only);
        let (chosen, blocked) = gate::choose(&jobs, &gate, cpu_path);
        let Some(index) = chosen else {
            if let Some(reason) = blocked {
                if paused.as_deref() != Some(reason.as_str()) {
                    tracing::info!("prepare paused: {reason}");
                    paused = Some(reason);
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(gate::PAUSE_RECHECK) => {}
                _ = app.src.changed() => tokio::time::sleep(SETTLE).await,
            }
            continue;
        };
        paused = None;
        let job = jobs.into_iter().nth(index).expect("chosen index exists");
        let allow_vaapi = !job.cpu_only;
        match app
            .jobs
            .prepare_watched(&job.src, &job.plan, &job.out, job.segment, allow_vaapi, &gate)
            .await
        {
            Ok(_) => {
                cpu_only.remove(&job.out);
            }
            // Paused, not failed: the output is gone and the next loop queues it again.
            Err(hls::JobError::Stopped(reason)) => {
                tracing::info!(src = %job.src.display(), variant = %hls::variant_name(&job.plan), "prepare stopped: {reason}");
            }
            Err(hls::JobError::Failed(e)) => {
                if hls::uses_vaapi(&job.plan, &background, allow_vaapi) {
                    tracing::warn!(
                        src = %job.src.display(),
                        "prepare: VAAPI failed, retrying on the CPU path in the night window: {e}"
                    );
                    cpu_only.insert(job.out);
                } else {
                    tracing::warn!(src = %job.src.display(), variant = %hls::variant_name(&job.plan), "prepare failed: {e}");
                    failed.insert(job.out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceItem;

    fn probe(container: &str, video: &str, audio: &str) -> Probe {
        Probe { container: container.into(), video_codec: Some(video.into()), audio_codec: Some(audio.into()), ..Default::default() }
    }

    fn names(probe: &Probe) -> Vec<String> {
        variants(probe).iter().map(hls::variant_name).collect()
    }

    #[test]
    fn variants_for_the_iphone_and_the_web() {
        // "Up": a DVD rip, MPEG-2 and AC-3. Both re-encode the video; only the web the audio.
        assert_eq!(names(&probe("mpeg", "mpeg2video", "ac3")), ["h264-copy", "h264-aac"]);
        // H.264 + AC-3 in MKV: the iPhone remuxes, the web needs AAC.
        assert_eq!(names(&probe("matroska", "h264", "ac3")), ["copy-copy", "copy-aac"]);
        // H.264 + AAC in MKV: one remux serves both.
        assert_eq!(names(&probe("matroska", "h264", "aac")), ["copy-copy"]);
        // An MP4 both play as it is: nothing to prepare.
        assert!(names(&probe("mov,mp4,m4a,3gp,3g2,mj2", "h264", "aac")).is_empty());
        // HEVC in MP4: direct on the iPhone, transcoded for the web.
        assert_eq!(names(&probe("mov,mp4,m4a,3gp,3g2,mj2", "hevc", "aac")), ["h264-copy"]);
    }

    fn video(id: &str, added_at: u64) -> Video {
        Video {
            item: SourceItem {
                id: id.into(),
                kind: "video".into(),
                title: id.into(),
                path: format!("/m/{id}.mkv").into(),
                mime: "video/x-matroska".into(),
                poster: None,
                thumb: None,
                subtitles: vec![],
            },
            added_at,
        }
    }

    fn entry(id: &str, position_ms: u64, finished: bool, updated_at: u64) -> (String, Entry) {
        (id.into(), Entry { id: id.into(), position_ms, duration_ms: 6_000_000, finished, updated_at, device: None })
    }

    #[test]
    fn unfinished_first_latest_watched_first_then_newest_added() {
        let videos = vec![video("old", 1), video("new", 9), video("watched-long-ago", 2), video("watching", 3), video("done", 8), video("barely", 7), video("mid", 5)];
        let progress: HashMap<String, Entry> = [
            entry("watched-long-ago", 1_200_000, false, 100),
            entry("watching", 2_400_000, false, 500),
            // Finished, or only a few seconds in: nothing to resume, so ranked by age like the rest.
            entry("done", 6_000_000, true, 900),
            entry("barely", 3_000, false, 950),
        ]
        .into_iter()
        .collect();
        let ids: Vec<String> = order(videos, &progress).into_iter().map(|v| v.item.id).collect();
        assert_eq!(ids, ["watching", "watched-long-ago", "new", "done", "barely", "mid", "old"]);
    }

    #[test]
    fn compare_is_a_total_preorder_on_samples() {
        let ranks: Vec<Rank> = [false, true].into_iter().flat_map(|u| [0, 1, 5].map(|r| Rank { unfinished: u, recency: r })).collect();
        for &a in &ranks {
            assert_eq!(compare(a, a), Ordering::Equal);
            for &b in &ranks {
                assert_eq!(compare(a, b), compare(b, a).reverse());
                for &c in &ranks {
                    if compare(a, b) != Ordering::Greater && compare(b, c) != Ordering::Greater {
                        assert_ne!(compare(a, c), Ordering::Greater);
                    }
                }
            }
        }
    }
}
