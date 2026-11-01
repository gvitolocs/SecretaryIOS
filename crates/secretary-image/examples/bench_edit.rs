//! Times `edit::render` on synthetic pictures: `cargo run --release -p secretary-image --example bench_edit`.
use std::time::Instant;

use secretary_image::edit::{render, Adjust, Crop, EditList, FilterRef, Mark};
use secretary_image::Frame;

fn picture(w: u32, h: u32) -> Frame {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
        complete: true,
    }
}

fn time(label: &str, src: &Frame, e: &EditList, runs: usize) {
    let mut ms: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            let out = render(src, e).unwrap();
            std::hint::black_box(&out);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{label:<40} {}x{}  median {:.1} ms  min {:.1} ms",
        src.width,
        src.height,
        ms[ms.len() / 2],
        ms[0]
    );
}

fn main() {
    let colour = EditList {
        adjust: Adjust {
            exposure: 0.4,
            contrast: 20.0,
            highlights: -30.0,
            shadows: 25.0,
            saturation: 15.0,
            warmth: 12.0,
            tint: -6.0,
            vignette: 30.0,
            fade: 10.0,
        },
        ..EditList::default()
    };
    let exposure = EditList {
        adjust: Adjust {
            exposure: 0.5,
            ..Adjust::default()
        },
        ..EditList::default()
    };
    let geometry = EditList {
        straighten: 7.0,
        crop: Some(Crop {
            x: 0.1,
            y: 0.1,
            w: 0.8,
            h: 0.8,
        }),
        ..EditList::default()
    };
    let full = EditList {
        rotate: 1,
        straighten: 7.0,
        crop: Some(Crop {
            x: 0.05,
            y: 0.05,
            w: 0.9,
            h: 0.9,
        }),
        filter: Some(FilterRef {
            name: "vintage".into(),
            strength: 0.7,
        }),
        markup: vec![
            Mark::Pen {
                color: "#ff3b30".into(),
                width: 0.006,
                points: (0..200)
                    .map(|i| [0.1 + i as f32 * 0.004, 0.3 + (i as f32 * 0.1).sin() * 0.1])
                    .collect(),
            },
            Mark::Text {
                color: "#ffffff".into(),
                size: 0.06,
                x: 0.2,
                y: 0.8,
                text: "Lucina".into(),
            },
        ],
        ..colour.clone()
    };
    for (w, h, runs) in [(1600, 1067, 15), (4032, 3024, 5)] {
        let src = picture(w, h);
        time("exposure only", &src, &exposure, runs);
        time("all colour sliders", &src, &colour, runs);
        time("straighten + crop", &src, &geometry, runs);
        time("everything + markup", &src, &full, runs);
    }
}
