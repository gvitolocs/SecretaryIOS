import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GymCommand } from "../../api/gym";
import { refreshGym, resetGym } from "../../state/gym";
import { stubFetch } from "../../test/fetch";
import { gymView } from "./fixture";
import ExercisePicker from "./Picker";

describe("ExercisePicker", () => {
  beforeEach(() => resetGym());

  it("filters by search and muscle, and adds the catalog exercise", async () => {
    const calls = stubFetch((call) =>
      call.method === "GET"
        ? { body: { ok: true, view: gymView() } }
        : { body: { ok: true, view: gymView({ revision: 6 }) } },
    );
    await refreshGym();
    const onClose = vi.fn();
    render(() => <ExercisePicker open activeExerciseIds={["bench-press", "plank"]} onClose={onClose} />);

    const list = () => within(screen.getByRole("list", { name: "Exercises" }));
    fireEvent.input(screen.getByLabelText("Search exercises"), { target: { value: "press" } });
    expect(list().getAllByRole("button").map((b) => b.querySelector(".picker-item-name")?.textContent)).toEqual([
      "Bench Press",
      "Incline Dumbbell Press",
      "Overhead Press",
    ]);
    expect(list().getByRole("button", { name: /Bench Press/ })).toBeDisabled();
    expect(list().getByRole("button", { name: /Bench Press/ })).toHaveTextContent("Added");

    fireEvent.click(within(screen.getByRole("group", { name: "Muscle" })).getByRole("button", { name: "Shoulders" }));
    expect(list().getAllByRole("button")).toHaveLength(1);

    fireEvent.click(list().getByRole("button", { name: /Overhead Press/ }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const post = calls.find((c) => c.method === "POST")?.body as { command: GymCommand };
    expect(post.command).toEqual({ type: "add_exercise", exercise_id: "overhead-press" });
  });

  it("scrolls the list inside the sheet's body, not the sheet", async () => {
    stubFetch(() => ({ body: { ok: true, view: gymView() } }));
    await refreshGym();
    render(() => <ExercisePicker open activeExerciseIds={[]} onClose={vi.fn()} />);
    const sheet = screen.getByRole("dialog");
    const body = screen.getByRole("list", { name: "Exercises" }).parentElement!;
    expect(body).toHaveClass("picker-body");
    expect(body.parentElement).toBe(sheet);
    expect(within(sheet).getByRole("group", { name: "Equipment" })).toHaveClass("picker-chips--equipment");
  });

  it("closes on Escape", async () => {
    stubFetch(() => ({ body: { ok: true, view: gymView() } }));
    await refreshGym();
    const onClose = vi.fn();
    render(() => <ExercisePicker open activeExerciseIds={[]} onClose={onClose} />);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
