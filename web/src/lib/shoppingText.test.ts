/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { fold } from "./shoppingCatalog";
import { MAX_PARTS, splitItems, type ItemPart } from "./shoppingText";

interface Fixture {
  known: string[];
  cases: Array<{ in: string; out: Array<{ name: string; quantity?: number; unit?: string }> }>;
}

// The shared cases, also run by crates/shopping-text and ios ShoppingTextTests.
const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/shopping-split-cases.json"), "utf8")) as Fixture;
const known = new Set(fixture.known.map(fold));
const isKnown = (s: string) => known.has(fold(s));

describe("splitItems", () => {
  it("passes every shared case", () => {
    expect(fixture.cases.length).toBeGreaterThanOrEqual(40);
    for (const c of fixture.cases) {
      const want: ItemPart[] = c.out.map((p) => ({ name: p.name, quantity: p.quantity ?? null, unit: p.unit ?? null }));
      expect(splitItems(c.in, isKnown), c.in).toEqual(want);
    }
  });

  it("caps the parts and bounds quantities", () => {
    const many = Array.from({ length: 60 }, (_, i) => `a${i}`).join(", ");
    expect(splitItems(many, () => false)).toHaveLength(MAX_PARTS);
    expect(splitItems("9999 uova", () => false)[0]?.quantity).toBe(9999);
    expect(splitItems("10000 uova", () => false)[0]).toEqual({ name: "10000 uova", quantity: null, unit: null });
  });
});
