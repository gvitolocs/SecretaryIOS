//! Compatibility against COPIES of the live Node data files.
//!
//! `LUCINA_COMPAT_TIMELINE=/tmp/copy/timeline.jsonl` and
//! `LUCINA_COMPAT_REMINDERS=/tmp/copy/reminders.json` point at copies (never
//! the originals: the test copies them again into a temp dir before
//! touching anything). Without the variables the tests are skipped.

use std::path::PathBuf;

use lucina_store::reminders::{CreateReminder, ReminderStore};
use lucina_store::timeline::{NewItem, TimelineItem, TimelineStore, DEFAULT_MAX_BYTES};

fn fixture(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.exists())
}

#[test]
fn timeline_round_trips_every_line() {
    let Some(src) = fixture("LUCINA_COMPAT_TIMELINE") else {
        eprintln!("LUCINA_COMPAT_TIMELINE unset: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    std::fs::copy(&src, &path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let text = std::str::from_utf8(&original).unwrap();
    let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty());

    // 1. every line parses into the typed item and re-serializes byte for byte
    for (n, line) in lines.iter().enumerate() {
        let item: TimelineItem = serde_json::from_str(line).unwrap_or_else(|e| panic!("line {n}: {e}"));
        let again = serde_json::to_string(&item).unwrap();
        assert_eq!(&again, line, "line {n} does not round-trip");
    }

    // 2. the store indexes every line and serves it verbatim, sorted by id
    let store = TimelineStore::new(&path, DEFAULT_MAX_BYTES, true);
    let all = store.read_all().unwrap();
    assert_eq!(all.len(), lines.len());
    let mut sorted: Vec<&str> = lines.clone();
    sorted.sort_by_key(|l| serde_json::from_str::<TimelineItem>(l).unwrap().id);
    for (stored, line) in all.iter().zip(sorted.iter()) {
        assert_eq!(&*stored.raw, *line);
    }
    let (page, next) = store.list(None, 50).unwrap();
    assert_eq!(page.len(), 50.min(lines.len()));
    if lines.len() > 50 {
        assert!(next.is_some());
    }

    // 3. appending keeps every existing byte
    store
        .append(NewItem { channel: "web".into(), role: "user".into(), text: "compat probe".into(), ..NewItem::default() })
        .unwrap();
    let after = std::fs::read(&path).unwrap();
    assert_eq!(&after[..original.len()], &original[..]);
    assert_eq!(store.len().unwrap(), lines.len() + 1);
    eprintln!("timeline: {} lines round-tripped", lines.len());
}

#[test]
fn reminders_round_trip_and_append() {
    let Some(src) = fixture("LUCINA_COMPAT_REMINDERS") else {
        eprintln!("LUCINA_COMPAT_REMINDERS unset: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reminders.json");
    std::fs::copy(&src, &path).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    assert_eq!(ReminderStore::reserialize(original.as_bytes()).unwrap(), original);

    let store = ReminderStore::new(&path, 500);
    let before = store.list(true).unwrap();
    let (created, _) = store
        .create(CreateReminder {
            text: "compat probe".into(),
            due_at: "2099-01-01T08:00:00.000Z".into(),
            chat_id: "compat".into(),
            user_peer_id: "giuseppe".into(),
            user_label: "test".into(),
            persona: "Lucina".into(),
            time_zone: "Europe/Rome".into(),
            dedupe_key: None,
            source: "secretary".into(),
        })
        .unwrap();
    assert!(created);
    let after = store.list(true).unwrap();
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(&after[..before.len()], &before[..]);
    eprintln!("reminders: {} entries round-tripped", before.len());
}
