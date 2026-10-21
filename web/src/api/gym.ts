import { request } from "./client";

// Gym module wire types, per docs/server-api.md ("Gym module") and
// crates/secretary-gym/src/models.rs. Everything is snake_case and units are
// integers (grams, seconds, meters, milliseconds); labels come preformatted.
// Fields the older lucina-gym-core lacked stay optional so the UI degrades
// gracefully (mock: MOCK_STRICT=1).

export type ExerciseKind = "weight_reps" | "reps_only" | "timed" | "distance_time";
export type SetKind = "normal" | "warmup" | "drop" | "failure";

export interface Exercise {
  id: string;
  name: string;
  muscle: string;
  equipment: string;
  kind: ExerciseKind;
  custom: boolean;
}

export interface WorkoutSet {
  id: string;
  kind: SetKind;
  weight_grams: number;
  reps: number;
  seconds: number;
  distance_meters: number;
  completed: boolean;
}

export interface WorkoutExercise {
  id: string;
  exercise_id: string;
  notes: string;
  sets: WorkoutSet[];
}

export interface Workout {
  id: string;
  title: string;
  notes: string;
  started_ms: number;
  finished_ms: number | null;
  exercises: WorkoutExercise[];
}

export interface RoutineExercise {
  exercise_id: string;
  sets: number;
  reps: number;
  weight_grams: number;
}

export interface Routine {
  id: string;
  name: string;
  exercises: RoutineExercise[];
}

export interface GymSettings {
  weight_unit: string;
  rest_seconds: number;
  weekly_goal: number;
}

export interface Measurement {
  id: string;
  timestamp_ms: number;
  kind: string;
  value_milli: number;
}

export interface RestTimer {
  workout_id: string;
  deadline_ms: number;
}

export interface GymState {
  schema_version: number;
  revision: number;
  settings: GymSettings;
  exercises: Exercise[];
  routines: Routine[];
  active: Workout | null;
  history: Workout[];
  measurements: Measurement[];
  rest: RestTimer | null;
}

export interface GymSummary {
  workouts: number;
  completed_sets: number;
  weekly_workouts: number;
  weekly_goal: number;
  volume: string;
  active_volume: string;
  active_duration: string;
  active_completed_sets: number;
  active_elapsed_seconds?: number;
}

export interface GymRecord {
  exercise_id: string;
  name: string;
  weight: string;
  reps: number;
  volume: string;
  seconds: number;
  distance_meters: number;
  /** Epley estimate label, null for exercises without weighted sets. */
  estimated_1rm?: string | null;
  estimated_1rm_grams?: number | null;
}

export interface ChartPoint {
  id: string;
  label: string;
  /** In the display unit, for plotting. */
  volume: number;
  volume_grams?: number;
}

export interface WorkoutSummary {
  sets: number;
  volume: string;
  duration: string;
}

export interface GymView {
  state: GymState;
  summary: GymSummary;
  records: GymRecord[];
  chart: ChartPoint[];
  rest_remaining: number;
  weight_labels: Record<string, string>;
  previous_labels: Record<string, string>;
  measurement_labels: Record<string, string>;
  workout_labels: Record<string, WorkoutSummary>;
  /** Per catalog exercise: its volume in each of its last 14 workouts. */
  exercise_charts?: Record<string, ChartPoint[]>;
}

export interface GymClock {
  rest_remaining: number;
  active_duration: string;
  active_elapsed_seconds?: number;
}

export interface GymResponse {
  ok: boolean;
  replayed?: boolean;
  code?: string;
  error?: string;
  view?: GymView;
  text?: string;
  exercises?: Exercise[];
  clock?: GymClock;
}

export interface GymEnvelope {
  op_id: string;
  expected_revision: number;
  timezone: string;
  command: GymCommand;
}

export type GymCommand =
  | { type: "clock" }
  | { type: "start_workout"; title: string; routine_id?: string }
  | { type: "repeat_workout"; workout_id: string }
  | { type: "rename_workout"; title: string }
  | { type: "set_notes"; notes: string; exercise_id?: string }
  | { type: "add_exercise"; exercise_id: string }
  | { type: "remove_exercise"; exercise_id: string }
  | { type: "add_set"; exercise_id: string }
  | {
      type: "edit_set";
      set_id: string;
      /** Text in the display unit, parsed by the engine (or send weight_grams instead). */
      weight: string;
      reps: number;
      seconds: number;
      distance_meters: number;
      kind: SetKind;
    }
  | { type: "complete_set"; set_id: string; completed: boolean }
  | { type: "remove_set"; set_id: string }
  | { type: "finish_workout" }
  | { type: "cancel_workout" }
  | { type: "save_routine"; name: string; exercise_ids?: string[]; from_active?: boolean }
  | { type: "delete_routine"; routine_id: string }
  | { type: "delete_workout"; workout_id: string }
  | { type: "create_exercise"; name: string; muscle: string; equipment: string; kind: ExerciseKind }
  | { type: "start_rest"; seconds: number }
  | { type: "stop_rest" }
  | { type: "update_settings"; weight_unit: string; rest_seconds: number; weekly_goal: number };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Accept either the engine Response `{ok, view}` or a bare View. */
export function normalizeGymView(raw: unknown): GymView {
  if (isRecord(raw) && isRecord(raw.view)) return raw.view as unknown as GymView;
  if (isRecord(raw) && isRecord(raw.state)) return raw as unknown as GymView;
  throw new Error("The gym server returned an unexpected response.");
}

function tzQuery(timezone: string): string {
  return `?timezone=${encodeURIComponent(timezone)}`;
}

export async function getGymState(timezone: string): Promise<GymView> {
  return normalizeGymView(await request<unknown>("GET", `/v1/gym/state${tzQuery(timezone)}`));
}

/** GET /v1/gym/clock: the cheap rest-timer poll. Accepts {ok, clock} or a bare clock. */
export async function getGymClock(timezone: string): Promise<GymClock> {
  const raw = await request<unknown>("GET", `/v1/gym/clock${tzQuery(timezone)}`);
  const clock = isRecord(raw) && isRecord(raw.clock) ? raw.clock : raw;
  if (!isRecord(clock) || typeof clock.rest_remaining !== "number") {
    throw new Error("The gym server returned an unexpected clock.");
  }
  return clock as unknown as GymClock;
}

export function sendGymCommand(envelope: GymEnvelope): Promise<GymResponse> {
  return request<GymResponse>("POST", "/v1/gym/command", { body: envelope });
}
