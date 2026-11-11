mod common;

use chrono::{TimeZone, Utc};
use common::status;
use lucina_parcels::{ParcelState, Store};

fn t0() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 28, 7, 0, 0).unwrap()
}

#[test]
fn the_list_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parcels").join("parcels.json");
    let store = Store::open(&path);
    let a = store.add(
        " 3uw1 s8iaaka23 ",
        Some("poste-italiane"),
        Some("Shoes"),
        t0(),
    );
    assert!(a.created);
    assert_eq!(a.parcel.number, "3UW1S8IAAKA23");
    assert!(
        a.parcel.id.starts_with("pc-") && a.parcel.id.len() == 11,
        "{}",
        a.parcel.id
    );
    store.update(&a.parcel.id, |p| {
        p.status = Some(status("postnord_delivered"))
    });
    store.set_digest_sent("2026-09-28");

    let again = Store::open(&path);
    let p = again.get("3UW1S8IAAKA23").unwrap();
    assert_eq!(p, store.get(&a.parcel.id).unwrap());
    assert_eq!(p.status, Some(status("postnord_delivered")));
    assert_eq!(again.digest_sent().as_deref(), Some("2026-09-28"));
    assert!(
        !path.with_extension("json.tmp").exists(),
        "the temp file is renamed away"
    );
}

#[test]
fn a_number_added_twice_is_one_parcel() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let a = store.add("rr123456789se", None, None, t0());
    let b = store.add("RR 123456789 SE", Some("postnord"), Some("Book"), t0());
    assert!(!b.created);
    assert_eq!(a.parcel.id, b.parcel.id);
    assert_eq!(store.list().len(), 1);
    assert_eq!(b.parcel.carrier.as_deref(), Some("postnord"));
    assert_eq!(b.parcel.label.as_deref(), Some("Book"));
    // A label already set is kept.
    let c = store.add("RR123456789SE", None, Some("Other"), t0());
    assert_eq!(c.parcel.label.as_deref(), Some("Book"));
}

#[test]
fn adding_a_stopped_parcel_reactivates_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let a = store.add("X1", None, None, t0());
    store.update(&a.parcel.id, |p| p.state = ParcelState::Stopped);
    let later = t0() + chrono::Duration::days(3);
    let b = store.add("x1", None, None, later);
    assert_eq!(
        (b.parcel.state, b.parcel.added_at),
        (ParcelState::Active, later)
    );
}

#[test]
fn a_parcel_is_found_by_id_number_or_label() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let a = store.add("ab12", None, Some("Red Shoes"), t0()).parcel;
    for key in [a.id.as_str(), "AB12", " ab12 ", "red shoes", "RED SHOES"] {
        assert_eq!(store.get(key).map(|p| p.id), Some(a.id.clone()), "{key}");
    }
    assert!(store.get("nope").is_none());
    assert_eq!(store.remove("red shoes").map(|p| p.id), Some(a.id));
    assert!(store.list().is_empty());
}

#[test]
fn a_missing_or_unreadable_file_is_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Store::open(dir.path().join("none.json")).list().is_empty());
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let store = Store::open(&bad);
    assert!(store.list().is_empty());
    store.add("A1", None, None, t0());
    assert_eq!(
        Store::open(&bad).list().len(),
        1,
        "the next save replaces the broken file"
    );
}

#[test]
fn a_change_made_by_another_process_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.json");
    let server = Store::open(&path);
    assert!(server.list().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let cli = Store::open(&path);
    cli.add("CLI1", None, None, t0());
    assert_eq!(server.list().len(), 1);
    assert_eq!(server.get("cli1").map(|p| p.number), Some("CLI1".into()));
}
