//! Synthetic fixtures only: parsing, read-only scanning, and additive merge invariants.
use ink_library::{clippings, epub, import::*, mobi, model::*, scan::*, search};
use proptest::prelude::*;
use std::{
    fs,
    io::{Cursor, Write},
    path::Path,
};
const CLIPS: &[u8] = include_bytes!("fixtures/My Clippings.txt");
const NOW: &str = "2026-10-24T10:00:00Z";
fn batch() -> ImportBatch {
    ImportBatch {
        source: "kindle-usb".into(),
        device: Some("Paperwhite".into()),
        scanned_at: NOW.into(),
        books: vec![ScannedBook {
            kind: ink_library::model::BookKind::Book,
            path: "documents/Book.azw3".into(),
            size_bytes: 100,
            fingerprint: "a".repeat(64),
            format: Format::Azw3,
            metadata: Metadata {
                title: Some("The Book (A Story)".into()),
                authors: vec!["Last, First".into(), "Other".into()],
                isbn: Some("9781234567890".into()),
                drm: DrmState::Protected,
                ..Metadata::default()
            },
            sidecar_present: true,
            sidecar_extensions: vec!["mbp1".into()],
        }],
        clippings: clippings::parse(CLIPS).unwrap().entries,
    }
}
fn put(b: &mut [u8], at: usize, n: u32) {
    b[at..at + 4].copy_from_slice(&n.to_be_bytes());
}
fn mobi_fixture(enc: u16) -> Vec<u8> {
    let records = [
        (100, "Writer"),
        (101, "Publisher"),
        (104, "9781234567890"),
        (113, "ASIN1"),
        (501, "EBOK"),
        (503, "Header Title"),
    ];
    let exlen = 12 + records.iter().map(|(_, s)| 8 + s.len()).sum::<usize>();
    let rlen = 16 + 116 + exlen;
    let mut b = vec![0; 86 + rlen];
    b[60..68].copy_from_slice(b"BOOKMOBI");
    b[76..78].copy_from_slice(&1u16.to_be_bytes());
    put(&mut b, 78, 86);
    let r = &mut b[86..];
    r[12..14].copy_from_slice(&enc.to_be_bytes());
    r[16..20].copy_from_slice(b"MOBI");
    put(r, 20, 116);
    put(r, 16 + 0x0c, 65001);
    put(r, 16 + 0x70, 0x40);
    put(r, 16 + 0x4c, 9);
    let ex = &mut r[132..];
    ex[..4].copy_from_slice(b"EXTH");
    put(ex, 4, exlen as u32);
    put(ex, 8, records.len() as u32);
    let mut p = 12;
    for (kind, s) in records {
        put(ex, p, kind);
        put(ex, p + 4, (8 + s.len()) as u32);
        ex[p + 8..p + 8 + s.len()].copy_from_slice(s.as_bytes());
        p += 8 + s.len();
    }
    b
}
fn epub_fixture(opf_path: &str, opf: &str) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("META-INF/container.xml", opts).unwrap();
    write!(
        z,
        "<container><rootfiles><rootfile full-path=\"{opf_path}\"/></rootfiles></container>"
    )
    .unwrap();
    z.start_file(opf_path, opts).unwrap();
    z.write_all(opf.as_bytes()).unwrap();
    z.finish().unwrap().into_inner()
}
#[test]
fn english_italian_and_unknown_clippings() {
    let p = clippings::parse(CLIPS).unwrap();
    assert_eq!(p.entries.len(), 7);
    assert!(p.warnings.iter().any(|s| s.contains("unknown metadata")));
    assert!(p.warnings.iter().any(|s| s.contains("unparseable")));
    let e = p
        .entries
        .iter()
        .find(|e| e.title == "The Book (A Story)" && e.highlight.kind == HighlightKind::Highlight)
        .unwrap();
    assert_eq!(e.authors, ["Last, First", "Other"]);
    assert_eq!(e.highlight.note.as_deref(), Some("Remember this."));
    assert_eq!(
        e.highlight.location,
        Some(LocRange {
            start: 170,
            end: 172
        })
    );
    assert_eq!(e.highlight.added_at.as_deref(), Some("2020-10-04T15:12:01"));
    let bookmark = p
        .entries
        .iter()
        .find(|e| e.title.starts_with("The") && e.highlight.kind == HighlightKind::Bookmark)
        .unwrap();
    assert_eq!(
        bookmark.highlight.added_at.as_deref(),
        Some("2016-03-03T22:52:44")
    );
    assert!(p
        .entries
        .iter()
        .any(|e| e.highlight.added_at.is_none() && !e.highlight.added_at_raw.is_empty()));
    let lf = String::from_utf8(CLIPS.to_vec())
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(clippings::parse(lf.as_bytes()).unwrap(), p);
}
#[test]
fn clippings_limits() {
    assert!(clippings::parse(&[0xff]).is_err());
    assert!(clippings::parse(&vec![b' '; clippings::MAX_BYTES + 1]).is_err());
    let many = "Title\nmeta\ntext\n==========\n".repeat(clippings::MAX_ENTRIES + 1);
    assert!(clippings::parse(many.as_bytes()).is_err());
}
#[test]
fn mobi_headers_and_protection() {
    for (enc, drm) in [
        (0, DrmState::None),
        (1, DrmState::Protected),
        (2, DrmState::Protected),
        (9, DrmState::Unknown),
    ] {
        let m = mobi::parse(&mobi_fixture(enc)).unwrap();
        assert_eq!(m.title.as_deref(), Some("Header Title"));
        assert_eq!(m.authors, ["Writer"]);
        assert_eq!(m.publisher.as_deref(), Some("Publisher"));
        assert_eq!(m.asin.as_deref(), Some("ASIN1"));
        assert_eq!(m.cdetype.as_deref(), Some("EBOK"));
        assert_eq!(m.drm, drm);
    }
    let b = mobi_fixture(0);
    for i in 0..b.len() {
        assert!(mobi::parse(&b[..i]).is_err(), "truncation at {i}");
    }
    assert!(mobi::parse(&[0xff; 400]).is_err());
}
#[test]
fn mobi_offsets_checked() {
    let mut b = mobi_fixture(0);
    put(&mut b, 78, u32::MAX);
    assert!(mobi::parse(&b).is_err());
    let mut b = mobi_fixture(0);
    put(&mut b, 86 + 16 + 0x44, u32::MAX);
    put(&mut b, 86 + 16 + 0x48, 30);
    assert!(mobi::parse(&b).is_err());
    let mut b = mobi_fixture(0);
    put(&mut b, 86 + 132 + 16, 0);
    assert!(mobi::parse(&b).is_err());
}
#[test]
fn epub_metadata_and_expansion_limits() {
    let b=epub_fixture("OPS/book.opf","<package xmlns:dc=\"dc\"><metadata><dc:title>Title &amp; More</dc:title><dc:creator>Writer</dc:creator><dc:identifier>urn:isbn:9781234567890</dc:identifier><dc:language>it</dc:language></metadata></package>");
    let m = epub::read(Cursor::new(b)).unwrap();
    assert_eq!(m.title.as_deref(), Some("Title & More"));
    assert_eq!(m.authors, ["Writer"]);
    assert_eq!(m.isbn.as_deref(), Some("9781234567890"));
    assert_eq!(m.language.as_deref(), Some("it"));
    assert!(epub::read(Cursor::new(epub_fixture("../evil.opf", "<metadata/>"))).is_err());
    assert!(epub::read(Cursor::new(epub_fixture(
        "book.opf",
        &"a".repeat(epub::MAX_ENTRY_BYTES + 1)
    )))
    .is_err());
}
fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_symlink() {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            out.extend(snapshot(&p));
        } else {
            out.push((
                p.clone(),
                fs::read(&p).unwrap(),
                fs::metadata(p).unwrap().modified().unwrap(),
            ));
        }
    }
    out.sort_by_key(|e| e.0.clone());
    out
}
#[test]
fn read_only_scan_with_odd_names_and_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("documents");
    fs::create_dir(&docs).unwrap();
    fs::create_dir(docs.join("Book.sdr")).unwrap();
    fs::write(docs.join("Book.sdr/private.yjr"), b"never parse").unwrap();
    fs::write(docs.join("Book.sdr/._junk.fake"), b"ignore").unwrap();
    fs::write(docs.join("Book.azw3"), mobi_fixture(2)).unwrap();
    fs::write(docs.join("My Clippings.txt"), CLIPS).unwrap();
    fs::write(docs.join("._ignored.txt"), b"ignore").unwrap();
    fs::write(docs.join(".DS_Store"), b"ignore").unwrap();
    fs::write(docs.join("奇妙 café (😀).md"), b"# First Heading\ntext").unwrap();
    fs::write(docs.join(format!("{}.kfx", "x".repeat(200))), b"opaque KFX").unwrap();
    fs::create_dir(dir.path().join("system")).unwrap();
    fs::write(dir.path().join("system/version.txt"), b"stock firmware").unwrap();
    fs::create_dir(dir.path().join("koreader")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.txt"), b"private").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), docs.join("outside")).unwrap();
    let before = snapshot(dir.path());
    let scanned = scan_kindle(dir.path(), ScanOptions::default()).unwrap();
    assert_eq!(before, snapshot(dir.path()));
    assert_eq!(scanned.batch.books.len(), 3);
    assert_eq!(scanned.batch.clippings.len(), 7);
    assert_eq!(scanned.version, "stock firmware");
    assert_eq!(scanned.indicators.get("koreader"), Some(&true));
    let b = scanned
        .batch
        .books
        .iter()
        .find(|b| b.format == Format::Azw3)
        .unwrap();
    assert_eq!(b.sidecar_extensions, ["yjr"]);
    assert_eq!(b.metadata.drm, DrmState::Protected);
    assert!(scanned
        .batch
        .books
        .iter()
        .all(|b| !b.path.contains("outside")));
    assert_eq!(
        scanned
            .batch
            .books
            .iter()
            .find(|b| b.format == Format::Kfx)
            .unwrap()
            .metadata
            .drm,
        DrmState::Unknown
    );
}
#[test]
fn depth_limit_refuses_partial_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = dir.path().join("documents");
    fs::create_dir(&p).unwrap();
    for _ in 0..9 {
        p = p.join("deep");
        fs::create_dir(&p).unwrap();
    }
    assert!(scan_kindle(dir.path(), ScanOptions::default()).is_err());
}
#[test]
fn merge_removal_owner_and_ownership() {
    let b = batch();
    let (mut first, o) = merge(&LibraryState::default(), &b, NOW);
    assert_eq!(o.books_added, 3);
    assert_eq!(o.highlights_added, 7);
    let id = first.books.values().find(|b| b.owned).unwrap().id.clone();
    assert_eq!(id.len(), 19);
    assert!(first
        .books
        .values()
        .all(|b| b.status == ReadingStatus::Unknown));
    assert!(first
        .books
        .values()
        .filter(|b| !b.owned)
        .all(|b| b.on_kindle.is_none()));
    let (same, o) = merge(&first, &b, "2026-10-25T00:00:00Z");
    assert_eq!(first, same);
    assert_eq!(o.books_updated, 0);
    assert_eq!(o.books_unchanged, 3);
    assert_eq!(o.highlights_skipped, 7);
    let owner = first.books.get_mut(&id).unwrap();
    owner.status = ReadingStatus::Completed;
    owner.status_source = Some("owner".into());
    owner.tags = vec!["mine".into()];
    let (again, _) = merge(&first, &b, NOW);
    assert_eq!(again.books[&id].status, ReadingStatus::Completed);
    assert_eq!(again.books[&id].tags, ["mine"]);
    let mut empty = b.clone();
    empty.books.clear();
    empty.scanned_at = "2026-10-25T00:00:00Z".into();
    let (removed, o) = merge(&again, &empty, &empty.scanned_at);
    assert_eq!(o.removed_from_kindle, 1);
    assert!(removed.books[&id].on_kindle.is_none());
    assert!(removed.books[&id].removed_from_kindle_at.is_some());
    assert_eq!(removed.highlights, again.highlights);
    assert!(removed.books[&id].owned);
}
#[test]
fn identities_conflicts_and_refs() {
    let b = batch();
    let (first, _) = merge(&LibraryState::default(), &b, NOW);
    let id = first.books.values().find(|b| b.owned).unwrap().id.clone();
    let mut second = b.clone();
    second.books[0].metadata.title = Some("Different title".into());
    second.books[0].metadata.isbn = Some("conflict".into());
    let (s, o) = merge(&first, &second, NOW);
    assert_eq!(s.books.len(), 3);
    assert!(!o.conflicts.is_empty());
    assert_eq!(s.books[&id].isbn.as_deref(), Some("9781234567890"));
    let results = search::search(&first, "cafe", 20);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].book_id, id);
    assert!(results[0].highlight_id.is_some());
    assert_eq!(
        results[0].location,
        Some(LocRange {
            start: 170,
            end: 172
        })
    );
    assert_eq!(search::normalize("The Café!"), "cafe");
    second.books[0].fingerprint = "b".repeat(64);
    second.books[0].metadata.isbn = b.books[0].metadata.isbn.clone();
    assert_eq!(merge(&first, &second, NOW).0.books.len(), 3);
}
#[test]
fn highlight_dedup_independent_of_entry_order() {
    let b = batch();
    let mut reverse = b.clone();
    reverse.clippings.reverse();
    let a = merge(&LibraryState::default(), &b, NOW).0;
    let c = merge(&LibraryState::default(), &reverse, NOW).0;
    assert_eq!(a, c);
}
proptest! {
    #[test]
    fn merge_is_idempotent(texts in prop::collection::vec("[a-zA-Z é]{0,80}",0..30)){
        let mut b=batch();for (i,t) in texts.into_iter().enumerate(){let mut c=b.clippings[0].clone();c.highlight.text=t;c.highlight.location=Some(LocRange{start:i as u32,end:i as u32});b.clippings.push(c);}
        let (a,_)=merge(&LibraryState::default(),&b,NOW);let (again,o)=merge(&a,&b,"2026-10-25T00:00:00Z");prop_assert_eq!(a,again);prop_assert_eq!(o.books_added+o.books_updated+o.highlights_added,0);
    }
    #[test]
    fn arbitrary_mobi_never_panics(bytes in prop::collection::vec(any::<u8>(),0..4096)){let _=mobi::parse(&bytes);}
}

#[test]
fn multiple_notes_and_later_batches_are_idempotent() {
    let mut b = batch();
    let mut note = b
        .clippings
        .iter()
        .find(|c| c.highlight.kind == HighlightKind::Note)
        .unwrap()
        .clone();
    note.highlight.text = "Another note at the same location".into();
    b.clippings.push(note);
    let (a, _) = merge(&LibraryState::default(), &b, NOW);
    assert_eq!(merge(&a, &b, "2026-10-25T00:00:00Z").0, a);
    let mut reversed = b.clone();
    reversed.clippings.reverse();
    assert_eq!(merge(&LibraryState::default(), &reversed, NOW).0, a);
}
#[test]
fn pdf_only_uses_trailer_info() {
    let pdf=b"%PDF-1.4\n1 0 obj << /Title (Safe title) /Author (Writer) >> endobj\ntrailer << /Info 1 0 R >>\n%%EOF";
    let m = ink_library::pdf::parse(pdf);
    assert_eq!(m.title.as_deref(), Some("Safe title"));
    assert_eq!(m.authors, ["Writer"]);
    assert!(
        ink_library::pdf::parse(b"1 0 obj << /Title (Page content) >> endobj /Info 1 0 R")
            .title
            .is_none()
    );
}
#[test]
fn zip_directory_entry_count_is_bounded() {
    let mut bytes = epub_fixture("book.opf", "<metadata/>");
    let pos = bytes.windows(4).rposition(|b| b == b"PK\x05\x06").unwrap();
    bytes[pos + 10..pos + 12].copy_from_slice(&5000u16.to_le_bytes());
    assert!(epub::read(Cursor::new(bytes)).is_err());
}

#[test]
fn conflicting_drm_keeps_existing_value() {
    let mut b = batch();
    b.books[0].metadata.drm = DrmState::None;
    let (first, _) = merge(&LibraryState::default(), &b, NOW);
    b.books[0].metadata.drm = DrmState::Protected;
    let (merged, outcome) = merge(&first, &b, NOW);
    assert_eq!(merged, first);
    assert!(outcome.conflicts.iter().any(|c| c.contains("DRM")));
}
