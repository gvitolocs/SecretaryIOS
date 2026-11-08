import { describe, expect, it } from "vitest";
import { keepHref, noteHref, parseKeepPath, sameView } from "./keepRoute";

describe("Keep's URLs", () => {
  it("reads each view", () => {
    expect(parseKeepPath("/keep")).toEqual({ view: { kind: "notes" } });
    expect(parseKeepPath("/keep/")).toEqual({ view: { kind: "notes" } });
    expect(parseKeepPath("/keep/reminders").view).toEqual({ kind: "reminders" });
    expect(parseKeepPath("/keep/archive").view).toEqual({ kind: "archive" });
    expect(parseKeepPath("/keep/trash").view).toEqual({ kind: "trash" });
  });

  it("reads a label by its display name, normalised", () => {
    expect(parseKeepPath("/keep/label/Fisica%202").view).toEqual({ kind: "label", tag: "fisica 2" });
    expect(parseKeepPath("/keep/label/GodLevel").view).toEqual({ kind: "label", tag: "godlevel" });
  });

  it("opens the note dialog from /keep/note/<id> over Notes", () => {
    expect(parseKeepPath("/keep/note/n_abc")).toEqual({ view: { kind: "notes" }, noteId: "n_abc" });
  });

  it("opens the dialog from ?note= over any other view", () => {
    expect(parseKeepPath("/keep/archive", "?note=n_9")).toEqual({ view: { kind: "archive" }, noteId: "n_9" });
    expect(parseKeepPath("/keep/label/Chill", "?note=n_9")).toEqual({ view: { kind: "label", tag: "chill" }, noteId: "n_9" });
  });

  it("builds the paths back", () => {
    expect(keepHref({ kind: "notes" })).toBe("/keep");
    expect(keepHref({ kind: "label", tag: "fisica 2" }, "Fisica 2")).toBe("/keep/label/Fisica%202");
    expect(noteHref("n_1", { kind: "notes" })).toBe("/keep/note/n_1");
    expect(noteHref("n_1", { kind: "trash" })).toBe("/keep/trash?note=n_1");
    expect(sameView({ kind: "label", tag: "a" }, { kind: "label", tag: "a" })).toBe(true);
    expect(sameView({ kind: "label", tag: "a" }, { kind: "label", tag: "b" })).toBe(false);
  });
});
