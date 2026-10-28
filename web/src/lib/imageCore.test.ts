/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { imageCoreFromBytes } from "./imageCore";

// The committed image-core.wasm (web/scripts/build-image-core.sh) through the ABI the viewer
// uses: the same answers as the iPhone's secretary_image_view.
const bytes = readFileSync(join(process.cwd(), "src/lib/image-core.wasm"));

describe("photo viewer core (wasm)", () => {
  it("fits, zooms on a double tap and keeps the pinch anchor still", async () => {
    const core = await imageCoreFromBytes(bytes);
    const fitted = core.view({ new: [1440, 960, 390, 844] });
    expect(fitted.ok).toBe(true);
    expect(fitted.rect).toEqual({ x: 0, y: (844 - 260) / 2, w: 390, h: 260 });
    const zoomed = core.view({ view: fitted.view, gesture: { op: "double_tap", x: 195, y: 422 } });
    expect(zoomed.view?.scale).toBe(2.5);

    const square = core.view({ new: [1000, 1000, 400, 400] });
    const before = square.rect!;
    const px = (300 - before.x) / before.w;
    const pinched = core.view({ view: square.view, gesture: { op: "pinch", factor: 2, x: 300, y: 100 } }).rect!;
    expect(pinched.x + px * pinched.w).toBeCloseTo(300, 1);
    // Fitted pictures don't pan; zoomed ones stop at their edges.
    expect(core.view({ view: square.view, gesture: { op: "pan", dx: 50, dy: 0 } }).rect).toEqual(before);
  });

  it("splits drags like the phone and closes on a swipe down", async () => {
    const core = await imageCoreFromBytes(bytes);
    const view = core.view({ new: [1440, 960, 390, 844] }).view;
    expect(core.view({ view, drag: { dx: 30, dy: 10 } }).drag?.axis).toBe("pager");
    const half = core.view({ view, drag: { dx: 30, dy: 211 } }).drag!;
    expect(half.axis).toBe("out");
    expect(half.out.scale).toBeCloseTo(0.875, 3);
    expect(half.out.backdrop).toBeCloseTo(0.5, 3);
    expect(core.view({ view, drag: { dx: 0, dy: 120, predicted_dy: 130, ended: true } }).drag?.end).toBe("dismiss");
    expect(core.view({ view, drag: { dx: 0, dy: 60, predicted_dy: 70, ended: true } }).drag?.end).toBe("back");
  });

  it("accepts diagonal drags, velocity, and an unloaded picture", async () => {
    const core = await imageCoreFromBytes(bytes);
    const view = core.view({ new: [1440, 960, 390, 844] }).view;
    for (const degrees of [0, 35, 45]) {
      expect(core.view({ view, drag: { dx: Math.tan(degrees * Math.PI / 180) * 100, dy: 100, vx: 0, vy: 100 } }).drag?.axis).toBe("out");
    }
    expect(core.view({ view, drag: { dx: 200, dy: 20, vx: 900, vy: 50 } }).drag?.axis).toBe("pager");
    expect(core.view({ view, drag: { dx: 0, dy: 24, vy: 900, ended: true } }).drag?.end).toBe("dismiss");
    expect(core.view({ view, drag: { dx: 0, dy: 60, vy: 40, ended: true } }).drag?.end).toBe("back");
    expect(core.view({ new: [1, 1, 390, 844], drag: { dx: 20, dy: 100 } }).drag?.axis).toBe("out");
  });

  it("answers bad requests without breaking, and keeps working after many calls", async () => {
    const core = await imageCoreFromBytes(bytes);
    expect(core.view({}).ok).toBe(false);
    let view = core.view({ new: [4000, 3000, 1280, 800] }).view;
    for (let i = 0; i < 5000; i++) view = core.view({ view, gesture: { op: "pinch", factor: i % 2 ? 1.1 : 1 / 1.1, x: 640, y: 400 } }).view;
    expect(view!.scale).toBeGreaterThanOrEqual(1);
  });
});
