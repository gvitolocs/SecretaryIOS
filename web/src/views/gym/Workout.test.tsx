import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GymCommand } from "../../api/gym";
import { resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { gymView } from "./fixture";
import WorkoutView from "./Workout";

type Envelope = { op_id: string; expected_revision: number; command: GymCommand };
const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST").map((c) => c.body as Envelope);

function renderWorkout() {
  return renderAt("/gym/workout", () => (
    <>
      <Route path="/gym/workout" component={WorkoutView} />
      <Route path="*" component={() => <p>Elsewhere</p>} />
    </>
  ));
}

/** A tiny engine stand-in: every command bumps the revision. */
function engine(opts: { restRemaining?: number } = {}) {
  let revision = 5;
  return stubFetch((call) => {
    if (call.method === "GET" && call.url.startsWith("/v1/gym/clock")) {
      return { body: { ok: true, replayed: false, clock: { rest_remaining: 0, active_duration: "5m 00s", active_elapsed_seconds: 300 } } };
    }
    if (call.method === "GET") return { body: { ok: true, view: gymView({ revision, restRemaining: opts.restRemaining }) } };
    const env = call.body as Envelope;
    revision += 1;
    const active = env.command.type === "cancel_workout" ? null : undefined;
    return { body: { ok: true, view: gymView({ revision, active }) } };
  });
}

describe("Workout", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
  });
  afterEach(() => vi.useRealTimers());

  it("renders set rows for each exercise kind", async () => {
    engine();
    renderWorkout();
    const bench = await screen.findByRole("region", { name: "Bench Press" });
    expect(within(bench).getByLabelText(/^Set 1 Weight \(kg\)/)).toHaveValue("60");
    expect(within(bench).getByLabelText(/^Set 1 Reps/)).toHaveValue("8");
    expect(within(bench).getByText("Previous · 57.5 kg × 8")).toBeInTheDocument();
    const plank = screen.getByRole("region", { name: "Plank" });
    expect(within(plank).getByLabelText(/^Set 1 Seconds/)).toHaveValue("60");
    expect(within(plank).queryByLabelText(/Weight/)).toBeNull();
    expect(screen.getByText("3m 05s")).toBeInTheDocument();
  });

  it("saves a changed set before completing it, each with the current revision", async () => {
    const calls = engine();
    renderWorkout();
    const bench = await screen.findByRole("region", { name: "Bench Press" });
    fireEvent.input(within(bench).getByLabelText(/^Set 1 Weight/), { target: { value: "62.5" } });
    fireEvent.click(within(bench).getByRole("button", { name: "Complete set 1" }));

    await waitFor(() => expect(posts(calls)).toHaveLength(2));
    const [edit, complete] = posts(calls);
    expect(edit).toMatchObject({
      expected_revision: 5,
      command: { type: "edit_set", set_id: "s-bench-1", weight: "62.5", reps: 8, seconds: 0, distance_meters: 0, kind: "normal" },
    });
    expect(complete).toMatchObject({
      expected_revision: 6,
      command: { type: "complete_set", set_id: "s-bench-1", completed: true },
    });
  });

  it("does not send a set with a non-integer rep count", async () => {
    const calls = engine();
    renderWorkout();
    const bench = await screen.findByRole("region", { name: "Bench Press" });
    fireEvent.input(within(bench).getByLabelText(/^Set 1 Reps/), { target: { value: "8.5" } });
    fireEvent.click(within(bench).getByRole("button", { name: "Complete set 1" }));
    expect(await within(bench).findByRole("alert")).toHaveTextContent("Reps must be a whole number.");
    expect(posts(calls)).toHaveLength(0);
  });

  it("counts the rest timer down locally and resyncs at zero", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    const calls = engine({ restRemaining: 90 });
    renderWorkout();
    expect(await screen.findByRole("timer")).toHaveTextContent("Rest · 1:30");

    vi.advanceTimersByTime(2_000);
    await waitFor(() => expect(screen.getByRole("timer")).toHaveTextContent("Rest · 1:28"));

    vi.advanceTimersByTime(88_000);
    await waitFor(() => expect(screen.queryByRole("timer")).toBeNull());
    expect(calls.some((c) => c.method === "GET" && c.url.startsWith("/v1/gym/clock?timezone="))).toBe(true);
    expect(screen.getByRole("button", { name: /Start rest timer/ })).toBeInTheDocument();
  });

  it("needs a second click before discarding", async () => {
    const calls = engine();
    renderWorkout();
    fireEvent.click(await screen.findByRole("button", { name: "Discard workout" }));
    expect(posts(calls)).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Discard for good" }));
    await waitFor(() => expect(posts(calls).map((p) => p.command.type)).toEqual(["cancel_workout"]));
    expect(await screen.findByText("Elsewhere")).toBeInTheDocument();
  });
});
