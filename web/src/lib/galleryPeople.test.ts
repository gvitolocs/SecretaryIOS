import { describe, expect, it } from "vitest";
import type { FaceSettings, Person } from "../api/gallery";
import {
  boxToScreen,
  canMerge,
  defaultKeeper,
  isBirthDate,
  normalizeName,
  personName,
  personPath,
  placedFromStyle,
  replacePerson,
  scanProgress,
  searchPeople,
  stillScanning,
  withoutPeople,
} from "./galleryPeople";

const person = (id: string, over: Partial<Person> = {}): Person => ({
  id,
  name: null,
  hidden: false,
  birthDate: null,
  faceCount: 3,
  itemCount: 3,
  coverFaceId: null,
  thumbUrl: null,
  ...over,
});
const settings = (over: Partial<FaceSettings> = {}): FaceSettings => ({
  enabled: true,
  model: "m",
  scanned: 0,
  pending: 0,
  faces: 0,
  people: 0,
  running: false,
  pausedHot: false,
  pausedBusy: false,
  ...over,
});

describe("names and dates", () => {
  it("trims a name, cuts it at 80 characters and turns blank into no name", () => {
    expect(normalizeName("  Alma  ")).toBe("Alma");
    expect(normalizeName("   ")).toBeNull();
    expect(normalizeName("x".repeat(100))).toHaveLength(80);
  });

  it("falls back for a person without a name", () => {
    expect(personName({ name: null }, "Unnamed")).toBe("Unnamed");
    expect(personName({ name: "  " }, "Unnamed")).toBe("Unnamed");
    expect(personName({ name: "Alma" }, "Unnamed")).toBe("Alma");
    expect(personPath("a b")).toBe("/gallery/person/a%20b");
  });

  it("accepts a real day that is not in the future", () => {
    const today = new Date(2026, 9, 28);
    expect(isBirthDate("1990-04-12", today)).toBe(true);
    expect(isBirthDate("2026-10-28", today)).toBe(true);
    expect(isBirthDate("2026-10-29", today)).toBe(false);
    expect(isBirthDate("2026-02-30", today)).toBe(false);
    expect(isBirthDate("12/04/1990", today)).toBe(false);
  });
});

describe("the scan", () => {
  it("says what the scan is doing and how far it has got", () => {
    expect(scanProgress(settings({ scanned: 50, pending: 50, running: true }))).toMatchObject({ phase: "scanning", percent: 50 });
    expect(scanProgress(settings({ scanned: 50, pending: 50, pausedHot: true }))).toMatchObject({ phase: "hot" });
    expect(scanProgress(settings({ scanned: 50, pending: 50, pausedBusy: true }))).toMatchObject({ phase: "busy" });
    expect(scanProgress(settings({ scanned: 100, pending: 0 }))).toMatchObject({ phase: "done", percent: 100 });
    expect(scanProgress(settings({ enabled: false, scanned: 1, pending: 1 })).phase).toBe("off");
    expect(scanProgress(settings())).toMatchObject({ phase: "done", percent: 100 });
    // Never rounds up to done while something waits.
    expect(scanProgress(settings({ scanned: 999, pending: 1 })).percent).toBe(99);
  });

  it("keeps asking while photos wait or the worker runs, and stops when off", () => {
    expect(stillScanning(settings({ pending: 3 }))).toBe(true);
    expect(stillScanning(settings({ running: true }))).toBe(true);
    expect(stillScanning(settings())).toBe(false);
    expect(stillScanning(settings({ enabled: false, pending: 3 }))).toBe(false);
  });
});

describe("lists", () => {
  it("replaces and removes people without touching the rest", () => {
    const list = [person("a"), person("b"), person("c")];
    expect(replacePerson(list, person("b", { name: "Bruno" })).map((p) => p.name)).toEqual([null, "Bruno", null]);
    expect(withoutPeople(list, ["a", "c"]).map((p) => p.id)).toEqual(["b"]);
  });

  it("keeps a named person when merging, else the one with the most faces", () => {
    expect(defaultKeeper([person("a", { faceCount: 30 }), person("b", { name: "Bruno", faceCount: 4 })])).toBe("b");
    expect(defaultKeeper([person("a", { faceCount: 5 }), person("b", { faceCount: 9 })])).toBe("b");
    expect(defaultKeeper([])).toBeNull();
    expect(canMerge(new Set(["a"]))).toBe(false);
    expect(canMerge(new Set(["a", "b"]))).toBe(true);
  });

  it("searches names by words, ignoring case and accents, and offers everyone for an empty search", () => {
    const all = [person("a", { name: "Zoë Larsen" }), person("b", { name: "Bruno" }), person("c")];
    expect(searchPeople(all, "").map((p) => p.id)).toEqual(["a", "b", "c"]);
    expect(searchPeople(all, "zoe lars").map((p) => p.id)).toEqual(["a"]);
    expect(searchPeople(all, "BRU").map((p) => p.id)).toEqual(["b"]);
    expect(searchPeople(all, "nobody")).toEqual([]);
  });
});

describe("where a face box lands on the screen", () => {
  const placed = { x: 100, y: 50, w: 400, h: 300 };
  const box = { x: 0.25, y: 0.5, w: 0.1, h: 0.2 };

  it("scales the fractions into the placed picture at a fit", () => {
    expect(boxToScreen(box, placed, 0)).toEqual({ x: 200, y: 200, w: 40, h: 60 });
  });

  it("follows the zoom and the pan, which are only a different placed box", () => {
    const zoomed = { x: -300, y: -100, w: 1600, h: 1200 };
    expect(boxToScreen(box, zoomed, 0)).toEqual({ x: 100, y: 500, w: 160, h: 240 });
  });

  it("turns a quarter about the picture's centre and keeps the box on the same face", () => {
    // Centre (300, 200); the box centre is at (220, 230), i.e. (-80, 30) from it.
    // 90 clockwise: (dx, dy) -> (-dy, dx) = (-30, -80), so the centre lands at (270, 120); width and height swap.
    const r = boxToScreen(box, placed, 90);
    expect({ cx: r.x + r.w / 2, cy: r.y + r.h / 2, w: r.w, h: r.h }).toEqual({ cx: 270, cy: 120, w: 60, h: 40 });
  });

  it("turns half and three quarters", () => {
    const half = boxToScreen(box, placed, 180);
    expect({ cx: half.x + half.w / 2, cy: half.y + half.h / 2, w: half.w, h: half.h }).toEqual({ cx: 380, cy: 170, w: 40, h: 60 });
    const three = boxToScreen(box, placed, 270);
    expect({ cx: three.x + three.w / 2, cy: three.y + three.h / 2, w: three.w, h: three.h }).toEqual({ cx: 330, cy: 280, w: 60, h: 40 });
  });

  it("keeps the whole picture inside the turned picture's bounds", () => {
    const whole = boxToScreen({ x: 0, y: 0, w: 1, h: 1 }, placed, 90);
    expect(whole).toEqual({ x: 150, y: 0, w: 300, h: 400 });
  });

  it("reads the placed box and the turn back from the style the viewer hands to overlays", () => {
    expect(placedFromStyle({ left: "100px", top: "50px", width: "400px", height: "300px" })).toEqual({ rect: placed, turn: 0 });
    expect(placedFromStyle({ left: "10.5px", top: "-20px", width: "300px", height: "400px", transform: "rotate(90deg)" })).toEqual({
      rect: { x: 10.5, y: -20, w: 300, h: 400 },
      turn: 90,
    });
    expect(placedFromStyle({ left: "1px", top: "1px", width: "3px", height: "3px", transform: "rotate(-90deg)" })?.turn).toBe(270);
    expect(placedFromStyle({ left: "1px", top: "1px", width: "3px", height: "3px", transform: "rotate(180deg)" })?.turn).toBe(180);
    expect(placedFromStyle(undefined)).toBeNull();
    expect(placedFromStyle({ left: "auto", top: "0px", width: "10px", height: "10px" })).toBeNull();
    expect(placedFromStyle({ left: "0px", top: "0px", width: "0px", height: "10px" })).toBeNull();
  });
});
