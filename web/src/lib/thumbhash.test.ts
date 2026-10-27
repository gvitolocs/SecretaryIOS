import { describe, expect, it } from "vitest";
import { base64ToBytes, rgbaToDataURL, thumbHashToApproximateAspectRatio, thumbHashToRGBA } from "./thumbhash";

// Known vector from the reference Rust crate (evanw's `thumbhash` 0.1.0):
// rgba_to_thumb_hash of a 24x16 gradient (r = 10x, g = 15y, b = 128, opaque),
// then thumb_hash_to_rgba, sampled at four pixels.
const HASH = "nQcKNZiAh3dwiIh4iHiIiICACPh4";
const PIXELS: Array<[number, number[]]> = [
  [0, [34, 22, 149, 255]],
  [31, [247, 8, 135, 255]],
  [368, [120, 109, 132, 255]],
  [735, [242, 230, 115, 255]],
];

describe("thumbhash", () => {
  it("decodes the reference vector to the same 32x23 pixels as the reference decoder", () => {
    const image = thumbHashToRGBA(base64ToBytes(HASH));
    expect(image.w).toBe(32);
    expect(image.h).toBe(23);
    expect(image.rgba).toHaveLength(32 * 23 * 4);
    for (const [index, rgba] of PIXELS) {
      expect(Array.from(image.rgba.slice(index * 4, index * 4 + 4))).toEqual(rgba);
    }
  });

  it("reports a landscape aspect ratio", () => {
    const ratio = thumbHashToApproximateAspectRatio(base64ToBytes(HASH));
    expect(ratio).toBeCloseTo(1.4, 5);
  });

  it("tolerates data URLs, base64url and whitespace", () => {
    expect(Array.from(base64ToBytes(`data:image/png;base64,${HASH}`))).toEqual(Array.from(base64ToBytes(HASH)));
    const url = HASH.replace(/\+/g, "-").replace(/\//g, "_");
    expect(Array.from(base64ToBytes(`  ${url}\n`)).slice(0, 6)).toEqual(Array.from(base64ToBytes(HASH)).slice(0, 6));
  });

  it("builds a PNG data URL with the decoded dimensions and signature", () => {
    const image = thumbHashToRGBA(base64ToBytes(HASH));
    const url = rgbaToDataURL(image.w, image.h, image.rgba);
    expect(url.startsWith("data:image/png;base64,")).toBe(true);
    const png = Uint8Array.from(atob(url.split(",")[1]!), (c) => c.charCodeAt(0));
    expect(Array.from(png.slice(0, 8))).toEqual([137, 80, 78, 71, 13, 10, 26, 10]);
    const view = new DataView(png.buffer);
    expect(view.getUint32(16)).toBe(32);
    expect(view.getUint32(20)).toBe(23);
  });

  it("returns an empty buffer for garbage", () => {
    expect(thumbHashToRGBA(new Uint8Array()).rgba).toHaveLength(0);
    expect(base64ToBytes("!!!!").length).toBe(0);
  });
});
