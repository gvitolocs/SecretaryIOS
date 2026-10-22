import { describe, expect, it } from "vitest";
import { itemArtUrl, sectionArtUrl, svgIconUrl } from "./itemArt";

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
    expect(itemArtUrl("🥝", "kiwi", "🥝")).toBeNull();
  });

  it("finds section art", () => {
    expect(sectionArtUrl("dairy-eggs")).toMatch(/dairy-eggs.*\.svg/);
    expect(sectionArtUrl("nope")).toBeNull();
  });
});
