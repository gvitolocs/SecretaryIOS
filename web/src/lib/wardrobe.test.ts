import { describe, expect, it } from "vitest";
import { applyPatch as applyLookPatch, lookHair, lookHairColor, lookOutfit, parseLooks, parseWardrobeFile as parseFile, wornLook } from "./wardrobe";
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
  hairChoices,
  NATURAL_HAIR,
  parseHairColors,
  parseHairstyles,
  parseWardrobeFile,
  resolveHair,
  resolveHairColor,
  wantedHair,
  wantedOutfit,
} from "./wardrobe";
import { OUTFITS, WARDROBE } from "../test/wardrobe";

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
    expect(parseSettings({ outfit: "maid", hair: "bun", hairColor: "pink", auto: false, rules: { gym: "x", shopping: 3, default: { hair: "bun" } }, updatedMs: 5 })).toEqual({
      outfit: "maid",
      hair: "bun",
      hairColor: "pink",
      auto: false,
      rules: {
        gym: { outfit: "x", hair: "natural" },
        shopping: { outfit: "chef", hair: "natural" },
        default: { outfit: "uniform", hair: "bun" },
      },
      updatedMs: 5,
      updatedBy: "",
      looks: [],
      current: null,
    });
    expect(DEFAULT_SETTINGS.rules.gym).toEqual({ outfit: "gym", hair: "ponytail" });
    expect(parseSettings({ outfit: "Bad Id" }).outfit).toBeNull();
  });

  it("patches", () => {
    const s = applyPatch(DEFAULT_SETTINGS, { rules: { gym: { outfit: "maid" }, shopping: { hair: "bun" } }, hairColor: "pink" });
    expect(s.rules).toEqual({
      gym: { outfit: "maid", hair: "ponytail" },
      shopping: { outfit: "chef", hair: "bun" },
      default: { outfit: "uniform", hair: "natural" },
    });
    expect(s.hairColor).toBe("pink");
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

describe("hair", () => {
  const file = parseWardrobeFile(WARDROBE);

  it("reads the whole wardrobe file, and the old bare array", () => {
    expect(file.hairstyles.map((h) => h.id)).toEqual(["natural", "ponytail", "bun"]);
    expect(file.hairstyles[0]!.glb).toBeUndefined();
    expect(file.hairstyles[1]!.hideTiara).toBe(true);
    expect(file.colors).toEqual([
      { id: "pink", name: { en: "Pink", it: "Rosa" }, hex: "#ff8ac8" },
      { id: "silver", name: { en: "Silver" }, hex: "#c8cedc" },
    ]);
    expect(parseWardrobeFile(OUTFITS).outfits).toHaveLength(4);
    expect(parseHairstyles([{ id: "x" }, { id: "y", glb: "../y.glb" }])).toEqual([]);
    expect(parseHairColors([{ id: "red", hex: "red" }])).toEqual([]);
  });

  it("resolves the hairstyle and colour by section, pins and installs", () => {
    const s = DEFAULT_SETTINGS;
    expect(wantedHair(s, "gym")).toBe("ponytail");
    expect(resolveHair(s, "gym", file.hairstyles)).toBe("ponytail");
    expect(resolveHair(s, "default", file.hairstyles)).toBe(NATURAL_HAIR);
    expect(resolveHair({ ...s, auto: false }, "gym", file.hairstyles)).toBe(NATURAL_HAIR);
    expect(resolveHair({ ...s, hair: "bun" }, "shopping", file.hairstyles)).toBe("bun");
    expect(resolveHair(s, "gym", []), "not installed: her own hair").toBe(NATURAL_HAIR);
    expect(resolveHairColor({ ...s, hairColor: "pink" }, file.colors)).toBe("pink");
    expect(resolveHairColor({ ...s, hairColor: "green" }, file.colors)).toBeNull();
    expect(hairChoices([]).map((h) => h.id)).toEqual([NATURAL_HAIR]);
    expect(hairChoices(file.hairstyles.slice(1)).map((h) => h.id)).toEqual([NATURAL_HAIR, "ponytail", "bun"]);
  });
});

describe("saved looks (the character editor)", () => {
  const look = { body: { height: 60 }, hair: { style: "ponytail", color: "#AA3355" }, outfit: { id: "maid", parts: {} } };

  it("parses looks and current from the server", () => {
    const parsed = parseLooks(
      [
        { id: "serata", name: " Serata ", look, updatedMs: 3 },
        { id: "serata", name: "dup", look },
        { id: "Bad Id", name: "x", look },
        { id: "noname", name: "", look },
        { id: "nolook", name: "x", look: [] },
      ],
      "serata",
    );
    expect(parsed.looks).toEqual([{ id: "serata", name: "Serata", look, updatedMs: 3 }]);
    expect(parsed.current).toBe("serata");
    expect(parseLooks([], "serata").current).toBeNull();
    expect(parseLooks("x", 1)).toEqual({ looks: [], current: null });
  });

  it("applies look patches optimistically", () => {
    const s0 = parseLooks([{ id: "a", name: "A", look: {} }], "a");
    const settings = { outfit: null, hair: null, hairColor: null, auto: true, rules: { gym: { outfit: "gym", hair: "natural" }, shopping: { outfit: "chef", hair: "natural" }, default: { outfit: "uniform", hair: "natural" } }, updatedMs: 0, updatedBy: "", ...s0 };
    const saved = applyLookPatch(settings, { saveLook: { id: "b", name: "B", look }, current: "b" });
    expect(saved.looks.map((l) => l.id)).toEqual(["a", "b"]);
    expect(saved.current).toBe("b");
    expect(wornLook(saved)).toEqual(look);
    const deleted = applyLookPatch(saved, { deleteLook: "b" });
    expect(deleted.current).toBeNull();
    expect(wornLook(deleted)).toBeNull();
  });

  it("a worn look overrides outfit, hair and colour only when installed", () => {
    const outfits = [{ id: "uniform", glb: "lucina.glb" }, { id: "maid", glb: "lucina-maid.glb" }];
    expect(lookOutfit(look, outfits)).toBe("maid");
    expect(lookOutfit({ outfit: { id: "pirate" } }, outfits)).toBeNull();
    expect(lookOutfit({ outfit: { id: null, parts: { top: "casual-top" } } }, outfits)).toBeNull();
    expect(lookOutfit({ outfit: { id: null, parts: { top: "casual-top" } } }, [...outfits, { id: "base", glb: "lucina-g-base.glb" }])).toBe("base");
    expect(lookHair(look, [{ id: "ponytail", glb: "lucina-hair-ponytail.glb" }])).toBe("ponytail");
    expect(lookHair(look, [])).toBeNull();
    expect(lookHair({ hair: { style: "natural" } }, [])).toBe("natural");
    expect(lookHairColor(look)).toBe("#aa3355");
    expect(lookHairColor({ hair: { color: "red" } })).toBeNull();
    expect(lookOutfit(null, outfits)).toBeNull();
  });

  it("passes the editor catalogue through", () => {
    const f = parseFile({ accessories: [{ id: "beret" }, 3, null, []], parts: "x" });
    expect(f.accessories).toEqual([{ id: "beret" }]);
    expect(f.parts).toEqual([]);
  });
});
