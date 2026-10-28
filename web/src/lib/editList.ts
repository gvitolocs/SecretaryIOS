// The photo editor's edit list: the JSON the Rust core renders (crates/secretary-image/src/edit.rs, "v": 1) and the
// server stores per item. This file has the types, the neutral list, the checks the UI needs (is it still the
// original? what did a quarter turn do to the crop box?) and the parser for what the server sends back.
// The units are the Rust ones: exposure in EV, every other adjustment -100..100 (fade 0..100), crop and markup in
// fractions of the straightened frame.

export const EDIT_VERSION = 1;

export interface Crop {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Adjust {
  exposure: number;
  contrast: number;
  highlights: number;
  shadows: number;
  saturation: number;
  warmth: number;
  tint: number;
  vignette: number;
  fade: number;
}

export type AdjustKey = keyof Adjust;

export const FILTER_NAMES = ["vivid", "warm", "cool", "bw", "noir", "fade", "vintage", "dramatic"] as const;
export type FilterName = (typeof FILTER_NAMES)[number];

export interface FilterRef {
  name: FilterName;
  strength: number;
}

export type Point = [number, number];

export type Mark =
  | { t: "pen"; color: string; width: number; points: Point[] }
  | { t: "highlighter"; color: string; width: number; points: Point[] }
  | { t: "arrow"; color: string; width: number; from: Point; to: Point }
  | { t: "text"; color: string; size: number; x: number; y: number; text: string };

export type Quarter = 0 | 1 | 2 | 3;

export interface EditList {
  v: typeof EDIT_VERSION;
  /** Quarter turns clockwise. */
  rotate: Quarter;
  /** Degrees, positive turns the picture clockwise. */
  straighten: number;
  crop: Crop | null;
  adjust: Adjust;
  filter: FilterRef | null;
  markup: Mark[];
}

/** The slider controls: what the Adjust tab shows, in order. `exposure` is in stops, the rest in percent. */
export const ADJUST_CONTROLS: { key: AdjustKey; min: number; max: number; step: number }[] = [
  { key: "exposure", min: -3, max: 3, step: 0.05 },
  { key: "contrast", min: -100, max: 100, step: 1 },
  { key: "highlights", min: -100, max: 100, step: 1 },
  { key: "shadows", min: -100, max: 100, step: 1 },
  { key: "saturation", min: -100, max: 100, step: 1 },
  { key: "warmth", min: -100, max: 100, step: 1 },
  { key: "tint", min: -100, max: 100, step: 1 },
  { key: "vignette", min: -100, max: 100, step: 1 },
  { key: "fade", min: 0, max: 100, step: 1 },
];

export const MAX_STRAIGHTEN = 45;
/** The most marks and points the server takes (kept in step with edit.rs). */
export const MAX_MARKS = 400;
export const MAX_POINTS = 40_000;

export function neutralAdjust(): Adjust {
  return { exposure: 0, contrast: 0, highlights: 0, shadows: 0, saturation: 0, warmth: 0, tint: 0, vignette: 0, fade: 0 };
}

export function emptyEdit(): EditList {
  return { v: EDIT_VERSION, rotate: 0, straighten: 0, crop: null, adjust: neutralAdjust(), filter: null, markup: [] };
}

export function isFullCrop(c: Crop | null): boolean {
  return c === null || (c.x === 0 && c.y === 0 && c.w === 1 && c.h === 1);
}

/** True when the list changes nothing (the server treats such a save as a revert). */
export function isIdentity(list: EditList): boolean {
  return (
    list.rotate === 0 &&
    Math.abs(list.straighten) < 0.001 &&
    isFullCrop(list.crop) &&
    Object.values(list.adjust).every((v) => v === 0) &&
    (list.filter === null || list.filter.strength <= 0) &&
    list.markup.length === 0
  );
}

/** Structural equality (lists are plain JSON). */
export function sameEdit(a: EditList, b: EditList): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function cloneEdit(list: EditList): EditList {
  return structuredClone(list);
}

export function setAdjust(list: EditList, key: AdjustKey, value: number): EditList {
  return { ...list, adjust: { ...list.adjust, [key]: value } };
}

export function setStraighten(list: EditList, degrees: number): EditList {
  return { ...list, straighten: Math.max(-MAX_STRAIGHTEN, Math.min(MAX_STRAIGHTEN, degrees)) };
}

export function setFilter(list: EditList, name: FilterName | null, strength = 1): EditList {
  return { ...list, filter: name ? { name, strength } : null };
}

/** `rotate` turned by `dir` quarter turns (1 = clockwise), with the crop box carried along. */
export function rotateEdit(list: EditList, dir: 1 | -1): EditList {
  const rotate = ((list.rotate + dir + 4) % 4) as Quarter;
  return { ...list, rotate, crop: list.crop ? rotateCrop(list.crop, dir) : null };
}

/** The same box after the picture turned a quarter clockwise (`dir` 1) or counter-clockwise (-1). */
export function rotateCrop(c: Crop, dir: 1 | -1): Crop {
  return dir === 1 ? { x: 1 - c.y - c.h, y: c.x, w: c.h, h: c.w } : { x: c.y, y: 1 - c.x - c.w, w: c.h, h: c.w };
}

/** Round the noise off slider values so the stored list is short and stable. */
export function tidy(list: EditList): EditList {
  const r = (v: number, d: number) => Math.round(v * 10 ** d) / 10 ** d;
  return {
    ...list,
    straighten: r(list.straighten, 2),
    crop: list.crop && !isFullCrop(list.crop) ? { x: r(list.crop.x, 4), y: r(list.crop.y, 4), w: r(list.crop.w, 4), h: r(list.crop.h, 4) } : null,
    adjust: Object.fromEntries(Object.entries(list.adjust).map(([k, v]) => [k, r(v, 2)])) as unknown as Adjust,
  };
}

// ---- reading what the server stores --------------------------------------------------------------------------------

type Raw = Record<string, unknown>;
const obj = (v: unknown): Raw => (v && typeof v === "object" && !Array.isArray(v) ? (v as Raw) : {});
const num = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? v : d);
const point = (v: unknown): Point | null =>
  Array.isArray(v) && v.length === 2 && v.every((n) => typeof n === "number" && Number.isFinite(n)) ? [v[0] as number, v[1] as number] : null;

function parseMark(raw: unknown): Mark | null {
  const m = obj(raw);
  const color = typeof m.color === "string" ? m.color : "#ffffff";
  switch (m.t) {
    case "pen":
    case "highlighter": {
      const points = (Array.isArray(m.points) ? m.points : []).map(point).filter((p): p is Point => p !== null);
      return points.length ? { t: m.t, color, width: num(m.width, 0.008), points } : null;
    }
    case "arrow": {
      const from = point(m.from);
      const to = point(m.to);
      return from && to ? { t: "arrow", color, width: num(m.width, 0.008), from, to } : null;
    }
    case "text":
      return typeof m.text === "string" ? { t: "text", color, size: num(m.size, 0.055), x: num(m.x), y: num(m.y), text: m.text } : null;
    default:
      return null;
  }
}

/** A stored list as an `EditList`; anything unusable (wrong version, not an object) gives null. */
export function parseEditList(raw: unknown): EditList | null {
  const r = obj(raw);
  if (r.v !== EDIT_VERSION) return null;
  const rotate = num(r.rotate);
  const crop = obj(r.crop);
  const adjust = obj(r.adjust);
  const filter = obj(r.filter);
  const base = neutralAdjust();
  return {
    v: EDIT_VERSION,
    rotate: (Number.isInteger(rotate) && rotate >= 0 && rotate <= 3 ? rotate : 0) as Quarter,
    straighten: num(r.straighten),
    crop: r.crop ? { x: num(crop.x), y: num(crop.y), w: num(crop.w, 1), h: num(crop.h, 1) } : null,
    adjust: Object.fromEntries(Object.keys(base).map((k) => [k, num(adjust[k])])) as unknown as Adjust,
    filter: (FILTER_NAMES as readonly string[]).includes(filter.name as string) ? { name: filter.name as FilterName, strength: num(filter.strength, 1) } : null,
    markup: (Array.isArray(r.markup) ? r.markup : []).map(parseMark).filter((m): m is Mark => m !== null),
  };
}

/** What the item's own rotation (the viewer's R key, drawn by CSS) means as quarter turns. */
export function quartersOf(rotation: number): Quarter {
  return ((((Math.round(rotation / 90) % 4) + 4) % 4) as Quarter);
}

// ---- sizes -------------------------------------------------------------------------------------------------------

/** The scale (<= 1) at which a w x h picture turned by `degrees` still covers a centred rectangle of its own shape. */
export function inscribedScale(w: number, h: number, degrees: number): number {
  const theta = (Math.abs(degrees) * Math.PI) / 180;
  const s = Math.sin(theta);
  const c = Math.cos(theta);
  return Math.min(1, w / (w * c + h * s), h / (w * s + h * c));
}

/**
 * The straightened frame of a `w` x `h` picture (the space the crop box lives in): the quarter turns, then the
 * largest same-shaped rectangle that fits inside the straightened picture. The same numbers as `layout` in edit.rs.
 */
export function frameSize(w: number, h: number, rotate: Quarter, straighten: number): { width: number; height: number } {
  const [tw, th] = rotate % 2 === 1 ? [h, w] : [w, h];
  if (Math.abs(straighten) < 0.001) return { width: tw, height: th };
  const s = inscribedScale(tw, th, straighten);
  return { width: Math.max(1, Math.round(tw * s)), height: Math.max(1, Math.round(th * s)) };
}
