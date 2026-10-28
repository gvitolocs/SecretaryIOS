use axum::http::StatusCode;

use super::*;

fn create(store: &NoteStore, title: &str, slug: Option<&str>, content: &str) -> Note {
    store
        .create(
            CreateNote {
                title: title.into(),
                content: content.into(),
                slug: slug.map(str::to_owned),
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap()
}

#[test]
fn slugs_ids_and_tags() {
    assert_eq!(
        slugify("CRAFT-IT4SD — meeting with Marianne & Jonas"),
        "craft-it4sd-meeting-with-marianne-jonas"
    );
    assert_eq!(slugify("  Città già più  "), "citta-gia-piu");
    assert_eq!(slugify("Træning øvelser"), "traening-ovelser");
    assert_eq!(slugify("!!!"), "");
    assert_eq!(slugify("Trash"), "trash-note");
    assert!(slugify(&"a".repeat(100)).len() <= MAX_SLUG_LEN);
    for ok in ["craftit-meeting", "a", "x1-2"] {
        assert!(valid_slug(ok), "{ok}");
    }
    for bad in [
        "",
        "-a",
        "a-",
        "a--b",
        "A",
        "a_b",
        "a b",
        "trash",
        &"a".repeat(65),
    ] {
        assert!(!valid_slug(bad), "{bad:?}");
    }
    let id = new_id().unwrap();
    assert!(valid_id(&id), "{id}");
    assert!(!valid_id("n_xyz") && !valid_id("craftit-meeting"));
    assert_eq!(
        normalize_tags(&["  Work ".into(), "#aarhus".into(), "work".into(), "".into()]).unwrap(),
        ["work", "aarhus"]
    );
    // Widened rules: any characters except control characters and ','.
    assert_eq!(
        normalize_tags(&["a/b".into(), "Fisica  2".into()]).unwrap(),
        ["a/b", "fisica 2"]
    );
    assert!(normalize_tags(&["a,b".into()]).is_err());
    assert!(normalize_tags(&["a\u{7}".into()]).is_err());
    assert!(normalize_tags(&["x".repeat(51)]).is_err());
    assert!(normalize_tags(&["x".repeat(50)]).is_ok());
    assert!(normalize_tags(&(0..51).map(|i| format!("t{i}")).collect::<Vec<_>>()).is_err());
    assert!(normalize_tags(&(0..50).map(|i| format!("t{i}")).collect::<Vec<_>>()).is_ok());
    assert!(valid_source("claude-code") && !valid_source("Claude") && !valid_source(""));
}

#[test]
fn create_find_update_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::new(dir.path());
    let a = create(
        &store,
        "CRAFT meeting",
        Some("craftit-meeting"),
        "Marianne & Jonas",
    );
    assert_eq!(
        (a.slug.as_str(), a.source.as_str()),
        ("craftit-meeting", "owner")
    );
    assert_eq!(store.get("craftit-meeting").unwrap(), a);
    assert_eq!(store.get("CRAFTIT-MEETING").unwrap().id, a.id);
    assert_eq!(store.get(&a.id).unwrap(), a);
    let e = store
        .create(
            CreateNote {
                title: "x".into(),
                slug: Some("craftit-meeting".into()),
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap_err();
    assert_eq!(
        (e.status, e.code.as_str()),
        (StatusCode::CONFLICT, "slug_taken")
    );
    let b = create(&store, "CRAFT meeting", None, "");
    assert_eq!(b.slug, "craft-meeting");
    let c = create(&store, "CRAFT meeting", None, "");
    assert_eq!(c.slug, "craft-meeting-2");

    let n = store
        .append(
            "craftit-meeting",
            AppendNote {
                content: "\npart 2".into(),
                ..AppendNote::default()
            },
        )
        .unwrap();
    assert_eq!(n.content, "Marianne & Jonas\npart 2");
    let n = store
        .append(
            &a.id,
            AppendNote {
                content: "part 3".into(),
                separator: Some("\n\n".into()),
                ..AppendNote::default()
            },
        )
        .unwrap();
    assert_eq!(n.content, "Marianne & Jonas\npart 2\n\npart 3");

    let stale = UpdateNote {
        title: Some("New".into()),
        base_updated_at: Some("2000-01-01T00:00:00Z".into()),
        ..UpdateNote::default()
    };
    assert_eq!(store.update(&a.id, stale).unwrap_err().code, "conflict");
    let u = store
        .update(
            &a.id,
            UpdateNote {
                title: Some("New".into()),
                tags: Some(vec!["Work".into()]),
                pinned: Some(true),
                base_updated_at: Some(n.updated_at.clone()),
                ..UpdateNote::default()
            },
        )
        .unwrap();
    assert_eq!((u.title.as_str(), u.pinned), ("New", true));
    assert_eq!(u.tags, ["work"]);
    assert_eq!(u.content, n.content, "absent fields are kept");
    let e = store
        .update(
            &a.id,
            UpdateNote {
                slug: Some("craft-meeting".into()),
                ..UpdateNote::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, "slug_taken");

    // Pinned first, then newest; search and tag filters.
    let l = store.list(None, None, 10);
    assert_eq!(l[0].id, a.id);
    assert_eq!(l.len(), 3);
    assert_eq!(store.list(Some("JONAS"), None, 10).len(), 1);
    assert_eq!(store.list(Some("craft"), None, 10).len(), 3);
    assert_eq!(store.list(None, Some("work"), 10).len(), 1);
    assert_eq!(store.list(Some("nothing here"), None, 10).len(), 0);
    let hit = &store.list(Some("part 3"), None, 10)[0];
    assert!(hit.excerpt.contains("part 3"), "{}", hit.excerpt);
}

#[test]
fn limits_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::new(dir.path());
    let big = "x".repeat(MAX_CONTENT_BYTES);
    let n = create(&store, "big", None, &big);
    let e = store
        .append(
            &n.id,
            AppendNote {
                content: "y".into(),
                ..AppendNote::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        (e.status, e.code.as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "note_too_large")
    );
    assert_eq!(store.get(&n.id).unwrap().content.len(), MAX_CONTENT_BYTES);
    for bad in [
        CreateNote {
            title: " ".into(),
            ..CreateNote::default()
        },
        CreateNote {
            title: "a\u{7}".into(),
            ..CreateNote::default()
        },
        CreateNote {
            title: "x".repeat(201),
            ..CreateNote::default()
        },
        CreateNote {
            title: "t".into(),
            content: "a\u{0}b".into(),
            ..CreateNote::default()
        },
        CreateNote {
            title: "t".into(),
            slug: Some("Bad Slug".into()),
            ..CreateNote::default()
        },
        CreateNote {
            title: "t".into(),
            source: Some("Chat GPT".into()),
            ..CreateNote::default()
        },
    ] {
        assert_eq!(
            store.create(bad, Caller("web")).unwrap_err().code,
            "bad_request"
        );
    }
    let n = create(&store, "crlf", None, "a\r\nb\rc");
    assert_eq!(n.content, "a\nb\nc");
    let lucina = store
        .create(
            CreateNote {
                title: "from lucina".into(),
                ..CreateNote::default()
            },
            Caller("lucina"),
        )
        .unwrap();
    assert_eq!(lucina.source, "lucina");
    assert_eq!(store.get("nope").unwrap_err().code, "note_not_found");
}

#[test]
fn trash_restore_purge_and_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::new(dir.path());
    let a = create(&store, "Alpha", Some("alpha"), "one");
    let file = dir.path().join("notes").join(format!("{}.json", a.id));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let d = store.delete("alpha").unwrap();
    assert!(d.deleted_at.is_some());
    assert!(!file.exists());
    assert!(store.get("alpha").is_err());
    let t = store.trash();
    assert_eq!(t.len(), 1);
    assert!(t[0].purge_after.is_some());
    // The slug is free again; a restore then gets a suffix.
    let b = create(&store, "Alpha again", Some("alpha"), "two");
    let r = store.restore(&a.id).unwrap();
    assert_eq!((r.slug.as_str(), r.content.as_str()), ("alpha-2", "one"));
    assert!(r.deleted_at.is_none());
    assert!(store.trash().is_empty());
    assert_eq!(store.get("alpha").unwrap().id, b.id);

    // Expired trash is purged.
    store.delete(&b.id).unwrap();
    let tfile = dir
        .path()
        .join("notes/trash")
        .join(format!("{}.json", b.id));
    // An entry that sat in the trash too long (a new file: the cache
    // reloads when the directory changes).
    let mut old: Note = serde_json::from_slice(&std::fs::read(&tfile).unwrap()).unwrap();
    std::fs::remove_file(&tfile).unwrap();
    old.id = "n_00000000000000000001".into();
    old.deleted_at = Some("2020-01-01T00:00:00Z".into());
    let ofile = dir.path().join("notes/trash/n_00000000000000000001.json");
    std::fs::write(&ofile, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(store.trash().is_empty());
    assert!(!ofile.exists());

    // Another process (the CLI) writing a note is picked up.
    let other = NoteStore::new(dir.path());
    let c = create(&other, "From the CLI", None, "");
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(store.get(&c.slug).unwrap().id, c.id);
}

#[test]
fn seeds() {
    let md = "---\nslug: craftit-meeting\ntitle: CRAFT-IT4SD — meeting\ntags: work, aarhus\nsource: chatgpt\npinned: true\n---\n\n# Hello\n\nBody: with colon\n";
    let s = parse_seed(md, true).unwrap();
    assert_eq!(s.slug, "craftit-meeting");
    assert_eq!(s.tags, ["work", "aarhus"]);
    assert_eq!((s.source.as_str(), s.pinned), ("chatgpt", true));
    assert_eq!(s.content, "# Hello\n\nBody: with colon\n");
    assert!(parse_seed("no front matter", true).is_err());
    let j = parse_seed(r#"{"slug":"a","title":"A","content":"x"}"#, false).unwrap();
    assert_eq!(j.source, "owner");

    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::new(dir.path());
    let (n, created) = store.import(s.clone(), false).unwrap().unwrap();
    assert!(created && n.pinned && n.source == "chatgpt");
    assert!(store.import(s.clone(), false).unwrap().is_none(), "kept");
    let mut s2 = s;
    s2.content = "changed".into();
    let (n2, created) = store.import(s2, true).unwrap().unwrap();
    assert!(!created);
    assert_eq!((n2.id, n2.content.as_str()), (n.id, "changed"));
}
