// People in the Gallery, with no DOM: where the pages live, what the scan status says, names and birth dates, merging, the
// picker's search, and where a face box lands on the screen for the picture as the viewer places it (fit, zoom, rotation).

import type { FaceBox, FaceSettings, Person, PersonRef } from "../api/gallery";
import { PERSON_NAME_MAX } from "../api/gallery";

export const PEOPLE_PATH = "/gallery/people";
export const personPath = (id: string): string => `/gallery/person/${encodeURIComponent(id)}`;

/** The name to show for a person, or `fallback` (the caller's "Unnamed") when they have none. */
export const personName = (p: { name: string | null }, fallback: string): string => p.name?.trim() || fallback;

/** A typed name: trimmed and cut to the server's limit; null when nothing is left (the name is removed). */
export function normalizeName(text: string): string | null {
  return text.trim().slice(0, PERSON_NAME_MAX).trim() || null;
}

/** `YYYY-MM-DD` that is a real day, not in the future. */
export function isBirthDate(text: string, today: Date = new Date()): boolean {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const at = new Date(Date.UTC(y, mo - 1, d));
  if (at.getUTCFullYear() !== y || at.getUTCMonth() !== mo - 1 || at.getUTCDate() !== d) return false;
  return at.getTime() <= Date.UTC(today.getFullYear(), today.getMonth(), today.getDate());
}

// ---- the scan -----------------------------------------------------------------------------------------------------

export type ScanPhase = "off" | "done" | "scanning" | "hot" | "busy";

export interface ScanProgress {
  phase: ScanPhase;
  scanned: number;
  pending: number;
  /** 0 to 100 of the photos looked at. */
  percent: number;
}

/** What the scanning is doing: done, working, or paused because the machine is hot or busy. */
export function scanProgress(s: FaceSettings): ScanProgress {
  const total = s.scanned + s.pending;
  const percent = total === 0 ? 100 : Math.min(100, Math.floor((s.scanned / total) * 100));
  if (!s.enabled) return { phase: "off", scanned: s.scanned, pending: s.pending, percent };
  let phase: ScanPhase = "done";
  if (s.pending > 0) phase = s.pausedHot ? "hot" : s.pausedBusy ? "busy" : "scanning";
  return { phase, scanned: s.scanned, pending: s.pending, percent };
}

/** The status is worth asking for again soon: photos are still being looked at. */
export const stillScanning = (s: FaceSettings): boolean => s.enabled && (s.running || s.pending > 0);

// ---- lists --------------------------------------------------------------------------------------------------------

export function replacePerson(list: readonly Person[], person: Person): Person[] {
  return list.map((p) => (p.id === person.id ? person : p));
}

/** The people left after `ids` are gone from the list (merged away). */
export const withoutPeople = (list: readonly Person[], ids: Iterable<string>): Person[] => {
  const gone = new Set(ids);
  return list.filter((p) => !gone.has(p.id));
};

/** Who to keep by default when merging: a named person first, then the one with the most faces. */
export function defaultKeeper(chosen: readonly Person[]): string | null {
  const sorted = chosen.slice().sort((a, b) => Number(!!b.name) - Number(!!a.name) || b.faceCount - a.faceCount || (a.id < b.id ? -1 : 1));
  return sorted[0]?.id ?? null;
}

/** Merging needs two people. */
export const canMerge = (selected: ReadonlySet<string>): boolean => selected.size >= 2;

/** The people the picker offers for a search: names containing the words (case and accents ignored); all when empty. */
export function searchPeople(people: readonly Person[], query: string): Person[] {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (!words.length) return people.slice();
  return people.filter((p) => {
    const name = fold(p.name ?? "");
    return words.every((w) => name.includes(w));
  });
}

function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/** Names for the chips in the info panel: named first, in the order given. */
export const chipLabel = (p: PersonRef, fallback: string): string => p.name?.trim() || fallback;

// ---- face boxes on the screen -------------------------------------------------------------------------------------

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type Turn = 0 | 90 | 180 | 270;

/**
 * The turn applied to a picture by `rotate(Ndeg)`, and the box the viewer drew it in (its `left`/`top`/`width`/`height`),
 * read back from the style the viewer hands to overlays; null while the picture is not placed yet.
 */
export function placedFromStyle(style: Record<string, string | number | undefined> | undefined): { rect: Rect; turn: Turn } | null {
  if (!style) return null;
  const px = (v: string | number | undefined): number | null => {
    if (typeof v === "number") return Number.isFinite(v) ? v : null;
    const m = typeof v === "string" ? /^(-?\d+(?:\.\d+)?)px$/.exec(v.trim()) : null;
    return m ? Number(m[1]) : null;
  };
  const x = px(style.left);
  const y = px(style.top);
  const w = px(style.width);
  const h = px(style.height);
  if (x === null || y === null || w === null || h === null || w <= 0 || h <= 0) return null;
  const rotate = typeof style.transform === "string" ? /rotate\((-?\d+(?:\.\d+)?)deg\)/.exec(style.transform) : null;
  const degrees = rotate ? ((Math.round(Number(rotate[1]) / 90) * 90) % 360 + 360) % 360 : 0;
  return { rect: { x, y, w, h }, turn: degrees as Turn };
}

/**
 * Where a face box (fractions of the unturned picture) lands on the screen. `placed` is the box the unturned picture was
 * drawn in and `turn` the clockwise rotation about its centre (the viewer draws a turned picture that way), so the box
 * follows the fit, the zoom, the pan and the rotation. The result is the axis-aligned rectangle.
 */
export function boxToScreen(box: FaceBox, placed: Rect, turn: Turn): Rect {
  const cx = placed.x + placed.w / 2;
  const cy = placed.y + placed.h / 2;
  const rad = (turn * Math.PI) / 180;
  const cos = Math.round(Math.cos(rad));
  const sin = Math.round(Math.sin(rad));
  const corners: [number, number][] = [
    [box.x, box.y],
    [box.x + box.w, box.y],
    [box.x, box.y + box.h],
    [box.x + box.w, box.y + box.h],
  ].map(([u, v]) => {
    const dx = placed.x + u! * placed.w - cx;
    const dy = placed.y + v! * placed.h - cy;
    return [cx + dx * cos - dy * sin, cy + dx * sin + dy * cos] as [number, number];
  });
  const xs = corners.map((c) => c[0]);
  const ys = corners.map((c) => c[1]);
  const left = Math.min(...xs);
  const top = Math.min(...ys);
  return { x: left, y: top, w: Math.max(...xs) - left, h: Math.max(...ys) - top };
}
