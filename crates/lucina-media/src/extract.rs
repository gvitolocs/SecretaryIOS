//! Text and previews of Drive documents.
//!
//! - Text and code files are read as they are (UTF-8, at most [`MAX_TEXT_BYTES`]).
//! - Office files (docx/xlsx/pptx, odt/ods/odp) are zip archives of XML: the text is read out of
//!   that XML here, in pure Rust. RTF loses its control words.
//! - PDFs: poppler's `pdftotext` when the box has it, else `pdf-extract` (pure Rust).
//! - The first page of a PDF becomes `page1.jpg` (poppler's `pdftoppm`), which the thumbnail worker
//!   turns into the usual renditions; Office files become `doc.pdf` with LibreOffice
//!   (`soffice --headless`) when it is installed.
//!
//! Every external tool, and the in-process extractors when `drive_isolate` is on, run as a child
//! process under the same CPU cap as the film preparation (`systemd-run --user --scope -p
//! CPUQuota=…`, `nice -n 19`), with a memory cap and a timeout, one job at a time
//! ([`crate::ingest::Library::jobs`]). The background worker also waits while Tctl is at or above
//! `drive_max_temp_c` (90 °C).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::doctype::{self, DocType};

/// The most text kept for one file (search index and previews).
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Text extraction stops after this long.
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(120);
/// LibreOffice conversions may take this long.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(180);

/// Exit code of `lucina-media extract-text` when the file has no text.
pub const NO_TEXT_EXIT: i32 = 3;

/// Whether the first bytes of a file look like text: no NUL, valid UTF-8 (a cut last character is fine).
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && head.len() - e.valid_up_to() < 4,
    }
}

/// The first `max` bytes of a text file as a string, and whether there was more; `None` for binary.
pub fn read_text_prefix(path: &Path, max: usize) -> std::io::Result<Option<(String, bool)>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(max.min(1 << 20));
    f.by_ref().take(max as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > max;
    buf.truncate(max);
    if !looks_like_text(&buf[..buf.len().min(8192)]) {
        return Ok(None);
    }
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        // Don't end on half a character.
        while s.ends_with('\u{FFFD}') {
            s.pop();
        }
    }
    Ok(Some((s, truncated)))
}

/// Text out of Office XML: tags removed, entities decoded, a newline after each paragraph-like
/// closing tag in `breaks` (`w:p`, `a:p`, `text:p`, `row`, …) and a tab after cell-like ones.
pub fn xml_text(xml: &str, breaks: &[&str], tabs: &[&str]) -> String {
    let mut out = String::with_capacity(xml.len() / 4);
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        push_decoded(&mut out, &rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let closing = tag.starts_with('/');
        let empty = tag.ends_with('/');
        if (closing || empty) && breaks.contains(&name) {
            out.push('\n');
        } else if ((closing || empty) && tabs.contains(&name))
            || matches!(name, "w:tab" | "text:tab" | "a:tab")
        {
            out.push('\t');
        } else if matches!(name, "w:br" | "text:line-break" | "a:br" | "w:cr") {
            out.push('\n');
        } else if matches!(name, "text:s") {
            out.push(' ');
        }
        rest = &rest[lt + gt + 1..];
    }
    push_decoded(&mut out, rest);
    out
}

fn push_decoded(out: &mut String, s: &str) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let ent = &after[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
}

fn zip_entry(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.by_ref()
        .take(MAX_TEXT_BYTES as u64 * 4)
        .read_to_string(&mut s)
        .ok()?;
    Some(s)
}

/// The numbered parts (`ppt/slides/slide12.xml`) in their numeric order.
fn numbered(z: &zip::ZipArchive<std::fs::File>, prefix: &str) -> Vec<String> {
    let mut v: Vec<(u32, String)> = z
        .file_names()
        .filter_map(|n| {
            let rest = n.strip_prefix(prefix)?.strip_suffix(".xml")?;
            Some((rest.parse().ok()?, n.to_string()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

/// The text of an Office file (OOXML or OpenDocument), `None` when it isn't one we can read.
pub fn office_text(path: &Path, ext: &str) -> anyhow::Result<Option<String>> {
    if ext == "rtf" {
        let (raw, _) = read_text_prefix(path, MAX_TEXT_BYTES)?.unwrap_or_default();
        return Ok(Some(rtf_text(&raw)));
    }
    let Ok(mut z) = zip::ZipArchive::new(std::fs::File::open(path)?) else {
        // Legacy binary .doc/.xls/.ppt: only the converter can read them.
        return Ok(None);
    };
    let mut parts = Vec::new();
    match ext {
        "docx" | "docm" | "dotx" => {
            for n in [
                "word/document.xml",
                "word/footnotes.xml",
                "word/endnotes.xml",
            ] {
                if let Some(x) = zip_entry(&mut z, n) {
                    parts.push(xml_text(&x, &["w:p"], &["w:tc"]));
                }
            }
        }
        "pptx" | "pptm" | "ppsx" => {
            for (i, n) in numbered(&z, "ppt/slides/slide").iter().enumerate() {
                if let Some(x) = zip_entry(&mut z, n) {
                    parts.push(format!("— {} —\n{}", i + 1, xml_text(&x, &["a:p"], &[])));
                }
            }
            for n in numbered(&z, "ppt/notesSlides/notesSlide") {
                if let Some(x) = zip_entry(&mut z, &n) {
                    parts.push(xml_text(&x, &["a:p"], &[]));
                }
            }
        }
        "xlsx" | "xlsm" => {
            // Shared strings hold the text cells; inline strings and numbers are in the sheets.
            if let Some(x) = zip_entry(&mut z, "xl/sharedStrings.xml") {
                parts.push(xml_text(&x, &["si"], &[]));
            }
            for n in numbered(&z, "xl/worksheets/sheet") {
                if let Some(x) = zip_entry(&mut z, &n) {
                    let t = xml_text(&x, &["row"], &["c"]);
                    if t.chars().any(|c| c.is_alphanumeric()) {
                        parts.push(t);
                    }
                }
            }
        }
        "odt" | "ott" | "ods" | "ots" | "odp" | "otp" => {
            if let Some(x) = zip_entry(&mut z, "content.xml") {
                parts.push(xml_text(
                    &x,
                    &["text:p", "text:h", "table:table-row"],
                    &["table:table-cell"],
                ));
            }
        }
        _ => return Ok(None),
    }
    let text = tidy(&parts.join("\n"));
    Ok(Some(text))
}

/// RTF without its groups' control words: good enough to search and to quote.
fn rtf_text(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    let mut skip_depth: Option<usize> = None;
    let mut depth = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' => depth += 1,
            '}' => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            '\\' => {
                let mut word = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        word.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if word.is_empty() {
                    match chars.next() {
                        Some('*') => skip_depth = skip_depth.or(Some(depth)),
                        Some('\'') => {
                            let h: String = chars.by_ref().take(2).collect();
                            if skip_depth.is_none() {
                                if let Ok(b) = u8::from_str_radix(&h, 16) {
                                    out.push(b as char);
                                }
                            }
                        }
                        Some(o) if skip_depth.is_none() && "\\{}".contains(o) => out.push(o),
                        _ => {}
                    }
                    continue;
                }
                while chars
                    .peek()
                    .is_some_and(|n| n.is_ascii_digit() || *n == '-')
                {
                    chars.next();
                }
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
                if matches!(
                    word.as_str(),
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict"
                ) {
                    skip_depth = skip_depth.or(Some(depth));
                } else if skip_depth.is_none() && matches!(word.as_str(), "par" | "line" | "row") {
                    out.push('\n');
                } else if skip_depth.is_none() && matches!(word.as_str(), "tab" | "cell") {
                    out.push('\t');
                }
            }
            '\r' | '\n' => {}
            c if skip_depth.is_none() => out.push(c),
            _ => {}
        }
    }
    tidy(&out)
}

/// Trailing spaces off, at most two newlines in a row.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Text in this process: the extractor behind `lucina-media extract-text`, and the fallback when
/// `drive_isolate` is off (tests). `Ok(None)`: the file has no text.
pub fn extract_in_process(path: &Path, name: &str, doc: DocType) -> anyhow::Result<Option<String>> {
    let ext = doctype::ext_of(name);
    let text = match doc {
        DocType::Pdf => {
            let p = path.to_path_buf();
            // pdf-extract panics on some malformed files.
            match std::panic::catch_unwind(move || pdf_extract::extract_text(&p)) {
                Ok(Ok(t)) => Some(tidy(&t)),
                Ok(Err(e)) => anyhow::bail!("pdf: {e}"),
                Err(_) => anyhow::bail!("pdf: the reader panicked"),
            }
        }
        DocType::Document | DocType::Spreadsheet | DocType::Presentation => {
            office_text(path, &ext)?
        }
        _ => read_text_prefix(path, MAX_TEXT_BYTES)?.map(|(t, _)| t),
    };
    Ok(text.filter(|t| t.chars().any(|c| c.is_alphanumeric())))
}

/// `cmd args` as the capped background command: `nice -n 19`, inside a `systemd-run --user --scope`
/// with a CPU quota and a memory cap when the box allows scopes.
pub fn capped_command(cfg: &Config, program: &Path, args: &[OsString]) -> Command {
    let mut inner: Vec<OsString> = vec!["-n".into(), "19".into(), program.into()];
    inner.extend(args.iter().cloned());
    if cfg.prepare_scope {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--scope", "--quiet", "-p"])
            .arg(format!("CPUQuota={}%", cfg.drive_cpu_quota))
            .args(["-p", "MemoryMax=2G", "--", "nice"])
            .args(inner);
        c
    } else {
        let mut c = Command::new("nice");
        c.args(inner);
        c
    }
}

/// Run a capped command; `Ok(Some(code))` when it ended, `Ok(None)` when it was killed at `timeout`.
pub fn run_capped(
    cfg: &Config,
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> anyhow::Result<Option<i32>> {
    let mut child = capped_command(cfg, program, args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.code().unwrap_or(-1)));
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn part_of(out: &Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    out.with_file_name(name)
}

/// Extract the text of `path` (real name `name`) into `out`. `Ok(false)`: no text in this file.
pub fn extract_text(
    cfg: &Config,
    path: &Path,
    name: &str,
    doc: DocType,
    out: &Path,
) -> anyhow::Result<bool> {
    let tmp = part_of(out);
    let _ = std::fs::remove_file(&tmp);
    let wrote = |tmp: &Path| -> anyhow::Result<bool> {
        let has_text = std::fs::read_to_string(tmp)
            .map(|t| t.chars().any(|c| c.is_alphanumeric()))
            .unwrap_or(false);
        if has_text {
            std::fs::rename(tmp, out)?;
        } else {
            let _ = std::fs::remove_file(tmp);
        }
        Ok(has_text)
    };
    if doc == DocType::Pdf {
        if let Some(pdftotext) = &cfg.pdftotext {
            let args: Vec<OsString> = vec![
                "-enc".into(),
                "UTF-8".into(),
                "-q".into(),
                path.into(),
                tmp.clone().into(),
            ];
            return match run_capped(cfg, pdftotext, &args, EXTRACT_TIMEOUT)? {
                Some(0) => wrote(&tmp),
                Some(code) => anyhow::bail!("pdftotext exited with {code}"),
                None => anyhow::bail!("pdftotext timed out"),
            };
        }
    }
    if cfg.drive_isolate {
        let exe = std::env::current_exe()?;
        let args: Vec<OsString> = vec![
            "extract-text".into(),
            "--type".into(),
            doc.as_str().into(),
            "--name".into(),
            name.into(),
            path.into(),
            tmp.clone().into(),
        ];
        return match run_capped(cfg, &exe, &args, EXTRACT_TIMEOUT)? {
            Some(0) => wrote(&tmp),
            Some(NO_TEXT_EXIT) => Ok(false),
            Some(code) => anyhow::bail!("extract-text exited with {code}"),
            None => anyhow::bail!("extract-text timed out"),
        };
    }
    match extract_in_process(path, name, doc)? {
        Some(t) => {
            std::fs::write(&tmp, truncate_text(t))?;
            wrote(&tmp)
        }
        None => Ok(false),
    }
}

/// At most [`MAX_TEXT_BYTES`], cut on a character boundary.
pub fn truncate_text(mut t: String) -> String {
    if t.len() > MAX_TEXT_BYTES {
        let mut cut = MAX_TEXT_BYTES;
        while !t.is_char_boundary(cut) {
            cut -= 1;
        }
        t.truncate(cut);
    }
    t
}

/// The first page of a PDF as a JPEG (1600 px long side) at `out`, with `pdftoppm`.
pub fn pdf_page(cfg: &Config, pdf: &Path, out: &Path) -> anyhow::Result<bool> {
    let Some(pdftoppm) = &cfg.pdftoppm else {
        return Ok(false);
    };
    let prefix = part_of(out);
    let args: Vec<OsString> = vec![
        "-jpeg".into(),
        "-f".into(),
        "1".into(),
        "-l".into(),
        "1".into(),
        "-scale-to".into(),
        "1600".into(),
        "-singlefile".into(),
        pdf.into(),
        prefix.clone().into(),
    ];
    let made = {
        let mut p = prefix.into_os_string();
        p.push(".jpg");
        PathBuf::from(p)
    };
    match run_capped(cfg, pdftoppm, &args, EXTRACT_TIMEOUT)? {
        Some(0) if made.is_file() => {
            std::fs::rename(&made, out)?;
            Ok(true)
        }
        _ => {
            let _ = std::fs::remove_file(&made);
            Ok(false)
        }
    }
}

/// An Office file converted to PDF at `out` with LibreOffice, in a scratch directory next to it.
pub fn office_pdf(cfg: &Config, path: &Path, name: &str, out: &Path) -> anyhow::Result<bool> {
    let Some(soffice) = &cfg.soffice else {
        return Ok(false);
    };
    let dir = out.with_file_name("convert.part");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    // LibreOffice names the output after the input: give it a plain name.
    let ext = doctype::ext_of(name);
    let input = dir.join(format!("doc.{}", if ext.is_empty() { "bin" } else { &ext }));
    if std::fs::hard_link(path, &input).is_err() {
        std::fs::copy(path, &input)?;
    }
    let profile = cfg.data_dir.join("soffice-profile");
    let args: Vec<OsString> = vec![
        format!("-env:UserInstallation=file://{}", profile.display()).into(),
        "--headless".into(),
        "--norestore".into(),
        "--convert-to".into(),
        "pdf".into(),
        "--outdir".into(),
        dir.clone().into(),
        input.into(),
    ];
    let res = run_capped(cfg, soffice, &args, CONVERT_TIMEOUT);
    let made = dir.join("doc.pdf");
    let ok = matches!(res, Ok(Some(0))) && made.is_file();
    if ok {
        std::fs::rename(&made, out)?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    res?;
    Ok(ok)
}

/// A short piece of `text` around the first of `words` it contains (ASCII case-insensitive),
/// whitespace collapsed.
pub fn snippet(text: &str, words: &[String]) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let at = words
        .iter()
        .filter(|w| w.len() >= 2)
        .filter_map(|w| lower.find(&w.to_ascii_lowercase()))
        .min()?;
    let mut start = at.saturating_sub(80);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + 160).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let body: String = text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body,
        if end < text.len() { "…" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn office(path: &Path, parts: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, body) in parts {
            z.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn office_text_from_docx_pptx_xlsx_odt() {
        let t = tempfile::tempdir().unwrap();
        let docx = t.path().join("a.docx");
        office(
            &docx,
            &[(
                "word/document.xml",
                r#"<w:document><w:body><w:p><w:r><w:t>File carving</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve"> &amp; recovery</w:t></w:r></w:p><w:p><w:r><w:t>Second &#232; para</w:t></w:r></w:p></w:body></w:document>"#,
            )],
        );
        assert_eq!(
            office_text(&docx, "docx").unwrap().unwrap(),
            "File carving\t & recovery\nSecond è para"
        );
        let pptx = t.path().join("s.pptx");
        office(
            &pptx,
            &[
                (
                    "ppt/slides/slide10.xml",
                    "<p:sld><a:p><a:r><a:t>Ten</a:t></a:r></a:p></p:sld>",
                ),
                (
                    "ppt/slides/slide2.xml",
                    "<p:sld><a:p><a:r><a:t>Two</a:t></a:r></a:p></p:sld>",
                ),
            ],
        );
        let s = office_text(&pptx, "pptx").unwrap().unwrap();
        assert!(s.find("Two").unwrap() < s.find("Ten").unwrap(), "{s}");
        let xlsx = t.path().join("g.xlsx");
        office(
            &xlsx,
            &[(
                "xl/sharedStrings.xml",
                "<sst><si><t>Grade</t></si><si><t>Name</t></si></sst>",
            )],
        );
        assert_eq!(office_text(&xlsx, "xlsx").unwrap().unwrap(), "Grade\nName");
        let odt = t.path().join("n.odt");
        office(&odt, &[("content.xml", "<office:document-content><text:h>Title</text:h><text:p>Body<text:s/>text</text:p></office:document-content>")]);
        assert_eq!(
            office_text(&odt, "odt").unwrap().unwrap(),
            "Title\nBody text"
        );
        let legacy = t.path().join("old.doc");
        std::fs::write(&legacy, b"\xD0\xCF\x11\xE0 binary").unwrap();
        assert_eq!(office_text(&legacy, "doc").unwrap(), None);
    }

    #[test]
    fn rtf_and_entities() {
        assert_eq!(
            rtf_text(r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Hello\par World \'e8 \{x\}}"),
            "Hello\nWorld è {x}"
        );
        assert_eq!(
            xml_text("<a>1 &lt; 2 &bogus; &#x41;</a>", &[], &[]),
            "1 < 2 &bogus; A"
        );
    }

    #[test]
    fn text_detection_and_prefix() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x.txt");
        std::fs::write(&p, "àèìòù".repeat(10)).unwrap();
        let (s, cut) = read_text_prefix(&p, 7).unwrap().unwrap();
        assert!(cut);
        assert_eq!(s, "àèì");
        std::fs::write(&p, b"ab\0cd").unwrap();
        assert!(read_text_prefix(&p, 100).unwrap().is_none());
        assert!(looks_like_text(&"è".as_bytes()[..1]));
        assert!(!looks_like_text(&[0xff, 0xfe, 0x41, 0x42, 0x43]));
    }

    #[test]
    fn in_process_extraction_without_tools() {
        let t = tempfile::tempdir().unwrap();
        let cfg = Config::for_dirs(t.path().join("d"), t.path().join("s"));
        let p = t.path().join("notes.md");
        std::fs::write(&p, "# Digital Forensics\nfile carving").unwrap();
        let out = t.path().join("text.txt");
        assert!(extract_text(&cfg, &p, "notes.md", DocType::Text, &out).unwrap());
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "# Digital Forensics\nfile carving"
        );
        let bad = t.path().join("bad.pdf");
        std::fs::write(&bad, b"%PDF-1.4 not really").unwrap();
        assert!(
            extract_text(&cfg, &bad, "bad.pdf", DocType::Pdf, &t.path().join("b.txt")).is_err()
        );
        assert!(!t.path().join("b.txt").exists());
    }

    #[test]
    fn capped_command_shape() {
        let t = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_dirs(t.path().join("d"), t.path().join("s"));
        cfg.prepare_scope = true;
        cfg.drive_cpu_quota = 50;
        let c = capped_command(&cfg, Path::new("/usr/bin/pdftotext"), &["a.pdf".into()]);
        assert_eq!(c.get_program(), "systemd-run");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--user",
                "--scope",
                "--quiet",
                "-p",
                "CPUQuota=50%",
                "-p",
                "MemoryMax=2G",
                "--",
                "nice",
                "-n",
                "19",
                "/usr/bin/pdftotext",
                "a.pdf"
            ]
        );
        cfg.prepare_scope = false;
        assert_eq!(
            capped_command(&cfg, Path::new("x"), &[]).get_program(),
            "nice"
        );
    }

    #[test]
    fn snippets() {
        let text = format!(
            "{}The lecture covers file carving in depth. {}",
            "x ".repeat(100),
            "y ".repeat(200)
        );
        let s = snippet(&text, &["CARVING".into()]).unwrap();
        assert!(
            s.starts_with('…') && s.ends_with('…') && s.contains("file carving"),
            "{s}"
        );
        assert_eq!(snippet("short", &["nope".into()]), None);
    }
}
