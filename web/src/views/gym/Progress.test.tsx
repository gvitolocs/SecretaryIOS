import { Route } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import type { GymView } from "../../api/gym";
import { resetGym } from "../../state/gym";
import { stubFetch } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { gymView } from "./fixture";
import ProgressDetailView from "./ProgressDetail";

const RECORD = {
  exercise_id: "squat",
  name: "Back Squat",
  weight: "100 kg",
  reps: 5,
  volume: "500 kg",
  seconds: 0,
  distance_meters: 0,
};

function withRecords(extra: Partial<GymView>, estimated?: string): GymView {
  return {
    ...gymView({ active: null }),
    records: [estimated ? { ...RECORD, estimated_1rm: estimated } : RECORD],
    ...extra,
  };
}

function renderDetail(view: GymView) {
  stubFetch(() => ({ body: { ok: true, view } }));
  renderAt("/gym/progress/squat", () => <Route path="/gym/progress/:exerciseId" component={ProgressDetailView} />);
}

describe("Progress detail", () => {
  beforeEach(() => resetGym());

  it("shows the estimated 1RM and trend chart when the server provides them", async () => {
    renderDetail(
      withRecords(
        {
          exercise_charts: {
            squat: [
              { id: "a", label: "01 Oct", volume: 400 },
              { id: "b", label: "08 Oct", volume: 450 },
            ],
          },
        },
        "116.7 kg",
      ),
    );
    expect(await screen.findByRole("heading", { name: "Back Squat" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Estimated 1RM" })).toHaveTextContent("116.7 kg");
    expect(screen.getByRole("img", { name: /Volume trend for Back Squat/ })).toBeInTheDocument();
    expect(screen.getByText("100 kg")).toBeInTheDocument();
    expect(screen.getByText("Best set volume · 500 kg")).toBeInTheDocument();
  });

  it("degrades gracefully without the proposed fields", async () => {
    renderDetail(withRecords({}));
    expect(await screen.findByRole("heading", { name: "Back Squat" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Estimated 1RM" })).toBeNull();
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByText("No per-exercise trend yet.")).toBeInTheDocument();
  });

  it("does not draw a single-point trend", async () => {
    renderDetail(withRecords({ exercise_charts: { squat: [{ id: "a", label: "01 Oct", volume: 400 }] } }));
    expect(await screen.findByText("No per-exercise trend yet.")).toBeInTheDocument();
  });
});
