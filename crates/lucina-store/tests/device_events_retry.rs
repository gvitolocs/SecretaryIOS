//! Regression test of finding E1 (docs/verification-gaps.md, Stateright model
//! `crates/secretary-server/tests/device_events_model.rs`): when appending a
//! batch to `device-context.jsonl` failed, `DeviceEventStore::ingest` had
//! already marked the batch's ids as seen, so the client's retry was answered
//! "duplicate" and the events were never stored (until a restart reloaded the
//! file). After the fix a failed append forgets the ids and the retry stores
//! the events.

use chrono::{Duration, Utc};
use lucina_store::device::DeviceEventStore;
use serde_json::{json, Value};

fn event(id: &str, ts: chrono::DateTime<Utc>) -> Value {
    json!({"id": id, "ts": ts.to_rfc3339(), "type": "app_open", "source": "ios", "data": {"app": "Safari"}})
}

#[test]
fn a_retry_after_a_failed_append_stores_the_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device-context.jsonl");
    let store = DeviceEventStore::new(&path, 30);
    let now = Utc::now();

    // The store is loaded and the file exists.
    let out = store.ingest(&[event("e1", now)], now).unwrap();
    assert_eq!(out["accepted"], json!(["e1"]));

    // The append fails: the path is now a directory.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.ingest(&[event("e2", now)], now).is_err());

    // The disk recovers; the client retries the same batch.
    std::fs::remove_dir(&path).unwrap();
    let out = store.ingest(&[event("e2", now)], now).unwrap();
    assert_eq!(out["accepted"], json!(["e2"]), "retry answered {out}");
    assert_eq!(out["duplicates"], json!([]));

    let stored: Vec<String> = store
        .between(now - Duration::minutes(1), now + Duration::minutes(1))
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(stored.contains(&"e2".to_owned()), "stored {stored:?}");

    // A real duplicate is still a duplicate.
    let out = store.ingest(&[event("e2", now)], now).unwrap();
    assert_eq!(out["duplicates"], json!(["e2"]));
}
