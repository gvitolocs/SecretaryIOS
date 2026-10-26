//! Synthetic files shaped like Kindle USB observations; no owner book content.
use ink_library::{
    clippings,
    import::{merge, ImportBatch, ScannedBook},
    model::*,
    scan::{filename_metadata, scan_kindle, ScanOptions},
    search::{author_key, title_key},
};
use std::fs;
const NOW: &str = "2026-10-24T00:00:00Z";
fn batch(books: Vec<ScannedBook>, text: &str) -> ImportBatch {
    ImportBatch {
        source: "kindle-usb".into(),
        device: None,
        scanned_at: NOW.into(),
        books,
        clippings: clippings::parse(text.as_bytes()).unwrap().entries,
    }
}
fn file(name: &str, fingerprint: char, authors: &[&str]) -> ScannedBook {
    let mut metadata = filename_metadata(name.strip_suffix(".kfx").unwrap_or(name));
    metadata.authors = authors.iter().map(|s| (*s).into()).collect();
    ScannedBook {
        kind: BookKind::Book,
        path: format!("documents/{name}"),
        size_bytes: 1,
        fingerprint: fingerprint.to_string().repeat(64),
        format: Format::Kfx,
        metadata,
        sidecar_present: false,
        sidecar_extensions: vec![],
    }
}
fn clip(title: &str, author: Option<&str>, text: &str) -> String {
    let heading = author
        .map(|a| format!("{title} ({a})"))
        .unwrap_or_else(|| title.into());
    format!("{heading}\n- Your Highlight at location 170-172 | Added on unknown\n\n{text}\n==========\n")
}
#[test]
fn fallback_title_suffixes_and_colons() {
    for (name, title, asin, pdoc) in [
        (
            "The Great Gatsby_ A Study_B012345678",
            "The Great Gatsby: A Study",
            Some("B012345678"),
            false,
        ),
        (
            "I Malavoglia_123456789X",
            "I Malavoglia",
            Some("123456789X"),
            false,
        ),
        (
            "I Malavoglia_1234567890",
            "I Malavoglia",
            Some("1234567890"),
            false,
        ),
        (
            "The Great Gatsby_ABCDEFGHIJKLMNOPQRSTUVWXYZ234567",
            "The Great Gatsby",
            None,
            true,
        ),
        (
            "The Great Gatsby_ABCDEFGHIJKLMNOPQRSTUVWX12345678",
            "The Great Gatsby_ABCDEFGHIJKLMNOPQRSTUVWX12345678",
            None,
            false,
        ),
        (
            "The Great Gatsby_B0123",
            "The Great Gatsby_B0123",
            None,
            false,
        ),
        (
            "The Great Gatsby_XXXXXXXXXX",
            "The Great Gatsby_XXXXXXXXXX",
            None,
            false,
        ),
    ] {
        let m = filename_metadata(name);
        assert_eq!(m.title.as_deref(), Some(title));
        assert_eq!(m.asin.as_deref(), asin);
        assert_eq!(m.cdetype.as_deref() == Some("PDOC"), pdoc);
    }
}
#[test]
fn zero_width_per_entry_and_double_space_italian() {
    let input="\u{feff}I Malavoglia (Italian Edition)\n-  La tua evidenziazione alla posizione 12-13 | Aggiunto in data domenica 4 ottobre 2020 15:12:01\n\nA synthetic quotation.\n==========\n\u{200b}\u{feff}I Malavoglia (Italian Edition)\n-  La tua evidenziazione alla posizione 12-13 | Aggiunto in data domenica 4 ottobre 2020 15:12:01\n\nA synthetic quotation.\n==========\n\u{2060}I Malavoglia (Italian Edition)\n- La tua nota alla posizione 13 | Aggiunto in data unknown\n\nA synthetic note.\n==========\n";
    let parsed = clippings::parse(input.as_bytes()).unwrap();
    assert_eq!(parsed.entries.len(), 2);
    let h = parsed
        .entries
        .iter()
        .find(|e| e.highlight.kind == HighlightKind::Highlight)
        .unwrap();
    assert_eq!(h.title, "I Malavoglia (Italian Edition)");
    assert!(h.authors.is_empty());
    assert_eq!(h.highlight.location, Some(LocRange { start: 12, end: 13 }));
    assert_eq!(h.highlight.note.as_deref(), Some("A synthetic note."));
    assert!(!parsed
        .warnings
        .iter()
        .any(|w| w.contains("unknown metadata")));
    let (s, _) = merge(
        &LibraryState::default(),
        &batch(vec![file("I Malavoglia_B00685RMT8.kfx", 'a', &[])], input),
        NOW,
    );
    assert_eq!(s.books.len(), 1);
}
#[test]
fn edition_and_subtitle_keys_match_without_changing_display_titles() {
    // Non-public-domain author-library observations are represented by invented names.
    for (clipping, filename, author) in [
        (
            "I Malavoglia (Italian Edition)",
            "I Malavoglia_B00685RMT8.kfx",
            "Verga, Giovanni",
        ),
        (
            "The Great Gatsby",
            "The Great Gatsby (English Edition)_B00EJRPZEQ.kfx",
            "Fitzgerald, F. Scott",
        ),
        (
            "The Odyssey (Super ET) (Italian Edition)",
            "The Odyssey (Super ET)_B012345678.kfx",
            "Homer",
        ),
        (
            "I Malavoglia (Italian Edition)",
            "I Malavoglia_ A synthetic subtitle_B012345678.kfx",
            "Verga, Giovanni",
        ),
    ] {
        let f = file(filename, 'a', &[]);
        let display = f.metadata.title.clone().unwrap();
        let b = batch(
            vec![f],
            &clip(clipping, Some(author), "Synthetic quotation."),
        );
        let (s, _) = merge(&LibraryState::default(), &b, NOW);
        assert_eq!(s.books.len(), 1, "{clipping} -> {filename}");
        let book = s.books.values().next().unwrap();
        assert!(book.owned);
        assert_eq!(book.title, display);
        assert_eq!(book.authors, [author], "authors come from the clipping");
        assert_eq!(s.highlights[&book.id].len(), 1);
        assert_eq!(merge(&s, &b, NOW).0, s);
    }
    for tag in [
        "Italian Edition",
        "English Edition",
        "Danish Edition",
        "German Edition",
        "French Edition",
        "Edizione italiana",
        "Super ET",
        "Classici",
        "eNewton Classici",
    ] {
        assert_eq!(title_key(&format!("The Odyssey ({tag})")), "odyssey");
    }
    assert_eq!(title_key("L’Ódissea: A Study"), "odissea");
    assert_eq!(title_key("L'Odissea_ A Study"), "odissea");
    assert_eq!(title_key("The Odyssey - A Study"), "odyssey");
    assert_eq!(author_key(&["Hómer, First".into()]), "homer first");
}
#[test]
fn sentence_periods_do_not_match_different_titles() {
    let b = batch(
        vec![file(
            "Synthetic Manual. The First Funnel Method_B012345678.kfx",
            'a',
            &[],
        )],
        &clip(
            "Synthetic Manual. The New Direct Method",
            None,
            "A quotation.",
        ),
    );
    let (s, _) = merge(&LibraryState::default(), &b, NOW);
    assert_eq!(s.books.len(), 2);
    assert_eq!(s.books.values().filter(|b| !b.owned).count(), 1);
}
#[test]
fn distinct_fingerprints_with_colliding_title_keys_stay_separate() {
    let a = file("The Odyssey_ One_B012345678.kfx", 'a', &["Homer"]);
    let b = file("The Odyssey_ Two_B087654321.kfx", 'b', &["Another Writer"]);
    let clips = clip(
        "The Odyssey (English Edition)",
        Some("Homer"),
        "Author-specific.",
    );
    let batch = batch(vec![a, b], &clips);
    let (state, outcome) = merge(&LibraryState::default(), &batch, NOW);
    assert_eq!(state.books.len(), 2);
    assert_eq!(outcome.conflicts.len(), 2);
    let matching = state
        .books
        .values()
        .find(|b| b.authors == ["Homer"])
        .unwrap();
    assert_eq!(state.highlights[&matching.id][0].text, "Author-specific.");
    let (again, outcome) = merge(&state, &batch, NOW);
    assert_eq!(again, state);
    assert_eq!(outcome.books_unchanged, 2);
    assert_eq!(outcome.highlights_skipped, 1);
    let unknown = batch_with_unknown_author(batch.clone());
    let (ambiguous, _) = merge(&LibraryState::default(), &unknown, NOW);
    assert_eq!(ambiguous.books.len(), 3);
    assert_eq!(ambiguous.books.values().filter(|b| !b.owned).count(), 1);
    let mut reverse = batch;
    reverse.books.reverse();
    assert_eq!(merge(&LibraryState::default(), &reverse, NOW).0, state);
}
fn batch_with_unknown_author(mut b: ImportBatch) -> ImportBatch {
    b.clippings = clippings::parse(clip("The Odyssey", None, "Ambiguous.").as_bytes())
        .unwrap()
        .entries;
    b
}
#[test]
fn identical_full_titles_with_different_files_are_not_deduplicated_by_name() {
    let a = file("The Odyssey.kfx", 'a', &["Homer"]);
    let mut b = a.clone();
    b.path = "documents/another.kfx".into();
    b.fingerprint = "b".repeat(64);
    let batch = batch(
        vec![a, b],
        &clip("The Odyssey", Some("Homer"), "Ambiguous."),
    );
    let (s, o) = merge(&LibraryState::default(), &batch, NOW);
    assert_eq!(s.books.len(), 3);
    assert!(!o.conflicts.is_empty());
    assert_eq!(s.books.values().filter(|b| b.owned).count(), 2);
}
#[test]
fn clipping_only_book_can_later_gain_a_file_and_keeps_its_id() {
    let clips = clip(
        "I Malavoglia (Italian Edition)",
        Some("Verga, Giovanni"),
        "A quotation.",
    );
    let (first, _) = merge(&LibraryState::default(), &batch(vec![], &clips), NOW);
    let id = first.books.keys().next().unwrap();
    let (second, _) = merge(
        &first,
        &batch(
            vec![file("I Malavoglia_ A subtitle_B00685RMT8.kfx", 'a', &[])],
            &clips,
        ),
        NOW,
    );
    assert_eq!(second.books.len(), 1);
    assert!(second.books[id].owned);
}
#[test]
fn canonical_highlight_key_and_id_are_pinned() {
    let entries = clippings::parse(
        clip(
            "The Great Gatsby (English Edition)",
            Some("Fitzgerald, F. Scott"),
            "A   thought.\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let h = &entries.entries[0].highlight;
    assert_eq!(
        h.dedup_key,
        "great gatsby|fitzgerald f scott|highlight|170-172|A thought."
    );
    assert_eq!(h.id, "hl_8c6f78e9e5e5c21e");
    let mut unknown = h.clone();
    unknown.location = None;
    unknown.kind = HighlightKind::Bookmark;
    assert_eq!(
        clippings::dedup_key(
            "The Great Gatsby",
            &["Fitzgerald, F. Scott".into()],
            &unknown
        ),
        "great gatsby|fitzgerald f scott|bookmark||A thought."
    );
}
fn put(b: &mut [u8], offset: usize, value: u32) {
    b[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
fn mobi(encoding: u32, full_name: &[u8], exth: &[(u32, &[u8])]) -> Vec<u8> {
    let ex_len = 12 + exth.iter().map(|(_, v)| 8 + v.len()).sum::<usize>();
    let title_offset = 132 + ex_len;
    let mut b = vec![0; 86 + title_offset + full_name.len()];
    b[60..68].copy_from_slice(b"BOOKMOBI");
    b[76..78].copy_from_slice(&1u16.to_be_bytes());
    put(&mut b, 78, 86);
    let r = &mut b[86..];
    r[16..20].copy_from_slice(b"MOBI");
    put(r, 20, 116);
    put(r, 16 + 0x0c, encoding);
    put(r, 16 + 0x44, title_offset as u32);
    put(r, 16 + 0x48, full_name.len() as u32);
    put(r, 16 + 0x70, 0x40);
    let ex = &mut r[132..];
    ex[..4].copy_from_slice(b"EXTH");
    put(ex, 4, ex_len as u32);
    put(ex, 8, exth.len() as u32);
    let mut pos = 12;
    for (kind, value) in exth {
        put(ex, pos, *kind);
        put(ex, pos + 4, (8 + value.len()) as u32);
        ex[pos + 8..pos + 8 + value.len()].copy_from_slice(value);
        pos += 8 + value.len();
    }
    r[title_offset..].copy_from_slice(full_name);
    b
}
#[test]
fn mobi_cp1252_and_utf8_decode_full_name_and_exth() {
    for encoding in [1252, 65001] {
        let name = if encoding == 1252 {
            b"W\xf6rterbuch \x80".to_vec()
        } else {
            "Wörterbuch €".as_bytes().to_vec()
        };
        let author = if encoding == 1252 {
            b"M\xfcller".to_vec()
        } else {
            "Müller".as_bytes().to_vec()
        };
        let full = mobi(encoding, &name, &[(100, &author)]);
        let m = ink_library::mobi::parse(&full).unwrap();
        assert_eq!(m.title.as_deref(), Some("Wörterbuch €"));
        assert_eq!(m.authors, ["Müller"]);
        let exth = mobi(
            encoding,
            b"Fallback",
            &[(503, &name), (100, &author), (101, &name)],
        );
        let m = ink_library::mobi::parse(&exth).unwrap();
        assert_eq!(m.title.as_deref(), Some("Wörterbuch €"));
        assert_eq!(m.publisher.as_deref(), Some("Wörterbuch €"));
        assert!(!m.title.unwrap().contains('\u{fffd}'));
    }
    assert!(ink_library::mobi::parse(&mobi(99999, b"Name", &[])).is_err());
}
#[test]
fn mobi_dictionary_language_presence_sets_dictionary_flag() {
    for kind in [531, 532] {
        let m =
            ink_library::mobi::parse(&mobi(65001, b"Dictionary", &[(kind, &9u32.to_be_bytes())]))
                .unwrap();
        assert!(m.dictionary);
    }
}
#[test]
fn scan_topaz_kinds_markers_and_all_macos_litter_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("documents");
    fs::create_dir_all(docs.join("dictionaries")).unwrap();
    fs::write(docs.join("The Odyssey_B012345678.azw1"), b"opaque Topaz").unwrap();
    fs::write(
        docs.join("I Malavoglia_ A Study_B087654321.tpz"),
        b"opaque Topaz",
    )
    .unwrap();
    fs::write(
        docs.join("Glossary.azw"),
        mobi(65001, b"Synthetic glossary", &[(531, &9u32.to_be_bytes())]),
    )
    .unwrap();
    fs::write(
        docs.join("dictionaries/dict-test.mobi"),
        mobi(65001, b"Synthetic dictionary", &[]),
    )
    .unwrap();
    fs::write(
        docs.join("The Great Gatsby_ABCDEFGHIJKLMNOPQRSTUVWXYZ234567.kfx"),
        b"opaque KFX",
    )
    .unwrap();
    fs::write(
        docs.join("Header_B012345678.mobi"),
        mobi(65001, b"Header title", &[]),
    )
    .unwrap();
    for marker in [
        "winterbreak.log",
        "jb.sh",
        "patchedUks.sqsh",
        "developer.keystore",
        "documents/KUAL.kual",
        "extensions/KUAL.cfg",
    ] {
        let p = dir.path().join(marker);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"names only").unwrap();
    }
    for marker in [
        "mkk",
        "extensions/MRInstaller",
        "extensions/renameotabin",
        "koreader",
        "libkh",
    ] {
        fs::create_dir_all(dir.path().join(marker)).unwrap();
    }
    for name in ["._book.kfx", ".DS_Store"] {
        fs::write(docs.join(name), b"ignore").unwrap();
    }
    for folder in [".Spotlight-V100", ".fseventsd", ".Trashes"] {
        fs::create_dir_all(docs.join(folder)).unwrap();
        fs::write(docs.join(folder).join("ignore.kfx"), b"ignore").unwrap();
    }
    fs::create_dir_all(docs.join("The Odyssey_B012345678.sdr/.Trashes")).unwrap();
    fs::write(
        docs.join("The Odyssey_B012345678.sdr/.Trashes/private.yjr"),
        b"ignore",
    )
    .unwrap();
    fs::write(
        docs.join("The Odyssey_B012345678.sdr/._ignore.yjf"),
        b"ignore",
    )
    .unwrap();
    let before = snapshot(dir.path());
    let scan = scan_kindle(dir.path(), ScanOptions::default()).unwrap();
    assert_eq!(before, snapshot(dir.path()));
    assert_eq!(scan.batch.books.len(), 6);
    assert_eq!(
        scan.batch
            .books
            .iter()
            .filter(|b| b.kind == BookKind::Dictionary)
            .count(),
        2
    );
    assert_eq!(
        scan.batch
            .books
            .iter()
            .filter(|b| b.kind == BookKind::Document)
            .count(),
        1
    );
    let topaz = scan
        .batch
        .books
        .iter()
        .find(|b| b.format == Format::Azw1)
        .unwrap();
    assert_eq!(topaz.metadata.title.as_deref(), Some("The Odyssey"));
    assert_eq!(topaz.metadata.drm, DrmState::Unknown);
    assert!(topaz.sidecar_extensions.is_empty());
    let header = scan
        .batch
        .books
        .iter()
        .find(|b| b.path.contains("Header_"))
        .unwrap();
    assert_eq!(header.metadata.title.as_deref(), Some("Header title"));
    assert_eq!(header.metadata.asin.as_deref(), Some("B012345678"));
    for key in ["jailbreak", "kual", "mrpi", "otaBlocker", "koreader"] {
        assert_eq!(scan.indicators.get(key), Some(&true));
    }
    for marker in [
        "winterbreak.log",
        "jb.sh",
        "patchedUks.sqsh",
        "developer.keystore",
        "mkk",
        "documents/KUAL.kual",
        "extensions/KUAL.cfg",
        "extensions/MRInstaller",
        "extensions/renameotabin",
        "koreader",
        "libkh",
    ] {
        assert!(scan.markers.iter().any(|m| m == marker), "missing {marker}");
    }
    let (state, _) = merge(&LibraryState::default(), &scan.batch, NOW);
    assert_eq!(
        state
            .books
            .values()
            .filter(|b| b.kind == BookKind::Dictionary)
            .count(),
        2
    );
}
fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = vec![];
    for entry in fs::read_dir(root).unwrap() {
        let e = entry.unwrap();
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
                fs::metadata(&p).unwrap().modified().unwrap(),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(unix)]
#[test]
fn marker_checks_do_not_follow_directory_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("documents")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("KUAL.cfg"), b"private marker").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("extensions")).unwrap();
    let scan = scan_kindle(dir.path(), ScanOptions::default()).unwrap();
    assert_eq!(scan.indicators.get("kual"), Some(&false));
    assert_eq!(scan.indicators.get("jailbreak"), Some(&false));
    assert!(scan.markers.is_empty());
}

#[test]
fn mobi_orthographic_and_inflection_indexes_mark_old_dictionaries() {
    // Values seen in a Duden dictionary on the owner's Kindle (no EXTH 531/532).
    let mut dict = mobi(1252, b"Duden", &[]);
    put(&mut dict[86..], 0x28, 0x3d0a);
    put(&mut dict[86..], 0x2C, 0x3d6c);
    assert!(ink_library::mobi::parse(&dict).unwrap().dictionary);
    // A KF8 book can carry the first index alone; absent is 0xFFFFFFFF (or 0).
    let mut book = mobi(1252, b"Book", &[]);
    put(&mut book[86..], 0x28, 0xfb);
    put(&mut book[86..], 0x2C, u32::MAX);
    assert!(!ink_library::mobi::parse(&book).unwrap().dictionary);
    assert!(
        !ink_library::mobi::parse(&mobi(1252, b"Plain", &[]))
            .unwrap()
            .dictionary
    );
}

#[test]
fn highlights_sort_in_reading_order() {
    let entry = |loc: &str, page: &str, text: &str| {
        format!(
            "Book (Writer)\n- Your Highlight on page {page} | Location {loc} | Added on Sunday, 4 October 2020 15:12:01\n\n{text}\n==========\n"
        )
    };
    let input = [
        entry("920-922", "93", "c"),
        entry("506-508", "53", "a"),
        entry("561-563", "58", "b"),
        "Book (Writer)\n- Your Bookmark | Added on Sunday, 4 October 2020 15:12:01\n\nz\n==========\n".to_string(),
    ]
    .concat();
    let mut hs: Vec<_> = clippings::parse(input.as_bytes())
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.highlight)
        .collect();
    ink_library::model::sort_reading_order(&mut hs);
    let order: Vec<_> = hs.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(order[..3], ["a", "b", "c"]);
    assert_eq!(hs.len(), 4, "an entry without a position is kept, last");
}

#[test]
fn rescan_that_only_refreshes_last_seen_is_unchanged() {
    let f = file("I Malavoglia_B00685RMT8.kfx", 'a', &[]);
    let first = batch(
        vec![f.clone()],
        &clip("I Malavoglia", Some("Verga, Giovanni"), "Quote."),
    );
    let (s1, o1) = merge(&LibraryState::default(), &first, NOW);
    assert_eq!(o1.books_added, 1);
    let mut later = first.clone();
    later.scanned_at = "2026-10-26T09:00:00Z".into();
    let (s2, o2) = merge(&s1, &later, "2026-10-26T09:00:01Z");
    assert_eq!(
        (o2.books_updated, o2.books_unchanged),
        (0, 1),
        "only lastSeenAt moved"
    );
    let (b1, b2) = (
        s1.books.values().next().unwrap(),
        s2.books.values().next().unwrap(),
    );
    assert_eq!(b2.updated_at, b1.updated_at, "updatedAt is not bumped");
    assert_eq!(
        b2.on_kindle.as_ref().unwrap().last_seen_at,
        "2026-10-26T09:00:00Z",
        "the new time is kept"
    );
    // A real change on a later scan still counts.
    let mut moved = later.clone();
    moved.scanned_at = "2026-10-27T09:00:00Z".into();
    moved.books[0].size_bytes += 1;
    let (s3, o3) = merge(&s2, &moved, "2026-10-27T09:00:01Z");
    assert_eq!(o3.books_updated, 1);
    assert_ne!(s3.books.values().next().unwrap().updated_at, b1.updated_at);
}
