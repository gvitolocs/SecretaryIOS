//! Play tokens: `<expiry in seconds, base 36>.<HMAC-SHA256(id, expiry), 16 bytes, base64url>`.
//!
//! The token sits in the URL path, so every relative URL inside an HLS
//! playlist carries it and no player has to send a header.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8], id: &str, exp: u64) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC takes any key length");
    m.update(b"secretary-media-v1\n");
    m.update(id.as_bytes());
    m.update(b"\n");
    m.update(exp.to_string().as_bytes());
    m
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII")
}

pub fn sign(secret: &[u8], id: &str, exp: u64) -> String {
    let tag = mac(secret, id, exp).finalize().into_bytes();
    format!("{}.{}", base36(exp), URL_SAFE_NO_PAD.encode(&tag[..16]))
}

/// True when `token` was signed for `id` and has not expired at `now` (seconds).
pub fn verify(secret: &[u8], id: &str, token: &str, now: u64) -> bool {
    let Some((exp, sig)) = token.split_once('.') else { return false };
    let Ok(exp) = u64::from_str_radix(exp, 36) else { return false };
    let Ok(sig) = URL_SAFE_NO_PAD.decode(sig) else { return false };
    if exp < now || sig.len() != 16 {
        return false;
    }
    // verify_truncated_left compares in constant time.
    mac(secret, id, exp).verify_truncated_left(&sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_expiry_and_binding() {
        let key = b"k".repeat(32);
        let t = sign(&key, "abc", 2_000_000_000);
        assert!(verify(&key, "abc", &t, 1_999_999_999));
        assert!(!verify(&key, "abc", &t, 2_000_000_001));
        assert!(!verify(&key, "abd", &t, 1));
        assert!(!verify(b"other", "abc", &t, 1));
        let (exp, _) = t.split_once('.').unwrap();
        let forged = format!("{}.{}", base36(2_100_000_000), t.split_once('.').unwrap().1);
        assert!(!verify(&key, "abc", &forged, 1));
        assert!(!verify(&key, "abc", exp, 1));
        assert!(!verify(&key, "abc", "zz.zz", 1));
    }
}
