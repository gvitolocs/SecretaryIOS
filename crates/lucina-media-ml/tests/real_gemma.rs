//! The real EmbeddingGemma 2 export (fp32 ONNX, `~/models/embeddinggemma-2-onnx`) on synthetic
//! pictures (plain colours, drawn shapes) and on the public XM3600 evaluation set. Needs the
//! model and ONNX Runtime (`LUCINA_MEDIA_ML_ORT_LIB`, `LUCINA_MEDIA_ML_GEMMA_DIR`, see
//! `gemma.rs`), so every test is ignored by default:
//! `LUCINA_MEDIA_ML_THREADS=1 cargo test -p lucina-media-ml --test real_gemma -- --ignored --nocapture --test-threads=1`.
//!
//! The golden check compares with the evaluation harness's vectors for XM3600 only
//! (`$LUCINA_GEMMA_EVAL_DIR`, default `~/scratch/gallery/embed-eval`: `out/xm_img_gemma_b70.*`,
//! `out/xm_txt_gemma.*`, `xm3600/queries.tsv`, `xm3600/images`). Nothing of a private library.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use image::{Rgb, RgbImage};
use lucina_media_ml::categories::{Category, PromptVectors, ZeroShot};
use lucina_media_ml::gemma::{self, Gemma, GemmaConfig};
use lucina_media_ml::siglip::{Siglip, SiglipConfig};
use lucina_media_ml::vector::{dot, normalize};
use lucina_media_ml::Embedder;

fn solid(rgb: [u8; 3]) -> RgbImage {
    RgbImage::from_pixel(320, 240, Rgb(rgb))
}

/// A white picture with a black filled circle or square in the middle.
fn shape(circle: bool) -> RgbImage {
    let mut img = RgbImage::from_pixel(300, 300, Rgb([255, 255, 255]));
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (dx, dy) = (x as f32 - 150.0, y as f32 - 150.0);
        let inside = if circle {
            dx * dx + dy * dy < 90.0 * 90.0
        } else {
            dx.abs() < 85.0 && dy.abs() < 85.0
        };
        if inside {
            *p = Rgb([0, 0, 0]);
        }
    }
    img
}

fn rect(img: &mut RgbImage, x: u32, y: u32, w: u32, h: u32, c: [u8; 3]) {
    for yy in y..(y + h).min(img.height()) {
        for xx in x..(x + w).min(img.width()) {
            img.put_pixel(xx, yy, Rgb(c));
        }
    }
}

/// A white page with grey lines of "text" and a heading bar.
fn page() -> RgbImage {
    let mut img = RgbImage::from_pixel(300, 400, Rgb([250, 250, 250]));
    rect(&mut img, 40, 30, 140, 14, [30, 30, 30]);
    for i in 0..14 {
        rect(
            &mut img,
            40,
            70 + i * 22,
            220 - (i % 3) * 30,
            6,
            [90, 90, 90],
        );
    }
    img
}

/// A phone-like screen: dark status bar, a title, rows with an icon and two lines of text.
fn screen() -> RgbImage {
    let mut img = RgbImage::from_pixel(240, 480, Rgb([245, 245, 247]));
    rect(&mut img, 0, 0, 240, 28, [20, 20, 24]);
    rect(&mut img, 16, 44, 120, 16, [20, 20, 20]);
    for i in 0..6 {
        let y = 80 + i * 56;
        rect(&mut img, 12, y, 216, 48, [255, 255, 255]);
        rect(&mut img, 20, y + 8, 32, 32, [0, 122, 255]);
        rect(&mut img, 64, y + 12, 110, 8, [60, 60, 60]);
        rect(&mut img, 64, y + 28, 70, 6, [150, 150, 150]);
    }
    rect(&mut img, 0, 440, 240, 40, [250, 250, 250]);
    img
}

/// A map-like picture: green land, a blue river, yellow and white roads.
fn map() -> RgbImage {
    let mut img = RgbImage::from_pixel(320, 240, Rgb([226, 236, 214]));
    rect(&mut img, 0, 150, 320, 26, [150, 195, 235]);
    for i in 0..6 {
        rect(&mut img, 20 + i * 55, 0, 6, 240, [255, 255, 255]);
        rect(&mut img, 0, 20 + i * 36, 320, 5, [255, 255, 255]);
    }
    rect(&mut img, 0, 100, 320, 9, [250, 215, 90]);
    rect(&mut img, 140, 0, 9, 240, [250, 215, 90]);
    img
}

fn gemma() -> Gemma {
    let cfg = GemmaConfig::from_env();
    if let Some(p) = cfg.missing() {
        panic!("{} is missing", p.display());
    }
    Gemma::load(cfg).unwrap()
}

fn eval_dir() -> PathBuf {
    std::env::var("LUCINA_GEMMA_EVAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap()).join("scratch/gallery/embed-eval")
        })
}

/// Vectors the harness wrote: `<base>.json` (keys, dim) and `<base>.f32` (little-endian rows).
fn load_vecs(base: &str) -> (Vec<String>, Vec<Vec<f32>>) {
    let dir = eval_dir().join("out");
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{base}.json"))).unwrap())
            .unwrap();
    let keys: Vec<String> = meta["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    let dim = meta["dim"].as_u64().unwrap() as usize;
    let raw = std::fs::read(dir.join(format!("{base}.f32"))).unwrap();
    let data: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    assert_eq!(data.len(), keys.len() * dim);
    let rows = data.chunks_exact(dim).map(|r| r.to_vec()).collect();
    (keys, rows)
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    let (mut x, mut y) = (a.to_vec(), b.to_vec());
    normalize(&mut x);
    normalize(&mut y);
    dot(&x, &y)
}

#[test]
#[ignore]
fn the_sessions_have_the_documented_tensors() {
    let m = gemma();
    let sig = m.io_signature().unwrap();
    for l in &sig {
        eprintln!("{l}");
    }
    let has = |needle: &str| sig.iter().any(|l| l.contains(needle));
    assert!(has("vision_encoder in  pixel_values"));
    assert!(has("vision_encoder in  pixel_position_ids"));
    assert!(has("vision_encoder out image_features"));
    for n in [
        "input_ids",
        "attention_mask",
        "image_features",
        "video_features",
        "audio_features",
    ] {
        assert!(has(&format!("model in  {n}")), "{n}");
    }
    assert!(has("model out sentence_embedding"));
    // The tokenizer wraps a query in <bos> … <eos>.
    let ids = m.query_ids("a red picture").unwrap();
    eprintln!("query ids: {ids:?}");
    assert_eq!(ids[0], 2);
    assert_eq!(*ids.last().unwrap(), 1);
    assert!(ids.len() > 8);
}

/// The query that a cosine `margin` is measured with: the best right pick minus the best wrong one.
fn margin(q: &[f32], pics: &[(&str, Vec<f32>)], want: &str) -> f32 {
    let right = pics
        .iter()
        .find(|p| p.0 == want)
        .map(|p| dot(q, &p.1))
        .unwrap();
    let wrong = pics
        .iter()
        .filter(|p| p.0 != want)
        .map(|p| dot(q, &p.1))
        .fold(f32::MIN, f32::max);
    right - wrong
}

#[test]
#[ignore]
fn matches_words_to_pictures_in_three_languages() {
    let m = gemma();
    let names = ["red", "green", "blue", "circle", "square"];
    let images = vec![
        solid([220, 20, 30]),
        solid([30, 180, 40]),
        solid([20, 40, 220]),
        shape(true),
        shape(false),
    ];
    m.embed_images(&images[..1]).unwrap();
    let t0 = Instant::now();
    let vecs = m.embed_images(&images).unwrap();
    let per = t0.elapsed().as_millis() as f64 / images.len() as f64;
    eprintln!("5 pictures, {per:.0} ms each");
    for v in &vecs {
        assert_eq!(v.len(), gemma::DIM);
        assert!((dot(v, v) - 1.0).abs() < 1e-3);
    }
    let pics: Vec<(&str, Vec<f32>)> = names.iter().copied().zip(vecs).collect();
    let (mut with, mut without) = (0.0, 0.0);
    let cases = [
        ("a red picture", "red"),
        ("a green picture", "green"),
        ("a blue picture", "blue"),
        ("una foto rossa", "red"),
        ("una foto verde", "green"),
        ("et blåt billede", "blue"),
        ("et grønt billede", "green"),
        ("a black circle", "circle"),
        ("a black square", "square"),
        ("un cerchio nero", "circle"),
        ("en sort firkant", "square"),
    ];
    for (query, want) in cases {
        let q = m.embed_text(query).unwrap();
        let (got, cos) = pics
            .iter()
            .map(|(n, v)| (*n, dot(&q, v)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        eprintln!(
            "{query:>18} → {got} (cos {cos:.3}, relevance {:.3})",
            gemma::relevance(cos)
        );
        assert_eq!(got, want, "{query}");
        // The same text as a description (no task prefix).
        let plain = m.embed_plain(query).unwrap();
        with += margin(&q, &pics, want);
        without += margin(&plain, &pics, want);
    }
    let n = cases.len() as f32;
    eprintln!(
        "mean margin right − best wrong: with the query prefix {:.4}, without {:.4}",
        with / n,
        without / n
    );
    let t0 = Instant::now();
    for _ in 0..10 {
        m.embed_text("a dog on the beach").unwrap();
    }
    eprintln!(
        "one query (model loaded) in {:.0} ms",
        t0.elapsed().as_millis() as f64 / 10.0
    );
}

/// A tall narrow receipt: white paper, lines of text, a bold total, on a grey table.
fn receipt() -> RgbImage {
    let mut img = RgbImage::from_pixel(320, 420, Rgb([120, 118, 114]));
    rect(&mut img, 90, 10, 140, 400, [252, 252, 250]);
    rect(&mut img, 120, 24, 80, 10, [20, 20, 20]);
    for i in 0..16 {
        rect(&mut img, 102, 56 + i * 15, 70, 5, [70, 70, 70]);
        rect(&mut img, 190, 56 + i * 15, 28, 5, [70, 70, 70]);
    }
    rect(&mut img, 102, 310, 60, 9, [0, 0, 0]);
    rect(&mut img, 186, 310, 32, 9, [0, 0, 0]);
    img
}

/// A QR-code-like black and white grid with the three corner squares, on white.
fn ticket() -> RgbImage {
    let mut img = RgbImage::from_pixel(300, 300, Rgb([255, 255, 255]));
    let cell = 8;
    let mut seed = 12345u32;
    for gy in 0..25u32 {
        for gx in 0..25u32 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            if seed >> 28 > 7 {
                rect(
                    &mut img,
                    50 + gx * cell,
                    50 + gy * cell,
                    cell,
                    cell,
                    [0, 0, 0],
                );
            }
        }
    }
    for (cx, cy) in [(50, 50), (50 + 18 * cell, 50), (50, 50 + 18 * cell)] {
        rect(&mut img, cx, cy, 7 * cell, 7 * cell, [0, 0, 0]);
        rect(
            &mut img,
            cx + cell,
            cy + cell,
            5 * cell,
            5 * cell,
            [255, 255, 255],
        );
        rect(
            &mut img,
            cx + 2 * cell,
            cy + 2 * cell,
            3 * cell,
            3 * cell,
            [0, 0, 0],
        );
    }
    img
}

/// A whiteboard: white with a few coloured strokes and boxes.
fn board() -> RgbImage {
    let mut img = RgbImage::from_pixel(320, 240, Rgb([246, 247, 248]));
    rect(&mut img, 30, 30, 120, 6, [20, 60, 200]);
    rect(&mut img, 30, 60, 6, 80, [20, 60, 200]);
    rect(&mut img, 60, 60, 100, 6, [200, 30, 30]);
    rect(&mut img, 60, 90, 140, 6, [30, 30, 30]);
    rect(&mut img, 200, 120, 80, 60, [30, 30, 30]);
    rect(&mut img, 204, 124, 72, 52, [246, 247, 248]);
    rect(&mut img, 40, 180, 160, 6, [20, 60, 200]);
    img
}

struct Prefixed<'a>(&'a Gemma, &'static str);

impl Embedder for Prefixed<'_> {
    fn model(&self) -> &str {
        "gemma-prompt-variant"
    }
    fn dim(&self) -> usize {
        gemma::DIM
    }
    fn embed_images(&self, i: &[RgbImage]) -> Result<Vec<Vec<f32>>, lucina_media_ml::MlError> {
        self.0.embed_images(i)
    }
    fn embed_text(&self, t: &str) -> Result<Vec<f32>, lucina_media_ml::MlError> {
        self.0.embed_plain(&format!("{}{t}", self.1))
    }
    fn relevance(&self, c: f32) -> f32 {
        gemma::relevance(c)
    }
}

/// Which category the prompts pick for crude pictures, with the prompts as search queries, as
/// plain descriptions and with the model card's classification task prefix, and the margin of
/// the right category over the best wrong one.
#[test]
#[ignore]
fn category_prompts_with_and_without_a_prefix() {
    let m = gemma();
    let pics = [
        ("screen", Category::Screenshot, screen()),
        ("page", Category::Document, page()),
        ("map", Category::Map, map()),
        ("receipt", Category::Receipt, receipt()),
        ("qr", Category::Ticket, ticket()),
        ("board", Category::Note, board()),
    ];
    let vecs: Vec<Vec<f32>> = pics
        .iter()
        .map(|p| {
            m.embed_images(std::slice::from_ref(&p.2))
                .unwrap()
                .remove(0)
        })
        .collect();
    for (label, prefix) in [
        ("search query", QUERY),
        ("plain", ""),
        ("classification", "task: classification | query: "),
    ] {
        let pv = PromptVectors::build(&Prefixed(&m, prefix)).unwrap();
        let mut right = 0;
        let mut gap = 0.0;
        for ((name, want, _), v) in pics.iter().zip(&vecs) {
            let zs = pv.score(v).unwrap();
            let mut ranked: Vec<(Category, f32)> =
                Category::ALL.iter().map(|c| (*c, zs.cosine(*c))).collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
            eprintln!(
                "{label:>14} {name:>8}: {} {:.3}, then {} {:.3}",
                ranked[0].0.as_str(),
                ranked[0].1,
                ranked[1].0.as_str(),
                ranked[1].1
            );
            if ranked[0].0 == *want {
                right += 1;
            }
            let other = ranked
                .iter()
                .filter(|r| r.0 != *want)
                .map(|r| r.1)
                .fold(f32::MIN, f32::max);
            gap += zs.cosine(*want) - other;
        }
        eprintln!(
            "{label:>14}: {right}/{} right, mean gap {:.4}",
            pics.len(),
            gap / pics.len() as f32
        );
    }
}

const QUERY: &str = gemma::QUERY_PREFIX;

/// Per model, over the public XM3600 pictures: how far apart the categories' best cosines are,
/// which is what the zero-shot softmax scale has to fit.
#[test]
#[ignore]
fn zero_shot_spread_on_xm3600() {
    let g = gemma();
    let s = Siglip::load(SiglipConfig::from_env()).unwrap();
    let (_, gv) = load_vecs("xm_img_gemma_b70");
    let (_, sv) = load_vecs("xm_img_siglip");
    let spread = |e: &dyn Embedder, vecs: &[Vec<f32>], label: &str| -> (f32, f32) {
        let pv = PromptVectors::build(e).unwrap();
        let (mut gap, mut sd, mut top) = (0.0f32, 0.0f32, 0.0f32);
        for v in vecs {
            let zs: ZeroShot = pv.score(v).unwrap();
            let mut c = zs.cosines.to_vec();
            c.sort_by(|a, b| b.total_cmp(a));
            gap += c[0] - c[1];
            top += c[0];
            let mean = c.iter().sum::<f32>() / c.len() as f32;
            sd += (c.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / c.len() as f32).sqrt();
        }
        let n = vecs.len() as f32;
        eprintln!(
            "{label}: mean top cosine {:.3}, mean gap top1−top2 {:.4}, mean sd across categories {:.4}",
            top / n,
            gap / n,
            sd / n
        );
        (gap / n, sd / n)
    };
    let (sg, ss) = spread(&s, &sv, "siglip2 (as stored)");
    let (gg, gs) = spread(&g, &gv, "gemma b70");
    eprintln!(
        "scale that matches the gap: {:.1}; the spread: {:.1} (siglip2 uses 100)",
        100.0 * sg / gg,
        100.0 * ss / gs
    );
}

#[test]
#[ignore]
fn agrees_with_the_evaluation_harness_on_xm3600() {
    let m = gemma();
    let dir = eval_dir();
    let (keys, golden) = load_vecs("xm_img_gemma_b70");
    let index: HashMap<&str, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let list = std::fs::read_to_string(dir.join("xm3600/list.tsv")).unwrap();
    let picks: Vec<&str> = list
        .lines()
        .take(8)
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    let mut cosines = Vec::new();
    let mut ms = Vec::new();
    for key in &picks {
        let img = image::open(dir.join(format!("xm3600/images/{key}.jpg")))
            .unwrap()
            .to_rgb8();
        let t0 = Instant::now();
        let v = m
            .embed_images(std::slice::from_ref(&img))
            .unwrap()
            .remove(0);
        ms.push(t0.elapsed().as_millis() as f64);
        cosines.push(cos(&v, &golden[index[key]]));
    }
    eprintln!("images: cosine to the harness {cosines:.5?}");
    eprintln!("images: ms each (first includes loading) {ms:?}");
    assert!(cosines.iter().all(|c| *c > 0.999), "{cosines:?}");

    let (tkeys, tgold) = load_vecs("xm_txt_gemma");
    let tindex: HashMap<&str, usize> = tkeys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let mut tcos = Vec::new();
    let mut tms = Vec::new();
    for l in std::fs::read_to_string(dir.join("xm3600/queries.tsv"))
        .unwrap()
        .lines()
        .take(300)
    {
        let (id, text) = l.split_once('\t').unwrap();
        if !picks.iter().any(|p| id.starts_with(p)) {
            continue;
        }
        let t0 = Instant::now();
        let v = m.embed_text(text).unwrap();
        tms.push(t0.elapsed().as_millis() as f64);
        tcos.push(cos(&v, &tgold[tindex[id]]));
    }
    assert!(!tcos.is_empty());
    let mean_ms = tms.iter().sum::<f64>() / tms.len() as f64;
    eprintln!(
        "texts: {} queries, min cosine to the harness {:.5}, {mean_ms:.0} ms each",
        tcos.len(),
        tcos.iter().cloned().fold(2.0, f32::min)
    );
    assert!(tcos.iter().all(|c| *c > 0.999), "{tcos:?}");
}
