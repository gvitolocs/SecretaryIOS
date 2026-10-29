import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GymCommand, GymView, Workout } from "../../api/gym";
import { resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { stubWide } from "../../test/wide";
import { gymView } from "./fixture";
import GymHome from "./GymHome";
import JournalView from "./History";
import HistoryDetailView from "./HistoryDetail";
import ProgressView from "./Progress";

const DAY = 86_400_000;

type Envelope = { op_id: string; expected_revision: number; command: GymCommand };
const posts = (calls: Call[]) =>
  calls.filter((c) => c.method === "POST" && c.url.includes("/v1/gym/")).map((c) => c.body as Envelope);

function workout(id: string, title: string, finished: number): Workout {
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
        sets: [{ id: `${id}-s1`, kind: "normal", weight_grams: 60_000, reps: 5, seconds: 0, distance_meters: 0, completed: true }],
      },
    ],
  };
}

function view(): GymView {
  const base = gymView({ active: null });
  const now = new Date(2026, 9, 28, 12).getTime();
  return {
    ...base,
    state: {
      ...base.state,
      history: [workout("w-oct", "Push", now - 2 * DAY), workout("w-sep", "Pull", now - 40 * DAY)],
      measurements: [{ id: "m1", timestamp_ms: now - DAY, kind: "waist", value_milli: 82_000 }],
    },
    records: [{ exercise_id: "bench-press", name: "Bench Press", weight: "60 kg", reps: 5, volume: "300 kg", seconds: 0, distance_meters: 0, estimated_1rm: "70 kg" }],
    chart: [{ id: "w-oct", label: "", volume: 300 }],
    weight_labels: { "w-oct-s1": "60", "w-sep-s1": "60" },
    measurement_labels: { m1: "82 cm" },
    workout_labels: {
      "w-oct": { sets: 1, volume: "300 kg", duration: "1h 00m" },
      "w-sep": { sets: 1, volume: "300 kg", duration: "1h 00m" },
    },
  };
}

function engine(v: () => GymView) {
  return stubFetch((call) => {
    if (call.url.includes("/v1/body/")) return { status: 404, body: { error: "none" } };
    if (call.method === "GET") return { body: { ok: true, view: v() } };
    return { body: { ok: true, view: { ...v(), state: { ...v().state, revision: v().state.revision + 1 } } } };
  });
}

function renderGym(path: string) {
  return renderAt(path, () => (
    <>
      <Route path="/gym" component={GymHome} />
      <Route path="/gym/history" component={JournalView} />
      <Route path="/gym/history/:id" component={HistoryDetailView} />
      <Route path="/gym/progress" component={ProgressView} />
      <Route path="/gym/workout" component={() => <p>Workout</p>} />
    </>
  ));
}

async function header(current: string) {
  expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Training");
  const nav = screen.getByRole("navigation", { name: "Gym sections" });
  const link = within(nav).getByRole("link", { name: current });
  expect(link).toHaveAttribute("aria-current", "page");
  expect(await screen.findAllByRole("button", { name: /^Start workout$/ })).toHaveLength(1);
  // The phone pill bar and its grey cards are gone on a desktop.
  expect(document.querySelector(".gym-subnav")).toBeNull();
  expect(document.querySelector(".card")).toBeNull();
}

describe("desktop Gym sections", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
    stubWide();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("keeps the Training header on the Journal and groups workouts by month", async () => {
    engine(view);
    renderGym("/gym/history");
    await header("Journal");
    expect(await screen.findByRole("region", { name: "October 2026" })).toHaveTextContent("Push");
    expect(screen.getByRole("region", { name: "September 2026" })).toHaveTextContent("Pull");
    expect(screen.getByRole("region", { name: "Totals" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Push" })).toHaveAttribute("href", "/gym/history/w-oct");
  });

  it("repeats a workout from its journal row", async () => {
    const calls = engine(view);
    renderGym("/gym/history");
    fireEvent.click(await screen.findByRole("button", { name: "Repeat Push" }));
    await waitFor(() => expect(posts(calls).map((p) => p.command)).toContainEqual({ type: "repeat_workout", workout_id: "w-oct" }));
  });

  it("shows a friendly empty journal with one action", async () => {
    engine(() => ({ ...view(), state: { ...view().state, history: [] } }));
    renderGym("/gym/history");
    await header("Journal");
    expect(screen.getByText("Your story starts here")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start your first workout" })).toBeInTheDocument();
  });

  it("keeps the header on Progress, with records as rows and measurements in the rail", async () => {
    engine(view);
    renderGym("/gym/progress");
    await header("Progress");
    const records = await screen.findByRole("region", { name: "Personal records" });
    expect(within(records).getByRole("link", { name: "Bench Press" })).toHaveAttribute("href", "/gym/progress/bench-press");
    expect(within(records).getByText("Heaviest weight 60 kg · Most reps 5")).toBeInTheDocument();
    const body = screen.getByRole("region", { name: "Body measurements" });
    expect(within(body).getByText("82 cm")).toBeInTheDocument();
  });

  it("adds a measurement in a sheet", async () => {
    const calls = engine(view);
    renderGym("/gym/progress");
    fireEvent.click(await screen.findByRole("button", { name: "Add measurement" }));
    const dialog = screen.getByRole("dialog", { name: "New measurement" });
    fireEvent.input(within(dialog).getByRole("textbox"), { target: { value: "81.5" } });
    fireEvent.submit(within(dialog).getByRole("form", { name: "New measurement" }));
    await waitFor(() =>
      expect(posts(calls).map((p) => p.command)).toContainEqual({ type: "record_measurement", kind: "waist", value: "81.5" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("keeps the header on Settings, with the preferences on one surface", async () => {
    engine(view);
    renderGym("/gym?tab=settings");
    await header("Settings");
    expect(await screen.findByRole("form", { name: "Training preferences" })).toHaveClass("pg-card");
    expect(screen.getByRole("region", { name: "Your data" })).toHaveClass("pg-card");
    expect(screen.getByRole("region", { name: "Lucina Gym" })).toBeInTheDocument();
  });

  it("shows a journal entry under the same header, with its summary in the rail", async () => {
    engine(view);
    renderGym("/gym/history/w-oct");
    await header("Journal");
    expect(screen.getByRole("link", { name: "‹ Journal" })).toHaveAttribute("href", "/gym/history");
    expect(screen.getByRole("region", { name: "Bench Press" })).toHaveTextContent("60 kg × 5");
    expect(screen.getByRole("region", { name: "Summary" })).toHaveTextContent("1h 00m");
    expect(screen.getByRole("button", { name: "Repeat workout" })).toBeEnabled();
  });
});
