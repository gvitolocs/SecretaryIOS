use crate::{kernel, models::*};
use chrono::{Datelike, TimeZone};
use std::collections::BTreeMap;

fn number(n: f64) -> String {
    format!("{n:.3}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
pub fn weight(grams: u64, unit: &str) -> String {
    number(if unit == "lb" {
        grams as f64 / 453.59237
    } else {
        grams as f64 / 1000.0
    })
}
pub(crate) fn volume(grams: u64, unit: &str) -> String {
    format!("{} {}", weight(grams, unit), unit)
}
fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs / 60) % 60)
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}
pub(crate) fn workout_volume(w: &Workout) -> u64 {
    w.exercises
        .iter()
        .flat_map(|e| &e.sets)
        .map(|set| {
            kernel::set_volume(
                set.weight_grams,
                set.reps,
                set.completed,
                set.kind == SetKind::Warmup,
            )
        })
        .sum()
}
pub(crate) fn completed(w: &Workout) -> usize {
    w.exercises
        .iter()
        .flat_map(|e| &e.sets)
        .filter(|set| set.completed)
        .count()
}
fn active_elapsed_ms(state: &State, now: u64) -> u64 {
    state
        .active
        .as_ref()
        .map_or(0, |w| now.saturating_sub(w.started_ms))
}
/// Epley estimate `w * (1 + reps/30)`, rounded half up; a single rep is `w`.
pub(crate) fn estimated_1rm(weight_grams: u64, reps: u64) -> u64 {
    if reps <= 1 {
        weight_grams
    } else {
        (weight_grams * (30 + reps) + 15) / 30
    }
}
pub(crate) fn chart_point(
    id: &str,
    finished_ms: u64,
    grams: u64,
    unit: &str,
    tz: chrono_tz::Tz,
) -> ChartPoint {
    ChartPoint {
        id: id.to_string(),
        label: tz
            .timestamp_millis_opt(finished_ms as i64)
            .single()
            .unwrap()
            .format("%d %b")
            .to_string(),
        volume: if unit == "lb" {
            grams as f64 / 453.59237
        } else {
            grams as f64 / 1000.0
        },
        volume_grams: grams,
    }
}
pub fn clock(state: &State, now: u64) -> Clock {
    let elapsed = active_elapsed_ms(state, now);
    Clock {
        rest_remaining: state
            .rest
            .as_ref()
            .map_or(0, |r| kernel::rest_remaining(r.deadline_ms, now)),
        active_duration: duration(elapsed),
        active_elapsed_seconds: elapsed / 1000,
    }
}
fn set_label(set: &WorkoutSet, kind: &ExerciseKind, unit: &str) -> String {
    match kind {
        ExerciseKind::WeightReps => {
            format!("{} {} × {}", weight(set.weight_grams, unit), unit, set.reps)
        }
        ExerciseKind::RepsOnly => format!("{} reps", set.reps),
        ExerciseKind::Timed => format!("{} s", set.seconds),
        ExerciseKind::DistanceTime => format!("{} m · {} s", set.distance_meters, set.seconds),
    }
}

pub fn view(state: &State, now: u64, tz: chrono_tz::Tz) -> View {
    let unit = &state.settings.weight_unit;
    let mut visible = state.clone();
    visible.receipts.clear();
    visible
        .history
        .sort_by_key(|w| std::cmp::Reverse(w.finished_ms));
    visible
        .measurements
        .sort_by_key(|m| std::cmp::Reverse(m.timestamp_ms));
    visible.exercises.sort_by_key(|a| a.name.to_lowercase());
    let today = tz
        .timestamp_millis_opt(now as i64)
        .single()
        .unwrap()
        .date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let weekly_workouts = state
        .history
        .iter()
        .filter(|w| {
            let date = tz
                .timestamp_millis_opt(w.finished_ms.unwrap() as i64)
                .single()
                .unwrap()
                .date_naive();
            date >= monday && date <= today
        })
        .count();
    let summary = Summary {
        workouts: state.history.len(),
        completed_sets: state.history.iter().map(completed).sum(),
        weekly_workouts,
        weekly_goal: state.settings.weekly_goal,
        volume: volume(state.history.iter().map(workout_volume).sum(), unit),
        active_volume: volume(state.active.as_ref().map_or(0, workout_volume), unit),
        active_duration: duration(active_elapsed_ms(state, now)),
        active_elapsed_seconds: active_elapsed_ms(state, now) / 1000,
        active_completed_sets: state.active.as_ref().map_or(0, completed),
    };
    let mut weight_labels = BTreeMap::new();
    let mut previous_labels = BTreeMap::new();
    let mut workout_labels = BTreeMap::new();
    for w in state.history.iter().chain(state.active.iter()) {
        workout_labels.insert(
            w.id.clone(),
            WorkoutSummary {
                sets: completed(w),
                volume: volume(workout_volume(w), unit),
                duration: duration(w.finished_ms.unwrap_or(now).saturating_sub(w.started_ms)),
            },
        );
        for e in &w.exercises {
            for set in &e.sets {
                weight_labels.insert(set.id.clone(), weight(set.weight_grams, unit));
            }
        }
    }
    if let Some(w) = &state.active {
        for e in &w.exercises {
            if let Some(old) = crate::previous_set(state, &e.exercise_id) {
                let kind = &state
                    .exercises
                    .iter()
                    .find(|x| x.id == e.exercise_id)
                    .unwrap()
                    .kind;
                previous_labels.insert(e.id.clone(), set_label(old, kind, unit));
            }
        }
    }
    let mut records = vec![];
    for e in &visible.exercises {
        let sets: Vec<_> = state
            .history
            .iter()
            .flat_map(|w| &w.exercises)
            .filter(|x| x.exercise_id == e.id)
            .flat_map(|x| &x.sets)
            .filter(|set| set.completed && set.kind != SetKind::Warmup)
            .collect();
        if sets.is_empty() {
            continue;
        }
        let best = sets
            .iter()
            .fold(0, |n, set| kernel::best_record(n, set.weight_grams));
        let e1rm = (e.kind == ExerciseKind::WeightReps)
            .then(|| {
                sets.iter()
                    .filter(|x| x.weight_grams > 0 && x.reps > 0)
                    .map(|x| estimated_1rm(x.weight_grams, x.reps))
                    .max()
            })
            .flatten();
        records.push(Record {
            exercise_id: e.id.clone(),
            name: e.name.clone(),
            weight: volume(best, unit),
            reps: sets.iter().map(|x| x.reps).max().unwrap_or(0),
            volume: volume(
                sets.iter()
                    .map(|x| kernel::set_volume(x.weight_grams, x.reps, true, false))
                    .max()
                    .unwrap_or(0),
                unit,
            ),
            seconds: sets.iter().map(|x| x.seconds).max().unwrap_or(0),
            distance_meters: sets.iter().map(|x| x.distance_meters).max().unwrap_or(0),
            estimated_1rm: e1rm.map(|g| volume(g, unit)),
            estimated_1rm_grams: e1rm,
        });
    }
    let chart = visible
        .history
        .iter()
        .take(14)
        .rev()
        .map(|w| chart_point(&w.id, w.finished_ms.unwrap(), workout_volume(w), unit, tz))
        .collect();
    // visible.history is newest first; keep each exercise's last 14 workouts.
    let mut exercise_charts: BTreeMap<String, Vec<ChartPoint>> = BTreeMap::new();
    for w in &visible.history {
        for e in &w.exercises {
            if !e
                .sets
                .iter()
                .any(|x| x.completed && x.kind != SetKind::Warmup)
            {
                continue;
            }
            let points = exercise_charts.entry(e.exercise_id.clone()).or_default();
            if points.len() < 14 {
                let grams = e
                    .sets
                    .iter()
                    .map(|x| {
                        kernel::set_volume(
                            x.weight_grams,
                            x.reps,
                            x.completed,
                            x.kind == SetKind::Warmup,
                        )
                    })
                    .sum();
                points.push(chart_point(&w.id, w.finished_ms.unwrap(), grams, unit, tz));
            }
        }
    }
    for points in exercise_charts.values_mut() {
        points.reverse();
    }
    let measurement_labels = state
        .measurements
        .iter()
        .map(|m| {
            let label = match m.kind.as_str() {
                "body_weight" => volume(m.value_milli, unit),
                "body_fat" => format!("{}%", number(m.value_milli as f64 / 1000.0)),
                _ => format!("{} cm", number(m.value_milli as f64 / 1000.0)),
            };
            (m.id.clone(), label)
        })
        .collect();
    View {
        state: visible,
        summary,
        records,
        chart,
        exercise_charts,
        rest_remaining: state
            .rest
            .as_ref()
            .map_or(0, |r| kernel::rest_remaining(r.deadline_ms, now)),
        weight_labels,
        previous_labels,
        measurement_labels,
        workout_labels,
    }
}

pub fn csv_export(s: &State) -> Result<String, String> {
    let mut csv = csv::Writer::from_writer(vec![]);
    csv.write_record([
        "workout_name",
        "started_utc",
        "finished_utc",
        "exercise",
        "set_order",
        "set_type",
        "weight_kg",
        "reps",
        "duration_seconds",
        "distance_meters",
        "notes",
    ])
    .map_err(|e| e.to_string())?;
    for w in &s.history {
        for e in &w.exercises {
            let definition = s.exercises.iter().find(|x| x.id == e.exercise_id).unwrap();
            for (i, set) in e.sets.iter().enumerate().filter(|(_, x)| x.completed) {
                let start = chrono::Utc
                    .timestamp_millis_opt(w.started_ms as i64)
                    .single()
                    .unwrap()
                    .to_rfc3339();
                let end = chrono::Utc
                    .timestamp_millis_opt(w.finished_ms.unwrap() as i64)
                    .single()
                    .unwrap()
                    .to_rfc3339();
                let kind = match set.kind {
                    SetKind::Normal => "normal",
                    SetKind::Warmup => "warmup",
                    SetKind::Drop => "drop",
                    SetKind::Failure => "failure",
                };
                csv.write_record([
                    w.title.clone(),
                    start,
                    end,
                    definition.name.clone(),
                    (i + 1).to_string(),
                    kind.into(),
                    weight(set.weight_grams, "kg"),
                    set.reps.to_string(),
                    set.seconds.to_string(),
                    set.distance_meters.to_string(),
                    format!("{}\n{}", w.notes, e.notes),
                ])
                .map_err(|e| e.to_string())?;
            }
        }
    }
    String::from_utf8(csv.into_inner().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
