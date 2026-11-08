// A clip that cuts an emoji in half leaves a lone surrogate, which a browser draws as a box
// (flaw: emoji render as boxes).
import { describe, expect, it } from "vitest";
import { parseRich } from "./rich";

const LONE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

describe("clipping a bubble", () => {
  for (const pad of [896, 897, 898, 899]) {
    it(`keeps emoji whole with ${pad} characters before them`, () => {
      const m = parseRich({ format: "v1", bubbles: ["x".repeat(pad) + "😀😀😀"] });
      const bubble = m!.bubbles[0]!;
      expect(bubble.endsWith("…")).toBe(true);
      expect(LONE.test(bubble)).toBe(false);
    });
  }
});
