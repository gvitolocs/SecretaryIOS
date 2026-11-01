//! A synthetic Google Takeout "Keep" export, built as zips in a temp dir.
//! Every note is invented; nothing here is real personal data.
//!
//! Part 1 (`takeout-001.zip`, root `Takeout/Keep/`) and part 2
//! (`takeout-002.zip`, a localized root `Mi Takeout/Keep/` plus a note JSON
//! outside any `Keep/` folder). Expected totals: 24 JSON files (22 notes,
//! 2 bad ones: a corrupt file and an empty note), 2 html, 7 attachments
//! found, 1 missing, 3 other files ignored.
#![allow(dead_code)]

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

pub type Files = Vec<(String, Vec<u8>)>;

/// Base of the `createdTimestampUsec` values: 2023-11-14T22:13:20Z.
pub const T0: i64 = 1_700_000_000_000_000;

/// `createdTimestampUsec` of note number `n` (1-based, one second apart).
pub fn usec(n: i64) -> i64 {
    T0 + n * 1_000_000
}

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// A valid 1x1 PNG; `tag` appends a byte after IEND so every tag is a
/// different file (different sha-256) that still reads as a PNG.
pub fn png(tag: u8) -> Vec<u8> {
    use base64::Engine;
    let mut b = base64::engine::general_purpose::STANDARD
        .decode(PNG_1X1)
        .unwrap();
    b.push(tag);
    b
}

/// A minimal JPEG (SOI, JFIF header, EOI) plus a distinguishing byte.
pub fn jpeg(tag: u8) -> Vec<u8> {
    let mut b = vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
    ];
    b.push(tag);
    b
}

/// A 3GPP audio header (`ftyp3gp4`) plus filler.
pub fn three_gp() -> Vec<u8> {
    let mut b = vec![
        0, 0, 0, 0x14, b'f', b't', b'y', b'p', b'3', b'g', b'p', b'4',
    ];
    b.extend_from_slice(&[0, 0, 0, 0, b'3', b'g', b'p', b'4', b'3', b'g', b'p', b'6']);
    b.extend_from_slice(&[7; 32]);
    b
}

pub fn amr() -> Vec<u8> {
    b"#!AMR\n\x3c\x00\x00".to_vec()
}

fn note(title: &str, text: &str, n: i64) -> Value {
    json!({
        "color": "DEFAULT",
        "isTrashed": false,
        "isPinned": false,
        "isArchived": false,
        "textContent": text,
        "title": title,
        "userEditedTimestampUsec": usec(n) + 60_000_000,
        "createdTimestampUsec": usec(n),
    })
}

fn put(files: &mut Files, name: &str, v: &Value) {
    files.push((name.to_owned(), serde_json::to_vec_pretty(v).unwrap()));
}

fn span(t: &str) -> String {
    format!(
        "<span style=\"font-size:11pt;font-family:Arial;color:#000000;font-weight:400;font-style:normal;text-decoration:none;white-space:pre-wrap;\">{t}</span>"
    )
}

/// The whole export, as `(entry name, bytes)` per part.
pub fn parts() -> (Files, Files) {
    let k = "Takeout/Keep";
    let mut a: Files = Vec::new();

    // 1: plain text with Takeout's span-per-line HTML.
    let mut n = note("Weekend plan", "Buy tea\nCall Marta", 1);
    n["textContentHtml"] = json!(format!(
        "<p dir=\"ltr\" style=\"line-height:1.38;margin-top:0pt;\">{}</p><p dir=\"ltr\">{}</p>",
        span("Buy tea"),
        span("Call Marta")
    ));
    n["labels"] = json!([{"name": "GodLevel"}]);
    put(&mut a, &format!("{k}/Weekend plan.json"), &n);
    a.push((
        format!("{k}/Weekend plan.html"),
        b"<html>copy</html>".to_vec(),
    ));

    // 2: rich text.
    let mut n = note(
        "Rich note",
        "Bold and italic and under\nHead\nSub\nVisit the site & bad",
        2,
    );
    n["textContentHtml"] = json!(
        "<p dir=\"ltr\"><b>Bold</b> and <i>italic</i> and <u>under</u></p><h1>Head</h1><h2>Sub</h2><p>Visit <a href=\"https://example.org/x?a=1&amp;b=2\">the site</a> &amp; <a href=\"javascript:x\">bad</a></p>"
    );
    put(&mut a, &format!("{k}/Rich note.json"), &n);

    // 3: checklist.
    let mut n = note("Shopping", "", 3);
    n.as_object_mut().unwrap().remove("textContent");
    n["listContent"] = json!([
        {"text": "milk", "textHtml": "milk", "isChecked": false},
        {"text": "eggs", "textHtml": "eggs", "isChecked": true},
        {"text": "bread", "textHtml": "bread", "isChecked": false},
    ]);
    n["labels"] = json!([{"name": "health tips"}]);
    put(&mut a, &format!("{k}/Shopping.json"), &n);

    // 4: pinned, CERULEAN.
    let mut n = note("Pinned one", "keep on top", 4);
    n["isPinned"] = json!(true);
    n["color"] = json!("CERULEAN");
    put(&mut a, &format!("{k}/Pinned one.json"), &n);

    // 5: archived, TEAL, the same label twice in different case.
    let mut n = note("Old stuff", "archived text", 5);
    n["isArchived"] = json!(true);
    n["color"] = json!("TEAL");
    n["labels"] = json!([{"name": "Fisica 2"}, {"name": "fisica  2"}]);
    put(&mut a, &format!("{k}/Old stuff.json"), &n);

    // 6: trashed, a colour this importer has never heard of.
    let mut n = note("Trashed", "gone soon", 6);
    n["isTrashed"] = json!(true);
    n["color"] = json!("NEON_GREEN");
    put(&mut a, &format!("{k}/Trashed.json"), &n);

    // 7-10: more palette colours.
    for (i, (name, color)) in [
        ("Red", "RED"),
        ("Yellow", "YELLOW"),
        ("Purple", "PURPLE"),
        ("Gray", "GRAY"),
    ]
    .iter()
    .enumerate()
    {
        let mut n = note(name, &format!("{name} note"), 7 + i as i64);
        n["color"] = json!(color);
        put(&mut a, &format!("{k}/{name}.json"), &n);
    }

    // 11: images: an exact match and a wrong declared type (a PNG that
    // claims to be a JPEG).
    let mut n = note("Photo", "two pictures", 11);
    n["attachments"] = json!([
        {"filePath": "img1.png", "mimetype": "image/png"},
        {"filePath": "pic3.png", "mimetype": "image/jpeg"},
    ]);
    put(&mut a, &format!("{k}/Photo.json"), &n);
    a.push((format!("{k}/img1.png"), png(1)));
    a.push((format!("{k}/pic3.png"), png(3)));

    // 12: jpeg/jpg mismatches both ways.
    let mut n = note("Jpeg mismatch", "extensions disagree", 12);
    n["attachments"] = json!([
        {"filePath": "pic2.jpeg", "mimetype": "image/jpeg"},
        {"filePath": "pic4.jpg", "mimetype": "image/jpeg"},
    ]);
    put(&mut a, &format!("{k}/Jpeg mismatch.json"), &n);
    a.push((format!("{k}/pic2.jpg"), jpeg(2)));
    a.push((format!("{k}/pic4.jpeg"), jpeg(4)));

    // 13: a missing attachment; the note still imports.
    let mut n = note("Missing attachment", "the file is not in the zip", 13);
    n["attachments"] = json!([{"filePath": "gone.png", "mimetype": "image/png"}]);
    put(&mut a, &format!("{k}/Missing attachment.json"), &n);

    // 14: a voice note and an AMR clip the store cannot take.
    let mut n = note("Voice", "", 14);
    n["attachments"] = json!([
        {"filePath": "rec.3gp", "mimetype": "audio/3gpp"},
        {"filePath": "clip.amr", "mimetype": "audio/amr"},
    ]);
    put(&mut a, &format!("{k}/Voice.json"), &n);
    a.push((format!("{k}/rec.3gp"), three_gp()));
    a.push((format!("{k}/clip.amr"), amr()));

    // 15: web links.
    let mut n = note("Link", "read later", 15);
    n["annotations"] = json!([
        {"description": "An example page", "source": "WEBLINK", "title": "Example", "url": "https://example.org/page"},
        {"description": "", "source": "WEBLINK", "title": "", "url": "ftp://example.org/x"},
    ]);
    put(&mut a, &format!("{k}/Link.json"), &n);

    // 16: sharees.
    let mut n = note("Shared", "with a friend", 16);
    n["sharees"] = json!([
        {"email": "me@example.org", "isOwner": true, "type": "USER"},
        {"email": "friend@example.org", "isOwner": false, "type": "USER"},
    ]);
    put(&mut a, &format!("{k}/Shared.json"), &n);

    // 17: untitled.
    put(
        &mut a,
        &format!("{k}/Untitled.json"),
        &note("", "no title here", 17),
    );

    // 18: a 300-char title.
    let long = format!("L{}", "o".repeat(298)) + "g";
    put(
        &mut a,
        &format!("{k}/Long title.json"),
        &note(&long, "short body", 18),
    );

    // corrupt JSON.
    a.push((
        format!("{k}/Broken.json"),
        b"{\"title\": \"oops\", \"textCon".to_vec(),
    ));

    // 19: two notes with the same createdTimestampUsec.
    let mut n = note("Dup A", "first", 19);
    put(&mut a, &format!("{k}/Dup A.json"), &n);
    n["title"] = json!("Dup B");
    n["textContent"] = json!("second");
    put(&mut a, &format!("{k}/Dup B.json"), &n);

    // an empty note.
    put(&mut a, &format!("{k}/Empty.json"), &note("", "", 30));

    // Labels.txt (with a BOM, one label no note uses) and noise.
    a.push((
        format!("{k}/Labels.txt"),
        "\u{feff}GodLevel\nFisica 2\nhealth tips\nUnused label\n\n"
            .as_bytes()
            .to_vec(),
    ));
    a.push((format!("{k}/stray.bin"), vec![1, 2, 3]));
    a.push(("Takeout/Drive/readme.txt".into(), b"not keep".to_vec()));
    a.push(("Takeout/archive_browser.html".into(), b"<html/>".to_vec()));

    // ---- part 2: localized root.
    let mut b: Files = Vec::new();
    let mut n = note("Second part", "lives in the second zip", 20);
    n["attachments"] = json!([{"filePath": "second.png", "mimetype": "image/png"}]);
    put(&mut b, "Mi Takeout/Keep/Second part.json", &n);
    b.push(("Mi Takeout/Keep/second.png".into(), png(20)));
    // A note JSON outside any Keep/ folder is found by its timestamp.
    put(
        &mut b,
        "Mi Takeout/Notas/Orphan.json",
        &note("Orphan", "found by content", 21),
    );
    put(
        &mut b,
        "Mi Takeout/Notas/other.json",
        &json!({"hello": "world"}),
    );
    (a, b)
}

pub fn write_zip(path: &Path, files: &[(String, Vec<u8>)]) {
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let deflate =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let stored =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in files {
        let opts = if name.ends_with(".json") || name.ends_with(".txt") {
            deflate
        } else {
            stored
        };
        z.start_file(name.as_str(), opts).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap();
}

/// Write both parts into `dir`; returns their paths.
pub fn write_takeout(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let (a, b) = parts();
    let p1 = dir.join("takeout-20231114T221320Z-001.zip");
    let p2 = dir.join("takeout-20231114T221320Z-002.zip");
    write_zip(&p1, &a);
    write_zip(&p2, &b);
    vec![p1, p2]
}

/// Edit the JSON of the entry whose name ends with `suffix`.
pub fn edit_json(files: &mut Files, suffix: &str, f: impl FnOnce(&mut Value)) {
    let (_, bytes) = files
        .iter_mut()
        .find(|(n, _)| n.ends_with(suffix))
        .unwrap_or_else(|| panic!("no entry ending {suffix}"));
    let mut v: Value = serde_json::from_slice(bytes).unwrap();
    f(&mut v);
    *bytes = serde_json::to_vec_pretty(&v).unwrap();
}
