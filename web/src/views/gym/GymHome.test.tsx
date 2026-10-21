import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GymCommand, GymView } from "../../api/gym";
import { setLocale } from "../../i18n";
import { elapsedNow, resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { gymView } from "./fixture";
import GymHome from "./GymHome";
import { moveItem } from "./RoutineBuilder";

type Envelope = { op_id: string; expected_revision: number; command: GymCommand };
const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST").map((c) => c.body as Envelope);

function renderHome(path = "/gym") {
  return renderAt(path, () => (
    <>
      <Route path="/gym" component={GymHome} />
      <Route path="*" component={() => <p>Elsewhere</p>} />
    </>
  ));
}

function engine(view: () => GymView) {
  return stubFetch((call) => {
    if (call.method === "GET") return { body: { ok: true, view: view() } };
    return { body: { ok: true, view: { ...view(), state: { ...view().state, revision: view().state.revision + 1 } } } };
  });
}

describe("Training", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
  });
  afterEach(() => vi.useRealTimers());

  it("shows the original hero, totals and footer", async () => {
    engine(() => gymView({ active: null }));
    renderHome();
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Make timefor strength.");
    expect(screen.getByText("Lucina / Gym")).toBeInTheDocument();
    expect(screen.getByText("0 / 3 workouts")).toBeInTheDocument();
    expect(screen.getByText("Sets logged")).toBeInTheDocument();
    expect(screen.getByText("Total volume")).toBeInTheDocument();
    expect(screen.getByText("A little stronger, one session at a time.")).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Gym sections" });
    expect(within(nav).getAllByRole("link").map((a) => a.textContent)).toEqual(["Training", "Journal", "Progress", "Settings"]);
    expect(within(nav).getByRole("link", { name: "Training" })).toHaveAttribute("aria-current", "page");
  });

  it("ticks the active workout's duration live from active_elapsed_seconds", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    const base = gymView();
    engine(() => ({ ...base, summary: { ...base.summary, active_elapsed_seconds: 300 } }));
    renderHome();
    const timer = await screen.findByRole("timer", { name: "Workout duration" });
    expect(timer).toHaveTextContent("5m 00s");
    vi.advanceTimersByTime(2_000);
    await waitFor(() => expect(timer).toHaveTextContent("5m 02s"));
    expect(screen.getByRole("link", { name: /Continue workout/ })).toHaveAttribute("href", "/gym/workout");
    expect(screen.getByText("In progress")).toBeInTheDocument();
  });

  it("builds a routine from the catalog, reordered, and saves it with save_routine", async () => {
    const calls = engine(() => gymView({ active: null }));
    renderHome();
    fireEvent.click(await screen.findByRole("button", { name: "Create routine" }));
    const builder = screen.getByRole("dialog", { name: "New routine" });

    // Validation before anything is sent.
    fireEvent.click(within(builder).getByRole("button", { name: "Save routine" }));
    expect(within(builder).getByRole("alert")).toHaveTextContent("Give the routine a name.");
    fireEvent.input(within(builder).getByLabelText("Routine name"), { target: { value: "Legs & push" } });
    fireEvent.click(within(builder).getByRole("button", { name: "Save routine" }));
    expect(within(builder).getByRole("alert")).toHaveTextContent("Add at least one exercise.");

    for (const name of ["Bench Press", "Back Squat"]) {
      fireEvent.click(within(builder).getByRole("button", { name: "Add exercise" }));
      const picker = screen.getByRole("dialog", { name: "Exercises" });
      fireEvent.click(within(picker).getByRole("button", { name: new RegExp(name) }));
      await waitFor(() => expect(screen.queryByRole("dialog", { name: "Exercises" })).toBeNull());
    }
    const list = within(builder).getByRole("list", { name: "Exercises in order" });
    expect(within(list).getAllByRole("listitem").map((li) => li.querySelector(".routine-item-name")?.textContent)).toEqual([
      "Bench Press",
      "Back Squat",
    ]);
    expect(within(list).getByRole("button", { name: "Move Bench Press up" })).toBeDisabled();
    fireEvent.click(within(list).getByRole("button", { name: "Move Back Squat up" }));
    expect(posts(calls)).toHaveLength(0);

    fireEvent.click(within(builder).getByRole("button", { name: "Save routine" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({
      type: "save_routine",
      name: "Legs & push",
      exercise_ids: ["squat", "bench-press"],
    });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New routine" })).toBeNull());
  });

  it("removes an exercise from the routine being built", async () => {
    engine(() => gymView({ active: null }));
    renderHome();
    fireEvent.click(await screen.findByRole("button", { name: "Create routine" }));
    const builder = screen.getByRole("dialog", { name: "New routine" });
    fireEvent.click(within(builder).getByRole("button", { name: "Add exercise" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "Exercises" })).getByRole("button", { name: /Plank/ }));
    fireEvent.click(await within(builder).findByRole("button", { name: "Remove Plank" }));
    expect(within(builder).getByText("No exercises yet.")).toBeInTheDocument();
  });

  it("opens Settings as its own Gym section", async () => {
    engine(() => gymView({ active: null }));
    renderHome("/gym?tab=settings");
    expect(await screen.findByRole("heading", { name: "Settings", level: 1 })).toBeInTheDocument();
    expect(screen.queryByText("Make time")).toBeNull();
    const nav = screen.getByRole("navigation", { name: "Gym sections" });
    expect(within(nav).getByRole("link", { name: "Settings" })).toHaveAttribute("aria-current", "page");
    expect(within(nav).getByRole("link", { name: "Training" })).not.toHaveAttribute("aria-current");
  });
});

describe("Gym translations", () => {
  beforeEach(() => resetGym());

  it("follows the chosen language (Italian and Danish)", async () => {
    engine(() => gymView({ active: null }));
    setLocale("it");
    renderHome();
    expect(await screen.findByRole("button", { name: /Inizia allenamento libero/ })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Diario" })).toBeInTheDocument();
    setLocale("da");
    expect(await screen.findByRole("button", { name: /Start tom træning/ })).toBeInTheDocument();
    expect(screen.getByText("Lidt stærkere, én træning ad gangen.")).toBeInTheDocument();
  });
});

describe("Gym sub-navigation", () => {
  beforeEach(() => resetGym());

  it("navigates in the app when a section is clicked", async () => {
    engine(() => gymView({ active: null }));
    renderHome();
    const nav = await screen.findByRole("navigation", { name: "Gym sections" });
    fireEvent.click(within(nav).getByRole("link", { name: "Journal" }));
    expect(await screen.findByText("Elsewhere")).toBeInTheDocument();
  });
});

describe("helpers", () => {
  it("moves list items and clamps the target", () => {
    expect(moveItem(["a", "b", "c"], 2, 0)).toEqual(["c", "a", "b"]);
    expect(moveItem(["a", "b", "c"], 0, 9)).toEqual(["b", "c", "a"]);
    expect(moveItem(["a"], 3, 0)).toEqual(["a"]);
  });

  it("adds local ticking to the server's elapsed seconds", () => {
    expect(elapsedNow({ active_elapsed_seconds: 60, receivedAt: 1_000 }, 4_500)).toBe(63);
    expect(elapsedNow({ active_elapsed_seconds: 60, receivedAt: 5_000 }, 4_000)).toBe(60);
    expect(elapsedNow({ receivedAt: 1_000 }, 9_000)).toBeNull();
    expect(elapsedNow(null, 9_000)).toBeNull();
  });
});
