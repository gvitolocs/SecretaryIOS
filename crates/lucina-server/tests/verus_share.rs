//! Differential tests for the share kernels against plain-Rust copies of the Verus models
//! `verification/verus/share_kernels.rs` and `share_reconcile.rs` (branch share-verify):
//!
//! - SK1: the real `router::route` over every magic header x extension x mime type (and
//!   random names) against the kernel's decision table, with the kernel's inputs taken
//!   from the real `router::sniff`; every result is in `KINDS`, and its `timeline_kind`
//!   survives the real `timeline::normalize` (the whitelist and take(32)).
//! - SK4.4: the real `share::sanitize` over adversarial and random names against the
//!   kernel's sanitize (today's rule, `SANITIZE_FIXED = false`, including the ".." cut bug).
//! - SK4.1-SK4.3: the real `POST /v1/share` at the part, field-cap, url, character and id
//!   limits, and random field sequences, against the kernel's receive loop.
//! - SK2: repeated receives of one shareId (and a staged dir left by a crash before the
//!   save) give one userItemId and one user item.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{harness, script, H};
use lucina_server::share::handlers::code;
use lucina_server::share::router::{route, sniff, timeline_kind, KINDS};
use lucina_store::timeline::{normalize, Attachment, Meta, NewItem};
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};

/// Flip to `true` when sanitize drops trailing dots after the 120-byte cut (the fix
/// proposed in share_kernels.rs, `unfixed_sanitize_keeps_dotdot`).
const SANITIZE_FIXED: bool = true;

/// Plain-Rust copy of the Verus kernels.
mod kernel {
    // ------------------------------------------------------------ SK1 router

    pub const KINDS: [&str; 15] = [
        "image", "video", "audio", "pdf", "document", "slides", "sheet", "text", "code", "archive",
        "calendar", "contact", "location", "link", "other",
    ];
    pub const ATTACHMENT_KINDS: [&str; 11] = [
        "image", "voice", "file", "video", "audio", "pdf", "document", "link", "text", "contact",
        "location",
    ];

    pub fn sniff_range(w: &str) -> bool {
        matches!(w, "pdf" | "image" | "video" | "audio" | "archive" | "document" | "calendar" | "contact")
    }
    pub fn ext_range(w: &str) -> bool {
        matches!(
            w,
            "image" | "video" | "audio" | "pdf" | "document" | "slides" | "sheet" | "text" | "archive"
                | "calendar" | "contact" | "code"
        )
    }
    pub fn mime_range(w: &str) -> bool {
        matches!(
            w,
            "pdf" | "calendar" | "contact" | "sheet" | "text" | "archive" | "image" | "video" | "audio"
                | "document" | "slides" | "code"
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Zip {
        WordDir,
        XlDir,
        PptDir,
        OdText,
        OdSheet,
        OdPresentation,
        OdOther,
        Plain,
        Unreadable,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Input {
        pub zip: Option<Zip>,
        pub sniff: Option<&'static str>,
        pub ext: Option<&'static str>,
        pub xls_numbers: bool,
        pub mime: Option<&'static str>,
        pub texty: bool,
    }

    pub fn well_formed(i: &Input) -> bool {
        i.sniff.is_none_or(sniff_range)
            && i.ext.is_none_or(ext_range)
            && i.mime.is_none_or(mime_range)
            && (!i.xls_numbers || i.ext == Some("sheet"))
    }

    pub fn zip_kind(z: Zip) -> Option<&'static str> {
        match z {
            Zip::WordDir | Zip::OdText => Some("document"),
            Zip::XlDir | Zip::OdSheet => Some("sheet"),
            Zip::PptDir | Zip::OdPresentation => Some("slides"),
            Zip::OdOther | Zip::Plain => Some("archive"),
            Zip::Unreadable => None,
        }
    }

    pub fn route(i: &Input) -> &'static str {
        if let Some(z) = i.zip {
            return match i.ext {
                Some(k @ ("document" | "sheet" | "slides" | "archive")) => k,
                _ => zip_kind(z).unwrap_or("archive"),
            };
        }
        if let Some(k) = i.sniff {
            return k;
        }
        match i.ext.or(i.mime) {
            Some(k) if matches!(k, "text" | "code" | "sheet" | "calendar" | "contact") && !i.texty => {
                if k == "sheet" && i.xls_numbers {
                    k
                } else {
                    "other"
                }
            }
            Some(k) => k,
            None if i.texty => "text",
            None => "other",
        }
    }

    pub fn timeline_kind(w: &str) -> &'static str {
        match w {
            "image" => "image",
            "video" => "video",
            "audio" => "audio",
            "pdf" => "pdf",
            "document" | "slides" | "sheet" => "document",
            "text" => "text",
            "contact" => "contact",
            "location" => "location",
            "link" => "link",
            _ => "file",
        }
    }

    // ------------------------------------------------------------ SK4.4 sanitize

    pub fn is_control(c: char) -> bool {
        (c as u32) <= 0x1f || (0x7f..=0x9f).contains(&(c as u32))
    }
    pub fn char_ok(c: char) -> bool {
        !is_control(c) && c != '/' && c != '\\' && c != ':'
    }
    pub fn blen(s: &[char]) -> usize {
        s.iter().map(|c| c.len_utf8()).sum()
    }
    fn replace_dd(s: &[char]) -> Vec<char> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < s.len() {
            if i + 1 < s.len() && s[i] == '.' && s[i + 1] == '.' {
                out.push('_');
                i += 2;
            } else {
                out.push(s[i]);
                i += 1;
            }
        }
        out
    }
    fn trimmed(c: char) -> bool {
        c == '.' || c == ' '
    }
    /// The part after the last '.', unless that '.' is the first char.
    fn ext_suffix(s: &[char]) -> Vec<char> {
        match s.iter().rposition(|&c| c == '.') {
            Some(d) if d > 0 => {
                let e = &s[d + 1..];
                if blen(e) <= 20 {
                    let mut v = vec!['.'];
                    v.extend_from_slice(e);
                    v
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }
    pub fn sanitize(raw: &str, fixed: bool) -> String {
        let m: Vec<char> = raw.chars().map(|c| if char_ok(c) { c } else { '_' }).collect();
        let r = replace_dd(&m);
        let mut a = 0;
        while a < r.len() && trimmed(r[a]) {
            a += 1;
        }
        let mut b = r.len();
        while b > a && trimmed(r[b - 1]) {
            b -= 1;
        }
        let s = &r[a..b];
        if s.is_empty() {
            return "file".into();
        }
        if blen(s) <= 120 {
            return s.iter().collect();
        }
        let e = ext_suffix(s);
        let lim = 120 - blen(&e);
        let mut cut: Vec<char> = s[..lim.min(s.len())].to_vec();
        while !cut.is_empty() && blen(&cut) > lim {
            cut.pop();
        }
        if fixed {
            while cut.last() == Some(&'.') {
                cut.pop();
            }
        }
        cut.extend(e);
        cut.into_iter().collect()
    }

    // ------------------------------------------------------------ SK4.1-SK4.3 receive

    #[derive(Debug, Clone)]
    pub enum Kind {
        File,
        Text,
        Url,
        Meta,
        Scalar(&'static str),
        Unknown,
    }

    #[derive(Debug, Clone)]
    pub struct FieldIn {
        pub kind: Kind,
        pub bytes: usize,
        pub blank: bool,
        pub location: bool,
    }

    pub const MAX_PARTS: usize = 32;

    fn cap(k: &Kind) -> Option<usize> {
        match k {
            Kind::File => None,
            Kind::Text => Some(3 * 1024 * 1024),
            Kind::Meta => Some(64 * 1024),
            _ => Some(16 * 1024),
        }
    }

    /// run + finish (location_check = true; SK4.1 proves it never changes the answer):
    /// the accepted part count, or None.
    pub fn accepted_parts(fs: &[FieldIn], location_check: bool) -> Option<usize> {
        let (mut count, mut parts) = (0usize, 0usize);
        let (mut meta, mut location) = (false, false);
        let mut seen = std::collections::HashSet::new();
        for f in fs {
            count += 1;
            if count > MAX_PARTS {
                return None;
            }
            if matches!(f.kind, Kind::Unknown) {
                return None;
            }
            if cap(&f.kind).is_some_and(|c| f.bytes > c) {
                return None;
            }
            match &f.kind {
                Kind::File => parts += 1,
                Kind::Text => parts += usize::from(!f.blank),
                Kind::Url => {
                    if !f.blank {
                        if f.bytes > 8192 {
                            return None;
                        }
                        parts += 1;
                    }
                }
                Kind::Meta => {
                    if meta {
                        return None;
                    }
                    meta = true;
                    location = f.location;
                }
                Kind::Scalar(n) => {
                    if !seen.insert(*n) {
                        return None;
                    }
                }
                Kind::Unknown => unreachable!(),
            }
        }
        if location {
            if location_check && parts + 1 > MAX_PARTS {
                return None;
            }
            parts += 1;
        }
        (parts > 0).then_some(parts)
    }

    pub fn alnum(b: u8) -> bool {
        b.is_ascii_digit() || b.is_ascii_uppercase() || b.is_ascii_lowercase()
    }
    pub fn valid_id(s: &[u8]) -> bool {
        (8..=64).contains(&s.len()) && s.iter().all(|&b| alnum(b) || b == b'-')
    }
    pub fn valid_session(s: &[u8]) -> bool {
        s.len() <= 64 && s.iter().all(|&b| alnum(b) || b == b'-' || b == b'_')
    }
}

/// xorshift64*: deterministic random inputs without extra dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ================================================================ SK1: router

/// The kernel's input classifiers. `texty` and the ext/mime tables are copies of the
/// private router functions (the abstraction the Verus model takes as given).
fn texty(head: &[u8]) -> bool {
    !head.is_empty()
        && !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none(),
        }
}
fn ext_table(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff" | "avif" => "image",
        "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "3gp" => "video",
        "mp3" | "wav" | "m4a" | "aac" | "ogg" | "oga" | "opus" | "flac" | "amr" | "caf" => "audio",
        "pdf" => "pdf",
        "docx" | "doc" | "odt" | "rtf" | "pages" => "document",
        "pptx" | "ppt" | "odp" | "key" => "slides",
        "xlsx" | "xlsm" | "xls" | "ods" | "numbers" | "csv" | "tsv" => "sheet",
        "txt" | "md" | "markdown" | "log" | "text" => "text",
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "xz" | "bz2" | "jar" | "apk" | "epub" => "archive",
        "ics" | "ical" | "ifb" => "calendar",
        "vcf" | "vcard" => "contact",
        _ => return code::language(&format!("x.{ext}")).map(|_| "code"),
    })
}
fn mime_table(mime: &str) -> Option<&'static str> {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match m.as_str() {
        "application/pdf" => "pdf",
        "text/calendar" => "calendar",
        "text/vcard" | "text/x-vcard" | "text/directory" => "contact",
        "text/csv" | "text/tab-separated-values" => "sheet",
        "text/plain" | "text/markdown" => "text",
        "application/zip" | "application/gzip" | "application/x-tar" | "application/x-7z-compressed" => "archive",
        m if m.starts_with("image/") => "image",
        m if m.starts_with("video/") => "video",
        m if m.starts_with("audio/") => "audio",
        m if m.contains("wordprocessingml") || m.contains("opendocument.text") || m == "application/rtf" => "document",
        m if m.contains("spreadsheetml") || m.contains("opendocument.spreadsheet") => "sheet",
        m if m.contains("presentationml") || m.contains("opendocument.presentation") => "slides",
        m if m.starts_with("text/") || m == "application/json" || m == "application/xml" => "code",
        _ => return None,
    })
}

fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (n, c) in entries {
        z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(c.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// Every header class sniff knows, the zip classes, and text/binary edge cases.
fn heads() -> Vec<(&'static str, Vec<u8>, Option<kernel::Zip>)> {
    use kernel::Zip;
    let mut tar = vec![0u8; 600];
    tar[..5].copy_from_slice(b"a.txt");
    tar[257..262].copy_from_slice(b"ustar");
    let mut long_text = b"plain words ".repeat(800);
    long_text[9000] = 0; // past the 8192-byte head
    let mut corrupt = b"PK\x03\x04".to_vec();
    corrupt.extend_from_slice(&[0x55; 64]);
    let raw: Vec<(&str, &[u8])> = vec![
        ("pdf", b"%PDF-1.7\n1 0 obj"),
        ("png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
        ("jpeg", b"\xff\xd8\xff\xe0\0\x10JFIF\0"),
        ("gif87", b"GIF87a\x01\0\x01\0"),
        ("gif89", b"GIF89a\x01\0\x01\0"),
        ("webp", b"RIFF\x24\0\0\0WEBPVP8 "),
        ("bmp", b"BM\x46\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0"),
        ("bmp-like text", b"BMW is a car brand, not a bitmap"),
        ("tiff-le", b"II*\0\x08\0\0\0"),
        ("tiff-be", b"MM\0*\0\0\0\x08"),
        ("heic", b"\0\0\0\x18ftypheic\0\0\0\0mif1"),
        ("avif", b"\0\0\0\x18ftypavif\0\0\0\0"),
        ("mp4", b"\0\0\0\x18ftypisom\0\0\x02\0"),
        ("mov", b"\0\0\0\x14ftypqt  \0\0\0\0"),
        ("m4a", b"\0\0\0\x18ftypM4A \0\0\0\0"),
        ("mkv", b"\x1a\x45\xdf\xa3\x01\0\0\0"),
        ("avi", b"RIFF\x24\0\0\0AVI LIST"),
        ("id3", b"ID3\x04\0\0\0\0\0\0"),
        ("ogg", b"OggS\0\x02\0\0\0\0"),
        ("flac", b"fLaC\0\0\0\x22"),
        ("wav", b"RIFF\x24\0\0\0WAVEfmt "),
        ("mp3-frame", b"\xff\xfb\x90\x64\0\0"),
        ("amr", b"#!AMR\n\x3c"),
        ("gzip", b"\x1f\x8b\x08\0\0\0\0\0"),
        ("7z", b"7z\xbc\xaf\x27\x1c\0\x04"),
        ("rar", b"Rar!\x1a\x07\x01\0"),
        ("xz", b"\xfd7zXZ\0\0\x04"),
        ("bz2", b"BZh91AY&SY"),
        ("rtf", b"{\\rtf1\\ansi hello}"),
        ("ics", b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"),
        ("ics-indented", b"  \r\nBEGIN:VCALENDAR\r\n"),
        ("vcf-bom", b"\xef\xbb\xbfBEGIN:VCARD\r\nVERSION:3.0\r\n"),
        ("vcf-lower", b"begin:vcard\r\nversion:3.0\r\n"),
        ("ascii", b"hello world\n"),
        ("utf8", "ciao \u{e8} \u{65e5}\n".as_bytes()),
        ("cut-multibyte", b"abc \xc3"),
        ("nul", b"\0\x01\x02garbage"),
        ("bad-utf8", b"\xc3\x28abc"),
        ("empty", b""),
        ("json", b"{\"a\": 1}\n"),
        ("csv", b"data;importo\n1/10;12,30\n"),
        ("tar", &tar),
        ("long-text", &long_text),
        ("corrupt-zip", &corrupt),
    ];
    let mut out: Vec<(&'static str, Vec<u8>, Option<Zip>)> = raw
        .into_iter()
        .map(|(n, b)| (n, b.to_vec(), b.starts_with(b"PK\x03\x04").then_some(Zip::Unreadable)))
        .collect();
    for (label, entries, class) in [
        ("docx", vec![("word/document.xml", "<w/>")], Zip::WordDir),
        ("xlsx", vec![("xl/workbook.xml", "<w/>")], Zip::XlDir),
        ("pptx", vec![("ppt/presentation.xml", "<p/>")], Zip::PptDir),
        ("word+xl", vec![("xl/a.xml", "x"), ("word/b.xml", "w")], Zip::WordDir),
        ("odt", vec![("mimetype", "application/vnd.oasis.opendocument.text")], Zip::OdText),
        ("ods", vec![("mimetype", "application/vnd.oasis.opendocument.spreadsheet")], Zip::OdSheet),
        ("odp", vec![("mimetype", "application/vnd.oasis.opendocument.presentation")], Zip::OdPresentation),
        ("epub", vec![("mimetype", "application/epub+zip")], Zip::OdOther),
        ("plain-zip", vec![("a.jpg", "x"), ("b.txt", "y")], Zip::Plain),
    ] {
        out.push((label, zip_bytes(&entries), Some(class)));
    }
    out
}

const EXTS: &[&str] = &[
    "", "jpg", "JPEG", "png", "heic", "webp", "mp4", "MOV", "mkv", "mp3", "m4a", "wav", "opus", "pdf",
    "PDF", "docx", "doc", "odt", "rtf", "pages", "pptx", "key", "odp", "xlsx", "xls", "numbers", "Numbers",
    "ods", "csv", "tsv", "txt", "md", "log", "zip", "gz", "tar", "7z", "epub", "apk", "ics", "vcf", "rs",
    "py", "json", "yml", "html", "sh", "bin", "dat", "xyz", "exe",
];

const MIMES: &[&str] = &[
    "",
    "application/octet-stream",
    "application/pdf",
    "text/calendar",
    "text/vcard",
    "text/x-vcard",
    "text/directory",
    "text/csv",
    "text/tab-separated-values",
    "text/plain",
    "text/plain; charset=utf-8",
    "TEXT/MARKDOWN",
    "application/zip",
    "application/gzip",
    "application/x-tar",
    "application/x-7z-compressed",
    "image/png",
    "image/heic",
    "video/mp4",
    "audio/mpeg",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.oasis.opendocument.text",
    "application/rtf",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.oasis.opendocument.presentation",
    "text/html",
    "application/json",
    "application/xml",
    "application/x-unknown",
];

fn as_static(k: &str) -> &'static str {
    *kernel::KINDS
        .iter()
        .find(|w| **w == k)
        .unwrap_or_else(|| panic!("{k:?} is not a kind"))
}

/// The kernel's input for one stored file, from the real sniff and the copied tables.
fn kernel_input(head: &[u8], zip: Option<kernel::Zip>, name: &str, mime: &str) -> kernel::Input {
    let head = &head[..head.len().min(8192)];
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let input = kernel::Input {
        zip: if head.starts_with(b"PK\x03\x04") { zip } else { None },
        sniff: sniff(head).map(as_static),
        ext: ext_table(&ext),
        xls_numbers: matches!(ext.as_str(), "xls" | "numbers"),
        mime: mime_table(mime),
        texty: texty(head),
    };
    assert!(kernel::well_formed(&input), "{name} {mime}: {input:?}");
    input
}

fn check_route(path: &Path, head: &[u8], zip: Option<kernel::Zip>, name: &str, mime: &str, label: &str) {
    let real = route(path, name, mime);
    let k = kernel::route(&kernel_input(head, zip, name, mime));
    assert_eq!(real, k, "route({label}, {name:?}, {mime:?})");
    assert!(KINDS.contains(&real) && kernel::KINDS.contains(&real), "{real} not in KINDS");
    assert_eq!(timeline_kind(real), kernel::timeline_kind(real));
    assert!(kernel::ATTACHMENT_KINDS.contains(&timeline_kind(real)));
}

#[test]
fn sk1_route_matches_the_kernel_on_every_header_extension_and_mime() {
    assert_eq!(KINDS, kernel::KINDS.as_slice());
    let dir = tempfile::tempdir().unwrap();
    let heads = heads();
    let mut checked = 0;
    for (i, (label, bytes, zip)) in heads.iter().enumerate() {
        let path = dir.path().join(format!("h{i:02}"));
        std::fs::write(&path, bytes).unwrap();
        for ext in EXTS {
            let name = if ext.is_empty() { "upload".to_owned() } else { format!("file.{ext}") };
            for mime in MIMES {
                check_route(&path, bytes, *zip, &name, mime, label);
                checked += 1;
            }
        }
    }
    assert_eq!(checked, heads.len() * EXTS.len() * MIMES.len());
}

#[test]
fn sk1_route_matches_the_kernel_on_random_names() {
    let dir = tempfile::tempdir().unwrap();
    let heads = heads();
    let paths: Vec<_> = heads
        .iter()
        .enumerate()
        .map(|(i, (_, b, _))| {
            let p = dir.path().join(format!("h{i:02}"));
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect();
    let alphabet: Vec<char> = "abcXYZ019 _-.é日".chars().collect();
    let mut rng = Rng(0x5eed_0001);
    for _ in 0..6000 {
        let mut name: String = (0..rng.below(10)).map(|_| *rng.pick(&alphabet)).collect();
        for _ in 0..rng.below(3) {
            name.push('.');
            if rng.below(3) == 0 {
                name.extend((0..1 + rng.below(5)).map(|_| *rng.pick(&alphabet)));
            } else {
                let e = rng.pick(EXTS);
                name.push_str(&if rng.below(2) == 0 { e.to_ascii_uppercase() } else { e.to_string() });
            }
        }
        let h = rng.below(heads.len());
        let mime = rng.pick(MIMES);
        check_route(&paths[h], &heads[h].1, heads[h].2, &name, mime, heads[h].0);
    }
}

fn share_item(kinds: &[&str]) -> NewItem {
    NewItem {
        channel: "web".into(),
        role: "user".into(),
        attachments: kinds
            .iter()
            .map(|k| Attachment { kind: (*k).into(), name: Some("x".into()), ..Attachment::default() })
            .collect(),
        meta: Some(Meta::kind("share")),
        ..NewItem::default()
    }
}

#[test]
fn sk1_every_timeline_kind_survives_normalize() {
    let mut words: Vec<&str> = KINDS.to_vec();
    words.extend(["file", "voice", "", "PDF", "unknown-kind"]);
    let mut kept: Vec<&str> = Vec::new();
    for w in &words {
        let tk = timeline_kind(w);
        assert!(kernel::ATTACHMENT_KINDS.contains(&tk), "{w} -> {tk}");
        kept.push(tk);
    }
    // the parts receive builds without routing
    kept.extend(["text", "link", "location"]);
    for k in &kept {
        let item = normalize(share_item(&[k]), 1_760_000_000_000).unwrap();
        assert_eq!(item.attachments.unwrap()[0].kind, *k);
    }
    // 32 attachments (SK4.1's bound) all survive take(32), in order
    let many: Vec<&str> = (0..32).map(|i| kept[i % kept.len()]).collect();
    let item = normalize(share_item(&many), 1_760_000_000_000).unwrap();
    let got: Vec<String> = item.attachments.unwrap().into_iter().map(|a| a.kind).collect();
    assert_eq!(got, many);
    // the whitelist is real: a raw handler kind that timeline_kind maps away is dropped
    let item = normalize(share_item(&["code", "image"]), 1_760_000_000_000).unwrap();
    assert_eq!(item.attachments.unwrap().len(), 1);
}

// ================================================================ SK4.4: sanitize

fn check_sanitize(raw: &str) {
    let real = lucina_server::share::sanitize(raw);
    assert_eq!(real, kernel::sanitize(raw, SANITIZE_FIXED), "sanitize({raw:?})");
    assert!(!real.is_empty() && real.len() <= 120, "{raw:?} -> {real:?}");
    assert!(real.chars().all(kernel::char_ok), "{raw:?} -> {real:?}");
    let fixed = kernel::sanitize(raw, true);
    assert!(!fixed.contains("..") && !fixed.is_empty() && fixed.len() <= 120, "{raw:?} -> {fixed:?}");
}

#[test]
fn sk4_is_control_is_category_cc() {
    for c in (0..=0x10ffffu32).filter_map(char::from_u32) {
        assert_eq!(kernel::is_control(c), c.is_control(), "{c:?}");
    }
}

#[test]
fn sk4_sanitize_matches_the_kernel() {
    for raw in [
        "../../etc/passwd", "a\\b\n.jpg", "...", "", "/", " . . ", ".hidden", "..hidden..", "a....b",
        "a.:b", "C:\\x.txt", "\u{7f}\u{85}\u{9f}\u{a0}", "photo.JPEG", "😀.png",
    ] {
        check_sanitize(raw);
    }
    // the cut: dots and multi-byte chars at every position near the 120-byte boundary
    for k in 90..=125usize {
        for ext in ["", ".jpg", ".jpeg", ".tar.gz", ".abcdefghijklmnopqrstu", ".é"] {
            check_sanitize(&format!("{}.{}{ext}", "a".repeat(k), "b".repeat(10)));
            check_sanitize(&format!("{}.{}{ext}", "é".repeat(k / 2), "c".repeat(5)));
            check_sanitize(&format!("{} .x{ext}", "日".repeat(k / 3)));
            check_sanitize(&format!("{}.{ext}", "a".repeat(k)));
        }
    }
    let alphabet: Vec<char> = "ab0Z..._- /\\:\n\t\u{7f}\u{85}é日😀".chars().collect();
    let mut rng = Rng(0x5eed_0004);
    for _ in 0..20_000 {
        let len = rng.below(200);
        let raw: String = (0..len).map(|_| *rng.pick(&alphabet)).collect();
        check_sanitize(&raw);
    }
}

#[test]
fn sk4_unfixed_sanitize_keeps_dotdot() {
    // share_kernels.rs dotdot_witness
    let witness = format!("{}.{}.jpg", "a".repeat(115), "b".repeat(10));
    let real = lucina_server::share::sanitize(&witness);
    assert_eq!(real, kernel::sanitize(&witness, SANITIZE_FIXED));
    assert_eq!(real.contains(".."), !SANITIZE_FIXED, "{real}");
    assert_eq!(kernel::sanitize(&witness, false), format!("{}..jpg", "a".repeat(115)));
    assert_eq!(kernel::sanitize(&witness, true), format!("{}.jpg", "a".repeat(115)));
}

// ================================================================ SK4.1-SK4.3: receive

const KEY: &str = "gateway-test-secret";

async fn setup() -> H {
    harness(
        script(json!({"rules":[
            {"kind":"intent", "content":"{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind":"agent", "content":"Read it."}
        ]})),
        |c| {
            c.share.dir = c.data_dir.join("shares");
            c.share.key_file = Some(c.data_dir.join("share.key"));
            std::fs::write(c.share.key_file.as_ref().unwrap(), format!("{KEY}\n")).unwrap();
            // nothing listens: the library and vision steps fail fast and keep the files
            c.share.media_url = "http://127.0.0.1:9".into();
            c.share.vision_url = Some("http://127.0.0.1:9".into());
        },
    )
    .await
}

async fn submit(h: &H, form: Form) -> (u16, Value) {
    let r = h
        .http
        .post(format!("{}/v1/share", h.base))
        .header("x-share-gateway", KEY)
        .multipart(form)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// One generated form field: the kernel's view and what goes on the wire.
#[derive(Clone)]
struct Field {
    k: kernel::FieldIn,
    name: &'static str,
    value: String,
}

fn field(kind: kernel::Kind, name: &'static str, value: String) -> Field {
    let blank = value.trim().is_empty();
    let location = value.contains("location");
    Field { k: kernel::FieldIn { kind, bytes: value.len(), blank, location }, name, value }
}
fn text(v: &str) -> Field {
    field(kernel::Kind::Text, "text", v.into())
}
fn file(i: usize) -> Field {
    field(kernel::Kind::File, "file", format!("file {i}\n"))
}
fn meta(location: bool) -> Field {
    let v = if location { r#"{"location":{"lat":55.6,"lon":12.5}}"# } else { r#"{"files":[]}"# };
    field(kernel::Kind::Meta, "meta", v.into())
}
fn scalar(name: &'static str, v: &str) -> Field {
    field(kernel::Kind::Scalar(name), name, v.into())
}

fn form(fields: &[Field]) -> Form {
    fields.iter().enumerate().fold(Form::new(), |f, (i, x)| {
        if x.name == "file" {
            f.part(
                "file",
                Part::bytes(x.value.clone().into_bytes())
                    .file_name(format!("part{i}.txt"))
                    .mime_str("text/plain")
                    .unwrap(),
            )
        } else {
            f.text(x.name, x.value.clone())
        }
    })
}

/// Send the fields; compare the real answer with the kernel's.
async fn check_receive(h: &H, fields: &[Field], label: &str) -> (u16, Value) {
    let ks: Vec<kernel::FieldIn> = fields.iter().map(|f| f.k.clone()).collect();
    let expected = kernel::accepted_parts(&ks, true);
    assert_eq!(expected, kernel::accepted_parts(&ks, false), "SK4.1: the location check never fires");
    let (status, body) = submit(h, form(fields)).await;
    match expected {
        Some(n) => {
            assert_eq!(status, 202, "{label}: kernel accepts {n} parts: {body}");
            assert_eq!(body["attachments"].as_array().map(Vec::len), Some(n), "{label}: {body}");
            assert!(n <= kernel::MAX_PARTS);
        }
        None => assert!((400..500).contains(&status), "{label}: kernel refuses, got {status} {body}"),
    }
    (status, body)
}

#[tokio::test]
async fn sk4_part_limit_with_a_location() {
    let h = setup().await;
    let id = |n: usize| scalar("shareId", &format!("diff-parts-{n:04}"));
    let texts = |n: usize| (0..n).map(|i| text(&format!("part {i}"))).collect::<Vec<_>>();
    // shareId + 30 texts + meta(location) = 32 fields -> 30 + 1 = 31 parts
    let mut f = vec![id(1)];
    f.extend(texts(30));
    f.push(meta(true));
    let (_, body) = check_receive(&h, &f, "30 texts + location").await;
    assert_eq!(body["attachments"].as_array().unwrap().len(), 31);
    // without a shareId field: 31 texts + meta(location) = 32 fields -> 32 parts, the
    // maximum (accepted before 485d658 too: the meta field is one of the 32)
    let mut f = texts(31);
    f.push(meta(true));
    let (_, body) = check_receive(&h, &f, "31 texts + location").await;
    assert_eq!(body["attachments"].as_array().unwrap().len(), 32);
    // 32 texts + meta: the 33rd field is refused before any location exists
    let mut f = texts(32);
    f.push(meta(true));
    check_receive(&h, &f, "32 texts + location").await;
    // the location first: still 33 fields
    let mut f = vec![meta(true)];
    f.extend(texts(32));
    check_receive(&h, &f, "location + 32 texts").await;
    check_receive(&h, &texts(32), "32 texts").await;
    check_receive(&h, &texts(33), "33 texts").await;
    // blank texts count as fields, not parts
    let mut f = vec![id(2), text("   "), text("\n"), meta(true)];
    f.extend(texts(2));
    check_receive(&h, &f, "blanks").await;
    check_receive(&h, &[text("  "), meta(false)], "nothing to share").await;
    check_receive(&h, &[meta(true)], "only a location").await;
}

#[tokio::test]
async fn sk4_field_caps_and_scalar_limits() {
    let h = setup().await;
    let mib = 1024 * 1024;
    let cases: Vec<(Vec<Field>, &str)> = vec![
        (vec![text(&"x".repeat(3 * mib))], "text at 3 MiB"),
        (vec![text(&"x".repeat(3 * mib + 1))], "text over 3 MiB"),
        (vec![text("a"), field(kernel::Kind::Meta, "meta", format!("{{\"files\":[]{}}}", " ".repeat(64 * 1024 - 12)))], "meta at 64 KiB"),
        (vec![text("a"), field(kernel::Kind::Meta, "meta", format!("{{\"files\":[]{}}}", " ".repeat(64 * 1024 - 11)))], "meta over 64 KiB"),
        (vec![text("a"), scalar("note", &"n".repeat(16 * 1024))], "note bytes at 16 KiB but over 4000 chars"),
        (vec![text("a"), scalar("note", &"n".repeat(16 * 1024 + 1))], "note over 16 KiB"),
        (vec![field(kernel::Kind::Url, "url", format!("http://127.0.0.1:9/{}", "p".repeat(8192 - 19)))], "url at 8192"),
        (vec![field(kernel::Kind::Url, "url", format!("http://127.0.0.1:9/{}", "p".repeat(8192 - 18)))], "url over 8192"),
        (vec![text("a"), field(kernel::Kind::Unknown, "evil", "x".into())], "unknown field"),
        (vec![text("a"), scalar("lang", "en"), scalar("lang", "it")], "repeated scalar"),
        (vec![text("a"), meta(false), meta(true)], "repeated meta"),
    ];
    for (fields, label) in &cases {
        let (status, body) = submit(&h, form(fields)).await;
        let ks: Vec<kernel::FieldIn> = fields.iter().map(|f| f.k.clone()).collect();
        let loop_ok = kernel::accepted_parts(&ks, true).is_some();
        // the note case passes the loop and fails the 4000-character check after it
        let chars_ok = !label.starts_with("note bytes");
        assert_eq!(status == 202, loop_ok && chars_ok, "{label}: {status} {body}");
    }
    // SK4.3: character limits (multi-byte chars: the limits count chars, not bytes)
    for (key, max) in [("note", 4000usize), ("lang", 32), ("source", 120), ("title", 1000)] {
        for (n, ok) in [(max, true), (max + 1, false)] {
            let (status, body) = submit(&h, Form::new().text("text", "a").text(key, "è".repeat(n))).await;
            assert_eq!(status == 202, ok, "{key} with {n} chars: {status} {body}");
        }
    }
}

#[tokio::test]
async fn sk4_share_and_session_ids() {
    let h = setup().await;
    let alphabet: Vec<char> = "aZ09-_./ é\\".chars().collect();
    let mut rng = Rng(0x5eed_0003);
    for i in 0..160 {
        let len = rng.below(70);
        let mut id: String = (0..len).map(|_| *rng.pick(&alphabet)).collect();
        if i % 4 == 0 {
            // make sure plenty are valid and distinct
            id = format!("ok-{i:03}-{}", "a".repeat(rng.below(60)));
        }
        let (status, body) = submit(&h, Form::new().text("shareId", id.clone()).text("text", "a")).await;
        assert_eq!(status == 202, kernel::valid_id(id.as_bytes()), "shareId {id:?}: {status} {body}");
        if kernel::valid_id(id.as_bytes()) {
            assert!(!id.contains('/') && !id.contains('.') && id.len() >= 8);
        } else {
            assert_eq!(body["error"], "invalid shareId");
        }
        let mut session: String = (0..rng.below(70)).map(|_| *rng.pick(&alphabet)).collect();
        if i % 3 == 0 {
            session = "s_1-".repeat(rng.below(18));
        }
        let (status, body) = submit(&h, Form::new().text("sessionId", session.clone()).text("text", "a")).await;
        assert_eq!(status == 202, kernel::valid_session(session.as_bytes()), "sessionId {session:?}: {status} {body}");
    }
}

#[tokio::test]
async fn sk4_random_field_sequences_match_the_kernel() {
    let h = setup().await;
    let scalars: [(&'static str, &str); 7] = [
        ("intent", "ask"),
        ("note", "a note"),
        ("lang", "it"),
        ("sessionId", "s1"),
        ("channel", "web"),
        ("source", "photos"),
        ("title", "A title"),
    ];
    let mut rng = Rng(0x5eed_0002);
    for round in 0..90 {
        let mut fields = vec![scalar("shareId", &format!("diff-rand-{round:04}"))];
        for i in 0..rng.below(37) {
            let x = match rng.below(100) {
                0..=29 => text(&format!("text {i}")),
                30..=34 => text(" \n "),
                35..=44 => field(kernel::Kind::Url, "url", format!("http://127.0.0.1:9/{i}")),
                45..=47 => field(kernel::Kind::Url, "url", "  ".into()),
                48..=62 => file(i),
                63..=70 => meta(true),
                71..=74 => meta(false),
                75..=94 => {
                    let (n, v) = *rng.pick(&scalars);
                    scalar(n, v)
                }
                _ => field(kernel::Kind::Unknown, "bogus", "x".into()),
            };
            fields.push(x);
        }
        check_receive(&h, &fields, &format!("round {round}")).await;
    }
}

// ================================================================ SK2: idempotency

async fn wait_done(h: &H, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let v: Value = h
                .http
                .get(format!("{}/v1/share/{id}", h.base))
                .header("x-share-gateway", KEY)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if v["status"] != "processing" {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("share must finish")
}

fn user_items_for(h: &H, id: &str) -> Vec<Value> {
    h.timeline_lines()
        .into_iter()
        .filter(|l| l["role"] == "user" && l["meta"]["shareId"] == id)
        .collect()
}

#[tokio::test]
async fn sk2_repeated_receives_share_one_user_item() {
    let h = setup().await;
    let id = "diff-idem-0001";
    let mut ids = Vec::new();
    for n in 0..4 {
        let (status, body) = submit(&h, Form::new().text("shareId", id).text("text", "same share")).await;
        assert_eq!(status, 202, "{body}");
        assert_eq!(body["duplicate"], n > 0, "{body}");
        ids.push(body["userItemId"].as_str().unwrap().to_owned());
    }
    assert!(ids.iter().all(|x| *x == ids[0]), "SK2.2: {ids:?}");
    let done = wait_done(&h, id).await;
    assert_eq!(done["userItemId"], ids[0].as_str());
    // a retry after completion still answers the same id
    let (_, again) = submit(&h, Form::new().text("shareId", id).text("text", "same share")).await;
    assert_eq!(again["userItemId"], ids[0].as_str());
    let items = user_items_for(&h, id);
    assert_eq!(items.len(), 1, "SK2.3: {items:?}");
    assert_eq!(items[0]["id"], ids[0].as_str());
    let replies = h
        .timeline_lines()
        .into_iter()
        .filter(|l| l["role"] == "lucina" && l["replyTo"] == ids[0].as_str())
        .count();
    assert_eq!(replies, 1, "SK3.1");
}

#[tokio::test]
async fn sk2_a_dir_left_by_a_crash_before_the_save_is_replaced() {
    let h = setup().await;
    let id = "diff-crash-0001";
    let month = h
        .state
        .cfg
        .share
        .dir
        .join(chrono::Utc::now().format("%Y-%m").to_string());
    let stale = month.join(id);
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("01-partial.txt"), "half a file").unwrap();
    let (status, body) = submit(&h, Form::new().text("shareId", id).text("text", "after a crash")).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["duplicate"], false);
    assert!(!stale.join("01-partial.txt").exists());
    assert!(stale.join("status.json").exists());
    let (_, dup) = submit(&h, Form::new().text("shareId", id).text("text", "after a crash")).await;
    assert_eq!(dup["duplicate"], true);
    assert_eq!(dup["userItemId"], body["userItemId"]);
    assert_eq!(user_items_for(&h, id).len(), 1);
}
