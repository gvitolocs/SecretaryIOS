import { describe, expect, it } from "vitest";
import {
  emptyEdit,
  frameSize,
  inscribedScale,
  isIdentity,
  parseEditList,
  quartersOf,
  rotateCrop,
  rotateEdit,
  sameEdit,
  setAdjust,
  setFilter,
  setStraighten,
  tidy,
  type EditList,
} from "./editList";

describe("the edit list", () => {
  it("starts neutral, and anything that changes the picture is not the identity", () => {
    const e = emptyEdit();
    expect(isIdentity(e)).toBe(true);
    expect(isIdentity({ ...e, crop: { x: 0, y: 0, w: 1, h: 1 } })).toBe(true);
    expect(isIdentity({ ...e, filter: { name: "bw", strength: 0 } })).toBe(true);
    expect(isIdentity({ ...e, straighten: 0.0004 })).toBe(true);
    for (const changed of [
      { ...e, rotate: 1 as const },
      { ...e, straighten: 0.5 },
      { ...e, crop: { x: 0.1, y: 0, w: 0.9, h: 1 } },
      setAdjust(e, "exposure", 0.05),
      setFilter(e, "vivid"),
      { ...e, markup: [{ t: "text" as const, color: "#fff", size: 0.05, x: 0, y: 0, text: "x" }] },
    ]) {
      expect(isIdentity(changed)).toBe(false);
    }
  });

  it("sets one adjustment without touching the rest, and a filter by name", () => {
    const e = setAdjust(setAdjust(emptyEdit(), "contrast", 20), "warmth", -10);
    expect(e.adjust).toMatchObject({ contrast: 20, warmth: -10, exposure: 0 });
    expect(setFilter(e, "noir", 0.5).filter).toEqual({ name: "noir", strength: 0.5 });
    expect(setFilter(e, null).filter).toBeNull();
    expect(setStraighten(e, 80).straighten).toBe(45);
    expect(setStraighten(e, -80).straighten).toBe(-45);
  });

  it("turns the crop box with the picture, and four turns come back", () => {
    const crop = { x: 0.1, y: 0.2, w: 0.5, h: 0.3 };
    // Clockwise: the box that sat in the top left goes to the top right; width and height swap.
    expect(rotateCrop({ x: 0, y: 0, w: 0.5, h: 0.25 }, 1)).toEqual({ x: 0.75, y: 0, w: 0.25, h: 0.5 });
    expect(rotateCrop({ x: 0, y: 0, w: 0.5, h: 0.25 }, -1)).toEqual({ x: 0, y: 0.5, w: 0.25, h: 0.5 });
    let e: EditList = { ...emptyEdit(), crop };
    for (let i = 0; i < 4; i++) e = rotateEdit(e, 1);
    expect(e.rotate).toBe(0);
    expect(e.crop!.x).toBeCloseTo(crop.x);
    expect(e.crop!.y).toBeCloseTo(crop.y);
    expect(e.crop!.w).toBeCloseTo(crop.w);
    expect(e.crop!.h).toBeCloseTo(crop.h);
    expect(rotateEdit(emptyEdit(), -1).rotate).toBe(3);
    expect(rotateEdit(rotateEdit(emptyEdit(), 1), -1).rotate).toBe(0);
    // Left then right is nothing.
    const back = rotateEdit(rotateEdit({ ...emptyEdit(), crop }, -1), 1);
    expect(back.crop!.x).toBeCloseTo(crop.x);
    expect(back.crop!.h).toBeCloseTo(crop.h);
  });

  it("knows the frame the crop box lives in, like the Rust core", () => {
    expect(frameSize(4000, 3000, 0, 0)).toEqual({ width: 4000, height: 3000 });
    expect(frameSize(4000, 3000, 1, 0)).toEqual({ width: 3000, height: 4000 });
    // At 45 degrees a square keeps 1 / (cos + sin) of its side (edit.rs: 707 of 1000).
    expect(frameSize(1000, 1000, 0, 45)).toEqual({ width: 707, height: 707 });
    const f = frameSize(3000, 2000, 0, 12);
    expect(f.width / f.height).toBeCloseTo(1.5, 2);
    expect(f.width).toBeLessThan(3000);
    expect(inscribedScale(3000, 2000, 0)).toBe(1);
    expect(frameSize(3000, 2000, 0, -12)).toEqual(f);
  });

  it("reads a stored list, and drops what it can't use", () => {
    expect(parseEditList(null)).toBeNull();
    expect(parseEditList({ v: 2 })).toBeNull();
    expect(parseEditList("x")).toBeNull();
    const e = parseEditList({
      v: 1,
      rotate: 3,
      straighten: -2.5,
      crop: { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
      adjust: { exposure: 0.5, contrast: "lots" },
      filter: { name: "vintage", strength: 0.4 },
      markup: [
        { t: "pen", color: "#ff0000", width: 0.01, points: [[0.1, 0.1], [0.2, "x"], [0.3, 0.3]] },
        { t: "arrow", color: "#fff", width: 0.01, from: [0, 0], to: [1, 1] },
        { t: "arrow", color: "#fff", from: [0, 0] },
        { t: "text", color: "#fff", size: 0.05, x: 0.1, y: 0.2, text: "hi" },
        { t: "star" },
        { t: "pen", points: [] },
      ],
    })!;
    expect(e.rotate).toBe(3);
    expect(e.adjust).toMatchObject({ exposure: 0.5, contrast: 0, fade: 0 });
    expect(e.filter).toEqual({ name: "vintage", strength: 0.4 });
    expect(e.markup.map((m) => m.t)).toEqual(["pen", "arrow", "text"]);
    expect(e.markup[0]).toMatchObject({ points: [[0.1, 0.1], [0.3, 0.3]] });
    expect(parseEditList({ v: 1, rotate: 9, filter: { name: "nope" } })).toMatchObject({ rotate: 0, filter: null, crop: null });
    // What the editor writes is what it reads.
    expect(sameEdit(parseEditList(JSON.parse(JSON.stringify(e)))!, e)).toBe(true);
  });

  it("tidies slider noise and a crop that is the whole frame", () => {
    const e = tidy({ ...setAdjust(emptyEdit(), "exposure", 0.3000000001), straighten: 1.234567, crop: { x: 0, y: 0, w: 1, h: 1 } });
    expect(e.adjust.exposure).toBe(0.3);
    expect(e.straighten).toBe(1.23);
    expect(e.crop).toBeNull();
    expect(tidy({ ...emptyEdit(), crop: { x: 0.123456, y: 0.2, w: 0.5, h: 0.5 } }).crop!.x).toBe(0.1235);
  });

  it("turns the viewer's rotation into quarter turns", () => {
    expect([0, 90, 180, 270, 360, -90].map(quartersOf)).toEqual([0, 1, 2, 3, 0, 3]);
  });
});
