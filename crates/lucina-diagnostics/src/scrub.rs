//! Removes obvious secrets from text before anything is stored.
//!
//! Replaced with `[redacted]`:
//! - `Bearer <token>` and `Basic <credentials>` (the engine's sidecar
//!   authenticates with `Basic base64(opencode:<password>)`);
//! - credentials in URLs (`https://user:pass@host`);
//! - values of secret-looking keys in `key=value`, `key: value` and JSON
//!   (`password`, `passwd`, `secret`, `token`, `api_key`, `authorization`,
//!   `cookie`, … including `OPENCODE_SERVER_PASSWORD=` and `"password": "…"`),
//!   and of `--password`/`--token`-style flags;
//! - `?key=`, `&sig=` query parameters;
//! - API-key shapes: `sk-…` (OpenAI, Anthropic, DeepSeek), `xai-…`, GitHub
//!   (`ghp_…`, `github_pat_…`), Slack (`xox?-…`), AWS (`AKIA…`), Google
//!   (`AIza…`), Stripe (`sk_live_…`), GitLab, Hugging Face, Telegram bot
//!   tokens, JWTs, Secretary's own `sec_`/`svc_`/`mcp_`/`mcpa_` tokens;
//! - PEM private key blocks.
//!
//! Context JSON: the value of any secret-looking key is replaced whole, and
//! every string is scrubbed as text.
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;

pub const REDACTED: &str = "[redacted]";

/// Key names whose values are secrets (a suffix match on the key, so
/// `OPENCODE_SERVER_PASSWORD` and `access_token` count; `max_tokens` does not).
const SECRET_KEYS: &str = r"password|passwd|passphrase|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|authorization|auth|cookie|credentials?";

static PEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
    )
    .expect("pem regex")
});

static SCHEME_AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer|basic|token|digest)(\s+)([A-Za-z0-9\-._~+/]{6,}=*)")
        .expect("auth regex")
});

static URL_CREDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.\-]*://)([^/\s:@]+):([^/\s@]+)@").expect("url regex")
});

static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)\b([a-z0-9_.\-]*(?:{SECRET_KEYS}))(["']?\s*[:=]\s*)("[^"\n]*"|'[^'\n]*'|[^\s"'&,;}}\]\)]+)"#
    ))
    .expect("key-value regex")
});

static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(--?(?:password|passwd|token|api-key|apikey|secret)(?:=|\s+))([^\s-][^\s]*)")
        .expect("flag regex")
});

static QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([?&](?:key|sig|signature|code|auth)=)([^&\s#]+)").expect("query regex")
});

/// Secretary's own tokens; a digit tells them from identifiers like `mcp_tool_handler`.
static SECRETARY_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sec|svc|mcp|mcpa)_[A-Za-z0-9_\-]{16,}").expect("secretary token regex")
});

static KEY_SHAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:",
        r"sk-[A-Za-z0-9_\-]{20,}",
        r"|xai-[A-Za-z0-9]{20,}",
        r"|(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}",
        r"|github_pat_[A-Za-z0-9_]{20,}",
        r"|xox[abposr]-[A-Za-z0-9\-]{10,}",
        r"|AKIA[0-9A-Z]{16}",
        r"|AIza[0-9A-Za-z_\-]{35}",
        r"|(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}",
        r"|glpat-[A-Za-z0-9_\-]{20,}",
        r"|hf_[A-Za-z0-9]{30,}",
        r"|\d{8,10}:AA[A-Za-z0-9_\-]{33,}",
        r"|eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
        r")"
    ))
    .expect("key shapes regex")
});

/// Scrubs `text`; returns the clean text and how many secrets were replaced.
pub fn text(input: &str) -> (String, usize) {
    let mut n = 0usize;
    let mut s = PEM
        .replace_all(input, |_: &Captures| {
            n += 1;
            REDACTED.to_owned()
        })
        .into_owned();
    s = SCHEME_AUTH
        .replace_all(&s, |c: &Captures| {
            // "token 12" / "basic setup" are words, not credentials: a credential has a digit
            // or is long. A Bearer value is always one.
            let v = &c[3];
            let scheme = c[1].to_ascii_lowercase();
            if matches!(scheme.as_str(), "basic" | "token")
                && v.len() < 16
                && !v.bytes().any(|b| b.is_ascii_digit())
            {
                return c[0].to_owned();
            }
            n += 1;
            format!("{}{}{REDACTED}", &c[1], &c[2])
        })
        .into_owned();
    s = URL_CREDS
        .replace_all(&s, |c: &Captures| {
            n += 1;
            format!("{}{REDACTED}@", &c[1])
        })
        .into_owned();
    s = KEY_SHAPES
        .replace_all(&s, |_: &Captures| {
            n += 1;
            REDACTED.to_owned()
        })
        .into_owned();
    s = SECRETARY_TOKENS
        .replace_all(&s, |c: &Captures| {
            if !c[0].bytes().any(|b| b.is_ascii_digit()) {
                return c[0].to_owned();
            }
            n += 1;
            REDACTED.to_owned()
        })
        .into_owned();
    s = KEY_VALUE
        .replace_all(&s, |c: &Captures| {
            let v = &c[3];
            let bare = v.trim_matches(|ch| ch == '"' || ch == '\'');
            if bare.is_empty() || bare == REDACTED || is_harmless(bare) {
                return c[0].to_owned();
            }
            n += 1;
            let quote = if v.starts_with('"') {
                "\""
            } else if v.starts_with('\'') {
                "'"
            } else {
                ""
            };
            format!("{}{}{quote}{REDACTED}{quote}", &c[1], &c[2])
        })
        .into_owned();
    s = FLAG
        .replace_all(&s, |c: &Captures| {
            if &c[2] == REDACTED {
                return c[0].to_owned();
            }
            n += 1;
            format!("{}{REDACTED}", &c[1])
        })
        .into_owned();
    s = QUERY
        .replace_all(&s, |c: &Captures| {
            n += 1;
            format!("{}{REDACTED}", &c[1])
        })
        .into_owned();
    (s, n)
}

/// Values of secret-looking keys that carry no secret (`token=null`, `auth: true`).
fn is_harmless(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "null"
            | "none"
            | "true"
            | "false"
            | "undefined"
            | "unset"
            | "***"
            | "<set>"
            // The credential after the scheme is handled by SCHEME_AUTH.
            | "bearer"
            | "basic"
            | "digest"
    ) || v.starts_with(REDACTED)
}

fn secret_key(k: &str) -> bool {
    static KEY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"(?i)(?:{SECRET_KEYS})$")).expect("key regex"));
    KEY.is_match(k.trim())
}

/// Scrubs a JSON value in place; returns the number of replacements.
pub fn json(v: &mut Value) -> usize {
    match v {
        Value::String(s) => {
            let (clean, n) = text(s);
            if n > 0 {
                *s = clean;
            }
            n
        }
        Value::Array(a) => a.iter_mut().map(json).sum(),
        Value::Object(o) => {
            let mut n = 0;
            for (k, val) in o.iter_mut() {
                let keep = matches!(val, Value::Null | Value::Bool(_))
                    || val.as_str().is_some_and(|s| s.is_empty() || is_harmless(s));
                if secret_key(k) && !keep && !val.is_object() && !val.is_array() {
                    *val = Value::String(REDACTED.into());
                    n += 1;
                } else {
                    n += json(val);
                }
            }
            n
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn clean(s: &str) -> String {
        text(s).0
    }

    #[test]
    fn auth_headers() {
        assert_eq!(
            clean("Authorization: Bearer abc.DEF-123_xyz"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            clean("authorization=Basic b3BlbmNvZGU6aHVudGVyMg=="),
            "authorization=Basic [redacted]"
        );
        assert_eq!(clean("a basic setup"), "a basic setup");
        assert_eq!(
            clean("mcp_tool_handler_function"),
            "mcp_tool_handler_function"
        );
        assert_eq!(
            clean("pwd=/home/nez session_id=ses_1"),
            "pwd=/home/nez session_id=ses_1"
        );
    }

    #[test]
    fn passwords_and_keys() {
        assert_eq!(
            clean("OPENCODE_SERVER_PASSWORD=hunter2 next"),
            "OPENCODE_SERVER_PASSWORD=[redacted] next"
        );
        assert_eq!(
            clean(r#"{"password": "two words", "user": "me"}"#),
            r#"{"password": "[redacted]", "user": "me"}"#
        );
        assert_eq!(clean("password=; x"), "password=; x");
        assert_eq!(clean("token: null"), "token: null");
        assert_eq!(
            clean("max_tokens=4096 tokens: 12"),
            "max_tokens=4096 tokens: 12"
        );
        assert_eq!(
            clean("run --password s3cret --verbose"),
            "run --password [redacted] --verbose"
        );
        assert_eq!(
            clean("GET /x?key=AbC123&sig=zz&page=2"),
            "GET /x?key=[redacted]&sig=[redacted]&page=2"
        );
    }

    #[test]
    fn urls_and_shapes() {
        assert_eq!(
            clean("fetch https://opencode:pw@127.0.0.1:4096/session failed"),
            "fetch https://[redacted]@127.0.0.1:4096/session failed"
        );
        for k in [
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345",
            "sk-proj-ABCDEFGHIJKLMNOPQRSTUV",
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            "AKIAABCDEFGHIJKLMNOP",
            "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234",
            "sec_0123456789abcdefABCD",
            "123456789:AAH-abcdefghijklmnopqrstuvwxyz0123456",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.c2lnbmF0dXJlLXZhbHVl",
        ] {
            let (s, n) = text(&format!("key {k} used"));
            assert_eq!(s, "key [redacted] used", "{k}");
            assert_eq!(n, 1, "{k}");
        }
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(clean(pem), "a\n[redacted]\nb");
    }

    #[test]
    fn ordinary_text_is_kept() {
        let s = "TypeError: Cannot read properties of undefined (reading 'id')\n    at Session.load (app.js:12:3)";
        assert_eq!(text(s), (s.to_owned(), 0));
    }

    #[test]
    fn context_json() {
        let mut v = json!({
            "route": "/settings",
            "apiKey": "abc",
            "sidecar": {"password": "pw123", "url": "http://opencode:pw@localhost:4096"},
            "headers": ["Authorization: Bearer abcdef123456"],
            "token": null,
            "auth": true,
        });
        let n = json(&mut v);
        assert_eq!(n, 4);
        assert_eq!(v["apiKey"], "[redacted]");
        assert_eq!(v["sidecar"]["password"], "[redacted]");
        assert_eq!(v["sidecar"]["url"], "http://[redacted]@localhost:4096");
        assert_eq!(v["headers"][0], "Authorization: Bearer [redacted]");
        assert_eq!(v["token"], Value::Null);
        assert_eq!(v["auth"], true);
        assert_eq!(v["route"], "/settings");
    }
}
