import type { Exercise, GymView, Workout } from "../../api/gym";

// Test fixtures for the Gym screens (shape of lucina-gym-core's View).

export const CATALOG: Exercise[] = [
  { id: "bench-press", name: "Bench Press", muscle: "Chest", equipment: "Barbell", kind: "weight_reps", custom: false },
  { id: "incline-press", name: "Incline Dumbbell Press", muscle: "Chest", equipment: "Dumbbell", kind: "weight_reps", custom: false },
  { id: "overhead-press", name: "Overhead Press", muscle: "Shoulders", equipment: "Barbell", kind: "weight_reps", custom: false },
  { id: "plank", name: "Plank", muscle: "Core", equipment: "Bodyweight", kind: "timed", custom: false },
  { id: "push-up", name: "Push Up", muscle: "Chest", equipment: "Bodyweight", kind: "reps_only", custom: false },
  { id: "run", name: "Running", muscle: "Cardio", equipment: "Outdoor", kind: "distance_time", custom: false },
  { id: "squat", name: "Back Squat", muscle: "Legs", equipment: "Barbell", kind: "weight_reps", custom: false },
];

export function activeWorkout(): Workout {
  return {
    id: "w-active",
    title: "Push day",
    notes: "",
    started_ms: 1_792_584_000_000,
    finished_ms: null,
    exercises: [
      {
        id: "we-bench",
        exercise_id: "bench-press",
        notes: "",
        sets: [
          { id: "s-bench-1", kind: "normal", weight_grams: 60_000, reps: 8, seconds: 0, distance_meters: 0, completed: false },
        ],
      },
      {
        id: "we-plank",
        exercise_id: "plank",
        notes: "",
        sets: [{ id: "s-plank-1", kind: "normal", weight_grams: 0, reps: 0, seconds: 60, distance_meters: 0, completed: false }],
      },
    ],
  };
}

export function gymView(opts: { revision?: number; active?: Workout | null; restRemaining?: number } = {}): GymView {
  const active = opts.active === undefined ? activeWorkout() : opts.active;
  return {
    state: {
      schema_version: 1,
      revision: opts.revision ?? 5,
      settings: { weight_unit: "kg", rest_seconds: 90, weekly_goal: 3 },
      exercises: CATALOG,
      routines: [],
      active,
      history: [],
      measurements: [],
      rest: null,
    },
    summary: {
      workouts: 0,
      completed_sets: 0,
      weekly_workouts: 0,
      weekly_goal: 3,
      volume: "0 kg",
      active_volume: "0 kg",
      active_duration: "3m 05s",
      active_completed_sets: 0,
    },
    records: [],
    chart: [],
    rest_remaining: opts.restRemaining ?? 0,
    weight_labels: { "s-bench-1": "60", "s-plank-1": "0" },
    previous_labels: { "we-bench": "57.5 kg × 8" },
    measurement_labels: {},
    workout_labels: {},
  };
}
