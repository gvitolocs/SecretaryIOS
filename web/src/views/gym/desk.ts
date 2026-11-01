import type { GymView, Routine, Workout, WorkoutSet } from "../../api/gym";

// Pure helpers for the desktop Training page (gym/TrainingDesk.tsx). They only
// read the server view and the clock; every label still comes from the server.

const DAY_MS = 86_400_000;

/** Local midnight of the day `ms` falls on. */
export function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Whole calendar days from `from` to `to` (local time, DST-safe). */
export function daysBetween(from: number, to: number): number {
  return Math.round((startOfDay(to) - startOfDay(from)) / DAY_MS);
}

export interface WeekDay {
  /** Local midnight. */
  ms: number;
  done: boolean;
  today: boolean;
  future: boolean;
}

function finishedAt(w: Workout): number {
  return w.finished_ms ?? w.started_ms;
}

/** Monday to Sunday of the week containing `now`, with the days a workout was finished. */
export function weekDays(history: readonly Workout[], now: number): WeekDay[] {
  const today = startOfDay(now);
  const d = new Date(today);
  const sinceMonday = (d.getDay() + 6) % 7;
  d.setDate(d.getDate() - sinceMonday);
  const trained = new Set(history.map((w) => startOfDay(finishedAt(w))));
  const days: WeekDay[] = [];
  for (let i = 0; i < 7; i++) {
    const ms = d.getTime();
    days.push({ ms, done: trained.has(ms), today: ms === today, future: ms > today });
    d.setDate(d.getDate() + 1);
  }
  return days;
}

function sameName(a: string, b: string): boolean {
  return a.trim().toLocaleLowerCase() === b.trim().toLocaleLowerCase();
}

/**
 * When each routine was last finished. A workout started from a routine is
 * titled with the routine's name (GymHome `start_workout`), so match by title.
 */
export function routineLastDone(view: GymView): Map<string, number> {
  const last = new Map<string, number>();
  for (const routine of view.state.routines) {
    const done = view.state.history.find((w) => sameName(w.title, routine.name));
    if (done) last.set(routine.id, finishedAt(done));
  }
  return last;
}

/**
 * The routine to suggest next, rotating through the list: one never done
 * first (in list order), else the one done longest ago.
 */
export function nextRoutine(view: GymView): Routine | null {
  const routines = view.state.routines;
  if (routines.length === 0) return null;
  const last = routineLastDone(view);
  const never = routines.find((r) => !last.has(r.id));
  if (never) return never;
  return [...routines].sort((a, b) => (last.get(a.id) ?? 0) - (last.get(b.id) ?? 0))[0] ?? null;
}

/** Exercise names of a routine: the first `max`, and how many more there are. */
export function exerciseSummary(view: GymView, routine: Routine, max = 3): { names: string[]; more: number } {
  const names = routine.exercises
    .map((re) => view.state.exercises.find((e) => e.id === re.exercise_id)?.name)
    .filter((n): n is string => Boolean(n));
  return { names: names.slice(0, max), more: Math.max(0, names.length - max) };
}

export interface BestLift {
  set: WorkoutSet;
  exercise: string;
}

export interface RecentStats {
  sessions: number;
  /** Volume in the display unit (from the server's chart points), or null with no chart data. */
  volume: number | null;
  best: BestLift | null;
}

/** Sessions, volume and the heaviest completed working set finished in the last `days` days. */
export function recentStats(view: GymView, now: number, days = 30): RecentStats {
  const since = startOfDay(now) - (days - 1) * DAY_MS;
  const workouts = view.state.history.filter((w) => finishedAt(w) >= since);
  const ids = new Set(workouts.map((w) => w.id));
  const points = view.chart.filter((p) => ids.has(p.id));
  const volume = points.length > 0 ? points.reduce((sum, p) => sum + p.volume, 0) : null;
  let best: BestLift | null = null;
  for (const w of workouts) {
    for (const ex of w.exercises) {
      for (const set of ex.sets) {
        if (!set.completed || set.kind === "warmup" || set.weight_grams <= 0 || set.reps <= 0) continue;
        if (!best || set.weight_grams > best.set.weight_grams) {
          const exercise = view.state.exercises.find((e) => e.id === ex.exercise_id)?.name ?? "";
          best = { set, exercise };
        }
      }
    }
  }
  return { sessions: workouts.length, volume, best };
}
