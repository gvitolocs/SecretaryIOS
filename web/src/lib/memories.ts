// The memories strip's rules: which cards it shows and in what order, and a stable cover for each.

import type { GalleryItem, MemoryYear } from "../api/gallery";

export interface MemoryCard {
  /** `day:<year>` or `recent`. */
  id: string;
  kind: "day" | "recent";
  year?: number;
  yearsAgo?: number;
  /** All of the card's pictures, in the order the viewer pages through them. */
  items: GalleryItem[];
  /** How many pictures the card stands for (more than `items` for a busy day). */
  total: number;
  cover: GalleryItem;
}

/** The picture that stands for a set: a favorite photo, else the first photo, else the first item. */
export function coverOf(items: GalleryItem[]): GalleryItem | null {
  return items.find((i) => i.favorite && i.kind === "photo") ?? items.find((i) => i.kind === "photo") ?? items[0] ?? null;
}

/** On this day (newest year first, as the server sends them), then Recently added; empty sets are dropped. */
export function memoryCards(years: MemoryYear[], recent: GalleryItem[], recentTotal: number): MemoryCard[] {
  const cards: MemoryCard[] = [];
  for (const y of years) {
    const cover = coverOf(y.items);
    if (cover) cards.push({ id: `day:${y.year}`, kind: "day", year: y.year, yearsAgo: y.yearsAgo, items: y.items, total: y.total, cover });
  }
  const cover = coverOf(recent);
  if (cover) cards.push({ id: "recent", kind: "recent", items: recent, total: Math.max(recentTotal, recent.length), cover });
  return cards;
}
