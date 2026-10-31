import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { GALLERY_CATEGORIES, type ItemCategory } from "../api/gallery";
import { LOCALES, setLocale, t } from "../i18n";
import { albumKey, categoryFromParam, categoryPath, corrections, kindKey, moveKey, notKey } from "./galleryCategories";

beforeEach(() => setLocale("en"));
afterEach(() => setLocale("en"));

const decided = (category: ItemCategory["category"], decidedBy: ItemCategory["decidedBy"] = "rules"): ItemCategory => ({ category, confidence: 0.8, decidedBy, autoCategory: null });

describe("addresses", () => {
  it("builds a category's page and reads it back", () => {
    expect(categoryPath("receipt")).toBe("/gallery/category/receipt");
    for (const c of GALLERY_CATEGORIES) expect(categoryFromParam(categoryPath(c).split("/").pop())).toBe(c);
    for (const bad of [undefined, "", "Receipt", "dragon", "receipt/x"]) expect(categoryFromParam(bad)).toBeNull();
  });

  it("has a name in every language for every category, in each of the four forms", () => {
    for (const locale of LOCALES) {
      setLocale(locale);
      for (const c of GALLERY_CATEGORIES) {
        for (const key of [albumKey(c), kindKey(c), notKey(c), moveKey(c)]) {
          // A missing key shows as the key itself.
          expect(t(key), `${locale} ${key}`).not.toBe(key);
          expect(t(key).length).toBeGreaterThan(1);
        }
      }
    }
    setLocale("en");
    expect(t(albumKey("receipt"))).toBe("Receipts");
    expect(t(notKey("receipt"))).toBe("Not a receipt");
    expect(t(moveKey("document"))).toBe("Move to Documents");
    expect(t(kindKey("pet"))).toBe("Pet");
  });
});

describe("the corrections offered", () => {
  it("offers 'Not a …' first, then a move to every other category", () => {
    const list = corrections(decided("receipt"));
    expect(list[0]).toEqual({ kind: "not", from: "receipt", to: "photo" });
    expect(list.slice(1).map((c) => (c.kind === "move" ? c.to : c.kind))).toEqual(["screenshot", "document", "note", "ticket", "map", "meme", "food", "pet"]);
  });

  it("offers no 'Not a photo' for a photo, and moves to each of the others", () => {
    const list = corrections(decided("photo"));
    expect(list.some((c) => c.kind === "not")).toBe(false);
    expect(list).toHaveLength(9);
    expect(list.every((c) => c.kind === "move" && c.to !== "photo")).toBe(true);
  });

  it("offers going back to automatic only for the owner's own choice, last", () => {
    expect(corrections(decided("receipt")).some((c) => c.kind === "auto")).toBe(false);
    const own = corrections(decided("document", "owner"));
    expect(own[own.length - 1]).toEqual({ kind: "auto" });
    expect(own.filter((c) => c.kind === "auto")).toHaveLength(1);
    // Never a move to where the photo already is.
    expect(own.some((c) => c.kind === "move" && c.to === "document")).toBe(false);
  });
});
