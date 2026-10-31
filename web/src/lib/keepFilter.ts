// Views, search and filters over the local cache (client-side, like Keep).

import type { KeepColor, KeepNote } from "../api/keep";
import { isAudio, isImage } from "../api/keep";
import { hostOf, linksIn, plainText } from "./keepText";

export type KeepView = { kind: "notes" } | { kind: "reminders" } | { kind: "archive" } | { kind: "trash" } | { kind: "label"; tag: string };

export const NOTE_TYPES = ["reminders", "lists", "images", "audio", "urls"] as const;
export type NoteType = (typeof NOTE_TYPES)[number];

export interface SearchState {
  q: string;
  type?: NoteType;
  /** A normalised label (tag). */
  label?: string;
  color?: KeepColor;
}

export const emptySearch = (): SearchState => ({ q: "" });
export const searchActive = (s: SearchState): boolean => !!(s.q.trim() || s.type || s.label || s.color);

const fold = (s: string): string =>
  s
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase();

export const inTrash = (n: KeepNote): boolean => !!n.deletedAt;

/** Higher position = earlier in the grid; ties fall back to the newest first. */
export function byPosition(a: KeepNote, b: KeepNote): number {
  return b.position - a.position || b.createdAt.localeCompare(a.createdAt) || a.id.localeCompare(b.id);
}

export function hasUrl(n: KeepNote): boolean {
  return n.annotations.length > 0 || linksIn(n.content).length > 0 || (n.items ?? []).some((i) => linksIn(i.text).length > 0);
}

export function matchesType(n: KeepNote, type: NoteType): boolean {
  switch (type) {
    case "reminders":
      return !!n.reminder;
    case "lists":
      return n.items !== undefined;
    case "images":
      return n.attachments.some(isImage);
    case "audio":
      return n.attachments.some(isAudio);
    case "urls":
      return hasUrl(n);
  }
}

/** The text a search looks in: title, content, item text, labels, link titles and hosts, file names. */
export function searchText(n: KeepNote, labelName: (tag: string) => string): string {
  const parts = [n.title, plainText(n.content)];
  for (const i of n.items ?? []) parts.push(i.text);
  for (const t of n.tags) parts.push(labelName(t));
  for (const a of n.annotations) parts.push(a.title ?? "", a.description ?? "", hostOf(a.url));
  for (const a of n.attachments) parts.push(a.name ?? "");
  return fold(parts.join("\n"));
}

const escapeRe = (s: string): string => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * How `word` (folded) occurs in `hay` (folded): 2 as a whole word, 1 at the start of a word, 0 not at a word start.
 * Only word starts count, so "rust" finds "Rust" and "rust-lang" but not "trust" or "frustrated" (owner, 2026-10-31).
 */
export function wordMatch(hay: string, word: string): 0 | 1 | 2 {
  const start = new RegExp(`(?:^|[^\\p{L}\\p{N}])${escapeRe(word)}`, "u");
  if (!start.test(hay)) return 0;
  return new RegExp(`(?:^|[^\\p{L}\\p{N}])${escapeRe(word)}(?=$|[^\\p{L}\\p{N}])`, "u").test(hay) ? 2 : 1;
}

/**
 * How well a note matches the query: 0 when some word is missing, else higher for title and label hits and for whole
 * words. Every word must start a word somewhere in the note (title, text, items, labels, links, file names).
 */
export function queryScore(n: KeepNote, q: string, labelName: (tag: string) => string): number {
  const words = fold(q).split(/\s+/).filter(Boolean);
  if (!words.length) return 1;
  const title = fold(n.title);
  const labels = fold(n.tags.map(labelName).join("\n"));
  const hay = searchText(n, labelName);
  let score = 0;
  for (const w of words) {
    const inAll = wordMatch(hay, w);
    if (!inAll) return 0;
    score += inAll + 6 * wordMatch(title, w) + 3 * wordMatch(labels, w);
  }
  return score;
}

export function matchesQuery(n: KeepNote, q: string, labelName: (tag: string) => string): boolean {
  return queryScore(n, q, labelName) > 0;
}

/** The notes a view shows, ordered: Notes, Archive and labels by position; Reminders by time; Trash by deletion. */
export function viewNotes(all: Iterable<KeepNote>, view: KeepView): KeepNote[] {
  const list: KeepNote[] = [];
  for (const n of all) {
    switch (view.kind) {
      case "notes":
        if (!n.archived && !inTrash(n)) list.push(n);
        break;
      case "archive":
        if (n.archived && !inTrash(n)) list.push(n);
        break;
      case "trash":
        if (inTrash(n)) list.push(n);
        break;
      case "reminders":
        if (n.reminder && !inTrash(n)) list.push(n);
        break;
      case "label":
        if (n.tags.includes(view.tag) && !inTrash(n)) list.push(n);
        break;
    }
  }
  if (view.kind === "reminders") return list.sort((a, b) => Date.parse(a.reminder!.at) - Date.parse(b.reminder!.at) || byPosition(a, b));
  if (view.kind === "trash") return list.sort((a, b) => (b.deletedAt ?? "").localeCompare(a.deletedAt ?? ""));
  return list.sort(byPosition);
}

/**
 * Search across Notes, Archive and labels (never the trash). With words, the best matches come first (title and
 * label hits, whole words), pinned or not; filters alone keep the grid order.
 */
export function searchNotes(all: Iterable<KeepNote>, s: SearchState, labelName: (tag: string) => string): KeepNote[] {
  const out: { n: KeepNote; score: number }[] = [];
  const q = s.q.trim();
  for (const n of all) {
    if (inTrash(n)) continue;
    if (s.type && !matchesType(n, s.type)) continue;
    if (s.label && !n.tags.includes(s.label)) continue;
    if (s.color && n.color !== s.color) continue;
    const score = q ? queryScore(n, q, labelName) : 1;
    if (score > 0) out.push({ n, score });
  }
  return out.sort((a, b) => b.score - a.score || byPosition(a.n, b.n)).map((x) => x.n);
}

/** "Pinned" and "Others" sections; the Others heading shows only when something is pinned. */
export function splitPinned(notes: KeepNote[]): { pinned: KeepNote[]; others: KeepNote[] } {
  const pinned: KeepNote[] = [];
  const others: KeepNote[] = [];
  for (const n of notes) (n.pinned ? pinned : others).push(n);
  return { pinned, others };
}

/** The position that puts a dragged card between its new neighbours in a descending-position list. */
export function positionBetween(above: number | undefined, below: number | undefined): number {
  if (above === undefined && below === undefined) return Date.now();
  if (above === undefined) return (below as number) + 1000;
  if (below === undefined) return above - 1000;
  return (above + below) / 2;
}

/** Labels in use or registered, alphabetical in display case, like Keep's nav. */
export function sortLabels<T extends { name: string }>(labels: T[]): T[] {
  return [...labels].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
}
