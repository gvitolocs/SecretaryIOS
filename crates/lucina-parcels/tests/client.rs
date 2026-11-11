mod common;

use common::{fixture, status};
use lucina_parcels::client::{parse_error, parse_status};
use lucina_parcels::{Detection, Stage, TrackError};
use serde_json::json;

#[test]
fn every_status_fixture_parses() {
    let s = status("postnord_delivered");
    assert_eq!(
        (s.carrier.as_str(), s.stage),
        ("postnord", Stage::Delivered)
    );
    assert_eq!(s.location.as_deref(), Some("AARHUS DEPOT"));
    assert_eq!(s.last_update.as_deref(), Some("2026-09-28T08:23:30Z"));
    assert_eq!(s.destination_country.as_deref(), Some("DK"));
    assert_eq!(s.expected_delivery, None);

    let s = status("postnord_out_for_delivery");
    assert_eq!(s.stage, Stage::OutForDelivery);
    assert_eq!(
        s.text.as_deref(),
        Some("The delivery of the shipment item is in progress.")
    );
    assert_eq!(s.location.as_deref(), Some("AARHUS DEPOT"));

    let s = status("postnord_in_transit_local");
    assert_eq!(
        (s.stage, s.location.as_deref()),
        (Stage::InTransit, Some("AARHUS DEPOT"))
    );
    assert_eq!(
        s.text.as_deref(),
        Some("The shipment item has arrived at the distribution terminal.")
    );

    let s = status("postnord_in_transit_far");
    assert_eq!(
        (s.stage, s.location.as_deref()),
        (Stage::InTransit, Some("TAASTRUP TERMINAL"))
    );

    let s = status("gls_ready_for_pickup");
    assert_eq!(
        (s.carrier.as_str(), s.stage),
        ("gls", Stage::ReadyForPickup)
    );

    let s = status("dhl_expected_today");
    assert_eq!((s.carrier.as_str(), s.stage), ("dhl", Stage::InTransit));
    assert_eq!(s.expected_delivery.as_deref(), Some("2026-09-29"));
}

#[test]
fn the_newest_event_is_the_one_with_the_largest_instant() {
    let mut body = fixture("postnord_delivered");
    let events = body["result"]["events"].as_array_mut().unwrap();
    events.reverse();
    assert_eq!(
        parse_status(&body).unwrap().location.as_deref(),
        Some("AARHUS DEPOT")
    );
    // No instants: the first event.
    for e in body["result"]["events"].as_array_mut().unwrap() {
        e.as_object_mut().unwrap().remove("instant");
    }
    assert_eq!(
        parse_status(&body).unwrap().location.as_deref(),
        Some("PostNord")
    );
}

#[test]
fn an_unknown_stage_is_unknown_and_a_body_without_result_is_unavailable() {
    let mut body = fixture("postnord_delivered");
    body["result"]["current_stage"] = json!("teleported");
    assert_eq!(parse_status(&body).unwrap().stage, Stage::Unknown);
    assert!(matches!(
        parse_status(&json!({"carrier": "x"})),
        Err(TrackError::Unavailable(_))
    ));
}

#[test]
fn error_bodies_map_to_track_errors() {
    assert_eq!(
        parse_error(404, &fixture("error_not_found")),
        TrackError::NotFound
    );
    assert_eq!(
        parse_error(400, &fixture("error_input_required")),
        TrackError::InputRequired("postcode".into())
    );
    assert_eq!(
        parse_error(
            400,
            &json!({"error": "bad", "hint": {"kind": "invalid_input"}})
        ),
        TrackError::Invalid
    );
    for kind in [
        "rate_limited",
        "challenge",
        "maintenance",
        "transport",
        "budget",
        "indeterminate",
        "schema",
    ] {
        let e = parse_error(502, &json!({"error": "boom", "hint": {"kind": kind}}));
        assert_eq!(e, TrackError::Unavailable("boom".into()), "{kind}");
    }
    assert_eq!(
        parse_error(500, &json!(null)),
        TrackError::Unavailable("HTTP 500".into())
    );
}

#[test]
fn a_detection_is_read_from_the_scraper_answer() {
    let d: Detection = serde_json::from_value(json!({
        "trackingNumber": "3UW1S8IAAKA23", "source": "number", "carrier": "poste-italiane",
        "confidence": "high", "candidates": ["poste-italiane"], "preferred": []
    }))
    .unwrap();
    assert_eq!(
        (d.found(), d.sure_carrier()),
        (Some("3UW1S8IAAKA23"), Some("poste-italiane"))
    );
    let d: Detection = serde_json::from_value(json!({
        "trackingNumber": "123", "source": "number", "carrier": "dhl", "confidence": "low", "candidates": ["dhl", "gls"]
    }))
    .unwrap();
    assert_eq!((d.found(), d.sure_carrier()), (Some("123"), None));
    let d: Detection =
        serde_json::from_value(json!({"source": "none", "confidence": "none", "candidates": []}))
            .unwrap();
    assert_eq!(d.found(), None);
}
