import { describe, expect, it } from "vitest";
import { ASPECTS, aspectRatio, fitAspect, fitBox, hitHandle, moveCrop, MIN_SIDE, resizeCrop } from "./cropMath";

const inside = (c: { x: number; y: number; w: number; h: number }) => c.x >= -1e-9 && c.y >= -1e-9 && c.x + c.w <= 1 + 1e-9 && c.y + c.h <= 1 + 1e-9 && c.w >= MIN_SIDE - 1e-9 && c.h >= MIN_SIDE - 1e-9;

describe("crop box geometry", () => {
  it("moves the box but keeps it in the frame", () => {
    const c = { x: 0.2, y: 0.2, w: 0.4, h: 0.4 };
    expect(moveCrop(c, 0.1, -0.1)).toEqual({ x: 0.30000000000000004, y: 0.1, w: 0.4, h: 0.4 });
    expect(moveCrop(c, 5, 5)).toEqual({ x: 0.6, y: 0.6, w: 0.4, h: 0.4 });
    expect(moveCrop(c, -5, -5)).toEqual({ x: 0, y: 0, w: 0.4, h: 0.4 });
  });

  it("resizes freely from a side or a corner with the opposite side fixed", () => {
    const c = { x: 0.2, y: 0.2, w: 0.4, h: 0.4 };
    const east = resizeCrop(c, "e", 0.1, 0.3, null, 1.5);
    expect(east).toMatchObject({ x: 0.2, y: 0.2 });
    expect(east.w).toBeCloseTo(0.5);
    expect(east.h).toBeCloseTo(0.4);
    const nw = resizeCrop(c, "nw", -0.1, -0.05, null, 1.5);
    expect(nw.x).toBeCloseTo(0.1);
    expect(nw.y).toBeCloseTo(0.15);
    expect(nw.x + nw.w).toBeCloseTo(0.6);
    expect(nw.y + nw.h).toBeCloseTo(0.6);
    // Dragged past the frame or past the opposite side: stopped at the frame and at the minimum.
    const far = resizeCrop(c, "se", 5, 5, null, 1.5);
    expect(far.w).toBeCloseTo(0.8);
    expect(far.h).toBeCloseTo(0.8);
    const thin = resizeCrop(c, "w", 5, 0, null, 1.5);
    expect(thin.w).toBeCloseTo(MIN_SIDE);
    expect(thin.x + thin.w).toBeCloseTo(0.6);
  });

  it("keeps the pixel ratio when it is locked", () => {
    // A 3:2 frame; a 1:1 box in pixels is 2/3 as wide as tall in fractions.
    const frameAspect = 1.5;
    const start = { x: 0.2, y: 0.2, w: 0.3, h: 0.45 };
    for (const handle of ["nw", "ne", "se", "sw", "n", "s", "e", "w"] as const) {
      for (const [dx, dy] of [[0.1, 0.05], [-0.15, 0.1], [0.4, -0.3], [-0.6, -0.6], [0.6, 0.6]]) {
        const c = resizeCrop(start, handle, dx!, dy!, 1, frameAspect);
        expect(inside(c), `${handle} ${dx} ${dy}: ${JSON.stringify(c)}`).toBe(true);
        expect((c.w * frameAspect) / c.h, `${handle} ${dx} ${dy}`).toBeCloseTo(1, 6);
      }
    }
    // The opposite corner stays where it was.
    const se = resizeCrop(start, "se", 0.1, 0.1, 1, frameAspect);
    expect(se.x).toBeCloseTo(start.x);
    expect(se.y).toBeCloseTo(start.y);
    const nw = resizeCrop(start, "nw", -0.05, -0.05, 1, frameAspect);
    expect(nw.x + nw.w).toBeCloseTo(start.x + start.w);
    expect(nw.y + nw.h).toBeCloseTo(start.y + start.h);
  });

  it("fits a preset ratio around the centre of the box", () => {
    const wide = fitAspect(null, 1, 1.5);
    expect((wide.w * 1.5) / wide.h).toBeCloseTo(1);
    expect(wide).toMatchObject({ h: 1 });
    expect(wide.x + wide.w / 2).toBeCloseTo(0.5);
    const tall = fitAspect({ x: 0.1, y: 0.1, w: 0.4, h: 0.8 }, 16 / 9, 1.5);
    expect((tall.w * 1.5) / tall.h).toBeCloseTo(16 / 9);
    expect(tall.x + tall.w / 2).toBeCloseTo(0.3);
    expect(tall.y + tall.h / 2).toBeCloseTo(0.5);
    expect(inside(tall)).toBe(true);
    expect(fitAspect({ x: 0.1, y: 0.1, w: 0.4, h: 0.8 }, null, 1.5)).toEqual({ x: 0.1, y: 0.1, w: 0.4, h: 0.8 });
  });

  it("knows the presets", () => {
    expect(aspectRatio("free", 1.5)).toBeNull();
    expect(aspectRatio("original", 1.5)).toBe(1.5);
    expect(aspectRatio("square", 1.5)).toBe(1);
    expect(aspectRatio("16:9", 1.5)).toBeCloseTo(16 / 9);
    expect(ASPECTS.map((a) => a.id)).toContain("9:16");
  });

  it("finds the side, corner or inside a press lands on", () => {
    const box = { x: 100, y: 100, w: 200, h: 100 };
    expect(hitHandle(box, 102, 98, 24)).toBe("nw");
    expect(hitHandle(box, 298, 202, 24)).toBe("se");
    expect(hitHandle(box, 300, 101, 24)).toBe("ne");
    expect(hitHandle(box, 200, 99, 24)).toBe("n");
    expect(hitHandle(box, 200, 201, 24)).toBe("s");
    expect(hitHandle(box, 99, 150, 24)).toBe("w");
    expect(hitHandle(box, 301, 150, 24)).toBe("e");
    expect(hitHandle(box, 200, 150, 24)).toBe("move");
    expect(hitHandle(box, 10, 10, 24)).toBeNull();
    expect(hitHandle(box, 200, 400, 24)).toBeNull();
  });

  it("fits a picture in a box", () => {
    expect(fitBox(800, 600, 4000, 3000)).toEqual({ width: 800, height: 600 });
    expect(fitBox(800, 600, 1000, 2000)).toEqual({ width: 300, height: 600 });
    expect(fitBox(0, 600, 10, 10)).toEqual({ width: 0, height: 0 });
  });
});
