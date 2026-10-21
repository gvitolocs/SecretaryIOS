import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import type { BodyStats, BodyView } from "../api/body";
import { resetBody } from "../state/body";
import { markLucinaOnline } from "../state/lucina";
import { stubFetch, type Call } from "../test/fetch";
import WeightView from "./Weight";

const STATS: BodyStats = {
  count: 2,
  current_grams: 80500,
  current_measured_ms: Date.UTC(2026, 9, 21, 7, 30),
  avg7_grams: 80300,
  weekly_change_grams: 400,
  min_grams: 79100,
  max_grams: 80900,
  trend30_grams: -300,
  trend90_grams: 1200,
  goal_grams: 80000,
  goal_distance_grams: 300,
  goal_reached: false,
};

function bodyView(overrides: Partial<BodyView> = {}): BodyView {
  return {
    revision: 4,
    settings: { unit: "kg", goal_grams: 80000 },
    entries: [
      { id: "e1", weight_grams: 80500, measured_ms: Date.UTC(2026, 9, 21, 7, 30), note: "Morning" },
      { id: "e2", weight_grams: 80100, measured_ms: Date.UTC(2026, 9, 20, 7, 30), note: "" },
    ],
    stats: STATS,
    series: [
      { entry_id: "e2", measured_ms: Date.UTC(2026, 9, 20, 7, 30), weight_grams: 80100, avg7_grams: 80100 },
      { entry_id: "e1", measured_ms: Date.UTC(2026, 9, 21, 7, 30), weight_grams: 80500, avg7_grams: 80300 },
    ],
    labels: { e1: "80.5 kg", e2: "80.1 kg" },
    stat_labels: {
      current: "80.5 kg",
      avg7: "80.3 kg",
      weekly_change: "+0.4 kg",
      min: "79.1 kg",
      max: "80.9 kg",
      trend30: "-0.3 kg",
      trend90: "+1.2 kg",
      goal: "80 kg",
      goal_distance: "+0.3 kg",
    },
    ...overrides,
  };
}

const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST").map((c) => c.body as { op_id?: string; command: Record<string, unknown> });

function submitLog() {
  const form = screen.getByRole("button", { name: "Log weigh-in" }).closest("form");
  if (!form) throw new Error("quick log form missing");
  fireEvent.submit(form);
}

describe("Weight", () => {
  beforeEach(() => {
    resetBody();
    markLucinaOnline();
    localStorage.clear();
  });

  it("renders the stats cards from the engine's labels", async () => {
    const calls = stubFetch(() => ({ body: { ok: true, view: bodyView() } }));
    render(() => <WeightView />);

    expect(await screen.findByLabelText("Current")).toHaveTextContent("80.5 kg");
    expect(screen.getByLabelText("7-day average")).toHaveTextContent("80.3 kg");
    expect(screen.getByLabelText("Weekly change")).toHaveTextContent("+0.4 kg");
    expect(screen.getByLabelText("Goal distance")).toHaveTextContent("+0.3 kg");
    expect(screen.getByLabelText("Goal distance")).toHaveTextContent("80 kg");
    expect(screen.getByText(/Min 79.1 kg · Max 80.9 kg · 30d -0.3 kg · 90d \+1.2 kg/)).toBeInTheDocument();
    expect(screen.getByText("80.1 kg")).toBeInTheDocument();
    expect(calls[0]?.url).toMatch(/^\/v1\/body\/state\?timezone=/);
  });

  it("shows pound labels, a reached goal, and a missing goal", async () => {
    stubFetch(() => ({
      body: {
        ok: true,
        view: bodyView({
          settings: { unit: "lb", goal_grams: 81000 },
          stats: { ...STATS, goal_grams: 81000, goal_distance_grams: -700, goal_reached: true },
          stat_labels: { current: "177.5 lb", avg7: "177 lb", weekly_change: "+0.9 lb", goal: "178.6 lb", goal_distance: "-1.5 lb" },
        }),
      },
    }));
    render(() => <WeightView />);
    expect(await screen.findByLabelText("Current")).toHaveTextContent("177.5 lb");
    expect(screen.getByLabelText("Weekly change")).toHaveTextContent("+0.9 lb");
    expect(screen.getByLabelText("Goal distance")).toHaveTextContent("Goal reached");
  });

  it("shows the empty state and dashes without data", async () => {
    const nulls = Object.fromEntries(Object.keys(STATS).map((k) => [k, null])) as unknown as BodyStats;
    stubFetch(() => ({
      body: {
        ok: true,
        view: bodyView({ settings: { unit: "kg", goal_grams: null }, entries: [], series: [], labels: {}, stat_labels: {}, stats: { ...nulls, count: 0 } }),
      },
    }));
    render(() => <WeightView />);
    expect(await screen.findByText("No weigh-ins yet. Log your first one above.")).toBeInTheDocument();
    expect(screen.getByLabelText("Current")).toHaveTextContent("—");
    expect(screen.getByLabelText("Goal distance")).toHaveTextContent("Set a goal");
    expect(screen.getByText("Not enough data to chart yet.")).toBeInTheDocument();
  });

  it("sends the typed text in the display unit for the engine to parse", async () => {
    const calls = stubFetch((call) =>
      call.method === "GET" ? { body: { ok: true, view: bodyView() } } : { body: { ok: true, view: bodyView({ revision: 5 }) } },
    );
    render(() => <WeightView />);
    await screen.findByLabelText("Current");
    const input = screen.getByLabelText("Weight");
    fireEvent.input(input, { target: { value: "80,5" } });
    submitLog();

    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]).toMatchObject({ expected_revision: 4, command: { type: "log_entry", weight: "80,5" } });
    expect(posts(calls)[0]?.command).not.toHaveProperty("weight_grams");
    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("asks the engine to parse a weight typed in the other unit", async () => {
    const calls = stubFetch((call) => {
      if (call.method === "GET") return { body: { ok: true, view: bodyView() } };
      const cmd = (call.body as { command: { type: string } }).command;
      if (cmd.type === "parse_weight") return { body: { ok: true, replayed: false, weight_grams: 80060 } };
      return { body: { ok: true, view: bodyView({ revision: 5 }) } };
    });
    render(() => <WeightView />);
    await screen.findByLabelText("Current");
    fireEvent.click(screen.getByRole("button", { name: "lb" }));
    fireEvent.input(screen.getByLabelText("Weight"), { target: { value: "176,5" } });
    submitLog();

    await waitFor(() => expect(posts(calls)).toHaveLength(2));
    const [parse, log] = posts(calls);
    expect(parse).toMatchObject({ command: { type: "parse_weight", value: "176,5", unit: "lb" } });
    expect(parse?.op_id).toBeUndefined();
    expect(log).toMatchObject({ command: { type: "log_entry", weight_grams: 80060 } });
  });

  it("shows the engine's validation message and keeps the text", async () => {
    stubFetch((call) =>
      call.method === "GET"
        ? { body: { ok: true, view: bodyView() } }
        : { status: 422, body: { ok: false, replayed: false, code: "invalid_command", error: "Use at most three decimal places" } },
    );
    render(() => <WeightView />);
    await screen.findByLabelText("Current");
    const input = screen.getByLabelText("Weight");
    fireEvent.input(input, { target: { value: "1.0001" } });
    submitLog();

    expect(await screen.findAllByText("Use at most three decimal places")).not.toHaveLength(0);
    expect(input).toHaveValue("1.0001");
  });

  it("draws the weight and 7-day average lines from the series", async () => {
    const { container } = (() => {
      stubFetch(() => ({ body: { ok: true, view: bodyView() } }));
      return render(() => <WeightView />);
    })();
    await screen.findByLabelText("Current");
    expect(container.querySelector(".chart-line--primary")).not.toBeNull();
    expect(container.querySelector(".chart-line--secondary")).not.toBeNull();
  });
});
