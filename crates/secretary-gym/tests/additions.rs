//! Secretary additions to the gym protocol: repeat_workout, integer-gram
//! edits, parse_weight, elapsed seconds, e1RM, per-exercise charts, insights.
use secretary_gym::{insights, Command, Engine, Response};
use serde_json::{json, Value};
use uuid::Uuid;

const NOW: u64 = 1_792_584_000_000; // 2026-10-21 (Wednesday) 12:00 UTC
const DAY: u64 = 86_400_000;

fn send_at(engine: &mut Engine, command: Value, now: u64) -> Response {
    let revision = engine.state().revision;
    engine.handle_at(
        &json!({"op_id":Uuid::new_v4().to_string(),"expected_revision":revision,"timezone":"Europe/Rome","command":command})
            .to_string(),
        now,
    )
}
fn ok_at(engine: &mut Engine, command: Value, now: u64) -> Response {
    let r = send_at(engine, command, now);
    assert!(r.ok, "{:?}", r.error);
    r
}
fn ok(engine: &mut Engine, command: Value) -> Response {
    ok_at(engine, command, NOW)
}
fn query(engine: &mut Engine, command: Value, now: u64) -> Response {
    engine.handle_at(
        &json!({"timezone":"Europe/Rome","command":command}).to_string(),
        now,
    )
}

/// One finished workout: `exercise` with completed sets of (weight_grams, reps).
fn workout(e: &mut Engine, exercise: &str, sets: &[(u64, u64)], finished: u64) -> String {
    ok_at(
        e,
        json!({"type":"start_workout","title":"Session"}),
        finished - 3_600_000,
    );
    ok_at(
        e,
        json!({"type":"add_exercise","exercise_id":exercise}),
        finished - 3_600_000,
    );
    let we = e.state().active.as_ref().unwrap().exercises[0].id.clone();
    for _ in 1..sets.len() {
        ok_at(
            e,
            json!({"type":"add_set","exercise_id":we}),
            finished - 3_600_000,
        );
    }
    let ids: Vec<String> = e.state().active.as_ref().unwrap().exercises[0]
        .sets
        .iter()
        .map(|s| s.id.clone())
        .collect();
    for (id, (grams, reps)) in ids.iter().zip(sets) {
        ok_at(
            e,
            json!({"type":"edit_set","set_id":id,"weight_grams":grams,"reps":reps,"seconds":0,"distance_meters":0,"kind":"normal"}),
            finished - 3_600_000,
        );
        ok_at(
            e,
            json!({"type":"complete_set","set_id":id,"completed":true}),
            finished - 3_600_000,
        );
    }
    ok_at(e, json!({"type":"finish_workout"}), finished);
    e.state().history.last().unwrap().id.clone()
}

#[test]
fn repeat_workout_copies_a_finished_workout_with_unchecked_sets() {
    let mut e = Engine::in_memory();
    let id = workout(&mut e, "bench-press", &[(60_000, 8), (62_500, 6)], NOW);
    let past = e.state().history[0].clone();
    ok_at(
        &mut e,
        json!({"type":"repeat_workout","workout_id":id}),
        NOW + DAY,
    );
    let active = e.state().active.clone().unwrap();
    assert_eq!(active.title, past.title);
    assert_eq!(active.started_ms, NOW + DAY);
    assert_eq!(active.exercises.len(), 1);
    assert_eq!(active.exercises[0].exercise_id, "bench-press");
    assert_ne!(active.exercises[0].id, past.exercises[0].id);
    let sets = &active.exercises[0].sets;
    assert_eq!(
        sets.iter()
            .map(|s| (s.weight_grams, s.reps))
            .collect::<Vec<_>>(),
        vec![(60_000, 8), (62_500, 6)]
    );
    assert!(sets.iter().all(|s| !s.completed));
    assert!(sets
        .iter()
        .zip(&past.exercises[0].sets)
        .all(|(a, b)| a.id != b.id));
    // history untouched; a second repeat is refused while one is active
    assert_eq!(e.state().history, vec![past]);
    assert!(
        !send_at(
            &mut e,
            json!({"type":"repeat_workout","workout_id":id}),
            NOW + DAY
        )
        .ok
    );
    ok(&mut e, json!({"type":"cancel_workout"}));
    assert!(
        !send_at(
            &mut e,
            json!({"type":"repeat_workout","workout_id":"missing"}),
            NOW
        )
        .ok
    );
}

#[test]
fn edit_set_takes_exactly_one_of_weight_and_weight_grams() {
    let mut e = Engine::in_memory();
    ok(&mut e, json!({"type":"start_workout","title":"T"}));
    ok(&mut e, json!({"type":"add_exercise","exercise_id":"squat"}));
    let set = e.state().active.as_ref().unwrap().exercises[0].sets[0]
        .id
        .clone();
    let edit = |w: Value| {
        let mut c = json!({"type":"edit_set","set_id":set,"reps":5,"seconds":0,"distance_meters":0,"kind":"normal"});
        for (k, v) in w.as_object().unwrap() {
            c[k] = v.clone();
        }
        c
    };
    ok(&mut e, edit(json!({"weight_grams": 100_500})));
    assert_eq!(
        e.state().active.as_ref().unwrap().exercises[0].sets[0].weight_grams,
        100_500
    );
    ok(&mut e, edit(json!({"weight": "101,5"})));
    assert_eq!(
        e.state().active.as_ref().unwrap().exercises[0].sets[0].weight_grams,
        101_500
    );
    let before = serde_json::to_string(e.state()).unwrap();
    assert!(
        !send_at(
            &mut e,
            edit(json!({"weight": "1", "weight_grams": 1000})),
            NOW
        )
        .ok
    );
    assert!(!send_at(&mut e, edit(json!({})), NOW).ok);
    assert!(!send_at(&mut e, edit(json!({"weight_grams": 2_000_001})), NOW).ok);
    assert_eq!(before, serde_json::to_string(e.state()).unwrap());
}

#[test]
fn string_only_edit_set_serializes_like_the_original_protocol() {
    // Receipt fingerprints hash the serialized command; the original
    // string-only form must keep its exact serialization.
    let original = json!({"type":"edit_set","set_id":"s","weight":"60","reps":8,"seconds":0,"distance_meters":0,"kind":"normal"});
    let command: Command = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(&command).unwrap(), original);
}

#[test]
fn parse_weight_is_a_query_in_the_display_unit() {
    let mut e = Engine::in_memory();
    let r = query(&mut e, json!({"type":"parse_weight","value":"80,5"}), NOW);
    assert!(r.ok && r.view.is_none());
    assert_eq!(r.weight_grams, Some(80_500));
    let r = query(
        &mut e,
        json!({"type":"parse_weight","value":"100","unit":"lb"}),
        NOW,
    );
    assert_eq!(r.weight_grams, Some(45_359));
    ok(
        &mut e,
        json!({"type":"update_settings","weight_unit":"lb","rest_seconds":90,"weekly_goal":3}),
    );
    let rev = e.state().revision;
    let r = query(&mut e, json!({"type":"parse_weight","value":"100"}), NOW);
    assert_eq!(r.weight_grams, Some(45_359));
    let r = query(&mut e, json!({"type":"parse_weight","value":"abc"}), NOW);
    assert_eq!(r.code.as_deref(), Some("invalid_command"));
    let r = query(
        &mut e,
        json!({"type":"parse_weight","value":"1","unit":"st"}),
        NOW,
    );
    assert_eq!(r.code.as_deref(), Some("invalid_command"));
    assert_eq!(e.state().revision, rev);
}

#[test]
fn elapsed_seconds_in_summary_and_clock() {
    let mut e = Engine::in_memory();
    ok(&mut e, json!({"type":"start_workout","title":"T"}));
    let r = query(&mut e, json!({"type":"clock"}), NOW + 725_400);
    let clock = r.clock.unwrap();
    assert_eq!(clock.active_elapsed_seconds, 725);
    assert_eq!(clock.active_duration, "12m 05s");
    let view = query(&mut e, json!({"type":"snapshot"}), NOW + 725_400)
        .view
        .unwrap();
    assert_eq!(view.summary.active_elapsed_seconds, 725);
    ok(&mut e, json!({"type":"cancel_workout"}));
    let clock = query(&mut e, json!({"type":"clock"}), NOW + 1)
        .clock
        .unwrap();
    assert_eq!(clock.active_elapsed_seconds, 0);
}

#[test]
fn records_carry_estimated_one_rep_max() {
    let mut e = Engine::in_memory();
    workout(&mut e, "bench-press", &[(100_000, 5), (110_000, 1)], NOW);
    workout(&mut e, "push-up", &[(0, 20)], NOW + DAY);
    let view = query(&mut e, json!({"type":"snapshot"}), NOW + DAY)
        .view
        .unwrap();
    let bench = view
        .records
        .iter()
        .find(|r| r.exercise_id == "bench-press")
        .unwrap();
    // Epley: 100 kg × 5 → 116.667 kg beats the 110 kg single.
    assert_eq!(bench.estimated_1rm_grams, Some(116_667));
    assert_eq!(bench.estimated_1rm.as_deref(), Some("116.7 kg"));
    let push = view
        .records
        .iter()
        .find(|r| r.exercise_id == "push-up")
        .unwrap();
    assert_eq!(push.estimated_1rm_grams, None);
    assert_eq!(push.estimated_1rm, None);
}

#[test]
fn per_exercise_charts_and_integer_chart_volume() {
    let mut e = Engine::in_memory();
    workout(&mut e, "squat", &[(100_000, 5)], NOW);
    workout(&mut e, "bench-press", &[(60_000, 8)], NOW + DAY);
    workout(
        &mut e,
        "squat",
        &[(105_000, 5), (105_000, 5)],
        NOW + 2 * DAY,
    );
    let view = query(&mut e, json!({"type":"snapshot"}), NOW + 2 * DAY)
        .view
        .unwrap();
    assert_eq!(
        view.chart
            .iter()
            .map(|p| p.volume_grams)
            .collect::<Vec<_>>(),
        vec![500_000, 480_000, 1_050_000]
    );
    let squat = &view.exercise_charts["squat"];
    assert_eq!(
        squat.iter().map(|p| p.volume_grams).collect::<Vec<_>>(),
        vec![500_000, 1_050_000]
    );
    assert_eq!(squat[1].volume, 1050.0);
    assert_eq!(squat[0].label, "21 Oct");
    assert_eq!(view.exercise_charts["bench-press"].len(), 1);
    assert!(!view.exercise_charts.contains_key("deadlift"));
    for i in 0..15 {
        workout(&mut e, "deadlift", &[(140_000, 3)], NOW + (3 + i) * DAY);
    }
    let view = query(&mut e, json!({"type":"snapshot"}), NOW + 20 * DAY)
        .view
        .unwrap();
    let dl = &view.exercise_charts["deadlift"];
    assert_eq!(dl.len(), 14);
    assert!(dl.windows(2).all(|w| w[0].label != w[1].label));
}

#[test]
fn insights_report_recent_workouts_prs_and_weekly_volume() {
    let tz: chrono_tz::Tz = "Europe/Rome".parse().unwrap();
    let mut e = Engine::in_memory();
    // first-ever bench is not a PR, heavier later ones are, equal ones are not
    let first = workout(&mut e, "bench-press", &[(80_000, 5)], NOW - 14 * DAY);
    let second = workout(
        &mut e,
        "bench-press",
        &[(82_500, 5), (80_000, 8)],
        NOW - 7 * DAY,
    );
    let third = workout(&mut e, "bench-press", &[(82_500, 6)], NOW - DAY);
    let fourth = workout(&mut e, "squat", &[(120_000, 5)], NOW);
    let s = insights::summary(e.state(), NOW, tz, 5);
    assert_eq!(s.unit, "kg");
    let ids: Vec<_> = s.recent_workouts.iter().map(|w| w.id.clone()).collect();
    assert_eq!(
        ids,
        vec![fourth.clone(), third.clone(), second.clone(), first.clone()]
    );
    let by_id = |id: &str| s.recent_workouts.iter().find(|w| w.id == id).unwrap();
    assert!(by_id(&first).prs.is_empty());
    assert!(by_id(&third).prs.is_empty());
    assert!(by_id(&fourth).prs.is_empty());
    let pr = &by_id(&second).prs;
    assert_eq!(pr.len(), 1);
    assert_eq!((pr[0].weight_grams, pr[0].reps), (82_500, 5));
    assert_eq!(pr[0].label, "82.5 kg × 5");
    assert_eq!(by_id(&second).volume_grams, 82_500 * 5 + 80_000 * 8);
    assert_eq!(by_id(&second).volume_label, "1052.5 kg");
    assert_eq!(by_id(&second).exercises, vec!["Bench Press".to_string()]);
    assert_eq!(by_id(&fourth).duration_min, 60);
    assert_eq!(by_id(&fourth).date, "2026-10-21");
    assert_eq!(by_id(&fourth).completed_sets, 1);

    assert_eq!(s.weekly_volume.len(), 8);
    assert_eq!(s.weekly_volume[7].week_start, "2026-10-19");
    assert_eq!(s.weekly_volume[0].week_start, "2026-08-31");
    // this week: squat (Wed 21 Oct) + bench (Tue 20 Oct)
    assert_eq!(s.weekly_volume[7].workouts, 2);
    assert_eq!(s.weekly_volume[7].volume_grams, 600_000 + 495_000);
    assert_eq!(s.weekly_volume[6].workouts, 1);
    assert_eq!(s.weekly_volume[5].workouts, 1);
    assert_eq!(s.weekly_volume[4].workouts, 0);
    assert_eq!(s.weekly_volume[4].volume_label, "0 kg");
    assert_eq!(s.workouts_this_week, 2);
    assert_eq!(
        insights::summary(e.state(), NOW, tz, 2)
            .recent_workouts
            .len(),
        2
    );
    assert_eq!(
        insights::summary(e.state(), NOW, tz, 0)
            .recent_workouts
            .len(),
        1
    );
    let empty = insights::summary(Engine::in_memory().state(), NOW, tz, 5);
    assert!(empty.recent_workouts.is_empty());
    assert!(empty.weekly_volume.iter().all(|w| w.workouts == 0));
}
