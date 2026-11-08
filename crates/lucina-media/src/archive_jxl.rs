//! The archive format: backup photos stored as JPEG XL, losslessly (docs/media-library.md,
//! "Archive format").
//!
//! - A JPEG original is *recompressed* (libjxl's JPEG transcoding, `JxlEncoderAddJPEGFrame` with the
//!   reconstruction data stored): the JXL decodes to the same pixels and also rebuilds the original
//!   file bit for bit. Only a JXL whose rebuilt file has the original's blake3 is ever kept.
//! - A PNG is stored as lossless JXL from its decoded pixels (effort 7, `archive_png.rs`). That is
//!   pixel-exact, not byte-exact: the file's bytes (and its text chunks) cannot be rebuilt. The
//!   original PNG is deleted only after the JXL read back from disk decodes to the same pixel
//!   digest, and once it is gone a PNG with the same pixels, layout and ICC profile is rebuilt
//!   (and verified) on demand. TIFF and BMP are measured but `apply` leaves them alone.
//! - HEIC, RAW and everything else stay as they are; HEIC/RAW get a lossy display derivative.
//!
//! The item keeps one identity. `items.hash` stays the original's hash (so the ETag, device
//! comparison and downloads do not change). `archive_blobs` records the JXL next to it, with the
//! hash it `reconstructs_to`. Where the original file is gone, [`materialize`] rebuilds it into a
//! small cache and `ingest::original_path` hands that path out, so every reader of the original
//! (the original route, thumbnails, EXIF details) works unchanged and serves the identical bytes.
//!
//! The original file is deleted only after a verified round trip, only when asked
//! (`--delete-originals`), and `apply` refuses to run without `--i-have-a-backup`. `--kinds`
//! chooses between JPEG and PNG, `--manifest` lists every processed file (`archive_manifest.rs`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{ColorType, DynamicImage, ImageDecoder, ImageReader};
use jpegxl_rs::decode::{Data, PixelFormat};
use jpegxl_rs::encode::{ColorEncoding, EncoderFrame, EncoderSpeed};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::archive_manifest::Manifest;
use crate::archive_png::{self, PngColor};
use crate::config::Config;
use crate::db::{now_str, Db};
use crate::ingest::Library;
use crate::store;

/// libjxl effort for both the JPEG transcode and the lossless pixels (7 = "squirrel", libjxl's
/// default; the report has the measured gain of effort 9).
pub const ARCHIVE_EFFORT: EncoderSpeed = EncoderSpeed::Squirrel;
/// The display derivative of HEIC/RAW: butteraugli distance 1.0 (= JPEG quality 90, "visually
/// lossless"), long side at most 2560 px.
pub const DISPLAY_DISTANCE: f32 = 1.0;
pub const DISPLAY_LONG_SIDE: u32 = 2560;
/// The rebuilt-original cache stays under this size (oldest files go first).
pub const CACHE_LIMIT_BYTES: u64 = 1 << 30;

// ---- the decision ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Jpeg,
    Png,
    Tiff,
    Bmp,
    Gif,
    Webp,
    Avif,
    Heic,
    Raw,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveAction {
    /// Lossless JPEG transcode; the original file is rebuilt from the JXL bit for bit.
    JpegRecompress,
    /// Lossless JXL from the decoded pixels; pixel-exact.
    LosslessJxl,
    /// Keep the original as the archive copy.
    KeepOriginal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub format: Format,
    pub action: ArchiveAction,
    /// Also make a lossy JXL for display (HEIC/RAW: the originals stay, clients get this).
    pub display_derivative: bool,
    pub reason: &'static str,
}

const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef",
    "srw", "x3f", "3fr", "erf", "kdc", "mrw", "raw",
];

/// The format of a file from its extension (RAW only) and its first bytes. The mime type the
/// client sent is not trusted.
pub fn sniff(ext: &str, head: &[u8]) -> Format {
    let ext = ext.to_ascii_lowercase();
    if RAW_EXTS.contains(&ext.as_str()) {
        return Format::Raw;
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Format::Jpeg;
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Format::Png;
    }
    if head.starts_with(b"II*\0")
        || head.starts_with(b"MM\0*")
        || head.starts_with(b"II+\0")
        || head.starts_with(b"MM\0+")
    {
        return Format::Tiff;
    }
    if head.starts_with(b"BM") && matches!(ext.as_str(), "bmp" | "dib") {
        return Format::Bmp;
    }
    if head.starts_with(b"GIF8") {
        return Format::Gif;
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Format::Webp;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        let compatible = &head[12..head.len().min(64)];
        let has = |b: &[u8]| compatible.windows(4).any(|w| w == b);
        if brand == b"avif" || brand == b"avis" || has(b"avif") {
            return Format::Avif;
        }
        if matches!(
            brand,
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1"
        ) {
            return Format::Heic;
        }
    }
    Format::Other
}

/// Which archive action a file gets. Pure: no I/O.
pub fn decide(ext: &str, head: &[u8]) -> Decision {
    let format = sniff(ext, head);
    let (action, display_derivative, reason) = match format {
        Format::Jpeg => (
            ArchiveAction::JpegRecompress,
            false,
            "lossless JPEG transcode, original rebuilt bit-exact",
        ),
        Format::Png | Format::Tiff | Format::Bmp => (
            ArchiveAction::LosslessJxl,
            false,
            "lossless JXL from pixels, pixel-exact (not byte-exact)",
        ),
        Format::Heic => (
            ArchiveAction::KeepOriginal,
            true,
            "HEVC original kept; lossy JXL for display",
        ),
        Format::Raw => (
            ArchiveAction::KeepOriginal,
            true,
            "RAW original kept; lossy JXL for display",
        ),
        Format::Gif | Format::Webp | Format::Avif => (
            ArchiveAction::KeepOriginal,
            false,
            "already compressed or animated; kept",
        ),
        Format::Other => (
            ArchiveAction::KeepOriginal,
            false,
            "not a still image; kept",
        ),
    };
    Decision {
        format,
        action,
        display_derivative,
        reason,
    }
}

// ---- timing ----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Timing {
    /// CPU time of the calling thread (what the work costs, whatever else the box is doing).
    pub cpu_ms: f64,
    pub wall_ms: f64,
}

fn thread_cpu_ms() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer for the whole call.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 * 1000.0 + ts.tv_nsec as f64 / 1e6
}

pub fn timed<T>(f: impl FnOnce() -> T) -> (T, Timing) {
    let (c0, w0) = (thread_cpu_ms(), Instant::now());
    let out = f();
    (
        out,
        Timing {
            cpu_ms: thread_cpu_ms() - c0,
            wall_ms: w0.elapsed().as_secs_f64() * 1000.0,
        },
    )
}

// ---- errors ----------------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("this JPEG cannot be transcoded: {0}")]
    JpegUnsupported(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("the JXL carries no JPEG reconstruction data")]
    NoReconstruction,
    #[error("the rebuilt file differs from the original")]
    Mismatch,
    #[error("unsupported colour profile")]
    UnsupportedProfile,
    #[error("unsupported pixel format: {0}")]
    UnsupportedPixels(&'static str),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl ConvertError {
    /// A short stable code for counting failures by reason (never a name or a path).
    pub fn reason(&self) -> &'static str {
        match self {
            ConvertError::JpegUnsupported(_) => "jpeg_unsupported",
            ConvertError::Encode(_) => "encode_error",
            ConvertError::Decode(_) => "decode_error",
            ConvertError::NoReconstruction => "no_reconstruction_data",
            ConvertError::Mismatch => "roundtrip_mismatch",
            ConvertError::UnsupportedProfile => "unsupported_icc_profile",
            ConvertError::UnsupportedPixels(_) => "unsupported_pixel_format",
            ConvertError::Io(_) => "io_error",
        }
    }
}

pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

// ---- JPEG: recompress and rebuild -----------------------------------------------------------

/// What a verified conversion hands back.
#[derive(Debug)]
pub struct Converted {
    pub jxl: Vec<u8>,
    /// The hash the JXL rebuilds to: the original file's blake3 for a JPEG, the pixel hash for
    /// lossless pixels ([`pixel_hash`]).
    pub reconstructs_to: String,
    pub byte_exact: bool,
    pub encode: Timing,
    pub decode: Timing,
}

/// JPEG bytes to a JXL that stores the reconstruction data. Not verified.
pub fn jpeg_to_jxl(jpeg: &[u8]) -> Result<Vec<u8>, ConvertError> {
    jpeg_to_jxl_at(jpeg, ARCHIVE_EFFORT)
}

/// [`jpeg_to_jxl`] at another effort (the report compares 7 with 9).
pub fn jpeg_to_jxl_at(jpeg: &[u8], speed: EncoderSpeed) -> Result<Vec<u8>, ConvertError> {
    let mut enc = jpegxl_rs::encoder_builder()
        .speed(speed)
        .use_container(true)
        .uses_original_profile(true)
        .build()
        .map_err(|e| ConvertError::Encode(e.to_string()))?;
    enc.encode_jpeg(jpeg)
        .map_err(|e| ConvertError::JpegUnsupported(e.to_string()))
}

/// The JPEG file a JXL rebuilds to. Fails when the JXL has no reconstruction data.
pub fn jxl_to_jpeg(jxl: &[u8]) -> Result<Vec<u8>, ConvertError> {
    let dec = jpegxl_rs::decoder_builder()
        .build()
        .map_err(|e| ConvertError::Decode(e.to_string()))?;
    match dec
        .reconstruct(jxl)
        .map_err(|e| ConvertError::Decode(e.to_string()))?
    {
        (_, Data::Jpeg(j)) => Ok(j),
        (_, Data::Pixels(_)) => Err(ConvertError::NoReconstruction),
    }
}

/// Recompress a JPEG and prove the JXL rebuilds it: the returned JXL is a bit-exact round trip
/// (same bytes, same blake3) or this is an error.
pub fn jpeg_roundtrip(jpeg: &[u8]) -> Result<Converted, ConvertError> {
    let want = blake3_hex(jpeg);
    let (jxl, encode) = timed(|| jpeg_to_jxl(jpeg));
    let jxl = jxl?;
    let (back, decode) = timed(|| jxl_to_jpeg(&jxl));
    let back = back?;
    if back.len() != jpeg.len() || blake3_hex(&back) != want || back != jpeg {
        return Err(ConvertError::Mismatch);
    }
    Ok(Converted {
        jxl,
        reconstructs_to: want,
        byte_exact: true,
        encode,
        decode,
    })
}

// ---- PNG/TIFF/BMP: lossless pixels ----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Srgb,
    DisplayP3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// Decoded pixels, interleaved RGB or RGBA (grey is expanded to three equal channels).
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub profile: Profile,
    pub samples: Samples,
}

impl Raster {
    pub fn channels(&self) -> u32 {
        if self.alpha {
            4
        } else {
            3
        }
    }
}

/// The colour space of an embedded ICC profile, for the two spaces the JXL binding can tag
/// (no profile means sRGB, as for a plain PNG). The profile is judged by what it says, not by its
/// name: an RGB matrix/TRC profile whose colorants are sRGB's or Display P3's (D50-adapted, within
/// 0.005) and whose tone curve is the sRGB curve (parametric type 3 with gamma 2.4, or a table that
/// matches it at four points). Anything else is refused rather than stored with the wrong colours.
pub fn profile_of(icc: Option<&[u8]>) -> Result<Profile, ConvertError> {
    let Some(p) = icc else {
        return Ok(Profile::Srgb);
    };
    let bad = ConvertError::UnsupportedProfile;
    if p.len() < 132 || &p[16..20] != b"RGB " {
        return Err(bad);
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let fixed = |b: &[u8]| be32(b) as i32 as f64 / 65536.0;
    let n = be32(&p[128..132]) as usize;
    let tag = |sig: &[u8; 4]| -> Option<&[u8]> {
        (0..n.min(200)).find_map(|i| {
            let e = p.get(132 + i * 12..144 + i * 12)?;
            if &e[0..4] != sig {
                return None;
            }
            p.get(be32(&e[4..8]) as usize..)?
                .get(..be32(&e[8..12]) as usize)
        })
    };
    let xyz = |sig: &[u8; 4]| -> Option<[f64; 3]> {
        let t = tag(sig)?;
        (t.len() >= 20 && &t[0..4] == b"XYZ ")
            .then(|| [fixed(&t[8..12]), fixed(&t[12..16]), fixed(&t[16..20])])
    };
    let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.005);
    let colorants = [xyz(b"rXYZ"), xyz(b"gXYZ"), xyz(b"bXYZ")];
    let [Some(r), Some(g), Some(b)] = colorants else {
        return Err(bad);
    };
    let profile = if near(r, [0.4361, 0.2225, 0.0139])
        && near(g, [0.3851, 0.7169, 0.0971])
        && near(b, [0.1431, 0.0606, 0.7141])
    {
        Profile::Srgb
    } else if near(r, [0.5151, 0.2412, -0.0011])
        && near(g, [0.2920, 0.6922, 0.0419])
        && near(b, [0.1571, 0.0666, 0.7841])
    {
        Profile::DisplayP3
    } else {
        return Err(bad);
    };
    let srgb_eotf = |x: f64| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let curve_ok = |sig: &[u8; 4]| -> bool {
        let Some(t) = tag(sig) else { return false };
        match t.get(0..4) {
            Some(b"para") if t.len() >= 16 => {
                u16::from_be_bytes([t[8], t[9]]) == 3 && (fixed(&t[12..16]) - 2.4).abs() < 0.01
            }
            Some(b"curv") if t.len() >= 12 => {
                let count = be32(&t[8..12]) as usize;
                if count < 16 || t.len() < 12 + count * 2 {
                    return false;
                }
                let at =
                    |i: usize| u16::from_be_bytes([t[12 + i * 2], t[13 + i * 2]]) as f64 / 65535.0;
                // Shadows tell the sRGB curve from a plain 2.2 gamma: compare within 3%.
                [0.1, 0.25, 0.5, 0.75].iter().all(|x| {
                    let i = (x * (count - 1) as f64).round() as usize;
                    let want = srgb_eotf(i as f64 / (count - 1) as f64);
                    (at(i) - want).abs() <= 0.03 * want + 0.0005
                })
            }
            _ => false,
        }
    };
    if curve_ok(b"rTRC") && curve_ok(b"gTRC") && curve_ok(b"bTRC") {
        Ok(profile)
    } else {
        Err(bad)
    }
}

pub fn raster_of(img: &DynamicImage, icc: Option<&[u8]>) -> Result<Raster, ConvertError> {
    let profile = profile_of(icc)?;
    let ct = img.color();
    let alpha = ct.has_alpha();
    let wide = match ct {
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8 => false,
        ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16 => true,
        _ => return Err(ConvertError::UnsupportedPixels("float or unknown")),
    };
    let samples = match (wide, alpha) {
        (false, false) => Samples::U8(img.to_rgb8().into_raw()),
        (false, true) => Samples::U8(img.to_rgba8().into_raw()),
        (true, false) => Samples::U16(img.to_rgb16().into_raw()),
        (true, true) => Samples::U16(img.to_rgba16().into_raw()),
    };
    Ok(Raster {
        width: img.width(),
        height: img.height(),
        alpha,
        profile,
        samples,
    })
}

/// Decode a PNG/TIFF/BMP file to a [`Raster`] (orientation is not applied: the stored pixels are
/// the file's own).
pub fn load_raster(path: &Path) -> Result<Raster, ConvertError> {
    let mut decoder = ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()
        .map_err(|e| ConvertError::Decode(e.to_string()))?;
    let icc = decoder
        .icc_profile()
        .map_err(|e| ConvertError::Decode(e.to_string()))?;
    let img =
        DynamicImage::from_decoder(decoder).map_err(|e| ConvertError::Decode(e.to_string()))?;
    raster_of(&img, icc.as_deref())
}

/// blake3 over the pixels with their shape, so two rasters hash alike only when every sample
/// matches.
pub fn pixel_hash(r: &Raster) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"lucina-pixels-v1");
    h.update(&r.width.to_le_bytes());
    h.update(&r.height.to_le_bytes());
    h.update(&r.channels().to_le_bytes());
    match &r.samples {
        Samples::U8(v) => {
            h.update(&[8]);
            h.update(v);
        }
        Samples::U16(v) => {
            h.update(&[16]);
            for s in v {
                h.update(&s.to_le_bytes());
            }
        }
    }
    h.finalize().to_hex().to_string()
}

fn colour_encoding(p: Profile) -> ColorEncoding {
    use jpegxl_sys::color::color_encoding::{JxlColorEncoding, JxlPrimaries};
    match p {
        Profile::Srgb => ColorEncoding::Srgb,
        Profile::DisplayP3 => {
            let mut enc = JxlColorEncoding::from(&ColorEncoding::Srgb);
            enc.primaries = JxlPrimaries::P3;
            ColorEncoding::Custom(enc)
        }
    }
}

/// Lossless JXL of a raster, effort 7. Not verified.
pub fn raster_to_jxl(r: &Raster) -> Result<Vec<u8>, ConvertError> {
    use jpegxl_sys::encoder::encode::JxlEncoderFrameSettingId as Opt;
    let mut enc = jpegxl_rs::encoder_builder()
        .lossless(true)
        .speed(ARCHIVE_EFFORT)
        .has_alpha(r.alpha)
        .uses_original_profile(true)
        .color_encoding(colour_encoding(r.profile))
        .build()
        .map_err(|e| ConvertError::Encode(e.to_string()))?;
    // Colour under fully transparent pixels is data too.
    enc.set_frame_option(Opt::KeepInvisible, 1);
    let err = |e: jpegxl_rs::EncodeError| ConvertError::Encode(e.to_string());
    match &r.samples {
        Samples::U8(v) => enc.encode_frame(
            &EncoderFrame::new(v).num_channels(r.channels()),
            r.width,
            r.height,
        ),
        Samples::U16(v) => enc.encode_frame(
            &EncoderFrame::new(v).num_channels(r.channels()),
            r.width,
            r.height,
        ),
    }
    .map_err(err)
}

/// The samples a JXL decodes to, in the shape of `like`.
pub fn jxl_to_samples(jxl: &[u8], like: &Raster) -> Result<Samples, ConvertError> {
    let format = PixelFormat {
        num_channels: like.channels(),
        ..PixelFormat::default()
    };
    let dec = jpegxl_rs::decoder_builder()
        .pixel_format(format)
        .build()
        .map_err(|e| ConvertError::Decode(e.to_string()))?;
    let err = |e: jpegxl_rs::DecodeError| ConvertError::Decode(e.to_string());
    Ok(match like.samples {
        Samples::U8(_) => Samples::U8(dec.decode_with::<u8>(jxl).map_err(err)?.1),
        Samples::U16(_) => Samples::U16(dec.decode_with::<u16>(jxl).map_err(err)?.1),
    })
}

/// Lossless JXL of a raster, decoded again and compared sample by sample: the JXL is returned
/// only when every pixel survived.
pub fn lossless_roundtrip(r: &Raster) -> Result<Converted, ConvertError> {
    let (jxl, encode) = timed(|| raster_to_jxl(r));
    let jxl = jxl?;
    let (back, decode) = timed(|| jxl_to_samples(&jxl, r));
    if back? != r.samples {
        return Err(ConvertError::Mismatch);
    }
    Ok(Converted {
        jxl,
        reconstructs_to: pixel_hash(r),
        byte_exact: false,
        encode,
        decode,
    })
}

/// The lossy JXL clients get instead of a HEIC/RAW original: long side at most 2560 px,
/// distance 1.0.
pub fn display_derivative(img: &DynamicImage) -> Result<Vec<u8>, ConvertError> {
    let img = if img.width().max(img.height()) > DISPLAY_LONG_SIDE {
        img.resize(
            DISPLAY_LONG_SIDE,
            DISPLAY_LONG_SIDE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img.clone()
    };
    let rgb = img.to_rgb8();
    let mut enc = jpegxl_rs::encoder_builder()
        .quality(DISPLAY_DISTANCE)
        .speed(ARCHIVE_EFFORT)
        .build()
        .map_err(|e| ConvertError::Encode(e.to_string()))?;
    enc.encode::<u8>(rgb.as_raw(), rgb.width(), rgb.height())
        .map_err(|e| ConvertError::Encode(e.to_string()))
}

// ---- the storage model -----------------------------------------------------------------------

/// The kinds of `archive_blobs.kind`.
pub const KIND_JPEG: &str = "jpeg_recompress";
pub const KIND_PNG: &str = "png_lossless";

const COLUMNS: &str =
    "hash, kind, jxl_hash, jxl_bytes, reconstructs_to, original_bytes, original_ext, \
     verified_at, original_deleted_at, original_sha256, pixel_digest, png_color, icc";

fn create_sql(name: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {name} (
            hash TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('{KIND_JPEG}', '{KIND_PNG}')),
            jxl_hash TEXT NOT NULL,
            jxl_bytes INTEGER NOT NULL,
            reconstructs_to TEXT NOT NULL,
            original_bytes INTEGER NOT NULL,
            original_ext TEXT NOT NULL,
            verified_at TEXT NOT NULL,
            original_deleted_at TEXT,
            original_sha256 TEXT,
            pixel_digest TEXT,
            png_color TEXT,
            icc BLOB
        );"
    )
}

/// Idempotent; called from the catalog's migrate step. Adds the columns the first release did not
/// have (SHA-256 of the original, and for PNG the pixel digest, layout and ICC profile) and widens
/// the `kind` check to allow PNG rows (SQLite cannot alter a CHECK, so the table is rebuilt once,
/// rows kept).
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&create_sql("archive_blobs"))?;
    let has = |col: &str| -> rusqlite::Result<bool> {
        let mut st =
            conn.prepare("SELECT 1 FROM pragma_table_info('archive_blobs') WHERE name = ?1")?;
        st.exists([col])
    };
    for (col, ddl) in [
        ("original_sha256", "original_sha256 TEXT"),
        ("pixel_digest", "pixel_digest TEXT"),
        ("png_color", "png_color TEXT"),
        ("icc", "icc BLOB"),
    ] {
        if !has(col)? {
            conn.execute(&format!("ALTER TABLE archive_blobs ADD COLUMN {ddl}"), [])?;
        }
    }
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'archive_blobs'",
        [],
        |r| r.get(0),
    )?;
    if !sql.contains(KIND_PNG) {
        conn.execute_batch(&format!(
            "SAVEPOINT archive_widen;
             DROP TABLE IF EXISTS archive_blobs_new;
             {create}
             INSERT INTO archive_blobs_new ({COLUMNS}) SELECT {COLUMNS} FROM archive_blobs;
             DROP TABLE archive_blobs;
             ALTER TABLE archive_blobs_new RENAME TO archive_blobs;
             RELEASE archive_widen;",
            create = create_sql("archive_blobs_new"),
        ))?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveRow {
    /// The original's hash: the item's identity and its ETag.
    pub hash: String,
    pub kind: String,
    pub jxl_hash: String,
    pub jxl_bytes: i64,
    /// What the JXL rebuilds to: the original's blake3 for a JPEG (always equal to `hash`), the
    /// pixel digest for a PNG.
    pub reconstructs_to: String,
    pub original_bytes: i64,
    pub original_ext: String,
    pub verified_at: String,
    pub original_deleted_at: Option<String>,
    /// SHA-256 of the original file (a JPEG's rebuilt file must match it too).
    pub original_sha256: Option<String>,
    /// PNG only: [`archive_png::digest`] of the original's pixels, the layout they are in and the
    /// ICC profile bytes (the JXL itself only tags sRGB or Display P3).
    pub pixel_digest: Option<String>,
    pub png_color: Option<String>,
    pub icc: Option<Vec<u8>>,
}

impl ArchiveRow {
    pub fn is_png(&self) -> bool {
        self.kind == KIND_PNG
    }
}

pub fn archive_row(c: &Connection, hash: &str) -> rusqlite::Result<Option<ArchiveRow>> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM archive_blobs WHERE hash = ?1"),
        [hash],
        |r| {
            Ok(ArchiveRow {
                hash: r.get(0)?,
                kind: r.get(1)?,
                jxl_hash: r.get(2)?,
                jxl_bytes: r.get(3)?,
                reconstructs_to: r.get(4)?,
                original_bytes: r.get(5)?,
                original_ext: r.get(6)?,
                verified_at: r.get(7)?,
                original_deleted_at: r.get(8)?,
                original_sha256: r.get(9)?,
                pixel_digest: r.get(10)?,
                png_color: r.get(11)?,
                icc: r.get(12)?,
            })
        },
    )
    .optional()
}

fn insert_row(c: &Connection, row: &ArchiveRow) -> rusqlite::Result<usize> {
    c.execute(
        &format!(
            "INSERT OR REPLACE INTO archive_blobs ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
        ),
        params![
            row.hash,
            row.kind,
            row.jxl_hash,
            row.jxl_bytes,
            row.reconstructs_to,
            row.original_bytes,
            row.original_ext,
            row.verified_at,
            row.original_deleted_at,
            row.original_sha256,
            row.pixel_digest,
            row.png_color,
            row.icc,
        ],
    )
}

fn is_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A hash's JXL, relative to the store: `archive-jxl/ab/cd/<hash>.jxl`.
pub fn jxl_rel(hash: &str) -> PathBuf {
    PathBuf::from("archive-jxl")
        .join(&hash[0..2])
        .join(&hash[2..4])
        .join(format!("{hash}.jxl"))
}

/// Where a hash's JXL lives: `archive-jxl/ab/cd/<hash>.jxl` on the big disk.
pub fn jxl_path(cfg: &Config, hash: &str) -> PathBuf {
    cfg.store_dir.join(jxl_rel(hash))
}

pub fn cache_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("archive-cache")
}

fn cache_path(cfg: &Config, hash: &str, png: bool) -> PathBuf {
    cache_dir(cfg).join(format!("{hash}.{}", if png { "png" } else { "jpg" }))
}

/// Is `path` a PNG rebuilt from a JXL (in the rebuilt-original cache)? Such a file has the original's
/// pixels but not its bytes, so it is served with a weak ETag.
pub fn is_rebuilt(cfg: &Config, path: &Path) -> bool {
    path.parent() == Some(cache_dir(cfg).as_path()) && path.extension().is_some_and(|e| e == "png")
}

/// The ETag value (without quotes; a `W/` prefix marks a weak one) for the original at `path`
/// of the item whose hash is `hash`: the hash, weak for a rebuilt PNG.
pub fn original_etag(cfg: &Config, path: &Path, hash: &str) -> String {
    if is_rebuilt(cfg, path) {
        format!("W/{hash}")
    } else {
        hash.to_string()
    }
}

/// Write `bytes` to `dst` durably: temp file next to it, fsync, rename, fsync the directory.
pub(crate) fn write_durable(dst: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = dst
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.part",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("obj"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dst)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn png_layout(row: &ArchiveRow) -> Result<(PngColor, &str), ConvertError> {
    let color = row
        .png_color
        .as_deref()
        .and_then(PngColor::parse)
        .ok_or(ConvertError::Mismatch)?;
    let digest = row.pixel_digest.as_deref().ok_or(ConvertError::Mismatch)?;
    Ok((color, digest))
}

/// The JXL of a row, read from disk and checked against its recorded hash.
fn read_jxl(cfg: &Config, row: &ArchiveRow) -> Result<Vec<u8>, ConvertError> {
    if !is_hash(&row.hash) {
        return Err(ConvertError::Mismatch);
    }
    let jxl = std::fs::read(jxl_path(cfg, &row.hash))?;
    if jxl.len() as i64 != row.jxl_bytes || blake3_hex(&jxl) != row.jxl_hash {
        return Err(ConvertError::Mismatch);
    }
    Ok(jxl)
}

/// The pixels a PNG row's JXL decodes to on disk, checked against the digest recorded from the
/// original. This is what lets a PNG original be deleted.
fn verify_png_pixels(
    cfg: &Config,
    row: &ArchiveRow,
) -> Result<archive_png::PngPixels, ConvertError> {
    let (color, digest) = png_layout(row)?;
    let jxl = read_jxl(cfg, row)?;
    let pixels = archive_png::jxl_to_pixels(&jxl, color, row.icc.clone())?;
    if archive_png::digest(&pixels) != digest || row.reconstructs_to != digest {
        return Err(ConvertError::Mismatch);
    }
    Ok(pixels)
}

/// Rebuild the original of an archived hash from its JXL on disk, proving it is the original.
///
/// JPEG: the JXL file must match its recorded hash, and the rebuilt bytes must hash to both
/// `reconstructs_to` and the item's own hash (and to the recorded SHA-256). PNG: the JXL must decode
/// to the recorded pixel digest, and the PNG built from it must decode to that digest again (the
/// bytes are not the original's). Anything else is an error, never wrong bytes.
pub fn reconstruct(cfg: &Config, row: &ArchiveRow) -> Result<Vec<u8>, ConvertError> {
    if row.is_png() {
        let pixels = verify_png_pixels(cfg, row)?;
        return archive_png::encode_verified(&pixels, &row.reconstructs_to);
    }
    let jxl = read_jxl(cfg, row)?;
    let jpeg = jxl_to_jpeg(&jxl)?;
    let got = blake3_hex(&jpeg);
    if got != row.reconstructs_to || got != row.hash {
        return Err(ConvertError::Mismatch);
    }
    if row
        .original_sha256
        .as_deref()
        .is_some_and(|want| sha256_hex(&jpeg) != want)
    {
        return Err(ConvertError::Mismatch);
    }
    Ok(jpeg)
}

/// Rebuilt PNG files already decoded and found right, by (path, size, modification time), so a
/// Range request or a thumbnail does not decode the picture again.
fn verified_cache(
) -> &'static std::sync::Mutex<std::collections::HashSet<(PathBuf, u64, std::time::SystemTime)>> {
    static SET: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<(PathBuf, u64, std::time::SystemTime)>>,
    > = std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Is the cached file at `path` still the picture `row` records?
fn cached_ok(path: &Path, row: &ArchiveRow) -> bool {
    let Ok(m) = std::fs::metadata(path) else {
        return false;
    };
    if !m.is_file() || m.len() == 0 {
        return false;
    }
    if !row.is_png() {
        return m.len() as i64 == row.original_bytes;
    }
    let Ok(mtime) = m.modified() else {
        return false;
    };
    let key = (path.to_path_buf(), m.len(), mtime);
    if verified_cache().lock().is_ok_and(|s| s.contains(&key)) {
        return true;
    }
    let ok = std::fs::read(path)
        .ok()
        .and_then(|b| archive_png::decode(&b).ok())
        .is_some_and(|p| Some(archive_png::digest(&p)) == row.pixel_digest);
    if ok {
        if let Ok(mut s) = verified_cache().lock() {
            if s.len() > 4096 {
                s.clear();
            }
            s.insert(key);
        }
    }
    ok
}

/// The file of an archived original whose stored file is gone: rebuilt into the cache (verified
/// before it is moved into place) and its path. `None` when the hash is not archived or the
/// rebuild fails (logged, without names). A JPEG comes back byte for byte; a PNG comes back as
/// the same picture (see [`archive_png`]).
pub fn materialize(cfg: &Config, db: &Db, hash: &str) -> Option<PathBuf> {
    if !is_hash(hash) {
        return None;
    }
    let row = db.with(|c| archive_row(c, hash)).ok()??;
    let cached = cache_path(cfg, hash, row.is_png());
    if cached_ok(&cached, &row) {
        return Some(cached);
    }
    match reconstruct(cfg, &row) {
        Ok(bytes) => {
            if let Err(e) = write_cache(cfg, &cached, &bytes) {
                tracing::warn!(error = %e, "archive: the rebuilt original could not be cached");
                return None;
            }
            Some(cached)
        }
        Err(e) => {
            tracing::warn!(
                reason = e.reason(),
                "archive: an original could not be rebuilt"
            );
            None
        }
    }
}

fn write_cache(cfg: &Config, dst: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = cache_dir(cfg);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(
        ".{}.{:?}.part",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("obj"),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dst)?;
    prune_cache(&dir, dst);
    Ok(())
}

/// Keep the cache under [`CACHE_LIMIT_BYTES`]: oldest first, never `keep`.
fn prune_cache(dir: &Path, keep: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !(m.is_file() && (name.ends_with(".jpg") || name.ends_with(".png"))) {
                return None;
            }
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= CACHE_LIMIT_BYTES {
        return;
    }
    files.sort();
    for (_, len, path) in files {
        if total <= CACHE_LIMIT_BYTES / 4 * 3 {
            break;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Forget the archive copy of a blob that was purged (called with the file removal of the
/// original, see `trash::finish_purge`). Missing files are fine.
pub fn remove_archive_files(cfg: &Config, hash: &str) {
    if !is_hash(hash) {
        return;
    }
    let _ = std::fs::remove_file(jxl_path(cfg, hash));
    let _ = std::fs::remove_file(cache_path(cfg, hash, false));
    let _ = std::fs::remove_file(cache_path(cfg, hash, true));
}

// ---- apply -----------------------------------------------------------------------------------

/// What `--apply` may touch. HEIC, RAW, GIF, WebP, AVIF, TIFF, BMP and video are never touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kinds {
    pub jpeg: bool,
    pub png: bool,
}

impl Default for Kinds {
    fn default() -> Self {
        Kinds {
            jpeg: true,
            png: true,
        }
    }
}

impl Kinds {
    /// `jpeg,png` (any order, any case); nothing else is accepted.
    pub fn parse(list: &[String]) -> anyhow::Result<Kinds> {
        let mut k = Kinds {
            jpeg: false,
            png: false,
        };
        for item in list.iter().flat_map(|s| s.split(',')) {
            match item.trim().to_ascii_lowercase().as_str() {
                "jpeg" | "jpg" => k.jpeg = true,
                "png" => k.png = true,
                other => anyhow::bail!("--kinds takes jpeg and png, not '{other}'"),
            }
        }
        anyhow::ensure!(k.jpeg || k.png, "--kinds needs jpeg, png or both");
        Ok(k)
    }

    fn exts(self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.jpeg {
            v.extend(["jpg", "jpeg", "jpe"]);
        }
        if self.png {
            v.push("png");
        }
        v
    }

    fn allows(self, format: Format) -> bool {
        match format {
            Format::Jpeg => self.jpeg,
            Format::Png => self.png,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApplyOpts {
    /// The owner confirmed that a backup of the store exists (`--i-have-a-backup`). Required.
    pub i_have_a_backup: bool,
    /// Delete each original after its verified round trip (`--delete-originals`).
    pub delete_originals: bool,
    /// Stop after this many converted or deleted files (one batch).
    pub limit: Option<usize>,
    /// Pause while the CPU is at or above this temperature.
    pub max_temp_c: u32,
    /// Which formats to convert (`--kinds`).
    pub kinds: Kinds,
    /// Append one JSON line per processed file here (`--manifest`); failures also go to
    /// `<manifest>.failures.jsonl`.
    pub manifest: Option<PathBuf>,
}

impl ApplyOpts {
    pub fn new(max_temp_c: u32) -> ApplyOpts {
        ApplyOpts {
            i_have_a_backup: false,
            delete_originals: false,
            limit: None,
            max_temp_c,
            kinds: Kinds::default(),
            manifest: None,
        }
    }
}

/// Counts only: no names, hashes or paths.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ApplyStats {
    pub candidates: usize,
    pub converted: usize,
    /// `converted` split by `jpeg` and `png`.
    pub converted_by_kind: std::collections::BTreeMap<String, usize>,
    pub already_archived: usize,
    pub originals_deleted: usize,
    pub original_bytes_converted: u64,
    pub jxl_bytes_written: u64,
    pub original_bytes_freed: u64,
    /// Files this run did not touch on purpose and that stay as they are: TIFF and BMP, and PNG
    /// when `--kinds` leaves it out.
    pub pixel_exact_left_alone: usize,
    /// Files whose sniffed format `--kinds` does not select (a PNG named .jpg, say).
    pub not_selected: usize,
    pub failed: std::collections::BTreeMap<String, usize>,
    /// How many 15-second pauses the CPU temperature limit caused.
    pub temp_pauses: u32,
    /// Lines written to the manifest (0 without `--manifest`).
    pub manifest_lines: usize,
}

struct Candidate {
    hash: String,
    ext: String,
    bytes: i64,
    archived: bool,
    deleted: bool,
}

/// Convert the store's JPEG and PNG originals to archived JXL (and, when asked, delete the originals
/// that proved themselves). Safe to interrupt and to repeat.
pub fn apply(lib: &Library, opts: &ApplyOpts) -> anyhow::Result<ApplyStats> {
    anyhow::ensure!(
        opts.i_have_a_backup,
        "refusing to apply without --i-have-a-backup"
    );
    let cfg = &lib.cfg;
    anyhow::ensure!(
        store::store_available(cfg),
        "the store is not available (marker missing)"
    );
    anyhow::ensure!(
        !cfg.is_quiet(chrono::Utc::now()),
        "quiet hours: the big disk is left alone"
    );
    let exts = opts
        .kinds
        .exts()
        .iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows: Vec<Candidate> = lib.db.with(|c| {
        let mut st = c.prepare(&format!(
            "SELECT b.hash, b.ext, b.bytes, a.hash IS NOT NULL, a.original_deleted_at IS NOT NULL
             FROM blobs b LEFT JOIN archive_blobs a ON a.hash = b.hash
             WHERE b.location = 'store' AND lower(b.ext) IN ({exts})
               AND (EXISTS (SELECT 1 FROM items i WHERE i.hash = b.hash AND i.kind = 'photo')
                    OR EXISTS (SELECT 1 FROM item_versions v WHERE v.hash = b.hash))
             ORDER BY b.hash"
        ))?;
        let r = st.query_map([], |r| {
            Ok(Candidate {
                hash: r.get(0)?,
                ext: r.get(1)?,
                bytes: r.get(2)?,
                archived: r.get(3)?,
                deleted: r.get(4)?,
            })
        })?;
        r.collect()
    })?;
    let mut manifest = opts.manifest.as_deref().map(Manifest::open).transpose()?;
    let mut stats = ApplyStats {
        candidates: rows.len(),
        ..ApplyStats::default()
    };
    let mut worked = 0usize;
    for cand in rows {
        if opts.limit.is_some_and(|l| worked >= l) {
            break;
        }
        if cand.archived && (cand.deleted || !opts.delete_originals) {
            stats.already_archived += 1;
            continue;
        }
        stats.temp_pauses += crate::archive_jxl_measure::wait_for_cool(opts.max_temp_c);
        let step = if cand.archived {
            delete_verified(lib, &cand.hash)
        } else {
            convert_one(lib, &cand, opts)
        };
        match step {
            Ok(Step::Skipped) => stats.not_selected += 1,
            Ok(step) => {
                let (row, freed, converted) = match &step {
                    Step::Converted { row, freed } => (row, *freed, true),
                    Step::Deleted { row, freed } => (row, *freed, false),
                    Step::Skipped => unreachable!("handled above"),
                };
                if converted {
                    stats.converted += 1;
                    *stats
                        .converted_by_kind
                        .entry(if row.is_png() { "png" } else { "jpeg" }.to_string())
                        .or_default() += 1;
                    stats.original_bytes_converted += row.original_bytes as u64;
                    stats.jxl_bytes_written += row.jxl_bytes as u64;
                }
                if freed > 0 || matches!(step, Step::Deleted { .. }) {
                    stats.originals_deleted += 1;
                    stats.original_bytes_freed += freed;
                }
                worked += 1;
                if let Some(m) = manifest.as_mut() {
                    // Re-read the row: deleting set `original_deleted_at` after it was built.
                    let row = lib
                        .db
                        .with(|c| archive_row(c, &cand.hash))?
                        .unwrap_or_else(|| row.clone());
                    stats.manifest_lines += m.record_success(lib, &row)?;
                }
            }
            Err(e) => {
                *stats.failed.entry(e.reason().to_string()).or_default() += 1;
                if let Some(m) = manifest.as_mut() {
                    let row = lib.db.with(|c| archive_row(c, &cand.hash))?;
                    stats.manifest_lines += m.record_failure(
                        lib,
                        &cand.hash,
                        &cand.ext,
                        cand.bytes,
                        row.as_ref(),
                        &format!("{}: {e}", e.reason()),
                    )?;
                }
            }
        }
    }
    if let Some(m) = manifest.as_mut() {
        m.sync()?;
    }
    stats.pixel_exact_left_alone = lib.db.with(|c| {
        c.query_row(
            "SELECT count(*) FROM blobs b WHERE b.location = 'store'
               AND (lower(b.ext) IN ('tif', 'tiff', 'bmp') OR (?1 AND lower(b.ext) = 'png'))
               AND EXISTS (SELECT 1 FROM items i WHERE i.hash = b.hash AND i.kind = 'photo')",
            [!opts.kinds.png],
            |r| r.get::<_, i64>(0),
        )
    })? as usize;
    tracing::info!(
        candidates = stats.candidates,
        converted = stats.converted,
        already = stats.already_archived,
        deleted = stats.originals_deleted,
        failed = stats.failed.values().sum::<usize>(),
        pauses = stats.temp_pauses,
        "archive-jxl apply"
    );
    Ok(stats)
}

enum Step {
    Converted {
        row: ArchiveRow,
        freed: u64,
    },
    Deleted {
        row: ArchiveRow,
        freed: u64,
    },
    /// The file's real format is not one `--kinds` selected.
    Skipped,
}

fn original_file(cfg: &Config, hash: &str, ext: &str) -> PathBuf {
    cfg.store_dir.join(store::object_rel(hash, ext))
}

fn db_err(e: impl std::fmt::Display) -> ConvertError {
    ConvertError::Io(std::io::Error::other(e.to_string()))
}

/// Convert one original and record it; with `--delete-originals`, remove the original after the
/// JXL has been read back from disk and proved it holds the original (a JPEG rebuilds bit for bit,
/// a PNG decodes to the recorded pixel digest).
fn convert_one(lib: &Library, cand: &Candidate, opts: &ApplyOpts) -> Result<Step, ConvertError> {
    let cfg = &lib.cfg;
    let (hash, ext, bytes) = (cand.hash.as_str(), cand.ext.as_str(), cand.bytes);
    let path = original_file(cfg, hash, ext);
    let data = std::fs::read(&path)?;
    // The stored object must be what the catalog says it is, before anything depends on it.
    if data.len() as i64 != bytes || blake3_hex(&data) != hash {
        return Err(ConvertError::Mismatch);
    }
    let decision = decide(ext, &data[..data.len().min(64)]);
    if !opts.kinds.allows(decision.format) {
        return match decision.format {
            Format::Jpeg | Format::Png => Ok(Step::Skipped),
            _ => Err(ConvertError::JpegUnsupported(
                "not a JPEG or PNG".to_string(),
            )),
        };
    }
    let sha256 = sha256_hex(&data);
    let (kind, jxl, reconstructs_to, pixel_digest, png_color, icc) = match decision.format {
        Format::Jpeg => {
            let conv = jpeg_roundtrip(&data)?;
            // The rebuilt file must have the original's SHA-256 as well as its blake3.
            if sha256_hex(&jxl_to_jpeg(&conv.jxl)?) != sha256 {
                return Err(ConvertError::Mismatch);
            }
            (KIND_JPEG, conv.jxl, conv.reconstructs_to, None, None, None)
        }
        _ => {
            let conv = archive_png::png_roundtrip(&data)?;
            (
                KIND_PNG,
                conv.converted.jxl,
                conv.digest.clone(),
                Some(conv.digest),
                Some(conv.color.as_str().to_string()),
                conv.icc,
            )
        }
    };
    let jxl_file = jxl_path(cfg, hash);
    write_durable(&jxl_file, &jxl)?;
    // The copy on disk is what must rebuild the original, not the buffer we still hold.
    let row = ArchiveRow {
        hash: hash.to_string(),
        kind: kind.to_string(),
        jxl_hash: blake3_hex(&jxl),
        jxl_bytes: jxl.len() as i64,
        reconstructs_to,
        original_bytes: bytes,
        original_ext: ext.to_string(),
        verified_at: now_str(),
        original_deleted_at: None,
        original_sha256: Some(sha256),
        pixel_digest,
        png_color,
        icc,
    };
    if let Err(e) = reconstruct(cfg, &row) {
        let _ = std::fs::remove_file(&jxl_file);
        return Err(e);
    }
    lib.db.with(|c| insert_row(c, &row)).map_err(db_err)?;
    let freed = if opts.delete_originals {
        remove_original(lib, &row)?
    } else {
        0
    };
    Ok(Step::Converted { row, freed })
}

/// A second pass for rows converted earlier: verify from disk again, then delete the original.
fn delete_verified(lib: &Library, hash: &str) -> Result<Step, ConvertError> {
    let cfg = &lib.cfg;
    let mut row = lib
        .db
        .with(|c| archive_row(c, hash))
        .map_err(db_err)?
        .ok_or(ConvertError::Mismatch)?;
    let rebuilt = reconstruct(cfg, &row)?;
    // A JPEG row converted before the SHA-256 was recorded gets it from the verified rebuild
    // (its blake3 matched the item's hash).
    if row.original_sha256.is_none() && !row.is_png() {
        let sha = sha256_hex(&rebuilt);
        lib.db
            .with(|c| {
                c.execute(
                    "UPDATE archive_blobs SET original_sha256 = ?2 WHERE hash = ?1",
                    params![hash, sha],
                )
            })
            .map_err(db_err)?;
        row.original_sha256 = Some(sha);
    }
    let freed = remove_original(lib, &row)?;
    Ok(Step::Deleted { row, freed })
}

/// Delete the original file of an archived hash. The file must still be the very object that was
/// verified (same hash), the catalog row is marked, and only then does the file go.
fn remove_original(lib: &Library, row: &ArchiveRow) -> Result<u64, ConvertError> {
    let path = original_file(&lib.cfg, &row.hash, &row.original_ext);
    match std::fs::read(&path) {
        Ok(data) => {
            if blake3_hex(&data) != row.hash
                || row
                    .original_sha256
                    .as_deref()
                    .is_some_and(|want| sha256_hex(&data) != want)
            {
                return Err(ConvertError::Mismatch);
            }
        }
        // Already gone (an interrupted earlier run): just record it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE archive_blobs SET original_deleted_at = ?2 WHERE hash = ?1",
                params![row.hash, now_str()],
            )
        })
        .map_err(db_err)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(row.original_bytes as u64),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RestoreStats {
    /// JPEG originals rebuilt into the store, byte for byte.
    pub restored: usize,
    /// PNG rows whose original is gone: a PNG cannot be put back byte for byte, so these stay
    /// served from their JXL (restore them from the backup).
    pub png_not_restorable: usize,
}

/// Rollback after deletion: rebuild every deleted JPEG original from its JXL back into the store
/// (verified), and clear the mark. A deleted PNG original cannot be rebuilt byte for byte; those
/// rows are left as they are and counted.
pub fn restore_originals(lib: &Library) -> anyhow::Result<RestoreStats> {
    let cfg = &lib.cfg;
    let rows: Vec<ArchiveRow> = lib.db.with(|c| {
        let mut st = c.prepare(
            "SELECT hash FROM archive_blobs WHERE original_deleted_at IS NOT NULL ORDER BY hash",
        )?;
        let hashes = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for h in hashes {
            if let Some(row) = archive_row(c, &h?)? {
                out.push(row);
            }
        }
        Ok(out)
    })?;
    let mut stats = RestoreStats::default();
    for row in rows {
        if row.is_png() {
            stats.png_not_restorable += 1;
            continue;
        }
        let bytes = reconstruct(cfg, &row)?;
        let dst = original_file(cfg, &row.hash, &row.original_ext);
        write_durable(&dst, &bytes)?;
        lib.db.with(|c| {
            c.execute(
                "UPDATE archive_blobs SET original_deleted_at = NULL WHERE hash = ?1",
                [&row.hash],
            )
        })?;
        stats.restored += 1;
    }
    tracing::info!(
        restored = stats.restored,
        png_not_restorable = stats.png_not_restorable,
        "archive-jxl restore"
    );
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic photo-like JPEG (gradients and noise, so the coefficients are not trivial).
    pub fn synthetic_jpeg(w: u32, h: u32, quality: u8, seed: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(w, h);
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
        for (x, y, p) in img.enumerate_pixels_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = (s >> 28) as u8;
            *p = image::Rgb([
                ((x * 255 / w) as u8).wrapping_add(n),
                ((y * 255 / h) as u8).wrapping_add(n / 2),
                (((x + y) * 255 / (w + h)) as u8).wrapping_add(n),
            ]);
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn the_decision_per_format() {
        let jpeg = synthetic_jpeg(16, 16, 80, 1);
        let d = decide("jpg", &jpeg);
        assert_eq!(
            (d.format, d.action, d.display_derivative),
            (Format::Jpeg, ArchiveAction::JpegRecompress, false)
        );
        // The extension does not decide: a JPEG called .png is still a JPEG.
        assert_eq!(decide("png", &jpeg).action, ArchiveAction::JpegRecompress);
        let png = b"\x89PNG\r\n\x1a\n....";
        assert_eq!(
            decide("png", png).action,
            ArchiveAction::LosslessJxl,
            "png is lossless"
        );
        assert_eq!(decide("tif", b"II*\0rest").format, Format::Tiff);
        assert_eq!(decide("bmp", b"BM......").format, Format::Bmp);
        assert_eq!(decide("dat", b"BM......").format, Format::Other);
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic";
        let d = decide("heic", heic);
        assert_eq!(
            (d.format, d.action, d.display_derivative),
            (Format::Heic, ArchiveAction::KeepOriginal, true)
        );
        // AVIF shares the HEIF container and brands; it is already AV1, so it is just kept.
        let avif = b"\0\0\0\x1cftypavif\0\0\0\0mif1avif";
        let d = decide("avif", avif);
        assert_eq!(
            (d.format, d.action, d.display_derivative),
            (Format::Avif, ArchiveAction::KeepOriginal, false)
        );
        // DNG is TIFF-shaped but a RAW: kept, with a display derivative.
        let d = decide("DNG", b"II*\0rest");
        assert_eq!(
            (d.format, d.action, d.display_derivative),
            (Format::Raw, ArchiveAction::KeepOriginal, true)
        );
        assert_eq!(decide("gif", b"GIF89a").action, ArchiveAction::KeepOriginal);
        assert_eq!(decide("webp", b"RIFF\0\0\0\0WEBPVP8 ").format, Format::Webp);
        assert_eq!(decide("mp4", b"\0\0\0\x18ftypisom").format, Format::Other);
        assert_eq!(decide("", b"").action, ArchiveAction::KeepOriginal);
    }

    #[test]
    fn a_jpeg_round_trips_bit_exact_and_gets_smaller() {
        let jpeg = synthetic_jpeg(160, 120, 85, 7);
        let c = jpeg_roundtrip(&jpeg).unwrap();
        assert!(c.byte_exact);
        assert_eq!(c.reconstructs_to, blake3_hex(&jpeg));
        assert!(
            c.jxl.len() < jpeg.len(),
            "{} !< {}",
            c.jxl.len(),
            jpeg.len()
        );
        assert_eq!(jxl_to_jpeg(&c.jxl).unwrap(), jpeg);
    }

    #[test]
    fn a_jpeg_with_trailing_bytes_still_rebuilds() {
        // Phones append data after the end-of-image marker (MPF, gain maps).
        let mut jpeg = synthetic_jpeg(64, 64, 90, 3);
        jpeg.extend_from_slice(b"TRAILER-DATA-AFTER-EOI-0123456789");
        match jpeg_roundtrip(&jpeg) {
            Ok(c) => assert_eq!(jxl_to_jpeg(&c.jxl).unwrap(), jpeg),
            // Or it is refused: never a quietly different file.
            Err(e) => assert!(
                matches!(e, ConvertError::JpegUnsupported(_) | ConvertError::Mismatch),
                "{e}"
            ),
        }
    }

    #[test]
    fn a_non_jpeg_and_a_pixel_only_jxl_are_refused() {
        assert!(matches!(
            jpeg_roundtrip(b"not a jpeg at all"),
            Err(ConvertError::JpegUnsupported(_))
        ));
        // A lossless-pixels JXL has nothing to rebuild a JPEG from.
        let r = Raster {
            width: 4,
            height: 4,
            alpha: false,
            profile: Profile::Srgb,
            samples: Samples::U8(vec![9; 48]),
        };
        let px = raster_to_jxl(&r).unwrap();
        assert!(matches!(
            jxl_to_jpeg(&px),
            Err(ConvertError::NoReconstruction)
        ));
    }

    fn raster(w: u32, h: u32, alpha: bool, wide: bool, seed: u32) -> Raster {
        let ch = if alpha { 4 } else { 3 };
        let n = (w * h * ch) as usize;
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(17);
        let mut next = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            s >> 16
        };
        let samples = if wide {
            Samples::U16((0..n).map(|_| next() as u16).collect())
        } else {
            Samples::U8(
                (0..n)
                    .map(|i| ((i as u32 / 7 + next() % 3) % 256) as u8)
                    .collect(),
            )
        };
        Raster {
            width: w,
            height: h,
            alpha,
            profile: Profile::Srgb,
            samples,
        }
    }

    #[test]
    fn lossless_pixels_survive_every_shape() {
        for (alpha, wide) in [(false, false), (true, false), (false, true), (true, true)] {
            let r = raster(37, 23, alpha, wide, 5);
            let c = lossless_roundtrip(&r).expect("lossless");
            assert!(!c.byte_exact);
            assert_eq!(c.reconstructs_to, pixel_hash(&r));
            assert_eq!(jxl_to_samples(&c.jxl, &r).unwrap(), r.samples);
        }
    }

    #[test]
    fn invisible_colour_under_transparent_pixels_is_kept() {
        let mut r = raster(16, 16, true, false, 9);
        if let Samples::U8(v) = &mut r.samples {
            for px in v.chunks_mut(4) {
                px[3] = 0; // fully transparent, colour still there
            }
        }
        assert!(lossless_roundtrip(&r).is_ok());
    }

    #[test]
    fn a_png_file_loads_to_a_raster_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.png");
        let mut img = image::RgbaImage::new(31, 17);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 8) as u8, (y * 15) as u8, ((x ^ y) * 9) as u8, 200]);
        }
        img.save(&p).unwrap();
        let r = load_raster(&p).unwrap();
        assert!(r.alpha);
        assert_eq!(r.profile, Profile::Srgb);
        let c = lossless_roundtrip(&r).unwrap();
        assert_eq!(jxl_to_samples(&c.jxl, &r).unwrap(), r.samples);
        // Grey becomes three equal channels.
        let g = tmp.path().join("g.png");
        image::GrayImage::from_fn(9, 9, |x, y| image::Luma([(x * 20 + y) as u8]))
            .save(&g)
            .unwrap();
        let r = load_raster(&g).unwrap();
        assert!(!r.alpha);
        let Samples::U8(v) = &r.samples else { panic!() };
        assert!(v.chunks(3).all(|c| c[0] == c[1] && c[1] == c[2]));
        assert!(lossless_roundtrip(&r).is_ok());
    }

    /// A minimal matrix/TRC RGB ICC profile with the given colorants (D50 XYZ) and tone curve.
    fn icc(r: [f64; 3], g: [f64; 3], b: [f64; 3], trc: &[u8]) -> Vec<u8> {
        let fixed = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
        let xyz = |c: [f64; 3]| {
            let mut t = b"XYZ \0\0\0\0".to_vec();
            for v in c {
                t.extend_from_slice(&fixed(v));
            }
            t
        };
        let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"rXYZ", xyz(r)),
            (b"gXYZ", xyz(g)),
            (b"bXYZ", xyz(b)),
            (b"rTRC", trc.to_vec()),
            (b"gTRC", trc.to_vec()),
            (b"bTRC", trc.to_vec()),
        ];
        let mut out = vec![0u8; 128];
        out[16..20].copy_from_slice(b"RGB ");
        out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut offset = 132 + tags.len() * 12;
        let mut body = Vec::new();
        for (sig, data) in &tags {
            out.extend_from_slice(*sig);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len();
            body.extend_from_slice(data);
        }
        out.extend_from_slice(&body);
        out
    }

    fn para_srgb() -> Vec<u8> {
        let mut t = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for v in [2.4f64, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            t.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
        }
        t
    }

    fn curv_table(gamma: f64) -> Vec<u8> {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend_from_slice(&1024u32.to_be_bytes());
        for i in 0..1024 {
            let x = i as f64 / 1023.0;
            let y = if gamma == 0.0 {
                if x <= 0.04045 {
                    x / 12.92
                } else {
                    ((x + 0.055) / 1.055).powf(2.4)
                }
            } else {
                x.powf(gamma)
            };
            t.extend_from_slice(&((y * 65535.0).round() as u16).to_be_bytes());
        }
        t
    }

    const SRGB_D50: ([f64; 3], [f64; 3], [f64; 3]) = (
        [0.4361, 0.2225, 0.0139],
        [0.3851, 0.7169, 0.0971],
        [0.1431, 0.0606, 0.7141],
    );
    const P3_D50: ([f64; 3], [f64; 3], [f64; 3]) = (
        [0.5151, 0.2412, -0.0011],
        [0.2920, 0.6922, 0.0419],
        [0.1571, 0.0666, 0.7841],
    );

    #[test]
    fn colour_profiles_are_srgb_or_display_p3_only() {
        assert_eq!(profile_of(None).unwrap(), Profile::Srgb);
        let (r, g, b) = SRGB_D50;
        assert_eq!(
            profile_of(Some(&icc(r, g, b, &para_srgb()))).unwrap(),
            Profile::Srgb
        );
        assert_eq!(
            profile_of(Some(&icc(r, g, b, &curv_table(0.0)))).unwrap(),
            Profile::Srgb,
            "a table profile (the old HP/Microsoft sRGB style)"
        );
        let (r, g, b) = P3_D50;
        assert_eq!(
            profile_of(Some(&icc(r, g, b, &para_srgb()))).unwrap(),
            Profile::DisplayP3,
            "judged by colorants, whatever the profile is called"
        );
        // Right primaries, wrong tone curve (a pure 2.2 gamma): refused, not tagged as sRGB.
        assert!(profile_of(Some(&icc(r, g, b, &curv_table(2.2)))).is_err());
        // Adobe RGB colorants, a grey profile, garbage and names alone are all refused.
        let adobe = icc(
            [0.6097, 0.3111, 0.0195],
            [0.2053, 0.6257, 0.0609],
            [0.1492, 0.0632, 0.7446],
            &para_srgb(),
        );
        assert!(matches!(
            profile_of(Some(&adobe)),
            Err(ConvertError::UnsupportedProfile)
        ));
        assert!(profile_of(Some(b"....Display P3....")).is_err());
        assert!(profile_of(Some(&[0u8; 200])).is_err());
    }

    #[test]
    fn display_p3_is_tagged_in_the_jxl() {
        let mut p3 = raster(8, 8, false, false, 2);
        p3.profile = Profile::DisplayP3;
        let srgb = Raster {
            profile: Profile::Srgb,
            ..p3.clone()
        };
        let icc_of = |r: &Raster| {
            let jxl = lossless_roundtrip(r).unwrap().jxl;
            let dec = jpegxl_rs::decoder_builder()
                .icc_profile(true)
                .build()
                .unwrap();
            dec.decode(&jxl).unwrap().0.icc_profile.unwrap()
        };
        assert_ne!(icc_of(&p3), icc_of(&srgb), "P3 must not collapse to sRGB");
    }

    #[test]
    fn the_display_derivative_is_smaller_and_capped() {
        let big = DynamicImage::ImageRgb8(image::RgbImage::from_fn(2700, 300, |x, y| {
            image::Rgb([(x / 12) as u8, (y / 8) as u8, ((x + y) / 20) as u8])
        }));
        let jxl = display_derivative(&big).unwrap();
        let dec = jpegxl_rs::decoder_builder().build().unwrap();
        let (meta, _) = dec.decode(&jxl).unwrap();
        assert_eq!(meta.width.max(meta.height), DISPLAY_LONG_SIDE);
        let small = DynamicImage::ImageRgb8(image::RgbImage::new(300, 200));
        let (meta, _) = dec.decode(&display_derivative(&small).unwrap()).unwrap();
        assert_eq!((meta.width, meta.height), (300, 200), "never upscaled");
    }
}
