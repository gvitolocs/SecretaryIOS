//! One shared linear-light color for every skin surface.
use crate::{invalid, Result};
pub const DEFAULT_SKIN: &str = "#d7b39c";
pub const DETAIL_GAIN: f32 = 1.35;
pub fn linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}
pub fn color(hex: &str) -> Result<[f32; 3]> {
    if hex.len() != 7 || !hex.starts_with('#') || !hex[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("skin color must be #rrggbb"));
    }
    let mut out = [0.; 3];
    for i in 0..3 {
        out[i] = linear(
            u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16).map_err(|_| invalid("skin color"))?
                as f32
                / 255.,
        );
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_color_roundtrips_and_rejects_bad_values() {
        for n in 0..=255 {
            let s = n as f32 / 255.;
            assert!((srgb(linear(s)) - s).abs() < 1e-6);
        }
        assert!(color("#abcdef").is_ok());
        for x in ["red", "#12345g", "#fff", "#é1234"] {
            assert!(color(x).is_err());
        }
    }
}
