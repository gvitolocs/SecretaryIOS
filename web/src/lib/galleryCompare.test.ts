/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { itemRenditions, withLatest } from "../test/galleryParityFixtures";
import {
  clampSplit,
  compareSources,
  decodable,
  defaultPair,
  deviceVerdict,
  dimensionsText,
  followRequest,
  formatName,
  shortHash,
  splitFromPointer,
  splitPercent,
} from "./galleryCompare";
import { imageCoreFromBytes } from "./imageCore";

const core = () => imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm")));

describe("what a browser can draw", () => {
  it("draws the web formats, and JPEG XL only where it is supported", () => {
    for (const mime of ["image/jpeg", "image/png", "image/webp", "image/gif", "IMAGE/JPEG"]) expect(decodable(mime, false), mime).toBe(true);
    for (const mime of ["image/heic", "image/x-adobe-dng", "image/tiff", "video/mp4", ""]) expect(decodable(mime, true), mime).toBe(false);
    expect(decodable("image/jxl", false)).toBe(false);
    expect(decodable("image/jxl", true)).toBe(true);
  });
});

describe("the sources a pane can show", () => {
  it("lists the original, the older versions newest first, then the renditions best first", () => {
    const r = itemRenditions({
      versions: [
        { version: 1, format: "heic", mime: "image/heic", bytes: 1, blake3: "b", width: null, height: null, url: "/v1/media/items/a/original?version=1", replacedAt: null, stored: "store", reachable: true },
        { version: 2, format: "jpeg", mime: "image/jpeg", bytes: 2, blake3: "c", width: null, height: null, url: "/v1/media/items/a/original?version=2", replacedAt: null, stored: "store", reachable: true },
      ],
    });
    const list = compareSources(r, true);
    expect(list.map((s) => s.id)).toEqual(["original", "version:2", "version:1", "preview", "large", "thumb", "grid"]);
    expect(list.map((s) => s.decodable)).toEqual([true, true, false, true, true, true, true]);
    // Without JPEG XL the preview and the grid tile can't be drawn.
    expect(compareSources(r, false).filter((s) => !s.decodable).map((s) => s.id)).toEqual(["version:1", "preview", "grid"]);
  });

  it("does not offer a file the server can't reach", () => {
    const r = itemRenditions();
    r.original.reachable = false;
    expect(compareSources(r, false)[0]).toMatchObject({ id: "original", decodable: false });
  });
});

describe("the default pair", () => {
  it("compares the original with the best display copy", () => {
    expect(defaultPair(compareSources(itemRenditions(), true))).toEqual(["original", "preview"]);
    expect(defaultPair(compareSources(itemRenditions(), false))).toEqual(["original", "large"]);
  });

  it("uses the phone's rendering in place of an original the browser can't draw", () => {
    const r = itemRenditions();
    r.original = { ...r.original, format: "heic", mime: "image/heic" };
    r.renditions.push({ name: "device_preview", format: "jpeg", mime: "image/jpeg", bytes: 900_000, blake3: null, width: 4032, height: 3024, url: "/v1/media/items/a/device-preview?v=1" });
    expect(defaultPair(compareSources(r, false))).toEqual(["device_preview", "large"]);
  });

  it("needs two forms that can be drawn", () => {
    const r = itemRenditions({ renditions: [] });
    expect(defaultPair(compareSources(r, true))).toBeNull();
    r.original.mime = "image/heic";
    expect(defaultPair(compareSources(itemRenditions(), false).filter((s) => s.id !== "large" && s.id !== "thumb"))).toBeNull();
  });
});

describe("the shared view", () => {
  it("makes the second picture follow the first, whatever their pixel sizes", async () => {
    const c = await core();
    // The original (4000 x 3000) and a preview (1440 x 1080) in the same 600 x 400 pane.
    const a = c.view({ new: [4000, 3000, 600, 400] }).view!;
    const b = c.view({ new: [1440, 1080, 600, 400] }).view!;
    const zoomed = c.view({ view: a, gesture: { op: "pinch", factor: 3, x: 300, y: 200 } }).view!;
    const panned = c.view({ view: zoomed, gesture: { op: "pan", dx: -80, dy: 30 } }).view!;
    const followed = c.view(followRequest(panned, b));
    expect(followed.ok).toBe(true);
    expect(followed.view!.scale).toBe(panned.scale);
    expect(followed.view!.offset_x).toBeCloseTo(panned.offset_x, 3);
    expect(followed.view!.offset_y).toBeCloseTo(panned.offset_y, 3);
    // The same part of the same picture: the drawn rectangles match (same shape, same viewport).
    const ra = c.view({ view: panned, gesture: { op: "resize", image_w: 4000, image_h: 3000, viewport_w: 600, viewport_h: 400 } }).rect!;
    expect(followed.rect!.x).toBeCloseTo(ra.x, 2);
    expect(followed.rect!.y).toBeCloseTo(ra.y, 2);
    expect(followed.rect!.w).toBeCloseTo(ra.w, 2);
    expect(followed.rect!.h).toBeCloseTo(ra.h, 2);
  });

  it("keeps the follower inside its own picture when the two differ in shape", async () => {
    const c = await core();
    const wide = c.view({ new: [4000, 3000, 600, 400] }).view!;
    const square = c.view({ new: [1000, 1000, 600, 400] }).view!;
    const zoomed = c.view({ view: wide, gesture: { op: "pinch", factor: 4, x: 300, y: 200 } }).view!;
    const far = c.view({ view: zoomed, gesture: { op: "pan", dx: 5000, dy: 5000 } }).view!;
    const followed = c.view(followRequest(far, square)).view!;
    // The square fits its height (400 points) at scale 1, so it is 400 * scale points wide and tall.
    const drawn = 400 * followed.scale;
    expect(Math.abs(followed.offset_x)).toBeLessThanOrEqual((drawn - 600) / 2 + 0.001);
    expect(Math.abs(followed.offset_y)).toBeLessThanOrEqual((drawn - 400) / 2 + 0.001);
  });
});

describe("the slider", () => {
  it("keeps the divider inside the stage", () => {
    expect(clampSplit(0.5)).toBe(0.5);
    expect(clampSplit(-3)).toBe(0.02);
    expect(clampSplit(9)).toBe(0.98);
    expect(clampSplit(Number.NaN)).toBe(0.5);
    expect(splitFromPointer(300, 100, 400)).toBe(0.5);
    expect(splitFromPointer(0, 100, 400)).toBe(0.02);
    expect(splitFromPointer(900, 100, 400)).toBe(0.98);
    expect(splitFromPointer(300, 100, 0)).toBe(0.5);
    // Steps of five never leave a floating-point tail on the divider's position.
    expect(clampSplit(0.5 + 0.05)).toBe(0.55);
    expect(splitPercent(clampSplit(0.5 + 0.05))).toBe(55);
    expect(splitPercent(0.02)).toBe(2);
  });
});

describe("the copy rows", () => {
  it("names formats and shortens checksums", () => {
    expect(formatName("heic")).toBe("HEIC");
    expect(formatName("jpeg")).toBe("JPEG");
    expect(formatName("jxl")).toBe("JPEG XL");
    expect(shortHash("")).toBe("");
    expect(shortHash("abc")).toBe("abc");
    expect(shortHash("0123456789abcdef0123456789abcdef")).toBe("0123456789…abcdef");
    expect(dimensionsText({ width: 4000, height: 3000 })).toBe("4000 × 3000");
    expect(dimensionsText({ width: null, height: 3000 })).toBe("");
  });

  it("says whether the device's last upload is the server's copy", () => {
    const r = withLatest(itemRenditions());
    expect(deviceVerdict(r)).toBe("same");
    const other = withLatest(itemRenditions());
    other.device!.latest = { ...other.device!.latest!, blake3: "b".repeat(64) };
    expect(deviceVerdict(other)).toBe("differs");
    // No checksum: only the sizes can be compared.
    const sized = withLatest(itemRenditions());
    sized.device!.latest = { ...sized.device!.latest!, blake3: null };
    expect(deviceVerdict(sized)).toBe("sameSize");
    sized.device!.latest = { ...sized.device!.latest!, blake3: null, bytes: 5 };
    expect(deviceVerdict(sized)).toBe("differs");
    sized.device!.latest = { ...sized.device!.latest!, blake3: null, bytes: null };
    expect(deviceVerdict(sized)).toBe("unknown");
    expect(deviceVerdict(itemRenditions({ device: null }))).toBe("unknown");
  });
});
