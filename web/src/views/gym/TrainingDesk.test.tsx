import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BodySeriesPoint, BodyView } from "../../api/body";
import type { GymCommand, GymView, Routine, Workout } from "../../api/gym";
import { setLocale } from "../../i18n";
import { resetBody } from "../../state/body";
import { resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { exerciseSummary, nextRoutine, recentStats, weekDays } from "./desk";
import { gymView } from "./fixture";
import { GymPage } from "./shared";
import TrainingDesk from "./TrainingDesk";
import { changeTone, sparkline } from "./WeightCard";

const DAY = 86_400_000;

type Envelope = { op_id: string; expected_revision: number; command: GymCommand };
const posts = (calls: Call[]) =>
  calls.filter((c) => c.method === "POST" && c.url.includes("/v1/gym/")).map((c) => c.body as Envelope);

function routine(id: string, name: string, exercises: string[]): Routine {
  return { id, name, exercises: exercises.map((exercise_id) => ({ exercise_id, sets: 3, reps: 8, weight_grams: 0 })) };
}

function workout(id: string, title: string, finished: number, weightGrams = 60_000): Workout {
  return {
    id,
    title,
    notes: "",
    started_ms: finished - 3_600_000,
    finished_ms: finished,
    exercises: [
      {
        id: `${id}-ex`,
        exercise_id: "bench-press",
        notes: "",
        sets: [
          { id: `${id}-s1`, kind: "warmup", weight_grams: weightGrams + 40_000, reps: 3, seconds: 0, distance_meters: 0, completed: true },
          { id: `${id}-s2`, kind: "normal", weight_grams: weightGrams, reps: 5, seconds: 0, distance_meters: 0, completed: true },
        ],
      },
    ],
  };
}

/** Push done 2 days ago, Pull 9 days ago, Legs never: Legs is next. */
function trainedView(now = Date.now()): GymView {
  const base = gymView({ active: null });
  const history = [workout("w-push", "Push", now - 2 * DAY, 80_000), workout("w-pull", "pull", now - 9 * DAY), workout("w-old", "Push", now - 40 * DAY, 120_000)];
  return {
    ...base,
    state: {
      ...base.state,
      routines: [
        routine("r-push", "Push", ["bench-press", "incline-press", "overhead-press", "push-up"]),
        routine("r-pull", "Pull", ["squat"]),
        routine("r-legs", "Legs", ["squat", "plank"]),
      ],
      history,
    },
    summary: { ...base.summary, weekly_workouts: 1, weekly_goal: 3 },
    chart: [
      { id: "w-push", label: "", volume: 400 },
      { id: "w-pull", label: "", volume: 300 },
      { id: "w-old", label: "", volume: 900 },
    ],
    weight_labels: { "w-push-s2": "80", "w-pull-s2": "60", "w-old-s2": "120" },
    workout_labels: {
      "w-push": { sets: 1, volume: "400 kg", duration: "1h 00m" },
      "w-pull": { sets: 1, volume: "300 kg", duration: "1h 00m" },
      "w-old": { sets: 1, volume: "600 kg", duration: "1h 00m" },
    },
  };
}

function bodyView(now = Date.now()): BodyView {
  const series: BodySeriesPoint[] = [0, 1, 2, 3].map((i) => ({
    entry_id: `e${i}`,
    measured_ms: now - (3 - i) * DAY,
    weight_grams: 81_200 - i * 400,
    avg7_grams: 81_000 - i * 300,
  }));
  return {
    revision: 1,
    settings: { unit: "kg", goal_grams: 75_000 },
    entries: series.map((p) => ({ id: p.entry_id, weight_grams: p.weight_grams, measured_ms: p.measured_ms, note: "" })).reverse(),
    stats: {
      count: 4,
      current_grams: 80_000,
      current_measured_ms: now,
      avg7_grams: 80_100,
      weekly_change_grams: -1_200,
      min_grams: 80_000,
      max_grams: 81_200,
      trend30_grams: null,
      trend90_grams: null,
      goal_grams: 75_000,
      goal_distance_grams: 5_000,
      goal_reached: false,
    },
    series,
    labels: {},
    stat_labels: { current: "80.0 kg", avg7: "80.1 kg", weekly_change: "-1.2 kg", goal: "75.0 kg" },
  };
}

function engine(view: () => GymView, body: () => BodyView = bodyView) {
  return stubFetch((call) => {
    if (call.url.includes("/v1/body/")) return { body: { ok: true, view: body() } };
    if (call.method === "GET") return { body: { ok: true, view: view() } };
    return { body: { ok: true, view: { ...view(), state: { ...view().state, revision: view().state.revision + 1 } } } };
  });
}

function renderDesk() {
  return renderAt("/gym", () => (
    <>
      <Route path="/gym" component={() => <GymPage wide subnav={false}>{(view) => <TrainingDesk view={view()} />}</GymPage>} />
      <Route path="*" component={() => <p>Elsewhere</p>} />
    </>
  ));
}

describe("desktop Training helpers", () => {
  it("lays the week out Monday to Sunday with the trained days", () => {
    const wed = new Date(2026, 9, 28, 15, 0).getTime();
    const days = weekDays([workout("a", "Push", new Date(2026, 9, 26, 19, 0).getTime())], wed);
    expect(days.map((d) => new Date(d.ms).getDate())).toEqual([26, 27, 28, 29, 30, 31, 1]);
    expect(days.map((d) => d.done)).toEqual([true, false, false, false, false, false, false]);
    expect(days.map((d) => d.today)).toEqual([false, false, true, false, false, false, false]);
    expect(days.filter((d) => d.future)).toHaveLength(4);
    // A Sunday belongs to the week that started the Monday before.
    const sunday = new Date(2026, 10, 1, 10, 0).getTime();
    expect(new Date(weekDays([], sunday)[0]!.ms).getDate()).toBe(26);
  });

  it("suggests a routine never done first, else the one done longest ago", () => {
    expect(nextRoutine(trainedView())?.name).toBe("Legs");
    const all = trainedView();
    all.state.routines = all.state.routines.slice(0, 2);
    expect(nextRoutine(all)?.name).toBe("Pull");
    expect(nextRoutine(gymView({ active: null }))).toBeNull();
  });

  it("summarises a routine's exercises as three names and +N", () => {
    const view = trainedView();
    expect(exerciseSummary(view, view.state.routines[0]!)).toEqual({
      names: ["Bench Press", "Incline Dumbbell Press", "Overhead Press"],
      more: 1,
    });
  });

  it("counts the last 30 days: sessions, chart volume and the heaviest working set", () => {
    const stats = recentStats(trainedView(), Date.now());
    expect(stats.sessions).toBe(2);
    expect(stats.volume).toBe(700);
    // The 120 kg set is older than 30 days and the 120 kg warmup does not count.
    expect(stats.best?.set.id).toBe("w-push-s2");
    expect(stats.best?.exercise).toBe("Bench Press");
  });

  it("keeps the sparkline inside its box and drops a far-away goal", () => {
    const view = bodyView();
    const near = sparkline(view.series, "kg", 79_000)!;
    const far = sparkline(view.series, "kg", 50_000)!;
    const ys = [...near.dots.matchAll(/M[\d.]+ ([\d.]+)h0/g)].map((m) => Number(m[1]));
    expect(ys.every((y) => y > 0 && y < 100)).toBe(true);
    expect(near.goalY).not.toBeNull();
    expect(near.goalY!).toBeGreaterThan(0);
    expect(near.goalY!).toBeLessThan(100);
    expect(far.goalY).toBeNull();
    expect(sparkline([], "kg", null)).toBeNull();
  });

  it("colours the weekly change by the goal's direction", () => {
    const view = bodyView();
    expect(changeTone(view)).toBe("good");
    expect(changeTone({ ...view, stats: { ...view.stats, weekly_change_grams: 900 } })).toBe("bad");
    expect(changeTone({ ...view, stats: { ...view.stats, weekly_change_grams: 20 } })).toBe("steady");
    expect(changeTone({ ...view, stats: { ...view.stats, goal_grams: null } })).toBe("steady");
  });
});

describe("desktop Training page", () => {
  beforeEach(() => {
    resetGym();
    resetBody();
    markLucinaOnline();
    setLocale("en");
  });
  afterEach(() => vi.useRealTimers());

  it("has one header row: the title, the sections and one start button", async () => {
    engine(() => trainedView());
    renderDesk();
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Training");
    const nav = screen.getByRole("navigation", { name: "Gym sections" });
    expect(within(nav).getAllByRole("link").map((a) => a.getAttribute("aria-label") ?? a.textContent)).toEqual([
      "Overview",
      "Journal",
      "Progress",
      "Settings",
    ]);
    expect(within(nav).getByRole("link", { name: "Overview" })).toHaveAttribute("aria-current", "page");
    expect(screen.getAllByRole("button", { name: /^Start workout$/ })).toHaveLength(1);
    // No eyebrow, no all-zero totals strip, no filler line.
    expect(screen.queryByText("Lucina / Gym")).toBeNull();
    expect(screen.queryByText("Sets logged")).toBeNull();
    expect(screen.queryByText("A little stronger, one session at a time.")).toBeNull();
  });

  it("lists routines as rows with the next one marked and when each was last done", async () => {
    engine(() => trainedView());
    renderDesk();
    expect(await screen.findByText("1 of 3")).toBeInTheDocument();
    expect(screen.getByText("Next up:").parentElement).toHaveTextContent("Next up: Legs");
    const routines = screen.getByRole("region", { name: "Routines" });
    const rows = within(routines).getAllByRole("listitem");
    expect(rows.map((r) => r.querySelector(".trd-row-name")?.textContent)).toEqual(["Push", "Pull", "Legs"]);
    expect(within(rows[0]!).getByText("+1")).toBeInTheDocument();
    expect(rows[0]).toHaveTextContent("Last done 2 days ago");
    expect(rows[1]).toHaveTextContent("Last done 9 days ago");
    expect(rows[2]).toHaveTextContent("Not done yet");
    expect(within(rows[2]!).getByText("Next")).toBeInTheDocument();
    expect(within(rows[0]!).queryByText("Next")).toBeNull();
    // Delete is not a button on the row any more.
    expect(within(routines).queryByRole("button", { name: /^Delete/ })).toBeNull();
  });

  it("deletes a routine from its ⋯ menu after a confirm", async () => {
    const calls = engine(() => trainedView());
    renderDesk();
    fireEvent.click(await screen.findByRole("button", { name: "More for Pull" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete routine" }));
    expect(screen.getByText("Delete Pull?")).toBeInTheDocument();
    expect(posts(calls)).toHaveLength(0);
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "delete_routine", routine_id: "r-pull" });
  });

  it("starts a routine from its play button", async () => {
    const calls = engine(() => trainedView());
    renderDesk();
    fireEvent.click(await screen.findByRole("button", { name: "Start Legs" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "start_workout", title: "Legs", routine_id: "r-legs" });
  });

  it("shows recent workouts with a quiet repeat, and the last 30 days", async () => {
    engine(() => trainedView());
    renderDesk();
    const recent = await screen.findByRole("region", { name: "Recent workouts" });
    expect(within(recent).getAllByRole("listitem")).toHaveLength(3);
    expect(within(recent).getAllByRole("link", { name: "Push" })[0]).toHaveAttribute("href", "/gym/history/w-push");
    expect(within(recent).getAllByRole("button", { name: "Repeat Push" })).toHaveLength(2);
    const last = screen.getByRole("region", { name: "Last 30 days" });
    expect(last).toHaveTextContent("Sessions2");
    expect(last).toHaveTextContent("Volume700 kg");
    expect(last).toHaveTextContent("Best liftBench Press 80 kg × 5");
  });

  it("shows the weight, its change toward the goal and a one-line log", async () => {
    const calls = engine(() => trainedView());
    renderDesk();
    const card = await screen.findByRole("region", { name: "Weight" });
    await waitFor(() => expect(card).toHaveTextContent("80.0kg"));
    const chip = within(card).getByText("−1.2 kg this week");
    expect(chip).toHaveClass("trd-chip--good");
    expect(card).toHaveTextContent("7-day avg 80.1 kg · goal 75.0 kg");
    expect(within(card).getByRole("group", { name: "Chart range" })).toBeInTheDocument();
    fireEvent.input(within(card).getByLabelText("Weight (kg)"), { target: { value: "79.6" } });
    fireEvent.click(within(card).getByRole("button", { name: "Log" }));
    await waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url.includes("/v1/body/command"))).toBe(true));
    const sent = calls.find((c) => c.method === "POST" && c.url.includes("/v1/body/command"))!.body as { command: unknown };
    expect(sent.command).toEqual({ type: "log_entry", weight: "79.6" });
  });

  it("has friendly empty states with one action each", async () => {
    const empty = bodyView();
    engine(
      () => gymView({ active: null }),
      () => ({ ...empty, entries: [], series: [], stats: { ...empty.stats, count: 0 }, stat_labels: {} }),
    );
    renderDesk();
    expect(await screen.findByText("No routines yet. Build one and start it with one click.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create routine" })).toBeInTheDocument();
    expect(screen.getByText("Your finished workouts show up here, ready to repeat.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start your first workout" })).toBeInTheDocument();
    expect(screen.getByText("No sessions in the last 30 days yet.")).toBeInTheDocument();
    expect(await screen.findByText("No weigh-ins yet. Log your first one below.")).toBeInTheDocument();
    expect(screen.queryByText(/^0 kg$/)).toBeNull();
  });

  it("turns the start button into Continue during a workout", async () => {
    engine(() => ({ ...trainedView(), state: { ...trainedView().state, active: gymView().state.active } }));
    renderDesk();
    expect(await screen.findByRole("link", { name: /Continue workout/ })).toHaveAttribute("href", "/gym/workout");
    expect(screen.queryByRole("button", { name: /^Start workout$/ })).toBeNull();
    expect(screen.getByRole("region", { name: "Workout in progress" })).toHaveTextContent("Push day");
    expect(screen.getByRole("button", { name: "Start Legs" })).toBeDisabled();
  });
});
