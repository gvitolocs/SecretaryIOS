//! Album share links: the rule for what a public request may see, as pure functions (no I/O).
//!
//! An album share is a random 128-bit token (`album_shares`). Everything under `/v1/media/a/{token}`
//! is reachable from the internet without the upstream key, so every public handler in
//! `share_http.rs` asks [`decide`] before it reads an item; the decision itself is the kernel in
//! `share_kernel_shared.rs`, verified by `verification/verus/album_share.rs`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;

macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        $(#[allow(clippy::too_many_arguments)]
          pub fn $name($($arg: $ty),*) -> $rty { $($body)* })*
    };
}
include!("share_kernel_shared.rs");

/// Length of a token: 16 bytes in base64url without padding.
pub const TOKEN_LEN: usize = 22;
/// Requests a minute one share answers before 429.
pub const RATE_PER_MINUTE: u32 = 120;

/// A row of `album_shares`.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ShareRow {
    pub token: String,
    pub album_id: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub allow_original: bool,
    pub views: i64,
    pub last_view_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The page or its item list.
    Page,
    /// A rendition of an item of the album.
    Item,
    /// The original of an item of the album.
    Original,
    NotFound,
    Gone,
    Forbidden,
}

/// RFC 3339 to Unix ms; `None` when it does not parse. Times before 1970 are 0.
fn unix_ms(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis().max(0) as u64)
}

/// What a public request gets. `share`: the row the token names, if any; `now`: RFC 3339;
/// `item_in_album`: `None` for the page and its item list, else whether the item is in the
/// share's album at request time (trashed and hidden items are not); `want_original`: the request
/// is for the original. An expiry or a clock that does not parse counts as expired.
pub fn decide(
    share: Option<&ShareRow>,
    now: &str,
    item_in_album: Option<bool>,
    want_original: bool,
) -> Access {
    let now_ms = unix_ms(now).unwrap_or(u64::MAX);
    let (known, revoked, has_expiry, expires_at, allow_original) = match share {
        None => (false, false, false, 0, false),
        Some(s) => (
            true,
            s.revoked_at.is_some(),
            s.expires_at.is_some(),
            s.expires_at.as_deref().and_then(unix_ms).unwrap_or(0),
            s.allow_original,
        ),
    };
    match decide_code(
        known,
        revoked,
        has_expiry,
        expires_at,
        now_ms,
        item_in_album.is_some(),
        item_in_album == Some(true),
        want_original,
        allow_original,
    ) {
        0 => Access::Page,
        1 => Access::Item,
        2 => Access::Original,
        4 => Access::Gone,
        5 => Access::Forbidden,
        _ => Access::NotFound,
    }
}

/// A new token: 16 bytes from the OS RNG, base64url without padding (22 characters).
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

/// Whether `s` is a token [`new_token`] could have made (22 base64url characters encoding exactly
/// 16 bytes). Anything else is answered 404 without a database lookup.
pub fn token_well_formed(s: &str) -> bool {
    s.len() == TOKEN_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .is_ok_and(|b| b.len() == 16)
}

/// `"YYYY-MM-DD"` of a stored `taken_at` (the date as recorded, without time or offset).
pub fn date_only(taken_at: &str) -> Option<String> {
    let d = taken_at.get(..10)?;
    let ok = d.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    ok.then(|| d.to_string())
}

/// A fixed one-minute window of requests per token.
pub struct RateLimit {
    per_minute: u32,
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self::new(RATE_PER_MINUTE)
    }
}

impl RateLimit {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Counts a request for `token` at `now`; false once the window holds `per_minute`.
    pub fn allow(&self, token: &str, now: Instant) -> bool {
        const WINDOW: Duration = Duration::from_secs(60);
        let mut w = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        if w.len() > 4096 {
            w.retain(|_, (start, _)| now.saturating_duration_since(*start) < WINDOW);
        }
        let e = w.entry(token.to_string()).or_insert((now, 0));
        if now.saturating_duration_since(e.0) >= WINDOW {
            *e = (now, 0);
        }
        if e.1 >= self.per_minute {
            return false;
        }
        e.1 += 1;
        true
    }
}

/// A JPEG without its metadata: APP1 (EXIF, XMP), APP13 (IPTC), comments, every APPn but JFIF
/// (APP0), an ICC profile (APP2 `ICC_PROFILE`) and Adobe (APP14), and anything after the end
/// (MPF's appended pictures). `None` when the bytes are not a JPEG this parser can walk; callers
/// then serve nothing.
pub fn strip_jpeg_metadata(src: &[u8]) -> Option<Vec<u8>> {
    if src.len() < 4 || src[0] != 0xFF || src[1] != 0xD8 {
        return None;
    }
    let mut out = Vec::with_capacity(src.len());
    out.extend_from_slice(&src[..2]);
    let mut i = 2;
    loop {
        // A marker, after any fill bytes.
        if src.get(i) != Some(&0xFF) {
            return None;
        }
        while src.get(i) == Some(&0xFF) {
            i += 1;
        }
        let m = *src.get(i)?;
        i += 1;
        match m {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Some(out);
            }
            0x01 | 0xD0..=0xD7 => {
                out.extend_from_slice(&[0xFF, m]);
                continue;
            }
            0x00 | 0xD8 => return None,
            _ => {}
        }
        let len = u16::from_be_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        let seg = src.get(i..i + len)?;
        let payload = &seg[2..];
        let keep = match m {
            0xE0 => payload.starts_with(b"JFIF\0") || payload.starts_with(b"JFXX\0"),
            0xE2 => payload.starts_with(b"ICC_PROFILE\0"),
            0xEE => payload.starts_with(b"Adobe"),
            0xE1 | 0xE3..=0xED | 0xEF | 0xFE => false,
            _ => true,
        };
        if keep {
            out.extend_from_slice(&[0xFF, m]);
            out.extend_from_slice(seg);
        }
        i += len;
        if m == 0xDA {
            // Entropy-coded data up to the next marker that is not a stuffed 0xFF00 or a restart.
            let start = i;
            loop {
                let b = *src.get(i)?;
                if b == 0xFF {
                    match *src.get(i + 1)? {
                        0x00 | 0xD0..=0xD7 | 0xFF => i += if src[i + 1] == 0xFF { 1 } else { 2 },
                        _ => break,
                    }
                } else {
                    i += 1;
                }
            }
            out.extend_from_slice(&src[start..i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-10-27T12:00:00.000Z";

    fn row() -> ShareRow {
        ShareRow {
            token: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            album_id: "alb_1".into(),
            created_at: "2026-10-01T00:00:00.000Z".into(),
            expires_at: None,
            revoked_at: None,
            allow_original: false,
            views: 0,
            last_view_at: None,
        }
    }

    #[test]
    fn unknown_token_is_not_found_whatever_is_asked() {
        for item in [None, Some(true), Some(false)] {
            for orig in [false, true] {
                assert_eq!(decide(None, NOW, item, orig), Access::NotFound);
            }
        }
    }

    #[test]
    fn revoked_is_gone_whatever_is_asked() {
        let s = ShareRow {
            revoked_at: Some("2026-10-02T00:00:00.000Z".into()),
            allow_original: true,
            ..row()
        };
        for item in [None, Some(true), Some(false)] {
            for orig in [false, true] {
                assert_eq!(decide(Some(&s), NOW, item, orig), Access::Gone);
            }
        }
    }

    #[test]
    fn expiry_is_inclusive_and_fails_closed() {
        let at = |e: &str| ShareRow {
            expires_at: Some(e.into()),
            ..row()
        };
        assert_eq!(decide(Some(&at(NOW)), NOW, None, false), Access::Gone);
        assert_eq!(
            decide(
                Some(&at("2026-10-27T11:59:59.999Z")),
                NOW,
                Some(true),
                false
            ),
            Access::Gone
        );
        assert_eq!(
            decide(Some(&at("2026-10-27T12:00:00.001Z")), NOW, None, false),
            Access::Page
        );
        // Another offset, same instant: still compared as instants.
        assert_eq!(
            decide(Some(&at("2026-10-27T13:00:00+01:00")), NOW, None, false),
            Access::Gone
        );
        assert_eq!(decide(Some(&at("garbage")), NOW, None, false), Access::Gone);
        assert_eq!(decide(Some(&row()), "garbage", None, false), Access::Page);
        assert_eq!(
            decide(Some(&at("2030-01-01T00:00:00Z")), "garbage", None, false),
            Access::Gone
        );
    }

    #[test]
    fn page_and_items() {
        let s = row();
        assert_eq!(decide(Some(&s), NOW, None, false), Access::Page);
        assert_eq!(decide(Some(&s), NOW, None, true), Access::NotFound);
        assert_eq!(decide(Some(&s), NOW, Some(true), false), Access::Item);
        assert_eq!(decide(Some(&s), NOW, Some(false), false), Access::NotFound);
    }

    #[test]
    fn originals_need_permission_and_membership() {
        let no = row();
        let yes = ShareRow {
            allow_original: true,
            ..row()
        };
        assert_eq!(decide(Some(&no), NOW, Some(true), true), Access::Forbidden);
        assert_eq!(decide(Some(&yes), NOW, Some(true), true), Access::Original);
        assert_eq!(decide(Some(&yes), NOW, Some(false), true), Access::NotFound);
        assert_eq!(decide(Some(&no), NOW, Some(false), true), Access::NotFound);
    }

    #[test]
    fn kernel_never_grants_without_every_condition() {
        // Exhaustive over the booleans, with times on both sides of now.
        for bits in 0u32..256 {
            let b = |k: u32| bits & (1 << k) != 0;
            for (exp, now) in [(5u64, 4u64), (5, 5), (5, 6)] {
                let r = decide_code(b(0), b(1), b(2), exp, now, b(3), b(4), b(5), b(6));
                let live = b(0) && !b(1) && !(b(2) && exp <= now);
                if r <= 2 {
                    assert!(live);
                }
                if r == 1 || r == 2 {
                    assert!(b(3) && b(4));
                }
                if r == 2 {
                    assert!(b(5) && b(6));
                }
            }
        }
    }

    #[test]
    fn tokens_are_22_url_safe_characters() {
        let t = new_token().unwrap();
        assert_eq!(t.len(), 22);
        assert!(token_well_formed(&t));
        assert_ne!(t, new_token().unwrap());
        assert!(!token_well_formed(""));
        assert!(!token_well_formed(&t[..21]));
        assert!(!token_well_formed(&format!("{t}A")));
        assert!(!token_well_formed("AAAAAAAAAAAAAAAAAAAA+/"));
        assert!(!token_well_formed("AAAAAAAAAAAAAAAAAAAA.."));
        // 22 characters carry 132 bits; the last 4 must be zero.
        assert!(!token_well_formed("AAAAAAAAAAAAAAAAAAAAAB"));
        assert!(token_well_formed("AAAAAAAAAAAAAAAAAAAAAQ"));
    }

    #[test]
    fn dates_keep_only_the_day() {
        assert_eq!(
            date_only("2025-07-12T14:03:22+02:00").as_deref(),
            Some("2025-07-12")
        );
        assert_eq!(date_only("2025-07-12").as_deref(), Some("2025-07-12"));
        assert_eq!(date_only("July 2025"), None);
        assert_eq!(date_only("2025"), None);
    }

    #[test]
    fn rate_limit_counts_per_token_per_minute() {
        let rl = RateLimit::new(3);
        let t0 = Instant::now();
        assert!((0..3).all(|_| rl.allow("a", t0)));
        assert!(!rl.allow("a", t0 + Duration::from_secs(59)));
        assert!(rl.allow("b", t0));
        assert!(rl.allow("a", t0 + Duration::from_secs(60)));
    }

    /// SOI, APP0 JFIF, APP1 Exif (with GPS), COM, DQT, SOS + data with a stuffed byte and a
    /// restart, EOI, then an appended picture.
    fn jpeg_with_exif() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        let seg = |m: u8, p: &[u8]| {
            let mut s = vec![0xFF, m];
            s.extend_from_slice(&((p.len() + 2) as u16).to_be_bytes());
            s.extend_from_slice(p);
            s
        };
        v.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        v.extend(seg(0xE1, b"Exif\0\0MM\0*GPSLatitude 55.67"));
        v.extend(seg(0xFE, b"taken at home"));
        v.extend(seg(0xDB, &[0u8; 65]));
        v.extend(seg(0xDA, &[1, 1, 0, 0, 63, 0]));
        v.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v.extend_from_slice(b"\xFF\xD8\xFF\xE1\x00\x10Exif\0\0GPS....");
        v
    }

    #[test]
    fn jpeg_metadata_is_stripped() {
        let src = jpeg_with_exif();
        let out = strip_jpeg_metadata(&src).unwrap();
        let has = |hay: &[u8], n: &[u8]| hay.windows(n.len()).any(|w| w == n);
        assert!(has(&src, b"Exif") && has(&src, b"GPS"));
        assert!(!has(&out, b"Exif"));
        assert!(!has(&out, b"GPS"));
        assert!(!has(&out, b"taken at home"));
        assert!(has(&out, b"JFIF"));
        assert!(has(
            &out,
            &[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56, 0xFF, 0xD9]
        ));
        assert!(out.ends_with(&[0xFF, 0xD9]));
        assert_eq!(strip_jpeg_metadata(&out).unwrap(), out);
    }

    #[test]
    fn jpeg_stripper_refuses_what_it_cannot_walk() {
        assert_eq!(strip_jpeg_metadata(b"\x89PNG\r\n"), None);
        assert_eq!(strip_jpeg_metadata(&[0xFF, 0xD8, 0x00, 0x00]), None);
        let src = jpeg_with_exif();
        // Cut inside the Exif segment, and inside the scan.
        assert_eq!(strip_jpeg_metadata(&src[..30]), None);
        let eoi = src.windows(2).position(|w| w == [0xFF, 0xD9]).unwrap();
        assert_eq!(strip_jpeg_metadata(&src[..eoi - 1]), None);
    }

    #[test]
    fn a_real_encoded_jpeg_survives_stripping() {
        let img = image::RgbImage::from_pixel(40, 30, image::Rgb([200, 30, 90]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .unwrap();
        let out = strip_jpeg_metadata(buf.get_ref()).unwrap();
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!((back.width(), back.height()), (40, 30));
    }
}
