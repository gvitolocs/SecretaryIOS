import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BodyStats, BodyView } from "../api/body";
import { resetBody } from "../state/body";
import { markLucinaOnline } from "../state/lucina";
import { stubFetch, type Call } from "../test/fetch";
import { stubWide } from "../test/wide";
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
    expect(container.querySelector(".chart-line--secondary")).toBeNull();
  });
});


describe("Weight desktop page", () => {
  beforeEach(() => { resetBody(); markLucinaOnline(); localStorage.clear(); stubWide(); });
  afterEach(() => vi.unstubAllGlobals());
  it("keeps the header above its chart, rows, quiet log form and stats rail", async () => {
    stubFetch(() => ({ body: { ok: true, view: bodyView() } }));
    render(() => <WeightView />);
    expect(screen.getByRole("heading", { level: 1, name: "Weight" })).toBeInTheDocument();
    expect(await screen.findByRole("region", { name: "Weigh-ins" })).toHaveTextContent("80.1 kg");
    expect(screen.getByRole("list", { name: "Weigh-ins" })).toHaveClass("pg-rows");
    expect(screen.getByRole("region", { name: "Trend" })).toContainElement(screen.getByRole("img", { name: "Body weight trend with 7-day average" }));
    expect(screen.getByRole("region", { name: "Statistics" }).querySelector(".pg-stats")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "‹ Training" })).toHaveAttribute("href", "/training");
    expect(screen.getByRole("button", { name: "Log weigh-in" })).toHaveClass("pg-btn");
    expect(document.querySelector(".pg-context")).toHaveTextContent("80.5 kg");
    expect(document.querySelector(".pg-grid--rail")).toBeInTheDocument();
    expect(document.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
    expect(document.querySelector(".view-title, .stat-grid, .weight-list, .segmented, .pg-seg")).toBeNull();
  });
  it("logs from the rail, edits in a sheet and confirms deletion", async () => {
    let revision = 4;
    const calls = stubFetch(call => ({ body: { ok: true, view: bodyView({revision: call.method === "GET" ? revision : ++revision}) } }));
    render(() => <WeightView />);
    await screen.findByRole("region", { name: "Weigh-ins" });
    fireEvent.input(screen.getByLabelText("Weight"), { target: { value: "80,5" } });
    submitLog();
    await waitFor(() => expect(posts(calls)[0]?.command).toEqual({ type: "log_entry", weight: "80,5" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Log weigh-in" })).toBeEnabled());
    const row = within(screen.getByRole("list", { name: "Weigh-ins" })).getAllByRole("listitem")[0]!;
    fireEvent.click(within(row).getByRole("button", { name: "Edit" }));
    const sheet = screen.getByRole("dialog", { name: "Edit weigh-in" });
    fireEvent.input(within(sheet).getByLabelText("Note"), { target: { value: "Updated note" } });
    fireEvent.submit(within(sheet).getByRole("form", { name: "Edit weigh-in" }));
    await waitFor(() => expect(posts(calls)[1]?.command).toEqual({ type: "edit_entry", entry_id: "e1", note: "Updated note" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.click(within(row).getByRole("button", { name: "Delete" }));
    expect(posts(calls)).toHaveLength(2);
    const confirm = within(row).getByRole("button", { name: "Confirm delete" });
    expect(confirm).toHaveClass("pg-btn--danger");
    fireEvent.click(confirm);
    await waitFor(() => expect(posts(calls)[2]?.command).toEqual({ type: "delete_entry", entry_id: "e1" }));
  });
  it("keeps the header for load failures and uses friendly empty cards after retry", async () => {
    let failed = true;
    stubFetch(() => failed ? { status: 503, body: {error: "Offline"} } : { body: {ok: true, view: bodyView({entries: [], series: []})} });
    render(() => <WeightView />);
    expect(await screen.findByRole("alert")).toHaveClass("pg-error");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Weight");
    failed = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Log your first weigh-in in the form beside this list.")).toHaveClass("pg-empty-text");
    expect(screen.getByText("Log a few weigh-ins to see your trend.")).toHaveClass("pg-empty-text");
  });
});
