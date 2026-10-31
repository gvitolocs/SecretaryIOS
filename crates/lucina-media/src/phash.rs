//! Perceptual hashes of a picture, for finding the same photo in two encodings (`dedup.rs`).
//!
//! Two 64-bit hashes of the same grey picture, both robust to re-encoding and to scaling:
//! - `phash`: the signs of the 63 lowest AC coefficients of a 32x32 DCT against their median
//!   (the classic pHash; bit 63, the DC term, is always 0 so a brightness change never flips it);
//! - `dhash`: whether each pixel of a 9x8 grid is darker than its right neighbour (a gradient hash).
//!
//! The hashes are computed from the oriented 360 px thumbnail (`thumbs` `s.jpg`), not from the
//! original: the thumbnail is on NVMe, already upright and the same size for every encoding of a photo.

use std::path::Path;

use image::imageops::FilterType;
use image::DynamicImage;

/// Side of the grey grid the DCT runs on.
const DCT_SIDE: usize = 32;
/// The low-frequency block kept from the DCT: 8x8 = 64 bits.
const KEEP: usize = 8;
/// A picture whose luma varies less than this (standard deviation, 0 to 255) is flat: a blank
/// wall, a black frame, a plain screenshot background. Flat pictures hash alike whatever they show.
pub const FLAT_STDDEV: f64 = 5.0;

/// What is stored per picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hashes {
    pub phash: u64,
    pub dhash: u64,
    /// Width and height of the picture that was hashed (the thumbnail), upright.
    pub width: u32,
    pub height: u32,
    /// Too little detail to tell pictures apart by hash.
    pub flat: bool,
}

/// Hamming distance of two 64-bit hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Hash a picture. The picture should already be upright.
pub fn hash_image(img: &DynamicImage) -> Hashes {
    let (width, height) = (img.width(), img.height());
    let gray = DynamicImage::ImageLuma8(img.to_luma8());
    let g32 = gray
        .resize_exact(DCT_SIDE as u32, DCT_SIDE as u32, FilterType::Triangle)
        .to_luma8();
    let px: Vec<f64> = g32.as_raw().iter().map(|&v| v as f64).collect();
    let mean = px.iter().sum::<f64>() / px.len() as f64;
    let var = px.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / px.len() as f64;
    Hashes {
        phash: phash_of(&px),
        dhash: dhash_of(&gray),
        width,
        height,
        flat: var.sqrt() < FLAT_STDDEV,
    }
}

/// Hash the picture in a JPEG (or any format `image` reads) file.
pub fn hash_file(path: &Path) -> anyhow::Result<Hashes> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    Ok(hash_image(&img))
}

fn dct_table() -> [[f64; DCT_SIDE]; KEEP] {
    let mut t = [[0.0; DCT_SIDE]; KEEP];
    for (u, row) in t.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            *v = (std::f64::consts::PI * (x as f64 + 0.5) * u as f64 / DCT_SIDE as f64).cos();
        }
    }
    t
}

fn phash_of(px: &[f64]) -> u64 {
    let t = dct_table();
    // Rows first: tmp[y][u] = sum_x px[y][x] * cos(u, x), for the kept u only.
    let mut tmp = [[0.0; KEEP]; DCT_SIDE];
    for y in 0..DCT_SIDE {
        for u in 0..KEEP {
            tmp[y][u] = (0..DCT_SIDE).map(|x| px[y * DCT_SIDE + x] * t[u][x]).sum();
        }
    }
    let mut coef = [0.0f64; KEEP * KEEP];
    for v in 0..KEEP {
        for u in 0..KEEP {
            coef[v * KEEP + u] = (0..DCT_SIDE).map(|y| tmp[y][u] * t[v][y]).sum();
        }
    }
    // Median of the 63 AC terms.
    let mut ac: Vec<f64> = coef[1..].to_vec();
    ac.sort_by(|a, b| a.total_cmp(b));
    let median = ac[ac.len() / 2];
    let mut bits = 0u64;
    for (i, c) in coef.iter().enumerate().skip(1) {
        if *c > median {
            bits |= 1 << i;
        }
    }
    bits
}

fn dhash_of(gray: &DynamicImage) -> u64 {
    let g = gray.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut bits = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if g.get_pixel(x, y).0[0] < g.get_pixel(x + 1, y).0[0] {
                bits |= 1 << (y * 8 + x);
            }
        }
    }
    bits
}

#[cfg(test)]
pub mod testing {
    //! Synthetic pictures for the tests of the hashes and of the duplicate detector.

    use image::{DynamicImage, Rgb, RgbImage};

    /// A deterministic pseudo-random picture of `w` x `h`: smooth colour blobs and hard-edged
    /// rectangles placed from `seed`. Different seeds give unrelated pictures; the same seed at
    /// another size gives the same picture scaled.
    pub fn scene(seed: u64, w: u32, h: u32) -> DynamicImage {
        let mut s = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut next = move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) as f64) / ((1u64 << 31) as f64)
        };
        let base = [
            next() * 200.0 + 20.0,
            next() * 200.0 + 20.0,
            next() * 200.0 + 20.0,
        ];
        let blobs: Vec<(f64, f64, f64, [f64; 3])> = (0..7)
            .map(|_| {
                (
                    next(),
                    next(),
                    0.08 + next() * 0.25,
                    [next() * 255.0, next() * 255.0, next() * 255.0],
                )
            })
            .collect();
        let rects: Vec<(f64, f64, f64, f64, [f64; 3])> = (0..5)
            .map(|_| {
                let (x, y) = (next() * 0.8, next() * 0.8);
                (
                    x,
                    y,
                    x + 0.05 + next() * 0.2,
                    y + 0.05 + next() * 0.2,
                    [next() * 255.0, next() * 255.0, next() * 255.0],
                )
            })
            .collect();
        let mut img = RgbImage::new(w, h);
        for py in 0..h {
            for px in 0..w {
                let (x, y) = (px as f64 / w as f64, py as f64 / h as f64);
                let mut c = base;
                for (bx, by, r, col) in &blobs {
                    let d2 = ((x - bx).powi(2) + (y - by).powi(2)) / (r * r);
                    let wgt = (-d2).exp();
                    for i in 0..3 {
                        c[i] = c[i] * (1.0 - wgt) + col[i] * wgt;
                    }
                }
                for (x0, y0, x1, y1, col) in &rects {
                    if x >= *x0 && x < *x1 && y >= *y0 && y < *y1 {
                        c = *col;
                    }
                }
                img.put_pixel(px, py, Rgb([c[0] as u8, c[1] as u8, c[2] as u8]));
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    /// Encode as JPEG at `quality` and decode again: what a re-encoded copy looks like.
    pub fn recompress(img: &DynamicImage, quality: u8) -> DynamicImage {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&img.to_rgb8())
            .unwrap();
        image::load_from_memory_with_format(&out, image::ImageFormat::Jpeg).unwrap()
    }

    /// The same scene shifted a little, as the next frame of a burst would be.
    pub fn shifted(seed: u64, w: u32, h: u32, dx: u32) -> DynamicImage {
        let big = scene(seed, w + dx, h);
        big.crop_imm(dx, 0, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn dist(a: &Hashes, b: &Hashes) -> u32 {
        hamming(a.phash, b.phash) + hamming(a.dhash, b.dhash)
    }

    #[test]
    fn the_same_picture_hashes_alike_whatever_the_quality_or_size() {
        let original = scene(7, 640, 480);
        let h0 = hash_image(&original);
        for q in [95u8, 70, 40] {
            let h = hash_image(&recompress(&original, q));
            assert!(dist(&h0, &h) <= 8, "quality {q}: {}", dist(&h0, &h));
        }
        let half = original.resize_exact(320, 240, image::imageops::FilterType::Lanczos3);
        assert!(dist(&h0, &hash_image(&half)) <= 8);
        let small = hash_image(&recompress(
            &original.resize_exact(160, 120, image::imageops::FilterType::Triangle),
            60,
        ));
        assert!(dist(&h0, &small) <= 12, "{}", dist(&h0, &small));
    }

    #[test]
    fn different_pictures_hash_apart() {
        let mut worst = u32::MAX;
        for seed in 1..=12u64 {
            let a = hash_image(&scene(seed, 320, 240));
            let b = hash_image(&scene(seed + 100, 320, 240));
            worst = worst.min(dist(&a, &b));
        }
        assert!(worst >= 30, "closest unrelated pair is {worst} bits apart");
    }

    #[test]
    fn flat_pictures_are_flagged() {
        let blank = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            100,
            80,
            image::Rgb([200, 200, 200]),
        ));
        assert!(hash_image(&blank).flat);
        assert!(!hash_image(&scene(3, 100, 80)).flat);
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(0, u64::MAX), 64);
        assert_eq!(hamming(0b1010, 0b0110), 2);
    }

    #[test]
    fn the_dc_bit_is_never_set() {
        for seed in 1..=5u64 {
            assert_eq!(hash_image(&scene(seed, 120, 90)).phash & 1, 0);
        }
    }
}
