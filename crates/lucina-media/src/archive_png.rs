//! PNG in the archive format: lossless JXL from the pixels, and the PNG rebuilt from that JXL.
//!
//! A PNG file cannot be rebuilt bit for bit from a JXL (the deflate stream, filters and ancillary
//! chunks are not stored). What is kept, and checked, is the picture: width, height, colour type,
//! bit depth, every sample and the ICC profile. [`digest`] is a SHA-256 over exactly those, taken
//! from the original file before conversion and compared again
//!
//! - after the JXL has been written and read back from disk (the only check that lets the original
//!   be deleted), and
//! - after the PNG rebuilt from the JXL has been decoded again (what the library serves once the
//!   original is gone).
//!
//! Only PNGs this can honour are converted: 8 or 16 bit grey, grey+alpha, RGB or RGBA, an
//! embedded profile that is sRGB or Display P3 (or none), no animation, no `tRNS` transparency, no
//! palette and no 1/2/4-bit depth. Anything else keeps its original and is listed as a failure.
//! Text, EXIF, resolution and gamma chunks are not carried over.

use std::io::Cursor;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use jpegxl_rs::decode::PixelFormat;
use sha2::{Digest, Sha256};

use crate::archive_jxl::{
    lossless_roundtrip, profile_of, ConvertError, Converted, Profile, Raster, Samples,
};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The pixel layout of a PNG as the decoder reports it, which is also the layout of the rebuilt
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngColor {
    L8,
    La8,
    Rgb8,
    Rgba8,
    L16,
    La16,
    Rgb16,
    Rgba16,
}

impl PngColor {
    pub fn as_str(self) -> &'static str {
        match self {
            PngColor::L8 => "l8",
            PngColor::La8 => "la8",
            PngColor::Rgb8 => "rgb8",
            PngColor::Rgba8 => "rgba8",
            PngColor::L16 => "l16",
            PngColor::La16 => "la16",
            PngColor::Rgb16 => "rgb16",
            PngColor::Rgba16 => "rgba16",
        }
    }

    pub fn parse(s: &str) -> Option<PngColor> {
        Some(match s {
            "l8" => PngColor::L8,
            "la8" => PngColor::La8,
            "rgb8" => PngColor::Rgb8,
            "rgba8" => PngColor::Rgba8,
            "l16" => PngColor::L16,
            "la16" => PngColor::La16,
            "rgb16" => PngColor::Rgb16,
            "rgba16" => PngColor::Rgba16,
            _ => return None,
        })
    }

    pub fn grey(self) -> bool {
        matches!(
            self,
            PngColor::L8 | PngColor::La8 | PngColor::L16 | PngColor::La16
        )
    }

    pub fn alpha(self) -> bool {
        matches!(
            self,
            PngColor::La8 | PngColor::Rgba8 | PngColor::La16 | PngColor::Rgba16
        )
    }

    pub fn wide(self) -> bool {
        matches!(
            self,
            PngColor::L16 | PngColor::La16 | PngColor::Rgb16 | PngColor::Rgba16
        )
    }

    /// Samples per pixel in the PNG itself.
    pub fn channels(self) -> usize {
        usize::from(!self.grey()) * 2 + 1 + usize::from(self.alpha())
    }
}

/// A decoded PNG, in its own layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngPixels {
    pub width: u32,
    pub height: u32,
    pub color: PngColor,
    /// Interleaved, `color.channels()` per pixel.
    pub samples: Samples,
    /// The embedded ICC profile bytes, as decoded from `iCCP`.
    pub icc: Option<Vec<u8>>,
}

/// Refuse a PNG this module cannot keep exactly, from its chunk headers alone (nothing is decoded).
pub fn inspect(bytes: &[u8]) -> Result<(), ConvertError> {
    let unsupported = ConvertError::UnsupportedPixels;
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(unsupported("not a PNG"));
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let mut pos = PNG_SIGNATURE.len();
    let mut ihdr = false;
    while pos + 8 <= bytes.len() {
        let len = be32(&bytes[pos..pos + 4]);
        let ty = &bytes[pos + 4..pos + 8];
        match ty {
            b"IHDR" => {
                if len < 13 || pos + 8 + 13 > bytes.len() {
                    return Err(unsupported("truncated header"));
                }
                let (depth, color_type) = (bytes[pos + 16], bytes[pos + 17]);
                if !matches!(depth, 8 | 16) || !matches!(color_type, 0 | 2 | 4 | 6) {
                    return Err(unsupported("palette or low bit depth"));
                }
                ihdr = true;
            }
            b"acTL" => return Err(unsupported("animated PNG")),
            b"tRNS" => return Err(unsupported("transparency chunk")),
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        pos = pos.saturating_add(12).saturating_add(len);
    }
    if ihdr {
        Ok(())
    } else {
        Err(unsupported("no header"))
    }
}

/// Decode a PNG file to its pixels and profile.
pub fn decode(bytes: &[u8]) -> Result<PngPixels, ConvertError> {
    let err = |e: image::ImageError| ConvertError::Decode(e.to_string());
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png)
        .into_decoder()
        .map_err(err)?;
    let icc = decoder.icc_profile().map_err(err)?;
    let img = DynamicImage::from_decoder(decoder).map_err(err)?;
    pixels_of(img, icc)
}

pub fn pixels_of(img: DynamicImage, icc: Option<Vec<u8>>) -> Result<PngPixels, ConvertError> {
    let (width, height) = (img.width(), img.height());
    let (color, samples) = match img {
        DynamicImage::ImageLuma8(b) => (PngColor::L8, Samples::U8(b.into_raw())),
        DynamicImage::ImageLumaA8(b) => (PngColor::La8, Samples::U8(b.into_raw())),
        DynamicImage::ImageRgb8(b) => (PngColor::Rgb8, Samples::U8(b.into_raw())),
        DynamicImage::ImageRgba8(b) => (PngColor::Rgba8, Samples::U8(b.into_raw())),
        DynamicImage::ImageLuma16(b) => (PngColor::L16, Samples::U16(b.into_raw())),
        DynamicImage::ImageLumaA16(b) => (PngColor::La16, Samples::U16(b.into_raw())),
        DynamicImage::ImageRgb16(b) => (PngColor::Rgb16, Samples::U16(b.into_raw())),
        DynamicImage::ImageRgba16(b) => (PngColor::Rgba16, Samples::U16(b.into_raw())),
        _ => return Err(ConvertError::UnsupportedPixels("float or unknown")),
    };
    Ok(PngPixels {
        width,
        height,
        color,
        samples,
        icc,
    })
}

/// SHA-256 (hex) over the picture: width, height, colour layout, every sample (16-bit ones
/// little-endian) and the ICC profile bytes. Two PNGs with the same digest show the same picture
/// in the same encoding layout with the same colour profile.
pub fn digest(p: &PngPixels) -> String {
    let mut h = Sha256::new();
    h.update(b"lucina-png-pixels-v1\0");
    h.update(p.width.to_le_bytes());
    h.update(p.height.to_le_bytes());
    h.update(p.color.as_str().as_bytes());
    h.update([0]);
    match &p.samples {
        Samples::U8(v) => h.update(v),
        Samples::U16(v) => {
            for s in v {
                h.update(s.to_le_bytes());
            }
        }
    }
    let icc = p.icc.as_deref().unwrap_or(&[]);
    h.update((icc.len() as u64).to_le_bytes());
    h.update(icc);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn expand<T: Copy>(v: &[T], grey: bool, alpha: bool) -> Vec<T> {
    if !grey {
        return v.to_vec();
    }
    let step = 1 + usize::from(alpha);
    let mut out = Vec::with_capacity(v.len() / step * (3 + usize::from(alpha)));
    for px in v.chunks_exact(step) {
        out.extend_from_slice(&[px[0], px[0], px[0]]);
        if alpha {
            out.push(px[1]);
        }
    }
    out
}

/// The inverse of [`expand`]; `None` when a grey picture's channels are not equal.
fn collapse<T: Copy + PartialEq>(v: &[T], grey: bool, alpha: bool) -> Option<Vec<T>> {
    if !grey {
        return Some(v.to_vec());
    }
    let step = 3 + usize::from(alpha);
    let mut out = Vec::with_capacity(v.len() / step * (1 + usize::from(alpha)));
    for px in v.chunks_exact(step) {
        if px[0] != px[1] || px[1] != px[2] {
            return None;
        }
        out.push(px[0]);
        if alpha {
            out.push(px[3]);
        }
    }
    Some(out)
}

/// The RGB(A) raster the JXL encoder takes (grey is stored as three equal channels).
pub fn to_raster(p: &PngPixels) -> Result<Raster, ConvertError> {
    let profile: Profile = profile_of(p.icc.as_deref())?;
    let (grey, alpha) = (p.color.grey(), p.color.alpha());
    let samples = match &p.samples {
        Samples::U8(v) => Samples::U8(expand(v, grey, alpha)),
        Samples::U16(v) => Samples::U16(expand(v, grey, alpha)),
    };
    Ok(Raster {
        width: p.width,
        height: p.height,
        alpha,
        profile,
        samples,
    })
}

/// Convert a PNG file to a verified lossless JXL, with the layout and profile needed to rebuild it.
pub struct PngConverted {
    pub converted: Converted,
    pub color: PngColor,
    pub icc: Option<Vec<u8>>,
    /// [`digest`] of the original's pixels.
    pub digest: String,
}

/// The PNG file's pixels as a lossless JXL, decoded again in memory and compared sample by sample.
pub fn png_roundtrip(bytes: &[u8]) -> Result<PngConverted, ConvertError> {
    inspect(bytes)?;
    let pixels = decode(bytes)?;
    let raster = to_raster(&pixels)?;
    let mut converted = lossless_roundtrip(&raster)?;
    let digest = digest(&pixels);
    // What the JXL decodes to, in the PNG's own layout, must be this very picture.
    let back = jxl_to_pixels(&converted.jxl, pixels.color, pixels.icc.clone())?;
    if self::digest(&back) != digest {
        return Err(ConvertError::Mismatch);
    }
    converted.reconstructs_to = digest.clone();
    Ok(PngConverted {
        converted,
        color: pixels.color,
        icc: pixels.icc,
        digest,
    })
}

/// A JXL decoded into a PNG's own layout. The ICC bytes come from the catalog (the JXL only tags
/// sRGB or Display P3).
pub fn jxl_to_pixels(
    jxl: &[u8],
    color: PngColor,
    icc: Option<Vec<u8>>,
) -> Result<PngPixels, ConvertError> {
    let format = PixelFormat {
        num_channels: if color.alpha() { 4 } else { 3 },
        ..PixelFormat::default()
    };
    let dec = jpegxl_rs::decoder_builder()
        .pixel_format(format)
        .build()
        .map_err(|e| ConvertError::Decode(e.to_string()))?;
    let err = |e: jpegxl_rs::DecodeError| ConvertError::Decode(e.to_string());
    let (grey, alpha) = (color.grey(), color.alpha());
    let ((meta, samples), want) = if color.wide() {
        let (m, v) = dec.decode_with::<u16>(jxl).map_err(err)?;
        let s = collapse(&v, grey, alpha).map(Samples::U16);
        ((m, s), v.len())
    } else {
        let (m, v) = dec.decode_with::<u8>(jxl).map_err(err)?;
        let s = collapse(&v, grey, alpha).map(Samples::U8);
        ((m, s), v.len())
    };
    let samples = samples.ok_or(ConvertError::Mismatch)?;
    let expanded = meta.width as usize * meta.height as usize * (3 + usize::from(alpha));
    if want != expanded {
        return Err(ConvertError::Decode("unexpected sample count".into()));
    }
    Ok(PngPixels {
        width: meta.width,
        height: meta.height,
        color,
        samples,
        icc,
    })
}

fn dynamic(p: &PngPixels) -> Result<DynamicImage, ConvertError> {
    let bad = || ConvertError::Encode("sample count does not match the size".into());
    let (w, h) = (p.width, p.height);
    Ok(match (&p.samples, p.color) {
        (Samples::U8(v), PngColor::L8) => {
            DynamicImage::ImageLuma8(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U8(v), PngColor::La8) => {
            DynamicImage::ImageLumaA8(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U8(v), PngColor::Rgb8) => {
            DynamicImage::ImageRgb8(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U8(v), PngColor::Rgba8) => {
            DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U16(v), PngColor::L16) => {
            DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U16(v), PngColor::La16) => {
            DynamicImage::ImageLumaA16(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U16(v), PngColor::Rgb16) => {
            DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        (Samples::U16(v), PngColor::Rgba16) => {
            DynamicImage::ImageRgba16(ImageBuffer::from_raw(w, h, v.clone()).ok_or_else(bad)?)
        }
        _ => {
            return Err(ConvertError::Encode(
                "sample width does not match the layout".into(),
            ))
        }
    })
}

/// The PNG file for some pixels: same layout, the profile embedded as `iCCP`, fixed compression and
/// filter settings, and no timestamp or text chunks. The same pixels and the same library version
/// give the same bytes; a library upgrade may change them (the server therefore serves a weak
/// ETag for a rebuilt PNG).
pub fn encode(p: &PngPixels) -> Result<Vec<u8>, ConvertError> {
    let img = dynamic(p)?;
    let mut out = Vec::new();
    let mut enc =
        PngEncoder::new_with_quality(&mut out, CompressionType::Default, FilterType::Adaptive);
    if let Some(icc) = &p.icc {
        enc.set_icc_profile(icc.clone())
            .map_err(|e| ConvertError::Encode(e.to_string()))?;
    }
    img.write_with_encoder(enc)
        .map_err(|e| ConvertError::Encode(e.to_string()))?;
    Ok(out)
}

/// A PNG file for `pixels`, decoded again and compared with the digest it must show.
pub fn encode_verified(p: &PngPixels, want_digest: &str) -> Result<Vec<u8>, ConvertError> {
    let bytes = encode(p)?;
    inspect(&bytes)?;
    if digest(&decode(&bytes)?) != want_digest {
        return Err(ConvertError::Mismatch);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(color: PngColor, w: u32, h: u32, seed: u32, icc: Option<Vec<u8>>) -> PngPixels {
        let n = w as usize * h as usize * color.channels();
        let mut s = seed.wrapping_mul(2654435761).wrapping_add(5);
        let mut next = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            s >> 16
        };
        let samples = if color.wide() {
            Samples::U16((0..n).map(|_| next() as u16).collect())
        } else {
            Samples::U8(
                (0..n)
                    .map(|i| ((i as u32 / 5 + next() % 4) % 256) as u8)
                    .collect(),
            )
        };
        PngPixels {
            width: w,
            height: h,
            color,
            samples,
            icc,
        }
    }

    #[test]
    fn every_layout_survives_png_jxl_png() {
        for color in [
            PngColor::L8,
            PngColor::La8,
            PngColor::Rgb8,
            PngColor::Rgba8,
            PngColor::L16,
            PngColor::La16,
            PngColor::Rgb16,
            PngColor::Rgba16,
        ] {
            let p = pixels(color, 21, 13, 3, None);
            let png = encode(&p).unwrap();
            let conv = png_roundtrip(&png).unwrap_or_else(|e| panic!("{color:?}: {e}"));
            assert_eq!(conv.color, color);
            assert_eq!(conv.digest, digest(&p), "{color:?}");
            let back = jxl_to_pixels(&conv.converted.jxl, color, None).unwrap();
            assert_eq!(back, p, "{color:?}");
            let again = encode_verified(&back, &conv.digest).unwrap();
            assert_eq!(decode(&again).unwrap(), p);
            // Deterministic: the same pixels always give the same bytes.
            assert_eq!(again, encode(&p).unwrap());
        }
    }

    #[test]
    fn the_digest_sees_every_part_of_the_picture() {
        let p = pixels(PngColor::Rgb8, 8, 8, 1, None);
        let d = digest(&p);
        let mut q = p.clone();
        if let Samples::U8(v) = &mut q.samples {
            v[17] ^= 1;
        }
        assert_ne!(digest(&q), d, "one sample");
        let mut q = p.clone();
        q.icc = Some(vec![1, 2, 3]);
        assert_ne!(digest(&q), d, "the profile");
        let q = PngPixels {
            width: 4,
            height: 16,
            ..p.clone()
        };
        assert_ne!(digest(&q), d, "the shape");
        assert_eq!(digest(&p.clone()), d);
    }

    #[test]
    fn unsupported_pngs_are_refused_from_their_headers() {
        let rgba = encode(&pixels(PngColor::Rgba8, 4, 4, 1, None)).unwrap();
        assert!(inspect(&rgba).is_ok());
        assert!(inspect(b"not a png").is_err());
        // Palette (colour type 3) and 4-bit grey: the IHDR bytes are edited, the CRC is not checked here.
        let mut pal = rgba.clone();
        pal[25] = 3;
        assert!(matches!(
            inspect(&pal),
            Err(ConvertError::UnsupportedPixels(_))
        ));
        let mut low = rgba.clone();
        low[24] = 4;
        assert!(inspect(&low).is_err());
        // An acTL or tRNS chunk before the image data.
        let with_chunk = |ty: &[u8; 4]| {
            let mut v = rgba[..33].to_vec();
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(ty);
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&rgba[33..]);
            v
        };
        assert!(inspect(&with_chunk(b"acTL")).is_err());
        assert!(inspect(&with_chunk(b"tRNS")).is_err());
        assert!(inspect(&with_chunk(b"tEXt")).is_ok());
    }
}
