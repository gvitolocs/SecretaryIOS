/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { emptyEdit, setAdjust, setFilter, type EditList } from "./editList";
import { EditError, imageCoreFromBytes } from "./imageCore";

// The committed image-core.wasm through the editor's ABI: the same Rust render the server runs for the saved derivative.
const bytes = readFileSync(join(process.cwd(), "src/lib/image-core.wasm"));

function solid(w: number, h: number, rgb: [number, number, number]): Uint8ClampedArray {
  const px = new Uint8ClampedArray(w * h * 4);
  for (let i = 0; i < w * h; i++) px.set([rgb[0], rgb[1], rgb[2], 255], i * 4);
  return px;
}

function photo(w: number, h: number): Uint8ClampedArray {
  const px = new Uint8ClampedArray(w * h * 4);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) px.set([(x * 255) / w, (y * 255) / h, (x * 7 + y * 13) % 256, 255], (y * w + x) * 4);
  }
  return px;
}

const srgb = (lin: number) => Math.round(255 * (lin <= 0.0031308 ? lin * 12.92 : 1.055 * lin ** (1 / 2.4) - 0.055));
const lin = (v: number) => (v / 255 <= 0.04045 ? v / 255 / 12.92 : ((v / 255 + 0.055) / 1.055) ** 2.4);

describe("photo editor core (wasm)", () => {
  it("returns the same pixels for the identity, whatever the size", async () => {
    const core = await imageCoreFromBytes(bytes);
    const src = photo(61, 47);
    const s = core.editSource(src, 61, 47);
    const out = s.render(emptyEdit());
    expect([out.width, out.height]).toEqual([61, 47]);
    expect(Array.from(out.rgba)).toEqual(Array.from(src));
    // The JSON form is the same call.
    expect(Array.from(s.render('{"v":1}').rgba)).toEqual(Array.from(src));
    s.free();
  });

  it("+1 EV doubles linear light", async () => {
    const core = await imageCoreFromBytes(bytes);
    const s = core.editSource(solid(8, 8, [60, 60, 60]), 8, 8);
    for (const [ev, factor] of [[1, 2], [-1, 0.5]] as const) {
      const out = s.render(setAdjust(emptyEdit(), "exposure", ev));
      expect(Math.abs(out.rgba[0]! - srgb(lin(60) * factor))).toBeLessThanOrEqual(1);
      expect(out.rgba[3]).toBe(255);
    }
    s.free();
  });

  it("crops, turns and straightens: the size follows the geometry", async () => {
    const core = await imageCoreFromBytes(bytes);
    const s = core.editSource(photo(200, 100), 200, 100);
    const crop = s.render({ ...emptyEdit(), crop: { x: 0.25, y: 0.5, w: 0.5, h: 0.5 } });
    expect([crop.width, crop.height]).toEqual([100, 50]);
    const turned = s.render({ ...emptyEdit(), rotate: 1 });
    expect([turned.width, turned.height]).toEqual([100, 200]);
    const tilted = s.render({ ...emptyEdit(), straighten: 10 });
    expect(tilted.width).toBeLessThan(200);
    expect(tilted.width / tilted.height).toBeCloseTo(2, 1);
    s.free();
  });

  it("draws markup and filters on top", async () => {
    const core = await imageCoreFromBytes(bytes);
    const s = core.editSource(solid(100, 100, [255, 255, 255]), 100, 100);
    const list: EditList = {
      ...emptyEdit(),
      markup: [
        { t: "pen", color: "#ff0000", width: 0.1, points: [[0.2, 0.5], [0.8, 0.5]] },
        { t: "text", color: "#000000", size: 0.15, x: 0.1, y: 0.1, text: "Hi àè" },
      ],
    };
    const out = s.render(list);
    const at = (x: number, y: number) => Array.from(out.rgba.slice((y * 100 + x) * 4, (y * 100 + x) * 4 + 3));
    expect(at(50, 50)).toEqual([255, 0, 0]);
    expect(at(50, 85)).toEqual([255, 255, 255]);
    expect(out.rgba.filter((v, i) => i % 4 === 0 && v < 100).length).toBeGreaterThan(40);
    const bw = s.render(setFilter(emptyEdit(), "bw"));
    expect(bw.rgba[0]).toBe(bw.rgba[1]);
    s.free();
  });

  it("refuses what the core refuses, with its message, and keeps working", async () => {
    const core = await imageCoreFromBytes(bytes);
    const s = core.editSource(solid(4, 4, [1, 2, 3]), 4, 4);
    expect(() => s.render('{"v":9}')).toThrow(EditError);
    expect(() => s.render('{"v":9}')).toThrow(/version/);
    expect(() => s.render("nonsense")).toThrow(/bad edit list/);
    expect(() => s.render({ ...emptyEdit(), straighten: 70 })).toThrow(/straighten/);
    expect(s.render(emptyEdit()).width).toBe(4);
    expect(() => core.editSource(new Uint8ClampedArray(10), 4, 4)).toThrow(EditError);
    s.free();
    expect(() => s.render(emptyEdit())).toThrow(/freed/);
    s.free();
  });

  it("suggests slider values from the picture's histogram", async () => {
    const core = await imageCoreFromBytes(bytes);
    const dark = core.editSource(solid(32, 32, [12, 12, 12]), 32, 32);
    const a = dark.auto();
    expect(a.exposure).toBeGreaterThan(0.5);
    expect(a.shadows).toBeGreaterThanOrEqual(0);
    expect(Object.keys(a).sort()).toEqual(["contrast", "exposure", "fade", "highlights", "saturation", "shadows", "tint", "vignette", "warmth"]);
    const out = dark.render({ ...emptyEdit(), adjust: a });
    expect(out.rgba[0]).toBeGreaterThan(25);
    dark.free();
  });

  it("holds up over many renders without growing its memory", async () => {
    const core = await imageCoreFromBytes(bytes);
    const memory = (core as unknown as { exports: { memory: WebAssembly.Memory } }).exports.memory;
    const s = core.editSource(photo(320, 240), 320, 240);
    const edit = setAdjust(setAdjust(emptyEdit(), "contrast", 20), "shadows", 30);
    s.render(edit);
    const before = memory.buffer.byteLength;
    for (let i = 0; i < 300; i++) s.render(setAdjust(edit, "exposure", (i % 20) / 10 - 1));
    expect(memory.buffer.byteLength).toBe(before);
    s.free();
  });

  it("renders a 1600 px preview at interactive speed", async () => {
    const core = await imageCoreFromBytes(bytes);
    const [w, h] = [1600, 1067];
    const s = core.editSource(photo(w, h), w, h);
    const sliders: EditList = {
      ...emptyEdit(),
      adjust: { exposure: 0.4, contrast: 20, highlights: -30, shadows: 25, saturation: 15, warmth: 12, tint: -6, vignette: 30, fade: 10 },
    };
    const geometry: EditList = { ...sliders, rotate: 1, straighten: 5, crop: { x: 0.05, y: 0.05, w: 0.9, h: 0.9 } };
    const time = (list: EditList, runs = 9) => {
      s.render(list);
      const ms: number[] = [];
      for (let i = 0; i < runs; i++) {
        const t = performance.now();
        s.render(list);
        ms.push(performance.now() - t);
      }
      return ms.sort((a, b) => a - b)[Math.floor(runs / 2)]!;
    };
    const results = { exposure: time(setAdjust(emptyEdit(), "exposure", 0.5)), sliders: time(sliders), geometry: time(geometry) };
    console.info(`image-core.wasm render of ${w}x${h} (median ms): ${JSON.stringify(results)}`);
    // Far from the frame budget of a slider drag on any desktop; the bound only catches an accidental slow path.
    for (const ms of Object.values(results)) expect(ms).toBeLessThan(400);
    s.free();
  }, 60_000);
});
