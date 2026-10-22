import { describe, expect, it } from "vitest";
import {
  applyPatch,
  categoriesPresent,
  categoryOf,
  DEFAULT_SETTINGS,
  outfitName,
  parseSettings,
  parseWardrobe,
  resolveOutfit,
  sectionFor,
  thumbnailUrl,
  wantedOutfit,
} from "./wardrobe";
import { OUTFITS } from "../test/wardrobe";

describe("wardrobe parsing", () => {
  it("keeps only valid outfits", () => {
    const list = parseWardrobe([
      ...OUTFITS,
      { id: "gym", glb: "dup.glb" },
      { id: "Nope", glb: "x.glb" },
      { id: "evil", glb: "../x.glb" },
      { id: "img", glb: "x.png" },
      { id: "t", glb: "t.glb", thumbnail: "javascript:alert(1)", name: { xx1: "bad", en: "  T  " } },
      42,
    ]);
    expect(list.map((o) => o.id)).toEqual(["uniform", "gym", "chef", "maid", "t"]);
    expect(list[4]).toEqual({ id: "t", glb: "t.glb", name: { en: "T" } });
    expect(parseWardrobe({ outfits: [] })).toEqual([]);
  });

  it("fills settings defaults", () => {
    expect(parseSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(parseSettings({ outfit: "maid", auto: false, rules: { gym: "x", shopping: 3 }, updatedMs: 5 })).toEqual({
      outfit: "maid",
      auto: false,
      rules: { gym: "x", shopping: "chef", default: "uniform" },
      updatedMs: 5,
      updatedBy: "",
    });
    expect(parseSettings({ outfit: "Bad Id" }).outfit).toBeNull();
  });

  it("patches", () => {
    const s = applyPatch(DEFAULT_SETTINGS, { rules: { gym: "maid" } });
    expect(s.rules).toEqual({ gym: "maid", shopping: "chef", default: "uniform" });
    expect(applyPatch({ ...s, outfit: "maid" }, { outfit: null }).outfit).toBeNull();
  });
});

describe("which outfit", () => {
  const outfits = parseWardrobe(OUTFITS);

  it("follows the section with auto, the default without, the pin always", () => {
    expect(sectionFor("/gym")).toBe("gym");
    expect(sectionFor("/gym/workout")).toBe("gym");
    expect(sectionFor("/shopping")).toBe("shopping");
    expect(sectionFor("/")).toBe("default");
    expect(sectionFor("/weight")).toBe("default");
    expect(sectionFor("/gymnastics")).toBe("default");

    const s = DEFAULT_SETTINGS;
    expect(resolveOutfit(s, "gym", outfits)?.id).toBe("gym");
    expect(resolveOutfit(s, "shopping", outfits)?.id).toBe("chef");
    expect(resolveOutfit(s, "default", outfits)?.id).toBe("uniform");
    expect(resolveOutfit({ ...s, auto: false }, "gym", outfits)?.id).toBe("uniform");
    expect(resolveOutfit({ ...s, outfit: "maid" }, "gym", outfits)?.id).toBe("maid");
    expect(wantedOutfit({ ...s, outfit: "maid" }, "default")).toBe("maid");
  });

  it("falls back to installed outfits", () => {
    expect(resolveOutfit({ ...DEFAULT_SETTINGS, outfit: "pirate" }, "gym", outfits)?.id).toBe("uniform");
    const onlyMaid = outfits.filter((o) => o.id === "maid");
    expect(resolveOutfit(DEFAULT_SETTINGS, "gym", onlyMaid)?.id).toBe("maid");
    expect(resolveOutfit(DEFAULT_SETTINGS, "gym", [])).toBeNull();
  });

  it("names, pictures and categories", () => {
    const [u, g, c, m] = outfits;
    expect(outfitName(u!, "it")).toBe("Divisa");
    expect(outfitName(g!, "da")).toBe("Gym");
    expect(outfitName(c!, "it")).toBe("Chef");
    expect(outfitName(m!, "da")).toBe("Maid");
    expect(thumbnailUrl(u!)).toBe("/v1/assets/outfit-uniform.webp");
    expect(thumbnailUrl(g!)).toBeNull();
    expect(outfits.map(categoryOf)).toEqual(["uniform", "sport", "work", "work"]);
    expect(categoriesPresent(outfits)).toEqual(["all", "uniform", "sport", "work"]);
    expect(categoryOf({ id: "x", glb: "x.glb" })).toBe("other");
  });
});
