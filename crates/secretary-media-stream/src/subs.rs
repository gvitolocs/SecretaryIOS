//! Every subtitle track of an item, and their conversion to WebVTT.
//!
//! Tracks: the library's files plus the files found next to the video
//! (`sidecar`), then the tracks inside the file. Text tracks convert in
//! seconds (embedded ones need a full read of the file); image tracks (DVD,
//! Blu-ray, `.idx`/`.sup`) go through OCR. Conversions run in the background,
//! one per track, and are cached; a request that arrives before the result
//! gets "preparing" and asks again.
use crate::{ocr, probe, sidecar, SourceItem};
use secretary_player::subtitles;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    File(PathBuf),
    /// ffmpeg stream index in the video file.
    Embedded(u32),
}

#[derive(Debug, Clone)]
pub struct Track {
    /// `x<n>` for files, `e<stream index>` for embedded tracks.
    pub id: String,
    pub lang: Option<String>,
    pub label: String,
    pub forced: bool,
    /// A picture track, shown through OCR.
    pub image: bool,
    /// `srt`, `ass`, `vobsub`, `pgs`, `subrip`, `dvd_subtitle`...
    pub format: String,
    pub origin: Origin,
    /// Canvas of an image track, when known.
    pub size: Option<(u32, u32)>,
}

fn image_codec(codec: &str) -> bool {
    matches!(codec, "dvd_subtitle" | "hdmv_pgs_subtitle" | "dvb_subtitle" | "xsub")
}

/// `size: 720x576` from a VobSub index.
fn idx_size(path: &Path) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().find(|l| l.starts_with("size:"))?;
    let (w, h) = line["size:".len()..].trim().split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The item's tracks in display order. Image tracks are left out when OCR
/// is not available; `hidden` counts them.
pub fn tracks(item: &SourceItem, info: &probe::Info, ocr: bool) -> (Vec<Track>, usize) {
    let mut out = Vec::new();
    let mut hidden = 0;
    for (i, s) in sidecar::merge(&item.subtitles, &item.path).into_iter().enumerate() {
        let ext = sidecar::ext_of(&s.path);
        let image = sidecar::is_image(&s.path);
        if image && !ocr {
            hidden += 1;
            continue;
        }
        let size = match ext.as_str() {
            "idx" => idx_size(&s.path),
            _ => None,
        };
        out.push(Track {
            id: format!("x{i}"),
            forced: sidecar::forced(&s.path),
            lang: s.lang,
            label: s.label,
            image,
            format: match ext.as_str() {
                "idx" => "vobsub".into(),
                "sup" => "pgs".into(),
                other => other.to_string(),
            },
            origin: Origin::File(s.path),
            size,
        });
    }
    let mut embedded: Vec<Track> = Vec::new();
    for s in &info.subtitles {
        let image = image_codec(&s.codec);
        if !s.text && !image {
            continue;
        }
        if image && !ocr {
            hidden += 1;
            continue;
        }
        embedded.push(Track {
            id: format!("e{}", s.index),
            lang: s.lang.clone(),
            label: s.title.clone().or_else(|| s.lang.clone()).unwrap_or_else(|| format!("Track {}", s.index)),
            forced: s.forced,
            image,
            format: s.codec.clone(),
            origin: Origin::Embedded(s.index),
            size: s.width.zip(s.height),
        });
    }
    // Text before pictures inside the file too.
    embedded.sort_by_key(|t| t.image);
    out.extend(embedded);
    (out, hidden)
}

/// What a conversion needs besides the track.
#[derive(Debug, Clone)]
pub struct Converter {
    pub ffmpeg: PathBuf,
    pub ocr: Option<ocr::OcrConfig>,
    pub cache_dir: PathBuf,
}

impl Converter {
    /// Cache file of a track: by the subtitle file's own key for files (so
    /// new files in the folder do not shift it), by video key and stream for
    /// embedded tracks.
    pub fn cache_path(&self, item: &SourceItem, track: &Track) -> Result<PathBuf, String> {
        let name = match &track.origin {
            Origin::File(p) => {
                let key = probe::file_key(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let mut h = Sha256::new();
                h.update(key.as_bytes());
                format!("file-{}.vtt", probe::hex(&h.finalize()[..12]))
            }
            Origin::Embedded(i) => {
                let key = probe::file_key(&item.path).map_err(|e| format!("{}: {e}", item.path.display()))?;
                format!("{key}-e{i}.vtt")
            }
        };
        Ok(self.cache_dir.join("subs").join(name))
    }

    async fn convert(&self, item: &SourceItem, track: &Track, video_size: Option<(u32, u32)>, duration_ms: u64, work: PathBuf) -> Result<String, String> {
        if track.image {
            let ocr = self.ocr.clone().ok_or("OCR is not available on this server")?;
            let (input, spec) = match &track.origin {
                Origin::File(p) => (p.clone(), "0:s:0".to_string()),
                Origin::Embedded(i) => (item.path.clone(), format!("0:{i}")),
            };
            let size = track.size.or(video_size).unwrap_or((720, 576));
            let (ffmpeg, lang) = (self.ffmpeg.clone(), track.lang.clone());
            let partial = partial_path(&self.cache_path(item, track)?);
            let cues = tokio::task::spawn_blocking(move || {
                let src = ocr::Source { input: &input, spec: &spec, size, duration_ms, lang: lang.as_deref() };
                // What is read so far, for a player that starts before OCR ends.
                let mut progress = |cues: &[subtitles::Cue]| {
                    let tmp = partial.with_extension("tmp");
                    if std::fs::write(&tmp, subtitles::to_vtt(cues)).is_ok() {
                        let _ = std::fs::rename(&tmp, &partial);
                    }
                };
                ocr::read_track(&ffmpeg, &ocr, &src, &work, &mut progress)
            })
            .await
            .map_err(|e| e.to_string())??;
            return Ok(subtitles::to_vtt(&cues));
        }
        match &track.origin {
            Origin::File(p) if matches!(track.format.as_str(), "srt" | "vtt") => {
                let bytes = tokio::fs::read(p).await.map_err(|e| e.to_string())?;
                Ok(subtitles::to_vtt(&subtitles::parse(&decode_text(&bytes))))
            }
            Origin::File(p) => ffmpeg_vtt(&self.ffmpeg, p, None).await,
            Origin::Embedded(i) => ffmpeg_vtt(&self.ffmpeg, &item.path, Some(*i)).await,
        }
    }
}

/// Decodes subtitle bytes: UTF-8 (with or without BOM), else Windows-1252,
/// which is what most European .srt files without a BOM are.
pub fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0x80 => '€',
                0x8a => 'Š',
                0x8c => 'Œ',
                0x8e => 'Ž',
                0x91 | 0x92 => '\'',
                0x93 | 0x94 => '"',
                0x96 | 0x97 => '-',
                0x9a => 'š',
                0x9c => 'œ',
                0x9e => 'ž',
                0x9f => 'Ÿ',
                b => b as char,
            })
            .collect(),
    }
}

async fn ffmpeg_vtt(ffmpeg: &Path, path: &Path, stream: Option<u32>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"]).arg(path);
    if let Some(i) = stream {
        cmd.arg("-map").arg(format!("0:{i}"));
    }
    cmd.args(["-f", "webvtt", "pipe:1"]);
    let out = cmd.kill_on_drop(true).output().await.map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // Re-parse to drop ASS styling ffmpeg leaves in.
    Ok(subtitles::to_vtt(&subtitles::parse(&String::from_utf8_lossy(&out.stdout))))
}

/// Cues OCR'd so far, next to the final cache file.
fn partial_path(cache: &Path) -> PathBuf {
    cache.with_extension("partial.vtt")
}

/// A track's WebVTT: all of it, or the part an OCR run has read so far.
pub enum Vtt {
    Complete(String),
    Partial(String),
}

enum JobState {
    Running,
    Failed(String),
}

/// Background conversions: one per track, OCR one at a time (it is the
/// expensive one).
pub struct Jobs {
    conv: Converter,
    state: Mutex<HashMap<PathBuf, JobState>>,
    ocr_slots: Arc<Semaphore>,
}

impl Jobs {
    pub fn new(conv: Converter) -> Self {
        Self { conv, state: Mutex::new(HashMap::new()), ocr_slots: Arc::new(Semaphore::new(1)) }
    }

    pub fn ocr_available(&self) -> bool {
        self.conv.ocr.is_some()
    }

    /// The track as WebVTT, or None while nothing is ready after waiting
    /// `wait`. A failed run is reported once; the next call retries.
    pub async fn get(
        self: &Arc<Self>,
        item: &SourceItem,
        track: &Track,
        video_size: Option<(u32, u32)>,
        duration_ms: u64,
        wait: Duration,
    ) -> Result<Option<Vtt>, String> {
        let cache = self.conv.cache_path(item, track)?;
        if let Ok(text) = tokio::fs::read_to_string(&cache).await {
            return Ok(Some(Vtt::Complete(text)));
        }
        {
            let mut state = self.state.lock().expect("subtitle jobs");
            match state.get(&cache) {
                Some(JobState::Failed(e)) => {
                    let e = e.clone();
                    state.remove(&cache);
                    return Err(e);
                }
                Some(JobState::Running) => {}
                None => {
                    state.insert(cache.clone(), JobState::Running);
                    let (this, item, track, cache) = (Arc::clone(self), item.clone(), track.clone(), cache.clone());
                    tokio::spawn(async move {
                        let _slot = if track.image { this.ocr_slots.clone().acquire_owned().await.ok() } else { None };
                        let work = cache.with_extension("ocr");
                        let result = this.conv.convert(&item, &track, video_size, duration_ms, work).await;
                        let result = match result {
                            Ok(text) => write_atomic(&cache, &text).await,
                            Err(e) => Err(e),
                        };
                        let _ = tokio::fs::remove_file(partial_path(&cache)).await;
                        let mut state = this.state.lock().expect("subtitle jobs");
                        match result {
                            Ok(()) => {
                                state.remove(&cache);
                            }
                            Err(e) => {
                                tracing::warn!(id = %item.id, track = %track.id, "subtitle conversion failed: {e}");
                                state.insert(cache, JobState::Failed(e));
                            }
                        }
                    });
                }
            }
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&cache).await {
                return Ok(Some(Vtt::Complete(text)));
            }
            if let Some(JobState::Failed(e)) = self.state.lock().expect("subtitle jobs").get(&cache) {
                return Err(e.clone());
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(tokio::fs::read_to_string(partial_path(&cache)).await.ok().map(Vtt::Partial));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

async fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, text).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, path).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{EmbeddedSub, Info};
    use secretary_player::playback::Probe;

    fn sub(index: u32, codec: &str, text: bool, lang: &str) -> EmbeddedSub {
        EmbeddedSub { index, codec: codec.into(), lang: Some(lang.into()), title: None, text, forced: false, width: Some(720), height: Some(576) }
    }

    #[test]
    fn lists_files_then_text_then_pictures() {
        let d = tempfile::tempdir().unwrap();
        for n in ["Up.mkv", "whisper-da.srt", "Up.idx", "Up.sub"] {
            std::fs::write(d.path().join(n), b"size: 720x480\n").unwrap();
        }
        let item = SourceItem {
            id: "a".into(),
            kind: "video".into(),
            title: "Up".into(),
            path: d.path().join("Up.mkv"),
            mime: "video/x-matroska".into(),
            poster: None,
            thumb: None,
            subtitles: vec![],
        };
        let info = Info {
            probe: Probe::default(),
            duration_ms: Some(1000),
            width: Some(720),
            height: Some(576),
            subtitles: vec![sub(2, "dvd_subtitle", false, "dan"), sub(3, "subrip", true, "eng"), sub(4, "eia_608", false, "eng")],
            audio_lead_ms: None,
            version: crate::probe::PROBE_VERSION,
        };
        let (all, hidden) = tracks(&item, &info, true);
        let ids: Vec<(&str, bool)> = all.iter().map(|t| (t.id.as_str(), t.image)).collect();
        assert_eq!(ids, vec![("x0", true), ("x1", false), ("e3", false), ("e2", true)]);
        assert_eq!(hidden, 0);
        assert_eq!(all[0].size, Some((720, 480)));
        assert_eq!(all[0].format, "vobsub");
        assert_eq!(all[1].lang.as_deref(), Some("da"));
        let (text_only, hidden) = tracks(&item, &info, false);
        assert_eq!(text_only.len(), 2);
        assert_eq!(hidden, 2);
    }
}
