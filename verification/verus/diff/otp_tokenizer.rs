//! Shared helpers for the OTP differential tests (otp_extract.rs, otp_redact.rs):
//! the tokenizer that turns a body into the Verus kernels' input, and a generator
//! of synthetic SMS bodies. Every code, amount and number here is random/synthetic.
//!
//! Installed by `otp_run.sh` as `crates/lucina-phone-core/tests/otp_common/mod.rs`
//! (a directory, so it is not a test target of its own).
//!
//! The patterns, `ascii_digits` and `MAX_DISTANCE` are copied verbatim from
//! lucina-rs c357646 `crates/lucina-phone-core/src/otp.rs` (they are private there).
//! The differential tests compare the kernels fed by this tokenizer with the real
//! public functions, so a drift in the copy shows up as a mismatch.
#![allow(dead_code)]

use std::sync::LazyLock;

use regex::Regex;

// ---- copied from lucina-rs c357646 crates/lucina-phone-core/src/otp.rs ----------
pub const MAX_DISTANCE: usize = 64;
pub const MAX_BODY_BYTES: usize = 8192;

pub static CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?iu)(?:\b(?:otp|one[ -]time|verification|verify|security|authentication|authentification|login|sign[ -]?in|access",
        r"|verifica|sicurezza|accesso|autenticazione",
        r"|bestätigung|bestatigung|anmelde|sicherheits",
        r"|bekræft|bekraeft|engangs|verifikations",
        r"|vérification|connexion|sécurité",
        r"|seguridad|verificación|autenticación|confirmação",
        r"|подтверждения|проверки|входа)\p{L}*",
        r"|验证码|驗證碼|認証コード|인증",
        r"|(?:your|votre|tuo|tu|din|dit|dein)\s+(?:code|codice|código|codigo|kode)\b)",
    ))
    .expect("OTP context pattern")
});
pub static CODES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:[0-9]{3}[- ][0-9]{3}|[0-9]{4,8}|[A-Z0-9]{4,10})\b").expect("OTP code pattern")
});
pub static GOOGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bG[-\s\x{2011}\x{2013}]([0-9]{6})\b").expect("Google code pattern")
});
pub static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").expect("URL pattern"));

pub fn ascii_digits(body: &str) -> String {
    body.chars()
        .map(|c| {
            let n = c as u32;
            [0x660, 0x6f0, 0x966, 0xff10]
                .into_iter()
                .find(|zero| n >= *zero && n < *zero + 10)
                .map_or(c, |zero| char::from_u32(48 + n - zero).unwrap_or(c))
        })
        .collect()
}
// ---- end of the copy -------------------------------------------------------------

/// The kernels' candidate (otp_extract.rs / otp_redact.rs `Cand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cand {
    pub start: usize,
    pub end: usize,
    pub len: usize,
    pub has_digit: bool,
    pub all_digits: bool,
    pub after_plus: bool,
    pub in_url: bool,
    pub currency: bool,
    pub numeric_join: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    pub start: usize,
    pub end: usize,
}

/// The proposed fix's currency markers (whole tokens, case-insensitive) and symbols.
const CURRENCY_WORDS: &[&str] = &[
    "dkk", "kr", "kr.", "sek", "nok", "eur", "euro", "euros", "usd", "gbp", "chf", "pln", "inr", "rs", "rs.",
];
const CURRENCY_SYMBOLS: &[char] = &['€', '$', '£', '¥', '₹'];

fn currency_word(w: &str) -> bool {
    let w = w.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '('));
    CURRENCY_WORDS.iter().any(|c| w.eq_ignore_ascii_case(c))
}

/// FIX tokenizer flag: the number is next to a currency marker ("1500 DKK", "DKK 1500",
/// "$1999", "1500kr" cannot happen: CODES needs a word boundary).
pub fn currency_adjacent(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].trim_end();
    let after = text[end..].trim_start();
    if before.ends_with(CURRENCY_SYMBOLS) || text[end..].starts_with(CURRENCY_SYMBOLS) {
        return true;
    }
    let next = after.split_whitespace().next().unwrap_or("");
    let prev = before.split_whitespace().last().unwrap_or("");
    currency_word(next) || currency_word(prev)
}

/// FIX tokenizer flag: glued to another number by . , : / - ("1250,00", "2026-10-25").
pub fn numeric_join(text: &str, start: usize, end: usize) -> bool {
    let b = text.as_bytes();
    let sep = |c: u8| matches!(c, b'.' | b',' | b':' | b'/' | b'-');
    (end + 1 < b.len() && sep(b[end]) && b[end + 1].is_ascii_digit())
        || (start >= 2 && sep(b[start - 1]) && b[start - 2].is_ascii_digit())
}

/// One CODES match as a kernel candidate, plus the code `plausible` would return.
pub fn candidate(text: &str, start: usize, end: usize, in_url: bool) -> (Cand, String) {
    let raw = &text[start..end];
    let code = raw.replace(['-', ' '], "");
    let c = Cand {
        start,
        end,
        len: code.len(),
        has_digit: code.bytes().any(|b| b.is_ascii_digit()),
        all_digits: code.bytes().all(|b| b.is_ascii_digit()),
        after_plus: text[..start].ends_with('+'),
        in_url,
        currency: currency_adjacent(text, start, end),
        numeric_join: numeric_join(text, start, end),
    };
    (c, code)
}

/// What `parse_otp` works on, as the kernel's input.
pub struct ParseTokens {
    pub too_long: bool,
    /// The Google path's code (sender or text mentions google, and GOOGLE matched).
    pub google: Option<String>,
    pub cands: Vec<Cand>,
    pub codes: Vec<String>,
    pub ctxs: Vec<Ctx>,
}

pub fn parse_tokens(sender: &str, body: &str) -> ParseTokens {
    let too_long = body.len() > MAX_BODY_BYTES;
    let normalized = ascii_digits(body);
    let text = URL.replace_all(&normalized, "").into_owned();
    let google = if sender.eq_ignore_ascii_case("google") || text.to_lowercase().contains("google") {
        GOOGLE.captures(&text).map(|m| m[1].to_owned())
    } else {
        None
    };
    let ctxs = CONTEXT.find_iter(&text).map(|m| Ctx { start: m.start(), end: m.end() }).collect();
    let mut cands = Vec::new();
    let mut codes = Vec::new();
    for m in CODES.find_iter(&text) {
        let (c, code) = candidate(&text, m.start(), m.end(), false);
        cands.push(c);
        codes.push(code);
    }
    // Codes inside URLs: never seen by the real loop; the kernel gets them flagged.
    let mut removed = 0;
    for u in URL.find_iter(&normalized) {
        let pos = u.start() - removed;
        for m in CODES.find_iter(u.as_str()) {
            let (mut c, code) = candidate(u.as_str(), m.start(), m.end(), true);
            c.start = pos;
            c.end = pos + 1;
            cands.push(c);
            codes.push(code);
        }
        removed += u.len();
    }
    ParseTokens { too_long, google, cands, codes, ctxs }
}

/// What `otp_spans` works on (the raw text: no digit normalization, no URL removal).
pub struct RawTokens {
    pub cands: Vec<Cand>,
    pub ctxs: Vec<Ctx>,
    pub google: Vec<(usize, usize)>,
}

pub fn raw_tokens(text: &str) -> RawTokens {
    RawTokens {
        cands: CODES.find_iter(text).map(|m| candidate(text, m.start(), m.end(), false).0).collect(),
        ctxs: CONTEXT.find_iter(text).map(|m| Ctx { start: m.start(), end: m.end() }).collect(),
        google: GOOGLE.find_iter(text).map(|m| (m.start(), m.end())).collect(),
    }
}

// ---- generator -------------------------------------------------------------------

/// xorshift64*: deterministic, no dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len() as u64) as usize]
    }
    pub fn digits(&mut self, n: usize) -> String {
        (0..n).map(|_| char::from(b'0' + self.below(10) as u8)).collect()
    }
    pub fn alnum_code(&mut self, n: usize) -> String {
        const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789";
        let mut s: Vec<u8> = (0..n).map(|_| A[self.below(A.len() as u64) as usize]).collect();
        let k = self.below(n as u64) as usize;
        s[k] = b'0' + self.below(10) as u8;
        String::from_utf8(s).unwrap()
    }
}

/// Maps ASCII digits into another Unicode digit block (Arabic-Indic, extended, Devanagari, full-width).
pub fn unicode_digits(s: &str, block: u32) -> String {
    s.chars()
        .map(|c| if c.is_ascii_digit() { char::from_u32(block + (c as u32 - 48)).unwrap() } else { c })
        .collect()
}

pub const CONTEXTS: &[&str] = &[
    "Your verification code is",
    "Il tuo codice di verifica è",
    "Din engangskode er",
    "Your security code:",
    "Use this login code",
    "Sign-in code",
    "验证码：",
    "Код подтверждения:",
    "Votre code de sécurité est",
    "Tu código de verificación es",
    "OTP",
    "Ihr Sicherheitscode lautet",
    "verification fee",
    "security deposit",
];

pub const FILLER: &[&str] = &[
    "Expires in 5 minutes.",
    "Do not share it.",
    "Thanks for shopping.",
    "Ref",
    "Hi!",
    "Your parcel left the depot",
    "at 12:30",
    "Google",
    "Total",
    "paid",
    "-",
    ".",
    ",",
];

/// One synthetic fragment. `ascii_only` keeps ASCII digits and leaves out URLs.
pub fn fragment(r: &mut Rng, ascii_only: bool) -> String {
    match r.below(if ascii_only { 13 } else { 16 }) {
        0 | 1 => r.pick(CONTEXTS).to_owned(),
        2 => { let n = 4 + r.below(5) as usize; r.digits(n) },
        3 => format!("{}-{}", r.digits(3), r.digits(3)),
        4 => { let n = 4 + r.below(7) as usize; r.alnum_code(n) },
        5 => format!("{} {}", r.digits(4), r.pick(&["DKK", "kr", "EUR", "kr."])),
        6 => format!("{} {}", r.pick(&["DKK", "EUR", "USD"]), r.digits(4)),
        7 => format!("{}{}", r.pick(&["$", "€", "£"]), r.digits(4)),
        8 => format!("{},{}", r.digits(4), r.digits(2)),
        9 => format!("+45{}", r.digits(8)),
        10 => format!("{}-{}-{}", r.digits(4), r.digits(2), r.digits(2)),
        11 => { let n = 9 + r.below(3) as usize; r.digits(n) },
        12 => "x".repeat(r.below(70) as usize),
        13 => format!("https://example.test/{}{}", r.alnum_code(6), "a".repeat(r.below(80) as usize)),
        14 => unicode_digits(&r.digits(6), [0x660, 0x6f0, 0x966, 0xff10][r.below(4) as usize]),
        _ => format!("G-{}", r.digits(6)),
    }
    .to_owned()
        + if r.below(3) == 0 { "" } else { r.pick(&[" ", " ", ". ", ", ", ": ", "\n"]) }
}

pub fn body(r: &mut Rng, ascii_only: bool) -> String {
    let n = 1 + r.below(6) as usize;
    let mut s = String::new();
    for _ in 0..n {
        if r.below(3) == 0 {
            s.push_str(r.pick(FILLER));
            s.push(' ');
        }
        s.push_str(&fragment(r, ascii_only));
    }
    s
}

pub fn sender(r: &mut Rng) -> &'static str {
    ["Bank", "Shop", "Google", "google", "Service"][r.below(5) as usize]
}
