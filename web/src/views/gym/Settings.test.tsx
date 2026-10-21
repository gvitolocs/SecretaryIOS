import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GymCommand, GymView } from "../../api/gym";
import { gym, resetGym } from "../../state/gym";
import { markLucinaOnline } from "../../state/lucina";
import { stubFetch, type Call, type Handler } from "../../test/fetch";
import { renderAt } from "../../test/router";
import { gymView } from "./fixture";
import GymSettingsView, { backupFileName } from "./Settings";

type Envelope = { op_id: string; expected_revision: number; command: GymCommand };
const posts = (calls: Call[]) => calls.filter((c) => c.method === "POST").map((c) => c.body as Envelope);

function renderSettings(view: GymView, onPost?: Handler) {
  const calls = stubFetch((call) => {
    if (call.method === "GET") return { body: { ok: true, view } };
    if (onPost) return onPost(call);
    return { body: { ok: true, view: { ...view, state: { ...view.state, revision: view.state.revision + 1 } } } };
  });
  renderAt("/gym/settings", () => <Route path="/gym/settings" component={GymSettingsView} />);
  return calls;
}

function fileInput(): HTMLInputElement {
  return screen.getByLabelText("Backup file") as HTMLInputElement;
}

function chooseFile(file: File) {
  fireEvent.change(fileInput(), { target: { files: [file] } });
}

describe("Gym settings", () => {
  beforeEach(() => {
    resetGym();
    markLucinaOnline();
  });
  afterEach(() => vi.restoreAllMocks());

  it("offers the original rest presets as a segmented choice and saves them", async () => {
    const calls = renderSettings(gymView({ active: null }));
    const group = await screen.findByRole("group", { name: "Rest timer" });
    const radios = Array.from(group.querySelectorAll<HTMLInputElement>("input[type=radio]"));
    expect(radios.map((r) => r.value)).toEqual(["30", "45", "60", "90", "120", "180", "300"]);
    expect(screen.getByRole("radio", { name: "90 s" })).toBeChecked();
    expect(screen.queryByRole("textbox", { name: /rest/i })).toBeNull();

    fireEvent.click(screen.getByRole("radio", { name: "60 s" }));
    fireEvent.click(screen.getByRole("radio", { name: "Pounds" }));
    fireEvent.click(screen.getByRole("button", { name: "More workouts per week" }));
    expect(screen.getByText("Weekly goal: 4 workouts")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save preferences" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "update_settings", weight_unit: "lb", rest_seconds: 60, weekly_goal: 4 });
    expect(await screen.findByText("Preferences saved.")).toBeInTheDocument();
  });

  it("keeps a rest time that is not a preset selectable", async () => {
    const view = gymView({ active: null });
    renderSettings({ ...view, state: { ...view.state, settings: { ...view.state.settings, rest_seconds: 75 } } });
    expect(await screen.findByRole("radio", { name: "75 s" })).toBeChecked();
  });

  it("exports JSON and CSV as Lucina-Gym-YYYY-MM-DD files", async () => {
    const created: Blob[] = [];
    // jsdom has no object URLs; record what would be saved instead.
    URL.createObjectURL = vi.fn((b: Blob) => {
      created.push(b);
      return "blob:gym";
    });
    URL.revokeObjectURL = vi.fn();
    const names: string[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      names.push(this.download);
    });
    const view = gymView({ active: null });
    const calls = renderSettings(view, (call) => {
      const type = (call.body as Envelope).command.type;
      return { body: { ok: true, view, text: type === "export_json" ? '{"schema_version":1}' : "workout_name\n" } };
    });

    fireEvent.click(await screen.findByRole("button", { name: "Export JSON backup" }));
    await waitFor(() => expect(names).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "Export workout CSV" }));
    await waitFor(() => expect(names).toHaveLength(2));

    expect(posts(calls).map((p) => p.command.type)).toEqual(["export_json", "export_csv"]);
    const today = new Date();
    expect(names).toEqual([backupFileName("json", today), backupFileName("csv", today)]);
    expect(names[0]).toMatch(/^Lucina-Gym-\d{4}-\d{2}-\d{2}\.json$/);
    expect(await created[0]?.text()).toBe('{"schema_version":1}');
    expect(created[1]?.type).toBe("text/csv");
    expect(screen.getByRole("status")).toHaveTextContent(`Exported ${names[1]}.`);
  });

  it("restores a JSON backup only after confirming", async () => {
    const calls = renderSettings(gymView({ active: null }));
    await screen.findByRole("button", { name: "Restore JSON backup" });
    const backup = '{"schema_version":1,"settings":{}}';
    chooseFile(new File([backup], "old.json", { type: "application/json" }));

    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Replace your current gym data with this backup?");
    expect(posts(calls)).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Restore backup" }));
    await waitFor(() => expect(posts(calls)).toHaveLength(1));
    expect(posts(calls)[0]?.command).toEqual({ type: "restore_json", backup });
    expect(await screen.findByText("Backup restored from old.json.")).toBeInTheDocument();
  });

  it("cancels a restore without sending anything", async () => {
    const calls = renderSettings(gymView({ active: null }));
    await screen.findByRole("button", { name: "Restore JSON backup" });
    chooseFile(new File(["{}"], "b.json"));
    await screen.findByRole("alertdialog");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(posts(calls)).toHaveLength(0);
  });

  it("refuses a backup over the engine's 16 MiB cap before reading it", async () => {
    const calls = renderSettings(gymView({ active: null }));
    await screen.findByRole("button", { name: "Restore JSON backup" });
    const big = new File(["{}"], "huge.json");
    Object.defineProperty(big, "size", { value: 16 * 1024 * 1024 + 1 });
    chooseFile(big);
    expect(await screen.findByRole("alert")).toHaveTextContent("This backup exceeds the 16 MB import limit.");
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(posts(calls)).toHaveLength(0);
  });

  it("rejects a file that is not JSON", async () => {
    renderSettings(gymView({ active: null }));
    await screen.findByRole("button", { name: "Restore JSON backup" });
    chooseFile(new File(["workout_name,reps"], "w.csv"));
    expect(await screen.findByRole("alert")).toHaveTextContent("That file is not a Lucina Gym JSON backup.");
  });

  it("shows the engine's reason when a restore is rejected, once", async () => {
    renderSettings(gymView({ active: null }), () => ({
      status: 422,
      body: { ok: false, code: "invalid_command", error: "Invalid backup: missing field `settings`" },
    }));
    await screen.findByRole("button", { name: "Restore JSON backup" });
    chooseFile(new File(["{}"], "b.json"));
    fireEvent.click(await screen.findByRole("button", { name: "Restore backup" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    await waitFor(() => expect(gym.actionError).toBeNull());
    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(1);
    expect(alerts[0]).toHaveTextContent("Invalid backup: missing field `settings`");
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("shows the original About text", async () => {
    renderSettings(gymView({ active: null }));
    expect(
      await screen.findByText("Your training, on your device. No account required. Export a backup to keep a copy of your workouts."),
    ).toBeInTheDocument();
  });

  it("names backups by the local date", () => {
    expect(backupFileName("json", new Date(2026, 9, 1, 23, 30))).toBe("Lucina-Gym-2026-10-01.json");
    expect(backupFileName("csv", new Date(2026, 0, 9))).toBe("Lucina-Gym-2026-01-09.csv");
  });
});
