//! The real SigLIP2 export on synthetic pictures (plain colours, drawn shapes). Needs the model
//! and ONNX Runtime (`LUCINA_MEDIA_ML_ORT_LIB`, `LUCINA_MEDIA_ML_MODEL_DIR`, see `siglip.rs`), so
//! it is ignored by default:
//! `cargo test -p lucina-media-ml --test real_model -- --ignored --nocapture`.

use image::{Rgb, RgbImage};
use lucina_media_ml::siglip::{self, Siglip, SiglipConfig};
use lucina_media_ml::vector::dot;
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

/// Which picture each query picks, by cosine.
fn best<'a>(m: &Siglip, query: &str, pics: &'a [(&'a str, Vec<f32>)]) -> (&'a str, f32) {
    let q = m.embed_text(query).unwrap();
    pics.iter()
        .map(|(name, v)| (*name, dot(&q, v)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

#[test]
#[ignore]
fn matches_words_to_pictures_in_three_languages() {
    let cfg = SiglipConfig::from_env();
    if let Some(p) = cfg.missing() {
        panic!("{} is missing", p.display());
    }
    let m = Siglip::load(cfg).unwrap();
    let names = ["red", "green", "blue", "circle", "square"];
    let images = vec![
        solid([220, 20, 30]),
        solid([30, 180, 40]),
        solid([20, 40, 220]),
        shape(true),
        shape(false),
    ];
    let t0 = std::time::Instant::now();
    let vecs = m.embed_images(&images).unwrap();
    eprintln!("5 pictures in {:?}", t0.elapsed());
    assert_eq!(vecs.len(), 5);
    for v in &vecs {
        assert_eq!(v.len(), siglip::DIM);
        assert!((dot(v, v) - 1.0).abs() < 1e-3);
    }
    let pics: Vec<(&str, Vec<f32>)> = names.iter().copied().zip(vecs).collect();
    for (query, want) in [
        ("a red picture", "red"),
        ("a green picture", "green"),
        ("a blue picture", "blue"),
        ("una foto rossa", "red"),
        ("et blåt billede", "blue"),
        ("a black circle", "circle"),
        ("a black square", "square"),
    ] {
        let (got, cos) = best(&m, query, &pics);
        eprintln!(
            "{query:>18} → {got} (cos {cos:.3}, relevance {:.3})",
            siglip::relevance(cos)
        );
        assert_eq!(got, want, "{query}");
    }
    let t0 = std::time::Instant::now();
    m.embed_text("a dog on the beach").unwrap();
    eprintln!("one query (model loaded) in {:?}", t0.elapsed());
}
