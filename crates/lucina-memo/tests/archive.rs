use lucina_memo::{archive, model::Status, store::NewMemo, App, Config};
use std::{path::Path, sync::Arc};
fn app(root: &Path, mount: &str) -> Arc<App> {
    let data = root.join("data").display().to_string();
    let archive = root.join("archive").display().to_string();
    let cfg = Config::from_lookup(|k| match k {
        "LUCINA_MEMO_DIR" => Some(data.clone()),
        "LUCINA_MEMO_ARCHIVE_DIR" => Some(archive.clone()),
        "LUCINA_MEMO_ARCHIVE_REQUIRE_MOUNT" => Some(mount.into()),
        _ => None,
    });
    App::new(
        cfg,
        Arc::new(lucina_llm::ScriptedModel::new()),
        "test".into(),
    )
    .unwrap()
}
fn memo(app: &App) -> String {
    let (m, _) = app
        .store
        .create(NewMemo {
            client_id: "archive-test".into(),
            title: None,
            recorded_at: "2026-10-27T08:20:00Z".parse().unwrap(),
            ended_at: None,
            location: None,
            timezone: None,
            duration_sec: 1.0,
            mime: "audio/mp4".into(),
            size_bytes: 4,
            source: "test".into(),
        })
        .unwrap();
    app.store
        .update(&m.id, |m| m.status = Status::Queued)
        .unwrap();
    app.store
        .update(&m.id, |m| m.status = Status::Done)
        .unwrap();
    m.id
}
#[test]
fn copies_all_files_incrementally_verifies_and_preserves_removed_sources() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), "");
    let id = memo(&app);
    let source = app.store.dir(&id);
    for name in [
        "audio.m4a",
        "device-transcript.json",
        "transcript.json",
        "summary.json",
        "memo.html",
        "memo.pdf",
        "note.md",
        "series-index.md",
        "extra.bin",
    ] {
        std::fs::write(source.join(name), name).unwrap();
    }
    std::fs::create_dir(source.join("work")).unwrap();
    std::fs::write(source.join("work/private"), "skip").unwrap();
    std::fs::write(source.join(".note.md.tmp123"), "skip").unwrap();
    let dest = root.path().join("archive/2026").join(&id);
    let first = archive::archive_memo(&app, &id).unwrap();
    assert_eq!(first.copied, 10);
    assert!(!dest.join("work").exists());
    assert!(!dest.join("audio.part").exists());
    assert_eq!(
        std::fs::read_to_string(dest.join("SHA256SUMS"))
            .unwrap()
            .lines()
            .count(),
        10
    );
    assert_eq!(archive::verify_memo(&app, &id).unwrap().verified_ok, 10);
    let second = archive::archive_memo(&app, &id).unwrap();
    assert_eq!(second.copied, 0);
    assert_eq!(second.unchanged, 10);
    std::fs::write(source.join("summary.json"), "changed").unwrap();
    let changed = archive::archive_memo(&app, &id).unwrap();
    assert_eq!(changed.copied, 1);
    assert_eq!(changed.bytes, 7);
    std::fs::remove_file(source.join("extra.bin")).unwrap();
    archive::archive_memo(&app, &id).unwrap();
    assert!(dest.join("extra.bin").exists());
    assert!(std::fs::read_to_string(dest.join("SHA256SUMS"))
        .unwrap()
        .contains("extra.bin"));
    std::fs::write(dest.join("summary.json"), "tampered").unwrap();
    assert_eq!(archive::verify_memo(&app, &id).unwrap().mismatches, 1);
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_lucina-memo"))
        .args(["archive", "--verify"])
        .env("LUCINA_MEMO_DIR", root.path().join("data"))
        .env("LUCINA_MEMO_ARCHIVE_DIR", root.path().join("archive"))
        .env("LUCINA_MEMO_ARCHIVE_REQUIRE_MOUNT", "")
        .output()
        .unwrap();
    assert_eq!(cli.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&cli.stdout).contains("mismatches=1"));
    assert!(archive::run_cli(&app, true, true).is_err());
    assert_eq!(archive::verify_memo(&app, &id).unwrap().mismatches, 0);
    assert_eq!(app.store.get(&id).unwrap().archived_rev, Some(0));
    app.store.trash(&id).unwrap();
    // A memo archived before deletion still needs its one trash copy.
    assert!(archive::archive_one(&app));
    assert!(root
        .path()
        .join("archive/trash")
        .join(&id)
        .join("SHA256SUMS")
        .exists());
    assert!(!archive::archive_one(&app));
}
#[test]
fn absent_mount_writes_nothing_and_retries_back_off() {
    let root = tempfile::tempdir().unwrap();
    let mount = root.path().join("not-mounted");
    std::fs::create_dir(&mount).unwrap();
    let app = app(root.path(), mount.to_str().unwrap());
    let id = memo(&app);
    assert!(archive::archive_memo(&app, &id).is_err());
    assert!(!archive::archive_one(&app));
    assert!(!root.path().join("archive").exists());
    assert!(app.store.get(&id).unwrap().archived_at.is_none());
}
