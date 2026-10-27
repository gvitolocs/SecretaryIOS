/// <reference types="node" />
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { itemArtUrl, itemVisual, sectionArtUrl, svgIconUrl, type VisualRow } from "./itemArt";
import { buildIndex, parseCatalog } from "./shoppingCatalog";

describe("itemArt", () => {
  it("resolves svg:<key> icons to bundled files", () => {
    expect(svgIconUrl("svg:banana")).toMatch(/banana.*\.svg/);
    expect(svgIconUrl("svg:produce")).toMatch(/produce.*\.svg/);
    expect(svgIconUrl("svg:no-such-icon")).toBeNull();
    expect(svgIconUrl("🍌")).toBeNull();
  });

  it("uses a catalog item's art unless the row has its own emoji", () => {
    expect(itemArtUrl("🍌", "banana", "🍌")).toMatch(/banana.*\.svg/);
    expect(itemArtUrl(null, "banana")).toMatch(/banana.*\.svg/);
    expect(itemArtUrl("🍒", "banana", "🍌")).toBeNull();
    expect(itemArtUrl("🍌", null)).toBeNull();
    expect(itemArtUrl("🫙", "not-a-catalog-item", "🫙")).toBeNull();
  });

  it("finds section art", () => {
    expect(sectionArtUrl("dairy-eggs")).toMatch(/dairy-eggs.*\.svg/);
    expect(sectionArtUrl("nope")).toBeNull();
  });
});

describe("itemVisual", () => {
  const catalog = parseCatalog(JSON.parse(readFileSync(join(process.cwd(), "../assets/catalog/catalog.it.json"), "utf8")));
  const index = buildIndex(catalog, "it");
  const v = (row: VisualRow) => itemVisual(row, index, catalog.sections);
  const art = (row: VisualRow) => {
    const out = v(row);
    return out.kind === "art" ? out.url.replace(/^.*\//, "").replace(/(-[\w-]{8})?\.svg$/, "") : out;
  };

  it("draws every item of the owner's list (catalog rows) with its own art", () => {
    expect(art({ icon: "🍖", catalogId: "prosciutto", name: "Prosciutto crudo" })).toMatch(/^prosciutto/);
    expect(art({ icon: "🍖", catalogId: "ham", name: "Prosciutto cotto" })).toMatch(/^ham/);
    expect(art({ icon: "🥛", catalogId: "milk", name: "Latte" })).toMatch(/^milk/);
    expect(art({ icon: "🍞", catalogId: "rye-bread", name: "Pane di segale" })).toMatch(/^rye-bread/);
    expect(art({ icon: null, catalogId: "yogurt", name: "Yogurt" })).toMatch(/^yogurt/);
  });

  it("finds art for free text and custom items by their name", () => {
    expect(art({ icon: null, name: "Zucchine" })).toMatch(/^zucchini/);
    expect(art({ icon: "🍞", name: "Pane di segale danese", section: "bakery" })).toMatch(/^rye-bread/);
    expect(art({ icon: null, name: "Prosciutto crudo di Parma" })).toMatch(/^prosciutto/);
    expect(art({ icon: "🕯️", name: "Candele profumate", section: "household" })).toMatch(/^candles/);
    expect(art({ icon: null, name: "Organic milk" })).toMatch(/^milk/);
  });

  it("does not borrow art from an item of another section", () => {
    // "Latte detergente" is cleansing milk, not milk.
    expect(art({ icon: "🫧", name: "Latte detergente", section: "personal-care" })).toEqual({ kind: "emoji", emoji: "🫧" });
    expect(art({ icon: "🧴", name: "Latte detergente", section: "personal-care" })).toMatch(/^personal-care/);
  });

  it("keeps an emoji the owner picked for a catalog row", () => {
    expect(v({ icon: "🍒", catalogId: "banana", name: "Banane" })).toEqual({ kind: "emoji", emoji: "🍒" });
  });

  it("falls back to a specific emoji, then section art, then the first letter", () => {
    expect(v({ icon: "🧯", name: "Estintore", section: "household" })).toEqual({ kind: "emoji", emoji: "🧯" });
    // The section's own emoji or the cart say nothing more than the section art.
    expect(art({ icon: "🧽", name: "Gadget magico", section: "household" })).toMatch(/^household/);
    expect(art({ icon: "🛒", name: "Qualcosa", section: "pets" })).toMatch(/^pets/);
    expect(v({ icon: null, name: "qwerty", section: "other" })).toEqual({ kind: "initial", letter: "Q" });
    expect(v({ icon: "🛒", name: "ünbekannt" })).toEqual({ kind: "initial", letter: "Ü" });
  });

  it("works before the catalog has loaded", () => {
    expect(itemVisual({ icon: "🍌", catalogId: "banana", name: "Banane" }, null, [])).toMatchObject({ kind: "art" });
    expect(itemVisual({ icon: "🍌", name: "Banane" }, null, [])).toEqual({ kind: "emoji", emoji: "🍌" });
  });
});
