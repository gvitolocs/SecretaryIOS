use secretary_body::{stats, units, Engine, Response};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use uuid::Uuid;

const NOW: u64 = 1_792_584_000_000; // Wed 2026-10-21 12:00 UTC
const DAY: u64 = 86_400_000;

fn request(engine: &Engine, op: &str, command: Value) -> String {
    json!({"op_id":op,"expected_revision":engine.state().revision,"timezone":"Europe/Rome","command":command})
        .to_string()
}
fn send_at(engine: &mut Engine, command: Value, now: u64) -> Response {
    let req = request(engine, &Uuid::new_v4().to_string(), command);
    engine.handle_at(&req, now)
}
fn send(engine: &mut Engine, command: Value) -> Response {
    send_at(engine, command, NOW)
}
fn ok(engine: &mut Engine, command: Value) -> Response {
    let r = send(engine, command);
    assert!(r.ok, "{:?}", r.error);
    r
}
fn log(engine: &mut Engine, grams: u64, measured_ms: u64) -> String {
    ok(
        engine,
        json!({"type":"log_entry","weight_grams":grams,"measured_ms":measured_ms}),
    );
    engine
        .state()
        .entries
        .iter()
        .find(|e| e.measured_ms == measured_ms && e.weight_grams == grams)
        .unwrap()
        .id
        .clone()
}
fn snapshot(engine: &mut Engine, now: u64) -> secretary_body::View {
    engine
        .handle_at(&json!({"command":{"type":"snapshot"}}).to_string(), now)
        .view
        .unwrap()
}
fn temp() -> PathBuf {
    std::env::temp_dir().join(format!("secretary-body-{}", Uuid::new_v4()))
}

#[test]
fn log_edit_delete_roundtrip() {
    let mut e = Engine::in_memory();
    let r = ok(
        &mut e,
        json!({"type":"log_entry","weight_grams":80_500,"note":"  after run "}),
    );
    let view = r.view.unwrap();
    assert_eq!(view.revision, 1);
    assert_eq!(view.entries.len(), 1);
    let entry = view.entries[0].clone();
    assert_eq!(
        (entry.weight_grams, entry.measured_ms, entry.note.as_str()),
        (80_500, NOW, "after run")
    );
    assert_eq!((entry.created_ms, entry.updated_ms), (NOW, NOW));
    assert_eq!(view.labels[&entry.id], "80.5 kg");

    let later = NOW + 60_000;
    let r = send_at(
        &mut e,
        json!({"type":"edit_entry","entry_id":entry.id,"weight_grams":80_100}),
        later,
    );
    assert!(r.ok, "{:?}", r.error);
    let edited = &e.state().entries[0];
    assert_eq!(
        (
            edited.weight_grams,
            edited.measured_ms,
            edited.note.as_str()
        ),
        (80_100, NOW, "after run")
    );
    assert_eq!((edited.created_ms, edited.updated_ms), (NOW, later));
    ok(
        &mut e,
        json!({"type":"edit_entry","entry_id":entry.id,"note":"","measured_ms":NOW - DAY}),
    );
    assert_eq!(e.state().entries[0].note, "");
    assert_eq!(e.state().entries[0].measured_ms, NOW - DAY);
    ok(&mut e, json!({"type":"delete_entry","entry_id":entry.id}));
    assert!(e.state().entries.is_empty());
    assert!(!send(&mut e, json!({"type":"delete_entry","entry_id":entry.id})).ok);
    assert_eq!(e.state().revision, 4);
}

#[test]
fn weight_strings_parse_in_the_display_unit() {
    let mut e = Engine::in_memory();
    ok(&mut e, json!({"type":"log_entry","weight":"80,5"}));
    assert_eq!(e.state().entries[0].weight_grams, 80_500);
    ok(
        &mut e,
        json!({"type":"update_settings","unit":"lb","goal":"170"}),
    );
    assert_eq!(e.state().settings.goal_grams, Some(77_111));
    ok(
        &mut e,
        json!({"type":"log_entry","weight":"176.5","measured_ms":NOW - 1}),
    );
    let grams: Vec<u64> = e.state().entries.iter().map(|x| x.weight_grams).collect();
    assert_eq!(grams, vec![80_500, 80_059]);
    let view = snapshot(&mut e, NOW);
    assert_eq!(view.labels[&view.entries[1].id], "176.5 lb");
    assert_eq!(view.stat_labels["goal"], "170.0 lb");
    ok(
        &mut e,
        json!({"type":"update_settings","unit":"kg","goal_grams":null}),
    );
    assert_eq!(e.state().settings.goal_grams, None);
}

#[test]
fn parse_weight_query_needs_no_op_id_and_changes_nothing() {
    let mut e = Engine::in_memory();
    let q = |v: Value| json!({"command":v}).to_string();
    let r = e.handle_at(&q(json!({"type":"parse_weight","value":"80.5"})), NOW);
    assert!(r.ok && r.view.is_none());
    assert_eq!(r.weight_grams, Some(80_500));
    let r = e.handle_at(
        &q(json!({"type":"parse_weight","value":"176,5","unit":"lb"})),
        NOW,
    );
    assert_eq!(r.weight_grams, Some(80_059));
    for bad in [
        json!({"type":"parse_weight","value":"-1"}),
        json!({"type":"parse_weight","value":"1.2345"}),
        json!({"type":"parse_weight","value":"80","unit":"stone"}),
    ] {
        assert_eq!(
            e.handle_at(&q(bad), NOW).code.as_deref(),
            Some("invalid_command")
        );
    }
    assert_eq!(e.state().revision, 0);
}

#[test]
fn exactly_one_weight_field_and_nonempty_edits() {
    let mut e = Engine::in_memory();
    let id = log(&mut e, 80_000, NOW);
    let before = serde_json::to_string(e.state()).unwrap();
    for bad in [
        json!({"type":"log_entry"}),
        json!({"type":"log_entry","weight":"80","weight_grams":80_000}),
        json!({"type":"edit_entry","entry_id":id}),
        json!({"type":"edit_entry","entry_id":id,"weight":"80","weight_grams":80_000}),
        json!({"type":"edit_entry","entry_id":"missing","note":"x"}),
        json!({"type":"update_settings","unit":"kg","goal":"70","goal_grams":70_000}),
        json!({"type":"update_settings","unit":"stone"}),
        json!({"type":"no_such_command"}),
    ] {
        let r = send(&mut e, bad.clone());
        assert_eq!(r.code.as_deref(), Some("invalid_command"), "{bad}");
    }
    assert_eq!(before, serde_json::to_string(e.state()).unwrap());
}

#[test]
fn bounds_on_weight_goal_note_and_time() {
    let mut e = Engine::in_memory();
    assert!(!send(&mut e, json!({"type":"log_entry","weight_grams":999})).ok);
    assert!(!send(&mut e, json!({"type":"log_entry","weight_grams":650_001})).ok);
    assert!(send(&mut e, json!({"type":"log_entry","weight_grams":1_000})).ok);
    assert!(send(&mut e, json!({"type":"log_entry","weight_grams":650_000})).ok);
    let long = "x".repeat(1001);
    assert!(
        !send(
            &mut e,
            json!({"type":"log_entry","weight_grams":80_000,"note":long})
        )
        .ok
    );
    assert!(
        send(
            &mut e,
            json!({"type":"log_entry","weight_grams":80_000,"note":"x".repeat(1000)})
        )
        .ok
    );
    assert!(
        !send(
            &mut e,
            json!({"type":"log_entry","weight_grams":80_000,"measured_ms":NOW + DAY + 1})
        )
        .ok
    );
    assert!(
        send(
            &mut e,
            json!({"type":"log_entry","weight_grams":80_000,"measured_ms":NOW + DAY})
        )
        .ok
    );
    assert!(
        !send(
            &mut e,
            json!({"type":"update_settings","unit":"kg","goal_grams":999})
        )
        .ok
    );
    assert!(
        !send(
            &mut e,
            json!({"type":"update_settings","unit":"kg","goal":"651"})
        )
        .ok
    );
    assert_eq!(e.state().entries.len(), 4);
    let r = e.handle_at(
        &json!({"timezone":"Mars/Olympus","command":{"type":"snapshot"}}).to_string(),
        NOW,
    );
    assert_eq!(r.code.as_deref(), Some("invalid_command"));
}

#[test]
fn op_id_replay_is_idempotent_even_after_the_revision_moves_on() {
    let mut e = Engine::in_memory();
    let first = request(
        &e,
        "log-1",
        json!({"type":"log_entry","weight_grams":80_000}),
    );
    let r = e.handle_at(&first, NOW);
    assert!(r.ok && !r.replayed);
    let r = e.handle_at(&first, NOW + 5);
    assert!(r.ok && r.replayed);
    assert_eq!(r.view.unwrap().entries.len(), 1);
    ok(&mut e, json!({"type":"log_entry","weight_grams":79_000}));
    let rev = e.state().revision;
    // replay of the first command with its stale expected_revision
    let r = e.handle_at(&first, NOW + 10);
    assert!(r.ok && r.replayed, "{:?}", r.error);
    assert_eq!((e.state().revision, e.state().entries.len()), (rev, 2));

    let mut reused: Value = serde_json::from_str(&first).unwrap();
    reused["command"]["weight_grams"] = json!(81_000);
    reused["expected_revision"] = json!(rev);
    let r = e.handle_at(&reused.to_string(), NOW);
    assert_eq!(r.code.as_deref(), Some("op_id_reused"));
    let stale = request(
        &e,
        "log-3",
        json!({"type":"log_entry","weight_grams":78_000}),
    );
    let mut stale: Value = serde_json::from_str(&stale).unwrap();
    stale["expected_revision"] = json!(rev - 1);
    let r = e.handle_at(&stale.to_string(), NOW);
    assert_eq!(r.code.as_deref(), Some("conflict"));
    assert!(!r.ok && r.view.is_none());
    let missing_op =
        json!({"expected_revision":rev,"command":{"type":"log_entry","weight_grams":78_000}});
    assert_eq!(
        e.handle_at(&missing_op.to_string(), NOW).code.as_deref(),
        Some("invalid_command")
    );
    assert_eq!((e.state().revision, e.state().entries.len()), (rev, 2));
}

#[test]
fn durable_state_and_receipts_survive_restart_and_writer_lock_is_exclusive() {
    let dir = temp();
    let path = dir.join("body.json");
    let mut e = Engine::open(&path).unwrap();
    let req = request(
        &e,
        "persisted",
        json!({"type":"log_entry","weight_grams":80_000}),
    );
    assert!(e.handle_at(&req, NOW).ok);
    assert!(Engine::open(&path).is_err());
    let expected = e.state().clone();
    drop(e);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let mut e = Engine::open(&path).unwrap();
    assert_eq!(e.state(), &expected);
    let r = e.handle_at(&req, NOW + 1);
    assert!(r.ok && r.replayed);
    assert_eq!(e.state().entries.len(), 1);
    drop(e);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_store_is_not_overwritten_and_storage_failure_does_not_commit() {
    let dir = temp();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("body.json");
    fs::write(&path, b"broken").unwrap();
    assert!(Engine::open(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"broken");
    fs::remove_dir_all(&dir).unwrap();

    let mut e = Engine::open(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    let old = e.state().clone();
    let r = send(&mut e, json!({"type":"log_entry","weight_grams":80_000}));
    assert_eq!(r.code.as_deref(), Some("storage_error"));
    assert_eq!(e.state(), &old);
}

/// Hand-computed fixture (grams, days before NOW):
/// 82000@-20, 81000@-10, 81400@-9, 80600@-3, 80400@-1, 80500@0.
fn fixture() -> Engine {
    let mut e = Engine::in_memory();
    for (g, d) in [
        (82_000, 20),
        (81_000, 10),
        (81_400, 9),
        (80_600, 3),
        (80_400, 1),
        (80_500, 0),
    ] {
        log(&mut e, g, NOW - d * DAY);
    }
    e
}

#[test]
fn stats_on_a_hand_computed_fixture() {
    let mut e = fixture();
    let s = snapshot(&mut e, NOW).stats;
    assert_eq!(s.count, 6);
    assert_eq!(s.current_grams, Some(80_500));
    assert_eq!(s.current_measured_ms, Some(NOW));
    // (80600 + 80400 + 80500) / 3
    assert_eq!(s.avg7_grams, Some(80_500));
    // avg7(NOW-7d) = (81000 + 81400) / 2 = 81200
    assert_eq!(s.weekly_change_grams, Some(-700));
    assert_eq!((s.min_grams, s.max_grams), (Some(80_400), Some(82_000)));
    // avg7 at the first entry in the window is 82000
    assert_eq!(s.trend30_grams, Some(-1_500));
    assert_eq!(s.trend90_grams, Some(-1_500));
    // 15 days later only the last entries are inside 30 d, still >= 2
    let s = snapshot(&mut e, NOW + 15 * DAY).stats;
    assert_eq!(s.trend30_grams, Some(80_500 - 81_000));
    // 29.5 days later only one entry is left in the 30-day window
    let s = snapshot(&mut e, NOW + 29 * DAY + DAY / 2).stats;
    assert_eq!(s.trend30_grams, None);
    assert_eq!(s.trend90_grams, Some(-1_500));
    assert_eq!(s.goal_grams, None);
    assert_eq!((s.goal_distance_grams, s.goal_reached), (None, None));

    let view = snapshot(&mut e, NOW);
    let series: Vec<(u64, u64)> = view
        .series
        .iter()
        .map(|p| (p.weight_grams, p.avg7_grams))
        .collect();
    assert_eq!(
        series,
        vec![
            (82_000, 82_000),
            (81_000, 81_000),
            (81_400, 81_200),
            // 81000@-10 is exactly 7 days before -3: excluded, window is (t-7d, t]
            (80_600, 81_000),
            (80_400, 80_500),
            (80_500, 80_500),
        ]
    );
    assert!(view
        .series
        .windows(2)
        .all(|w| w[0].measured_ms <= w[1].measured_ms));
    assert!(view
        .entries
        .windows(2)
        .all(|w| w[0].measured_ms >= w[1].measured_ms));
    assert_eq!(view.stat_labels["avg7"], "80.5 kg");
    assert_eq!(view.stat_labels["weekly_change"], "-0.7 kg");
    assert_eq!(view.stat_labels["trend30"], "-1.5 kg");
    assert_eq!(view.stat_labels["min"], "80.4 kg");
    assert_eq!(view.stat_labels["max"], "82.0 kg");
    assert!(!view.stat_labels.contains_key("goal"));
}

#[test]
fn goal_distance_and_direction() {
    let mut e = fixture();
    ok(
        &mut e,
        json!({"type":"update_settings","unit":"kg","goal_grams":78_000}),
    );
    let s = snapshot(&mut e, NOW).stats;
    assert_eq!(s.goal_grams, Some(78_000));
    assert_eq!(s.goal_distance_grams, Some(2_500));
    assert_eq!(s.goal_reached, Some(false));
    ok(
        &mut e,
        json!({"type":"update_settings","unit":"kg","goal_grams":81_000}),
    );
    let view = snapshot(&mut e, NOW);
    assert_eq!(view.stats.goal_distance_grams, Some(-500));
    assert_eq!(view.stats.goal_reached, Some(true));
    assert_eq!(view.stat_labels["goal_distance"], "-0.5 kg");
    // gaining: goal above the first entry
    ok(
        &mut e,
        json!({"type":"update_settings","unit":"kg","goal_grams":85_000}),
    );
    let s = snapshot(&mut e, NOW).stats;
    assert_eq!(
        (s.goal_distance_grams, s.goal_reached),
        (Some(-4_500), Some(false))
    );
    let mut empty = Engine::in_memory();
    ok(
        &mut empty,
        json!({"type":"update_settings","unit":"kg","goal_grams":85_000}),
    );
    let s = snapshot(&mut empty, NOW).stats;
    assert_eq!(
        (s.goal_grams, s.goal_distance_grams, s.goal_reached),
        (Some(85_000), None, None)
    );
}

#[test]
fn null_stats_with_zero_and_one_entry() {
    let mut e = Engine::in_memory();
    let view = snapshot(&mut e, NOW);
    assert_eq!(view.stats, stats::Stats::default());
    assert!(view.series.is_empty() && view.stat_labels.is_empty());
    log(&mut e, 80_000, NOW);
    let s = snapshot(&mut e, NOW).stats;
    assert_eq!(s.count, 1);
    assert_eq!(s.avg7_grams, Some(80_000));
    assert_eq!(s.min_grams, Some(80_000));
    assert_eq!(s.weekly_change_grams, None);
    assert_eq!((s.trend30_grams, s.trend90_grams), (None, None));
}

#[test]
fn same_time_entries_and_half_up_rounding() {
    let mut e = Engine::in_memory();
    log(&mut e, 80_001, NOW);
    log(&mut e, 80_002, NOW);
    let s = snapshot(&mut e, NOW).stats;
    // (80001 + 80002) / 2 = 80001.5 → 80002
    assert_eq!(s.avg7_grams, Some(80_002));
    assert_eq!(s.trend30_grams, Some(0));
    assert_eq!(stats::rounded_mean(&[1, 2, 2]), Some(2));
    assert_eq!(stats::rounded_mean(&[]), None);
}

#[test]
fn labels_in_kg_and_lb() {
    assert_eq!(units::label(80_500, "kg"), "80.5 kg");
    assert_eq!(units::label(80_450, "kg"), "80.5 kg");
    assert_eq!(units::label(80_449, "kg"), "80.4 kg");
    assert_eq!(units::label(80_000, "kg"), "80.0 kg");
    assert_eq!(units::label(1_000, "lb"), "2.2 lb");
    assert_eq!(units::label(80_500, "lb"), "177.5 lb");
    assert_eq!(units::label(45_359, "lb"), "100.0 lb");
    assert_eq!(units::signed_label(-700, "kg"), "-0.7 kg");
    assert_eq!(units::signed_label(1_200, "kg"), "+1.2 kg");
    assert_eq!(units::signed_label(-30, "kg"), "0.0 kg");
    assert_eq!(units::signed_label(0, "lb"), "0.0 lb");
    assert_eq!(units::signed_label(-453, "lb"), "-1.0 lb");
}

#[test]
fn insights_mirror_the_view_stats() {
    let mut e = fixture();
    let view = snapshot(&mut e, NOW);
    let i = secretary_body::insights(e.state(), NOW);
    assert_eq!(i.unit, "kg");
    assert_eq!(i.stats, view.stats);
    assert_eq!(i.stat_labels, view.stat_labels);
}
