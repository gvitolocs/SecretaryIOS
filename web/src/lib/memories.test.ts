import { describe, expect, it } from "vitest";
import type { MemoryYear } from "../api/gallery";
import { galleryItem } from "../test/galleryFixtures";
import { coverOf, memoryCards } from "./memories";

describe("coverOf", () => {
  it("prefers a favorite photo, then any photo, then the first item", () => {
    const clip = galleryItem("clip", { kind: "video" });
    const plain = galleryItem("plain");
    const fav = galleryItem("fav", { favorite: true });
    expect(coverOf([clip, plain, fav])).toBe(fav);
    expect(coverOf([clip, plain])).toBe(plain);
    expect(coverOf([clip])).toBe(clip);
    expect(coverOf([])).toBeNull();
    // A favorite video does not beat a photo.
    expect(coverOf([galleryItem("v", { kind: "video", favorite: true }), plain])).toBe(plain);
  });
});

describe("memoryCards", () => {
  const year = (y: number, ids: string[], total = ids.length): MemoryYear => ({ year: y, yearsAgo: 2026 - y, total, items: ids.map((i) => galleryItem(i)) });

  it("makes one card per year, newest first as sent, then Recently added", () => {
    const recent = [galleryItem("r1"), galleryItem("r2")];
    const cards = memoryCards([year(2025, ["a"]), year(2023, ["b", "c"], 9)], recent, 40);
    expect(cards.map((c) => c.id)).toEqual(["day:2025", "day:2023", "recent"]);
    expect(cards[1]).toMatchObject({ kind: "day", year: 2023, yearsAgo: 3, total: 9 });
    expect(cards[2]).toMatchObject({ kind: "recent", total: 40 });
    expect(cards[2]!.items).toBe(recent);
  });

  it("drops empty sets and never counts fewer pictures than it shows", () => {
    expect(memoryCards([{ year: 2020, yearsAgo: 6, total: 0, items: [] }], [], 0)).toEqual([]);
    const cards = memoryCards([], [galleryItem("r1"), galleryItem("r2"), galleryItem("r3")], 1);
    expect(cards[0]!.total).toBe(3);
  });
});
