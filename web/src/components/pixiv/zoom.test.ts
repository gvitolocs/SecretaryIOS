import { describe, expect, it } from "vitest";
import { clampPan, clampZoom, fitScale, isTall, MAX_ZOOM, pannable, stepZoom, topPan, wheelFactor, widthScale, zoomAround, zoomLabel } from "./zoom";

const WIDE = { w: 2400, h: 1200 };
const TALL = { w: 1200, h: 1700 };
const SQUARE = { w: 1200, h: 1200 };
const MANGA = { w: 800, h: 6000 };
const SMALL = { w: 400, h: 300 };
const VIEWS = { mac: { w: 1400, h: 760 }, laptop: { w: 1100, h: 640 }, phone: { w: 390, h: 640 }, fullscreen: { w: 1920, h: 1080 } };

describe("fitScale", () => {
  it("shows the whole image inside every stage", () => {
    for (const img of [WIDE, TALL, SQUARE, MANGA]) {
      for (const view of Object.values(VIEWS)) {
        const s = fitScale(img, view);
        expect(img.w * s).toBeLessThanOrEqual(view.w + 1e-9);
        expect(img.h * s).toBeLessThanOrEqual(view.h + 1e-9);
        // and touches the stage on one axis (letterboxed on the other)
        expect(Math.max((img.w * s) / view.w, (img.h * s) / view.h)).toBeCloseTo(1, 9);
      }
    }
  });

  it("matches the hand-worked cases", () => {
    expect(fitScale(TALL, VIEWS.mac)).toBeCloseTo(760 / 1700, 9); // height-bound: the Ronova page
    expect(fitScale(WIDE, VIEWS.mac)).toBeCloseTo(1400 / 2400, 9); // width-bound
    expect(fitScale(SQUARE, VIEWS.phone)).toBeCloseTo(390 / 1200, 9);
    expect(fitScale(SQUARE, VIEWS.fullscreen)).toBeCloseTo(1080 / 1200, 9);
  });

  it("never enlarges a small image past its pixels, and is 1 without sizes", () => {
    expect(fitScale(SMALL, VIEWS.fullscreen)).toBe(1);
    expect(fitScale(null, VIEWS.mac)).toBe(1);
    expect(fitScale(TALL, { w: 0, h: 0 })).toBe(1);
  });
});

describe("fit width", () => {
  it("fills the width and is offered only for tall pages", () => {
    expect(widthScale(MANGA, VIEWS.mac)).toBe(1); // 800 px wide fits as it is
    expect(widthScale(TALL, VIEWS.phone)).toBeCloseTo(390 / 1200, 9);
    expect(isTall(MANGA, VIEWS.mac)).toBe(true);
    expect(isTall(TALL, VIEWS.phone)).toBe(false); // at the phone's width it is already Fit
    expect(isTall(TALL, VIEWS.mac)).toBe(true);
    expect(isTall(WIDE, VIEWS.mac)).toBe(false);
    expect(isTall(SQUARE, VIEWS.mac)).toBe(false);
  });

  it("starts at the top of the page", () => {
    const s = widthScale(MANGA, VIEWS.mac);
    expect(topPan(MANGA, VIEWS.mac, s)).toEqual({ x: 0, y: (6000 - 760) / 2 });
  });
});

describe("pan", () => {
  it("does not move an image that fits, and stops at the edges when zoomed", () => {
    const fit = fitScale(TALL, VIEWS.mac);
    expect(pannable(TALL, VIEWS.mac, fit)).toBe(false);
    expect(clampPan({ x: 500, y: -500 }, TALL, VIEWS.mac, fit)).toEqual({ x: 0, y: 0 });
    expect(pannable(TALL, VIEWS.mac, 1)).toBe(true);
    // at 100 %: 1200x1700 in 1400x760 -> no horizontal room, (1700-760)/2 vertically
    expect(clampPan({ x: 500, y: -5000 }, TALL, VIEWS.mac, 1)).toEqual({ x: 0, y: -470 });
  });

  it("keeps the point under the cursor in place", () => {
    const at = { x: 200, y: -100 };
    const pan = zoomAround({ x: 0, y: 0 }, at, 0.5, 1);
    // the image point under `at` was (at - pan) / 0.5 from the image centre; it stays under `at`
    expect((at.x - pan.x) / 1).toBeCloseTo((at.x - 0) / 0.5, 9);
    expect((at.y - pan.y) / 1).toBeCloseTo((at.y - 0) / 0.5, 9);
  });
});

describe("zoom steps", () => {
  it("goes from Fit to 400 % and back to Fit", () => {
    const fit = fitScale(TALL, VIEWS.mac);
    let s = fit;
    for (let i = 0; i < 30; i++) s = stepZoom(s, true, fit);
    expect(s).toBe(MAX_ZOOM);
    for (let i = 0; i < 30; i++) s = stepZoom(s, false, fit);
    expect(s).toBe(fit);
    expect(clampZoom(fit * 1.005, fit)).toBe(fit);
    expect(clampZoom(0.01, fit)).toBe(fit);
  });

  it("turns wheel deltas into gentle factors", () => {
    expect(wheelFactor(-100)).toBeGreaterThan(1);
    expect(wheelFactor(100)).toBeLessThan(1);
    expect(wheelFactor(-100)).toBeCloseTo(wheelFactor(-50), 9);
    expect(wheelFactor(-2)).toBeLessThan(1.02);
  });

  it("labels Fit or the percentage of the image's pixels", () => {
    expect(zoomLabel(true, 0.447, "Fit")).toBe("Fit");
    expect(zoomLabel(false, 1, "Fit")).toBe("100%");
    expect(zoomLabel(false, 0.559, "Fit")).toBe("56%");
  });
});
