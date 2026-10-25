//! The redactor: one filter in front of every output sink.
//!
//! Two passes:
//!
//! 1. **Known values.** Every secret Lucina resolves (and the bootstrap
//!    credentials: Infisical identity, DeepSeek key, API tokens) is
//!    registered with its common encodings: raw, JSON-escaped (once and
//!    twice), URL-encoded, and base64 at all three byte alignments in both
//!    alphabets (so `base64("user:" + value)` is caught too). Matches become
//!    `[secret:<NAME>]`.
//!
//!    The redactor keeps **no values**: per encoding it stores the length, a
//!    Rabin-Karp rolling hash (random base) and a salted SHA-256 digest
//!    (random salt, per process). A scan slides a window per distinct length
//!    and confirms rolling-hash hits with the digest.
//! 2. **Detector.** Token shapes without a known value: `sk-`, `ghp_`,
//!    `github_pat_`, `mcp_`, `sec_`, `svc_`, Slack, AWS, Google, Telegram bot
//!    tokens, JWTs, PEM blocks, `Bearer …`, `password=…` style pairs and
//!    URL parameters, and long high-entropy strings (mixed case and digits,
//!    so ULIDs, UUIDs and hex digests pass).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::{OnceLock, RwLock};

use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Encodings shorter than this are not registered (too many false hits).
pub const MIN_PATTERN_LEN: usize = 6;

/// Placeholder for a known secret.
pub fn placeholder(name: &str) -> String {
    format!("[secret:{name}]")
}

struct Known {
    name: String,
    digest: [u8; 32],
}

struct Inner {
    salt: [u8; 32],
    base: u64,
    /// length -> rolling hash -> entries
    by_len: BTreeMap<usize, HashMap<u64, Vec<Known>>>,
    names: Vec<String>,
    /// (name, reason) of values that were not registered whole.
    rejected: Vec<(String, String)>,
}

/// The labels the detector pass writes as `[secret:<label>]`.
pub const DETECTOR_LABELS: &[&str] = &["pem", "jwt", "token", "credential"];

/// Why a value (or name) would break the redactor's well-formedness.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejected {
    #[error("the value of {0} contains '[' or ']', which placeholders are made of")]
    Bracket(String),
    #[error("the value of {0} occurs inside the placeholder {1}")]
    InsidePlaceholder(String, String),
    #[error("the placeholder of {0} contains a registered secret")]
    NameContainsSecret(String),
}

/// `needle` occurs in `hay`.
fn occurs(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= hay.len()
        && hay.windows(needle.len()).any(|w| w == needle)
}

/// One pattern of the Verus proof's `wf` (verification/verus/
/// secrets_redactor.rs, exec twin `admissible` proved there): non-empty, no
/// `[` or `]`, and inside none of the placeholders. When every registered
/// pattern is admissible against every placeholder the redactor can write,
/// no secret survives a pass and a second pass changes nothing.
pub fn admissible(pattern: &[u8], placeholders: &[Vec<u8>]) -> bool {
    !pattern.is_empty()
        && !pattern.contains(&b'[')
        && !pattern.contains(&b']')
        && placeholders.iter().all(|p| !occurs(p, pattern))
}

/// The process-wide redactor (see [`redactor`]); `Redactor::new()` for an
/// isolated one (tests).
pub struct Redactor {
    inner: RwLock<Inner>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("known", &self.known_names().len())
            .finish()
    }
}

/// The process-wide redactor every sink uses.
pub fn redactor() -> &'static Redactor {
    static R: OnceLock<Redactor> = OnceLock::new();
    R.get_or_init(Redactor::new)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random");
    b
}

fn roll_hash(base: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |h, &b| h.wrapping_mul(base).wrapping_add(b as u64))
}

fn digest(salt: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(bytes);
    h.finalize().into()
}

/// The encodings of `value` worth matching (deduplicated, raw first).
pub fn encodings(value: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![value.to_owned()];
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    // JSON string escaping, once and twice (a JSON document inside a string).
    let once = json_escape(value);
    push(json_escape(&once));
    push(once);
    // URL encoding (upper- and lowercase hex, form style).
    push(percent_encode(value, true, false));
    push(percent_encode(value, false, false));
    push(percent_encode(value, true, true));
    // base64 at the three alignments, both alphabets.
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for k in 0..3usize {
            let mut bytes = vec![0u8; k];
            bytes.extend_from_slice(value.as_bytes());
            let enc = engine.encode(&bytes);
            let n = value.len();
            let start = (8 * k).div_ceil(6);
            let end = (8 * (k + n)) / 6;
            if end > start && end <= enc.len() {
                push(enc[start..end].to_owned());
            }
        }
    }
    out
}

fn json_escape(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap_or_default();
    q[1..q.len().saturating_sub(1).max(1)].to_owned()
}

fn percent_encode(s: &str, upper: bool, form: bool) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else if form && b == b' ' {
            out.push('+');
        } else if upper {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

struct Detector {
    re: Regex,
    label: &'static str,
}

fn detectors() -> &'static [Detector] {
    static D: OnceLock<Vec<Detector>> = OnceLock::new();
    D.get_or_init(|| {
        let d = |re: &str, label: &'static str| Detector {
            re: Regex::new(re).expect("detector regex"),
            label,
        };
        vec![
            // PEM blocks (whole, then an unterminated private key, e.g. clipped).
            d(r"(?s)-----BEGIN [A-Z0-9 ]{1,40}-----.*?-----END [A-Z0-9 ]{1,40}-----", "pem"),
            d(r"-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY-----(?:[A-Za-z0-9+/=\s]|\\[nr])*", "pem"),
            d(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", "jwt"),
            d(r"\bsk-[A-Za-z0-9_-]{16,}", "token"),
            d(r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}", "token"),
            d(r"\bgh[pousr]_[A-Za-z0-9]{20,}", "token"),
            d(r"\bgithub_pat_[A-Za-z0-9_]{20,}", "token"),
            d(r"\bglpat-[A-Za-z0-9_-]{20,}", "token"),
            d(r"\b(?:mcp|sec|svc)_[A-Za-z0-9_-]{16,}", "token"),
            d(r"\bxox[abposr]-[A-Za-z0-9-]{10,}", "token"),
            d(r"\bAKIA[0-9A-Z]{16}\b", "token"),
            d(r"\bAIza[0-9A-Za-z_-]{35}", "token"),
            d(r"\b\d{8,10}:AA[A-Za-z0-9_-]{30,}", "token"),
            d(r"\bst\.[A-Za-z0-9-]{8,}\.[a-f0-9]{16,}\.[a-f0-9]{16,}", "token"),
            // `Bearer <x>`, `Basic <x>`: keep the scheme word.
            d(r"(?i)\b(?:bearer|basic)\s+(?P<s>[A-Za-z0-9._~+/-]{16,}=*)", "token"),
            // `password=…`, `"token": "…"`, `api_key: …`: keep the name.
            d(
                r#"(?i)\b(?:password|passwd|pwd|passphrase|secret|client[_-]?secret|api[_-]?key|apikey|access[_-]?token|auth[_-]?token|token)["']?\s*[:=]\s*["']?(?P<s>[^\s"'&,;\[\]{}/][^\s"'&,;\[\]{}]{7,})"#,
                "credential",
            ),
            // URL parameters.
            d(
                r"(?i)[?&](?:api[_-]?key|apikey|key|token|access_token|auth|sig|signature|secret|password)=(?P<s>[^&\s#\[][^&\s#]{7,})",
                "credential",
            ),
        ]
    })
}

fn entropy_candidates() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_-]{32,}={0,2}").expect("entropy regex"))
}

/// Shannon entropy in bits per character.
pub fn shannon(s: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut n = 0usize;
    for c in s.chars() {
        *counts.entry(c).or_default() += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n as f64;
            -p * p.log2()
        })
        .sum()
}

/// A random-looking token: 32+ characters with lower and upper case and
/// digits (ULIDs are upper case only, UUIDs and hex digests lower case
/// only), at least 3 digits and high entropy.
pub fn looks_random(s: &str) -> bool {
    let core = s.trim_end_matches('=');
    if core.len() < 32 {
        return false;
    }
    let lower = core.bytes().any(|b| b.is_ascii_lowercase());
    let upper = core.bytes().any(|b| b.is_ascii_uppercase());
    let digits = core.bytes().filter(|b| b.is_ascii_digit()).count();
    lower && upper && digits >= 3 && shannon(core) >= 4.2
}

/// Prefix that marks a Google Drive file id (`gdrive:<id>`, written by
/// Lucina's `drive` tool). Drive ids look random, so without it the entropy
/// detector would mask every id she needs to pass back.
pub const DRIVE_REF_PREFIX: &str = "gdrive:";

/// `s` (a candidate at `start` in `text`) is a labelled Drive id: right
/// after `gdrive:` and shaped like one (25-60 of `[A-Za-z0-9_-]`).
fn is_drive_ref(text: &str, start: usize, s: &str) -> bool {
    text[..start].ends_with(DRIVE_REF_PREFIX)
        && (25..=60).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn mask_entropy(text: &str) -> Cow<'_, str> {
    let re = entropy_candidates();
    if !re.is_match(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut changed = false;
    for m in re.find_iter(text) {
        let s = m.as_str();
        if is_drive_ref(text, m.start(), s) {
            continue;
        }
        // Paths (`a/b/c`) are checked per segment unless it reads as base64.
        let base64ish = s.contains('+') || s.ends_with('=');
        if !base64ish && s.matches('/').count() >= 2 {
            let mut offset = m.start();
            for seg in s.split('/') {
                if looks_random(seg) {
                    out.push_str(&text[last..offset]);
                    out.push_str(&placeholder("token"));
                    last = offset + seg.len();
                    changed = true;
                }
                offset += seg.len() + 1;
            }
            continue;
        }
        if looks_random(s) {
            out.push_str(&text[last..m.start()]);
            out.push_str(&placeholder("token"));
            last = m.end();
            changed = true;
        }
    }
    if !changed {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[last..]);
    Cow::Owned(out)
}

fn mask_detectors(text: &str) -> Cow<'_, str> {
    let mut cur: Cow<'_, str> = Cow::Borrowed(text);
    for d in detectors() {
        if !d.re.is_match(&cur) {
            continue;
        }
        let hay: &str = &cur;
        let replaced =
            d.re.replace_all(hay, |caps: &regex::Captures<'_>| {
                let whole = caps.get(0).expect("match");
                // Our own placeholders and secret refs stay as they are.
                let after_bracket = whole.start() > 0 && hay.as_bytes()[whole.start() - 1] == b'[';
                let is_ref = caps
                    .name("s")
                    .is_some_and(|s| s.as_str().starts_with("secret://"));
                if after_bracket || is_ref {
                    return whole.as_str().to_owned();
                }
                match caps.name("s") {
                    Some(s) => {
                        let w = whole.as_str();
                        let (a, b) = (s.start() - whole.start(), s.end() - whole.start());
                        format!("{}{}{}", &w[..a], placeholder(d.label), &w[b..])
                    }
                    None => placeholder(d.label),
                }
            })
            .into_owned();
        cur = Cow::Owned(replaced);
    }
    match mask_entropy(&cur) {
        Cow::Borrowed(_) => cur,
        Cow::Owned(s) => Cow::Owned(s),
    }
}

impl Redactor {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                salt: random_bytes::<32>(),
                base: u64::from_le_bytes(random_bytes::<8>()) | 1,
                by_len: BTreeMap::new(),
                names: Vec::new(),
                rejected: Vec::new(),
            }),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Learn a secret value under `name` (all its encodings), only if the
    /// redactor stays well formed (the Verus proof's `wf`, see
    /// [`admissible`]): otherwise nothing is registered and the reason is
    /// returned. Returns how many patterns were added. The value itself is
    /// not kept.
    ///
    /// Without `wf` the redactor broke its two guarantees (docs/
    /// verification.md, "Secrets redactor overlap"): a value inside a
    /// placeholder (`secret`, or part of a name) survived and a second pass
    /// rewrote the placeholder; a value starting with the end of a
    /// placeholder (`X]bbbbbb` after a secret `X`) was re-created by it.
    pub fn try_register(&self, name: &str, value: &str) -> Result<usize, Rejected> {
        let value = value.trim_matches(|c: char| c == '\n' || c == '\r');
        if value.len() < MIN_PATTERN_LEN {
            return Ok(0);
        }
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let encs = Self::admit(&inner, name, value)?;
        Ok(Self::insert_patterns(&mut inner, name, encs))
    }

    /// [`Self::try_register`], best effort: a value that is not admissible
    /// keeps only its admissible encodings (URL-encoded and base64 never
    /// contain brackets), and the name is listed in [`Self::rejected`].
    /// For bootstrap credentials; resolved secrets use `try_register` and
    /// are refused instead (fail closed).
    pub fn register(&self, name: &str, value: &str) -> usize {
        let value = value.trim_matches(|c: char| c == '\n' || c == '\r');
        if value.len() < MIN_PATTERN_LEN {
            return 0;
        }
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        match Self::admit(&inner, name, value) {
            Ok(encs) => Self::insert_patterns(&mut inner, name, encs),
            Err(why) => {
                let reason = why.to_string();
                if !inner
                    .rejected
                    .iter()
                    .any(|(n, r)| n == name && *r == reason)
                {
                    inner.rejected.push((name.to_owned(), reason));
                }
                if matches!(why, Rejected::NameContainsSecret(_)) {
                    return 0;
                }
                let phs = Self::placeholders(&inner, name);
                let safe: Vec<String> = encodings(value)
                    .into_iter()
                    .filter(|e| e.len() >= MIN_PATTERN_LEN && admissible(e.as_bytes(), &phs))
                    .collect();
                Self::insert_patterns(&mut inner, name, safe)
            }
        }
    }

    /// Names whose value (or name) could not be registered whole, with the
    /// reason. No values.
    pub fn rejected(&self) -> Vec<(String, String)> {
        self.read().rejected.clone()
    }

    /// Every placeholder the redactor can write once `name` is known.
    fn placeholders(inner: &Inner, name: &str) -> Vec<Vec<u8>> {
        inner
            .names
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(name))
            .chain(DETECTOR_LABELS.iter().copied())
            .map(|n| placeholder(n).into_bytes())
            .collect()
    }

    /// The encodings to register, if all of them keep `wf`.
    fn admit(inner: &Inner, name: &str, value: &str) -> Result<Vec<String>, Rejected> {
        let ph = placeholder(name);
        if !inner.names.iter().any(|n| n == name) && !Self::scan(inner, &ph).is_empty() {
            return Err(Rejected::NameContainsSecret(name.to_owned()));
        }
        let phs = Self::placeholders(inner, name);
        let encs: Vec<String> = encodings(value)
            .into_iter()
            .filter(|e| e.len() >= MIN_PATTERN_LEN)
            .collect();
        for e in &encs {
            let b = e.as_bytes();
            if b.contains(&b'[') || b.contains(&b']') {
                return Err(Rejected::Bracket(name.to_owned()));
            }
            if let Some(p) = phs.iter().find(|p| occurs(p, b)) {
                return Err(Rejected::InsidePlaceholder(
                    name.to_owned(),
                    String::from_utf8_lossy(p).into_owned(),
                ));
            }
        }
        Ok(encs)
    }

    fn insert_patterns(inner: &mut Inner, name: &str, encs: Vec<String>) -> usize {
        let (salt, base) = (inner.salt, inner.base);
        let mut added = 0;
        for enc in encs {
            let bytes = enc.as_bytes();
            if bytes.len() < MIN_PATTERN_LEN {
                continue;
            }
            let h = roll_hash(base, bytes);
            let d = digest(&salt, bytes);
            let slot = inner
                .by_len
                .entry(bytes.len())
                .or_default()
                .entry(h)
                .or_default();
            if slot.iter().any(|k| k.digest == d) {
                continue;
            }
            slot.push(Known {
                name: name.to_owned(),
                digest: d,
            });
            added += 1;
        }
        if !inner.names.iter().any(|n| n == name) {
            inner.names.push(name.to_owned());
        }
        added
    }

    /// Names with registered values (no values).
    pub fn known_names(&self) -> Vec<String> {
        self.read().names.clone()
    }

    /// `(start, end, name)` of known values in `text`, non-overlapping,
    /// longest first at each position.
    fn find_known(&self, text: &str) -> Vec<(usize, usize, String)> {
        Self::scan(&self.read(), text)
    }

    fn scan(inner: &Inner, text: &str) -> Vec<(usize, usize, String)> {
        if inner.by_len.is_empty() {
            return Vec::new();
        }
        let bytes = text.as_bytes();
        let mut hits: Vec<(usize, usize, String)> = Vec::new();
        for (&len, table) in inner.by_len.iter().rev() {
            if len > bytes.len() {
                continue;
            }
            let pow = inner.base.wrapping_pow((len - 1) as u32);
            let mut h = roll_hash(inner.base, &bytes[..len]);
            let mut i = 0usize;
            loop {
                if let Some(entries) = table.get(&h) {
                    let d = digest(&inner.salt, &bytes[i..i + len]);
                    if let Some(k) = entries.iter().find(|k| k.digest == d) {
                        if text.is_char_boundary(i) && text.is_char_boundary(i + len) {
                            hits.push((i, i + len, k.name.clone()));
                        }
                    }
                }
                if i + len >= bytes.len() {
                    break;
                }
                h = h
                    .wrapping_sub((bytes[i] as u64).wrapping_mul(pow))
                    .wrapping_mul(inner.base)
                    .wrapping_add(bytes[i + len] as u64);
                i += 1;
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0).then((b.1 - b.0).cmp(&(a.1 - a.0))));
        let mut out: Vec<(usize, usize, String)> = Vec::new();
        for h in hits {
            if out.last().is_none_or(|l| h.0 >= l.1) {
                out.push(h);
            }
        }
        out
    }

    /// Only known values (no detector): for text that must otherwise stay
    /// byte-identical.
    pub fn redact_known<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let hits = self.find_known(text);
        if hits.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (s, e, name) in hits {
            out.push_str(&text[last..s]);
            out.push_str(&placeholder(&name));
            last = e;
        }
        out.push_str(&text[last..]);
        Cow::Owned(out)
    }

    /// Known values, then the detector.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match self.redact_known(text) {
            Cow::Borrowed(t) => mask_detectors(t),
            Cow::Owned(s) => Cow::Owned(mask_detectors(&s).into_owned()),
        }
    }

    pub fn redact_string(&self, text: &mut String) -> bool {
        let changed = match self.redact(text) {
            Cow::Borrowed(_) => None,
            Cow::Owned(s) => Some(s),
        };
        match changed {
            Some(s) => {
                *text = s;
                true
            }
            None => false,
        }
    }

    /// Every string value in a JSON document (keys are left alone).
    pub fn redact_json(&self, v: &mut Value) -> bool {
        match v {
            Value::String(s) => self.redact_string(s),
            Value::Array(a) => a.iter_mut().fold(false, |c, x| self.redact_json(x) | c),
            Value::Object(o) => o
                .iter_mut()
                .fold(false, |c, (_, x)| self.redact_json(x) | c),
            _ => false,
        }
    }

    /// Does `text` contain a known value or a token shape?
    pub fn contains_secret(&self, text: &str) -> bool {
        matches!(self.redact(text), Cow::Owned(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "Zp9qL2vX8rT5wY1u";

    fn r() -> Redactor {
        let r = Redactor::new();
        assert!(r.register("TODOIST_TOKEN", V) >= 3);
        r
    }

    #[test]
    fn masks_raw_and_every_encoding() {
        let r = r();
        assert_eq!(
            r.redact(&format!("token is {V}.")),
            "token is [secret:TODOIST_TOKEN]."
        );
        for enc in encodings(V) {
            let out = r.redact(&format!("x {enc} y")).into_owned();
            assert!(!out.contains(&enc), "{enc} -> {out}");
            assert!(out.contains("[secret:TODOIST_TOKEN]"), "{enc} -> {out}");
        }
        // base64 of "user:" + value (another alignment), standard padding.
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("user:{V}"));
        let out = r.redact_known(&format!("blob {basic}")).into_owned();
        assert!(!out.contains(&basic[8..24]), "{out}");
        assert!(out.contains("[secret:TODOIST_TOKEN]"), "{out}");
        // JSON inside JSON.
        let doc =
            serde_json::json!({"inner": serde_json::json!({"t": format!("a\"{V}")}).to_string()});
        let s = doc.to_string();
        assert!(!r.redact(&s).contains(V));
    }

    #[test]
    fn keeps_no_values_and_unicode_safe() {
        let r = Redactor::new();
        r.register("PASS_PHRASE", "pässwörd-ünïcode-42");
        let out = r.redact("ciao pässwörd-ünïcode-42 è");
        assert_eq!(out, "ciao [secret:PASS_PHRASE] è");
        let inner = r.read();
        // only digests: no stored string contains the value
        assert!(!format!("{:?}", inner.names).contains("pässwörd"));
        assert_eq!(r.known_names(), vec!["PASS_PHRASE".to_string()]);
    }

    #[test]
    fn short_values_are_not_registered() {
        let r = Redactor::new();
        assert_eq!(r.register("PIN", "1234"), 0);
        assert_eq!(r.redact("1234"), "1234");
    }

    #[test]
    fn detector_masks_token_shapes() {
        let r = Redactor::new();
        let cases = [
            "sk-abcdefghijklmnop1234",
            "ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234",
            "github_pat_11ABCDEFG0123456789_abcdefghijk",
            "mcp_Q2hhbmdlTWUxMjM0NTY3OA",
            "sec_9f8e7d6c5b4a39281706",
            "svc_live_0123456789abcdefXYZ",
            "xoxb-123456789012-abcdefghijkl",
            "AKIAIOSFODNN7EXAMPLE",
            "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
            "Q9zX4mL7pR2tV8wY3nB6cK1jH5gF0dS9aE7uI2oP",
        ];
        for c in cases {
            let out = r.redact(&format!("here: {c} end")).into_owned();
            assert!(out.contains("[secret:"), "{c} -> {out}");
            assert!(!out.contains(c), "{c} -> {out}");
        }
        assert_eq!(
            r.redact("Authorization: Bearer abcdEFGH1234ijklMNOP5678"),
            "Authorization: Bearer [secret:token]"
        );
        assert_eq!(
            r.redact(r#"{"password":"hunter2hunter2","user":"g"}"#),
            r#"{"password":"[secret:credential]","user":"g"}"#
        );
        assert_eq!(
            r.redact("https://x.test/v1?apikey=abcdef123456&days=3"),
            "https://x.test/v1?apikey=[secret:credential]&days=3"
        );
    }

    #[test]
    fn detector_leaves_ordinary_text_and_ids() {
        let r = Redactor::new();
        let keep = [
            "01JABCDEFGHJKMNPQRSTVWXYZ0",           // ULID
            "550e8400-e29b-41d4-a716-446655440000", // UUID
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", // sha256
            "/api/secretary/timeline/stream?after=01JABCDEFGHJKMNPQRSTVWXYZ0",
            "Domani a Copenaghen: pioggia leggera, 5-10 °C.",
            "ThisIsAnExtremelyLongCamelCaseIdentifierName",
            "secret://lucina/prod/lucina#OPEN_METEO_API_KEY",
            "gdrive:150qHWei8cXQbw1ssTSWFjEgC-XsmyKX3",
            "[secret:TODOIST_TOKEN] and [secret:token]",
            "La password di Gmail la trovi in Infisical.",
            "token: set, password: missing",
            r#"{"ref":"secret://lucina/prod/lucina#X_KEY","set":true}"#,
            "secret: secret://lucina/prod/lucina#X_KEY",
        ];
        for k in keep {
            assert_eq!(r.redact(k), k, "{k}");
        }
    }

    #[test]
    fn json_strings_only() {
        let r = r();
        let mut v = serde_json::json!({"a": [format!("x{V}x"), 3], "b": {"c": V}, V: 1});
        assert!(r.redact_json(&mut v));
        assert_eq!(v["a"][0], "x[secret:TODOIST_TOKEN]x");
        assert_eq!(v["b"]["c"], "[secret:TODOIST_TOKEN]");
    }
}

/// The redactor overlap finding (docs/verification.md): the Verus proof of
/// "no secret survives" and "a second pass changes nothing" needs `wf`; these
/// are its two counterexamples on the code without the admission check.
#[cfg(test)]
mod overlap_tests {
    use super::*;

    fn assert_wf_guarantees(r: &Redactor, values: &[&str], input: &str) {
        let once = r.redact_known(input).into_owned();
        assert_eq!(r.redact_known(&once), once, "second pass rewrote: {once}");
        for v in values {
            assert!(!once.contains(v), "{v} survives in {once}");
        }
    }

    #[test]
    fn a_value_inside_a_placeholder_is_rejected() {
        let r = Redactor::new();
        assert!(r.try_register("WORD", "Zp9qL2vX8rT5").is_ok());
        assert_eq!(
            r.try_register("PASS", "secret"),
            Err(Rejected::InsidePlaceholder(
                "PASS".into(),
                "[secret:WORD]".into()
            ))
        );
        assert_eq!(
            r.try_register("PASS", "ecret:"),
            r.try_register("PASS", "ecret:")
        );
        assert!(r.try_register("PASS", "ecret:").is_err());
        // Best effort keeps only admissible encodings; the guarantees hold.
        r.register("PASS", "secret");
        assert_eq!(r.rejected().len(), 1);
        assert_wf_guarantees(&r, &["Zp9qL2vX8rT5"], "Zp9qL2vX8rT5 and secret");
        // Before: "secret" was a pattern, "[secret:WORD]" was rewritten again.
    }

    #[test]
    fn a_value_ending_in_a_placeholder_is_rejected() {
        let r = Redactor::new();
        r.try_register("X", "aaaaaa").unwrap();
        // "aaaaaabbbbbb" -> "[secret:X]bbbbbb", which is Y's value.
        assert_eq!(
            r.try_register("Y", "X]bbbbbb"),
            Err(Rejected::Bracket("Y".into()))
        );
        assert_wf_guarantees(&r, &["aaaaaa"], "aaaaaabbbbbb");
        r.register("Y", "X]bbbbbb");
        let out = r.redact_known("aaaaaabbbbbb").into_owned();
        assert_eq!(out, "[secret:X]bbbbbb");
        assert_eq!(r.redact_known(&out), out);
        // Its URL-encoded form has no brackets and is still caught.
        assert_eq!(r.redact_known("q=X%5Dbbbbbb"), "q=[secret:Y]");
    }

    #[test]
    fn a_name_whose_placeholder_contains_a_secret_is_rejected() {
        let r = Redactor::new();
        r.try_register("A", "GITHUB").unwrap();
        assert_eq!(
            r.try_register("GITHUB_TOKEN", "Zp9qL2vX8rT5wY1u"),
            Err(Rejected::NameContainsSecret("GITHUB_TOKEN".into()))
        );
        let r = Redactor::new();
        r.try_register("GITHUB_TOKEN", "Zp9qL2vX8rT5wY1u").unwrap();
        assert!(matches!(
            r.try_register("A", "GITHUB"),
            Err(Rejected::InsidePlaceholder(..))
        ));
        // Detector placeholders count too: "credential" is in [secret:credential].
        assert!(Redactor::new().try_register("C", "credential").is_err());
    }

    #[test]
    fn admissible_values_keep_both_guarantees() {
        let r = Redactor::new();
        let values = [
            "Zp9qL2vX8rT5wY1u",
            "hunter2hunter2",
            "pa:ss-word_99",
            "ab(cd)ef{}",
        ];
        for (i, v) in values.iter().enumerate() {
            assert!(r.try_register(&format!("K{i}"), v).unwrap() > 0, "{v}");
        }
        for input in [
            "Zp9qL2vX8rT5wY1uhunter2hunter2",
            "x hunter2hunter2hunter2 [secret:K0] pa:ss-word_99ab(cd)ef{}",
            "\"Zp9qL2vX8rT5wY1u\" %22pa%3Ass-word_99%22",
        ] {
            assert_wf_guarantees(&r, &values, input);
        }
        assert!(admissible(b"abcdef", &[b"[secret:K]".to_vec()]));
        assert!(!admissible(b"ecret", &[b"[secret:K]".to_vec()]));
        assert!(!admissible(b"ab]cd", &[]));
        assert!(!admissible(b"", &[]));
    }

    #[test]
    fn only_labelled_drive_ids_pass() {
        let id = "150qHWei8cXQbw1ssTSWFjEgC-XsmyKX3";
        assert_eq!(
            mask_entropy(&format!("folder gdrive:{id} ok")),
            format!("folder gdrive:{id} ok")
        );
        assert_ne!(
            mask_entropy(&format!("folder {id} ok")),
            format!("folder {id} ok")
        );
        // The label does not excuse other shapes.
        let b64 = "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRkdISUpL+/AB==";
        assert_ne!(
            mask_entropy(&format!("gdrive:{b64}")),
            format!("gdrive:{b64}")
        );
    }
}
