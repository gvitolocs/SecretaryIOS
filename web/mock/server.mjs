#!/usr/bin/env node
// In-memory mock of the Secretary server API (docs/server-api.md) for local development.
// No dependencies. Sign in with owner@example.test / dev-password-123 (override with
// MOCK_OWNER_EMAIL / MOCK_OWNER_PASSWORD), or the legacy device key {"token":"dev"}.
// Five wrong passwords in a row lock sign-in for MOCK_LOCK_SECONDS (default 60):
// 429 account_locked with Retry-After. POST /v1/session/password {currentPassword,
// newPassword} changes the password (session + CSRF; min 12 characters).
//
//   node mock/server.mjs            # listens on 127.0.0.1:8791
//   MOCK_PORT=9000 node mock/server.mjs
//
// Chat triggers (anywhere in the message, case-insensitive):
//   "offline"  -> 502 lucina_unreachable      "timeout" -> 504 lucina_timeout
//   "fail"     -> 500 internal                "link"    -> reply with URLs
//   "job" / "order" / "book" / "image" -> reply with a job (queued -> running -> done)
//   "remind"   -> schedule_reminder reply with a reminder object (no job)
//   "jobfail"  -> a job that ends in error
// Shopping: items whose name contains "stubborn" refuse PATCH (500), to test rollback.
// Jobs: "image" in the message makes a generate_image job with an imageUrl.
// POST /v1/_mock/lucina {"online": false} toggles Lucina's health (CSRF header needed).
// Gym + body follow docs/server-api.md ("Local modules: gym and body"):
//   GET  /v1/gym/state?timezone=…     -> {ok, replayed, view}
//   GET  /v1/gym/clock?timezone=…     -> {ok, replayed, clock}
//   POST /v1/gym/command              -> engine response; 409 conflict / 422 invalid_command, op_id_reused
//   GET  /v1/body/state?timezone=…    -> {ok, replayed, view}
//   POST /v1/body/command             -> engine response (parse_weight answers {ok, weight_grams})
// Test hooks (CSRF header needed):
//   POST /v1/_mock/gym/bump           -> bumps the gym revision (next command gets 409)
//   POST /v1/_mock/body/bump          -> bumps the body revision
//   POST /v1/_mock/server {"down":true} -> /v1/gym/* and /v1/body/* answer 502 with no body
// MOCK_STRICT=1 emulates the older lucina-gym-core view (no estimated_1rm, exercise_charts,
// active_elapsed_seconds or volume_grams; repeat_workout and parse_weight fail) to check
// that the UI degrades gracefully.

import http from "node:http";
import { randomUUID } from "node:crypto";

const PORT = Number(process.env.MOCK_PORT ?? 8791);
const HOST = process.env.MOCK_HOST ?? "127.0.0.1";
const ACCESS_KEY = process.env.MOCK_ACCESS_KEY ?? "dev";
const COOKIE = "sec_session";
const MAX_BODY = 256 * 1024;
const RATE_WINDOW_MS = 60_000;
const RATE_MAX_FAILURES = 20;
const OWNER_EMAIL = (process.env.MOCK_OWNER_EMAIL ?? "owner@example.test").toLowerCase();
let ownerPassword = process.env.MOCK_OWNER_PASSWORD ?? "dev-password-123";
const LOCK_AFTER = 5;
const LOCK_MS = Number(process.env.MOCK_LOCK_SECONDS ?? 60) * 1000;
const MIN_PASSWORD = 12;
let wrongPasswords = 0;
let lockedUntil = 0;

/** @type {Map<string, {deviceName: string}>} */
const sessions = new Map();
/** @type {Map<string, number[]>} */
const loginFailures = new Map();
/** @type {Map<string, {id: string, action: string, createdAt: number, fail: boolean, summary: string}>} */
const jobs = new Map();
/** @type {Map<string, object>} clientMessageId -> response, for idempotent retries */
const chatReplies = new Map();
let lucinaOnline = process.env.MOCK_LUCINA_OFFLINE !== "1";

const iso = () => new Date().toISOString();
const shopItem = (rawName, product, category, status) => ({
  id: randomUUID(), product, rawName, category, status, createdAt: iso(), updatedAt: iso(),
});
let shopping = [
  shopItem("latte d'avena", "oat milk", "dairy", "needed"),
  shopItem("uova x6", "eggs", "dairy", "needed"),
  shopItem("caffè in grani", "espresso beans", "pantry", "needed"),
  shopItem("basilico", "basil", "produce", "purchased"),
  shopItem("stubborn sponge (refuses to toggle)", "sponge", "household", "needed"),
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function send(res, status, body, headers = {}) {
  const payload = body === undefined ? "" : JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    ...headers,
  });
  res.end(payload);
}

function fail(res, status, code, message, headers = {}) {
  send(res, status, { error: { code, message } }, headers);
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on("data", (c) => {
      size += c.length;
      if (size > MAX_BODY) {
        reject(Object.assign(new Error("body too large"), { status: 413 }));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => {
      const text = Buffer.concat(chunks).toString("utf8");
      if (!text) return resolve({});
      try {
        resolve(JSON.parse(text));
      } catch {
        reject(Object.assign(new Error("invalid JSON"), { status: 400 }));
      }
    });
    req.on("error", reject);
  });
}

function cookies(req) {
  const out = {};
  for (const part of (req.headers.cookie ?? "").split(";")) {
    const i = part.indexOf("=");
    if (i > 0) out[part.slice(0, i).trim()] = decodeURIComponent(part.slice(i + 1).trim());
  }
  return out;
}

function currentSession(req) {
  const sid = cookies(req)[COOKIE];
  return sid ? (sessions.get(sid) ?? null) : null;
}

function jobView(job) {
  const age = Date.now() - job.createdAt;
  const status = age < 2000 ? "queued" : age < 7000 ? "running" : job.fail ? "error" : "done";
  const view = {
    id: job.id,
    status,
    action: job.action,
    createdAt: new Date(job.createdAt).toISOString(),
    updatedAt: new Date(Math.min(Date.now(), job.createdAt + 7000)).toISOString(),
  };
  view.result = status === "done" ? job.summary : null;
  view.error = status === "error" ? "The store's site refused the request (mock failure)." : null;
  view.imageUrl = status === "done" && job.action === "generate_image" ? "https://example.com/mock-image.png" : null;
  view.sessionId = job.sessionId;
  view.clientMessageId = job.clientMessageId;
  return view;
}

function lucinaReply(message) {
  const m = message.toLowerCase();
  if (m.includes("link")) {
    return "Here you are:\nhttps://example.com/recipes/risotto (the one you liked)\nand www.example.org/list.\n\nA javascript:alert(1) is never linked.";
  }
  if (/\b(hi|hello|hey)\b/.test(m)) return "Good day. I'm here, and I remember where we left off.\nWhat do you need?";
  return `Understood: "${message}".\nI'll keep it in mind.`;
}

// ---------- Gym + Body mock engines ----------
// JS ports of crates/secretary-gym and crates/secretary-body semantics as
// documented in docs/server-api.md. MOCK_STRICT=1 emulates the older
// lucina-gym-core view (see the header).

const MOCK_STRICT = process.env.MOCK_STRICT === "1";
const DAY_MS = 86_400_000;
let mockServerDown = false;

const EXERCISE_KINDS = ["weight_reps", "reps_only", "timed", "distance_time"];
const SET_KINDS = ["normal", "warmup", "drop", "failure"];

function utf8Bytes(value) {
  return Buffer.byteLength(String(value), "utf8");
}

function gymName(value) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text || utf8Bytes(text) > 120) throw new Error("Names must contain 1–120 bytes");
  return text;
}

function decimalMilli(input) {
  const normalized = String(input).trim().replace(/,/g, ".");
  const parts = normalized.split(".");
  const wholePart = parts[0] ?? "";
  if (parts.length === 0 || parts.length > 2 || wholePart === "" || !/^\d+$/.test(wholePart)) {
    throw new Error("Enter a positive number with up to three decimal places");
  }
  const fraction = parts[1] ?? "";
  if (fraction.length > 3 || !/^\d*$/.test(fraction)) {
    throw new Error("Use at most three decimal places");
  }
  return BigInt(wholePart) * 1000n + BigInt(`${fraction}000`.slice(0, 3));
}

function parseGymWeight(input, unit) {
  const value = decimalMilli(input);
  let grams;
  if (unit === "kg") grams = value;
  else if (unit === "lb") grams = (value * 45_359_237n + 50_000_000n) / 100_000_000n;
  else throw new Error("Weight unit must be kg or lb");
  if (grams > 2_000_000n) throw new Error("Weight exceeds 2,000 kg");
  return Number(grams);
}

const GYM_CATALOG = [
  ["bench-press", "Bench Press", "Chest", "Barbell", "weight_reps"],
  ["squat", "Back Squat", "Legs", "Barbell", "weight_reps"],
  ["deadlift", "Deadlift", "Back", "Barbell", "weight_reps"],
  ["overhead-press", "Overhead Press", "Shoulders", "Barbell", "weight_reps"],
  ["cable-row", "Seated Cable Row", "Back", "Cable", "weight_reps"],
  ["biceps-curl", "Dumbbell Curl", "Arms", "Dumbbell", "weight_reps"],
  ["lateral-raise", "Lateral Raise", "Shoulders", "Dumbbell", "weight_reps"],
  ["pull-up", "Pull Up", "Back", "Bodyweight", "reps_only"],
  ["push-up", "Push Up", "Chest", "Bodyweight", "reps_only"],
  ["plank", "Plank", "Core", "Bodyweight", "timed"],
  ["run", "Running", "Cardio", "Outdoor", "distance_time"],
  ["cycle", "Cycling", "Cardio", "Bicycle", "distance_time"],
];

function newGymSet(reps, weightGrams) {
  return {
    id: randomUUID(),
    kind: "normal",
    weight_grams: weightGrams,
    reps,
    seconds: 60,
    distance_meters: 1000,
    completed: false,
  };
}

function adaptSet(set, kind) {
  if (kind === "weight_reps") {
    set.seconds = 0;
    set.distance_meters = 0;
  } else if (kind === "reps_only") {
    set.weight_grams = 0;
    set.seconds = 0;
    set.distance_meters = 0;
  } else if (kind === "timed") {
    set.weight_grams = 0;
    set.reps = 0;
    set.distance_meters = 0;
  } else if (kind === "distance_time") {
    set.weight_grams = 0;
    set.reps = 0;
  }
}

function gymPreviousSet(state, exerciseId) {
  let best = null;
  for (const workout of state.history) {
    const exercise = workout.exercises.find((e) => e.exercise_id === exerciseId);
    if (!exercise) continue;
    const finished = workout.finished_ms ?? 0;
    if (!best || finished > (best.workout.finished_ms ?? 0)) best = { workout, exercise };
  }
  if (!best) return null;
  return [...best.exercise.sets].reverse().find((s) => s.completed && s.kind !== "warmup") ?? null;
}

function seedGymWorkout(state, daysAgo, title, rows) {
  const finished = Date.now() - daysAgo * DAY_MS;
  const exercises = rows.map(([exerciseId, sets]) => ({
    id: randomUUID(),
    exercise_id: exerciseId,
    notes: "",
    sets: sets.map(([weight, reps, seconds, distance]) => ({
      id: randomUUID(),
      kind: "normal",
      weight_grams: weight,
      reps,
      seconds,
      distance_meters: distance,
      completed: true,
    })),
  }));
  state.history.push({
    id: randomUUID(),
    title,
    notes: "",
    started_ms: finished - 3_600_000,
    finished_ms: finished,
    exercises,
  });
}

function makeGymState() {
  const exercises = GYM_CATALOG.map(([id, name, muscle, equipment, kind]) => ({
    id,
    name,
    muscle,
    equipment,
    kind,
    custom: false,
  }));
  const routines = [
    {
      id: "full-body",
      name: "Full Body",
      exercises: ["squat", "bench-press", "cable-row", "overhead-press"].map((exercise_id) => ({
        exercise_id,
        sets: 3,
        reps: 8,
        weight_grams: 0,
      })),
    },
    {
      id: "push",
      name: "Push",
      exercises: ["bench-press", "overhead-press", "lateral-raise", "push-up"].map((exercise_id) => ({
        exercise_id,
        sets: 3,
        reps: 8,
        weight_grams: 0,
      })),
    },
  ];
  const state = {
    schema_version: 1,
    revision: 0,
    settings: { weight_unit: "kg", rest_seconds: 90, weekly_goal: 3 },
    exercises,
    routines,
    active: null,
    history: [],
    measurements: [],
    rest: null,
    receipts: new Map(),
  };
  seedGymWorkout(state, 12, "Lower body", [
    ["squat", [[80000, 5, 0, 0], [80000, 5, 0, 0], [80000, 5, 0, 0]]],
    ["cable-row", [[55000, 10, 0, 0], [55000, 10, 0, 0], [55000, 10, 0, 0]]],
  ]);
  seedGymWorkout(state, 7, "Push day", [
    ["bench-press", [[60000, 8, 0, 0], [60000, 8, 0, 0], [60000, 8, 0, 0]]],
    ["overhead-press", [[35000, 8, 0, 0], [35000, 8, 0, 0]]],
    ["plank", [[0, 0, 60, 0], [0, 0, 60, 0]]],
  ]);
  seedGymWorkout(state, 2, "Heavy pulls", [
    ["deadlift", [[100000, 5, 0, 0], [100000, 5, 0, 0], [100000, 5, 0, 0]]],
    ["pull-up", [[0, 8, 0, 0], [0, 8, 0, 0], [0, 8, 0, 0]]],
    ["run", [[0, 0, 900, 3000]]],
  ]);
  return state;
}

let gymState = makeGymState();

function gymActive(state) {
  if (!state.active) throw new Error("Start a workout first");
  return state.active;
}

function findGymSet(state, id) {
  const active = gymActive(state);
  for (const exercise of active.exercises) {
    const set = exercise.sets.find((s) => s.id === id);
    if (set) return set;
  }
  throw new Error("Set not found");
}

function gymUint(value, max) {
  if (!Number.isInteger(value) || value < 0 || value > max) throw new Error("Set values exceed supported limits");
  return value;
}

function gymApply(state, command, now) {
  switch (command.type) {
    case "start_workout": {
      if (state.active) throw new Error("Finish or discard your current workout first");
      const exercises = [];
      if (command.routine_id !== undefined && command.routine_id !== null) {
        const routine = state.routines.find((r) => r.id === command.routine_id);
        if (!routine) throw new Error("Routine not found");
        for (const template of routine.exercises) {
          const previous = gymPreviousSet(state, template.exercise_id);
          const definition = state.exercises.find((e) => e.id === template.exercise_id);
          const sets = [];
          for (let i = 0; i < template.sets; i++) {
            const set = newGymSet(template.reps, template.weight_grams);
            if (previous) {
              set.weight_grams = previous.weight_grams;
              set.reps = previous.reps;
              set.seconds = previous.seconds;
              set.distance_meters = previous.distance_meters;
            }
            if (definition) adaptSet(set, definition.kind);
            sets.push(set);
          }
          exercises.push({ id: randomUUID(), exercise_id: template.exercise_id, notes: "", sets });
        }
      }
      state.active = {
        id: randomUUID(),
        title: gymName(command.title),
        notes: "",
        started_ms: now,
        finished_ms: null,
        exercises,
      };
      state.rest = null;
      return;
    }
    case "repeat_workout": {
      if (MOCK_STRICT) throw new Error("unknown variant `repeat_workout`");
      if (state.active) throw new Error("Finish or discard your current workout first");
      const old = state.history.find((w) => w.id === command.workout_id);
      if (!old) throw new Error("Workout not found");
      state.active = {
        id: randomUUID(),
        title: old.title,
        notes: "",
        started_ms: now,
        finished_ms: null,
        exercises: old.exercises.map((exercise) => ({
          id: randomUUID(),
          exercise_id: exercise.exercise_id,
          notes: exercise.notes,
          sets: exercise.sets.map((set) => ({ ...set, id: randomUUID(), completed: false })),
        })),
      };
      state.rest = null;
      return;
    }
    case "rename_workout": {
      gymActive(state).title = gymName(command.title);
      return;
    }
    case "set_notes": {
      const notes = typeof command.notes === "string" ? command.notes : "";
      if (utf8Bytes(notes) > 8000) throw new Error("Notes exceed 8,000 bytes");
      const active = gymActive(state);
      if (command.exercise_id !== undefined && command.exercise_id !== null) {
        const exercise = active.exercises.find((e) => e.id === command.exercise_id);
        if (!exercise) throw new Error("Exercise not found");
        exercise.notes = notes;
      } else {
        active.notes = notes;
      }
      return;
    }
    case "add_exercise": {
      const definition = state.exercises.find((e) => e.id === command.exercise_id);
      if (!definition) throw new Error("Exercise not found");
      const previous = gymPreviousSet(state, command.exercise_id);
      const active = gymActive(state);
      if (active.exercises.some((e) => e.exercise_id === command.exercise_id)) {
        throw new Error("This exercise is already in your workout");
      }
      const set = previous ? { ...previous, id: randomUUID(), completed: false } : newGymSet(8, 0);
      adaptSet(set, definition.kind);
      active.exercises.push({ id: randomUUID(), exercise_id: command.exercise_id, notes: "", sets: [set] });
      return;
    }
    case "remove_exercise": {
      const active = gymActive(state);
      const index = active.exercises.findIndex((e) => e.id === command.exercise_id);
      if (index < 0) throw new Error("Exercise not found");
      active.exercises.splice(index, 1);
      return;
    }
    case "add_set": {
      const active = gymActive(state);
      const exercise = active.exercises.find((e) => e.id === command.exercise_id);
      if (!exercise) throw new Error("Exercise not found");
      const last = exercise.sets[exercise.sets.length - 1];
      exercise.sets.push(last ? { ...last, id: randomUUID(), completed: false } : newGymSet(8, 0));
      return;
    }
    case "edit_set": {
      const grams = weightInput(command.weight_grams, command.weight, state.settings.weight_unit, true);
      const reps = gymUint(command.reps, 1000);
      const seconds = gymUint(command.seconds, 86400);
      const distance = gymUint(command.distance_meters, 1_000_000);
      const active = gymActive(state);
      const exercise = active.exercises.find((e) => e.sets.some((s) => s.id === command.set_id));
      if (!exercise) throw new Error("Set not found");
      const definition = state.exercises.find((e) => e.id === exercise.exercise_id);
      if (!definition) throw new Error("Exercise not found");
      const set = exercise.sets.find((s) => s.id === command.set_id);
      if (set.completed) throw new Error("Uncheck this set before editing it");
      set.weight_grams = grams;
      set.reps = reps;
      set.seconds = seconds;
      set.distance_meters = distance;
      set.kind = command.kind;
      adaptSet(set, definition.kind);
      return;
    }
    case "complete_set": {
      if (typeof command.completed !== "boolean") throw new Error("invalid type: expected a boolean");
      const set = findGymSet(state, command.set_id);
      const wasCompleted = set.completed;
      set.completed = command.completed;
      if (set.completed && !wasCompleted) {
        const active = gymActive(state);
        state.rest = { workout_id: active.id, deadline_ms: now + state.settings.rest_seconds * 1000 };
      }
      return;
    }
    case "remove_set": {
      const active = gymActive(state);
      const exercise = active.exercises.find((e) => e.sets.some((s) => s.id === command.set_id));
      if (!exercise) throw new Error("Set not found");
      exercise.sets = exercise.sets.filter((s) => s.id !== command.set_id);
      return;
    }
    case "finish_workout": {
      const active = gymActive(state);
      if (!active.exercises.some((e) => e.sets.some((s) => s.completed))) {
        throw new Error("Complete at least one set before finishing");
      }
      if (now < active.started_ms) throw new Error("The clock moved backwards; retry after the workout start time");
      state.active = null;
      active.finished_ms = now;
      state.history.push(active);
      state.rest = null;
      return;
    }
    case "cancel_workout": {
      gymActive(state);
      state.active = null;
      state.rest = null;
      return;
    }
    case "save_routine": {
      const name = gymName(command.name);
      let exercises;
      if (command.from_active === true) {
        const active = gymActive(state);
        exercises = active.exercises.map((exercise) => {
          const set = exercise.sets[0];
          return {
            exercise_id: exercise.exercise_id,
            sets: Math.max(exercise.sets.length, 1),
            reps: set ? set.reps : 8,
            weight_grams: set ? set.weight_grams : 0,
          };
        });
      } else {
        const ids = Array.isArray(command.exercise_ids) ? command.exercise_ids : [];
        exercises = ids.map((exercise_id) => ({ exercise_id, sets: 3, reps: 8, weight_grams: 0 }));
      }
      state.routines.push({ id: randomUUID(), name, exercises });
      return;
    }
    case "delete_routine": {
      const index = state.routines.findIndex((r) => r.id === command.routine_id);
      if (index < 0) throw new Error("Routine not found");
      state.routines.splice(index, 1);
      return;
    }
    case "delete_workout": {
      const index = state.history.findIndex((w) => w.id === command.workout_id);
      if (index < 0) throw new Error("Workout not found");
      state.history.splice(index, 1);
      return;
    }
    case "create_exercise": {
      state.exercises.push({
        id: randomUUID(),
        name: gymName(command.name),
        muscle: gymName(command.muscle),
        equipment: gymName(command.equipment),
        kind: command.kind,
        custom: true,
      });
      return;
    }
    case "start_rest": {
      if (!Number.isInteger(command.seconds) || command.seconds < 1 || command.seconds > 3600) {
        throw new Error("Rest time must be 1–3,600 seconds");
      }
      const active = gymActive(state);
      state.rest = { workout_id: active.id, deadline_ms: now + command.seconds * 1000 };
      return;
    }
    case "stop_rest": {
      state.rest = null;
      return;
    }
    case "update_settings": {
      state.settings = {
        weight_unit: command.weight_unit,
        rest_seconds: command.rest_seconds,
        weekly_goal: command.weekly_goal,
      };
      return;
    }
    default:
      throw new Error(`unknown variant \`${command.type}\``);
  }
}

function gymValidate(state) {
  if (state.schema_version !== 1) throw new Error("Unsupported backup schema version");
  const settings = state.settings;
  if (
    !["kg", "lb"].includes(settings.weight_unit) ||
    !Number.isInteger(settings.rest_seconds) ||
    settings.rest_seconds < 1 ||
    settings.rest_seconds > 3600 ||
    !Number.isInteger(settings.weekly_goal) ||
    settings.weekly_goal < 1 ||
    settings.weekly_goal > 14
  ) {
    throw new Error("Settings are outside their allowed ranges");
  }
  if (state.exercises.length > 2000 || state.routines.length > 200 || state.history.length > 10000 || state.measurements.length > 50000) {
    throw new Error("Data capacity reached; export a backup before archiving data");
  }
  const ids = new Set();
  const unique = (id) => {
    if (typeof id !== "string" || !id || id.length > 128 || ids.has(id)) throw new Error("Empty or duplicate object ID");
    ids.add(id);
  };
  for (const exercise of state.exercises) {
    unique(exercise.id);
    gymName(exercise.name);
    gymName(exercise.muscle);
    gymName(exercise.equipment);
    if (!EXERCISE_KINDS.includes(exercise.kind)) throw new Error(`unknown variant \`${exercise.kind}\``);
  }
  const catalog = new Set(state.exercises.map((e) => e.id));
  for (const routine of state.routines) {
    unique(routine.id);
    gymName(routine.name);
    const distinct = new Set(routine.exercises.map((e) => e.exercise_id));
    if (!routine.exercises.length || routine.exercises.length > 30 || distinct.size !== routine.exercises.length) {
      throw new Error("Routines need 1–30 distinct exercises");
    }
    for (const exercise of routine.exercises) {
      if (
        !catalog.has(exercise.exercise_id) ||
        !Number.isInteger(exercise.sets) ||
        exercise.sets < 1 ||
        exercise.sets > 100 ||
        !Number.isInteger(exercise.reps) ||
        exercise.reps > 1000 ||
        !Number.isInteger(exercise.weight_grams) ||
        exercise.weight_grams > 2_000_000
      ) {
        throw new Error("Invalid routine prescription");
      }
    }
  }
  const workouts = [...state.history, ...(state.active ? [state.active] : [])];
  for (const workout of workouts) {
    unique(workout.id);
    gymName(workout.title);
    if (
      !Number.isInteger(workout.started_ms) ||
      (workout.finished_ms !== null && (!Number.isInteger(workout.finished_ms) || workout.finished_ms < workout.started_ms)) ||
      typeof workout.notes !== "string" ||
      utf8Bytes(workout.notes) > 8000 ||
      workout.exercises.length > 30
    ) {
      throw new Error("Invalid workout lifecycle or size");
    }
    const distinct = new Set(workout.exercises.map((e) => e.exercise_id));
    if (distinct.size !== workout.exercises.length) throw new Error("Duplicate exercise in workout");
    if (workout.finished_ms !== null && !workout.exercises.some((e) => e.sets.some((s) => s.completed))) {
      throw new Error("Finished workouts need a completed set");
    }
    for (const exercise of workout.exercises) {
      unique(exercise.id);
      const definition = state.exercises.find((e) => e.id === exercise.exercise_id);
      if (!definition) throw new Error("Unknown exercise reference");
      if (typeof exercise.notes !== "string" || utf8Bytes(exercise.notes) > 8000 || exercise.sets.length > 100) {
        throw new Error("Exercise exceeds its note or set limit");
      }
      for (const set of exercise.sets) {
        unique(set.id);
        if (
          !Number.isInteger(set.weight_grams) ||
          set.weight_grams < 0 ||
          set.weight_grams > 2_000_000 ||
          !Number.isInteger(set.reps) ||
          set.reps < 0 ||
          set.reps > 1000 ||
          !Number.isInteger(set.seconds) ||
          set.seconds < 0 ||
          set.seconds > 86400 ||
          !Number.isInteger(set.distance_meters) ||
          set.distance_meters < 0 ||
          set.distance_meters > 1_000_000
        ) {
          throw new Error("Set values exceed supported limits");
        }
        if (!SET_KINDS.includes(set.kind)) throw new Error(`unknown variant \`${set.kind}\``);
        if (set.completed) {
          const valid =
            definition.kind === "timed"
              ? set.seconds > 0
              : definition.kind === "distance_time"
                ? set.seconds > 0 && set.distance_meters > 0
                : set.reps > 0;
          if (!valid) throw new Error("Completed sets need positive reps, time, or distance for their exercise type");
        }
      }
    }
  }
  for (const measurement of state.measurements) {
    unique(measurement.id);
    const bound =
      measurement.kind === "body_weight" ? 2_000_000 : measurement.kind === "body_fat" ? 100_000 : measurement.kind === "waist" ? 500_000 : null;
    if (
      bound === null ||
      !Number.isInteger(measurement.value_milli) ||
      measurement.value_milli <= 0 ||
      measurement.value_milli > bound ||
      !Number.isInteger(measurement.timestamp_ms)
    ) {
      throw new Error("Measurement is outside its allowed range");
    }
  }
  if (state.rest && (!state.active || state.active.id !== state.rest.workout_id || !Number.isInteger(state.rest.deadline_ms))) {
    throw new Error("Rest timer must belong to the active workout");
  }
}

/** Engine rule: exactly one of weight_grams (integer) or weight (text in `unit`). */
function weightInput(grams, text, unit, required) {
  const hasGrams = grams !== undefined && grams !== null;
  const hasText = text !== undefined && text !== null;
  if (hasGrams && hasText) throw new Error("Give either weight_grams or weight, not both");
  if (hasGrams) {
    if (!Number.isInteger(grams) || grams < 0) throw new Error("weight_grams must be a non-negative integer");
    return grams;
  }
  if (hasText) return parseGymWeight(String(text), unit);
  if (required) throw new Error("A weight (weight_grams or weight) is required");
  return null;
}

function gymNumber(value) {
  const text = value.toFixed(3).replace(/0+$/, "").replace(/\.$/, "");
  return text || "0";
}

function gymWeightLabel(grams, unit) {
  return gymNumber(unit === "lb" ? grams / 453.59237 : grams / 1000);
}

function gymVolumeLabel(grams, unit) {
  return `${gymWeightLabel(grams, unit)} ${unit}`;
}

function gymDuration(ms) {
  const secs = Math.floor(Math.max(0, ms) / 1000);
  if (secs >= 3600) return `${Math.floor(secs / 3600)}h ${String(Math.floor(secs / 60) % 60).padStart(2, "0")}m`;
  return `${Math.floor(secs / 60)}m ${String(secs % 60).padStart(2, "0")}s`;
}

function gymSetVolume(set) {
  return set.completed && set.kind !== "warmup" ? set.weight_grams * set.reps : 0;
}

function gymWorkoutVolume(workout) {
  return workout.exercises.reduce((sum, exercise) => sum + exercise.sets.reduce((n, set) => n + gymSetVolume(set), 0), 0);
}

function gymCompleted(workout) {
  return workout.exercises.reduce((sum, exercise) => sum + exercise.sets.filter((set) => set.completed).length, 0);
}

function gymRestRemaining(state, now) {
  if (!state.rest) return 0;
  return now >= state.rest.deadline_ms ? 0 : Math.ceil((state.rest.deadline_ms - now) / 1000);
}

function gymDateKey(ms, tz) {
  try {
    return new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date(ms));
  } catch {
    return new Date(ms).toISOString().slice(0, 10);
  }
}

function gymMondayKey(ms, tz) {
  const key = gymDateKey(ms, tz);
  const [year, month, day] = key.split("-").map(Number);
  const date = new Date(Date.UTC(year, month - 1, day));
  date.setUTCDate(date.getUTCDate() - ((date.getUTCDay() + 6) % 7));
  return date.toISOString().slice(0, 10);
}

function gymMonthDay(ms, tz) {
  try {
    return new Intl.DateTimeFormat("en-GB", { timeZone: tz, day: "2-digit", month: "short" }).format(new Date(ms));
  } catch {
    return new Date(ms).toISOString().slice(8, 10);
  }
}

function gymSetLabel(set, kind, unit) {
  if (kind === "weight_reps") return `${gymWeightLabel(set.weight_grams, unit)} ${unit} × ${set.reps}`;
  if (kind === "reps_only") return `${set.reps} reps`;
  if (kind === "timed") return `${set.seconds} s`;
  return `${set.distance_meters} m · ${set.seconds} s`;
}

function gymClock(state, now) {
  const clock = {
    rest_remaining: gymRestRemaining(state, now),
    active_duration: gymDuration(state.active ? Math.max(0, now - state.active.started_ms) : 0),
  };
  // Not in the older lucina-gym-core (MOCK_STRICT).
  if (!MOCK_STRICT) clock.active_elapsed_seconds = state.active ? Math.floor(Math.max(0, now - state.active.started_ms) / 1000) : 0;
  return clock;
}

function gymView(state, now, tz) {
  const unit = state.settings.weight_unit;
  const history = [...state.history].sort((a, b) => (b.finished_ms ?? 0) - (a.finished_ms ?? 0));
  const measurements = [...state.measurements].sort((a, b) => b.timestamp_ms - a.timestamp_ms);
  const exercises = [...state.exercises].sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  const visible = {
    schema_version: state.schema_version,
    revision: state.revision,
    settings: state.settings,
    exercises,
    routines: state.routines,
    active: state.active,
    history,
    measurements,
    rest: state.rest,
  };

  const today = gymDateKey(now, tz);
  const monday = gymMondayKey(now, tz);
  const weeklyWorkouts = history.filter((workout) => {
    const key = gymDateKey(workout.finished_ms, tz);
    return key >= monday && key <= today;
  }).length;

  const summary = {
    workouts: history.length,
    completed_sets: history.reduce((sum, workout) => sum + gymCompleted(workout), 0),
    weekly_workouts: weeklyWorkouts,
    weekly_goal: state.settings.weekly_goal,
    volume: gymVolumeLabel(history.reduce((sum, workout) => sum + gymWorkoutVolume(workout), 0), unit),
    active_volume: gymVolumeLabel(state.active ? gymWorkoutVolume(state.active) : 0, unit),
    active_duration: gymDuration(state.active ? Math.max(0, now - state.active.started_ms) : 0),
    active_completed_sets: state.active ? gymCompleted(state.active) : 0,
  };
  // Not in the older lucina-gym-core (MOCK_STRICT).
  if (!MOCK_STRICT) summary.active_elapsed_seconds = state.active ? Math.floor(Math.max(0, now - state.active.started_ms) / 1000) : 0;

  const weightLabels = {};
  const workoutLabels = {};
  for (const workout of [...state.history, ...(state.active ? [state.active] : [])]) {
    workoutLabels[workout.id] = {
      sets: gymCompleted(workout),
      volume: gymVolumeLabel(gymWorkoutVolume(workout), unit),
      duration: gymDuration((workout.finished_ms ?? now) - workout.started_ms),
    };
    for (const exercise of workout.exercises) {
      for (const set of exercise.sets) weightLabels[set.id] = gymWeightLabel(set.weight_grams, unit);
    }
  }

  const previousLabels = {};
  if (state.active) {
    for (const exercise of state.active.exercises) {
      const previous = gymPreviousSet(state, exercise.exercise_id);
      if (previous) {
        const definition = state.exercises.find((e) => e.id === exercise.exercise_id);
        if (definition) previousLabels[exercise.id] = gymSetLabel(previous, definition.kind, unit);
      }
    }
  }

  const records = [];
  for (const exercise of exercises) {
    const sets = state.history
      .flatMap((workout) => workout.exercises)
      .filter((entry) => entry.exercise_id === exercise.id)
      .flatMap((entry) => entry.sets)
      .filter((set) => set.completed && set.kind !== "warmup");
    if (!sets.length) continue;
    const best = Math.max(...sets.map((set) => set.weight_grams));
    const record = {
      exercise_id: exercise.id,
      name: exercise.name,
      weight: gymVolumeLabel(best, unit),
      reps: Math.max(...sets.map((set) => set.reps)),
      volume: gymVolumeLabel(Math.max(...sets.map((set) => set.weight_grams * set.reps)), unit),
      seconds: Math.max(...sets.map((set) => set.seconds)),
      distance_meters: Math.max(...sets.map((set) => set.distance_meters)),
    };
    // Epley w·(1 + reps/30), w for single reps; null without weighted sets.
    if (!MOCK_STRICT) {
      const estimate = Math.max(
        0,
        ...sets.map((set) => (set.weight_grams > 0 && set.reps > 0 ? (set.reps === 1 ? set.weight_grams : set.weight_grams * (1 + set.reps / 30)) : 0)),
      );
      record.estimated_1rm_grams = estimate > 0 ? Math.round(estimate) : null;
      record.estimated_1rm = estimate > 0 ? gymVolumeLabel(Math.round(estimate), unit) : null;
    }
    records.push(record);
  }

  const chartWorkouts = history.slice(0, 14).reverse();
  const chart = chartWorkouts.map((workout) => ({
    id: workout.id,
    label: gymMonthDay(workout.finished_ms, tz),
    volume: unit === "lb" ? gymWorkoutVolume(workout) / 453.59237 : gymWorkoutVolume(workout) / 1000,
    ...(MOCK_STRICT ? {} : { volume_grams: gymWorkoutVolume(workout) }),
  }));

  const exerciseCharts = {};
  // Not in the older lucina-gym-core (MOCK_STRICT).
  if (!MOCK_STRICT) {
    for (const exercise of exercises) {
      const points = chartWorkouts.map((workout) => {
        const volume = workout.exercises
          .filter((entry) => entry.exercise_id === exercise.id)
          .flatMap((entry) => entry.sets)
          .reduce((sum, set) => sum + gymSetVolume(set), 0);
        return {
          id: workout.id,
          label: gymMonthDay(workout.finished_ms, tz),
          volume: unit === "lb" ? volume / 453.59237 : volume / 1000,
          volume_grams: volume,
        };
      });
      if (points.some((point) => point.volume > 0)) exerciseCharts[exercise.id] = points;
    }
  }

  const measurementLabels = {};
  for (const measurement of state.measurements) {
    if (measurement.kind === "body_weight") measurementLabels[measurement.id] = gymVolumeLabel(measurement.value_milli, unit);
    else if (measurement.kind === "body_fat") measurementLabels[measurement.id] = `${gymNumber(measurement.value_milli / 1000)}%`;
    else measurementLabels[measurement.id] = `${gymNumber(measurement.value_milli / 1000)} cm`;
  }

  return {
    state: visible,
    summary,
    records,
    chart,
    rest_remaining: gymRestRemaining(state, now),
    weight_labels: weightLabels,
    previous_labels: previousLabels,
    measurement_labels: measurementLabels,
    workout_labels: workoutLabels,
    ...(MOCK_STRICT ? {} : { exercise_charts: exerciseCharts }),
  };
}

function checkTimeZone(value) {
  const tz = typeof value === "string" && value.trim() ? value : "UTC";
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: tz }).format(new Date());
    return tz;
  } catch {
    throw new Error("Unknown IANA timezone");
  }
}

function engineError(res, status, code, error) {
  send(res, status, { ok: false, replayed: false, code, error });
}

async function handleGymCommand(req, res) {
  let body;
  try {
    body = await readJson(req);
  } catch (e) {
    return engineError(res, e?.status ?? 400, "invalid_json", String(e?.message ?? e));
  }
  let tz;
  try {
    tz = checkTimeZone(body.timezone);
  } catch {
    return engineError(res, 422, "invalid_command", "Unknown IANA timezone");
  }
  const command = body.command;
  if (!command || typeof command !== "object" || typeof command.type !== "string") {
    return engineError(res, 422, "invalid_command", "missing field `command`");
  }
  const now = Date.now();
  if (command.type === "clock") return send(res, 200, { ok: true, replayed: false, clock: gymClock(gymState, now) });
  if (command.type === "parse_weight" && !MOCK_STRICT) {
    try {
      const grams = parseGymWeight(String(command.value ?? ""), command.unit ?? gymState.settings.weight_unit);
      return send(res, 200, { ok: true, replayed: false, weight_grams: grams });
    } catch (e) {
      return engineError(res, 422, "invalid_command", String(e?.message ?? e));
    }
  }
  if (command.type === "snapshot") return send(res, 200, { ok: true, replayed: false, view: gymView(gymState, now, tz) });

  const opId = typeof body.op_id === "string" ? body.op_id : "";
  if (!opId.trim() || opId.length > 128) {
    return engineError(res, 422, "invalid_command", "A nonempty op_id (at most 128 bytes) is required");
  }
  const fingerprint = JSON.stringify(command);
  const previous = gymState.receipts.get(opId);
  if (previous !== undefined) {
    if (previous !== fingerprint) {
      return engineError(res, 422, "op_id_reused", "This operation ID was already used for a different command");
    }
    return send(res, 200, { ok: true, replayed: true, view: gymView(gymState, now, tz) });
  }
  if (body.expected_revision !== gymState.revision) {
    return engineError(res, 409, "conflict", "State changed. Refresh and retry the edit.");
  }
  try {
    const candidate = structuredClone(gymState);
    gymApply(candidate, command, now);
    candidate.revision = gymState.revision + 1;
    candidate.receipts.set(opId, fingerprint);
    gymValidate(candidate);
    gymState = candidate;
  } catch (e) {
    return engineError(res, 422, "invalid_command", String(e?.message ?? e));
  }
  return send(res, 200, { ok: true, replayed: false, view: gymView(gymState, now, tz) });
}

// ---------- Body weight mock engine (docs/server-api.md, "Body module") ----------

const BODY_MIN = 1_000;
const BODY_MAX = 650_000;
const BODY_MAX_NOTE = 1_000;

function makeBodyState() {
  const now = Date.now();
  const entries = [];
  for (let i = 0; i < 40; i++) {
    const measured = now - i * DAY_MS - (i % 5) * 3_600_000;
    entries.push({
      id: randomUUID(),
      weight_grams: Math.round(80000 + Math.sin(i * 0.55) * 900 + (i % 4) * 120),
      measured_ms: measured,
      note: i % 9 === 0 ? "Morning weigh-in" : "",
      created_ms: measured,
      updated_ms: measured,
    });
  }
  entries.sort((a, b) => b.measured_ms - a.measured_ms);
  return { revision: 0, settings: { unit: "kg", goal_grams: 78000 }, entries, receipts: new Map() };
}

let bodyState = makeBodyState();

function bodyRange(grams, what) {
  if (!Number.isInteger(grams) || grams < BODY_MIN || grams > BODY_MAX) {
    throw new Error(`${what} must be between 1 and 650 kg`);
  }
  return grams;
}

function bodyMeasured(value, now) {
  if (!Number.isInteger(value) || value < 0) throw new Error("Invalid measurement time");
  if (value > now + DAY_MS) throw new Error("The measurement time is too far in the future");
  return value;
}

function bodyNote(value) {
  const note = typeof value === "string" ? value : "";
  if (utf8Bytes(note) > BODY_MAX_NOTE) throw new Error("Notes exceed 1,000 bytes");
  return note;
}

function bodyApply(state, command, now) {
  const unit = state.settings.unit;
  switch (command.type) {
    case "log_entry": {
      const grams = bodyRange(weightInput(command.weight_grams, command.weight, unit, true), "Weight");
      state.entries.push({
        id: randomUUID(),
        weight_grams: grams,
        measured_ms: command.measured_ms === undefined ? now : bodyMeasured(command.measured_ms, now),
        note: bodyNote(command.note),
        created_ms: now,
        updated_ms: now,
      });
      break;
    }
    case "edit_entry": {
      const entry = state.entries.find((e) => e.id === command.entry_id);
      if (!entry) throw new Error("Entry not found");
      const grams = weightInput(command.weight_grams, command.weight, unit, false);
      if (grams === null && command.measured_ms === undefined && command.note === undefined) {
        throw new Error("Nothing to change");
      }
      if (grams !== null) entry.weight_grams = bodyRange(grams, "Weight");
      if (command.measured_ms !== undefined) entry.measured_ms = bodyMeasured(command.measured_ms, now);
      if (command.note !== undefined) entry.note = bodyNote(command.note);
      entry.updated_ms = now;
      break;
    }
    case "delete_entry": {
      const index = state.entries.findIndex((e) => e.id === command.entry_id);
      if (index < 0) throw new Error("Entry not found");
      state.entries.splice(index, 1);
      break;
    }
    case "update_settings": {
      if (command.unit !== "kg" && command.unit !== "lb") throw new Error("Unit must be kg or lb");
      const goal = weightInput(command.goal_grams, command.goal, command.unit, false);
      state.settings = { unit: command.unit, goal_grams: goal === null ? null : bodyRange(goal, "Goal") };
      break;
    }
    default:
      throw new Error(`unknown variant \`${command.type}\``);
  }
  state.entries.sort((a, b) => b.measured_ms - a.measured_ms);
}

/** Mean of the entries in (t − 7 days, t], rounded half up to the gram. */
function bodyAvg7(entries, t) {
  const inRange = entries.filter((e) => e.measured_ms > t - 7 * DAY_MS && e.measured_ms <= t);
  if (!inRange.length) return null;
  return Math.floor(inRange.reduce((sum, e) => sum + e.weight_grams, 0) / inRange.length + 0.5);
}

function bodyTrend(entries, days, now) {
  const window = entries.filter((e) => e.measured_ms > now - days * DAY_MS && e.measured_ms <= now);
  if (window.length < 2) return null;
  const oldest = window[window.length - 1];
  const newest = window[0];
  return bodyAvg7(entries, newest.measured_ms) - bodyAvg7(entries, oldest.measured_ms);
}

function bodyStats(entries, settings, now) {
  const nulls = {
    count: entries.length, current_grams: null, current_measured_ms: null, avg7_grams: null,
    weekly_change_grams: null, min_grams: null, max_grams: null, trend30_grams: null,
    trend90_grams: null, goal_grams: settings.goal_grams, goal_distance_grams: null, goal_reached: null,
  };
  if (!entries.length) return nulls;
  const latest = entries[0];
  const avg7 = bodyAvg7(entries, latest.measured_ms);
  const priorWindow = entries.some(
    (e) => e.measured_ms > latest.measured_ms - 14 * DAY_MS && e.measured_ms <= latest.measured_ms - 7 * DAY_MS,
  );
  const weights = entries.map((e) => e.weight_grams);
  const first = entries[entries.length - 1];
  const goal = settings.goal_grams;
  return {
    ...nulls,
    current_grams: latest.weight_grams,
    current_measured_ms: latest.measured_ms,
    avg7_grams: avg7,
    weekly_change_grams: priorWindow ? avg7 - bodyAvg7(entries, latest.measured_ms - 7 * DAY_MS) : null,
    min_grams: Math.min(...weights),
    max_grams: Math.max(...weights),
    trend30_grams: bodyTrend(entries, 30, now),
    trend90_grams: bodyTrend(entries, 90, now),
    goal_distance_grams: goal === null ? null : avg7 - goal,
    goal_reached: goal === null ? null : goal < first.weight_grams ? avg7 <= goal : avg7 >= goal,
  };
}

/** Same rounding as secretary-body units::tenths: one decimal, half up. */
function bodyTenths(grams, unit) {
  return unit === "lb" ? Math.floor((grams * 1_000_000 + 22_679_618) / 45_359_237) : Math.floor((grams + 50) / 100);
}

function bodyLabel(grams, unit) {
  const t = bodyTenths(grams, unit);
  return `${Math.floor(t / 10)}.${t % 10} ${unit}`;
}

function bodySignedLabel(delta, unit) {
  const t = bodyTenths(Math.abs(delta), unit);
  const sign = t === 0 || delta === 0 ? "" : delta > 0 ? "+" : "-";
  return `${sign}${Math.floor(t / 10)}.${t % 10} ${unit}`;
}

function bodyView(state, now) {
  const unit = state.settings.unit;
  const entries = [...state.entries].sort((a, b) => b.measured_ms - a.measured_ms);
  const stats = bodyStats(entries, state.settings, now);
  const labels = Object.fromEntries(entries.map((e) => [e.id, bodyLabel(e.weight_grams, unit)]));
  const statLabels = {};
  for (const [key, value] of [["current", stats.current_grams], ["avg7", stats.avg7_grams], ["min", stats.min_grams], ["max", stats.max_grams], ["goal", stats.goal_grams]]) {
    if (value !== null) statLabels[key] = bodyLabel(value, unit);
  }
  for (const [key, value] of [["weekly_change", stats.weekly_change_grams], ["trend30", stats.trend30_grams], ["trend90", stats.trend90_grams], ["goal_distance", stats.goal_distance_grams]]) {
    if (value !== null) statLabels[key] = bodySignedLabel(value, unit);
  }
  const series = [...entries]
    .reverse()
    .map((e) => ({ entry_id: e.id, measured_ms: e.measured_ms, weight_grams: e.weight_grams, avg7_grams: bodyAvg7(entries, e.measured_ms) }));
  return { revision: state.revision, settings: { ...state.settings }, entries, stats, series, labels, stat_labels: statLabels };
}

async function handleBodyCommand(req, res) {
  let body;
  try {
    body = await readJson(req);
  } catch (e) {
    return engineError(res, e?.status ?? 400, "invalid_json", String(e?.message ?? e));
  }
  try {
    checkTimeZone(body.timezone);
  } catch {
    return engineError(res, 422, "invalid_command", "Unknown IANA timezone");
  }
  const command = body.command;
  if (!command || typeof command !== "object" || typeof command.type !== "string") {
    return engineError(res, 422, "invalid_command", "missing field `command`");
  }
  const now = Date.now();
  if (command.type === "snapshot") return send(res, 200, { ok: true, replayed: false, view: bodyView(bodyState, now) });
  if (command.type === "parse_weight") {
    try {
      const unit = command.unit ?? bodyState.settings.unit;
      if (unit !== "kg" && unit !== "lb") throw new Error("Weight unit must be kg or lb");
      return send(res, 200, { ok: true, replayed: false, weight_grams: parseGymWeight(String(command.value ?? ""), unit) });
    } catch (e) {
      return engineError(res, 422, "invalid_command", String(e?.message ?? e));
    }
  }
  const opId = typeof body.op_id === "string" ? body.op_id : "";
  if (!opId.trim() || opId.length > 128) {
    return engineError(res, 422, "invalid_command", "A nonempty op_id (at most 128 bytes) is required");
  }
  const fingerprint = JSON.stringify(command);
  const previous = bodyState.receipts.get(opId);
  if (previous !== undefined) {
    if (previous !== fingerprint) {
      return engineError(res, 422, "op_id_reused", "This operation ID was already used for a different command");
    }
    return send(res, 200, { ok: true, replayed: true, view: bodyView(bodyState, now) });
  }
  if (body.expected_revision !== bodyState.revision) {
    return engineError(res, 409, "conflict", "State changed. Refresh and retry the edit.");
  }
  try {
    const candidate = structuredClone(bodyState);
    bodyApply(candidate, command, now);
    candidate.revision = bodyState.revision + 1;
    candidate.receipts.set(opId, fingerprint);
    bodyState = candidate;
  } catch (e) {
    return engineError(res, 422, "invalid_command", String(e?.message ?? e));
  }
  return send(res, 200, { ok: true, replayed: false, view: bodyView(bodyState, now) });
}

async function handleChat(req, res) {
  const body = await readJson(req);
  const message = typeof body.message === "string" ? body.message : "";
  const sessionId = body.sessionId === undefined ? "app" : body.sessionId;
  const clientMessageId = body.clientMessageId ?? null;
  if (!message.trim() || message.length > 4000) return fail(res, 400, "bad_request", "message must be 1-4000 characters");
  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/.test(message)) {
    return fail(res, 400, "bad_request", "message contains control characters");
  }
  if (typeof sessionId !== "string" || !/^[A-Za-z0-9_-]{1,64}$/.test(sessionId)) {
    return fail(res, 400, "bad_request", "sessionId must match [A-Za-z0-9_-]{1,64}");
  }
  if (clientMessageId !== null && (typeof clientMessageId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(clientMessageId))) {
    return fail(res, 400, "bad_request", "clientMessageId must match [A-Za-z0-9_.:-]{1,128}");
  }

  const key = clientMessageId ? `${sessionId}/${clientMessageId}` : null;
  const cached = key ? chatReplies.get(key) : undefined;
  if (cached) return send(res, 200, { ...cached, duplicate: true });

  const m = message.toLowerCase();
  await sleep(600 + Math.random() * 900);
  if (!lucinaOnline || m.includes("offline")) {
    return fail(res, 502, "lucina_unreachable", "Lucina is not reachable right now.");
  }
  if (m.includes("timeout")) {
    await sleep(1000);
    return fail(res, 504, "lucina_timeout", "Lucina did not answer in time.");
  }
  if (/\bfail\b/.test(m)) return fail(res, 500, "internal", "Something broke (mock).");

  let reply;
  const ids = { sessionId: `secretary_${sessionId}`, clientMessageId };
  if (/(job|order|book|image)/.test(m)) {
    const id = randomUUID();
    const action = m.includes("image") ? "generate_image" : m.includes("order") ? "spreadsheet_update_task" : "cursor_task";
    jobs.set(id, {
      id,
      action,
      createdAt: Date.now(),
      fail: m.includes("jobfail"),
      summary: `Finished "${message}".\nDetails: https://example.com/jobs/${id.slice(0, 8)}`,
      ...ids,
    });
    reply = { reply: "On it. I'll report back here when it's done.", action, job: { id, status: "queued" }, ...ids };
  } else if (m.includes("remind")) {
    const dueAt = new Date(Date.now() + 3600_000).toISOString();
    reply = {
      reply: `Reminder set for ${dueAt} (Europe/Rome): ${message}`,
      action: "schedule_reminder",
      job: null,
      ...ids,
      reminder: { id: randomUUID(), text: message, dueAt, timeZone: "Europe/Rome", status: "pending", source: "secretary" },
    };
  } else {
    reply = { reply: lucinaReply(message), action: "reply", job: null, ...ids };
  }
  if (key) chatReplies.set(key, reply);
  send(res, 200, reply);
}

async function handleLogin(req, res) {
  const ip = req.socket.remoteAddress ?? "?";
  const now = Date.now();
  if (lockedUntil > now) {
    return fail(res, 429, "account_locked", "too many failed password attempts; try again later", {
      "Retry-After": String(Math.ceil((lockedUntil - now) / 1000)),
    });
  }
  const recent = (loginFailures.get(ip) ?? []).filter((t) => now - t < RATE_WINDOW_MS);
  loginFailures.set(ip, recent);
  if (recent.length >= RATE_MAX_FAILURES) {
    const retry = Math.ceil((RATE_WINDOW_MS - (now - (recent[0] ?? now))) / 1000);
    return fail(res, 429, "rate_limited", "Too many attempts.", { "Retry-After": String(retry) });
  }
  const body = await readJson(req);
  await sleep(250);
  let ok;
  if (typeof body.email === "string" || typeof body.password === "string") {
    if (typeof body.email !== "string" || typeof body.password !== "string" || !body.email || !body.password) {
      return fail(res, 400, "bad_request", "email and password are required");
    }
    ok = body.email.trim().toLowerCase() === OWNER_EMAIL && body.password === ownerPassword;
  } else if (typeof body.token === "string" && body.token) {
    ok = body.token === ACCESS_KEY; // legacy device-key login, kept for scripts
  } else {
    return fail(res, 400, "bad_request", "email and password are required");
  }
  if (!ok) {
    recent.push(now);
    wrongPasswords += 1;
    if (wrongPasswords >= LOCK_AFTER) {
      wrongPasswords = 0;
      lockedUntil = now + LOCK_MS;
    }
    return fail(res, 401, "invalid_credentials", "wrong email or password");
  }
  wrongPasswords = 0;
  loginFailures.delete(ip);
  const sid = randomUUID();
  const ua = String(req.headers["user-agent"] ?? "");
  sessions.set(sid, { deviceName: /iPhone/.test(ua) ? "iPhone" : "Browser" });
  send(res, 200, { authenticated: true, deviceName: sessions.get(sid).deviceName }, {
    "Set-Cookie": `${COOKIE}=${sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`,
  });
}

async function handlePasswordChange(req, res) {
  if (!currentSession(req)) return fail(res, 401, "unauthorized", "session expired or invalid");
  const body = await readJson(req);
  await sleep(200);
  if (typeof body.currentPassword !== "string" || typeof body.newPassword !== "string") {
    return fail(res, 400, "bad_request", "currentPassword and newPassword are required");
  }
  if ([...body.newPassword].length < MIN_PASSWORD) {
    return fail(res, 400, "bad_request", `newPassword: password must be at least ${MIN_PASSWORD} characters`);
  }
  // Like secretary-server: a wrong current password is 403 (the caller stays signed in).
  if (body.currentPassword !== ownerPassword) {
    return fail(res, 403, "invalid_credentials", "the current password is wrong");
  }
  ownerPassword = body.newPassword;
  const keep = cookies(req)[COOKIE];
  let revoked = 0;
  for (const sid of [...sessions.keys()]) {
    if (sid !== keep) {
      sessions.delete(sid);
      revoked += 1;
    }
  }
  send(res, 200, { changed: true, sessionsRevoked: revoked });
}

async function route(req, res) {
  const url = new URL(req.url ?? "/", "http://mock.local");
  const path = url.pathname.replace(/\/+$/, "") || "/";
  const method = req.method ?? "GET";

  if (!path.startsWith("/v1/")) return fail(res, 404, "not_found", "Not found.");
  const writes = ["POST", "PUT", "PATCH", "DELETE"].includes(method);
  // POST /v1/session is exempt (its JSON content type already forces a preflight).
  if (writes && !(method === "POST" && path === "/v1/session") && req.headers["x-secretary-csrf"] !== "1") {
    return fail(res, 403, "csrf", "missing X-Secretary-CSRF header");
  }
  if (["POST", "PUT", "PATCH"].includes(method) && !String(req.headers["content-type"] ?? "").startsWith("application/json")) {
    return fail(res, 415, "unsupported_media_type", "Content-Type must be application/json");
  }

  if (path === "/v1/health" && method === "GET") {
    return send(res, 200, {
      ok: true,
      version: "mock-0.1.0",
      lucina: lucinaOnline
        ? { reachable: true, latencyMs: Math.round(40 + Math.random() * 160), apiVersion: 1 }
        : { reachable: false, latencyMs: null, error: "lucina_unreachable" },
    });
  }

  if (path === "/v1/session/password") {
    if (method === "POST") return handlePasswordChange(req, res);
    return fail(res, 405, "method_not_allowed", "Method not allowed.");
  }

  if (path === "/v1/session") {
    if (method === "GET") {
      const s = currentSession(req);
      return send(res, 200, { authenticated: !!s, deviceName: s?.deviceName ?? null });
    }
    if (method === "POST") return handleLogin(req, res);
    if (method === "DELETE") {
      const sid = cookies(req)[COOKIE];
      if (sid) sessions.delete(sid);
      return send(res, 200, { authenticated: false, deviceName: null }, {
        "Set-Cookie": `${COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0`,
      });
    }
    return fail(res, 405, "method_not_allowed", "Method not allowed.");
  }

  if (path === "/v1/_mock/lucina" && method === "POST") {
    const body = await readJson(req);
    lucinaOnline = body.online !== false;
    return send(res, 200, { online: lucinaOnline });
  }

  if (path === "/v1/_mock/gym/bump" && method === "POST") {
    gymState.revision += 1;
    return send(res, 200, { revision: gymState.revision });
  }

  if (path === "/v1/_mock/body/bump" && method === "POST") {
    bodyState.revision += 1;
    return send(res, 200, { revision: bodyState.revision });
  }

  if (path === "/v1/_mock/server" && method === "POST") {
    const body = await readJson(req);
    mockServerDown = body.down !== false;
    return send(res, 200, { down: mockServerDown });
  }

  if (!currentSession(req)) {
    return fail(res, 401, "unauthorized", "session expired or invalid", { "WWW-Authenticate": "Bearer" });
  }

  if (mockServerDown && (path.startsWith("/v1/gym/") || path.startsWith("/v1/body/"))) {
    res.writeHead(502, { "Cache-Control": "no-store" });
    res.end();
    return;
  }

  if ((path === "/v1/gym/state" || path === "/v1/gym/clock" || path === "/v1/body/state") && method === "GET") {
    let tz;
    try {
      tz = checkTimeZone(url.searchParams.get("timezone") ?? "UTC");
    } catch {
      return engineError(res, 422, "invalid_command", "Unknown IANA timezone");
    }
    const now = Date.now();
    if (path === "/v1/gym/clock") return send(res, 200, { ok: true, replayed: false, clock: gymClock(gymState, now) });
    if (path === "/v1/body/state") return send(res, 200, { ok: true, replayed: false, view: bodyView(bodyState, now) });
    return send(res, 200, { ok: true, replayed: false, view: gymView(gymState, now, tz) });
  }
  if (path === "/v1/gym/command" && method === "POST") return handleGymCommand(req, res);

  if (path === "/v1/body/command" && method === "POST") return handleBodyCommand(req, res);

  if (path === "/v1/chat" && method === "POST") return handleChat(req, res);

  const jobMatch = path.match(/^\/v1\/jobs\/([^/]+)$/);
  if (jobMatch && method === "GET") {
    const job = jobs.get(decodeURIComponent(jobMatch[1]));
    if (!job) return fail(res, 404, "job_not_found", "No such job.");
    return send(res, 200, jobView(job));
  }

  if (path === "/v1/shopping") {
    if (method === "GET") {
      await sleep(150);
      const rank = (i) => (i.status === "needed" ? 0 : 1);
      return send(res, 200, [...shopping].sort((a, b) => rank(a) - rank(b)));
    }
    if (method === "POST") {
      const body = await readJson(req);
      const rawName = typeof body.rawName === "string" ? body.rawName.trim() : "";
      if (!rawName || rawName.length > 200) return fail(res, 400, "bad_request", "rawName must be 1-200 characters");
      const product = typeof body.product === "string" && body.product.trim() ? body.product.trim() : rawName.toLowerCase();
      await sleep(300);
      const item = shopItem(rawName, product, null, "needed");
      shopping = [...shopping, item];
      return send(res, 201, item);
    }
    return fail(res, 405, "method_not_allowed", "Method not allowed.");
  }

  const itemMatch = path.match(/^\/v1\/shopping\/([^/]+)$/);
  if (itemMatch && method === "PATCH") {
    const id = decodeURIComponent(itemMatch[1]);
    const item = shopping.find((i) => i.id === id);
    if (!item) return fail(res, 404, "item_not_found", "No such item.");
    const body = await readJson(req);
    await sleep(250);
    if (body.status !== "needed" && body.status !== "purchased") {
      return fail(res, 400, "invalid_status", 'status must be "needed" or "purchased"');
    }
    if (/stubborn/i.test(item.rawName)) return fail(res, 500, "internal", "This item refuses to change (mock).");
    item.status = body.status;
    item.updatedAt = iso();
    // Like Lucina: PATCH answers only {id, product, status}, product = raw name.
    return send(res, 200, { id: item.id, product: item.rawName, status: item.status });
  }

  return fail(res, 404, "not_found", "Not found.");
}

const server = http.createServer((req, res) => {
  route(req, res).catch((err) => {
    const status = typeof err?.status === "number" ? err.status : 500;
    const code = status === 500 ? "internal" : status === 413 ? "payload_too_large" : "invalid_json";
    if (!res.headersSent) fail(res, status, code, String(err?.message ?? err));
    else res.end();
  });
});

server.listen(PORT, HOST, () => {
  console.log(`[mock] Secretary API on http://${HOST}:${PORT} (sign in: ${OWNER_EMAIL} / ${ownerPassword}, or token "${ACCESS_KEY}")`);
});

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => server.close(() => process.exit(0)));
}
