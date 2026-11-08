//! The synthetic fixture vault, plus hostile files created at test time.
use ink_obsidian::{
    books::{How, LibraryBook, LibraryIndex},
    config::VaultConfig,
    index::{index, update, IndexState},
    walk::{read_note, Read},
};
use proptest::prelude::*;
use std::{fs, path::Path};

fn book(
    id: &str,
    title: &str,
    author: &str,
    isbn: Option<&str>,
    asin: Option<&str>,
) -> LibraryBook {
    LibraryBook {
        id: id.into(),
        title: title.into(),
        authors: vec![author.into()],
        isbn: isbn.map(Into::into),
        asin: asin.map(Into::into),
    }
}
fn books() -> Vec<LibraryBook> {
    vec![
        book(
            "bk_0000000000000001",
            "The Great Gatsby",
            "F. Scott Fitzgerald",
            None,
            None,
        ),
        book(
            "bk_0000000000000002",
            "Dune",
            "Frank Herbert",
            Some("0441172717"),
            None,
        ),
        book(
            "bk_0000000000000003",
            "Sapiens: A Brief History of Humankind",
            "Yuval Noah Harari",
            None,
            Some("B0TESTASIN"),
        ),
        book("bk_0000000000000004", "Love", "Someone", None, None),
    ]
}
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), to).unwrap();
        }
    }
}
/// A temp copy of the fixture vault and a sibling folder outside it.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault"),
        &vault,
    );
    (dir, vault)
}
fn hostile(dir: &Path, vault: &Path) {
    let outside = dir.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("secret.md"),
        "The Great Gatsby outside the vault",
    )
    .unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.md"), vault.join("Ideas/Escape.md"))
            .unwrap();
        std::os::unix::fs::symlink(&outside, vault.join("Linked")).unwrap();
    }
    // "Grüße The Great Gatsby" in Latin-1.
    let mut latin1 = b"Gr\xfc\xdfe ".to_vec();
    latin1.extend_from_slice(b"The Great Gatsby");
    fs::write(vault.join("Ideas/Latin1.md"), latin1).unwrap();
    let mut huge = String::from("The Great Gatsby\n");
    huge.push_str(&"x".repeat(2 * 1024 * 1024));
    fs::write(vault.join("Ideas/Huge.md"), huge).unwrap();
}
fn summary(refs: &[ink_obsidian::books::BookRef]) -> Vec<(String, String, How, u32)> {
    refs.iter()
        .map(|r| {
            (
                r.book_id[18..].to_owned(),
                r.note_path.clone(),
                r.how,
                r.line,
            )
        })
        .collect()
}

#[test]
fn fixture_vault_refs_links_and_privacy() {
    let (dir, vault) = fixture();
    hostile(dir.path(), &vault);
    let r = index(&VaultConfig::new(&vault), &LibraryIndex::new(&books())).unwrap();
    let c = &r.counts;
    assert_eq!(c.notes, 7, "{:?}", r.warnings);
    assert_eq!(c.attachments, 1);
    assert_eq!((c.excluded_dirs, c.excluded_files), (3, 1));
    assert_eq!(
        c.skipped,
        if cfg!(unix) { 4 } else { 2 },
        "{:?}",
        r.warnings
    );
    assert_eq!(
        (c.links, c.wikilinks, c.embeds, c.markdown_links),
        (9, 7, 1, 1)
    );
    assert_eq!((c.resolved, c.unresolved, c.ambiguous), (6, 2, 1));
    assert_eq!(c.tags, 6);
    assert_eq!(c.book_notes, 1);
    let w = r.warnings.join("\n");
    assert!(w.contains("non-UTF-8 note skipped: Ideas/Latin1.md"), "{w}");
    assert!(
        w.contains("larger than 2097152 bytes skipped: Ideas/Huge.md"),
        "{w}"
    );
    #[cfg(unix)]
    {
        assert!(w.contains("symlink not followed: Ideas/Escape.md"), "{w}");
        assert!(w.contains("symlink not followed: Linked"), "{w}");
    }
    assert_eq!(
        summary(&r.refs),
        vec![
            ("1".into(), "Ideas/Aliases.md".into(), How::Wikilink, 7),
            (
                "1".into(),
                "Reading/Great Gatsby.md".into(),
                How::Frontmatter,
                2
            ),
            (
                "1".into(),
                "Reading/Mention only.md".into(),
                How::Mention,
                3
            ),
            ("2".into(), "Ideas/Aliases.md".into(), How::Wikilink, 7),
            (
                "2".into(),
                "Ideas/Frontmatter book.md".into(),
                How::Frontmatter,
                3
            ),
            ("2".into(), "Ideas/Identifiers.md".into(), How::Isbn, 1),
            (
                "3".into(),
                "Ideas/Frontmatter book.md".into(),
                How::Frontmatter,
                2
            ),
            ("3".into(), "Ideas/Identifiers.md".into(), How::Asin, 1),
        ]
    );
    assert_eq!(c.refs, 8);
    assert_eq!(c.books_referenced, 3);
    assert_eq!(c.refs_by_how["frontmatter"], 3);
    let mention = r.refs.iter().find(|r| r.how == How::Mention).unwrap();
    assert_eq!(mention.note_title, "Mention only");
    assert_eq!(
        mention.excerpt,
        "Someone said the great gatsby has a famous ending."
    );
    assert!(r.refs.iter().all(|r| r.excerpt.chars().count() <= 240));
    let gatsby = r
        .refs
        .iter()
        .find(|r| r.how == How::Frontmatter && r.book_id.ends_with('1'))
        .unwrap();
    assert_eq!(gatsby.note_title, "The Great Gatsby");
    assert_eq!(gatsby.excerpt, "title: The Great Gatsby");
}

#[test]
fn reads_refuse_traversal_and_symlinks() {
    let (dir, vault) = fixture();
    hostile(dir.path(), &vault);
    let root = fs::canonicalize(&vault).unwrap();
    for rel in [
        "../outside/secret.md",
        "Ideas/../../outside/secret.md",
        "./Ideas/Identifiers.md",
        "Ideas//Identifiers.md",
    ] {
        assert!(
            matches!(read_note(&root, rel, 1 << 20).unwrap(), Read::Skipped(_)),
            "{rel}"
        );
    }
    #[cfg(unix)]
    for rel in ["Ideas/Escape.md", "Linked/secret.md"] {
        assert!(
            matches!(read_note(&root, rel, 1 << 20).unwrap(), Read::Skipped(_)),
            "{rel}"
        );
    }
    assert!(matches!(
        read_note(&root, "Ideas/Identifiers.md", 1 << 20).unwrap(),
        Read::Text(_)
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn arbitrary_relative_paths_never_read_outside(parts in proptest::collection::vec("(\\.\\.|\\.|outside|secret\\.md|Ideas|Identifiers\\.md|)", 1..6)) {
        let (dir, vault) = fixture();
        fs::create_dir_all(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("outside/secret.md"), "outside").unwrap();
        let root = fs::canonicalize(&vault).unwrap();
        let rel = parts.join("/");
        if let Ok(Read::Text(t)) = read_note(&root, &rel, 1 << 20) {
            prop_assert_ne!(t, "outside");
            prop_assert!(fs::canonicalize(root.join(&rel)).unwrap().starts_with(&root));
        }
    }
}

#[test]
fn privacy_override_and_include() {
    let (_dir, vault) = fixture();
    let lib = LibraryIndex::new(&books());
    let mut cfg = VaultConfig::new(&vault);
    cfg.exclude.clear();
    let r = index(&cfg, &lib).unwrap();
    assert!(r.refs.iter().any(|r| r.note_path == "Private/Secret.md"));
    assert!(r
        .refs
        .iter()
        .any(|r| r.note_path == "Templates/Book template.md"));
    let mut cfg = VaultConfig::new(&vault);
    cfg.include = vec!["Reading/".into()];
    let r = index(&cfg, &lib).unwrap();
    assert_eq!(r.counts.notes, 2);
    assert!(r.refs.iter().all(|r| r.note_path.starts_with("Reading/")));
}

#[test]
fn incremental_reindex_reads_only_changes() {
    let (_dir, vault) = fixture();
    let cfg = VaultConfig::new(&vault);
    let lib = LibraryIndex::new(&books());
    let mut state = IndexState::default();
    let first = update(&mut state, &cfg, &lib).unwrap();
    assert_eq!((first.counts.added, first.counts.reparsed), (7, 7));
    let again = update(&mut state, &cfg, &lib).unwrap();
    assert_eq!((again.counts.unchanged, again.counts.reparsed), (7, 0));
    assert_eq!(again.refs, first.refs);
    // The state survives a JSON round trip.
    let mut state: IndexState =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    let p = vault.join("Ideas/Identifiers.md");
    let text = fs::read_to_string(&p).unwrap();
    fs::write(&p, format!("{text}Another word on The Great Gatsby.\n")).unwrap();
    fs::write(vault.join("Ideas/New.md"), "[[Dune]]\n").unwrap();
    fs::remove_file(vault.join("Projects/B/Index.md")).unwrap();
    let a = vault.join("Projects/A/Index.md");
    fs::write(&a, fs::read(&a).unwrap()).unwrap();
    let r = update(&mut state, &cfg, &lib).unwrap();
    assert_eq!(r.changed, vec!["Ideas/Identifiers.md"]);
    assert_eq!(r.added, vec!["Ideas/New.md"]);
    assert_eq!(r.removed, vec!["Projects/B/Index.md"]);
    assert_eq!(r.counts.reparsed, 2);
    assert_eq!(r.counts.unchanged, 5);
    // `[[Index]]` is now unique: resolution is recomputed without reparsing.
    assert_eq!(r.counts.ambiguous, 0);
    assert!(summary(&r.refs).contains(&(
        "1".into(),
        "Ideas/Identifiers.md".into(),
        How::Mention,
        3
    )));
    assert!(summary(&r.refs).contains(&("2".into(), "Ideas/New.md".into(), How::Wikilink, 1)));

    let mut more = books();
    more.push(book(
        "bk_0000000000000005",
        "Famous Ending",
        "X",
        None,
        None,
    ));
    let r = update(&mut state, &cfg, &LibraryIndex::new(&more)).unwrap();
    assert_eq!(r.counts.reparsed, 7);
    assert!(r.changed.is_empty() && r.added.is_empty());
    assert!(r
        .refs
        .iter()
        .any(|r| r.book_id.ends_with('5') && r.how == How::Mention));
}
