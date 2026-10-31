import { describe, expect, it } from "vitest";
import { buildIndex, exactMatch, fold, itemKey, parseCatalog, resolveByName, searchCatalog } from "./shoppingCatalog";

const catalog = parseCatalog({
  version: 1,
  lang: "it",
  sections: [
    { id: "dairy", name: "Latticini", icon: "🧀", order: 2 },
    { id: "produce", name: "Frutta", icon: "🥦", order: 1 },
  ],
  items: [
    { id: "milk", name: "Latte", names: { en: "Milk", da: "Mælk" }, aliases: [], section: "dairy", icon: "🥛" },
    { id: "oat-milk", name: "Latte d'avena", names: { en: "Oat milk" }, aliases: ["avena"], section: "dairy", icon: "🥛" },
    { id: "chocolate-milk", name: "Cioccolato al latte", aliases: [], section: "pantry", icon: "🍫" },
    { id: "coffee", name: "Caffè", aliases: ["espresso"], section: "pantry", icon: "☕" },
    { id: "lettuce", name: "Insalata", aliases: ["lattuga"], section: "produce", icon: "🥬" },
    { id: "parmesan", name: "Parmigiano", aliases: ["grana padano"], section: "dairy", icon: "🧀" },
    { id: "apple", name: "Mele", names: { da: "Æbler" }, aliases: [], section: "produce", icon: "🍎" },
    { id: "broken" },
  ],
});
const it_ = buildIndex(catalog, "it");
const names = (q: string, index = it_) => searchCatalog(index, q).map((i) => i.id);

describe("catalog parsing", () => {
  it("drops invalid items and sorts sections", () => {
    expect(catalog.items).toHaveLength(7);
    expect(catalog.sections.map((s) => s.id)).toEqual(["produce", "dairy"]);
  });
});

describe("fold", () => {
  it("folds diacritics, Danish letters and punctuation", () => {
    expect(fold("Caffè")).toBe("caffe");
    expect(fold("  Latte d'Avena ")).toBe("latte d avena");
    expect(fold("Æbler og smør")).toBe("aebler og smoer");
    expect(fold("Rugbrød")).toBe("rugbroed");
    expect(fold("blåbær")).toBe("blaabaer");
    expect(fold("olio d’oliva")).toBe("olio d oliva");
    expect(fold("Coca-Cola 0%")).toBe("coca cola 0");
    expect(itemKey(null, "Pané")).toBe(itemKey(null, "pane"));
  });
});

describe("searchCatalog", () => {
  it("ranks exact and prefix before word-start before substring", () => {
    expect(names("latte")).toEqual(["milk", "oat-milk", "chocolate-milk"]);
  });

  it("matches without the accent", () => {
    expect(names("caffe")).toEqual(["coffee"]);
    expect(names("CAFFÈ")).toEqual(["coffee"]);
  });

  it("uses aliases after names, and other-language names as aliases", () => {
    expect(names("espresso")).toEqual(["coffee"]);
    expect(names("grana")).toEqual(["parmesan"]);
    expect(names("milk")).toEqual(["milk", "oat-milk"]);
    // "latt": name prefixes, then a name word-start, then the alias prefix ("lattuga").
    expect(names("latt")).toEqual(["milk", "oat-milk", "chocolate-milk", "lettuce"]);
  });

  it("matches every query word at word starts", () => {
    expect(names("lat ave")).toEqual(["oat-milk"]);
  });

  it("searches in the UI language", () => {
    const da = buildIndex(catalog, "da");
    expect(names("aebler", da)).toEqual(["apple"]);
    expect(names("æb", da)).toEqual(["apple"]);
    expect(names("mael", da)).toEqual(["milk"]);
  });

  it("boosts recently used items within a tier", () => {
    const res = searchCatalog(it_, "latte", { boost: (id) => (id === "oat-milk" ? 10 : 0) });
    // Exact match still wins; the boost only reorders equal tiers.
    expect(res.map((i) => i.id)).toEqual(["milk", "oat-milk", "chocolate-milk"]);
    const prefix = searchCatalog(it_, "lat", { boost: (id) => (id === "oat-milk" ? 10 : 0) });
    expect(prefix.map((i) => i.id).slice(0, 2)).toEqual(["oat-milk", "milk"]);
  });

  it("finds exact matches by name or alias", () => {
    expect(exactMatch(it_, "caffe")?.id).toBe("coffee");
    expect(exactMatch(it_, "Avena")?.id).toBe("oat-milk");
    expect(exactMatch(it_, "latt")).toBeNull();
  });
});

describe("resolveByName (rows with no catalog id)", () => {
  const hit = (q: string) => {
    const r = resolveByName(it_, q);
    return r ? [r.item.id, r.exact] : null;
  };
  it("matches a name in any language exactly", () => {
    expect(hit("latte")).toEqual(["milk", true]);
    expect(hit("Milk")).toEqual(["milk", true]);
    expect(hit("MÆLK")).toEqual(["milk", true]);
    expect(hit("aebler")).toEqual(["apple", true]);
    expect(hit("caffe")).toEqual(["coffee", true]);
  });
  it("falls back to aliases", () => {
    expect(hit("espresso")).toEqual(["coffee", false]);
    expect(hit("Grana Padano")).toEqual(["parmesan", false]);
  });
  it("never guesses", () => {
    expect(hit("latt")).toBeNull();
    expect(hit("milks")).toBeNull();
    expect(hit("the milk")).toBeNull();
    expect(hit("detersivo piatti")).toBeNull();
    expect(hit("")).toBeNull();
  });
});
