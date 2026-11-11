mod common;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Europe::Copenhagen;
use common::{status, Fake};
use lucina_parcels::{tick, Home, MessageKind, ParcelState, ScheduleCfg, Store, TrackError};

/// 2026-09-D HH:MM UTC. Copenhagen is UTC+2 then: 09:00 local is 07:00 UTC.
fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, d, h, m, 0).unwrap()
}

fn cfg() -> ScheduleCfg {
    ScheduleCfg::new(Copenhagen, 9, Home::default())
}

fn new_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("parcels.json"));
    (dir, s)
}

fn ok(name: &str) -> Result<lucina_parcels::Status, TrackError> {
    Ok(status(name))
}

#[tokio::test]
async fn two_parcels_get_one_digest_a_day_after_the_digest_hour() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    for n in ["AAA1", "BBB2"] {
        store.add(n, Some("postnord"), None, at(27, 12, 0));
        fake.script(n, vec![ok("postnord_in_transit_far")]);
    }
    assert!(
        tick(&store, &fake, at(28, 6, 30), &cfg()).await.is_empty(),
        "08:30 local"
    );
    assert_eq!(fake.count(), 0);

    let m = tick(&store, &fake, at(28, 7, 0), &cfg()).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].kind, MessageKind::Digest);
    assert_eq!(m[0].dedup_key, "parcel-digest:2026-09-28");
    assert!(
        m[0].text.contains("AAA1") && m[0].text.contains("BBB2"),
        "{}",
        m[0].text
    );
    assert_eq!(fake.count(), 2);

    assert!(tick(&store, &fake, at(28, 8, 0), &cfg()).await.is_empty());
    assert!(tick(&store, &fake, at(28, 20, 0), &cfg()).await.is_empty());
    assert_eq!(fake.count(), 2);

    let m = tick(&store, &fake, at(29, 7, 5), &cfg()).await;
    assert_eq!(
        (m.len(), m[0].dedup_key.as_str()),
        (1, "parcel-digest:2026-09-29")
    );
    assert_eq!(fake.count(), 4);
}

#[tokio::test]
async fn a_parcel_out_for_delivery_is_checked_hourly_until_it_arrives() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    let id = store
        .add("CCC3", Some("postnord"), Some("Shoes"), at(27, 12, 0))
        .parcel
        .id;
    let mut later = status("postnord_out_for_delivery");
    later.text = Some("Handed to the driver".into());
    later.last_update = Some("2026-09-28T07:40:00Z".into());
    fake.script(
        "CCC3",
        vec![
            ok("postnord_out_for_delivery"),
            ok("postnord_out_for_delivery"),
            Ok(later),
            ok("postnord_delivered"),
        ],
    );

    let m = tick(&store, &fake, at(28, 7, 0), &cfg()).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].kind, MessageKind::Digest);
    assert!(m[0].text.contains("checking hourly now"), "{}", m[0].text);
    assert_eq!(fake.count(), 1);

    assert!(tick(&store, &fake, at(28, 7, 20), &cfg()).await.is_empty());
    assert_eq!(fake.count(), 1, "20 minutes later: not due");

    assert!(
        tick(&store, &fake, at(28, 8, 0), &cfg()).await.is_empty(),
        "same status: no message"
    );
    assert_eq!(fake.count(), 2);

    let m = tick(&store, &fake, at(28, 9, 0), &cfg()).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].kind, MessageKind::Change);
    assert!(
        m[0].text.starts_with("📦 Shoes: Handed to the driver"),
        "{}",
        m[0].text
    );
    assert_eq!(
        m[0].dedup_key,
        format!("parcel:{id}:out_for_delivery:2026-09-28T07:40:00Z")
    );
    assert_eq!(fake.count(), 3);

    let m = tick(&store, &fake, at(28, 10, 0), &cfg()).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].kind, MessageKind::Final);
    assert!(
        m[0].text.contains("Shoes was delivered") && m[0].text.contains("I've stopped tracking it"),
        "{}",
        m[0].text
    );
    assert_eq!(fake.count(), 4);
    let p = store.get(&id).unwrap();
    assert_eq!((p.state, p.final_sent), (ParcelState::Delivered, true));

    for when in [at(28, 11, 0), at(28, 23, 0), at(29, 7, 0), at(30, 7, 0)] {
        assert!(tick(&store, &fake, when, &cfg()).await.is_empty());
    }
    assert_eq!(fake.count(), 4, "never fetched again");
}

#[tokio::test]
async fn the_daily_pass_finding_a_delivered_parcel_puts_it_in_the_digest_only() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    let id = store
        .add("DDD4", Some("postnord"), None, at(27, 12, 0))
        .parcel
        .id;
    store.add("EEE5", Some("postnord"), None, at(27, 12, 0));
    fake.script("DDD4", vec![ok("postnord_delivered")]);
    fake.script("EEE5", vec![ok("postnord_in_transit_far")]);

    let m = tick(&store, &fake, at(28, 7, 0), &cfg()).await;
    assert_eq!(m.len(), 1, "no separate final message");
    assert_eq!(m[0].kind, MessageKind::Digest);
    assert!(
        m[0].text.contains("DDD4") && m[0].text.contains("EEE5"),
        "{}",
        m[0].text
    );
    assert!(
        m[0].text.contains("I've stopped tracking it"),
        "{}",
        m[0].text
    );
    let p = store.get(&id).unwrap();
    assert_eq!((p.state, p.final_sent), (ParcelState::Delivered, true));

    tick(&store, &fake, at(29, 7, 0), &cfg()).await;
    assert_eq!(
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == "DDD4")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_failed_hourly_lookup_sends_nothing_and_keeps_the_parcel() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    let id = store
        .add("FFF6", Some("postnord"), None, at(27, 12, 0))
        .parcel
        .id;
    store.update(&id, |p| {
        p.status = Some(status("postnord_out_for_delivery"));
        p.last_checked = Some(at(28, 5, 0));
    });
    store.set_digest_sent("2026-09-28");
    fake.script(
        "FFF6",
        vec![
            Err(TrackError::Unavailable("timeout".into())),
            Err(TrackError::NotFound),
        ],
    );

    assert!(tick(&store, &fake, at(28, 7, 0), &cfg()).await.is_empty());
    let p = store.get(&id).unwrap();
    assert_eq!(p.state, ParcelState::Active);
    assert_eq!(p.last_checked, Some(at(28, 7, 0)));
    assert_eq!(p.last_error.as_deref(), Some("couldn't check (timeout)"));
    assert_eq!(
        p.status,
        Some(status("postnord_out_for_delivery")),
        "the old status stays"
    );

    assert!(tick(&store, &fake, at(28, 8, 0), &cfg()).await.is_empty());
    assert_eq!(
        store.get(&id).unwrap().last_error.as_deref(),
        Some("the carrier has no record of it yet")
    );
    assert_eq!(fake.count(), 2);
}

#[tokio::test]
async fn a_failed_daily_lookup_is_a_line_in_the_digest() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    store.add("GGG7", None, None, at(27, 12, 0));
    fake.script(
        "GGG7",
        vec![Err(TrackError::InputRequired("postcode".into()))],
    );
    let m = tick(&store, &fake, at(28, 7, 0), &cfg()).await;
    assert!(
        m[0].text
            .contains("couldn't check (the carrier needs the postcode)"),
        "{}",
        m[0].text
    );
    assert_eq!(store.list().len(), 1);
}

#[tokio::test]
async fn a_parcel_with_no_change_for_45_days_is_dropped() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    let stale = store
        .add("HHH8", Some("postnord"), Some("Old"), at(1, 0, 0))
        .parcel
        .id;
    let fresh = store
        .add("III9", Some("postnord"), None, at(1, 0, 0))
        .parcel
        .id;
    store.update(&fresh, |p| {
        p.last_change = Some(at(28, 0, 0) - chrono::Duration::days(2))
    });
    fake.script("III9", vec![ok("postnord_in_transit_far")]);

    // 45 days after the 1st of September is the 16th of October; Copenhagen is UTC+2 until the 25th.
    let now = Utc.with_ymd_and_hms(2026, 10, 16, 7, 0, 0).unwrap();
    store.update(&fresh, |p| {
        p.last_change = Some(now - chrono::Duration::days(2))
    });
    let m = tick(&store, &fake, now, &cfg()).await;
    assert_eq!(m.len(), 1);
    assert!(
        m[0].text
            .contains("Old: no change in 45 days, so I've stopped tracking it."),
        "{}",
        m[0].text
    );
    assert_eq!(store.get(&stale).unwrap().state, ParcelState::Stopped);
    assert_eq!(store.get(&fresh).unwrap().state, ParcelState::Active);
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        ["III9"],
        "the stale one was not fetched"
    );

    // A day earlier it would still have been followed.
    let (_d2, store2) = new_store();
    store2.add("HHH8", Some("postnord"), None, at(1, 0, 0));
    fake.script("HHH8", vec![ok("postnord_in_transit_far")]);
    tick(&store2, &fake, now - chrono::Duration::days(1), &cfg()).await;
    assert_eq!(store2.list()[0].state, ParcelState::Active);
}

#[tokio::test]
async fn no_active_parcel_means_no_digest() {
    let (_d, store) = new_store();
    let fake = Fake::default();
    assert!(tick(&store, &fake, at(28, 7, 0), &cfg()).await.is_empty());
    assert_eq!(store.digest_sent().as_deref(), Some("2026-09-28"));
}
