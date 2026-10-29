import { describe, expect, it } from "vitest";
import { note } from "../test/keepFixtures";
import { byPosition, emptySearch, matchesType, positionBetween, searchActive, searchNotes, sortLabels, splitPinned, viewNotes } from "./keepFilter";

const labelName = (tag: string) => ({ godlevel: "GodLevel", "fisica 2": "Fisica 2", chill: "Chill" })[tag] ?? tag;

const notes = [
  note({ id: "a", title: "Active recall", content: "Test yourself before rereading", tags: ["godlevel"], position: 5 }),
  note({ id: "b", title: "Groceries", items: [{ id: "i1", text: "Oats", checked: false, indent: 0 }, { id: "i2", text: "Lemons", checked: true, indent: 0 }], color: "green", position: 9, pinned: true }),
  note({ id: "c", title: "Photo", attachments: [{ id: "x", file: "x.png", mimetype: "image/png", size: 1, createdAt: "" }], position: 7 }),
  note({ id: "d", title: "Dentist", reminder: { at: "2026-11-02T09:00:00.000Z" }, position: 3 }),
  note({ id: "e", title: "Old", archived: true, content: "archived words", position: 1 }),
  note({ id: "f", title: "Gone", deletedAt: "2026-10-26T10:00:00.000Z", content: "trashed words", position: 8 }),
  note({ id: "g", title: "Later", reminder: { at: "2026-10-30T09:00:00.000Z" }, position: 2 }),
  note({ id: "h", title: "Read", annotations: [{ url: "https://example.org/sleep", title: "Sleep and memory" }], position: 4 }),
  note({ id: "i", title: "", content: "see https://example.com/x", position: 6 }),
  note({ id: "j", title: "Voice", attachments: [{ id: "y", file: "y.m4a", mimetype: "audio/mp4", size: 1, createdAt: "" }], position: 0.5 }),
];
const ids = (l: { id: string }[]) => l.map((n) => n.id);

describe("views", () => {
  it("Notes: live, not archived, by position", () => {
    expect(ids(viewNotes(notes, { kind: "notes" }))).toEqual(["b", "c", "i", "a", "h", "d", "g", "j"]);
  });
  it("Archive and Trash", () => {
    expect(ids(viewNotes(notes, { kind: "archive" }))).toEqual(["e"]);
    expect(ids(viewNotes(notes, { kind: "trash" }))).toEqual(["f"]);
  });
  it("Reminders: soonest first", () => {
    expect(ids(viewNotes(notes, { kind: "reminders" }))).toEqual(["g", "d"]);
  });
  it("a label shows its live notes", () => {
    expect(ids(viewNotes(notes, { kind: "label", tag: "godlevel" }))).toEqual(["a"]);
  });
  it("orders ties by creation, newest first", () => {
    const x = note({ id: "x", position: 5, createdAt: "2026-10-01T00:00:00.000Z" });
    const y = note({ id: "y", position: 5, createdAt: "2026-10-02T00:00:00.000Z" });
    expect([x, y].sort(byPosition).map((n) => n.id)).toEqual(["y", "x"]);
  });
  it("splits Pinned from Others", () => {
    const s = splitPinned(viewNotes(notes, { kind: "notes" }));
    expect(ids(s.pinned)).toEqual(["b"]);
    expect(s.others).toHaveLength(7);
  });
});

describe("search", () => {
  const find = (q: string, extra = {}) => ids(searchNotes(notes, { ...emptySearch(), q, ...extra }, labelName));

  it("looks in the title, the text, list items, label names and link titles", () => {
    expect(find("recall")).toEqual(["a"]);
    expect(find("rereading")).toEqual(["a"]);
    expect(find("lemons")).toEqual(["b"]);
    expect(find("godlevel")).toEqual(["a"]);
    expect(find("memory")).toEqual(["h"]);
    expect(find("example.org")).toEqual(["h"]);
  });

  it("needs every word, ignores case and accents", () => {
    expect(find("TEST reread")).toEqual(["a"]);
    expect(find("test lemons")).toEqual([]);
    expect(find("décalL")).toEqual([]);
    expect(find("ACTIVE")).toEqual(["a"]);
  });

  it("includes archived notes but never the trash", () => {
    expect(find("archived")).toEqual(["e"]);
    expect(find("trashed")).toEqual([]);
  });

  it("filters by type", () => {
    const t = (type: never) => ids(searchNotes(notes, { ...emptySearch(), type }, labelName));
    expect(t("lists" as never)).toEqual(["b"]);
    expect(t("images" as never)).toEqual(["c"]);
    expect(t("audio" as never)).toEqual(["j"]);
    expect(t("reminders" as never)).toEqual(["d", "g"]);
    expect(t("urls" as never).sort()).toEqual(["h", "i"]);
  });

  it("filters by label and colour, and combines with text", () => {
    expect(find("", { label: "godlevel" })).toEqual(["a"]);
    expect(find("", { color: "green" })).toEqual(["b"]);
    expect(find("recall", { color: "green" })).toEqual([]);
  });

  it("knows when a search is active", () => {
    expect(searchActive(emptySearch())).toBe(false);
    expect(searchActive({ q: "  " })).toBe(false);
    expect(searchActive({ q: "", color: "red" })).toBe(true);
    expect(matchesType(notes[2]!, "images")).toBe(true);
  });
});

describe("dragging cards", () => {
  it("puts a card between its neighbours", () => {
    expect(positionBetween(10, 4)).toBe(7);
    expect(positionBetween(undefined, 4)).toBe(1004);
    expect(positionBetween(10, undefined)).toBe(-990);
  });
});

describe("labels", () => {
  it("sorts alphabetically in display case, like Keep's nav", () => {
    expect(sortLabels([{ name: "health tips" }, { name: "Chill" }, { name: "EUNE" }, { name: "film" }]).map((l) => l.name)).toEqual(["Chill", "EUNE", "film", "health tips"]);
  });
});
