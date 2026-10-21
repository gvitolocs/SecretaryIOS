import { describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { getGymClock, getGymState, sendGymCommand, type GymView } from "./gym";

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

describe("gym API", () => {
  it("normalises both the engine response and a bare view", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, view: view(7) } }));
    expect((await getGymState("Europe/Rome")).state.revision).toBe(7);
    expect(calls[0]?.url).toBe("/v1/gym/state?timezone=Europe%2FRome");

    stubFetch(() => ({ body: view(9) }));
    expect((await getGymState("UTC")).state.revision).toBe(9);
  });

  it("reads the clock from GET /v1/gym/clock", async () => {
    const calls = stubFetch(() => ({
      body: { ok: true, replayed: false, clock: { rest_remaining: 42, active_duration: "12m 05s", active_elapsed_seconds: 725 } },
    }));
    expect(await getGymClock("UTC")).toEqual({ rest_remaining: 42, active_duration: "12m 05s", active_elapsed_seconds: 725 });
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/gym/clock?timezone=UTC" });
    expect(calls[0]?.headers["x-secretary-csrf"]).toBeUndefined();
  });

  it("sends a complete command envelope", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, view: view(4) } }));
    await sendGymCommand({
      op_id: "11111111-1111-4111-8111-111111111111",
      expected_revision: 3,
      timezone: "Europe/Rome",
      command: { type: "start_rest", seconds: 90 },
    });

    expect(calls).toHaveLength(1);
    const call = calls[0]!;
    expect(call).toMatchObject({
      method: "POST",
      url: "/v1/gym/command",
      credentials: "same-origin",
    });
    expect(call.headers["x-secretary-csrf"]).toBe("1");
    expect(call.headers["content-type"]).toBe("application/json");
    expect(call.body).toMatchObject({
      op_id: "11111111-1111-4111-8111-111111111111",
      expected_revision: 3,
      timezone: "Europe/Rome",
      command: { type: "start_rest", seconds: 90 },
    });
  });
});
