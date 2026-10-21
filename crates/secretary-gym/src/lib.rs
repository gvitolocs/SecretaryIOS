//! Domain transitions, atomic persistence, replay protection and presentation.
pub mod catalog;
pub mod kernel;
pub mod models;
mod presentation;
pub use models::*;

use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_INPUT: usize = 16 * 1024 * 1024;
const MAX_TIME: u64 = 253_402_300_799_000;
type Result<T> = std::result::Result<T, String>;

pub struct Engine {
    state: State,
    path: Option<PathBuf>,
    _lock: Option<File>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(MAX_TIME as u128) as u64
}

impl Engine {
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn in_memory() -> Self {
        Self {
            state: catalog::initial_state(),
            path: None,
            _lock: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "This gym data file is already open in another process".to_string())?;
        let state = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            serde_json::from_slice::<State>(&bytes)
                .map_err(|e| format!("Saved data could not be read: {e}"))?
        } else {
            catalog::initial_state()
        };
        validate(&state)?;
        Ok(Self {
            state,
            path: Some(path),
            _lock: Some(lock),
        })
    }

    pub fn handle(&mut self, json: &str) -> Response {
        self.handle_at(json, now_ms())
    }

    pub fn handle_at(&mut self, json: &str, now: u64) -> Response {
        match self.process(json, now) {
            Ok(r) => r,
            Err((code, error)) => Response {
                ok: false,
                replayed: false,
                error: Some(error),
                code: Some(code.into()),
                view: None,
                text: None,
                exercises: None,
                clock: None,
            },
        }
    }

    fn process(
        &mut self,
        json: &str,
        now: u64,
    ) -> std::result::Result<Response, (&'static str, String)> {
        let invalid = |e: String| ("invalid_command", e);
        if json.len() > MAX_INPUT || now > MAX_TIME {
            return Err(invalid(
                "Request or timestamp exceeds supported bounds".into(),
            ));
        }
        let req: Request = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
        let tz: chrono_tz::Tz = req
            .timezone
            .parse()
            .map_err(|_| invalid("Unknown IANA timezone".into()))?;
        if matches!(req.command, Command::Clock) {
            return Ok(Response {
                ok: true,
                replayed: false,
                error: None,
                code: None,
                view: None,
                text: None,
                exercises: None,
                clock: Some(presentation::clock(&self.state, now)),
            });
        }
        let mut text = None;
        let mut exercises = None;
        let mut replayed = false;
        if req.command.is_query() {
            match req.command {
                Command::ExportJson => {
                    let mut backup = self.state.clone();
                    backup.receipts.clear();
                    text = Some(
                        serde_json::to_string_pretty(&backup)
                            .map_err(|e| invalid(e.to_string()))?,
                    );
                }
                Command::ExportCsv => {
                    text = Some(presentation::csv_export(&self.state).map_err(invalid)?)
                }
                Command::SearchExercises { query } => {
                    let q = query.trim().to_lowercase();
                    exercises = Some(
                        self.state
                            .exercises
                            .iter()
                            .filter(|e| {
                                format!("{} {} {}", e.name, e.muscle, e.equipment)
                                    .to_lowercase()
                                    .contains(&q)
                            })
                            .cloned()
                            .collect(),
                    );
                }
                _ => {}
            }
        } else {
            let op = req
                .op_id
                .filter(|id| !id.trim().is_empty() && id.len() <= 128)
                .ok_or_else(|| {
                    invalid("A nonempty op_id (at most 128 bytes) is required".into())
                })?;
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&req.command).map_err(|e| invalid(e.to_string()))?
                )
            );
            if let Some(previous) = self.state.receipts.get(&op) {
                if previous != &fingerprint {
                    return Err((
                        "op_id_reused",
                        "This operation ID was already used for a different command".into(),
                    ));
                }
                replayed = true;
            } else {
                if req.expected_revision != Some(self.state.revision) {
                    return Err((
                        "conflict",
                        "State changed. Refresh and retry the edit.".into(),
                    ));
                }
                // Hoare boundary: rejection never mutates live state. Only a fully
                // validated, durably written candidate can become the new state.
                let mut candidate = self.state.clone();
                apply(&mut candidate, req.command, now).map_err(invalid)?;
                candidate.revision = self
                    .state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("Revision limit reached".into()))?;
                candidate.receipts.insert(op, fingerprint);
                validate(&candidate).map_err(invalid)?;
                self.persist(&candidate).map_err(|e| ("storage_error", e))?;
                self.state = candidate;
            }
        }
        Ok(Response {
            ok: true,
            replayed,
            error: None,
            code: None,
            view: Some(presentation::view(&self.state, now, tz)),
            text,
            exercises,
            clock: None,
        })
    }

    fn persist(&self, state: &State) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
            serde_json::to_writer(&mut file, state).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            // No fallible step after commit: some mobile filesystems do not
            // support directory fsync, so this extra durability fence is best effort.
            if let Some(parent) = path.parent() {
                if let Ok(dir) = File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn name(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 120 {
        Err("Names must contain 1–120 bytes".into())
    } else {
        Ok(s.into())
    }
}
fn active(s: &mut State) -> Result<&mut Workout> {
    s.active.as_mut().ok_or("Start a workout first".into())
}
fn find_set<'a>(s: &'a mut State, id: &str) -> Result<&'a mut WorkoutSet> {
    active(s)?
        .exercises
        .iter_mut()
        .flat_map(|e| &mut e.sets)
        .find(|x| x.id == id)
        .ok_or("Set not found".into())
}
fn new_set(reps: u64, weight_grams: u64) -> WorkoutSet {
    WorkoutSet {
        id: id(),
        kind: SetKind::Normal,
        weight_grams,
        reps,
        seconds: 60,
        distance_meters: 1000,
        completed: false,
    }
}

fn adapt_set(set: &mut WorkoutSet, kind: &ExerciseKind) {
    match kind {
        ExerciseKind::WeightReps => {
            set.seconds = 0;
            set.distance_meters = 0;
        }
        ExerciseKind::RepsOnly => {
            set.weight_grams = 0;
            set.seconds = 0;
            set.distance_meters = 0;
        }
        ExerciseKind::Timed => {
            set.weight_grams = 0;
            set.reps = 0;
            set.distance_meters = 0;
        }
        ExerciseKind::DistanceTime => {
            set.weight_grams = 0;
            set.reps = 0;
        }
    }
}

fn previous_set<'a>(s: &'a State, exercise_id: &str) -> Option<&'a WorkoutSet> {
    s.history
        .iter()
        .filter_map(|w| {
            w.exercises
                .iter()
                .find(|e| e.exercise_id == exercise_id)
                .map(|e| (w, e))
        })
        .max_by_key(|(w, _)| w.finished_ms)
        .and_then(|(_, e)| {
            e.sets
                .iter()
                .rev()
                .find(|x| x.completed && x.kind != SetKind::Warmup)
        })
}

pub fn decimal_milli(input: &str) -> Result<u64> {
    let normalized = input.trim().replace(',', ".");
    let parts: Vec<_> = normalized.split('.').collect();
    if parts.is_empty()
        || parts.len() > 2
        || parts[0].is_empty()
        || !parts[0].bytes().all(|b| b.is_ascii_digit())
    {
        return Err("Enter a positive number with up to three decimal places".into());
    }
    let fraction = parts.get(1).copied().unwrap_or("");
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Use at most three decimal places".into());
    }
    let whole = parts[0].parse::<u64>().map_err(|_| "Number is too large")?;
    let fractional = format!("{fraction:0<3}")
        .parse::<u64>()
        .map_err(|_| "Invalid decimal")?;
    whole
        .checked_mul(1000)
        .and_then(|n| n.checked_add(fractional))
        .ok_or("Number is too large".into())
}

pub fn parse_weight(input: &str, unit: &str) -> Result<u64> {
    let value = decimal_milli(input)?;
    let grams = if unit == "kg" {
        value
    } else if unit == "lb" {
        ((value as u128 * 45_359_237 + 50_000_000) / 100_000_000)
            .try_into()
            .map_err(|_| "Weight is too large")?
    } else {
        return Err("Weight unit must be kg or lb".into());
    };
    if grams > 2_000_000 {
        return Err("Weight exceeds 2,000 kg".into());
    }
    Ok(grams)
}

fn apply(s: &mut State, command: Command, now: u64) -> Result<()> {
    match command {
        Command::StartWorkout { title, routine_id } => {
            if s.active.is_some() {
                return Err("Finish or discard your current workout first".into());
            }
            let mut exercises = vec![];
            if let Some(rid) = routine_id {
                let routine = s
                    .routines
                    .iter()
                    .find(|r| r.id == rid)
                    .ok_or("Routine not found")?;
                for template in &routine.exercises {
                    let previous = previous_set(s, &template.exercise_id);
                    let sets = (0..template.sets)
                        .map(|_| {
                            let mut set = new_set(template.reps, template.weight_grams);
                            if let Some(p) = previous {
                                set.weight_grams = p.weight_grams;
                                set.reps = p.reps;
                                set.seconds = p.seconds;
                                set.distance_meters = p.distance_meters;
                            }
                            if let Some(e) =
                                s.exercises.iter().find(|e| e.id == template.exercise_id)
                            {
                                adapt_set(&mut set, &e.kind);
                            }
                            set
                        })
                        .collect();
                    exercises.push(WorkoutExercise {
                        id: id(),
                        exercise_id: template.exercise_id.clone(),
                        notes: String::new(),
                        sets,
                    });
                }
            }
            s.active = Some(Workout {
                id: id(),
                title: name(&title)?,
                notes: String::new(),
                started_ms: now,
                finished_ms: None,
                exercises,
            });
            s.rest = None;
        }
        Command::RenameWorkout { title } => active(s)?.title = name(&title)?,
        Command::SetNotes { notes, exercise_id } => {
            if notes.len() > 8000 {
                return Err("Notes exceed 8,000 bytes".into());
            }
            if let Some(eid) = exercise_id {
                active(s)?
                    .exercises
                    .iter_mut()
                    .find(|e| e.id == eid)
                    .ok_or("Exercise not found")?
                    .notes = notes;
            } else {
                active(s)?.notes = notes;
            }
        }
        Command::AddExercise { exercise_id } => {
            if !s.exercises.iter().any(|e| e.id == exercise_id) {
                return Err("Exercise not found".into());
            }
            let previous = previous_set(s, &exercise_id).cloned();
            let kind = s
                .exercises
                .iter()
                .find(|e| e.id == exercise_id)
                .unwrap()
                .kind
                .clone();
            let w = active(s)?;
            if w.exercises.iter().any(|e| e.exercise_id == exercise_id) {
                return Err("This exercise is already in your workout".into());
            }
            let mut set = previous.unwrap_or_else(|| new_set(8, 0));
            set.id = id();
            set.completed = false;
            adapt_set(&mut set, &kind);
            w.exercises.push(WorkoutExercise {
                id: id(),
                exercise_id,
                notes: String::new(),
                sets: vec![set],
            });
        }
        Command::RemoveExercise { exercise_id } => {
            let w = active(s)?;
            let pos = w
                .exercises
                .iter()
                .position(|e| e.id == exercise_id)
                .ok_or("Exercise not found")?;
            w.exercises.remove(pos);
        }
        Command::ReorderExercises { ids } => {
            let w = active(s)?;
            let old: HashSet<_> = w.exercises.iter().map(|e| e.id.clone()).collect();
            let new: HashSet<_> = ids.iter().cloned().collect();
            if old != new || ids.len() != w.exercises.len() {
                return Err("Exercise order must be a permutation of existing IDs".into());
            }
            w.exercises = ids
                .iter()
                .map(|i| w.exercises.iter().find(|e| &e.id == i).unwrap().clone())
                .collect();
        }
        Command::AddSet { exercise_id } => {
            let e = active(s)?
                .exercises
                .iter_mut()
                .find(|e| e.id == exercise_id)
                .ok_or("Exercise not found")?;
            let mut set = e.sets.last().cloned().unwrap_or_else(|| new_set(8, 0));
            set.id = id();
            set.completed = false;
            e.sets.push(set);
        }
        Command::EditSet {
            set_id,
            weight,
            reps,
            seconds,
            distance_meters,
            kind,
        } => {
            let grams = parse_weight(&weight, &s.settings.weight_unit)?;
            let exercise_id = active(s)?
                .exercises
                .iter()
                .find(|e| e.sets.iter().any(|x| x.id == set_id))
                .ok_or("Set not found")?
                .exercise_id
                .clone();
            let exercise_kind = s
                .exercises
                .iter()
                .find(|e| e.id == exercise_id)
                .unwrap()
                .kind
                .clone();
            let set = find_set(s, &set_id)?;
            if set.completed {
                return Err("Uncheck this set before editing it".into());
            }
            set.weight_grams = grams;
            set.reps = reps;
            set.seconds = seconds;
            set.distance_meters = distance_meters;
            set.kind = kind;
            adapt_set(set, &exercise_kind);
        }
        Command::CompleteSet { set_id, completed } => {
            let duration = s.settings.rest_seconds;
            let set = find_set(s, &set_id)?;
            let was_completed = set.completed;
            set.completed = completed;
            if completed && !was_completed {
                s.rest = Some(RestTimer {
                    workout_id: active(s)?.id.clone(),
                    deadline_ms: kernel::rest_deadline(now, duration),
                });
            }
        }
        Command::RemoveSet { set_id } => {
            let w = active(s)?;
            let e = w
                .exercises
                .iter_mut()
                .find(|e| e.sets.iter().any(|x| x.id == set_id))
                .ok_or("Set not found")?;
            e.sets.retain(|x| x.id != set_id);
        }
        Command::FinishWorkout => {
            let w = active(s)?;
            if !w
                .exercises
                .iter()
                .flat_map(|e| &e.sets)
                .any(|x| x.completed)
            {
                return Err("Complete at least one set before finishing".into());
            }
            if now < w.started_ms {
                return Err("The clock moved backwards; retry after the workout start time".into());
            }
            let mut w = s.active.take().unwrap();
            w.finished_ms = Some(now);
            s.history.push(w);
            s.rest = None;
        }
        Command::CancelWorkout => {
            active(s)?;
            s.active = None;
            s.rest = None;
        }
        Command::SaveRoutine {
            name: n,
            exercise_ids,
            from_active,
        } => {
            let exercises = if from_active {
                active(s)?
                    .exercises
                    .iter()
                    .map(|e| {
                        let set = e.sets.first();
                        RoutineExercise {
                            exercise_id: e.exercise_id.clone(),
                            sets: e.sets.len().max(1) as u64,
                            reps: set.map_or(8, |x| x.reps),
                            weight_grams: set.map_or(0, |x| x.weight_grams),
                        }
                    })
                    .collect()
            } else {
                exercise_ids
                    .into_iter()
                    .map(|exercise_id| RoutineExercise {
                        exercise_id,
                        sets: 3,
                        reps: 8,
                        weight_grams: 0,
                    })
                    .collect()
            };
            s.routines.push(Routine {
                id: id(),
                name: name(&n)?,
                exercises,
            });
        }
        Command::DeleteRoutine { routine_id } => {
            let pos = s
                .routines
                .iter()
                .position(|r| r.id == routine_id)
                .ok_or("Routine not found")?;
            s.routines.remove(pos);
        }
        Command::DeleteWorkout { workout_id } => {
            let pos = s
                .history
                .iter()
                .position(|w| w.id == workout_id)
                .ok_or("Workout not found")?;
            s.history.remove(pos);
        }
        Command::CreateExercise {
            name: n,
            muscle,
            equipment,
            kind,
        } => s.exercises.push(Exercise {
            id: id(),
            name: name(&n)?,
            muscle: name(&muscle)?,
            equipment: name(&equipment)?,
            kind,
            custom: true,
        }),
        Command::StartRest { seconds } => {
            if !(1..=3600).contains(&seconds) {
                return Err("Rest time must be 1–3,600 seconds".into());
            }
            s.rest = Some(RestTimer {
                workout_id: active(s)?.id.clone(),
                deadline_ms: kernel::rest_deadline(now, seconds),
            });
        }
        Command::StopRest => s.rest = None,
        Command::UpdateSettings {
            weight_unit,
            rest_seconds,
            weekly_goal,
        } => {
            s.settings = Settings {
                weight_unit,
                rest_seconds,
                weekly_goal,
            }
        }
        Command::RecordMeasurement { kind, value } => {
            let value_milli = if kind == "body_weight" {
                parse_weight(&value, &s.settings.weight_unit)?
            } else {
                decimal_milli(&value)?
            };
            s.measurements.push(Measurement {
                id: id(),
                timestamp_ms: now,
                kind,
                value_milli,
            });
        }
        Command::DeleteMeasurement { measurement_id } => {
            let pos = s
                .measurements
                .iter()
                .position(|m| m.id == measurement_id)
                .ok_or("Measurement not found")?;
            s.measurements.remove(pos);
        }
        Command::RestoreJson { backup } => {
            let mut restored: State =
                serde_json::from_str(&backup).map_err(|e| format!("Invalid backup: {e}"))?;
            restored.receipts = s.receipts.clone();
            restored.revision = s.revision;
            restored.rest = None;
            validate(&restored)?;
            *s = restored;
        }
        _ => return Err("Queries cannot mutate state".into()),
    }
    Ok(())
}

pub fn validate(s: &State) -> Result<()> {
    if s.schema_version != 1 {
        return Err("Unsupported backup schema version".into());
    }
    if !["kg", "lb"].contains(&s.settings.weight_unit.as_str())
        || !(1..=3600).contains(&s.settings.rest_seconds)
        || !(1..=14).contains(&s.settings.weekly_goal)
    {
        return Err("Settings are outside their allowed ranges".into());
    }
    if s.exercises.len() > 2000
        || s.routines.len() > 200
        || s.history.len() > 10000
        || s.measurements.len() > 50000
        || s.receipts.len() > 100000
    {
        return Err("Data capacity reached; export a backup before archiving data".into());
    }
    let mut ids = HashSet::new();
    let mut unique = |value: &str| -> Result<()> {
        if value.is_empty() || value.len() > 128 || !ids.insert(value.to_string()) {
            Err("Empty or duplicate object ID".into())
        } else {
            Ok(())
        }
    };
    for e in &s.exercises {
        unique(&e.id)?;
        name(&e.name)?;
        name(&e.muscle)?;
        name(&e.equipment)?;
    }
    let catalog: HashSet<_> = s.exercises.iter().map(|e| e.id.as_str()).collect();
    for r in &s.routines {
        unique(&r.id)?;
        name(&r.name)?;
        let distinct: HashSet<_> = r.exercises.iter().map(|e| &e.exercise_id).collect();
        if r.exercises.is_empty() || r.exercises.len() > 30 || distinct.len() != r.exercises.len() {
            return Err("Routines need 1–30 distinct exercises".into());
        }
        for e in &r.exercises {
            if !catalog.contains(e.exercise_id.as_str())
                || !(1..=100).contains(&e.sets)
                || e.reps > 1000
                || e.weight_grams > 2_000_000
            {
                return Err("Invalid routine prescription".into());
            }
        }
    }
    for (w, finished) in s
        .history
        .iter()
        .map(|w| (w, true))
        .chain(s.active.iter().map(|w| (w, false)))
    {
        unique(&w.id)?;
        name(&w.title)?;
        if w.started_ms > MAX_TIME
            || w.finished_ms
                .is_some_and(|end| end < w.started_ms || end > MAX_TIME)
            || w.finished_ms.is_some() != finished
            || w.notes.len() > 8000
            || w.exercises.len() > 30
        {
            return Err("Invalid workout lifecycle or size".into());
        }
        let distinct: HashSet<_> = w.exercises.iter().map(|e| &e.exercise_id).collect();
        if distinct.len() != w.exercises.len() {
            return Err("Duplicate exercise in workout".into());
        }
        if finished
            && !w
                .exercises
                .iter()
                .flat_map(|e| &e.sets)
                .any(|x| x.completed)
        {
            return Err("Finished workouts need a completed set".into());
        }
        for e in &w.exercises {
            unique(&e.id)?;
            let definition = s
                .exercises
                .iter()
                .find(|x| x.id == e.exercise_id)
                .ok_or("Unknown exercise reference")?;
            if e.notes.len() > 8000 || e.sets.len() > 100 {
                return Err("Exercise exceeds its note or set limit".into());
            }
            for set in &e.sets {
                unique(&set.id)?;
                if set.weight_grams > 2_000_000
                    || set.reps > 1000
                    || set.seconds > 86400
                    || set.distance_meters > 1_000_000
                {
                    return Err("Set values exceed supported limits".into());
                }
                if set.completed {
                    let valid = match definition.kind {
                        ExerciseKind::WeightReps | ExerciseKind::RepsOnly => set.reps > 0,
                        ExerciseKind::Timed => set.seconds > 0,
                        ExerciseKind::DistanceTime => set.seconds > 0 && set.distance_meters > 0,
                    };
                    if !valid {
                        return Err("Completed sets need positive reps, time, or distance for their exercise type".into());
                    }
                }
            }
        }
    }
    for m in &s.measurements {
        unique(&m.id)?;
        let bound = match m.kind.as_str() {
            "body_weight" => 2_000_000,
            "body_fat" => 100_000,
            "waist" => 500_000,
            _ => return Err("Unknown measurement type".into()),
        };
        if m.timestamp_ms > MAX_TIME || m.value_milli == 0 || m.value_milli > bound {
            return Err("Measurement is outside its allowed range".into());
        }
    }
    if let Some(rest) = &s.rest {
        if s.active.as_ref().map(|w| &w.id) != Some(&rest.workout_id)
            || rest.deadline_ms > MAX_TIME + 3_600_000
        {
            return Err("Rest timer must belong to the active workout".into());
        }
    }
    Ok(())
}
