//! Secrets out of text before anything is written (docs/diagnostics.md, "Privacy").
//!
//! Applied to the message, the stack, every string in `context` and every text
//! attachment. Replaced with `[redacted]`:
//! - private key blocks (`-----BEGIN … PRIVATE KEY-----`);
//! - the password of URLs with credentials (`https://user:pw@host`);
//! - `Authorization`, `Cookie`, `Set-Cookie` and `X-…-Gateway/-Token/-Secret/-Api-Key`
//!   header values;
//! - `Bearer <token>` and `Basic <base64>` anywhere;
//! - `key=value` / `key: value` / `"key": "value"` where the key ends in password,
//!   passwd, secret, token, api_key, access_key, private_key or credential(s): this
//!   covers `OPENCODE_SERVER_PASSWORD=…`, Secretary Code's background-service
//!   `"password": "…"` (the engine's local sidecar password), `?access_token=…`;
//! - token shapes: OpenAI/Anthropic/DeepSeek `sk-…`, GitHub, Slack, AWS, Google
//!   API keys, Telegram bot tokens, JWTs, Secretary `sec_`/`svc_`/`mcp_` tokens.
//!
//! In `context`, the value of any key that names a secret is replaced whatever
//! its type. Short numbers, booleans and null values are kept (`max_token: 4096`).
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;

pub const REDACTED: &str = "[redacted]";

static PEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
        .unwrap()
});
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b([a-z][a-z0-9+.\-]*://)([^/\s:@"'<>]+):([^/\s@"'<>]+)@"#).unwrap()
});
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?im)\b((?:proxy-)?authorization|set-cookie|cookie|x-[a-z0-9-]*(?:gateway|api-key|token|secret))(["']?[ \t]*[:=][ \t]*["']?)([^\r\n"']+)"#,
    )
    .unwrap()
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(bearer)\s+[A-Za-z0-9\-._~+/]{8,}=*").unwrap());
static BASIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(basic)\s+([A-Za-z0-9+/]{12,}={0,2})").unwrap());
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_.\-]*(?:password|passwd|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|credentials?|sec_session))(["']?[ \t]*[=:][ \t]*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'&,;}\])<>]+)"#,
    )
    .unwrap()
});
static SHAPES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\bsk-[A-Za-z0-9_\-]{20,}",
        r"|\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}",
        r"|\bgithub_pat_[A-Za-z0-9_]{30,}",
        r"|\bglpat-[A-Za-z0-9_\-]{20,}",
        r"|\bxox[abposr]-[A-Za-z0-9\-]{10,}",
        r"|\bAKIA[0-9A-Z]{16}\b",
        r"|\bAIza[0-9A-Za-z_\-]{35}",
        r"|\b\d{8,10}:[A-Za-z0-9_\-]{35}\b",
        r"|\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
        r"|\b(?:sec|svc|mcp|mcpa|mcpr|npm|hf)_[A-Za-z0-9_\-]{20,}",
    ))
    .unwrap()
});

/// A value that is not worth hiding (and keeps logs readable).
fn harmless(v: &str) -> bool {
    let v = v.trim();
    v.is_empty()
        || v.starts_with("[redacted")
        || matches!(
            v.to_ascii_lowercase().as_str(),
            "null" | "undefined" | "none" | "true" | "false" | "nil" | "<unset>" | "<set>"
        )
        || (v.len() < 8 && v.bytes().all(|b| b.is_ascii_digit()))
}

/// Scrubs `s`; returns the clean text and how many secrets were replaced.
pub fn text(s: &str) -> (String, u32) {
    let mut n = 0u32;
    let out = PEM.replace_all(s, |_: &Captures| {
        n += 1;
        "[redacted private key]".to_owned()
    });
    let out = URL_USERINFO.replace_all(&out, |c: &Captures| {
        n += 1;
        format!("{}{}:{REDACTED}@", &c[1], &c[2])
    });
    let out = HEADER.replace_all(&out, |c: &Captures| {
        if harmless(&c[3]) {
            return c[0].to_owned();
        }
        n += 1;
        format!("{}{}{REDACTED}", &c[1], &c[2])
    });
    let out = BEARER.replace_all(&out, |c: &Captures| {
        n += 1;
        format!("{} {REDACTED}", &c[1])
    });
    let out = BASIC.replace_all(&out, |c: &Captures| {
        let t = &c[2];
        // "basic configuration" is prose; base64 credentials mix cases or digits.
        let looks_encoded = t.bytes().any(|b| b.is_ascii_digit() || b"+/=".contains(&b))
            || (t.bytes().any(|b| b.is_ascii_uppercase())
                && t.bytes().any(|b| b.is_ascii_lowercase()));
        if !looks_encoded {
            return c[0].to_owned();
        }
        n += 1;
        format!("{} {REDACTED}", &c[1])
    });
    let out = KEY_VALUE.replace_all(&out, |c: &Captures| {
        let v = &c[3];
        let (q, inner) = match v.as_bytes()[0] {
            b'"' | b'\'' => (&v[..1], &v[1..v.len() - 1]),
            _ => ("", v),
        };
        if harmless(inner) {
            return c[0].to_owned();
        }
        n += 1;
        format!("{}{}{q}{REDACTED}{q}", &c[1], &c[2])
    });
    let out = SHAPES.replace_all(&out, |_: &Captures| {
        n += 1;
        REDACTED.to_owned()
    });
    (out.into_owned(), n)
}

/// A JSON key whose value is a secret whatever it holds.
pub fn secret_key(k: &str) -> bool {
    let k: String = k
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "password",
        "passwd",
        "apikey",
        "authorization",
        "cookie",
        "privatekey",
        "credential",
        "accesskey",
        "clientsecret",
    ]
    .iter()
    .any(|w| k.contains(w))
        || k.ends_with("secret")
        || k.ends_with("token")
        || k == "auth"
}

/// Scrubs every string of `v` in place; returns the number of replacements.
pub fn json(v: &mut Value) -> u32 {
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
                let keep = match val {
                    Value::Null | Value::Bool(_) => true,
                    Value::String(s) => harmless(s),
                    _ => false,
                };
                if secret_key(k) && !keep {
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
    fn headers_and_bearer() {
        assert_eq!(
            clean("Authorization: Bearer abc.def-123456789"),
            "Authorization: [redacted]"
        );
        assert_eq!(
            clean("curl -H 'x' bearer sk_live_abcdefgh1234"),
            "curl -H 'x' bearer [redacted]"
        );
        assert_eq!(
            clean("cookie: sec_session=abcd1234efgh; theme=dark"),
            "cookie: [redacted]"
        );
        assert_eq!(
            clean("X-Memo-Gateway: 0123456789abcdef0123"),
            "X-Memo-Gateway: [redacted]"
        );
        assert_eq!(
            clean("auth=Basic b3BlbmNvZGU6c2VjcmV0"),
            "auth=Basic [redacted]"
        );
        assert_eq!(
            clean("a basic configuration file"),
            "a basic configuration file"
        );
    }

    #[test]
    fn key_values() {
        assert_eq!(
            clean("OPENCODE_SERVER_PASSWORD=hunter2hunter2 opencode serve"),
            "OPENCODE_SERVER_PASSWORD=[redacted] opencode serve"
        );
        assert_eq!(
            clean(r#"ready {"url":"http://127.0.0.1:4096","password":"s3cr3t pass"}"#),
            r#"ready {"url":"http://127.0.0.1:4096","password":"[redacted]"}"#
        );
        assert_eq!(
            clean("GET /cb?code=1&access_token=abcdef123&x=1"),
            "GET /cb?code=1&access_token=[redacted]&x=1"
        );
        assert_eq!(clean("api_key: 'AbC123xyz'"), "api_key: '[redacted]'");
        // not secrets
        for s in [
            "max_tokens=4096",
            "tokens: 1234 used",
            "secretary_url=https://getsecretary.tech",
            "password: null",
            "at validatePassword (auth.ts:12:3)",
            "session ses_01JABCDEF started",
        ] {
            assert_eq!(clean(s), s);
        }
    }

    #[test]
    fn urls_and_shapes() {
        assert_eq!(
            clean("fetch http://opencode:pw123@127.0.0.1:4096/session failed"),
            "fetch http://opencode:[redacted]@127.0.0.1:4096/session failed"
        );
        let (s, n) = text(
            "keys sk-ant-api03-abcdefghijklmnopqrstuvwx ghp_abcdefghijklmnopqrstuvwxyz0123456789 \
             AKIAABCDEFGHIJKLMNOP 123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw0 \
             eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U \
             sec_abcdefghijklmnopqrstuvwxyz",
        );
        assert_eq!(n, 6, "{s}");
        assert_eq!(s.matches(REDACTED).count(), 6, "{s}");
        let (s, n) = text("-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----");
        assert_eq!((s.as_str(), n), ("[redacted private key]", 1));
    }

    #[test]
    fn json_context() {
        let mut v = json!({
            "route": "/sessions",
            "serverPassword": "abc",
            "sidecar": {"url": "http://127.0.0.1:4096", "password": "x9", "pid": 12},
            "headers": [{"name": "authorization", "value": "Bearer abcdefgh123"}],
            "apiKey": 42424242,
            "max_tokens": 4096,
            "refreshToken": null
        });
        let n = json(&mut v);
        assert_eq!(n, 4);
        assert_eq!(v["serverPassword"], REDACTED);
        assert_eq!(v["sidecar"]["password"], REDACTED);
        assert_eq!(v["sidecar"]["pid"], 12);
        assert_eq!(v["headers"][0]["value"], "Bearer [redacted]");
        assert_eq!(v["apiKey"], REDACTED);
        assert_eq!(v["max_tokens"], 4096);
        assert!(v["refreshToken"].is_null());
    }
}
