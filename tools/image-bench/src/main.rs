//! Gallery rendition formats on the owner's real photos.
//!
//! `image-bench <file>...` decodes each file (JPEG, PNG, WebP; HEIC goes in as the phone's
//! JPEG preview), makes the grid thumbnail (256 px square, centre crop) and the full-screen
//! preview (1440 px long side), encodes both as JPEG, WebP, AVIF and JPEG XL, and prints
//! the sizes and encode times, the JXL decode time with jxl-oxide, and the ThumbHash size.

use std::time::Instant;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageReader, RgbImage};

fn jpeg(img: &RgbImage, q: u8) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q).encode_image(img).unwrap();
    out
}

fn webp(img: &RgbImage, q: f32) -> Vec<u8> {
    webp::Encoder::from_rgb(img.as_raw(), img.width(), img.height()).encode(q).to_vec()
}

fn avif(img: &RgbImage, q: f32) -> Vec<u8> {
    let px: Vec<rgb::RGB8> = img.pixels().map(|p| rgb::RGB8::new(p[0], p[1], p[2])).collect();
    let buf = ravif::Img::new(px.as_slice(), img.width() as usize, img.height() as usize);
    ravif::Encoder::new().with_quality(q).with_speed(6).encode_rgb(buf).unwrap().avif_file
}

fn jxl(img: &RgbImage, distance: f32, progressive: bool) -> Vec<u8> {
    use jpegxl_rs::encode::EncoderSpeed;
    use jpegxl_sys::encoder::encode::JxlEncoderFrameSettingId as Opt;
    let mut enc = jpegxl_rs::encoder_builder().quality(distance).speed(EncoderSpeed::Squirrel).build().unwrap();
    if progressive {
        enc.set_frame_option(Opt::Responsive, 1);
        enc.set_frame_option(Opt::ProgressiveAc, 1);
        enc.set_frame_option(Opt::ProgressiveDc, 1);
    }
    enc.encode::<u8>(img.as_raw(), img.width(), img.height()).unwrap()
}

fn time<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed().as_secs_f64() * 1000.0)
}

fn grid(img: &DynamicImage) -> RgbImage {
    img.resize_to_fill(256, 256, FilterType::Lanczos3).to_rgb8()
}

fn preview(img: &DynamicImage) -> RgbImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= 1440 {
        img.to_rgb8()
    } else {
        img.resize(1440, 1440, FilterType::Lanczos3).to_rgb8()
    }
}

/// SSIMULACRA2 of `dist` against `reference` (higher is better; ~70 good, ~80 very good, 90 visually lossless).
fn score(reference: &RgbImage, dist: &[f32]) -> f64 {
    use ssimulacra2::{compute_frame_ssimulacra2, ColorPrimaries, Rgb, TransferCharacteristic};
    let (w, h) = (reference.width() as usize, reference.height() as usize);
    let r: Vec<[f32; 3]> = reference.pixels().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]).collect();
    let d: Vec<[f32; 3]> = dist.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let a = Rgb::new(r, w, h, TransferCharacteristic::SRGB, ColorPrimaries::BT709).unwrap();
    let b = Rgb::new(d, w, h, TransferCharacteristic::SRGB, ColorPrimaries::BT709).unwrap();
    compute_frame_ssimulacra2(a, b).unwrap_or(f64::NAN)
}

fn decode_image_crate(bytes: &[u8]) -> Vec<f32> {
    image::load_from_memory(bytes).unwrap().to_rgb8().as_raw().iter().map(|v| *v as f32 / 255.0).collect()
}

fn decode_jxl(bytes: &[u8]) -> Vec<f32> {
    let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(bytes)).unwrap();
    let r = img.render_frame(0).unwrap();
    let fb = r.image_all_channels();
    let ch = fb.channels();
    fb.buf().chunks_exact(ch).flat_map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0), p[2].clamp(0.0, 1.0)]).collect()
}

struct Row2 {
    name: String,
    bytes: usize,
    ms: f64,
    score: f64,
}

fn encode_all(img: &RgbImage, progressive: bool) -> Vec<Row2> {
    let mut rows = Vec::new();
    for q in [70u8, 80, 90] {
        let (b, ms) = time(|| jpeg(img, q));
        rows.push(Row2 { name: format!("jpeg q{q}"), bytes: b.len(), ms, score: score(img, &decode_image_crate(&b)) });
    }
    for q in [70.0f32, 80.0, 90.0] {
        let (b, ms) = time(|| webp(img, q));
        rows.push(Row2 { name: format!("webp q{q}"), bytes: b.len(), ms, score: score(img, &decode_image_crate(&b)) });
    }
    for d in [1.0f32, 1.5, 2.5, 3.5] {
        let (b, ms) = time(|| jxl(img, d, progressive));
        rows.push(Row2 { name: format!("jxl d{d}"), bytes: b.len(), ms, score: score(img, &decode_jxl(&b)) });
    }
    let (b, ms) = time(|| avif(img, 60.0));
    rows.push(Row2 { name: "avif q60 (no score)".into(), bytes: b.len(), ms, score: f64::NAN });
    rows
}

fn jxl_decode_ms(bytes: &[u8]) -> f64 {
    let (_, ms) = time(|| {
        let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(bytes)).unwrap();
        let r = img.render_frame(0).unwrap();
        r.image_all_channels().buf().len()
    });
    ms
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let mut totals: Vec<(String, usize, f64, usize, f64)> = Vec::new();
    let mut add = |key: String, bytes: usize, ms: f64, sc: f64| {
        let sc = if sc.is_nan() { 0.0 } else { sc };
        if let Some(t) = totals.iter_mut().find(|t| t.0 == key) {
            t.1 += bytes;
            t.2 += ms;
            t.3 += 1;
            t.4 += sc;
        } else {
            totals.push((key, bytes, ms, 1, sc));
        }
    };
    let mut decode_ms = Vec::new();
    let mut th_bytes = Vec::new();
    for f in &files {
        let Ok(img) = ImageReader::open(f).and_then(|r| r.with_guessed_format()).map_err(|e| e.to_string()).and_then(|r| r.decode().map_err(|e| e.to_string())) else {
            eprintln!("skip {f}");
            continue;
        };
        let (w, h) = img.dimensions();
        let src = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        println!("{f}: {w}x{h}, {} KB", src / 1024);
        for (label, r, prog) in [("grid 256", grid(&img), false), ("preview 1440", preview(&img), true)] {
            for row in encode_all(&r, prog) {
                println!("  {label:12} {:20} {:>7.1} KB {:>8.1} ms  ssimulacra2 {:>5.1}", row.name, row.bytes as f64 / 1024.0, row.ms, row.score);
                add(format!("{label} {}", row.name), row.bytes, row.ms, row.score);
            }
            if label == "preview 1440" {
                let b = jxl(&r, 2.0, true);
                decode_ms.push(jxl_decode_ms(&b));
            }
        }
        let small = img.resize(100, 100, FilterType::Triangle).to_rgba8();
        let th = thumbhash::rgba_to_thumb_hash(small.width() as usize, small.height() as usize, small.as_raw());
        th_bytes.push(th.len());
    }
    println!("\naverages over {} photos", th_bytes.len());
    for (k, b, ms, n, sc) in &totals {
        println!("  {k:36} {:>7.1} KB {:>8.1} ms  ssimulacra2 {:>5.1}", *b as f64 / *n as f64 / 1024.0, ms / *n as f64, sc / *n as f64);
    }
    if !decode_ms.is_empty() {
        println!("  jxl-oxide decode 1440 px      {:>8.1} ms avg", decode_ms.iter().sum::<f64>() / decode_ms.len() as f64);
    }
    if !th_bytes.is_empty() {
        println!("  thumbhash                     {:>5.1} bytes avg", th_bytes.iter().sum::<usize>() as f64 / th_bytes.len() as f64);
    }
}
