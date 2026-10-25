//! Which handler reads a shared file. The bytes decide first (a PDF named
//! `notes.txt` is a PDF), then the extension, then the client's mime type.

use std::io::Read;
use std::path::Path;

/// Handler kinds. `timeline_kind` maps them to the coarser attachment kinds
/// the apps render.
pub const KINDS: &[&str] = &[
    "image", "video", "audio", "pdf", "document", "slides", "sheet", "text", "code", "archive",
    "calendar", "contact", "location", "link", "other",
];

/// What the first bytes say, when they are decisive.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    let at = |offset: usize, sig: &[u8]| head.get(offset..offset + sig.len()) == Some(sig);
    if starts(b"%PDF-") {
        return Some("pdf");
    }
    if starts(b"\x89PNG\r\n\x1a\n")
        || starts(b"\xff\xd8\xff")
        || starts(b"GIF87a")
        || starts(b"GIF89a")
        || (starts(b"RIFF") && at(8, b"WEBP"))
        || starts(b"BM")
            && head.len() > 14
            && u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize > 14
            && at(6, b"\0\0\0\0")
        || starts(b"II*\0")
        || starts(b"MM\0*")
    {
        return Some("image");
    }
    if at(4, b"ftyp") {
        let brand = head.get(8..12).unwrap_or(b"");
        return Some(match brand {
            b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif" => "image",
            b"M4A " | b"M4B " | b"M4P " => "audio",
            _ => "video",
        });
    }
    if starts(b"\x1a\x45\xdf\xa3") || (starts(b"RIFF") && at(8, b"AVI ")) {
        return Some("video");
    }
    if starts(b"ID3")
        || starts(b"OggS")
        || starts(b"fLaC")
        || (starts(b"RIFF") && at(8, b"WAVE"))
        || (head.len() > 1 && head[0] == 0xff && head[1] & 0xe0 == 0xe0 && head[1] & 0x06 != 0)
        || starts(b"#!AMR")
    {
        return Some("audio");
    }
    if starts(b"\x1f\x8b")
        || at(257, b"ustar")
        || starts(b"7z\xbc\xaf\x27\x1c")
        || starts(b"Rar!\x1a\x07")
        || starts(b"\xfd7zXZ\0")
        || starts(b"BZh")
    {
        return Some("archive");
    }
    if starts(b"{\\rtf") {
        return Some("document");
    }
    let text = std::str::from_utf8(head)
        .or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
        .unwrap_or("");
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("BEGIN:VCALENDAR") {
        return Some("calendar");
    }
    if trimmed.to_ascii_uppercase().starts_with("BEGIN:VCARD") {
        return Some("contact");
    }
    None
}

/// The kind of an OOXML/ODF/zip container, from its entry names.
fn zip_kind(path: &Path) -> Option<&'static str> {
    let archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let names: Vec<&str> = archive.file_names().take(4096).collect();
    let has = |p: &str| names.iter().any(|n| n.starts_with(p));
    if has("word/") {
        return Some("document");
    }
    if has("xl/") {
        return Some("sheet");
    }
    if has("ppt/") {
        return Some("slides");
    }
    if names.contains(&"mimetype") {
        let mut archive = archive;
        let mut mime = String::new();
        if let Ok(f) = archive.by_name("mimetype") {
            let _ = f.take(200).read_to_string(&mut mime);
        }
        return Some(match mime.trim() {
            m if m.ends_with("opendocument.text") => "document",
            m if m.ends_with("opendocument.spreadsheet") => "sheet",
            m if m.ends_with("opendocument.presentation") => "slides",
            _ => "archive",
        });
    }
    Some("archive")
}

fn ext_kind(ext: &str) -> Option<&'static str> {
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
        _ => return super::handlers::code::language(&format!("x.{ext}")).map(|_| "code"),
    })
}

fn mime_kind(mime: &str) -> Option<&'static str> {
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

/// Whether the head looks like text (UTF-8, no NULs).
fn texty(head: &[u8]) -> bool {
    !head.is_empty()
        && !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            // A multi-byte character cut at the end of the sample is fine.
            Err(e) => e.error_len().is_none(),
        }
}

/// Route a stored file.
pub fn route(path: &Path, name: &str, mime: &str) -> &'static str {
    let mut head = vec![0u8; 8192];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(n);
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if head.starts_with(b"PK\x03\x04") {
        return match ext_kind(&ext) {
            Some(k @ ("document" | "sheet" | "slides" | "archive")) => k,
            _ => zip_kind(path).unwrap_or("archive"),
        };
    }
    if let Some(k) = sniff(&head) {
        return k;
    }
    let by_name = ext_kind(&ext).or_else(|| mime_kind(mime));
    match by_name {
        // Text-like claims must actually be text.
        Some(k @ ("text" | "code" | "sheet" | "calendar" | "contact")) if !texty(&head) => {
            if k == "sheet" && matches!(ext.as_str(), "xls" | "numbers") {
                k
            } else {
                "other"
            }
        }
        Some(k) => k,
        None if texty(&head) => "text",
        None => "other",
    }
}

/// The coarse kind stored on timeline attachments (what the apps draw).
pub fn timeline_kind(kind: &str) -> &'static str {
    match kind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn bytes_win_over_names() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let pdf = file(d, "notes.txt", b"%PDF-1.7\n...");
        assert_eq!(route(&pdf, "notes.txt", "text/plain"), "pdf");
        let png = file(d, "photo.pdf", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        assert_eq!(route(&png, "photo.pdf", "application/pdf"), "image");
        let heic = file(d, "IMG.bin", b"\0\0\0\x18ftypheic\0\0\0\0");
        assert_eq!(route(&heic, "IMG.bin", "application/octet-stream"), "image");
        let mov = file(d, "clip", b"\0\0\0\x14ftypqt  \0\0\0\0");
        assert_eq!(route(&mov, "clip", ""), "video");
        let m4a = file(d, "memo", b"\0\0\0\x18ftypM4A \0\0\0\0");
        assert_eq!(route(&m4a, "memo", ""), "audio");
        let wav = file(d, "x.dat", b"RIFF\x24\0\0\0WAVEfmt ");
        assert_eq!(route(&wav, "x.dat", ""), "audio");
        let ics = file(d, "invite", b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
        assert_eq!(route(&ics, "invite", "application/octet-stream"), "calendar");
        let vcf = file(d, "card.txt", b"\xef\xbb\xbfBEGIN:VCARD\r\nVERSION:3.0\r\n");
        assert_eq!(route(&vcf, "card.txt", "text/plain"), "contact");
        let gz = file(d, "logs.bin", b"\x1f\x8b\x08\0\0\0\0\0");
        assert_eq!(route(&gz, "logs.bin", ""), "archive");
    }

    #[test]
    fn zip_containers_by_content() {
        let d = tempfile::tempdir().unwrap();
        let make = |name: &str, entries: &[(&str, &str)]| {
            let p = d.path().join(name);
            let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
            for (n, c) in entries {
                z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(c.as_bytes()).unwrap();
            }
            z.finish().unwrap();
            p
        };
        let docx = make("download", &[("word/document.xml", "<w/>")]);
        assert_eq!(route(&docx, "download", "application/octet-stream"), "document");
        let xlsx = make("data.zip", &[("xl/workbook.xml", "<w/>")]);
        assert_eq!(route(&xlsx, "data.zip", "application/zip"), "archive");
        let xlsx = make("data", &[("xl/workbook.xml", "<w/>")]);
        assert_eq!(route(&xlsx, "data", ""), "sheet");
        let odp = make("talk", &[("mimetype", "application/vnd.oasis.opendocument.presentation")]);
        assert_eq!(route(&odp, "talk", ""), "slides");
        let plain = make("photos", &[("a.jpg", "x")]);
        assert_eq!(route(&plain, "photos", ""), "archive");
    }

    #[test]
    fn names_and_text() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let rs = file(d, "main.rs", b"fn main() {}\n");
        assert_eq!(route(&rs, "main.rs", "application/octet-stream"), "code");
        let csv = file(d, "spese.csv", "data;importo\n1/10;12,30\n".as_bytes());
        assert_eq!(route(&csv, "spese.csv", "text/csv"), "sheet");
        let md = file(d, "README.md", b"# Title\n");
        assert_eq!(route(&md, "README.md", ""), "text");
        let unknown = file(d, "note", "ciao è".as_bytes());
        assert_eq!(route(&unknown, "note", "application/octet-stream"), "text");
        let bin = file(d, "blob.txt", b"\0\x01\x02garbage");
        assert_eq!(route(&bin, "blob.txt", "text/plain"), "other");
        assert_eq!(timeline_kind("sheet"), "document");
        assert_eq!(timeline_kind("code"), "file");
        assert!(KINDS.iter().all(|k| !timeline_kind(k).is_empty()));
    }
}
