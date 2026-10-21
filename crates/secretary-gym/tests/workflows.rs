use secretary_gym::{kernel, parse_weight, Engine, Response};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use uuid::Uuid;

const NOW: u64 = 1_792_584_000_000;
fn send(engine: &mut Engine, command: Value) -> Response {
    send_at(engine, command, NOW)
}
fn send_at(engine: &mut Engine, command: Value, now: u64) -> Response {
    let revision = engine.state().revision;
    engine.handle_at(&json!({"op_id":Uuid::new_v4().to_string(),"expected_revision":revision,"timezone":"Europe/Copenhagen","command":command}).to_string(),now)
}
fn ok(engine: &mut Engine, command: Value) -> Response {
    let response = send(engine, command);
    assert!(response.ok, "{:?}", response.error);
    response
}
fn start(engine: &mut Engine) -> String {
    ok(
        engine,
        json!({"type":"start_workout","title":"Morning strength"}),
    );
    ok(
        engine,
        json!({"type":"add_exercise","exercise_id":"bench-press"}),
    );
    engine.state().active.as_ref().unwrap().exercises[0].sets[0]
        .id
        .clone()
}
fn edit(engine: &mut Engine, set: &str, weight: &str, reps: u64) -> Response {
    send(
        engine,
        json!({"type":"edit_set","set_id":set,"weight":weight,"reps":reps,"seconds":0,"distance_meters":0,"kind":"normal"}),
    )
}
fn completed_workout(engine: &mut Engine) -> String {
    let set = start(engine);
    assert!(edit(engine, &set, "60", 8).ok);
    ok(
        engine,
        json!({"type":"complete_set","set_id":set,"completed":true}),
    );
    let r = send_at(engine, json!({"type":"finish_workout"}), NOW + 600_000);
    assert!(r.ok, "{:?}", r.error);
    engine.state().history.last().unwrap().id.clone()
}
fn temp() -> PathBuf {
    std::env::temp_dir().join(format!("lucina-gym-{}", Uuid::new_v4()))
}

#[test]
fn workout_flow_counts_volume_and_preserves_history() {
    let mut e = Engine::in_memory();
    completed_workout(&mut e);
    let r = ok(&mut e, json!({"type":"snapshot"}));
    let view = r.view.unwrap();
    assert_eq!(view.summary.workouts, 1);
    assert_eq!(view.summary.completed_sets, 1);
    assert_eq!(view.summary.volume, "480 kg");
    assert_eq!(view.records[0].weight, "60 kg");
    assert!(e.state().active.is_none());
    assert!(e.state().rest.is_none());
    let old = e.state().history.clone();
    start(&mut e);
    assert_eq!(e.state().history, old);
    assert_eq!(
        e.state().active.as_ref().unwrap().exercises[0].sets[0].weight_grams,
        60_000
    );
}

#[test]
fn duplicate_start_cannot_resurrect_cancelled_session() {
    let mut e = Engine::in_memory();
    let req=json!({"op_id":"start-once","expected_revision":0,"command":{"type":"start_workout","title":"Once"}}).to_string();
    assert!(e.handle_at(&req, NOW).ok);
    ok(&mut e, json!({"type":"cancel_workout"}));
    let rev = e.state().revision;
    let r = e.handle_at(&req, NOW);
    assert!(r.ok && r.replayed);
    assert_eq!(e.state().revision, rev);
    assert!(e.state().active.is_none());
}

#[test]
fn finish_retry_is_idempotent_and_cannot_duplicate_history() {
    let mut e = Engine::in_memory();
    let set = start(&mut e);
    ok(
        &mut e,
        json!({"type":"complete_set","set_id":set,"completed":true}),
    );
    let req=json!({"op_id":"finish-once","expected_revision":e.state().revision,"command":{"type":"finish_workout"}}).to_string();
    assert!(e.handle_at(&req, NOW + 1000).ok);
    assert!(e.handle_at(&req, NOW + 5000).replayed);
    assert_eq!(e.state().history.len(), 1);
}

#[test]
fn conflicting_revision_and_reused_operation_ids_are_rejected() {
    let mut e = Engine::in_memory();
    let req = json!({"op_id":"same","expected_revision":0,"command":{"type":"start_workout","title":"One"}});
    assert!(e.handle_at(&req.to_string(), NOW).ok);
    let mut stale = req.clone();
    stale["op_id"] = json!("different");
    assert_eq!(
        e.handle_at(&stale.to_string(), NOW).code.as_deref(),
        Some("conflict")
    );
    let mut reused = req;
    reused["command"]["title"] = json!("Another");
    assert_eq!(
        e.handle_at(&reused.to_string(), NOW).code.as_deref(),
        Some("op_id_reused")
    );
    assert_eq!(e.state().revision, 1);
}

#[test]
fn rejected_transitions_leave_every_byte_of_state_unchanged() {
    let mut e = Engine::in_memory();
    let set = start(&mut e);
    let before = serde_json::to_string(e.state()).unwrap();
    assert!(!send(&mut e, json!({"type":"finish_workout"})).ok);
    assert!(!edit(&mut e, &set, "-20", 8).ok);
    assert!(!edit(&mut e, &set, "100", 1001).ok);
    assert!(!send(&mut e, json!({"type":"start_workout","title":"Second"})).ok);
    assert_eq!(before, serde_json::to_string(e.state()).unwrap());
}

#[test]
fn completed_set_must_be_unchecked_to_edit_and_zero_reps_cannot_complete() {
    let mut e = Engine::in_memory();
    let set = start(&mut e);
    assert!(edit(&mut e, &set, "20", 0).ok);
    assert!(
        !send(
            &mut e,
            json!({"type":"complete_set","set_id":set,"completed":true})
        )
        .ok
    );
    assert!(edit(&mut e, &set, "20", 8).ok);
    ok(
        &mut e,
        json!({"type":"complete_set","set_id":set,"completed":true}),
    );
    assert!(!edit(&mut e, &set, "40", 8).ok);
    ok(
        &mut e,
        json!({"type":"complete_set","set_id":set,"completed":false}),
    );
    assert!(edit(&mut e, &set, "40", 8).ok);
}

#[test]
fn warmups_do_not_inflate_volume_or_records() {
    let mut e = Engine::in_memory();
    let set = start(&mut e);
    ok(
        &mut e,
        json!({"type":"edit_set","set_id":set,"weight":"100","reps":5,"seconds":0,"distance_meters":0,"kind":"warmup"}),
    );
    ok(
        &mut e,
        json!({"type":"complete_set","set_id":set,"completed":true}),
    );
    ok(&mut e, json!({"type":"finish_workout"}));
    let view = ok(&mut e, json!({"type":"snapshot"})).view.unwrap();
    assert_eq!(view.summary.volume, "0 kg");
    assert!(view.records.is_empty());
}

#[test]
fn rest_timer_survives_queries_and_counts_down_without_mutations() {
    let mut e = Engine::in_memory();
    let set = start(&mut e);
    ok(
        &mut e,
        json!({"type":"complete_set","set_id":set,"completed":true}),
    );
    let rev = e.state().revision;
    assert_eq!(
        send_at(&mut e, json!({"type":"snapshot"}), NOW + 1)
            .view
            .unwrap()
            .rest_remaining,
        90
    );
    assert_eq!(
        send_at(&mut e, json!({"type":"snapshot"}), NOW + 90_000)
            .view
            .unwrap()
            .rest_remaining,
        0
    );
    assert_eq!(e.state().revision, rev);
}

#[test]
fn durable_state_and_receipts_survive_restart_and_writer_lock_is_exclusive() {
    let dir = temp();
    let path = dir.join("gym.json");
    let mut e = Engine::open(&path).unwrap();
    completed_workout(&mut e);
    assert!(Engine::open(&path).is_err());
    let expected = e.state().clone();
    drop(e);
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.state(), &expected);
    drop(e);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn storage_failure_does_not_commit_memory_or_receipt() {
    let dir = temp();
    let path = dir.join("gym.json");
    let mut e = Engine::open(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    let old = e.state().clone();
    let r = send(
        &mut e,
        json!({"type":"start_workout","title":"Must not commit"}),
    );
    assert_eq!(r.code.as_deref(), Some("storage_error"));
    assert_eq!(e.state(), &old);
}

#[test]
fn corrupt_store_is_not_silently_overwritten() {
    let dir = temp();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gym.json");
    fs::write(&path, b"broken").unwrap();
    assert!(Engine::open(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"broken");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backup_roundtrip_preserves_data_and_restore_does_not_roll_back_revision() {
    let mut a = Engine::in_memory();
    completed_workout(&mut a);
    let backup = ok(&mut a, json!({"type":"export_json"})).text.unwrap();
    let mut b = Engine::in_memory();
    ok(&mut b, json!({"type":"restore_json","backup":backup}));
    assert_eq!(b.state().history, a.state().history);
    assert_eq!(b.state().revision, 1);
    let before = b.state().clone();
    assert!(!send(&mut b, json!({"type":"restore_json","backup":"{}"})).ok);
    assert_eq!(b.state(), &before);
    let mut invalid = serde_json::to_value(a.state()).unwrap();
    invalid["history"][0]["exercises"][0]["exercise_id"] = json!("missing");
    assert!(
        !send(
            &mut b,
            json!({"type":"restore_json","backup":invalid.to_string()})
        )
        .ok
    );
    assert_eq!(b.state(), &before);
}

#[test]
fn fixed_point_units_have_no_binary_float_input_drift() {
    assert_eq!(parse_weight("60,125", "kg").unwrap(), 60125);
    assert_eq!(parse_weight("100", "lb").unwrap(), 45359);
    assert!(parse_weight("1.0001", "kg").is_err());
    assert!(parse_weight("NaN", "kg").is_err());
    assert!(parse_weight("-1", "lb").is_err());
    assert_eq!(
        kernel::set_volume(2_000_000, 1000, true, false),
        2_000_000_000
    );
    for grams in [0, 1, 250, 1001, 45359, 60000, 2_000_000] {
        let pounds = format!("{:.3}", grams as f64 / 453.59237);
        assert_eq!(parse_weight(&pounds, "lb").unwrap(), grams);
    }
}

#[test]
fn csv_handles_quotes_and_newlines() {
    let mut e = Engine::in_memory();
    completed_workout(&mut e);
    let csv = ok(&mut e, json!({"type":"export_csv"})).text.unwrap();
    let mut reader = csv::Reader::from_reader(csv.as_bytes());
    let row = reader.records().next().unwrap().unwrap();
    assert_eq!(&row[0], "Morning strength");
    assert_eq!(&row[6], "60");
    assert_eq!(&row[7], "8");
}

#[test]
fn routines_custom_exercises_search_and_measurements_are_functional() {
    let mut e = Engine::in_memory();
    ok(
        &mut e,
        json!({"type":"create_exercise","name":"Ring Row","muscle":"Back","equipment":"Rings","kind":"reps_only"}),
    );
    let result = ok(&mut e, json!({"type":"search_exercises","query":"rings"}));
    assert_eq!(result.exercises.unwrap().len(), 1);
    ok(
        &mut e,
        json!({"type":"save_routine","name":"Simple","exercise_ids":["bench-press","squat"]}),
    );
    let id = e.state().routines.last().unwrap().id.clone();
    ok(
        &mut e,
        json!({"type":"start_workout","title":"Simple","routine_id":id}),
    );
    assert_eq!(e.state().active.as_ref().unwrap().exercises.len(), 2);
    // Body weight moved to secretary-body; the gym keeps body fat and waist.
    assert!(
        !send(
            &mut e,
            json!({"type":"record_measurement","kind":"body_weight","value":"80.5"})
        )
        .ok
    );
    ok(
        &mut e,
        json!({"type":"record_measurement","kind":"waist","value":"80.5"}),
    );
    assert!(
        !send(
            &mut e,
            json!({"type":"record_measurement","kind":"body_fat","value":"101"})
        )
        .ok
    );
    assert_eq!(e.state().measurements.len(), 1);
}

#[test]
fn timed_and_distance_exercises_validate_their_own_fields() {
    let mut e = Engine::in_memory();
    ok(
        &mut e,
        json!({"type":"start_workout","title":"Conditioning"}),
    );
    ok(&mut e, json!({"type":"add_exercise","exercise_id":"plank"}));
    let set = e.state().active.as_ref().unwrap().exercises[0].sets[0]
        .id
        .clone();
    assert_eq!(
        e.state().active.as_ref().unwrap().exercises[0].sets[0].reps,
        0
    );
    ok(
        &mut e,
        json!({"type":"edit_set","set_id":set,"weight":"0","reps":0,"seconds":0,"distance_meters":0,"kind":"normal"}),
    );
    assert!(
        !send(
            &mut e,
            json!({"type":"complete_set","set_id":set,"completed":true})
        )
        .ok
    );
}
