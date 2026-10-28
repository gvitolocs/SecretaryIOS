use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExerciseKind {
    WeightReps,
    RepsOnly,
    Timed,
    DistanceTime,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Exercise {
    pub id: String,
    pub name: String,
    pub muscle: String,
    pub equipment: String,
    pub kind: ExerciseKind,
    pub custom: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetKind {
    Normal,
    Warmup,
    Drop,
    Failure,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkoutSet {
    pub id: String,
    pub kind: SetKind,
    pub weight_grams: u64,
    pub reps: u64,
    pub seconds: u64,
    pub distance_meters: u64,
    pub completed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkoutExercise {
    pub id: String,
    pub exercise_id: String,
    pub notes: String,
    pub sets: Vec<WorkoutSet>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workout {
    pub id: String,
    pub title: String,
    pub notes: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub exercises: Vec<WorkoutExercise>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineExercise {
    pub exercise_id: String,
    pub sets: u64,
    pub reps: u64,
    pub weight_grams: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Routine {
    pub id: String,
    pub name: String,
    pub exercises: Vec<RoutineExercise>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub weight_unit: String,
    pub rest_seconds: u64,
    pub weekly_goal: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Measurement {
    pub id: String,
    pub timestamp_ms: u64,
    pub kind: String,
    pub value_milli: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestTimer {
    pub workout_id: String,
    pub deadline_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: Settings,
    pub exercises: Vec<Exercise>,
    pub routines: Vec<Routine>,
    pub active: Option<Workout>,
    pub history: Vec<Workout>,
    pub measurements: Vec<Measurement>,
    pub rest: Option<RestTimer>,
    #[serde(default)]
    pub receipts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    #[serde(default)]
    pub op_id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default = "utc")]
    pub timezone: String,
    pub command: Command,
}
fn utc() -> String {
    "UTC".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Snapshot,
    Clock,
    SearchExercises {
        query: String,
    },
    /// Query: parse a decimal weight string to integer grams.
    ParseWeight {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    ExportJson,
    ExportCsv,
    StartWorkout {
        title: String,
        #[serde(default)]
        routine_id: Option<String>,
    },
    /// Start a new active workout that copies a finished one.
    RepeatWorkout {
        workout_id: String,
    },
    RenameWorkout {
        title: String,
    },
    SetNotes {
        notes: String,
        #[serde(default)]
        exercise_id: Option<String>,
    },
    AddExercise {
        exercise_id: String,
    },
    RemoveExercise {
        exercise_id: String,
    },
    ReorderExercises {
        ids: Vec<String>,
    },
    AddSet {
        exercise_id: String,
    },
    EditSet {
        set_id: String,
        /// Decimal string in the display unit; exactly one of `weight` and
        /// `weight_grams`. Absent fields are not serialized, so the receipt
        /// fingerprint of the original string-only form is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_grams: Option<u64>,
        reps: u64,
        seconds: u64,
        distance_meters: u64,
        kind: SetKind,
    },
    CompleteSet {
        set_id: String,
        completed: bool,
    },
    RemoveSet {
        set_id: String,
    },
    FinishWorkout,
    CancelWorkout,
    SaveRoutine {
        name: String,
        #[serde(default)]
        exercise_ids: Vec<String>,
        #[serde(default)]
        from_active: bool,
    },
    /// Rename a routine and/or replace its exercises (in the given order).
    /// Exercises it already had keep their sets, reps and weight; new ones
    /// start at 3 × 8. Absent fields are left as they are.
    UpdateRoutine {
        routine_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exercise_ids: Option<Vec<String>>,
    },
    DeleteRoutine {
        routine_id: String,
    },
    DeleteWorkout {
        workout_id: String,
    },
    CreateExercise {
        name: String,
        muscle: String,
        equipment: String,
        kind: ExerciseKind,
    },
    StartRest {
        seconds: u64,
    },
    StopRest,
    UpdateSettings {
        weight_unit: String,
        rest_seconds: u64,
        weekly_goal: u64,
    },
    RecordMeasurement {
        kind: String,
        value: String,
    },
    DeleteMeasurement {
        measurement_id: String,
    },
    RestoreJson {
        backup: String,
    },
}

impl Command {
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Snapshot
                | Self::Clock
                | Self::SearchExercises { .. }
                | Self::ParseWeight { .. }
                | Self::ExportJson
                | Self::ExportCsv
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Summary {
    pub workouts: usize,
    pub completed_sets: usize,
    pub weekly_workouts: usize,
    pub weekly_goal: u64,
    pub volume: String,
    pub active_volume: String,
    pub active_duration: String,
    pub active_elapsed_seconds: u64,
    pub active_completed_sets: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub exercise_id: String,
    pub name: String,
    pub weight: String,
    pub reps: u64,
    pub volume: String,
    pub seconds: u64,
    pub distance_meters: u64,
    /// Best Epley estimate over completed non-warmup weighted sets.
    pub estimated_1rm: Option<String>,
    pub estimated_1rm_grams: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChartPoint {
    pub id: String,
    pub label: String,
    pub volume: f64,
    pub volume_grams: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkoutSummary {
    pub sets: usize,
    pub volume: String,
    pub duration: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct View {
    pub state: State,
    pub summary: Summary,
    pub records: Vec<Record>,
    pub chart: Vec<ChartPoint>,
    /// Catalog exercise id → its volume in each of its last 14 workouts.
    pub exercise_charts: BTreeMap<String, Vec<ChartPoint>>,
    pub rest_remaining: u64,
    pub weight_labels: BTreeMap<String, String>,
    pub previous_labels: BTreeMap<String, String>,
    pub measurement_labels: BTreeMap<String, String>,
    pub workout_labels: BTreeMap<String, WorkoutSummary>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub replayed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<View>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exercises: Option<Vec<Exercise>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock: Option<Clock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_grams: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Clock {
    pub rest_remaining: u64,
    pub active_duration: String,
    pub active_elapsed_seconds: u64,
}
