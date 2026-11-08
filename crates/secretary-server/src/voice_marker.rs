//! The proof that a voice share comes from secretary-server for a test device (docs/lucinavoice-protocol.md section 10.8).
//!
//! Shares reach lucina-rs from any device through Caddy, so the form field `test=1` alone proves nothing. secretary-server also
//! sends `X-Voice-Test: <shareId>.<hex HMAC-SHA256(key, "voice-test:" + shareId)>`, the key being the Lucina API token both
//! sides already hold. The token itself is never sent (Caddy logs custom headers). The marker names one share, so it cannot be
//! replayed for another. Shared by path with lucina-server: one source.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The request header.
pub const HEADER: &str = "x-voice-test";

fn mac(key: &[u8], share_id: &str) -> Option<Hmac<Sha256>> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).ok()?;
    m.update(b"voice-test:");
    m.update(share_id.as_bytes());
    Some(m)
}

/// The header value for `share_id`.
pub fn sign(key: &[u8], share_id: &str) -> Option<String> {
    let tag = mac(key, share_id)?.finalize().into_bytes();
    let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{share_id}.{hex}"))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Whether `header` is the marker of `share_id` under `key`. The tag is compared in constant time.
pub fn verify(key: &[u8], share_id: &str, header: &str) -> bool {
    let Some((id, tag)) = header.rsplit_once('.') else {
        return false;
    };
    if id != share_id || key.is_empty() {
        return false;
    }
    let (Some(tag), Some(m)) = (unhex(tag), mac(key, share_id)) else {
        return false;
    };
    m.verify_slice(&tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_one_share_and_one_key() {
        let h = sign(b"token", "abc-123").unwrap();
        assert!(h.starts_with("abc-123.") && h.len() == 8 + 64);
        assert!(verify(b"token", "abc-123", &h));
        assert!(!verify(b"token", "abc-124", &h));
        assert!(!verify(b"other", "abc-123", &h));
        assert!(!verify(b"", "abc-123", &h));
        let other = sign(b"token", "abc-124").unwrap();
        assert!(!verify(b"token", "abc-123", &other));
        // The id in the header must be the share's, whatever the tag.
        let forged = format!("abc-124.{}", &h[8..]);
        assert!(!verify(b"token", "abc-124", &forged));
        for bad in ["", ".", "abc-123.", "abc-123.zz", "abc-123.0", &h[..h.len() - 2]] {
            assert!(!verify(b"token", "abc-123", bad), "{bad}");
        }
    }
}
