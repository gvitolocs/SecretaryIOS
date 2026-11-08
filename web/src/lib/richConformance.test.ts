// The shared rich-message vectors (tests/conformance/rich/*.json): the web's side. The server
// (crates/lucina-store/tests/conformance_rich.rs) and iOS (RichMessageTests.swift) run the same files.
import { describe, expect, it } from "vitest";
import { parseRich, speechText } from "./rich";

interface Vector {
  name: string;
  input: unknown;
  expect: { bubbles: string[]; blocks: string[]; actions: string[]; speech: string };
}

const vectors = Object.values(
  import.meta.glob<Vector>("../../../tests/conformance/rich/*.json", { eager: true, import: "default" }),
);

describe("rich conformance vectors", () => {
  it("found the vectors", () => {
    expect(vectors.length).toBeGreaterThanOrEqual(4);
  });

  for (const v of vectors) {
    it(v.name, () => {
      const m = parseRich(v.input);
      expect(m).not.toBeNull();
      expect(m!.bubbles).toEqual(v.expect.bubbles);
      expect(m!.blocks.map((b) => b.type)).toEqual(v.expect.blocks);
      expect(m!.actions.map((a) => a.id)).toEqual(v.expect.actions);
      expect(speechText(m!)).toEqual(v.expect.speech);
    });
  }
});
