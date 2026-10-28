//! `lucina-media archive-jxl --dry-run`: measure what the archive format would save, read-only.
//!
//! Opens the catalog read-only, picks a stratified random sample of the owned originals, runs the
//! real conversion on each (with its verification), and writes the per-file records and a summary
//! to a report file. Nothing in the library is written. Records carry opaque ids (12 hex digits of
//! the blob hash), sizes, ratios and timings, never names, paths or places.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use image::DynamicImage;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::archive_jxl::{
    blake3_hex, decide, display_derivative, jpeg_roundtrip, jpeg_to_jxl_at, jxl_to_jpeg,
    load_raster, lossless_roundtrip, raster_to_jxl, ArchiveAction, ConvertError, Format, Timing,
};
use crate::config::Config;
use crate::store;
use crate::thumbs;

/// The CPU temperature (k10temp Tctl) in degrees, when the box has one.
pub fn cpu_temp_c() -> Option<f32> {
    for e in std::fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let p = e.path();
        if std::fs::read_to_string(p.join("name")).is_ok_and(|n| n.trim() == "k10temp") {
            let raw = std::fs::read_to_string(p.join("temp1_input")).ok()?;
            return Some(raw.trim().parse::<f32>().ok()? / 1000.0);
        }
    }
    None
}

/// Block while the CPU is at or above `max_c` (the owner's limit is 90). Returns how many
/// 15-second pauses it took.
pub fn wait_for_cool(max_c: u32) -> u32 {
    let mut pauses = 0;
    while cpu_temp_c().is_some_and(|t| t >= max_c as f32) {
        std::thread::sleep(Duration::from_secs(15));
        pauses += 1;
    }
    pauses
}

// ---- the population ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stratum {
    /// JPEGs from the old backups (phone-archive, archive-nespc, imports).
    JpegArchive,
    /// JPEGs the iPhone uploaded.
    JpegIos,
    /// PNG/TIFF/BMP, any source.
    Lossless,
    /// HEIC/HEIF/RAW, any source.
    HeicRaw,
}

pub const STRATA: [Stratum; 4] = [
    Stratum::JpegArchive,
    Stratum::JpegIos,
    Stratum::Lossless,
    Stratum::HeicRaw,
];

#[derive(Clone, Debug)]
pub struct Candidate {
    pub hash: String,
    pub ext: String,
    pub bytes: u64,
    pub location: String,
    pub item_id: String,
    pub source: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

impl Candidate {
    pub fn opaque_id(&self) -> String {
        self.hash.chars().take(12).collect()
    }
    /// Only blobs the library owns are ever converted; `external` files belong to someone else.
    pub fn owned(&self) -> bool {
        matches!(self.location.as_str(), "store" | "spool")
    }
    pub fn path(&self, cfg: &Config) -> PathBuf {
        let rel = store::object_rel(&self.hash, &self.ext);
        match self.location.as_str() {
            "spool" => cfg.spool_dir().join(rel),
            _ => cfg.store_dir.join(rel),
        }
    }
}

/// The stratum of an owned candidate, from its extension and source (the file is sniffed later).
pub fn stratum_of(c: &Candidate) -> Option<Stratum> {
    if !c.owned() {
        return None;
    }
    match c.ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" => {
            if c.source.starts_with("phone-ios") || c.source.starts_with("ios-") {
                Some(Stratum::JpegIos)
            } else {
                Some(Stratum::JpegArchive)
            }
        }
        "png" | "tif" | "tiff" | "bmp" => Some(Stratum::Lossless),
        "heic" | "heif" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf" | "pef"
        | "srw" => Some(Stratum::HeicRaw),
        _ => None,
    }
}

/// Every photo blob with the first item that uses it, opened read-only.
pub fn open_readonly(db_path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(Duration::from_secs(10))?;
    Ok(c)
}

pub fn population(c: &Connection) -> rusqlite::Result<Vec<Candidate>> {
    let mut st = c.prepare(
        "SELECT b.hash, b.ext, b.bytes, b.location, i.id, i.source, i.width, i.height
         FROM blobs b JOIN items i ON i.hash = b.hash
         WHERE i.kind = 'photo' ORDER BY b.hash, i.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Candidate {
            hash: r.get(0)?,
            ext: r.get(1)?,
            bytes: r.get::<_, i64>(2)? as u64,
            location: r.get(3)?,
            item_id: r.get(4)?,
            source: r.get(5)?,
            width: r.get(6)?,
            height: r.get(7)?,
        })
    })?;
    let mut out: Vec<Candidate> = Vec::new();
    for r in rows {
        let r = r?;
        if out.last().is_none_or(|l| l.hash != r.hash) {
            out.push(r);
        }
    }
    Ok(out)
}

/// The default sample: 300 old-backup JPEGs, 150 iPhone JPEGs, up to 100 lossless-source files
/// and up to 50 HEIC/RAW files (`--sample N` scales these in the same proportions).
pub fn caps_for(total: usize) -> BTreeMap<Stratum, usize> {
    let scale = |n: usize| (total * n).div_ceil(600).max(1);
    BTreeMap::from([
        (Stratum::JpegArchive, scale(300)),
        (Stratum::JpegIos, scale(150)),
        (Stratum::Lossless, scale(100)),
        (Stratum::HeicRaw, scale(50)),
    ])
}

struct SplitMix(u64);
impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// A deterministic random sample per stratum (the same seed gives the same files).
pub fn pick_sample(
    pop: &[Candidate],
    caps: &BTreeMap<Stratum, usize>,
    seed: u64,
) -> Vec<(Stratum, Candidate)> {
    let mut rng = SplitMix(seed);
    let mut out = Vec::new();
    for s in STRATA {
        let mut pool: Vec<&Candidate> = pop.iter().filter(|c| stratum_of(c) == Some(s)).collect();
        let take = caps.get(&s).copied().unwrap_or(0).min(pool.len());
        // Partial Fisher-Yates.
        for i in 0..take {
            let j = i + (rng.next() % (pool.len() - i) as u64) as usize;
            pool.swap(i, j);
        }
        out.extend(pool[..take].iter().map(|c| (s, (*c).clone())));
    }
    out
}

// ---- one file ---------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub stratum: Option<Stratum>,
    pub format: Option<Format>,
    pub action: Option<ArchiveAction>,
    pub orig_bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The archive copy: the transcoded JXL (JPEG) or the lossless JXL (pixels).
    pub jxl_bytes: Option<u64>,
    pub encode: Option<Timing>,
    pub decode: Option<Timing>,
    /// JPEG: the rebuilt file's blake3 equals the original's. Pixels: every sample equal.
    pub verified: bool,
    pub byte_exact: bool,
    pub failure: Option<String>,
    /// JPEG transcode at effort 9 on a subset, to show what the slower setting gains.
    pub jxl_bytes_effort9: Option<u64>,
    pub encode_effort9: Option<Timing>,
    /// HEIC/RAW: where the pixels came from (`original` or `phone_preview`) and what they gave.
    pub pixel_source: Option<String>,
    pub pixel_source_wh: Option<(u32, u32)>,
    /// Lossless JXL of those pixels, scaled to the original's dimensions when they came from the
    /// (smaller) phone preview.
    pub est_lossless_full_bytes: Option<u64>,
    pub display_bytes: Option<u64>,
    pub display_encode: Option<Timing>,
    /// The stored object did not hash to its catalog hash (never expected).
    pub catalog_hash_ok: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MeasureOpts {
    /// Also run the effort-9 transcode on this JPEG (the caller picks a subset).
    pub effort9: bool,
}

pub fn measure_file(
    cfg: &Config,
    cand: &Candidate,
    stratum: Stratum,
    opts: MeasureOpts,
) -> FileRecord {
    let mut rec = FileRecord {
        id: cand.opaque_id(),
        stratum: Some(stratum),
        orig_bytes: cand.bytes,
        width: cand.width.map(|w| w as u32),
        height: cand.height.map(|h| h as u32),
        ..FileRecord::default()
    };
    let path = cand.path(cfg);
    if let Err(e) = measure_into(cfg, cand, &path, opts, &mut rec) {
        rec.verified = false;
        rec.failure = Some(e.reason().to_string());
    }
    rec
}

fn measure_into(
    cfg: &Config,
    cand: &Candidate,
    path: &Path,
    opts: MeasureOpts,
    rec: &mut FileRecord,
) -> Result<(), ConvertError> {
    let mut head = [0u8; 64];
    let n = std::fs::File::open(path)?.read(&mut head)?;
    let d = decide(&cand.ext, &head[..n]);
    rec.format = Some(d.format);
    rec.action = Some(d.action);
    match d.action {
        ArchiveAction::JpegRecompress => {
            let data = std::fs::read(path)?;
            rec.orig_bytes = data.len() as u64;
            rec.catalog_hash_ok = Some(blake3_hex(&data) == cand.hash);
            let c = jpeg_roundtrip(&data)?;
            rec.jxl_bytes = Some(c.jxl.len() as u64);
            rec.encode = Some(c.encode);
            rec.decode = Some(c.decode);
            rec.verified = true;
            rec.byte_exact = true;
            if opts.effort9 {
                let (jxl, t) = crate::archive_jxl::timed(|| {
                    jpeg_to_jxl_at(&data, jpegxl_rs::encode::EncoderSpeed::Tortoise)
                });
                let jxl = jxl?;
                if jxl_to_jpeg(&jxl)? == data {
                    rec.jxl_bytes_effort9 = Some(jxl.len() as u64);
                    rec.encode_effort9 = Some(t);
                }
            }
        }
        ArchiveAction::LosslessJxl => {
            let raster = load_raster(path)?;
            rec.width = Some(raster.width);
            rec.height = Some(raster.height);
            let c = lossless_roundtrip(&raster)?;
            rec.jxl_bytes = Some(c.jxl.len() as u64);
            rec.encode = Some(c.encode);
            rec.decode = Some(c.decode);
            rec.verified = true;
            rec.byte_exact = false;
        }
        ArchiveAction::KeepOriginal if d.display_derivative => {
            // nezopt's libheif has no HEVC decoder, so the phone's own rendering (preview.jpg,
            // sent with the upload) stands in for the pixels when the original cannot be decoded.
            let (img, source) = match thumbs::load_oriented(path) {
                Ok(img) => (img, "original"),
                Err(_) => {
                    let preview = thumbs::dir_for(cfg, &cand.item_id).join("preview.jpg");
                    let img = thumbs::load_oriented(&preview)
                        .map_err(|e| ConvertError::Decode(e.to_string()))?;
                    (img, "phone_preview")
                }
            };
            rec.pixel_source = Some(source.to_string());
            rec.pixel_source_wh = Some((img.width(), img.height()));
            let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
            let raster = crate::archive_jxl::raster_of(&rgb, None)?;
            let (jxl, _) = crate::archive_jxl::timed(|| raster_to_jxl(&raster));
            let lossless = jxl?.len() as f64;
            let full_px = match (cand.width, cand.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w * h) as f64,
                _ => (img.width() * img.height()) as f64,
            };
            let have_px = (img.width() * img.height()) as f64;
            rec.est_lossless_full_bytes = Some((lossless * (full_px / have_px).max(1.0)) as u64);
            let (derived, t) = crate::archive_jxl::timed(|| display_derivative(&img));
            rec.display_bytes = Some(derived?.len() as u64);
            rec.display_encode = Some(t);
            rec.verified = true;
        }
        ArchiveAction::KeepOriginal => {}
    }
    Ok(())
}

// ---- the summary ------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub median: f64,
    pub p10: f64,
    pub p90: f64,
    pub mean: f64,
}

fn stats(mut v: Vec<f64>) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    Some(Stats {
        median: at(0.5),
        p10: at(0.1),
        p90: at(0.9),
        mean: v.iter().sum::<f64>() / v.len() as f64,
    })
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumSummary {
    pub population_files: usize,
    pub population_bytes: u64,
    pub sampled: usize,
    pub sampled_bytes: u64,
    pub converted_ok: usize,
    pub failures: BTreeMap<String, usize>,
    /// jxl / original over the files that converted (1.0 = no saving).
    pub ratio: Option<Stats>,
    /// Saving per file in percent.
    pub saving_pct: Option<Stats>,
    pub total_original_bytes_ok: u64,
    pub total_jxl_bytes_ok: u64,
    /// 1 - total jxl / total original over the files that converted.
    pub total_saving_pct: Option<f64>,
    /// The same over the whole sample, a failed or non-gaining file counting as unchanged.
    pub effective_saving_pct: Option<f64>,
    pub projected_saved_bytes: Option<u64>,
    pub encode_cpu_ms: Option<Stats>,
    pub decode_cpu_ms: Option<Stats>,
    pub encode_wall_ms: Option<Stats>,
    pub decode_wall_ms: Option<Stats>,
    pub effort9_files: usize,
    /// Over the effort-9 subset: how much smaller effort 9 is than effort 7 on the same files.
    pub effort9_extra_saving_pct: Option<f64>,
    pub effort9_encode_cpu_ratio: Option<f64>,
    pub catalog_hash_bad: usize,
    // HEIC / RAW
    pub pixel_from_preview: usize,
    pub display_to_original_ratio: Option<Stats>,
    pub est_lossless_to_original_ratio: Option<Stats>,
    pub total_display_saving_pct: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub strata: BTreeMap<Stratum, StratumSummary>,
    pub catalog_photo_files: usize,
    pub catalog_photo_bytes: u64,
    pub owned_files: usize,
    pub owned_bytes: u64,
    /// Files other services own (Drive folders): never touched.
    pub external_files: usize,
    pub external_bytes: u64,
    /// Owned photo files no stratum covers (GIF, WebP, AVIF, ...): kept as they are.
    pub other_owned_files: usize,
    pub other_owned_bytes: u64,
    pub projected_saved_bytes: u64,
    /// What the byte-exact part alone (JPEG recompression) would free.
    pub projected_saved_bytes_byte_exact: u64,
    pub round_trip_failures: usize,
    pub cpu_pauses: u32,
    pub jobs: usize,
}

pub fn summarize(pop: &[Candidate], records: &[FileRecord]) -> Summary {
    let mut sum = Summary::default();
    for c in pop {
        sum.catalog_photo_files += 1;
        sum.catalog_photo_bytes += c.bytes;
        if !c.owned() {
            sum.external_files += 1;
            sum.external_bytes += c.bytes;
            continue;
        }
        sum.owned_files += 1;
        sum.owned_bytes += c.bytes;
        match stratum_of(c) {
            Some(s) => {
                let e = sum.strata.entry(s).or_default();
                e.population_files += 1;
                e.population_bytes += c.bytes;
            }
            None => {
                sum.other_owned_files += 1;
                sum.other_owned_bytes += c.bytes;
            }
        }
    }
    for s in STRATA {
        let e = sum.strata.entry(s).or_default();
        let recs: Vec<&FileRecord> = records.iter().filter(|r| r.stratum == Some(s)).collect();
        e.sampled = recs.len();
        e.sampled_bytes = recs.iter().map(|r| r.orig_bytes).sum();
        let ok: Vec<&FileRecord> = recs
            .iter()
            .copied()
            .filter(|r| r.verified && r.jxl_bytes.is_some())
            .collect();
        e.converted_ok = ok.len();
        for r in recs.iter().filter(|r| !r.verified) {
            *e.failures
                .entry(r.failure.clone().unwrap_or_else(|| "unknown".into()))
                .or_default() += 1;
        }
        e.catalog_hash_bad = recs
            .iter()
            .filter(|r| r.catalog_hash_ok == Some(false))
            .count();
        let ratios: Vec<f64> = ok
            .iter()
            .map(|r| r.jxl_bytes.unwrap() as f64 / r.orig_bytes as f64)
            .collect();
        e.saving_pct = stats(ratios.iter().map(|x| (1.0 - x) * 100.0).collect());
        e.ratio = stats(ratios);
        e.total_original_bytes_ok = ok.iter().map(|r| r.orig_bytes).sum();
        e.total_jxl_bytes_ok = ok.iter().map(|r| r.jxl_bytes.unwrap()).sum();
        if e.total_original_bytes_ok > 0 {
            e.total_saving_pct = Some(
                (1.0 - e.total_jxl_bytes_ok as f64 / e.total_original_bytes_ok as f64) * 100.0,
            );
        }
        // Over everything sampled, a file that failed or came out larger stays as it is.
        let kept: u64 = recs
            .iter()
            .map(|r| match r.jxl_bytes {
                Some(j) if r.verified => j.min(r.orig_bytes),
                _ => r.orig_bytes,
            })
            .sum();
        if s != Stratum::HeicRaw && e.sampled_bytes > 0 {
            let eff = 1.0 - kept as f64 / e.sampled_bytes as f64;
            e.effective_saving_pct = Some(eff * 100.0);
            e.projected_saved_bytes = Some((e.population_bytes as f64 * eff.max(0.0)) as u64);
        }
        let col =
            |f: fn(&FileRecord) -> Option<f64>| stats(ok.iter().filter_map(|r| f(r)).collect());
        e.encode_cpu_ms = col(|r| r.encode.map(|t| t.cpu_ms));
        e.decode_cpu_ms = col(|r| r.decode.map(|t| t.cpu_ms));
        e.encode_wall_ms = col(|r| r.encode.map(|t| t.wall_ms));
        e.decode_wall_ms = col(|r| r.decode.map(|t| t.wall_ms));
        let e9: Vec<&FileRecord> = ok
            .iter()
            .copied()
            .filter(|r| r.jxl_bytes_effort9.is_some())
            .collect();
        e.effort9_files = e9.len();
        if !e9.is_empty() {
            let a: u64 = e9.iter().map(|r| r.jxl_bytes.unwrap()).sum();
            let b: u64 = e9.iter().map(|r| r.jxl_bytes_effort9.unwrap()).sum();
            e.effort9_extra_saving_pct = Some((1.0 - b as f64 / a as f64) * 100.0);
            let t7: f64 = e9.iter().filter_map(|r| r.encode.map(|t| t.cpu_ms)).sum();
            let t9: f64 = e9
                .iter()
                .filter_map(|r| r.encode_effort9.map(|t| t.cpu_ms))
                .sum();
            if t7 > 0.0 {
                e.effort9_encode_cpu_ratio = Some(t9 / t7);
            }
        }
        if s == Stratum::HeicRaw {
            let hr: Vec<&FileRecord> = recs
                .iter()
                .copied()
                .filter(|r| r.display_bytes.is_some())
                .collect();
            e.pixel_from_preview = hr
                .iter()
                .filter(|r| r.pixel_source.as_deref() == Some("phone_preview"))
                .count();
            e.display_to_original_ratio = stats(
                hr.iter()
                    .map(|r| r.display_bytes.unwrap() as f64 / r.orig_bytes as f64)
                    .collect(),
            );
            e.est_lossless_to_original_ratio = stats(
                hr.iter()
                    .filter_map(|r| r.est_lossless_full_bytes)
                    .zip(hr.iter().map(|r| r.orig_bytes as f64))
                    .map(|(l, o)| l as f64 / o)
                    .collect(),
            );
            let o: u64 = hr.iter().map(|r| r.orig_bytes).sum();
            let d: u64 = hr.iter().map(|r| r.display_bytes.unwrap()).sum();
            if o > 0 {
                e.total_display_saving_pct = Some((1.0 - d as f64 / o as f64) * 100.0);
            }
        }
    }
    sum.projected_saved_bytes = sum
        .strata
        .values()
        .filter_map(|e| e.projected_saved_bytes)
        .sum();
    sum.projected_saved_bytes_byte_exact = [Stratum::JpegArchive, Stratum::JpegIos]
        .iter()
        .filter_map(|s| sum.strata.get(s).and_then(|e| e.projected_saved_bytes))
        .sum();
    sum.round_trip_failures = records
        .iter()
        .filter(|r| {
            matches!(
                r.failure.as_deref(),
                Some("roundtrip_mismatch" | "no_reconstruction_data")
            )
        })
        .count();
    sum
}

// ---- the run ----------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DryRunOpts {
    pub sample: usize,
    pub seed: u64,
    pub jobs: usize,
    pub max_temp_c: u32,
    /// How many of the JPEGs (per stratum) also get the effort-9 transcode.
    pub effort9_per_stratum: usize,
    /// One JSON record per line, appended as each file is measured. A run that was killed picks up
    /// from it (records of files outside the current sample are ignored).
    pub checkpoint: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct Report {
    pub generated_at: String,
    pub libjxl: &'static str,
    pub binding: &'static str,
    pub sample_size: usize,
    pub summary: Summary,
    pub records: Vec<FileRecord>,
}

/// Measure a sample read-only. Progress goes to stderr as counts.
pub fn dry_run(cfg: &Config, opts: &DryRunOpts) -> anyhow::Result<Report> {
    let conn = open_readonly(&cfg.db_path())?;
    let pop = population(&conn)?;
    drop(conn);
    let caps = caps_for(opts.sample);
    let picked = pick_sample(&pop, &caps, opts.seed);
    let mut e9_left: BTreeMap<Stratum, usize> = BTreeMap::new();
    let work: Vec<(Stratum, Candidate, MeasureOpts)> = picked
        .into_iter()
        .map(|(s, c)| {
            let left = e9_left.entry(s).or_insert(opts.effort9_per_stratum);
            let effort9 = matches!(s, Stratum::JpegArchive | Stratum::JpegIos) && *left > 0;
            if effort9 {
                *left -= 1;
            }
            (s, c, MeasureOpts { effort9 })
        })
        .collect();
    let mut finished: BTreeMap<String, FileRecord> = BTreeMap::new();
    if let Some(path) = &opts.checkpoint {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Ok(rec) = serde_json::from_str::<FileRecord>(line) {
                    finished.insert(rec.id.clone(), rec);
                }
            }
        }
    }
    let order: Vec<String> = work.iter().map(|(_, c, _)| c.opaque_id()).collect();
    let todo: Vec<&(Stratum, Candidate, MeasureOpts)> = work
        .iter()
        .filter(|(_, c, _)| !finished.contains_key(&c.opaque_id()))
        .collect();
    if !finished.is_empty() {
        eprintln!(
            "archive-jxl dry-run: resuming, {} of {} already measured",
            work.len() - todo.len(),
            work.len()
        );
    }
    let log: Option<Mutex<std::fs::File>> = match &opts.checkpoint {
        Some(p) => Some(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?,
        )),
        None => None,
    };
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let pauses = AtomicUsize::new(0);
    let out: Mutex<Vec<FileRecord>> = Mutex::new(Vec::new());
    let jobs = opts.jobs.max(1);
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some((s, c, mo)) = todo.get(i).copied() else {
                    break;
                };
                pauses.fetch_add(wait_for_cool(opts.max_temp_c) as usize, Ordering::SeqCst);
                // A decoder panic on one odd file must not lose the run.
                let rec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    measure_file(cfg, c, *s, *mo)
                }))
                .unwrap_or_else(|_| FileRecord {
                    id: c.opaque_id(),
                    stratum: Some(*s),
                    orig_bytes: c.bytes,
                    failure: Some("panic".into()),
                    ..FileRecord::default()
                });
                if let Some(log) = &log {
                    use std::io::Write;
                    if let Ok(line) = serde_json::to_string(&rec) {
                        let _ = writeln!(log.lock().unwrap(), "{line}");
                    }
                }
                out.lock().unwrap().push(rec);
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                if n.is_multiple_of(25) || n == todo.len() {
                    eprintln!("archive-jxl dry-run: {n}/{} measured", todo.len());
                }
            });
        }
    });
    for rec in out.into_inner().unwrap() {
        finished.insert(rec.id.clone(), rec);
    }
    let records: Vec<FileRecord> = order
        .iter()
        .filter_map(|id| finished.get(id).cloned())
        .collect();
    let mut summary = summarize(&pop, &records);
    summary.cpu_pauses = pauses.load(Ordering::SeqCst) as u32;
    summary.jobs = jobs;
    Ok(Report {
        generated_at: crate::db::now_str(),
        libjxl: "0.12.0 (vendored, jpegxl-sys 0.13.0)",
        binding: "jpegxl-rs 0.16.0",
        sample_size: records.len(),
        summary,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(i: u32, ext: &str, source: &str, location: &str) -> Candidate {
        Candidate {
            hash: format!("{i:064x}"),
            ext: ext.into(),
            bytes: 1000 + i as u64,
            location: location.into(),
            item_id: format!("itm_{i}"),
            source: source.into(),
            width: Some(100),
            height: Some(100),
        }
    }

    #[test]
    fn strata_follow_the_extension_and_the_source() {
        assert_eq!(
            stratum_of(&cand(1, "jpg", "phone-archive", "store")),
            Some(Stratum::JpegArchive)
        );
        assert_eq!(
            stratum_of(&cand(2, "JPEG", "phone-ios", "store")),
            Some(Stratum::JpegIos)
        );
        assert_eq!(
            stratum_of(&cand(3, "png", "ios-chat", "spool")),
            Some(Stratum::Lossless)
        );
        assert_eq!(
            stratum_of(&cand(4, "heic", "phone-ios", "store")),
            Some(Stratum::HeicRaw)
        );
        // Another service's files and formats we keep are in no stratum.
        assert_eq!(stratum_of(&cand(5, "jpg", "folder", "external")), None);
        assert_eq!(stratum_of(&cand(6, "gif", "phone-ios", "store")), None);
    }

    #[test]
    fn the_sample_is_deterministic_capped_and_per_stratum() {
        let mut pop = Vec::new();
        for i in 0..400 {
            pop.push(cand(i, "jpg", "phone-archive", "store"));
        }
        for i in 400..440 {
            pop.push(cand(i, "jpg", "phone-ios", "store"));
        }
        for i in 440..445 {
            pop.push(cand(i, "png", "phone-ios", "store"));
        }
        let caps = caps_for(60);
        assert_eq!(caps[&Stratum::JpegArchive], 30);
        assert_eq!(caps[&Stratum::JpegIos], 15);
        let a = pick_sample(&pop, &caps, 42);
        let b = pick_sample(&pop, &caps, 42);
        let c = pick_sample(&pop, &caps, 43);
        let ids =
            |v: &[(Stratum, Candidate)]| v.iter().map(|x| x.1.hash.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
        assert_ne!(ids(&a), ids(&c));
        let count = |s| a.iter().filter(|x| x.0 == s).count();
        assert_eq!(count(Stratum::JpegArchive), 30);
        assert_eq!(count(Stratum::JpegIos), 15);
        assert_eq!(
            count(Stratum::Lossless),
            5,
            "fewer than the cap: all of them"
        );
        let mut seen = ids(&a);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), a.len(), "no file twice");
    }

    #[test]
    fn the_summary_counts_savings_and_failures() {
        let pop = vec![
            cand(1, "jpg", "phone-archive", "store"),
            cand(2, "jpg", "phone-archive", "store"),
            cand(3, "jpg", "folder", "external"),
        ];
        let rec = |id: &str, orig: u64, jxl: Option<u64>, fail: Option<&str>| FileRecord {
            id: id.into(),
            stratum: Some(Stratum::JpegArchive),
            orig_bytes: orig,
            jxl_bytes: jxl,
            verified: fail.is_none(),
            failure: fail.map(Into::into),
            ..FileRecord::default()
        };
        let records = vec![
            rec("a", 1000, Some(800), None),
            rec("b", 1000, None, Some("roundtrip_mismatch")),
        ];
        let s = summarize(&pop, &records);
        assert_eq!((s.owned_files, s.external_files), (2, 1));
        let e = &s.strata[&Stratum::JpegArchive];
        assert_eq!((e.sampled, e.converted_ok), (2, 1));
        assert_eq!(e.failures["roundtrip_mismatch"], 1);
        assert_eq!(s.round_trip_failures, 1);
        assert!((e.total_saving_pct.unwrap() - 20.0).abs() < 1e-9);
        // Over the whole sample the failed file stays as it is: 200 of 2000 bytes saved.
        assert!((e.effective_saving_pct.unwrap() - 10.0).abs() < 1e-9);
    }
}
