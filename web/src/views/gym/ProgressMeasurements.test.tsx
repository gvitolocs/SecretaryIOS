import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import type { GymCommand, GymView } from "../../api/gym";
import { resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { barLayout } from "./BarChart";
import { gymView } from "./fixture";
import ProgressView from "./Progress";

type Envelope = { command: GymCommand };
const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST").map((c) => c.body as Envelope);

function progressView(): GymView {
  const base = gymView({ active: null });
  return {
    ...base,
    state: {
      ...base.state,
      measurements: [
        { id: "m-waist", timestamp_ms: 1_792_584_000_000, kind: "waist", value_milli: 82_500 },
        { id: "m-old", timestamp_ms: 1_792_000_000_000, kind: "body_weight", value_milli: 80_000 },
      ],
    },
    measurement_labels: { "m-waist": "82.5 cm", "m-old": "80 kg" },
    chart: [
      { id: "w1", label: "20 Oct", volume: 1200 },
      { id: "w2", label: "21 Oct", volume: 800 },
      { id: "w3", label: "21 Oct", volume: 0 },
    ],
  };
}

function renderProgress() {
  const view = progressView();
  const calls = stubFetch((call) =>
    call.method === "GET"
      ? { body: { ok: true, view } }
      : { body: { ok: true, view: { ...view, state: { ...view.state, revision: view.state.revision + 1 } } } },
  );
  renderAt("/gym/progress", () => (
    <>
      <Route path="/gym/progress" component={ProgressView} />
      <Route path="*" component={() => <p>Elsewhere</p>} />
    </>
  ));
  return calls;
}

describe("Progress: body measurements", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
  });

  it("lists measurements and links body weight to the Weight tab", async () => {
    renderProgress();
    const list = await screen.findByRole("list", { name: "Measurements" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows[0]).toHaveTextContent("Waist");
    expect(rows[0]).toHaveTextContent("82.5 cm");
    expect(rows[1]).toHaveTextContent("Body weight");
    expect(screen.getByRole("link", { name: /Body weight.*Weight tab/ })).toHaveAttribute("href", "/weight");
    expect(screen.getByRole("heading", { name: "Body measurements" })).toBeInTheDocument();
  });

  it("records a waist or body-fat value as text for the engine to parse", async () => {
    const calls = renderProgress();
    fireEvent.click(await screen.findByRole("button", { name: "Add measurement" }));
    const form = screen.getByRole("form", { name: "New measurement" });
    const kind = within(form).getByLabelText("Measurement") as HTMLSelectElement;
    expect(Array.from(kind.options).map((o) => o.value)).toEqual(["body_fat", "waist"]);
    fireEvent.change(kind, { target: { value: "body_fat" } });
    expect(within(form).getByLabelText("Value (%)")).toBeInTheDocument();

    fireEvent.click(within(form).getByRole("button", { name: "Save measurement" }));
    expect(within(form).getByRole("alert")).toHaveTextContent("Enter a value.");
    fireEvent.input(within(form).getByLabelText("Value (%)"), { target: { value: "18,5" } });
    fireEvent.click(within(form).getByRole("button", { name: "Save measurement" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "record_measurement", kind: "body_fat", value: "18,5" });
    await waitFor(() => expect(screen.queryByRole("form", { name: "New measurement" })).toBeNull());
  });

  it("deletes a measurement after a second click", async () => {
    const calls = renderProgress();
    fireEvent.click(await screen.findByRole("button", { name: "Delete Waist measurement" }));
    expect(posts(calls)).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Delete measurement" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "delete_measurement", measurement_id: "m-waist" });
  });

  it("draws one bar per workout, even two on the same day", async () => {
    renderProgress();
    const chart = await screen.findByRole("img", { name: "Volume over the most recent 3 workouts" });
    const bars = chart.querySelectorAll("rect.bar-chart-bar");
    expect(bars).toHaveLength(3);
    expect(Array.from(bars).map((b) => b.querySelector("title")?.textContent)).toEqual([
      "20 Oct: 1,200 kg",
      "21 Oct: 800 kg",
      "21 Oct: 0 kg",
    ]);
    expect(screen.queryByText("Make your first mark")).toBeInTheDocument();
  });

  it("scales bars from zero", () => {
    const { bars, max } = barLayout(
      [
        { id: "a", label: "a", volume: 100 },
        { id: "b", label: "b", volume: 50 },
      ],
      "kg",
    );
    expect(max).toBe(100);
    expect(bars[0]!.height).toBeCloseTo(2 * bars[1]!.height, 5);
    expect(bars[0]!.x).toBeLessThan(bars[1]!.x);
  });
});
