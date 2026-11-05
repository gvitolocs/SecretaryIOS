//! Media URLs: which hosts the service will fetch, how the backend's worker URLs are unwrapped,
//! and the signed `/v1/instagram/s/<exp>/<sig>/<token>` URLs clients get instead.
//!
//! `<token>` is the Instagram CDN URL in unpadded base64url; `<sig>` is the first 16 bytes
//! (hex) of HMAC-SHA256(key, "<exp>/<token>"). Nothing is stored server side, so URLs survive
//! restarts. The service never fetches a URL whose host is not on the allowlist, however it
//! got there.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

pub const PREFIX: &str = "/v1/instagram";
/// How long a signed media URL works: about an hour (2026-10-31; it was six). The URLs skip the
/// session check so `<img>`/`<video>` can load them, so whoever holds one (a shared link, a log
/// line) can fetch that one item until then.
pub const URL_LIFETIME_SECS: i64 = 3600;
/// Expiries are rounded up to this, so a client's image cache sees one URL per item for a while.
const URL_WINDOW_SECS: i64 = 15 * 60;

/// The expiry for a URL signed now: an hour on, rounded up to the next window (60-75 minutes).
fn expiry() -> i64 {
    let exp = chrono::Utc::now().timestamp() + URL_LIFETIME_SECS;
    exp - exp.rem_euclid(URL_WINDOW_SECS) + URL_WINDOW_SECS
}

/// The backend's media worker (`…workers.dev/cdn?url=<cdn>&exp=&sig=`).
fn is_worker_host(host: &str) -> bool {
    host.ends_with(".workers.dev")
}

/// `*.cdninstagram.com`, `*.fbcdn.net`: the only hosts media is ever fetched from.
pub fn is_allowed_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    !h.is_empty()
        && !h.starts_with('.')
        && (h.ends_with(".cdninstagram.com") || h.ends_with(".fbcdn.net"))
}

/// `Some(url)` when `s` is an `https` URL on an allowlisted host, with no credentials or odd
/// port.
pub fn allowed_cdn_url(s: &str) -> Option<String> {
    let u = Url::parse(s).ok()?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some_and(|p| p != 443)
    {
        return None;
    }
    let host = u.host_str()?;
    if host.parse::<std::net::IpAddr>().is_ok() || !is_allowed_host(host) {
        return None;
    }
    Some(u.to_string())
}

/// The Instagram CDN URL behind a URL from the backend: unwraps the worker URL's `url=`
/// parameter, or accepts a direct CDN URL. `None` for anything else.
pub fn inner_cdn_url(s: &str) -> Option<String> {
    let u = Url::parse(s).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    if is_worker_host(&host) {
        if u.path() != "/cdn" {
            return None;
        }
        let inner = u
            .query_pairs()
            .find(|(k, _)| k == "url")
            .map(|(_, v)| v.into_owned())?;
        return allowed_cdn_url(&inner);
    }
    allowed_cdn_url(s)
}

/// Whether a CDN URL is a video (by its path extension).
pub fn is_video_url(cdn: &str) -> bool {
    Url::parse(cdn)
        .map(|u| {
            let p = u.path().to_ascii_lowercase();
            p.ends_with(".mp4") || p.ends_with(".webm") || p.ends_with(".mov")
        })
        .unwrap_or(false)
}

/// Content type guessed from a CDN URL's path extension.
pub fn ctype_from_url(cdn: &str) -> Option<&'static str> {
    let u = Url::parse(cdn).ok()?;
    let p = u.path().to_ascii_lowercase();
    let ext = p.rsplit('.').next()?;
    crate::cache::ctype_for_ext(match ext {
        "jpeg" => "jpg",
        e => e,
    })
}

/// Cache key of a CDN URL. The query carries volatile signature parameters (`oh`, `oe`), so the
/// key is the host-less path plus the parameters that choose the rendition (`stp`, `efg`):
/// the same picture fetched through a fresh backend URL hits the same cache entry.
pub fn cache_key(cdn: &str) -> String {
    let (path, stp, efg) = match Url::parse(cdn) {
        Ok(u) => {
            let q = |name: &str| {
                u.query_pairs()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
            };
            (u.path().to_string(), q("stp"), q("efg"))
        }
        Err(_) => (cdn.to_string(), String::new(), String::new()),
    };
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update([0]);
    h.update(stp.as_bytes());
    h.update([0]);
    h.update(efg.as_bytes());
    hex::encode(&h.finalize()[..16])
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignError {
    BadSignature,
    Expired,
    BadToken,
    HostNotAllowed,
}

pub struct Signer {
    key: Vec<u8>,
}

impl Signer {
    pub fn new(key: Vec<u8>) -> Signer {
        Signer { key }
    }

    fn mac(&self, msg: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).expect("hmac key");
        mac.update(msg.as_bytes());
        hex::encode(&mac.finalize().into_bytes()[..16])
    }

    /// The signed path for a CDN URL, valid for 60-75 minutes (expiry rounded up to a quarter
    /// hour, so a client's image cache sees the same URL for the same media meanwhile).
    pub fn url_for(&self, cdn: &str) -> String {
        self.url_with_exp(cdn, expiry())
    }

    pub fn url_with_exp(&self, cdn: &str, exp: i64) -> String {
        let token = URL_SAFE_NO_PAD.encode(cdn.as_bytes());
        let sig = self.mac(&format!("{exp}/{token}"));
        format!("{PREFIX}/s/{exp}/{sig}/{token}")
    }

    /// The CDN URL behind a signed path's parts, or why it is refused.
    pub fn verify(&self, exp: i64, sig: &str, token: &str) -> Result<String, SignError> {
        let cdn = self.verify_any_age(exp, sig, token);
        if cdn.is_ok() && exp < chrono::Utc::now().timestamp() {
            return Err(SignError::Expired);
        }
        cdn
    }

    /// [`Signer::verify`] without the expiry. Only for callers behind the owner's check: an expired
    /// link in a cached page is still one this service handed out.
    fn verify_any_age(&self, exp: i64, sig: &str, token: &str) -> Result<String, SignError> {
        let expected = self.mac(&format!("{exp}/{token}"));
        let ok =
            sig.len() == expected.len() && bool::from(sig.as_bytes().ct_eq(expected.as_bytes()));
        if !ok {
            return Err(SignError::BadSignature);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| SignError::BadToken)?;
        let cdn = String::from_utf8(bytes).map_err(|_| SignError::BadToken)?;
        // Defence in depth: a valid signature on a foreign host (a key leak, a bug) still
        // never reaches the network.
        allowed_cdn_url(&cdn).ok_or(SignError::HostNotAllowed)
    }

    /// A signed URL for a saved item (`/v1/instagram/a/<exp>/<sig>/<item id>`), served from the
    /// Gallery's file.
    pub fn archive_url(&self, media_id: &str) -> String {
        let exp = expiry();
        format!(
            "{PREFIX}/a/{exp}/{}/{media_id}",
            self.mac(&format!("a/{exp}/{media_id}"))
        )
    }

    pub fn verify_archive(&self, exp: i64, sig: &str, media_id: &str) -> Result<(), SignError> {
        let expected = self.mac(&format!("a/{exp}/{media_id}"));
        let ok =
            sig.len() == expected.len() && bool::from(sig.as_bytes().ct_eq(expected.as_bytes()));
        if !ok {
            return Err(SignError::BadSignature);
        }
        if exp < chrono::Utc::now().timestamp() {
            return Err(SignError::Expired);
        }
        Ok(())
    }

    /// Verify a signed URL as a client sends it back (`/v1/instagram/s/…`, optionally with a
    /// scheme and host in front).
    pub fn verify_url(&self, url: &str) -> Result<String, SignError> {
        let (exp, sig, token) = url_parts(url, "/s/")?;
        self.verify(exp, &sig, &token)
    }

    /// [`Signer::verify_url`] without the expiry, for requests already behind the owner's check
    /// (saving an item from a page opened over an hour ago).
    pub fn verify_url_any_age(&self, url: &str) -> Result<String, SignError> {
        let (exp, sig, token) = url_parts(url, "/s/")?;
        self.verify_any_age(exp, &sig, &token)
    }

    /// A new link for one this service signed, however old (`/s/` or `/a/`), so a cached page never
    /// shows an expired one. `None` for anything else: the owner cannot get a URL signed that the
    /// service did not hand out, and rotating the key voids old links for good.
    pub fn resign(&self, url: &str) -> Option<String> {
        if let Ok((exp, sig, id)) = url_parts(url, "/a/") {
            let expected = self.mac(&format!("a/{exp}/{id}"));
            let ok = sig.len() == expected.len()
                && bool::from(sig.as_bytes().ct_eq(expected.as_bytes()));
            return ok.then(|| self.archive_url(&id));
        }
        self.verify_url_any_age(url)
            .ok()
            .map(|cdn| self.url_for(&cdn))
    }
}

/// The `<exp>`, `<sig>` and last part of a signed path of this kind (`/s/` or `/a/`).
fn url_parts(url: &str, kind: &str) -> Result<(i64, String, String), SignError> {
    let path = match Url::parse(url) {
        Ok(u) => u.path().to_string(),
        Err(_) => url.split(['?', '#']).next().unwrap_or("").to_string(),
    };
    let rest = path
        .strip_prefix(PREFIX)
        .and_then(|r| r.strip_prefix(kind))
        .ok_or(SignError::BadToken)?;
    let mut parts = rest.split('/');
    let (Some(exp), Some(sig), Some(last), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(SignError::BadToken);
    };
    let exp: i64 = exp.parse().map_err(|_| SignError::BadToken)?;
    Ok((exp, sig.to_string(), last.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CDN: &str =
        "https://scontent-gmp1-1.cdninstagram.com/v/t51.82787-15/a_n.jpg?stp=dst-jpg_e35&oh=1&oe=2";

    fn worker(inner: &str) -> String {
        let enc: String = url::form_urlencoded::byte_serialize(inner.as_bytes()).collect();
        format!("https://storyview-ig-proxy.yodams.workers.dev/cdn?url={enc}&exp=1&sig=abc")
    }

    #[test]
    fn allowlist_is_a_suffix_match_on_a_label_boundary() {
        assert!(is_allowed_host("scontent.cdninstagram.com"));
        assert!(is_allowed_host("instagram.ficn2-2.fna.fbcdn.net"));
        assert!(!is_allowed_host("cdninstagram.com.evil.test"));
        assert!(!is_allowed_host("evilcdninstagram.com"));
        assert!(!is_allowed_host("fbcdn.net"));
        assert!(!is_allowed_host("evil.test"));
        assert!(allowed_cdn_url("http://scontent.cdninstagram.com/a.jpg").is_none());
        assert!(allowed_cdn_url("https://u:p@scontent.cdninstagram.com/a.jpg").is_none());
        assert!(allowed_cdn_url("https://scontent.cdninstagram.com:8443/a.jpg").is_none());
        assert!(allowed_cdn_url("https://10.0.0.1/a.jpg").is_none());
        assert!(allowed_cdn_url("https://scontent.cdninstagram.com.evil.test/a.jpg").is_none());
    }

    #[test]
    fn unwraps_the_worker_url() {
        assert_eq!(inner_cdn_url(&worker(CDN)).as_deref(), Some(CDN));
        assert_eq!(inner_cdn_url(CDN).as_deref(), Some(CDN));
        assert!(inner_cdn_url(&worker("https://evil.test/a.jpg")).is_none());
        assert!(
            inner_cdn_url("https://evil.test/cdn?url=https%3A%2F%2Fa.cdninstagram.com%2Fx")
                .is_none()
        );
        assert!(inner_cdn_url(
            "https://x.workers.dev/other?url=https%3A%2F%2Fa.cdninstagram.com%2Fx"
        )
        .is_none());
        assert!(inner_cdn_url("not a url").is_none());
    }

    #[test]
    fn the_cache_key_ignores_volatile_signature_params() {
        let a = "https://s.cdninstagram.com/v/t51/a_n.jpg?stp=dst-jpg_e35&oh=1&oe=2";
        let b = "https://s2.cdninstagram.com/v/t51/a_n.jpg?stp=dst-jpg_e35&oh=9&oe=8";
        let c = "https://s.cdninstagram.com/v/t51/a_n.jpg?stp=dst-jpg_s150x150&oh=1&oe=2";
        assert_eq!(cache_key(a), cache_key(b));
        assert_ne!(cache_key(a), cache_key(c));
    }

    #[test]
    fn signed_urls_round_trip_and_reject_tampering() {
        let s = Signer::new(vec![7; 32]);
        let u = s.url_for(CDN);
        assert!(u.starts_with("/v1/instagram/s/"));
        assert_eq!(s.verify_url(&u).as_deref(), Ok(CDN));
        assert_eq!(
            s.verify_url(&format!("https://getsecretary.tech{u}?x=1"))
                .as_deref(),
            Ok(CDN)
        );
        // Other key, tampered signature, tampered token, expired.
        assert_eq!(
            Signer::new(vec![8; 32]).verify_url(&u),
            Err(SignError::BadSignature)
        );
        let bad = u.clone();
        let mut parts: Vec<&str> = bad.split('/').collect();
        let n = parts.len();
        let mut tok = parts[n - 1].to_string();
        tok.push('A');
        parts[n - 1] = &tok;
        assert_eq!(s.verify_url(&parts.join("/")), Err(SignError::BadSignature));
        let old = s.url_with_exp(CDN, chrono::Utc::now().timestamp() - 5);
        assert_eq!(s.verify_url(&old), Err(SignError::Expired));
        // A valid signature on a foreign host is still refused.
        let evil = s.url_for("https://evil.test/x.jpg");
        assert_eq!(s.verify_url(&evil), Err(SignError::HostNotAllowed));
        assert_eq!(
            s.verify_url("/v1/instagram/s/1/2"),
            Err(SignError::BadToken)
        );
        assert_eq!(s.verify_url("/v1/other/s/1/2/3"), Err(SignError::BadToken));
    }

    #[test]
    fn urls_are_stable_within_a_window() {
        let s = Signer::new(vec![1; 32]);
        assert_eq!(s.url_for(CDN), s.url_for(CDN));
    }

    /// The part of a signed path after the prefix: `[exp, sig, token]`.
    fn parts(url: &str, kind: &str) -> Vec<String> {
        url.strip_prefix(&format!("{PREFIX}/{kind}/"))
            .unwrap()
            .split('/')
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn urls_live_about_an_hour_not_six() {
        let s = Signer::new(vec![1; 32]);
        let now = chrono::Utc::now().timestamp();
        for url in [s.url_for(CDN), s.archive_url("itm_1")] {
            let kind = if url.contains("/a/") { "a" } else { "s" };
            let exp: i64 = parts(&url, kind)[0].parse().unwrap();
            assert_eq!(exp % URL_WINDOW_SECS, 0, "{url}");
            assert!(exp - now >= URL_LIFETIME_SECS, "{url}");
            assert!(exp - now <= URL_LIFETIME_SECS + URL_WINDOW_SECS, "{url}");
        }
    }

    #[test]
    fn a_signature_is_bound_to_its_item_and_expiry() {
        let s = Signer::new(vec![1; 32]);
        let p = parts(&s.url_for(CDN), "s");
        let exp: i64 = p[0].parse().unwrap();
        assert!(s.verify(exp, &p[1], &p[2]).is_ok());
        // Another picture's token under the same signature, or a later expiry, is refused.
        let other = URL_SAFE_NO_PAD.encode("https://scontent.cdninstagram.com/v/t51/other.jpg");
        assert_eq!(s.verify(exp, &p[1], &other), Err(SignError::BadSignature));
        assert_eq!(
            s.verify(exp + 3600, &p[1], &p[2]),
            Err(SignError::BadSignature)
        );
        let a = parts(&s.archive_url("itm_1"), "a");
        let aexp: i64 = a[0].parse().unwrap();
        assert!(s.verify_archive(aexp, &a[1], "itm_1").is_ok());
        assert_eq!(
            s.verify_archive(aexp, &a[1], "itm_2"),
            Err(SignError::BadSignature)
        );
        // A media signature is not an archive one.
        assert_eq!(
            s.verify_archive(exp, &p[1], &p[2]),
            Err(SignError::BadSignature)
        );
    }

    #[test]
    fn only_links_this_service_signed_are_signed_again() {
        let s = Signer::new(vec![1; 32]);
        let now = chrono::Utc::now().timestamp();
        let old = s.url_with_exp(CDN, now - 7 * 24 * 3600);
        assert_eq!(s.verify_url(&old), Err(SignError::Expired));
        let fresh = s.resign(&old).unwrap();
        assert_eq!(s.verify_url(&fresh).as_deref(), Ok(CDN));
        // Through a full URL as well; an archive link stays an archive link.
        assert!(s
            .resign(&format!("https://getsecretary.tech{old}"))
            .is_some());
        let a = s.archive_url("itm_1");
        let again = s.resign(&a).unwrap();
        assert!(again.starts_with("/v1/instagram/a/") && again.ends_with("/itm_1"));
        // Another key's link, a forged one, a foreign host, a raw CDN URL: refused.
        assert_eq!(Signer::new(vec![2; 32]).resign(&old), None);
        assert_eq!(s.resign(&a.replace("itm_1", "itm_2")), None);
        let tampered = old.replacen(
            &(now - 7 * 24 * 3600).to_string(),
            &(now + 99).to_string(),
            1,
        );
        assert_eq!(s.resign(&tampered), None);
        assert_eq!(s.resign(&s.url_for("https://evil.test/x.jpg")), None);
        assert_eq!(s.resign(CDN), None);
    }

    #[test]
    fn video_and_content_type_guesses() {
        assert!(is_video_url(
            "https://a.fbcdn.net/o1/v/t2/f2/m78/x.mp4?efg=1"
        ));
        assert!(!is_video_url(CDN));
        assert_eq!(
            ctype_from_url("https://a.fbcdn.net/x.mp4?y=1"),
            Some("video/mp4")
        );
        assert_eq!(
            ctype_from_url("https://a.fbcdn.net/x.jpeg"),
            Some("image/jpeg")
        );
        assert_eq!(ctype_from_url("https://a.fbcdn.net/x"), None);
    }
}
