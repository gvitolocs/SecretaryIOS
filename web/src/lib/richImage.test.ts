import { describe, expect, it } from "vitest";
import { MAX_IMAGES, parseRich, validImageSrc } from "./rich";

const map = {
  type: "image",
  src: "/v1/au/map.png?building=1532&z=17",
  alt: "Map of AU building 1532",
  caption: "AU building 1532 (AU Find, © OpenStreetMap contributors)",
  width: 800,
  height: 600,
};

function blocks(...b: unknown[]) {
  return parseRich({ format: "v1", bubbles: ["Here"], blocks: b })!.blocks;
}

describe("image blocks", () => {
  it("keeps an AU map from this server", () => {
    expect(blocks(map)).toEqual([map]);
  });

  it("accepts only this server's paths", () => {
    expect(validImageSrc("/v1/au/map.png?lat=56.16970&lng=10.20390&z=15")).toBe(true);
    expect(validImageSrc("https://getsecretary.tech/v1/au/map.png?building=1872")).toBe(true);
    for (const bad of [
      "",
      "javascript:alert(1)",
      "data:image/png;base64,AAAA",
      "https://evil.example/x.png",
      "http://getsecretary.tech/v1/x.png",
      "https://getsecretary.tech.evil.example/x.png",
      "//evil.example/x.png",
      "/v1//evil",
      "/v1/../admin",
      "/api/secretary/au/map.png",
      `/v1/${"a".repeat(400)}`,
    ]) {
      expect(validImageSrc(bad), bad).toBe(false);
      expect(blocks({ ...map, src: bad }), bad).toEqual([]);
    }
  });

  it("drops bad sizes, falls back to the caption for alt, and keeps at most two", () => {
    expect(blocks({ ...map, width: 0, height: 99999 })).toEqual([
      { type: "image", src: map.src, alt: map.alt, caption: map.caption },
    ]);
    expect(blocks({ type: "image", src: map.src, caption: "Campus" })).toEqual([
      { type: "image", src: map.src, alt: "Campus", caption: "Campus" },
    ]);
    expect(blocks(map, map, map)).toHaveLength(MAX_IMAGES);
  });
});
