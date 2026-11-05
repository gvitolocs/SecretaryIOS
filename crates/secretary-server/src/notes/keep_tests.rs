//! Unit tests of the Keep additions: legacy files, checklists, labels,
//! `changes`, attachments, batch and the import store API.

use std::os::unix::fs::PermissionsExt;

use super::*;

fn store() -> (tempfile::TempDir, NoteStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = NoteStore::new(dir.path());
    (dir, s)
}

fn mk(s: &NoteStore, title: &str, content: &str) -> Note {
    s.create(
        CreateNote {
            title: title.into(),
            content: content.into(),
            ..CreateNote::default()
        },
        Caller("web"),
    )
    .unwrap()
}

fn mk_list(s: &NoteStore, title: &str, items: &[(&str, bool)]) -> Note {
    s.create(
        CreateNote {
            title: title.into(),
            items: Some(
                items
                    .iter()
                    .map(|(t, c)| ItemReq {
                        text: (*t).into(),
                        checked: *c,
                        ..ItemReq::default()
                    })
                    .collect(),
            ),
            ..CreateNote::default()
        },
        Caller("web"),
    )
    .unwrap()
}

fn patch(s: &NoteStore, key: &str, u: UpdateNote) -> Result<Note, crate::error::ApiError> {
    s.update(key, u)
}

fn png(n: u8) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.push(n);
    v
}

fn imported(key: &str, edited: i64, title: &str) -> ImportedNote {
    ImportedNote {
        key: key.into(),
        edited_usec: edited,
        title: title.into(),
        content: "body".into(),
        color: "teal".into(),
        created_at: "2024-03-01T10:00:00.000Z".into(),
        updated_at: "2024-03-02T10:00:00.000Z".into(),
        ..ImportedNote::default()
    }
}

#[test]
fn a_note_file_without_the_new_fields_loads() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(notes.join("trash")).unwrap();
    let legacy = |id: &str, t: &str| {
        format!(
            r#"{{"id":"{id}","slug":"old-{t}","title":"Old {t}","content":"text","tags":["work"],"source":"owner","pinned":true,"createdAt":"2025-01-01T00:00:00.000Z","updatedAt":"2025-0{t}-01T00:00:00.000Z"}}"#
        )
    };
    std::fs::write(
        notes.join("n_00000000000000000001.json"),
        legacy("n_00000000000000000001", "1"),
    )
    .unwrap();
    std::fs::write(
        notes.join("n_00000000000000000002.json"),
        legacy("n_00000000000000000002", "2"),
    )
    .unwrap();
    // Trashed a day ago: a fixed date here expired after TRASH_DAYS and the purge took the note.
    let deleted_at = model::rfc3339(time::OffsetDateTime::now_utc() - time::Duration::days(1));
    let trashed = legacy("n_00000000000000000003", "3")
        .replace("}", &format!(r#","deletedAt":"{deleted_at}"}}"#));
    std::fs::write(notes.join("trash/n_00000000000000000003.json"), trashed).unwrap();

    let s = NoteStore::new(dir.path());
    let n = s.get("old-1").unwrap();
    assert_eq!(n.color, "default");
    assert!(!n.archived && n.items.is_none() && n.reminder.is_none() && n.import.is_none());
    assert!(n.attachments.is_empty() && n.annotations.is_empty() && n.collaborators.is_empty());
    assert_eq!(
        n.position, 1_735_689_600_000.0,
        "position defaults to createdAt in ms"
    );
    assert!(n.pinned);
    // Revisions are handed out oldest-updated first, trash included, and
    // written back to the files.
    let c = s.changes(0, 10).unwrap();
    assert_eq!(c.notes.len(), 3);
    let revs: Vec<u64> = c.notes.iter().map(|n| n.rev).collect();
    assert_eq!(revs, [1, 2, 3]);
    assert_eq!(c.notes[2].id, "n_00000000000000000003");
    assert!(c.notes[2].deleted_at.is_some());
    assert_eq!(c.cursor, "3");
    let on_disk: Note =
        serde_json::from_slice(&std::fs::read(notes.join("n_00000000000000000001.json")).unwrap())
            .unwrap();
    assert_eq!(on_disk.rev, 1);
    // A new write continues the counter, also after a restart.
    let m = mk(&s, "new", "x");
    assert_eq!(m.rev, 4);
    let s2 = NoteStore::new(dir.path());
    assert_eq!(mk(&s2, "newer", "x").rev, 5);
}

#[test]
fn untitled_notes_and_slugs() {
    let (_d, s) = store();
    let a = mk(
        &s,
        "",
        "Buy milk and eggs for the whole week please now\nsecond line",
    );
    assert_eq!(a.title, "");
    assert_eq!(a.slug, "buy-milk-and-eggs-for-the", "first six words");
    let b = mk(&s, "", "Buy milk and eggs for the whole week please now");
    assert_eq!(b.slug, "buy-milk-and-eggs-for-the-2");
    let l = mk_list(&s, "", &[("", false), ("Pasta", false)]);
    assert_eq!(l.slug, "pasta");
    let e = s.create(CreateNote::default(), Caller("web")).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("bad_request", "title: must not be empty")
    );
    // Attachments alone make a note.
    let img = s
        .put_attachment(&png(1), "image/png", Some("a.png"))
        .unwrap();
    let c = s
        .create(
            CreateNote {
                attachments: Some(vec![AttachmentRef {
                    id: img.id.clone(),
                    file: Some(img.file.clone()),
                    ..AttachmentRef::default()
                }]),
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap();
    assert_eq!((c.slug.as_str(), c.attachments.len()), ("note", 1));
    // Clearing the title is fine while there is content, not when it was all there was.
    let u = patch(
        &s,
        &a.id,
        UpdateNote {
            title: Some("  ".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(u.title, "");
    let t = mk(&s, "only title", "");
    let e = patch(
        &s,
        &t.id,
        UpdateNote {
            title: Some("".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code, "bad_request");
    // Reserved route words are not slugs.
    for w in [
        "trash",
        "labels",
        "changes",
        "attachments",
        "import",
        "batch",
        "new",
        "search",
    ] {
        assert!(!valid_slug(w), "{w}");
        assert_eq!(slugify(w), format!("{w}-note"));
    }
}

#[test]
fn checklists_derive_parse_and_convert() {
    let (_d, s) = store();
    let l = mk_list(&s, "Groceries", &[("milk", false), ("eggs", true)]);
    assert_eq!(l.content, "- [ ] milk\n- [x] eggs");
    let items = l.items.clone().unwrap();
    assert!(items.iter().all(|i| valid_item_id(&i.id)));
    assert!(items[1].checked);
    let sm = &s.list(None, None, 10)[0];
    assert_eq!((sm.is_list, sm.items_total, sm.items_done), (true, 2, 1));

    // PATCH content on a list note parses it back; ids of unchanged text stay.
    let u = patch(
        &s,
        &l.id,
        UpdateNote {
            content: Some("- [x] milk\n  - [ ] oat milk\nbread\n* [ ] eggs\n\n- plain\n".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    let it = u.items.clone().unwrap();
    let texts: Vec<(&str, bool, u8)> = it
        .iter()
        .map(|i| (i.text.as_str(), i.checked, i.indent))
        .collect();
    assert_eq!(
        texts,
        [
            ("milk", true, 0),
            ("oat milk", false, 1),
            ("bread", false, 0),
            ("eggs", false, 0),
            ("plain", false, 0)
        ]
    );
    assert_eq!(it[0].id, items[0].id, "same text keeps its id");
    assert_eq!(it[3].id, items[1].id);
    assert_eq!(
        u.content,
        "- [x] milk\n  - [ ] oat milk\n- [ ] bread\n- [ ] eggs\n- [ ] plain"
    );
    // Derived content parses to the same items (round trip).
    let again = parse_items(&u.content, &it);
    assert_eq!(again, it);

    // Append: new lines become items at the end.
    let a = s
        .append(
            &l.id,
            AppendNote {
                content: "- [x] done thing\ncoffee".into(),
                ..AppendNote::default()
            },
        )
        .unwrap();
    let t: Vec<&str> = a
        .items
        .as_ref()
        .unwrap()
        .iter()
        .map(|i| i.text.as_str())
        .collect();
    assert_eq!(&t[5..], ["done thing", "coffee"]);
    assert!(a.items.as_ref().unwrap()[5].checked);
    assert!(a.content.ends_with("- [x] done thing\n- [ ] coffee"));
    let e = s
        .append(
            &l.id,
            AppendNote {
                content: "  \n".into(),
                ..AppendNote::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, "bad_request");

    // items win over content in one body; items: null is refused.
    let w = patch(
        &s,
        &l.id,
        UpdateNote {
            content: Some("ignored".into()),
            items: Some(Some(vec![ItemReq {
                text: "only".into(),
                ..ItemReq::default()
            }])),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(
        (w.content.as_str(), w.items.as_ref().unwrap().len()),
        ("- [ ] only", 1)
    );
    let e = patch(
        &s,
        &l.id,
        UpdateNote {
            items: Some(None),
            ..UpdateNote::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code, "bad_request");
    // Bad items.
    for bad in [
        ItemReq {
            text: "a\nb".into(),
            ..ItemReq::default()
        },
        ItemReq {
            text: "a".into(),
            indent: 2,
            ..ItemReq::default()
        },
    ] {
        assert!(patch(
            &s,
            &l.id,
            UpdateNote {
                items: Some(Some(vec![bad])),
                ..UpdateNote::default()
            }
        )
        .is_err());
    }

    // Convert to text and back.
    let t = patch(
        &s,
        &l.id,
        UpdateNote {
            convert: Some(Convert::Text),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert!(t.items.is_none());
    assert_eq!(t.content, "only");
    let c = patch(
        &s,
        &t.id,
        UpdateNote {
            content: Some("one\ntwo\n\nthree".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert!(c.items.is_none(), "content on a text note stays text");
    let c = patch(
        &s,
        &c.id,
        UpdateNote {
            convert: Some(Convert::List),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(c.items.as_ref().unwrap().len(), 3);
    assert_eq!(c.content, "- [ ] one\n- [ ] two\n- [ ] three");
    let e = patch(
        &s,
        &c.id,
        UpdateNote {
            convert: Some(Convert::Text),
            content: Some("x".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code, "bad_request");
    // Search matches item text.
    assert_eq!(s.list(Some("three"), None, 10).len(), 1);
}

#[test]
fn labels_registry_rename_merge_delete() {
    let (d, s) = store();
    let a = s
        .create(
            CreateNote {
                title: "a".into(),
                tags: vec!["GodLevel".into(), "work".into()],
                labels: vec!["#Fisica  2".into(), "WORK".into()],
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap();
    assert_eq!(a.tags, ["godlevel", "work", "fisica 2"]);
    let b = mk(&s, "b", "");
    let b = patch(
        &s,
        &b.id,
        UpdateNote {
            labels: Some(vec!["Work".into(), "Extra".into()]),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(b.tags, ["work", "extra"]);
    let l = s.labels();
    let names: Vec<(&str, &str, usize)> = l
        .iter()
        .map(|x| (x.tag.as_str(), x.name.as_str(), x.count))
        .collect();
    assert_eq!(
        names,
        [
            ("extra", "Extra", 1),
            ("fisica 2", "Fisica 2", 1),
            ("godlevel", "GodLevel", 1),
            ("work", "work", 2)
        ],
        "sorted by name, case-insensitive; the first spelling wins"
    );
    let sm = s.list(None, Some("GODLEVEL"), 10);
    assert_eq!(
        (sm.len(), sm[0].labels.clone()),
        (
            1,
            vec!["GodLevel".to_string(), "work".into(), "Fisica 2".into()]
        )
    );
    let perm = std::fs::metadata(d.path().join("notes/labels.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(perm, 0o600);

    // create: 201-like, then label_exists (also for a label in use).
    let c = s.create_label("Empty one").unwrap();
    assert_eq!(
        (c.tag.as_str(), c.name.as_str(), c.count),
        ("empty one", "Empty one", 0)
    );
    assert_eq!(
        s.create_label("EMPTY ONE").unwrap_err().code,
        "label_exists"
    );
    assert_eq!(s.create_label("work").unwrap_err().code, "label_exists");
    assert_eq!(s.create_label("a,b").unwrap_err().code, "bad_request");

    // Rename display only: no note is touched.
    let rev = s.rev();
    let r = s.rename_label("work", "Work").unwrap();
    assert_eq!(
        (r.tag.as_str(), r.name.as_str(), r.count),
        ("work", "Work", 2)
    );
    assert_eq!(s.rev(), rev);
    // Rename to another tag: rewritten in live notes and in the trash.
    let t = mk(&s, "trashed", "");
    let t = patch(
        &s,
        &t.id,
        UpdateNote {
            tags: Some(vec!["extra".into()]),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    s.delete(&t.id).unwrap();
    let r = s.rename_label("extra", "Bonus").unwrap();
    assert_eq!(
        (r.tag.as_str(), r.name.as_str(), r.count),
        ("bonus", "Bonus", 1)
    );
    assert_eq!(s.get(&b.id).unwrap().tags, ["work", "bonus"]);
    assert!(s.get(&b.id).unwrap().rev > rev);
    let ch = s.changes(rev, 50).unwrap();
    assert!(
        ch.notes.iter().any(|n| n.id == t.id && n.tags == ["bonus"]),
        "the trashed note is touched too"
    );
    // Merge into an existing label: keeps that label's name.
    let m = s.rename_label("bonus", "godlevel").unwrap();
    assert_eq!(
        (m.tag.as_str(), m.name.as_str(), m.count),
        ("godlevel", "GodLevel", 2)
    );
    assert_eq!(s.get(&b.id).unwrap().tags, ["work", "godlevel"]);
    assert!(s.labels().iter().all(|x| x.tag != "bonus"));
    assert_eq!(
        s.rename_label("nope", "x").unwrap_err().code,
        "label_not_found"
    );
    // Delete: out of every note and the registry.
    assert_eq!(s.delete_label("GodLevel").unwrap(), 3);
    assert!(s.get(&a.id).unwrap().tags == ["work", "fisica 2"]);
    assert!(s.labels().iter().all(|x| x.tag != "godlevel"));
    assert_eq!(
        s.delete_label("godlevel").unwrap_err().code,
        "label_not_found"
    );
    // The registry survives a restart.
    let s2 = NoteStore::new(d.path());
    assert!(s2
        .labels()
        .iter()
        .any(|x| x.tag == "fisica 2" && x.name == "Fisica 2"));

    // ensure_labels: new names only; dry run writes nothing; a ',' is cleaned.
    let dry = s
        .ensure_labels(&["Brand New".into(), "work".into(), "a,b".into()], true)
        .unwrap();
    assert_eq!(dry, ["Brand New", "a b"]);
    assert!(s.labels().iter().all(|x| x.tag != "brand new"));
    let real = s
        .ensure_labels(&["Brand New".into(), "work".into(), "a,b".into()], false)
        .unwrap();
    assert_eq!(real, ["Brand New", "a b"]);
    assert!(s
        .ensure_labels(&["brand new".into()], false)
        .unwrap()
        .is_empty());
}

#[test]
fn changes_cursor_paging_tombstones_and_expiry() {
    let (d, s) = store();
    let empty = s.changes(0, 10).unwrap();
    assert_eq!(
        (
            empty.notes.len(),
            empty.removed.len(),
            empty.cursor.as_str(),
            empty.has_more
        ),
        (0, 0, "0", false)
    );
    let a = mk(&s, "a", "1");
    let b = mk(&s, "b", "2");
    let c = mk(&s, "c", "3");
    assert_eq!((a.rev, b.rev, c.rev), (1, 2, 3));
    // Paging by 2.
    let p1 = s.changes(0, 2).unwrap();
    assert_eq!(
        (p1.notes.len(), p1.has_more, p1.cursor.as_str()),
        (2, true, "2")
    );
    let p2 = s.changes(2, 2).unwrap();
    assert_eq!(
        (p2.notes.len(), p2.has_more, p2.cursor.as_str()),
        (1, false, "3")
    );
    assert_eq!(p2.notes[0].id, c.id);
    let none = s.changes(3, 2).unwrap();
    assert_eq!((none.notes.len(), none.cursor.as_str()), (0, "3"));
    // An edit moves the note to the end; archive/trash/restore are changes too.
    let a2 = patch(
        &s,
        &a.id,
        UpdateNote {
            archived: Some(true),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(a2.rev, 4);
    s.delete(&b.id).unwrap();
    let ch = s.changes(3, 10).unwrap();
    assert_eq!(
        ch.notes
            .iter()
            .map(|n| (n.id.clone(), n.rev))
            .collect::<Vec<_>>(),
        [(a.id.clone(), 4), (b.id.clone(), 5)]
    );
    assert!(ch.notes[1].deleted_at.is_some() && ch.notes[0].archived);
    assert_eq!(s.restore(&b.id).unwrap().rev, 6);
    // Delete forever leaves a tombstone, in the same ordered stream.
    s.delete(&b.id).unwrap();
    s.delete_forever(&b.id).unwrap();
    let ch = s.changes(6, 10).unwrap();
    assert_eq!(ch.removed, std::slice::from_ref(&b.id));
    assert!(
        ch.notes.is_empty(),
        "the note itself is gone: only the removal is reported"
    );
    assert_eq!(ch.cursor, "8");
    // Everything from 0: live notes and no tombstones for rev 0 ... (the removed one is listed)
    let all = s.changes(0, 50).unwrap();
    assert_eq!(all.notes.len(), 2);
    assert_eq!(all.removed, std::slice::from_ref(&b.id));
    // A cursor from the future is not ours.
    assert_eq!(
        s.changes(999, 10).unwrap_err().status,
        axum::http::StatusCode::GONE
    );

    // Tombstones older than 60 days are pruned: a `since` below the pruned
    // revision is expired, `since` 0 and newer ones are fine.
    let state_path = d.path().join("notes/state.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    v["tombstones"][0]["at"] = "2020-01-01T00:00:00.000Z".into();
    std::fs::write(&state_path, serde_json::to_vec(&v).unwrap()).unwrap();
    // A new file in the notes dir makes the cache reload (as another process would).
    std::fs::write(d.path().join("notes/touch"), "").unwrap();
    let pruned_rev = 8;
    let e = s.changes(2, 10).unwrap_err();
    assert_eq!(
        (e.status, e.code.as_str()),
        (axum::http::StatusCode::GONE, "cursor_expired")
    );
    assert!(s.changes(pruned_rev, 10).is_ok());
    let all = s.changes(0, 10).unwrap();
    assert!(all.removed.is_empty() && all.notes.len() == 2);
    // The state file is private.
    assert_eq!(
        std::fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn expired_trash_is_purged_with_a_tombstone() {
    let (d, s) = store();
    let a = mk(&s, "keep me", "x");
    let b = mk(&s, "old", "y");
    s.delete(&b.id).unwrap();
    let p = d.path().join(format!("notes/trash/{}.json", b.id));
    let mut n: Note = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    n.deleted_at = Some("2020-01-01T00:00:00.000Z".into());
    std::fs::remove_file(&p).unwrap();
    n.id = "n_0000000000000000000f".into();
    std::fs::write(
        d.path().join("notes/trash/n_0000000000000000000f.json"),
        serde_json::to_vec(&n).unwrap(),
    )
    .unwrap();
    assert!(s.trash().is_empty());
    let ch = s.changes(0, 10).unwrap();
    assert_eq!(ch.notes.len(), 1);
    assert_eq!(ch.notes[0].id, a.id);
    assert_eq!(ch.removed, ["n_0000000000000000000f"]);
}

#[test]
fn attachments_dedup_gc_validation() {
    let (d, s) = store();
    let a = mk(&s, "a", "x");
    let b = mk(&s, "b", "y");
    let att = s
        .add_attachment(&a.id, &png(1), "image/png", Some("../etc/Photo 1.png"))
        .unwrap();
    let at = &att.attachments[0];
    assert_eq!(at.id.len(), 64);
    assert_eq!(at.file, format!("{}.png", at.id));
    assert_eq!(
        (at.mimetype.as_str(), at.size),
        ("image/png", png(1).len() as u64)
    );
    assert_eq!(
        at.name.as_deref(),
        Some("Photo 1.png"),
        "path parts dropped"
    );
    let path = d.path().join("notes/attachments").join(&at.file);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(d.path().join("notes/attachments"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(s.has_attachment(&at.id, "png") && !s.has_attachment(&at.id, "jpg"));
    assert!(s.attachment_path(&at.file).is_some());
    assert!(s.attachment_path("../../etc/passwd").is_none());
    assert!(s.attachment_path(&format!("{}.PNG", at.id)).is_none());
    assert!(!s.has_attachment("zz", "png"));
    // The same bytes again on one note: one attachment; on another: the same file.
    let again = s.add_attachment(&a.id, &png(1), "image/png", None).unwrap();
    assert_eq!(again.attachments.len(), 1);
    let on_b = s
        .add_attachment(&b.id, &png(1), "image/png; charset=binary", None)
        .unwrap();
    assert_eq!(on_b.attachments[0].file, at.file);
    assert_eq!(
        std::fs::read_dir(d.path().join("notes/attachments"))
            .unwrap()
            .count(),
        1
    );
    // Removing it from one note keeps the file (the other still uses it) ...
    let r = s.remove_attachment(&a.id, &at.id).unwrap();
    assert!(r.attachments.is_empty() && r.rev > att.rev);
    assert!(path.is_file());
    assert_eq!(
        s.remove_attachment(&a.id, &at.id).unwrap_err().code,
        "attachment_not_found"
    );
    // ... trashing keeps it, deleting forever the last user removes it.
    s.delete(&b.id).unwrap();
    assert!(path.is_file());
    s.delete_forever(&b.id).unwrap();
    assert!(!path.exists(), "unreferenced files go with the last note");
    // Refused: type, empty, size, too many.
    let e = s
        .add_attachment(&a.id, b"<html>", "text/html", None)
        .unwrap_err();
    assert_eq!(
        (e.status.as_u16(), e.code.as_str()),
        (415, "unsupported_media_type")
    );
    assert_eq!(
        s.add_attachment(&a.id, b"", "image/png", None)
            .unwrap_err()
            .status
            .as_u16(),
        400
    );
    let big = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
    let e = s
        .add_attachment(&a.id, &big, "application/pdf", None)
        .unwrap_err();
    assert_eq!(
        (e.status.as_u16(), e.code.as_str()),
        (413, "attachment_too_large")
    );
    let ok = vec![1u8; MAX_ATTACHMENT_BYTES];
    assert!(s.put_attachment(&ok, "application/pdf", None).is_ok());
    for i in 0..MAX_ATTACHMENTS {
        s.add_attachment(&a.id, &png(100 + i as u8), "image/png", None)
            .unwrap();
    }
    let e = s
        .add_attachment(&a.id, &png(99), "image/png", None)
        .unwrap_err();
    assert_eq!(
        (e.status.as_u16(), e.code.as_str()),
        (409, "attachments_full")
    );
    assert!(
        !s.has_attachment(&hex_sha(&png(99)), "png"),
        "the refused upload is not kept"
    );
    assert_eq!(
        s.add_attachment("nope", &png(1), "image/png", None)
            .unwrap_err()
            .code,
        "note_not_found"
    );
    // Mimetype table.
    for (m, e) in [
        ("audio/3gpp", "3gp"),
        ("audio/x-m4a", "m4a"),
        ("image/heic", "heic"),
        ("application/pdf", "pdf"),
        ("IMAGE/JPEG", "jpg"),
    ] {
        assert_eq!(ext_for_mime(m), Some(e), "{m}");
    }
    assert_eq!(ext_for_mime("video/mp4"), None);
}

fn hex_sha(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[test]
fn put_attachment_and_attachment_refs() {
    let (_d, s) = store();
    let att = s.put_attachment(&png(7), "image/png", None).unwrap();
    assert_eq!(att.id, hex_sha(&png(7)));
    let again = s.put_attachment(&png(7), "image/png", Some("x")).unwrap();
    assert_eq!(again.file, att.file);
    // A body may refer to stored files; unknown ones are refused.
    let n = s
        .create(
            CreateNote {
                title: "refs".into(),
                attachments: Some(vec![AttachmentRef {
                    id: att.id.clone(),
                    mimetype: Some("image/png".into()),
                    ..AttachmentRef::default()
                }]),
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap();
    assert_eq!(n.attachments[0].size, att.size);
    let e = s
        .create(
            CreateNote {
                title: "x".into(),
                attachments: Some(vec![AttachmentRef {
                    id: "a".repeat(64),
                    mimetype: Some("image/png".into()),
                    ..AttachmentRef::default()
                }]),
                ..CreateNote::default()
            },
            Caller("web"),
        )
        .unwrap_err();
    assert_eq!(e.code, "bad_request");
    // Replacing the list drops (and collects) the old file.
    let u = patch(
        &s,
        &n.id,
        UpdateNote {
            attachments: Some(vec![]),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert!(u.attachments.is_empty());
    assert!(!s.has_attachment(&att.id, "png"));
}

#[test]
fn batch_operations() {
    let (_d, s) = store();
    let a = mk(&s, "a", "");
    let b = mk(&s, "b", "");
    let c = mk(&s, "c", "");
    let out = s
        .batch(&BatchReq {
            ids: vec![a.id.clone(), b.slug.clone(), "missing".into(), a.id.clone()],
            set: Some(BatchSet {
                color: Some("Red".into()),
                pinned: Some(true),
                archived: None,
            }),
            add_labels: vec!["Work".into(), "x".into()],
            remove_labels: vec![],
            ..BatchReq::default()
        })
        .unwrap();
    assert_eq!(out.notes.len(), 2, "duplicates and unknown ids are skipped");
    assert!(out
        .notes
        .iter()
        .all(|n| n.color == "red" && n.pinned && n.tags == ["work", "x"]));
    assert!(s.labels().iter().any(|l| l.name == "Work" && l.count == 2));
    // Nothing to change: nothing returned, no rev bump.
    let rev = s.rev();
    let same = s
        .batch(&BatchReq {
            ids: vec![a.id.clone()],
            set: Some(BatchSet {
                pinned: Some(true),
                ..BatchSet::default()
            }),
            ..BatchReq::default()
        })
        .unwrap();
    assert!(same.notes.is_empty());
    assert_eq!(s.rev(), rev);
    let rm = s
        .batch(&BatchReq {
            ids: vec![a.id.clone()],
            remove_labels: vec!["X".into()],
            ..BatchReq::default()
        })
        .unwrap();
    assert_eq!(rm.notes[0].tags, ["work"]);
    // Trash, restore, delete forever.
    let t = s
        .batch(&BatchReq {
            ids: vec![a.id.clone(), c.id.clone()],
            trash: true,
            ..BatchReq::default()
        })
        .unwrap();
    assert!(t.notes.iter().all(|n| n.deleted_at.is_some()));
    assert_eq!(s.list(None, None, 10).len(), 1);
    let r = s
        .batch(&BatchReq {
            ids: vec![a.id.clone()],
            restore: true,
            ..BatchReq::default()
        })
        .unwrap();
    assert!(r.notes[0].deleted_at.is_none());
    let f = s
        .batch(&BatchReq {
            ids: vec![c.id.clone(), a.id.clone()],
            delete_forever: true,
            ..BatchReq::default()
        })
        .unwrap();
    assert_eq!(f.removed, std::slice::from_ref(&c.id), "only trashed notes");
    assert!(f.notes.is_empty());
    // Refused combinations and limits.
    for bad in [
        BatchReq {
            ids: vec![a.id.clone()],
            trash: true,
            restore: true,
            ..BatchReq::default()
        },
        BatchReq {
            ids: vec![a.id.clone()],
            trash: true,
            set: Some(BatchSet::default()),
            ..BatchReq::default()
        },
        BatchReq {
            ids: (0..501).map(|i| format!("k{i}")).collect(),
            ..BatchReq::default()
        },
        BatchReq {
            ids: vec![a.id.clone()],
            set: Some(BatchSet {
                color: Some("mauve".into()),
                ..BatchSet::default()
            }),
            ..BatchReq::default()
        },
    ] {
        assert_eq!(s.batch(&bad).unwrap_err().code, "bad_request");
    }
}

#[test]
fn delete_forever_and_empty_trash() {
    let (_d, s) = store();
    let a = mk(&s, "a", "");
    let b = mk(&s, "b", "");
    assert_eq!(
        s.delete_forever(&a.id).unwrap_err().code,
        "note_not_found",
        "live notes go to the trash first"
    );
    s.delete(&a.id).unwrap();
    s.delete(&b.id).unwrap();
    s.delete_forever(&a.id).unwrap();
    assert_eq!(s.restore(&a.id).unwrap_err().code, "note_not_found");
    assert_eq!(s.empty_trash(), 1);
    assert_eq!(s.empty_trash(), 0);
    let ch = s.changes(0, 10).unwrap();
    assert_eq!(ch.removed.len(), 2);
}

#[test]
fn list_filters_and_summary_fields() {
    let (_d, s) = store();
    let a = mk_list(&s, "list", &[("x", true), ("y", false)]);
    let b = mk(&s, "text", "hello");
    let att = s.add_attachment(&b.id, &png(3), "image/png", None).unwrap();
    assert!(att.attachments[0].mimetype.starts_with("image/"));
    s.add_attachment(&b.id, b"ID3", "audio/mpeg", None).unwrap();
    let c = patch(
        &s,
        &mk(&s, "linky", "z").id,
        UpdateNote {
            annotations: Some(vec![Annotation {
                url: "https://example.com".into(),
                title: Some("Ex".into()),
                description: None,
                source: None,
            }]),
            color: Some("blue".into()),
            archived: Some(true),
            reminder: Some(Some(ReminderReq {
                at: "2030-01-01T09:00:00+01:00".into(),
                recurrence: Some("Weekly".into()),
                lucina_id: None,
            })),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(
        c.reminder.as_ref().unwrap().recurrence.as_deref(),
        Some("weekly")
    );
    let f = |q: &ListQuery| Filter::from_query(q).unwrap();
    let ids = |q: &ListQuery| -> Vec<String> {
        s.list_filtered(&f(q), 50)
            .into_iter()
            .map(|n| n.title)
            .collect()
    };
    let q = |k: &str, v: &str| match k {
        "type" => ListQuery {
            kind: Some(v.into()),
            ..ListQuery::default()
        },
        "color" => ListQuery {
            color: Some(v.into()),
            ..ListQuery::default()
        },
        _ => ListQuery {
            archived: Some(v.into()),
            ..ListQuery::default()
        },
    };
    assert_eq!(ids(&q("type", "list")), ["list"]);
    assert_eq!(ids(&q("type", "image")), ["text"]);
    assert_eq!(ids(&q("type", "audio")), ["text"]);
    assert_eq!(ids(&q("type", "link")), ["linky"]);
    assert_eq!(ids(&q("type", "reminder")), ["linky"]);
    assert_eq!(ids(&q("type", "text")).len(), 2);
    assert_eq!(ids(&q("color", "BLUE")), ["linky"]);
    assert_eq!(ids(&q("archived", "only")), ["linky"]);
    assert_eq!(ids(&q("archived", "exclude")).len(), 2);
    assert_eq!(ids(&q("archived", "include")).len(), 3);
    assert_eq!(ids(&ListQuery::default()).len(), 3);
    for bad in [
        q("type", "video"),
        q("color", "mauve"),
        q("archived", "maybe"),
    ] {
        assert!(Filter::from_query(&bad).is_err());
    }
    let sm: Vec<NoteSummary> = s.list(None, None, 10);
    let get = |id: &str| sm.iter().find(|n| n.id == id).unwrap().clone();
    let (la, lb, lc) = (get(&a.id), get(&b.id), get(&c.id));
    assert_eq!(
        (
            la.is_list,
            la.items_total,
            la.items_done,
            la.attachment_count
        ),
        (true, 2, 1, 0)
    );
    assert_eq!(
        (lb.is_list, lb.attachment_count, lb.has_image),
        (false, 2, true)
    );
    assert_eq!(
        (lc.color.as_str(), lc.archived, lc.reminder_at.as_deref()),
        ("blue", true, Some("2030-01-01T09:00:00+01:00"))
    );
    assert!(lc.position > 1e12 && lc.rev > 0 && lc.labels.is_empty());
    // Bad field values.
    let bad = |u: UpdateNote| patch(&s, &a.id, u).unwrap_err().code;
    assert_eq!(
        bad(UpdateNote {
            color: Some("mauve".into()),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    assert_eq!(
        bad(UpdateNote {
            reminder: Some(Some(ReminderReq {
                at: "tomorrow".into(),
                ..ReminderReq::default()
            })),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    assert_eq!(
        bad(UpdateNote {
            reminder: Some(Some(ReminderReq {
                at: "2030-01-01T00:00:00Z".into(),
                recurrence: Some("hourly".into()),
                lucina_id: None
            })),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    assert_eq!(
        bad(UpdateNote {
            collaborators: Some(vec!["not an email".into()]),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    assert_eq!(
        bad(UpdateNote {
            annotations: Some(vec![Annotation {
                url: "javascript:alert(1)".into(),
                title: None,
                description: None,
                source: None
            }]),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    assert_eq!(
        bad(UpdateNote {
            position: Some(f64::NAN),
            ..UpdateNote::default()
        }),
        "bad_request"
    );
    let ok = patch(
        &s,
        &a.id,
        UpdateNote {
            collaborators: Some(vec!["Me@Example.com".into(), "me@example.com".into()]),
            position: Some(5.0),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert_eq!(
        (ok.collaborators, ok.position),
        (vec!["me@example.com".to_string()], 5.0)
    );
    // Clearing a reminder.
    let cleared = patch(
        &s,
        &c.id,
        UpdateNote {
            reminder: Some(None),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    assert!(cleared.reminder.is_none());
}

#[test]
fn keep_fields_serialize_camel_case_and_old_clients_still_parse() {
    let (_d, s) = store();
    let n = mk_list(&s, "t", &[("x", false)]);
    let v = serde_json::to_value(&n).unwrap();
    for k in [
        "id",
        "slug",
        "title",
        "content",
        "tags",
        "source",
        "pinned",
        "createdAt",
        "updatedAt",
        "color",
        "archived",
        "items",
        "attachments",
        "annotations",
        "collaborators",
        "position",
        "rev",
    ] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert!(
        v.get("reminder").is_none() && v.get("import").is_none() && v.get("deletedAt").is_none()
    );
    assert_eq!(v["items"][0]["checked"], false);
    let plain = serde_json::to_value(mk(&s, "plain", "x")).unwrap();
    assert!(
        plain.get("items").is_none(),
        "text notes carry no items key"
    );
}

#[test]
fn import_note_outcomes() {
    let (d, s) = store();
    let att = s
        .put_attachment(&png(9), "image/png", Some("pic.png"))
        .unwrap();
    let mut n = imported("gkeep:1700000000000000", 100, "Imported");
    n.labels = vec!["Fisica 2".into(), "GodLevel".into()];
    n.attachments = vec![att.clone()];
    n.pinned = true;
    n.annotations = vec![Annotation {
        url: "https://example.com".into(),
        title: None,
        description: None,
        source: Some("web".into()),
    }];
    n.collaborators = vec!["friend@example.com".into()];
    // A dry run decides and writes nothing, not even labels.
    assert_eq!(
        s.import_note(n.clone(), true, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    assert!(s.list(None, None, 10).is_empty() && s.labels().is_empty());
    assert_eq!(s.rev(), 0);
    assert_eq!(
        s.import_note(n.clone(), false, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    let got = s.get("imported").unwrap();
    assert_eq!(
        (got.source.as_str(), got.color.as_str(), got.pinned),
        ("google-keep", "teal", true)
    );
    assert_eq!(got.tags, ["fisica 2", "godlevel"]);
    assert_eq!(got.created_at, "2024-03-01T10:00:00Z");
    assert_eq!(got.updated_at, "2024-03-02T10:00:00Z");
    assert_eq!(
        got.position, 1_709_287_200_000.0,
        "position = createdAt in ms"
    );
    assert_eq!(got.attachments, std::slice::from_ref(&att));
    assert_eq!(got.collaborators, ["friend@example.com"]);
    let info = got.import.clone().unwrap();
    assert_eq!(
        (info.source.as_str(), info.key.as_str(), info.edited_usec),
        ("google-keep", "gkeep:1700000000000000", 100)
    );
    assert_eq!(info.imported_at, got.updated_at);
    assert!(s.labels().iter().any(|l| l.name == "Fisica 2"));

    // Same export again: unchanged, nothing written.
    let rev = s.rev();
    assert_eq!(
        s.import_note(n.clone(), false, Caller("owner")).unwrap(),
        ImportOutcome::Unchanged
    );
    assert_eq!(s.rev(), rev);
    // Newer in Keep, untouched here: updated in place (same id and slug).
    let mut newer = n.clone();
    newer.edited_usec = 200;
    newer.title = "Imported (renamed)".into();
    newer.content = "new body".into();
    newer.updated_at = "2024-03-05T10:00:00.000Z".into();
    newer.color = "mauve".into();
    newer.labels = vec!["GodLevel".into()];
    newer.pinned = false;
    assert_eq!(
        s.import_note(newer.clone(), true, Caller("owner")).unwrap(),
        ImportOutcome::Updated
    );
    assert_eq!(s.get("imported").unwrap().content, "body", "dry run");
    assert_eq!(
        s.import_note(newer.clone(), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::Updated
    );
    let up = s.get(&got.id).unwrap();
    assert_eq!(
        (up.slug.as_str(), up.title.as_str(), up.content.as_str()),
        ("imported", "Imported (renamed)", "new body")
    );
    assert_eq!(
        (up.color.as_str(), up.pinned),
        ("default", false),
        "unknown colors fall back"
    );
    assert_eq!(up.tags, ["godlevel"]);
    assert_eq!(up.import.as_ref().unwrap().edited_usec, 200);
    assert_eq!(up.updated_at, "2024-03-05T10:00:00Z");
    assert!(up.rev > got.rev);

    // Edited here since the import: a newer export leaves it alone.
    let edited = patch(
        &s,
        &got.id,
        UpdateNote {
            content: Some("my words".into()),
            ..UpdateNote::default()
        },
    )
    .unwrap();
    let mut newest = newer.clone();
    newest.edited_usec = 300;
    newest.content = "keep's words".into();
    assert_eq!(
        s.import_note(newest.clone(), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::KeptLocalEdits
    );
    assert_eq!(s.get(&got.id).unwrap().content, "my words");
    assert_eq!(s.get(&got.id).unwrap().rev, edited.rev);
    // The same export (or an older one) is "unchanged" even then.
    assert_eq!(
        s.import_note(newer.clone(), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::Unchanged
    );

    // Trashed here: left alone (kept).
    s.delete(&got.id).unwrap();
    assert_eq!(
        s.import_note(newest.clone(), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::KeptLocalEdits
    );
    assert!(s.list(None, None, 10).is_empty());
    // Deleted forever here: never resurrected, also across a restart.
    s.delete_forever(&got.id).unwrap();
    assert_eq!(
        s.import_note(n.clone(), false, Caller("owner")).unwrap(),
        ImportOutcome::SkippedDeleted
    );
    assert_eq!(
        s.import_note(n.clone(), true, Caller("owner")).unwrap(),
        ImportOutcome::SkippedDeleted
    );
    let keys: Vec<String> =
        serde_json::from_slice(&std::fs::read(d.path().join("notes/import-deleted.json")).unwrap())
            .unwrap();
    assert_eq!(keys, ["gkeep:1700000000000000"]);
    let s2 = NoteStore::new(d.path());
    assert_eq!(
        s2.import_note(n.clone(), false, Caller("owner")).unwrap(),
        ImportOutcome::SkippedDeleted
    );
    assert_eq!(
        std::fs::metadata(d.path().join("notes/import-deleted.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    // The attachment was collected with the note.
    assert!(!s.has_attachment(&att.id, "png"));
}

#[test]
fn import_note_kinds_and_leniency() {
    let (_d, s) = store();
    // A list note, archived; items keep order and state; slug from the items.
    let mut l = imported("gkeep:1", 1, "");
    l.content = String::new();
    l.items = Some(vec![("Buy bread".into(), false), ("Call mum".into(), true)]);
    l.archived = true;
    assert_eq!(
        s.import_note(l, false, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    let n = s.get("buy-bread").unwrap();
    assert_eq!(
        (
            n.content.as_str(),
            n.archived,
            n.items.as_ref().unwrap().len()
        ),
        ("- [ ] Buy bread\n- [x] Call mum", true, 2)
    );
    // Trashed in Keep: lands in the trash with deletedAt = now.
    let mut t = imported("gkeep:2", 1, "In the bin");
    t.trashed = true;
    assert_eq!(
        s.import_note(t, false, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    assert!(s.get("in-the-bin").is_err());
    let tr = s.trash();
    assert_eq!(tr.len(), 1);
    assert!(tr[0].deleted_at.is_some());
    // Odd input is cleaned, not refused: long title clipped, control chars, bad links, a ',' label.
    let mut odd = imported("gkeep:3", 1, &"x".repeat(300));
    odd.content = "a\u{7}b\r\nc".into();
    odd.labels = vec!["a,b".into(), "".into(), "ok".into()];
    odd.annotations = vec![Annotation {
        url: "ftp://x".into(),
        title: None,
        description: None,
        source: None,
    }];
    odd.collaborators = vec!["nonsense".into()];
    assert_eq!(
        s.import_note(odd, false, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    let o = s.get(&"x".repeat(64)).unwrap();
    assert_eq!(
        (o.title.chars().count(), o.content.as_str()),
        (200, "ab\nc")
    );
    assert_eq!(o.tags, ["a b", "ok"]);
    assert!(o.annotations.is_empty() && o.collaborators.is_empty());
    // Colliding titles get -2; a blank Keep note is still imported.
    assert_eq!(
        s.import_note(imported("gkeep:4", 1, "Same"), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::New
    );
    assert_eq!(
        s.import_note(imported("gkeep:5", 1, "Same"), false, Caller("owner"))
            .unwrap(),
        ImportOutcome::New
    );
    assert!(s.get("same").is_ok() && s.get("same-2").is_ok());
    let mut blank = imported("gkeep:6", 1, "");
    blank.content = String::new();
    assert_eq!(
        s.import_note(blank, false, Caller("owner")).unwrap(),
        ImportOutcome::New
    );
    // Content over the limit is an error for the importer's report.
    let mut huge = imported("gkeep:7", 1, "Huge");
    huge.content = "x".repeat(MAX_CONTENT_BYTES + 1);
    assert_eq!(
        s.import_note(huge, false, Caller("owner"))
            .unwrap_err()
            .code,
        "note_too_large"
    );
    assert_eq!(
        s.import_note(imported("", 1, "k"), false, Caller("owner"))
            .unwrap_err()
            .code,
        "bad_request"
    );
}

#[test]
fn two_stores_on_one_dir_keep_the_counter_ordered() {
    // The CLI importer and the server write the same directory.
    let (d, s) = store();
    let other = NoteStore::new(d.path());
    let a = mk(&s, "a", "");
    let b = mk(&other, "b", "");
    let c = mk(&s, "c", "");
    let d2 = mk(&other, "d", "");
    assert_eq!([a.rev, b.rev, c.rev, d2.rev], [1, 2, 3, 4]);
    assert_eq!(s.changes(0, 10).unwrap().notes.len(), 4);
}

#[test]
fn thousands_of_notes_are_served_from_the_cache() {
    // 3000 legacy files load once; list, changes and labels then work on the
    // index: corrupting a file in place (no directory change) does not matter.
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    for i in 0..3000u32 {
        let id = format!("n_{i:020x}");
        let n = format!(
            r#"{{"id":"{id}","slug":"note-{i}","title":"Note {i}","content":"body {i}","tags":["t{}"],"source":"owner","createdAt":"2025-01-01T00:00:00.000Z","updatedAt":"2025-01-01T00:{:02}:{:02}.000Z"}}"#,
            i % 7,
            (i / 60) % 60,
            i % 60
        );
        std::fs::write(notes.join(format!("{id}.json")), n).unwrap();
    }
    let s = NoteStore::new(dir.path());
    assert_eq!(s.list(None, None, 5000).len(), 3000);
    let victim = notes.join(format!("n_{:020x}.json", 5));
    std::fs::write(&victim, "not json").unwrap();
    assert_eq!(s.list(None, None, 5000).len(), 3000, "no re-read");
    assert_eq!(s.list(Some("body 2999"), None, 5000).len(), 1);
    assert_eq!(s.changes(0, 500).unwrap().notes.len(), 500);
    assert_eq!(s.changes(2990, 500).unwrap().notes.len(), 10);
    assert_eq!(s.labels().len(), 7);
    assert_eq!(s.rev(), 3000);
    // A write keeps the cache valid (our own stamp is refreshed).
    mk(&s, "new", "x");
    assert_eq!(s.list(None, None, 5000).len(), 3001);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "not json");
}
