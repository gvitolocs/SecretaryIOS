import Foundation

// Wire types of the gym engine (crates/secretary-gym, docs/server-api.md "Gym module").
// Everything is snake_case with integer units (grams, seconds, meters, milliseconds).
// The view is the source of truth: labels, totals and records come from the engine.
// Fields marked "newer engine" are decoded leniently so older engines still work.

extension KeyedDecodingContainer {
    /// Decodes an optional field, falling back to `fallback` when it is missing, null or mistyped.
    func lenient<T: Decodable>(_ type: T.Type, _ key: Key, _ fallback: T) -> T {
        ((try? decodeIfPresent(T.self, forKey: key)) ?? nil) ?? fallback
    }

    func lenientOptional<T: Decodable>(_ type: T.Type, _ key: Key) -> T? {
        (try? decodeIfPresent(T.self, forKey: key)) ?? nil
    }
}

enum ExerciseKind: String, CaseIterable, Sendable {
    case weightReps = "weight_reps"
    case repsOnly = "reps_only"
    case timed
    case distanceTime = "distance_time"

    var title: String {
        switch self {
        case .weightReps: return "Weight & reps"
        case .repsOnly: return "Reps only"
        case .timed: return "Time"
        case .distanceTime: return "Distance & time"
        }
    }
}

enum SetKind: String, CaseIterable, Sendable {
    case normal, warmup, drop, failure

    var title: String { rawValue.capitalized }
}

struct Exercise: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let name: String
    let muscle: String
    let equipment: String
    let kind: String
    let custom: Bool

    var exerciseKind: ExerciseKind { ExerciseKind(rawValue: kind) ?? .weightReps }

    init(id: String, name: String, muscle: String, equipment: String, kind: String, custom: Bool) {
        self.id = id
        self.name = name
        self.muscle = muscle
        self.equipment = equipment
        self.kind = kind
        self.custom = custom
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        muscle = c.lenient(String.self, .muscle, "")
        equipment = c.lenient(String.self, .equipment, "")
        kind = c.lenient(String.self, .kind, ExerciseKind.weightReps.rawValue)
        custom = c.lenient(Bool.self, .custom, false)
    }

    private enum CodingKeys: String, CodingKey { case id, name, muscle, equipment, kind, custom }
}

struct WorkoutSet: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let kind: String
    let weightGrams: Int
    let reps: Int
    let seconds: Int
    let distanceMeters: Int
    let completed: Bool

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = c.lenient(String.self, .kind, SetKind.normal.rawValue)
        weightGrams = c.lenient(Int.self, .weightGrams, 0)
        reps = c.lenient(Int.self, .reps, 0)
        seconds = c.lenient(Int.self, .seconds, 0)
        distanceMeters = c.lenient(Int.self, .distanceMeters, 0)
        completed = c.lenient(Bool.self, .completed, false)
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, reps, seconds, completed
        case weightGrams = "weight_grams"
        case distanceMeters = "distance_meters"
    }
}

struct WorkoutExercise: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let exerciseId: String
    let notes: String
    let sets: [WorkoutSet]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        exerciseId = try c.decode(String.self, forKey: .exerciseId)
        notes = c.lenient(String.self, .notes, "")
        sets = try c.decodeIfPresent([WorkoutSet].self, forKey: .sets) ?? []
    }

    private enum CodingKeys: String, CodingKey {
        case id, notes, sets
        case exerciseId = "exercise_id"
    }
}

struct Workout: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let notes: String
    let startedMs: Int
    let finishedMs: Int?
    let exercises: [WorkoutExercise]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = c.lenient(String.self, .title, "Workout")
        notes = c.lenient(String.self, .notes, "")
        startedMs = c.lenient(Int.self, .startedMs, 0)
        finishedMs = c.lenientOptional(Int.self, .finishedMs)
        exercises = try c.decodeIfPresent([WorkoutExercise].self, forKey: .exercises) ?? []
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, notes, exercises
        case startedMs = "started_ms"
        case finishedMs = "finished_ms"
    }
}

struct RoutineExercise: Decodable, Equatable, Sendable {
    let exerciseId: String
    let sets: Int
    let reps: Int
    let weightGrams: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        exerciseId = try c.decode(String.self, forKey: .exerciseId)
        sets = c.lenient(Int.self, .sets, 0)
        reps = c.lenient(Int.self, .reps, 0)
        weightGrams = c.lenient(Int.self, .weightGrams, 0)
    }

    private enum CodingKeys: String, CodingKey {
        case sets, reps
        case exerciseId = "exercise_id"
        case weightGrams = "weight_grams"
    }
}

struct Routine: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let exercises: [RoutineExercise]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenient(String.self, .name, "Routine")
        exercises = try c.decodeIfPresent([RoutineExercise].self, forKey: .exercises) ?? []
    }

    private enum CodingKeys: String, CodingKey { case id, name, exercises }
}

struct GymSettings: Decodable, Equatable, Sendable {
    let weightUnit: String
    let restSeconds: Int
    let weeklyGoal: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        weightUnit = c.lenient(String.self, .weightUnit, "kg")
        restSeconds = c.lenient(Int.self, .restSeconds, 90)
        weeklyGoal = c.lenient(Int.self, .weeklyGoal, 3)
    }

    private enum CodingKeys: String, CodingKey {
        case weightUnit = "weight_unit"
        case restSeconds = "rest_seconds"
        case weeklyGoal = "weekly_goal"
    }
}

struct Measurement: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let timestampMs: Int
    let kind: String
    let valueMilli: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        timestampMs = c.lenient(Int.self, .timestampMs, 0)
        kind = c.lenient(String.self, .kind, "")
        valueMilli = c.lenient(Int.self, .valueMilli, 0)
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind
        case timestampMs = "timestamp_ms"
        case valueMilli = "value_milli"
    }
}

struct RestTimer: Decodable, Equatable, Sendable {
    let workoutId: String
    let deadlineMs: Int

    private enum CodingKeys: String, CodingKey {
        case workoutId = "workout_id"
        case deadlineMs = "deadline_ms"
    }
}

struct GymState: Decodable, Equatable, Sendable {
    let schemaVersion: Int
    let revision: Int
    let settings: GymSettings
    let exercises: [Exercise]
    let routines: [Routine]
    let active: Workout?
    let history: [Workout]
    let measurements: [Measurement]
    let rest: RestTimer?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = c.lenient(Int.self, .schemaVersion, 1)
        revision = try c.decode(Int.self, forKey: .revision)
        settings = try c.decode(GymSettings.self, forKey: .settings)
        exercises = try c.decodeIfPresent([Exercise].self, forKey: .exercises) ?? []
        routines = try c.decodeIfPresent([Routine].self, forKey: .routines) ?? []
        active = try c.decodeIfPresent(Workout.self, forKey: .active)
        history = try c.decodeIfPresent([Workout].self, forKey: .history) ?? []
        measurements = try c.decodeIfPresent([Measurement].self, forKey: .measurements) ?? []
        rest = c.lenientOptional(RestTimer.self, .rest)
    }

    private enum CodingKeys: String, CodingKey {
        case revision, settings, exercises, routines, active, history, measurements, rest
        case schemaVersion = "schema_version"
    }
}

struct GymSummary: Decodable, Equatable, Sendable {
    let workouts: Int
    let completedSets: Int
    let weeklyWorkouts: Int
    let weeklyGoal: Int
    let volume: String
    let activeVolume: String
    let activeDuration: String
    /// Newer engine; tick it locally between refreshes.
    let activeElapsedSeconds: Int?
    let activeCompletedSets: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        workouts = c.lenient(Int.self, .workouts, 0)
        completedSets = c.lenient(Int.self, .completedSets, 0)
        weeklyWorkouts = c.lenient(Int.self, .weeklyWorkouts, 0)
        weeklyGoal = c.lenient(Int.self, .weeklyGoal, 0)
        volume = c.lenient(String.self, .volume, "")
        activeVolume = c.lenient(String.self, .activeVolume, "")
        activeDuration = c.lenient(String.self, .activeDuration, "")
        activeElapsedSeconds = c.lenientOptional(Int.self, .activeElapsedSeconds)
        activeCompletedSets = c.lenient(Int.self, .activeCompletedSets, 0)
    }

    private enum CodingKeys: String, CodingKey {
        case workouts, volume
        case completedSets = "completed_sets"
        case weeklyWorkouts = "weekly_workouts"
        case weeklyGoal = "weekly_goal"
        case activeVolume = "active_volume"
        case activeDuration = "active_duration"
        case activeElapsedSeconds = "active_elapsed_seconds"
        case activeCompletedSets = "active_completed_sets"
    }
}

struct PersonalRecord: Decodable, Identifiable, Equatable, Sendable {
    var id: String { exerciseId }
    let exerciseId: String
    let name: String
    let weight: String
    let reps: Int
    let volume: String
    let seconds: Int
    let distanceMeters: Int
    /// Newer engine (Epley estimate label).
    let estimated1rm: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        exerciseId = try c.decode(String.self, forKey: .exerciseId)
        name = c.lenient(String.self, .name, "")
        weight = c.lenient(String.self, .weight, "")
        reps = c.lenient(Int.self, .reps, 0)
        volume = c.lenient(String.self, .volume, "")
        seconds = c.lenient(Int.self, .seconds, 0)
        distanceMeters = c.lenient(Int.self, .distanceMeters, 0)
        estimated1rm = c.lenientOptional(String.self, .estimated1rm)
    }

    private enum CodingKeys: String, CodingKey {
        case name, weight, reps, volume, seconds
        case exerciseId = "exercise_id"
        case distanceMeters = "distance_meters"
        case estimated1rm = "estimated_1rm"
    }
}

struct ChartPoint: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let label: String
    /// In the display unit.
    let volume: Double

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = c.lenient(String.self, .label, "")
        volume = c.lenient(Double.self, .volume, 0)
    }

    private enum CodingKeys: String, CodingKey { case id, label, volume }
}

struct WorkoutLabel: Decodable, Equatable, Sendable {
    let sets: Int
    let volume: String
    let duration: String

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sets = c.lenient(Int.self, .sets, 0)
        volume = c.lenient(String.self, .volume, "")
        duration = c.lenient(String.self, .duration, "")
    }

    private enum CodingKeys: String, CodingKey { case sets, volume, duration }
}

struct GymView: Decodable, Equatable, Sendable {
    let state: GymState
    let summary: GymSummary
    let records: [PersonalRecord]
    let chart: [ChartPoint]
    /// Newer engine: per catalog exercise.
    let exerciseCharts: [String: [ChartPoint]]
    let restRemaining: Int
    let weightLabels: [String: String]
    let previousLabels: [String: String]
    let measurementLabels: [String: String]
    let workoutLabels: [String: WorkoutLabel]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        state = try c.decode(GymState.self, forKey: .state)
        summary = try c.decode(GymSummary.self, forKey: .summary)
        records = c.lenient([PersonalRecord].self, .records, [])
        chart = c.lenient([ChartPoint].self, .chart, [])
        exerciseCharts = c.lenient([String: [ChartPoint]].self, .exerciseCharts, [:])
        restRemaining = c.lenient(Int.self, .restRemaining, 0)
        weightLabels = c.lenient([String: String].self, .weightLabels, [:])
        previousLabels = c.lenient([String: String].self, .previousLabels, [:])
        measurementLabels = c.lenient([String: String].self, .measurementLabels, [:])
        workoutLabels = c.lenient([String: WorkoutLabel].self, .workoutLabels, [:])
    }

    private enum CodingKeys: String, CodingKey {
        case state, summary, records, chart
        case exerciseCharts = "exercise_charts"
        case restRemaining = "rest_remaining"
        case weightLabels = "weight_labels"
        case previousLabels = "previous_labels"
        case measurementLabels = "measurement_labels"
        case workoutLabels = "workout_labels"
    }

    func exercise(_ id: String) -> Exercise? { state.exercises.first { $0.id == id } }

    var unit: String { state.settings.weightUnit }
}

struct GymClock: Decodable, Equatable, Sendable {
    let restRemaining: Int
    let activeDuration: String
    let activeElapsedSeconds: Int?

    init(restRemaining: Int, activeDuration: String, activeElapsedSeconds: Int?) {
        self.restRemaining = restRemaining
        self.activeDuration = activeDuration
        self.activeElapsedSeconds = activeElapsedSeconds
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        restRemaining = c.lenient(Int.self, .restRemaining, 0)
        activeDuration = c.lenient(String.self, .activeDuration, "")
        activeElapsedSeconds = c.lenientOptional(Int.self, .activeElapsedSeconds)
    }

    private enum CodingKeys: String, CodingKey {
        case restRemaining = "rest_remaining"
        case activeDuration = "active_duration"
        case activeElapsedSeconds = "active_elapsed_seconds"
    }
}

/// The engine response, for writes and queries alike.
struct GymResponse: Decodable, Equatable, EngineResponse {
    let ok: Bool
    let replayed: Bool
    let code: String?
    let error: String?
    let view: GymView?
    let text: String?
    let exercises: [Exercise]?
    let clock: GymClock?
    let weightGrams: Int?

    var viewRevision: Int? { view?.state.revision }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = c.lenient(Bool.self, .ok, false)
        replayed = c.lenient(Bool.self, .replayed, false)
        code = c.lenientOptional(String.self, .code)
        error = c.lenientOptional(String.self, .error)
        view = try c.decodeIfPresent(GymView.self, forKey: .view)
        text = c.lenientOptional(String.self, .text)
        exercises = c.lenientOptional([Exercise].self, .exercises)
        clock = c.lenientOptional(GymClock.self, .clock)
        weightGrams = c.lenientOptional(Int.self, .weightGrams)
    }

    private enum CodingKeys: String, CodingKey {
        case ok, replayed, code, error, view, text, exercises, clock
        case weightGrams = "weight_grams"
    }
}

/// Gym commands (docs/server-api.md, "Gym module" table). Encoded as
/// `{"type": "<snake_case>", ...fields}`.
enum GymCommand: Encodable, Equatable, Sendable {
    case snapshot
    case clock
    case startWorkout(title: String, routineId: String?)
    case repeatWorkout(workoutId: String)
    case renameWorkout(title: String)
    case setNotes(notes: String, exerciseId: String?)
    case addExercise(exerciseId: String)
    case removeExercise(exerciseId: String)
    case addSet(exerciseId: String)
    /// `weight` is the text the user typed in the display unit; the engine parses it.
    case editSet(setId: String, weight: String, reps: Int, seconds: Int, distanceMeters: Int, kind: String)
    case completeSet(setId: String, completed: Bool)
    case removeSet(setId: String)
    case finishWorkout
    case cancelWorkout
    case saveRoutine(name: String, exerciseIds: [String]?, fromActive: Bool)
    case deleteRoutine(routineId: String)
    case deleteWorkout(workoutId: String)
    case createExercise(name: String, muscle: String, equipment: String, kind: String)
    case startRest(seconds: Int)
    case stopRest
    case updateSettings(weightUnit: String, restSeconds: Int, weeklyGoal: Int)
    case recordMeasurement(kind: String, value: String)
    case deleteMeasurement(measurementId: String)
    case exportJSON
    case exportCSV
    case restoreJSON(backup: String)

    /// Queries never change state and need no op_id or revision.
    var isQuery: Bool {
        switch self {
        case .snapshot, .clock, .exportJSON, .exportCSV: return true
        default: return false
        }
    }

    var type: String {
        switch self {
        case .snapshot: return "snapshot"
        case .clock: return "clock"
        case .startWorkout: return "start_workout"
        case .repeatWorkout: return "repeat_workout"
        case .renameWorkout: return "rename_workout"
        case .setNotes: return "set_notes"
        case .addExercise: return "add_exercise"
        case .removeExercise: return "remove_exercise"
        case .addSet: return "add_set"
        case .editSet: return "edit_set"
        case .completeSet: return "complete_set"
        case .removeSet: return "remove_set"
        case .finishWorkout: return "finish_workout"
        case .cancelWorkout: return "cancel_workout"
        case .saveRoutine: return "save_routine"
        case .deleteRoutine: return "delete_routine"
        case .deleteWorkout: return "delete_workout"
        case .createExercise: return "create_exercise"
        case .startRest: return "start_rest"
        case .stopRest: return "stop_rest"
        case .updateSettings: return "update_settings"
        case .recordMeasurement: return "record_measurement"
        case .deleteMeasurement: return "delete_measurement"
        case .exportJSON: return "export_json"
        case .exportCSV: return "export_csv"
        case .restoreJSON: return "restore_json"
        }
    }

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ string: String) { stringValue = string }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(type, forKey: Key("type"))
        switch self {
        case .snapshot, .clock, .finishWorkout, .cancelWorkout, .stopRest, .exportJSON, .exportCSV:
            break
        case let .startWorkout(title, routineId):
            try c.encode(title, forKey: Key("title"))
            try c.encodeIfPresent(routineId, forKey: Key("routine_id"))
        case let .repeatWorkout(workoutId):
            try c.encode(workoutId, forKey: Key("workout_id"))
        case let .renameWorkout(title):
            try c.encode(title, forKey: Key("title"))
        case let .setNotes(notes, exerciseId):
            try c.encode(notes, forKey: Key("notes"))
            try c.encodeIfPresent(exerciseId, forKey: Key("exercise_id"))
        case let .addExercise(id), let .removeExercise(id), let .addSet(id):
            try c.encode(id, forKey: Key("exercise_id"))
        case let .editSet(setId, weight, reps, seconds, distance, kind):
            try c.encode(setId, forKey: Key("set_id"))
            try c.encode(weight, forKey: Key("weight"))
            try c.encode(reps, forKey: Key("reps"))
            try c.encode(seconds, forKey: Key("seconds"))
            try c.encode(distance, forKey: Key("distance_meters"))
            try c.encode(kind, forKey: Key("kind"))
        case let .completeSet(setId, completed):
            try c.encode(setId, forKey: Key("set_id"))
            try c.encode(completed, forKey: Key("completed"))
        case let .removeSet(setId):
            try c.encode(setId, forKey: Key("set_id"))
        case let .saveRoutine(name, exerciseIds, fromActive):
            try c.encode(name, forKey: Key("name"))
            try c.encodeIfPresent(exerciseIds, forKey: Key("exercise_ids"))
            if fromActive { try c.encode(true, forKey: Key("from_active")) }
        case let .deleteRoutine(id):
            try c.encode(id, forKey: Key("routine_id"))
        case let .deleteWorkout(id):
            try c.encode(id, forKey: Key("workout_id"))
        case let .createExercise(name, muscle, equipment, kind):
            try c.encode(name, forKey: Key("name"))
            try c.encode(muscle, forKey: Key("muscle"))
            try c.encode(equipment, forKey: Key("equipment"))
            try c.encode(kind, forKey: Key("kind"))
        case let .startRest(seconds):
            try c.encode(seconds, forKey: Key("seconds"))
        case let .updateSettings(unit, rest, goal):
            try c.encode(unit, forKey: Key("weight_unit"))
            try c.encode(rest, forKey: Key("rest_seconds"))
            try c.encode(goal, forKey: Key("weekly_goal"))
        case let .recordMeasurement(kind, value):
            try c.encode(kind, forKey: Key("kind"))
            try c.encode(value, forKey: Key("value"))
        case let .deleteMeasurement(id):
            try c.encode(id, forKey: Key("measurement_id"))
        case let .restoreJSON(backup):
            try c.encode(backup, forKey: Key("backup"))
        }
    }
}
