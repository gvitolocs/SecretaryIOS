//! Google Takeout parser on a synthetic export (zips built in a temp dir).

#[path = "common/keep_fixture.rs"]
mod keep_fixture;

use keep_fixture::*;
use secretary_server::keep_import::{decide_mime, ParsedNote, Scan, Takeout};

fn scan_fixture() -> (tempfile::TempDir, Takeout, Scan) {
    let dir = tempfile::tempdir().unwrap();
    let paths = write_takeout(dir.path());
    let mut t = Takeout::open(&paths).unwrap();
    let scan = t.scan();
    (dir, t, scan)
}

fn by_title<'a>(s: &'a Scan, title: &str) -> &'a ParsedNote {
    s.notes
        .iter()
        .find(|n| n.title == title)
        .unwrap_or_else(|| panic!("no note titled {title:?}"))
}

#[test]
fn counts_files_and_isolates_bad_ones() {
    let (_d, _t, s) = scan_fixture();
    assert_eq!(s.files.json, 24, "22 notes + corrupt + empty");
    assert_eq!(s.notes.len(), 22);
    assert_eq!(s.files.html, 2);
    assert_eq!(s.files.attachments, 7, "gone.png is not in the zips");
    assert_eq!(s.files.ignored, 3, "stray.bin, readme.txt, other.json");
    assert_eq!(s.errors.len(), 2);
    assert!(s
        .errors
        .iter()
        .any(|e| e.file.ends_with("Broken.json") && e.error.contains("invalid JSON")));
    assert!(s
        .errors
        .iter()
        .any(|e| e.file.ends_with("Empty.json") && e.error.contains("empty note")));
    assert_eq!(
        s.label_names,
        vec!["GodLevel", "Fisica 2", "health tips", "Unused label"],
        "Labels.txt seeds the labels (BOM stripped)"
    );
}

#[test]
fn keys_timestamps_and_collisions() {
    let (_d, _t, s) = scan_fixture();
    let w = by_title(&s, "Weekend plan");
    assert_eq!(w.key, format!("gkeep:{}", usec(1)));
    assert_eq!(w.edited_usec, usec(1) + 60_000_000);
    assert_eq!(w.created_at, "2023-11-14T22:13:21Z");
    assert_eq!(w.updated_at, "2023-11-14T22:14:21Z");
    let a = by_title(&s, "Dup A");
    let b = by_title(&s, "Dup B");
    assert_eq!(a.key, format!("gkeep:{}", usec(19)));
    assert_eq!(
        b.key,
        format!("gkeep:{}-2", usec(19)),
        "file-name order decides"
    );
    let keys: std::collections::HashSet<_> = s.notes.iter().map(|n| &n.key).collect();
    assert_eq!(keys.len(), s.notes.len());
    // Found in the second zip, under a localized root, and by content
    // outside a Keep/ folder.
    assert!(s
        .notes
        .iter()
        .any(|n| n.file == "Mi Takeout/Keep/Second part.json"));
    assert!(s
        .notes
        .iter()
        .any(|n| n.file == "Mi Takeout/Notas/Orphan.json"));
}

#[test]
fn text_rich_text_and_lists() {
    let (_d, _t, s) = scan_fixture();
    assert_eq!(by_title(&s, "Weekend plan").content, "Buy tea\nCall Marta");
    assert_eq!(
        by_title(&s, "Rich note").content,
        "**Bold** and *italic* and <u>under</u>\n# Head\n## Sub\nVisit [the site](https://example.org/x?a=1&b=2) & bad"
    );
    let l = by_title(&s, "Shopping");
    assert_eq!(l.content, "");
    assert_eq!(
        l.items.as_deref().unwrap(),
        [
            ("milk".to_owned(), false),
            ("eggs".to_owned(), true),
            ("bread".to_owned(), false)
        ]
    );
    assert_eq!(l.kind(), "list");
    assert!(by_title(&s, "Weekend plan").items.is_none());
}

#[test]
fn flags_colours_labels_and_metadata() {
    let (_d, _t, s) = scan_fixture();
    let p = by_title(&s, "Pinned one");
    assert!(p.pinned && !p.archived && !p.trashed);
    assert_eq!(p.color, "cerulean");
    let o = by_title(&s, "Old stuff");
    assert!(o.archived);
    assert_eq!(o.color, "teal");
    assert_eq!(
        o.labels,
        vec!["Fisica 2"],
        "same label in another case collapses"
    );
    let t = by_title(&s, "Trashed");
    assert!(t.trashed);
    assert_eq!(t.color, "default", "unknown colour");
    for (title, c) in [
        ("Red", "red"),
        ("Yellow", "yellow"),
        ("Purple", "purple"),
        ("Gray", "gray"),
    ] {
        assert_eq!(by_title(&s, title).color, c);
    }
    assert_eq!(
        by_title(&s, "Weekend plan").labels,
        vec!["GodLevel"],
        "display case kept"
    );
    assert_eq!(by_title(&s, "Shopping").labels, vec!["health tips"]);

    let link = by_title(&s, "Link");
    assert_eq!(link.annotations.len(), 1, "ftp link dropped");
    assert_eq!(link.annotations[0].url, "https://example.org/page");
    assert_eq!(link.annotations[0].title.as_deref(), Some("Example"));
    assert_eq!(link.annotations[0].source.as_deref(), Some("WEBLINK"));
    assert_eq!(link.kind(), "link");

    assert_eq!(
        by_title(&s, "Shared").collaborators,
        vec!["friend@example.org"]
    );
    let u = s
        .notes
        .iter()
        .find(|n| n.content == "no title here")
        .unwrap();
    assert_eq!(u.title, "");
}

#[test]
fn long_title_is_clipped_and_kept_in_the_body() {
    let (_d, _t, s) = scan_fixture();
    let n = s
        .notes
        .iter()
        .find(|n| n.content.ends_with("short body"))
        .unwrap();
    assert_eq!(n.title.chars().count(), 200);
    assert!(n.title.ends_with('…') && n.title.starts_with("Lo"));
    let (first, rest) = n.content.split_once("\n\n").unwrap();
    assert_eq!(
        first.chars().count(),
        300,
        "the full title is the first line"
    );
    assert!(first.starts_with("Lo") && first.ends_with('g'));
    assert_eq!(rest, "short body");
}

#[test]
fn attachments_resolve_across_extensions_and_zips() {
    let (_d, mut t, s) = scan_fixture();
    let photo = by_title(&s, "Photo");
    assert_eq!(photo.kind(), "image");
    assert_eq!(photo.attachments.len(), 2);
    assert!(photo.attachments.iter().all(|a| a.entry.is_some()));

    // json says .jpeg, the file is .jpg; json says .jpg, the file is .jpeg.
    let j = by_title(&s, "Jpeg mismatch");
    assert_eq!(
        j.attachments[0].entry_name.as_deref(),
        Some("Takeout/Keep/pic2.jpg")
    );
    assert_eq!(
        j.attachments[1].entry_name.as_deref(),
        Some("Takeout/Keep/pic4.jpeg")
    );
    assert_eq!(t.read_attachment(&j.attachments[0]).unwrap(), jpeg(2));

    let m = by_title(&s, "Missing attachment");
    assert_eq!(m.attachments.len(), 1);
    assert!(m.attachments[0].entry.is_none());
    assert!(t.read_attachment(&m.attachments[0]).is_err());

    let second = by_title(&s, "Second part");
    assert_eq!(t.read_attachment(&second.attachments[0]).unwrap(), png(20));
    assert_eq!(by_title(&s, "Voice").kind(), "audio");
}

#[test]
fn mimetype_sniffing_overrides_wrong_declarations_and_refuses_the_rest() {
    // A PNG declared as a JPEG is stored as PNG.
    assert_eq!(decide_mime("image/jpeg", &png(1)).unwrap(), "image/png");
    // Missing declaration, JPEG bytes.
    assert_eq!(decide_mime("", &jpeg(1)).unwrap(), "image/jpeg");
    assert_eq!(decide_mime("image/jpg", &jpeg(1)).unwrap(), "image/jpeg");
    // 3gp bytes with the declared audio family.
    assert_eq!(
        decide_mime("audio/3gpp", &three_gp()).unwrap(),
        "audio/3gpp"
    );
    assert_eq!(decide_mime("", &three_gp()).unwrap(), "audio/3gpp");
    // AMR: neither sniffable nor allowed.
    assert!(decide_mime("audio/amr", &amr())
        .unwrap_err()
        .contains("unsupported"));
    // Generic ISO container declared as video is refused.
    let mut mp4 = vec![
        0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm',
    ];
    mp4.extend_from_slice(&[0; 16]);
    assert!(decide_mime("video/mp4", &mp4).is_err());
    assert_eq!(decide_mime("audio/mp4", &mp4).unwrap(), "audio/mp4");
    // Unrecognisable bytes: trust an allowed declaration, refuse otherwise.
    assert_eq!(
        decide_mime("application/pdf", b"???").unwrap(),
        "application/pdf"
    );
    assert!(decide_mime("text/plain", b"hello").is_err());
    assert_eq!(decide_mime("", b"%PDF-1.7 x").unwrap(), "application/pdf");
}

#[test]
fn unreadable_zip_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.zip");
    std::fs::write(&p, b"this is not a zip").unwrap();
    assert!(Takeout::open(&[p]).is_err());
    assert!(Takeout::open(&[dir.path().join("missing.zip")]).is_err());
}
