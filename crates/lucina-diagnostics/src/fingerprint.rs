//! Dedupe key: app, version, kind and the normalized top stack frames.
//!
//! A frame keeps the function and the file's base name; line and column
//! numbers, directories (install paths, home folders), addresses, bundle
//! hashes, ids and numbers are dropped, so the same crash on another machine,
//! build path or run gives the same signature. Without a stack the first line
//! of the message, normalized the same way, stands in for the frames.
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::model::{AppName, Kind};

/// Frames that make the signature.
pub const TOP_FRAMES: usize = 5;

static FRAME_NO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:#\d+\s+|\d+\s*[:.]\s+|frame\s+#?\d+:?\s*)").unwrap());
/// Apple crash-report frames: `12  Secretary  0x0000000104a1c2d4 symbol + 40`.
static NATIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+\s+(\S+\s+0x[0-9a-fA-F]+\s)").unwrap());
static DIRS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[^\s()\[\]"'`]*[/\\]"#).unwrap());
static LINE_COL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":\d+(?::\d+)?").unwrap());
static PY_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bline \d+").unwrap());
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b0x[0-9a-f]+\b").unwrap());
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").unwrap()
});
static BUNDLE_HASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[.\-_][0-9a-f]{6,}(\.(?:m?js|css|wasm)\b)").unwrap());
static QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\?[^\s)]*").unwrap());
static LONG_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{12,}\b").unwrap());
static NUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
static QUOTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""[^"]*"|'[^']*'|`[^`]*`"#).unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// A line that is a stack frame rather than the error message.
fn is_frame(l: &str) -> bool {
    let l = l.trim_start();
    l.starts_with("at ")
        || l.starts_with("File \"")
        || FRAME_NO.is_match(l)
        || NATIVE.is_match(l)
        || l.contains(".swift")
        || l.contains(".kt:")
        || l.contains(".java:")
        || (l.contains('@') && l.contains(':') && !l.contains(' '))
}

fn common(s: &str) -> String {
    let s = UUID.replace_all(s, "<id>");
    let s = HEX.replace_all(&s, "0x?");
    let s = QUERY.replace_all(&s, "");
    let s = BUNDLE_HASH.replace_all(&s, "$1");
    let s = LONG_HEX.replace_all(&s, "<hex>");
    s.into_owned()
}

/// One frame without paths, positions and run-specific values.
pub fn normalize_frame(line: &str) -> String {
    let l = FRAME_NO.replace(line.trim(), "");
    let l = NATIVE.replace(&l, "$1");
    let l = common(&l);
    let l = DIRS.replace_all(&l, "");
    let l = LINE_COL.replace_all(&l, "");
    let l = PY_LINE.replace_all(&l, "line ?");
    let l = NUM.replace_all(&l, "#");
    SPACES.replace_all(l.trim(), " ").into_owned()
}

/// The message's first line without values that change between runs.
pub fn normalize_message(msg: &str) -> String {
    let l = msg
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let l = common(l);
    let l = QUOTED.replace_all(&l, "…");
    let l = DIRS.replace_all(&l, "");
    let l = NUM.replace_all(&l, "#");
    let l = SPACES.replace_all(l.trim(), " ");
    l.chars().take(200).collect()
}

/// The normalized top frames (or the normalized message when there are none).
pub fn signature(message: &str, stack: &str) -> Vec<String> {
    let lines: Vec<&str> = stack.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut frames: Vec<&str> = lines.iter().copied().filter(|l| is_frame(l)).collect();
    if frames.is_empty() {
        // An unknown format: every line but a leading message line.
        frames = lines.clone();
        if frames.len() > 1 {
            frames.remove(0);
        }
    }
    let mut out: Vec<String> = frames
        .iter()
        .map(|f| normalize_frame(f))
        .filter(|f| !f.is_empty())
        .take(TOP_FRAMES)
        .collect();
    if out.is_empty() {
        out.push(format!("message: {}", normalize_message(message)));
    }
    out
}

/// 16 hex characters of SHA-256 over app, version, kind and the signature.
pub fn fingerprint(app: AppName, version: &str, kind: Kind, signature: &[String]) -> String {
    let mut h = Sha256::new();
    for part in [app.as_str(), version.trim(), kind.as_str()] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for f in signature {
        h.update(f.as_bytes());
        h.update(b"\n");
    }
    h.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_frames_lose_paths_and_positions() {
        let a = "Error: Background service timed out after 30012ms\n    at startSidecar (/Applications/Secretary Code.app/Contents/Resources/app.asar/out/main/index.js:1234:56)\n    at async Promise.all (index 0)\n    at boot (file:///Users/nez/dev/main-3f9a8b2c.js:10:2)";
        let b = "Error: Background service timed out after 29871ms\n    at startSidecar (/Users/other/Applications/Secretary Code.app/Contents/Resources/app.asar/out/main/index.js:1240:9)\n    at async Promise.all (index 0)\n    at boot (file:///Users/nez/dev/main-77aa99bb.js:11:4)";
        let sa = signature("Background service timed out", a);
        assert_eq!(sa, signature("other text", b));
        assert_eq!(sa[0], "at startSidecar (Secretary index.js)");
        assert_eq!(sa[2], "at boot (main.js)");
        let v = "1.4.2";
        assert_eq!(
            fingerprint(AppName::SecretaryCode, v, Kind::Crash, &sa),
            fingerprint(AppName::SecretaryCode, v, Kind::Crash, &signature("", b))
        );
        assert_ne!(
            fingerprint(AppName::SecretaryCode, v, Kind::Crash, &sa),
            fingerprint(AppName::SecretaryCode, "1.4.3", Kind::Crash, &sa)
        );
    }

    #[test]
    fn native_and_jvm_frames() {
        let swift = "0   Secretary   0x0000000104a1c2d4 MemoRecorder.stop() + 120 (MemoRecorder.swift:88)\n1   Secretary   0x0000000104a1c000 closure #1 in MemoView.body.getter + 40";
        let s = signature("EXC_BAD_ACCESS", swift);
        assert_eq!(s.len(), 2);
        assert!(
            s[0].contains("MemoRecorder.stop()") && !s[0].contains("0x0000"),
            "{s:?}"
        );
        let kt = "java.lang.IllegalStateException: boom 42\n\tat tech.getsecretary.Share.send(Share.kt:120)\n\tat android.os.Handler.dispatchMessage(Handler.java:106)";
        assert_eq!(
            signature("", kt),
            vec![
                "at tech.getsecretary.Share.send(Share.kt)",
                "at android.os.Handler.dispatchMessage(Handler.java)"
            ]
        );
    }

    #[test]
    fn message_only() {
        let a = signature(
            "Request 1234 to \"/v1/x\" failed with id 6f1c2a9e-1b2c-4d5e-8f90-123456789abc",
            "",
        );
        let b = signature(
            "Request 99 to \"/v1/y\" failed with id 00000000-1b2c-4d5e-8f90-123456789abc",
            "",
        );
        assert_eq!(a, b);
        assert_eq!(a[0], "message: Request # to … failed with id <id>");
    }
}
