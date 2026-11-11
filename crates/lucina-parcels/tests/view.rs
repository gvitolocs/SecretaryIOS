mod common;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Europe::Copenhagen;
use common::{fixture, status};
use lucina_parcels::client::parse_status;
use lucina_parcels::{next_check, ordered, view, Home, ParcelState, ScheduleCfg, Stage, Store};
use serde_json::{json, Value};

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn cfg(hour: u32) -> ScheduleCfg {
    ScheduleCfg::new(Copenhagen, hour, Home::default())
}

fn parcel_with(fixture_name: &str, checked: Option<DateTime<Utc>>) -> lucina_parcels::Parcel {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let added = store.add("PN123", Some("postnord"), Some("Headphones"), utc(2026, 9, 25, 12, 0));
    store
        .update(&added.parcel.id, |p| {
            p.status = Some(status(fixture_name));
            p.last_checked = checked;
        })
        .unwrap()
}

#[test]
fn events_are_newest_first_whatever_the_order_they_came_in() {
    let s = status("postnord_delivered");
    assert_eq!(s.events.len(), 6);
    assert_eq!(s.events[0].stage, Stage::Delivered);
    assert_eq!(s.events[0].description, "The shipment item has been delivered.");
    assert_eq!(s.events[0].time.as_deref(), Some("2026-09-28T08:23:30Z"));
    assert_eq!(s.events[0].location.as_deref(), Some("AARHUS DEPOT"));
    assert_eq!(s.events[5].stage, Stage::Registered);

    let mut body = fixture("postnord_delivered");
    body["result"]["events"].as_array_mut().unwrap().reverse();
    assert_eq!(parse_status(&body).unwrap().events, s.events);
}

#[test]
fn at_most_fifty_events_are_kept_and_they_are_the_newest() {
    let mut body = fixture("postnord_delivered");
    let events: Vec<Value> = (0..60)
        .map(|i| {
            let t = utc(2026, 9, 1, 0, 0) + chrono::Duration::hours(i);
            let t = t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            json!({"time": t, "description": format!("scan {i}"), "location": null, "stage": "in_transit", "instant": t})
        })
        .collect();
    body["result"]["events"] = Value::Array(events);
    let s = parse_status(&body).unwrap();
    assert_eq!(s.events.len(), 50);
    assert_eq!(s.events[0].description, "scan 59");
    assert_eq!(s.events[49].description, "scan 10");
}

#[test]
fn a_list_saved_before_events_were_kept_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parcels.json");
    let store = Store::open(&path);
    let p = store.add("PN123", None, None, utc(2026, 9, 25, 12, 0)).parcel;
    store.update(&p.id, |q| q.status = Some(status("postnord_delivered")));
    let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["parcels"][0]["status"].as_object_mut().unwrap().remove("events").unwrap();
    std::fs::write(&path, raw.to_string()).unwrap();
    let loaded = Store::open(&path).get_id(&p.id).unwrap();
    assert!(loaded.status.unwrap().events.is_empty());
}

#[test]
fn an_hourly_parcel_is_next_checked_an_hour_after_its_last_check() {
    let now = utc(2026, 9, 28, 8, 0);
    let p = parcel_with("postnord_out_for_delivery", Some(utc(2026, 9, 28, 7, 30)));
    assert_eq!(next_check(&p, now, &cfg(9), Some("2026-09-28")), Some(utc(2026, 9, 28, 8, 30)));
    let never = parcel_with("postnord_out_for_delivery", None);
    assert_eq!(next_check(&never, now, &cfg(9), None), Some(now));
}

#[test]
fn a_daily_parcel_is_next_checked_at_the_next_digest_hour() {
    let p = parcel_with("postnord_in_transit_far", Some(utc(2026, 9, 28, 7, 0)));
    let c = cfg(9);
    // 06:00 local, digest still to come today: 09:00 local is 07:00Z.
    assert_eq!(next_check(&p, utc(2026, 9, 28, 4, 0), &c, Some("2026-09-27")), Some(utc(2026, 9, 28, 7, 0)));
    // Past the hour and the digest is unsent: due on the next tick.
    let now = utc(2026, 9, 28, 10, 0);
    assert_eq!(next_check(&p, now, &c, Some("2026-09-27")), Some(now));
    // Sent today: tomorrow morning.
    assert_eq!(next_check(&p, now, &c, Some("2026-09-28")), Some(utc(2026, 9, 29, 7, 0)));
    // Late evening local (23:30 on the 28th) with today's digest sent.
    assert_eq!(next_check(&p, utc(2026, 9, 28, 21, 30), &c, Some("2026-09-28")), Some(utc(2026, 9, 29, 7, 0)));
}

#[test]
fn the_next_daily_instant_is_right_across_the_clock_change() {
    let p = parcel_with("postnord_in_transit_far", None);
    // Spring forward: 2027-03-28, 02:00 CET becomes 03:00 CEST (01:00Z).
    let evening = utc(2027, 3, 27, 20, 0);
    assert_eq!(next_check(&p, evening, &cfg(9), Some("2027-03-27")), Some(utc(2027, 3, 28, 7, 0)), "09:00 CEST");
    assert_eq!(next_check(&p, evening, &cfg(2), Some("2027-03-27")), Some(utc(2027, 3, 28, 1, 0)), "02:00 does not exist: 03:00 CEST");
    // Fall back: 2026-10-25, 03:00 CEST becomes 02:00 CET.
    let evening = utc(2026, 10, 24, 20, 0);
    assert_eq!(next_check(&p, evening, &cfg(9), Some("2026-10-24")), Some(utc(2026, 10, 25, 8, 0)), "09:00 CET");
}

#[test]
fn a_finished_parcel_has_no_next_check() {
    let mut p = parcel_with("postnord_delivered", Some(utc(2026, 9, 28, 8, 30)));
    p.state = ParcelState::Delivered;
    assert_eq!(next_check(&p, utc(2026, 9, 28, 9, 0), &cfg(9), None), None);
    let mut q = parcel_with("postnord_in_transit_far", None);
    q.state = ParcelState::Stopped;
    assert_eq!(next_check(&q, utc(2026, 9, 28, 9, 0), &cfg(9), None), None);
}

#[test]
fn the_view_is_the_contract_parcel() {
    let now = utc(2026, 9, 28, 8, 0);
    let p = parcel_with("postnord_out_for_delivery", Some(utc(2026, 9, 28, 7, 30)));
    let v = view(&p, now, &cfg(9), Some("2026-09-28"));
    assert_eq!(
        v,
        json!({
            "id": p.id,
            "number": "PN123",
            "carrier": "postnord",
            "label": "Headphones",
            "state": "active",
            "schedule": "hourly",
            "near": true,
            "addedAt": "2026-09-25T12:00:00Z",
            "lastChecked": "2026-09-28T07:30:00Z",
            "nextCheck": "2026-09-28T08:30:00Z",
            "lastError": null,
            "status": {
                "stage": "out_for_delivery",
                "text": "The delivery of the shipment item is in progress.",
                "lastUpdate": "2026-09-28T05:33:46Z",
                "expectedDelivery": null,
                "location": "AARHUS DEPOT",
                "destinationCountry": "DK"
            },
            "events": v["events"].clone(),
        })
    );
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(
        events[0],
        json!({"time": "2026-09-28T05:33:46Z", "description": "The delivery of the shipment item is in progress.", "location": "AARHUS DEPOT", "stage": "out_for_delivery"})
    );
}

#[test]
fn a_parcel_never_checked_has_nulls_and_no_events() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let p = store.add("zz 9999", None, None, utc(2026, 9, 25, 12, 0)).parcel;
    let v = view(&p, utc(2026, 9, 25, 12, 1), &cfg(9), None);
    assert_eq!(v["carrier"], Value::Null);
    assert_eq!(v["label"], Value::Null);
    assert_eq!(v["lastChecked"], Value::Null);
    assert_eq!(v["status"], Value::Null);
    assert_eq!(v["events"], json!([]));
    assert_eq!((v["schedule"].as_str(), v["near"].as_bool()), (Some("daily"), Some(false)));
    assert_eq!(v["nextCheck"], "2026-09-25T12:01:00Z", "past 09:00 local with no digest yet: due on the next tick");
}

#[test]
fn a_delivered_parcel_is_stopped_and_not_near() {
    let mut p = parcel_with("postnord_delivered", Some(utc(2026, 9, 28, 8, 30)));
    p.state = ParcelState::Delivered;
    let v = view(&p, utc(2026, 9, 28, 9, 0), &cfg(9), None);
    assert_eq!(
        (v["state"].as_str(), v["schedule"].as_str(), v["near"].as_bool()),
        (Some("delivered"), Some("stopped"), Some(false))
    );
    assert_eq!(v["nextCheck"], Value::Null);
    assert_eq!(v["status"]["stage"], "delivered");
}

#[test]
fn the_list_has_active_parcels_first_and_the_newest_change_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("p.json"));
    let ids: Vec<String> = ["AAAA1", "BBBB2", "CCCC3"]
        .iter()
        .enumerate()
        .map(|(i, n)| store.add(n, None, None, utc(2026, 9, 25, 12, i as u32)).parcel.id)
        .collect();
    store.update(&ids[0], |p| p.last_change = Some(utc(2026, 9, 27, 0, 0)));
    store.update(&ids[1], |p| p.state = ParcelState::Delivered);
    store.update(&ids[2], |p| p.last_change = Some(utc(2026, 9, 28, 0, 0)));
    let order: Vec<String> = ordered(store.list()).into_iter().map(|p| p.id).collect();
    assert_eq!(order, vec![ids[2].clone(), ids[0].clone(), ids[1].clone()]);
}
