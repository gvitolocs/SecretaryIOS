mod common;

use chrono::NaiveDate;
use chrono_tz::Europe::Copenhagen;
use common::status;
use lucina_parcels::policy::{cadence, changed, near_destination};
use lucina_parcels::{Cadence, Home, Stage, Status};

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn aarhus() -> Home {
    Home {
        city: Some("Aarhus".into()),
    }
}

fn near(s: &Status, today: &str, home: &Home) -> bool {
    near_destination(s, day(today), Copenhagen, home)
}

#[test]
fn the_last_stages_are_near() {
    let none = Home::default();
    assert!(near(
        &status("postnord_out_for_delivery"),
        "2026-09-28",
        &none
    ));
    assert!(near(&status("gls_ready_for_pickup"), "2026-09-28", &none));
    let mut s = status("postnord_in_transit_far");
    s.stage = Stage::FailedAttempt;
    assert!(near(&s, "2026-09-28", &none));
}

#[test]
fn an_expected_delivery_today_is_near_until_it_arrives() {
    let none = Home::default();
    let s = status("dhl_expected_today");
    assert!(near(&s, "2026-09-29", &none));
    assert!(!near(&s, "2026-09-28", &none), "expected tomorrow");
    let mut done = s.clone();
    done.stage = Stage::Delivered;
    assert!(!near(&done, "2026-09-29", &none));
    done.stage = Stage::Returned;
    assert!(!near(&done, "2026-09-29", &none));
}

#[test]
fn an_instant_is_read_in_the_local_zone() {
    let mut s = status("dhl_expected_today");
    // 22:30 UTC on the 28th is 00:30 on the 29th in Copenhagen (summer time).
    s.expected_delivery = Some("2026-09-28T22:30:00Z".into());
    assert!(near(&s, "2026-09-29", &Home::default()));
    assert!(!near(&s, "2026-09-28", &Home::default()));
    s.expected_delivery = Some("sometime".into());
    assert!(!near(&s, "2026-09-29", &Home::default()));
}

#[test]
fn in_transit_is_near_only_at_the_home_city() {
    let local = status("postnord_in_transit_local");
    assert!(near(&local, "2026-09-28", &aarhus()));
    assert!(
        !near(&local, "2026-09-28", &Home::default()),
        "no home city configured"
    );
    assert!(!near(
        &local,
        "2026-09-28",
        &Home {
            city: Some("  ".into())
        }
    ));
    assert!(!near(
        &status("postnord_in_transit_far"),
        "2026-09-28",
        &aarhus()
    ));
    let mut s = local;
    s.location = Some("AARHUSVEJ 12".into());
    assert!(!near(&s, "2026-09-28", &aarhus()), "not a whole word");
    s.location = Some("Depot, aarhus C".into());
    assert!(near(&s, "2026-09-28", &aarhus()));
    s.stage = Stage::Customs;
    assert!(!near(&s, "2026-09-28", &aarhus()), "only in_transit counts");
}

#[test]
fn delivered_and_returned_are_never_near() {
    let mut s = status("postnord_delivered");
    assert!(!near(&s, "2026-09-28", &aarhus()));
    s.stage = Stage::Returned;
    assert!(!near(&s, "2026-09-28", &aarhus()));
}

#[test]
fn the_cadence_follows_the_status() {
    let t = day("2026-09-28");
    let c = |s: Option<&Status>, home: &Home| cadence(s, t, Copenhagen, home);
    let none = Home::default();
    assert_eq!(c(None, &none), Cadence::Daily);
    assert_eq!(
        c(Some(&status("postnord_in_transit_far")), &aarhus()),
        Cadence::Daily
    );
    assert_eq!(
        c(Some(&status("postnord_in_transit_local")), &aarhus()),
        Cadence::Hourly
    );
    assert_eq!(
        c(Some(&status("postnord_in_transit_local")), &none),
        Cadence::Daily
    );
    assert_eq!(
        c(Some(&status("postnord_out_for_delivery")), &none),
        Cadence::Hourly
    );
    assert_eq!(
        c(Some(&status("gls_ready_for_pickup")), &none),
        Cadence::Hourly
    );
    assert_eq!(
        c(Some(&status("postnord_delivered")), &none),
        Cadence::Stopped
    );
    let mut r = status("postnord_delivered");
    r.stage = Stage::Returned;
    assert_eq!(c(Some(&r), &none), Cadence::Stopped);
}

#[test]
fn a_change_is_a_new_stage_text_or_update() {
    let a = status("postnord_out_for_delivery");
    assert!(changed(None, &a));
    assert!(!changed(Some(&a), &a.clone()));
    for edit in [
        (|s: &mut Status| s.stage = Stage::Delivered) as fn(&mut Status),
        |s| s.text = Some("other".into()),
        |s| s.last_update = Some("2026-09-28T09:00:00Z".into()),
    ] {
        let mut b = a.clone();
        edit(&mut b);
        assert!(changed(Some(&a), &b));
    }
    let mut b = a.clone();
    b.location = Some("elsewhere".into());
    assert!(!changed(Some(&a), &b), "a location alone is not news");
}
