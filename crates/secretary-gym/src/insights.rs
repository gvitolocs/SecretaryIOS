//! Compact, read-only training summary for Lucina (`GET /v1/insights/summary`).
use crate::{models::*, presentation};
use chrono::{Datelike, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonalRecord {
    pub exercise_id: String,
    pub name: String,
    pub weight_grams: u64,
    pub reps: u64,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkoutInsight {
    pub id: String,
    pub title: String,
    pub finished_ms: u64,
    pub date: String,
    pub duration_min: u64,
    pub completed_sets: usize,
    pub volume_grams: u64,
    pub volume_label: String,
    pub exercises: Vec<String>,
    pub prs: Vec<PersonalRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WeekVolume {
    pub week_start: String,
    pub workouts: usize,
    pub volume_grams: u64,
    pub volume_label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GymInsights {
    pub unit: String,
    pub weekly_goal: u64,
    pub workouts_this_week: usize,
    pub recent_workouts: Vec<WorkoutInsight>,
    pub weekly_volume: Vec<WeekVolume>,
}

pub const WEEKS: i64 = 8;

fn date(ms: u64, tz: chrono_tz::Tz) -> NaiveDate {
    tz.timestamp_millis_opt(ms as i64)
        .single()
        .unwrap()
        .date_naive()
}

fn monday(d: NaiveDate) -> NaiveDate {
    d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64)
}

/// The heaviest completed non-warmup set (ties: more reps) of one workout exercise.
fn top_set(e: &WorkoutExercise) -> Option<(u64, u64)> {
    e.sets
        .iter()
        .filter(|x| x.completed && x.kind != SetKind::Warmup && x.reps > 0)
        .map(|x| (x.weight_grams, x.reps))
        .max()
}

/// `recent` (clamped to 1..=20) newest finished workouts with volume and PRs,
/// plus zero-filled weekly volume for the last [`WEEKS`] weeks.
pub fn summary(state: &State, now: u64, tz: chrono_tz::Tz, recent: usize) -> GymInsights {
    let unit = state.settings.weight_unit.as_str();
    let names: HashMap<&str, &Exercise> =
        state.exercises.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut finished: Vec<&Workout> = state.history.iter().collect();
    finished.sort_by(|a, b| (a.finished_ms, &a.id).cmp(&(b.finished_ms, &b.id)));

    // PRs in chronological order: best weight per exercise before each workout.
    let mut best: HashMap<&str, u64> = HashMap::new();
    let mut prs: HashMap<&str, Vec<PersonalRecord>> = HashMap::new();
    for w in &finished {
        let mut found = vec![];
        for e in &w.exercises {
            let Some(def) = names.get(e.exercise_id.as_str()) else {
                continue;
            };
            if def.kind != ExerciseKind::WeightReps {
                continue;
            }
            let Some((weight, reps)) = top_set(e) else {
                continue;
            };
            if weight == 0 {
                continue;
            }
            let previous = best.get(e.exercise_id.as_str()).copied();
            if previous.is_some_and(|p| weight > p) {
                found.push(PersonalRecord {
                    exercise_id: e.exercise_id.clone(),
                    name: def.name.clone(),
                    weight_grams: weight,
                    reps,
                    label: format!("{} {unit} × {reps}", presentation::weight(weight, unit)),
                });
            }
            if previous.is_none_or(|p| weight > p) {
                best.insert(e.exercise_id.as_str(), weight);
            }
        }
        prs.insert(w.id.as_str(), found);
    }

    let recent_workouts = finished
        .iter()
        .rev()
        .take(recent.clamp(1, 20))
        .map(|w| {
            let end = w.finished_ms.unwrap_or(w.started_ms);
            let grams = presentation::workout_volume(w);
            WorkoutInsight {
                id: w.id.clone(),
                title: w.title.clone(),
                finished_ms: end,
                date: date(end, tz).format("%Y-%m-%d").to_string(),
                duration_min: end.saturating_sub(w.started_ms) / 60_000,
                completed_sets: presentation::completed(w),
                volume_grams: grams,
                volume_label: presentation::volume(grams, unit),
                exercises: w
                    .exercises
                    .iter()
                    .map(|e| {
                        names
                            .get(e.exercise_id.as_str())
                            .map_or_else(|| e.exercise_id.clone(), |d| d.name.clone())
                    })
                    .collect(),
                prs: prs.remove(w.id.as_str()).unwrap_or_default(),
            }
        })
        .collect();

    let this_week = monday(date(now, tz));
    let mut weekly_volume: Vec<WeekVolume> = (0..WEEKS)
        .rev()
        .map(|k| WeekVolume {
            week_start: (this_week - chrono::Duration::days(7 * k))
                .format("%Y-%m-%d")
                .to_string(),
            workouts: 0,
            volume_grams: 0,
            volume_label: String::new(),
        })
        .collect();
    for w in &finished {
        let week = monday(date(w.finished_ms.unwrap_or(w.started_ms), tz));
        let k = (this_week - week).num_days() / 7;
        if (0..WEEKS).contains(&k) && week <= this_week {
            let slot = &mut weekly_volume[(WEEKS - 1 - k) as usize];
            slot.workouts += 1;
            slot.volume_grams += presentation::workout_volume(w);
        }
    }
    for slot in &mut weekly_volume {
        slot.volume_label = presentation::volume(slot.volume_grams, unit);
    }
    GymInsights {
        unit: unit.to_string(),
        weekly_goal: state.settings.weekly_goal,
        workouts_this_week: weekly_volume.last().map_or(0, |s| s.workouts),
        recent_workouts,
        weekly_volume,
    }
}
