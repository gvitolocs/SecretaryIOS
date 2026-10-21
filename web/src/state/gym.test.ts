import { beforeEach, describe, expect, it } from "vitest";
import type { GymView } from "../api/gym";
import { stubFetch, type Call } from "../test/fetch";
import { gym, resetGym, restRemainingNow, runGym } from "./gym";
import { lucinaNotice, markLucinaOnline } from "./lucina";

function view(revision: number): GymView {
  return {
    state: {
      schema_version: 1,
      revision,
      settings: { weight_unit: "kg", rest_seconds: 90, weekly_goal: 3 },
      exercises: [],
      routines: [],
      active: null,
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
      active_duration: "0m 00s",
      active_completed_sets: 0,
    },
    records: [],
    chart: [],
    rest_remaining: 0,
    weight_labels: {},
    previous_labels: {},
    measurement_labels: {},
    workout_labels: {},
  };
}

function envelope(call: Call): { op_id: string; expected_revision: number; timezone: string } {
  return call.body as { op_id: string; expected_revision: number; timezone: string };
}

const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST");

describe("gym state", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
  });

  it("retries a conflict once with a new op_id and the new revision (docs: rebuild with a new op_id)", async () => {
    let gets = 0;
    let postCount = 0;
    const calls = stubFetch((call) => {
      if (call.method === "GET") {
        gets++;
        return { body: { ok: true, view: view(gets) } };
      }
      postCount++;
      if (postCount === 1) {
        return { status: 409, body: { ok: false, code: "conflict", error: "State changed." } };
      }
      return { body: { ok: true, replayed: false, view: view(3) } };
    });

    const result = await runGym({ type: "start_workout", title: "One" });
    expect(result?.ok).toBe(true);

    const sent = posts(calls);
    expect(sent).toHaveLength(2);
    const first = envelope(sent[0]!);
    const second = envelope(sent[1]!);
    expect(first.op_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(second.op_id).not.toBe(first.op_id);
    expect(second.op_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(first.expected_revision).toBe(1);
    expect(second.expected_revision).toBe(2);
    expect(gets).toBe(2);
    expect(gym.view?.state.revision).toBe(3);
  });

  it("stops after a second conflict and explains", async () => {
    let gets = 0;
    let postCount = 0;
    const calls = stubFetch((call) => {
      if (call.method === "GET") {
        gets++;
        return { body: { ok: true, view: view(gets) } };
      }
      postCount++;
      return { status: 409, body: { ok: false, code: "conflict", error: "State changed." } };
    });

    expect(await runGym({ type: "start_workout", title: "One" })).toBeNull();
    expect(posts(calls)).toHaveLength(2);
    expect(postCount).toBe(2);
    expect(gym.actionError).toBe("This changed somewhere else. Refreshed \u2014 please try again.");
  });

  it("shows the engine message on a 422", async () => {
    let postCount = 0;
    const calls = stubFetch((call) => {
      if (call.method === "GET") return { body: { ok: true, view: view(1) } };
      postCount++;
      return {
        status: 422,
        body: { ok: false, code: "invalid_command", error: "Finish or discard your current workout first." },
      };
    });

    expect(await runGym({ type: "start_workout", title: "Two" })).toBeNull();
    expect(postCount).toBe(1);
    expect(posts(calls)).toHaveLength(1);
    expect(gym.actionError).toBe("Finish or discard your current workout first.");
  });

  it("queues commands so the next one carries the previous revision", async () => {
    const bodies: { expected_revision: number }[] = [];
    const calls = stubFetch((call) => {
      if (call.method === "GET") return { body: { ok: true, view: view(1) } };
      const body = call.body as { expected_revision: number };
      bodies.push(body);
      return { body: { ok: true, view: view(bodies.length + 1) } };
    });

    const first = runGym({ type: "start_workout", title: "One" });
    const second = runGym({ type: "start_rest", seconds: 90 });
    await Promise.all([first, second]);

    expect(posts(calls)).toHaveLength(2);
    expect(bodies[0]?.expected_revision).toBe(1);
    expect(bodies[1]?.expected_revision).toBe(2);
    expect(gym.view?.state.revision).toBe(3);
  });

  it("raises the shared offline banner on a 502", async () => {
    stubFetch((call) => {
      if (call.method === "GET") return { body: { ok: true, view: view(1) } };
      return { status: 502 };
    });

    expect(await runGym({ type: "start_workout", title: "One" })).toBeNull();
    expect(lucinaNotice()).toBe("Secretary's server isn't reachable right now. Your last change wasn't saved.");
  });

  it("ticks the rest countdown from the received snapshot", () => {
    expect(restRemainingNow({ rest_remaining: 90, receivedAt: 1_000 }, 1_000)).toBe(90);
    expect(restRemainingNow({ rest_remaining: 90, receivedAt: 1_000 }, 2_500)).toBe(89);
    expect(restRemainingNow({ rest_remaining: 2, receivedAt: 1_000 }, 99_000)).toBe(0);
    expect(restRemainingNow(null, 1_000)).toBe(0);
  });
});
