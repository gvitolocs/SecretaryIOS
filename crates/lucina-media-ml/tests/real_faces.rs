//! The real YuNet and SFace networks on synthetic pictures only: drawn cartoon faces (flat
//! ellipses) and pictures with nothing face-like. No photo of a person is used. Needs the models
//! and ONNX Runtime (`LUCINA_MEDIA_ML_ORT_LIB`, `LUCINA_MEDIA_FACES_MODEL_DIR`, see `faces.rs`), so
//! it is ignored by default:
//! `cargo test -p lucina-media-ml --test real_faces -- --ignored --nocapture --test-threads 1`.
//!
//! `LUCINA_FACES_SCRATCH=<dir>` also writes the cartoon, the aligned crop and the numbers the test
//! saw, so an independent check (OpenCV's own detector and recogniser in Python) can compare.

use std::time::Instant;

use image::{Rgb, RgbImage};
use lucina_media_ml::faces::{self, FaceModel, FacesConfig, YuNetSface, ALIGN_PX, DIM};
use lucina_media_ml::vector::dot;

#[derive(Clone, Copy)]
enum Part {
    Full,
    Top,
    Bottom,
}

fn ellipse(img: &mut RgbImage, cx: f32, cy: f32, rx: f32, ry: f32, rgb: [u8; 3], part: Part) {
    let (x0, x1) = (
        (cx - rx).floor().max(0.0) as u32,
        (cx + rx).ceil().min(img.width() as f32 - 1.0) as u32,
    );
    let (y0, y1) = (
        (cy - ry).floor().max(0.0) as u32,
        (cy + ry).ceil().min(img.height() as f32 - 1.0) as u32,
    );
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = ((x as f32 - cx) / rx, (y as f32 - cy) / ry);
            let keep = match part {
                Part::Full => true,
                Part::Top => dy <= 0.0,
                Part::Bottom => dy >= 0.0,
            };
            if keep && dx * dx + dy * dy <= 1.0 {
                img.put_pixel(x, y, Rgb(rgb));
            }
        }
    }
}

/// A flat cartoon head of "radius" `r` centred at (`cx`, `cy`) on a plain background.
fn cartoon(w: u32, h: u32, cx: f32, cy: f32, r: f32, tone: [u8; 3]) -> RgbImage {
    let mut img = RgbImage::from_pixel(w, h, Rgb([150, 170, 190]));
    let hair = [60, 40, 30];
    // Neck, hair, ears, face, fringe, eyes, brows, nose, mouth (RGB).
    for yy in 0..h {
        for xx in 0..w {
            let (dx, dy) = (xx as f32 - cx, yy as f32 - (cy + 1.1 * r));
            if dx.abs() < 0.3 * r && dy > 0.0 && dy < 0.6 * r {
                img.put_pixel(xx, yy, Rgb(tone));
            }
        }
    }
    ellipse(
        &mut img,
        cx,
        cy + 0.1 * r,
        0.78 * r,
        1.0 * r,
        hair,
        Part::Full,
    );
    for s in [-1.0f32, 1.0] {
        ellipse(
            &mut img,
            cx + s * 0.78 * r,
            cy,
            0.1 * r,
            0.2 * r,
            tone,
            Part::Full,
        );
    }
    ellipse(
        &mut img,
        cx,
        cy + 0.1 * r,
        0.72 * r,
        0.92 * r,
        tone,
        Part::Full,
    );
    ellipse(
        &mut img,
        cx,
        cy - 0.55 * r,
        0.75 * r,
        0.3 * r,
        hair,
        Part::Top,
    );
    for s in [-1.0f32, 1.0] {
        let (ex, ey) = (cx + s * 0.33 * r, cy - 0.12 * r);
        ellipse(
            &mut img,
            ex,
            ey,
            0.14 * r,
            0.07 * r,
            [245, 245, 245],
            Part::Full,
        );
        ellipse(
            &mut img,
            ex,
            ey,
            0.06 * r,
            0.06 * r,
            [40, 60, 90],
            Part::Full,
        );
        ellipse(
            &mut img,
            ex,
            ey,
            0.025 * r,
            0.025 * r,
            [0, 0, 0],
            Part::Full,
        );
        ellipse(
            &mut img,
            ex,
            ey - 0.15 * r,
            0.17 * r,
            0.04 * r,
            hair,
            Part::Full,
        );
    }
    ellipse(
        &mut img,
        cx,
        cy + 0.22 * r,
        0.06 * r,
        0.12 * r,
        [190, 150, 120],
        Part::Full,
    );
    ellipse(
        &mut img,
        cx,
        cy + 0.5 * r,
        0.22 * r,
        0.09 * r,
        [190, 70, 70],
        Part::Bottom,
    );
    img
}

/// Gradients, stripes and discs: nothing like a face.
fn clutter(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let stripe = if (x / 40 + y / 60) % 2 == 0 { 40 } else { 0 };
        *p = Rgb([
            (x * 255 / w) as u8,
            (y * 255 / h) as u8,
            (128 + stripe) as u8,
        ]);
    }
    ellipse(
        &mut img,
        w as f32 * 0.3,
        h as f32 * 0.4,
        90.0,
        90.0,
        [250, 250, 20],
        Part::Full,
    );
    ellipse(
        &mut img,
        w as f32 * 0.7,
        h as f32 * 0.6,
        60.0,
        120.0,
        [20, 20, 20],
        Part::Full,
    );
    img
}

fn model() -> YuNetSface {
    let cfg = FacesConfig::from_env();
    if let Some(p) = cfg.missing() {
        panic!("{} is missing", p.display());
    }
    YuNetSface::load(cfg).unwrap()
}

fn hot() -> bool {
    lucina_media_ml::thermal::cpu_temp_c().is_some_and(|t| t >= 90.0)
}

#[test]
#[ignore]
fn finds_a_drawn_face_and_nothing_in_clutter() {
    while hot() {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    let m = model();
    eprintln!("{}", m.describe().unwrap());

    // Nothing face-like: no faces, at both sizes.
    for (w, h) in [(640, 480), (1600, 1200)] {
        let t0 = Instant::now();
        let found = faces::find_faces(&m, &clutter(w, h)).unwrap();
        eprintln!(
            "clutter {w}x{h}: {} faces in {:?}",
            found.len(),
            t0.elapsed()
        );
        assert!(found.is_empty());
    }

    // One drawn face, 640×640 (a single pass, the same input OpenCV's detector gets).
    let img = cartoon(640, 640, 320.0, 320.0, 140.0, [235, 200, 170]);
    let t0 = Instant::now();
    let found = faces::find_faces(&m, &img).unwrap();
    let dt = t0.elapsed();
    assert_eq!(found.len(), 1, "{found:?}");
    let f = &found[0];
    eprintln!(
        "cartoon 640x640: score {:.3}, box ({:.1}, {:.1}, {:.1}, {:.1}) px, {dt:?}",
        f.det.score,
        f.det.x * 640.0,
        f.det.y * 640.0,
        f.det.w * 640.0,
        f.det.h * 640.0
    );
    let (cx, cy) = (f.det.x + f.det.w / 2.0, f.det.y + f.det.h / 2.0);
    assert!(
        (cx - 0.5).abs() < 0.08 && (cy - 0.5).abs() < 0.12,
        "{cx} {cy}"
    );
    let lm = &f.det.landmarks;
    assert!(
        lm[0][0] < lm[2][0] && lm[2][0] < lm[1][0],
        "eyes either side of the nose: {lm:?}"
    );
    assert!(
        lm[0][1] < lm[2][1] && lm[2][1] < lm[3][1],
        "eyes above nose above mouth: {lm:?}"
    );
    assert!(lm[3][0] < lm[4][0], "mouth corners left to right: {lm:?}");
    assert_eq!(f.vec.len(), DIM);
    assert!((dot(&f.vec, &f.vec) - 1.0).abs() < 1e-4);

    // The same drawing, smaller and elsewhere in a 1600×1200 picture: found by a window pass.
    let big = cartoon(1600, 1200, 1100.0, 700.0, 110.0, [235, 200, 170]);
    let t0 = Instant::now();
    let found = faces::find_faces(&m, &big).unwrap();
    let dt = t0.elapsed();
    eprintln!("cartoon in 1600x1200: {} faces in {dt:?}", found.len());
    assert_eq!(found.len(), 1);
    let g = &found[0];
    assert!((g.det.x + g.det.w / 2.0 - 1100.0 / 1600.0).abs() < 0.03);
    assert!((g.det.y + g.det.h / 2.0 - 700.0 / 1200.0).abs() < 0.04);
    let same = dot(&f.vec, &g.vec);

    // A big one (a close-up: a third of the picture's height) is found by the shrunk pass.
    let close = cartoon(1600, 1200, 800.0, 600.0, 330.0, [235, 200, 170]);
    let found = faces::find_faces(&m, &close).unwrap();
    eprintln!("close-up in 1600x1200: {} faces", found.len());
    assert_eq!(found.len(), 1);
    let close_cos = dot(&f.vec, &found[0].vec);

    // Another drawing (other skin tone): printed for the record, not asserted.
    let other =
        faces::find_faces(&m, &cartoon(640, 640, 320.0, 320.0, 140.0, [120, 80, 60])).unwrap();
    let other_cos = other.first().map(|o| dot(&f.vec, &o.vec));
    eprintln!(
        "cosine to the first drawing: same drawing smaller {same:.3}, close-up {close_cos:.3}, other tone {other_cos:?}"
    );
    assert!(same > 0.5 && close_cos > 0.5);

    // Where the picture is saved, for the OpenCV cross-check.
    if let Ok(dir) = std::env::var("LUCINA_FACES_SCRATCH") {
        let dir = std::path::PathBuf::from(dir);
        img.save(dir.join("rust_cartoon.png")).unwrap();
        let px = lm.map(|[x, y]| [x * 640.0, y * 640.0]);
        let crop = faces::align(&img, &px).unwrap();
        assert_eq!(crop.dimensions(), (ALIGN_PX, ALIGN_PX));
        crop.save(dir.join("rust_aligned.png")).unwrap();
        let line = format!(
            "{{\"landmarks\": {:?}, \"box\": [{}, {}, {}, {}], \"score\": {}, \"vec\": {:?}}}",
            px,
            f.det.x * 640.0,
            f.det.y * 640.0,
            f.det.w * 640.0,
            f.det.h * 640.0,
            f.det.score,
            f.vec
        );
        std::fs::write(dir.join("rust_numbers.json"), line).unwrap();
    }

    // Per-picture cost with the networks loaded.
    let t0 = Instant::now();
    for _ in 0..5 {
        faces::find_faces(&m, &big).unwrap();
    }
    eprintln!(
        "1600x1200 with one face: {:?} per picture",
        t0.elapsed() / 5
    );
    let t0 = Instant::now();
    for _ in 0..5 {
        faces::find_faces(&m, &clutter(1600, 1200)).unwrap();
    }
    eprintln!("1600x1200 with no face: {:?} per picture", t0.elapsed() / 5);
    let _ = m.model();
}
