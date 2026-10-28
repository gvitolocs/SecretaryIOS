import { describe, expect, it } from "vitest";
import { parseDuplicateGroup, parseItem, type DuplicateGroup, type StackSuggestion } from "../api/gallery";
import { rawItem } from "../test/galleryGrid";
import { copiesOf, itemName, scorePercent, spanSeconds, withoutPhotos, withoutSuggestion } from "./galleryDedup";

function group(ids: string[], id = "dg"): DuplicateGroup {
  return parseDuplicateGroup({
    id,
    class: "exact",
    score: 1,
    applicable: true,
    keep: ids[0],
    reason: "largest_file",
    members: ids.map((i) => ({ ...rawItem(i), review: { metadata: 1 } })),
  })!;
}

describe("the duplicate helpers", () => {
  it("names a photo by its title, else its file name", () => {
    expect(itemName(parseItem(rawItem("a", { title: "Beach" }))!)).toBe("Beach");
    expect(itemName(parseItem(rawItem("a", { title: "", origin: { filename: "IMG_1.HEIC" } }))!)).toBe("IMG_1.HEIC");
  });

  it("never rounds an uncertain score up to 100", () => {
    expect(scorePercent(0.994)).toBe(99);
    expect(scorePercent(0.999)).toBe(99);
    expect(scorePercent(1)).toBe(100);
    expect(scorePercent(0.62)).toBe(62);
  });

  it("lists the copies a Keep this would hide", () => {
    expect(copiesOf(group(["a", "b", "c"]), "b")).toEqual(["a", "c"]);
  });

  it("drops photos from groups, and a group left with one photo is gone", () => {
    const left = withoutPhotos([group(["a", "b", "c"], "g1"), group(["d", "e"], "g2")], new Set(["c", "e"]));
    expect(left.map((g) => [g.id, g.members.map((m) => m.item.id)])).toEqual([["g1", ["a", "b"]]]);
  });

  it("keeps the same group object when nothing in it changed", () => {
    const g = group(["a", "b"]);
    expect(withoutPhotos([g], new Set(["zzz"]))[0]).toBe(g);
  });

  it("removes a suggestion by id", () => {
    const s = (id: string) => ({ id }) as StackSuggestion;
    expect(withoutSuggestion([s("1"), s("2")], "1").map((x) => x.id)).toEqual(["2"]);
  });

  it("formats a burst's span", () => {
    expect(spanSeconds(800)).toBe("0.8");
    expect(spanSeconds(9400)).toBe("9.4");
    expect(spanSeconds(12_400)).toBe("12");
    expect(spanSeconds(-5)).toBe("0.0");
  });
});
