//! Report fingerprints: `sha256(app, app_version, error type, top frames)`.
//!
//! Frames are normalized so the same fault on another machine, run or
//! install path gives the same fingerprint: frame indices, hex addresses,
//! `+ offset`s, line and column numbers, directories, query strings and
//! hashes in file names are dropped; whitespace collapses. Without frame
//! lines, the first lines of the stack are used; without a stack, the message
//! with its numbers removed.
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// Frames that make the fingerprint.
pub const TOP_FRAMES: usize = 5;

static FRAME: LazyLock<Regex> = LazyLock::new(|| {
    // JS/Java/C# "at …", native "#3 …", Rust "  3: …", Swift/ObjC "3  Module  0x…",
    // Python 'File "…", line N', Go "pkg.fn(…)" with a tab-indented file line next.
    Regex::new(r#"^\s*(?:at\s+\S|#\d+\s|\d+:\s|\d+\s+\S+\s+0x|File\s+")"#).expect("frame regex")
});
static INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:at\s+|#\d+\s+|\d+:\s+|\d+\s+)").expect("index regex"));
static HEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]+|\+\s*\d+\b").expect("hex regex"));
static LINE_COL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?::\d+)+\b|,\s*line\s+\d+").expect("line regex"));
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:[A-Za-z][A-Za-z0-9+.\-]*://[^\s()]*?/|[A-Za-z]:\\(?:[^\\()\n]+\\)*|/?(?:[^/\s()"']+/)+)"#)
        .expect("path regex")
});
static QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\?[^\s():]*").expect("query regex"));
static HASHED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    // index-3f9a1c2b.js, main.a1b2c3d4e5.js
    Regex::new(r"[.\-][0-9a-fA-F]{8,}(\.[A-Za-z]+)\b").expect("hash name regex")
});
static NUMBERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+\b|\b[0-9a-fA-F\-]{16,}\b").expect("numbers regex"));
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("space regex"));

/// One frame line without what changes between machines and runs.
pub fn normalize_frame(line: &str) -> String {
    let s = INDEX.replace(line, "");
    let s = HEX.replace_all(&s, "");
    let s = QUERY.replace_all(&s, "");
    let s = PATH.replace_all(&s, "");
    let s = LINE_COL.replace_all(&s, "");
    let s = HASHED_NAME.replace_all(&s, "$1");
    SPACE.replace_all(s.trim(), " ").into_owned()
}

/// The error type of the first stack line ("TypeError" of "TypeError: x is undefined").
fn error_type(stack: &str) -> String {
    let first = stack.lines().map(str::trim).find(|l| !l.is_empty());
    match first {
        Some(l) if !FRAME.is_match(l) => l
            .split(':')
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>()
            .trim()
            .to_owned(),
        _ => String::new(),
    }
}

/// The normalized top frames of a stack (what the fingerprint is made of).
pub fn top_frames(stack: &str, message: &str) -> Vec<String> {
    let frames: Vec<String> = stack
        .lines()
        .filter(|l| FRAME.is_match(l))
        .map(normalize_frame)
        .filter(|f| !f.is_empty())
        .take(TOP_FRAMES)
        .collect();
    if !frames.is_empty() {
        return frames;
    }
    let lines: Vec<String> = stack
        .lines()
        .map(normalize_frame)
        .filter(|f| !f.is_empty())
        .take(TOP_FRAMES)
        .collect();
    if !lines.is_empty() {
        return lines;
    }
    let m = NUMBERS.replace_all(message.lines().next().unwrap_or(""), "#");
    vec![SPACE.replace_all(m.trim(), " ").chars().take(200).collect()]
}

pub fn compute(app: &str, version: &str, stack: &str, frames: &[String]) -> String {
    let mut h = Sha256::new();
    for part in [app, version, &error_type(stack)] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for f in frames {
        h.update(f.as_bytes());
        h.update([0]);
    }
    h.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_frames_ignore_paths_and_lines() {
        let a = "TypeError: x is undefined\n    at Session.load (/Users/a/App.app/Contents/Resources/app.asar/out/main/index-3f9a1c2b.js:120:33)\n    at async run (file:///Users/a/app/main.js:9:1)";
        let b = "TypeError: x is undefined\n    at Session.load (C:\\Program Files\\Secretary\\resources\\app.asar\\out\\main\\index-77aa88bb.js:121:7)\n    at async run (file:///C:/b/main.js:10:2)";
        let fa = top_frames(a, "x");
        assert_eq!(fa, vec!["Session.load (index.js)", "async run (main.js)"]);
        assert_eq!(fa, top_frames(b, "x"));
        assert_eq!(
            compute("secretary-code", "1.2.0", a, &fa),
            compute("secretary-code", "1.2.0", b, &top_frames(b, "x"))
        );
        assert_ne!(
            compute("secretary-code", "1.2.0", a, &fa),
            compute("secretary-code", "1.2.1", a, &fa)
        );
        let other = a.replace("TypeError", "RangeError");
        assert_ne!(
            compute("secretary-code", "1.2.0", a, &fa),
            compute("secretary-code", "1.2.0", &other, &top_frames(&other, "x"))
        );
    }

    #[test]
    fn native_frames() {
        let s = "0  Secretary  0x0000000104a8c000 closure #1 in Engine.start() + 120\n1  libswiftCore.dylib  0x00000001a0000000 swift_task + 40";
        assert_eq!(
            top_frames(s, ""),
            vec![
                "Secretary closure #1 in Engine.start()",
                "libswiftCore.dylib swift_task"
            ]
        );
        let r = "   0: lucina_server::chat::turn\n             at ./src/chat/turn.rs:42:9\n   1: tokio::runtime::task";
        assert_eq!(
            top_frames(r, ""),
            vec![
                "lucina_server::chat::turn",
                "turn.rs",
                "tokio::runtime::task"
            ]
        );
    }

    #[test]
    fn message_only() {
        assert_eq!(
            top_frames("", "background service timed out after 30000 ms (pid 4411)"),
            vec!["background service timed out after # ms (pid #)"]
        );
        assert_eq!(top_frames("", ""), vec![String::new()]);
    }
}
