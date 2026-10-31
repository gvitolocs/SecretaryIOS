import type { DuplicateGroup, GalleryItem, StackSuggestion } from "../api/gallery";

// The duplicate review's and the stacks' pure helpers (views/GalleryDuplicates.tsx, components/gallery/duplicates, components/gallery/stacks).

/** What to call a photo in labels: its title, else its file name, else "". */
export function itemName(item: GalleryItem): string {
  return item.title || item.origin.filename || "";
}

/** A score from 0 to 1 as a whole percent, never 100 unless it is exactly 1. */
export function scorePercent(score: number): number {
  const p = Math.round(score * 100);
  return score < 1 ? Math.min(99, p) : 100;
}

/** The ids of a group other than the one to keep: what "Keep this" hides. */
export function copiesOf(group: DuplicateGroup, keepId: string): string[] {
  return group.members.map((m) => m.item.id).filter((id) => id !== keepId);
}

/** The groups without the given photos; a group left with fewer than two photos is no group any more. */
export function withoutPhotos(groups: DuplicateGroup[], gone: ReadonlySet<string>): DuplicateGroup[] {
  const out: DuplicateGroup[] = [];
  for (const g of groups) {
    const members = g.members.filter((m) => !gone.has(m.item.id));
    if (members.length < 2) continue;
    out.push(members.length === g.members.length ? g : { ...g, members });
  }
  return out;
}

/** The suggestions without the given ones (by id). */
export function withoutSuggestion(list: StackSuggestion[], id: string): StackSuggestion[] {
  return list.filter((s) => s.id !== id);
}

/** Seconds a burst spans, for its label ("0.8 s"); one decimal under ten seconds. */
export function spanSeconds(ms: number): string {
  const s = Math.max(0, ms) / 1000;
  return s < 10 ? s.toFixed(1) : String(Math.round(s));
}
