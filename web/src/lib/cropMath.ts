// Crop box geometry for the editor's Crop tab, as pure functions. A box is {x, y, w, h} in fractions of the
// straightened frame (the same numbers the edit list stores). The frame is not square, so a fixed aspect ratio
// is a ratio of pixels: `aspect` is width / height in pixels and `frameAspect` the frame's width / height; in
// fractions a box of that shape has w / h = aspect / frameAspect.

import type { Crop } from "./editList";

export type Handle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "move";

/** The smallest side of a box, as a fraction of the frame. */
export const MIN_SIDE = 0.05;

export const FULL: Crop = { x: 0, y: 0, w: 1, h: 1 };

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** The ratio presets of the Crop tab: `null` is free, "original" the frame's own shape. */
export const ASPECTS = [
  { id: "free", ratio: null },
  { id: "original", ratio: "original" },
  { id: "square", ratio: 1 },
  { id: "4:3", ratio: 4 / 3 },
  { id: "3:4", ratio: 3 / 4 },
  { id: "3:2", ratio: 3 / 2 },
  { id: "2:3", ratio: 2 / 3 },
  { id: "16:9", ratio: 16 / 9 },
  { id: "9:16", ratio: 9 / 16 },
] as const;

export type AspectId = (typeof ASPECTS)[number]["id"];

/** The pixel ratio of a preset for a frame, or null for free. */
export function aspectRatio(id: AspectId, frameAspect: number): number | null {
  const found = ASPECTS.find((a) => a.id === id);
  if (!found || found.ratio === null) return null;
  return found.ratio === "original" ? frameAspect : found.ratio;
}

export function moveCrop(c: Crop, dx: number, dy: number): Crop {
  return { ...c, x: clamp(c.x + dx, 0, 1 - c.w), y: clamp(c.y + dy, 0, 1 - c.h) };
}

/**
 * The largest box of the given pixel ratio inside `c`, around the same centre (the whole frame when `c` is
 * null). With a ratio of null the box is kept.
 */
export function fitAspect(c: Crop | null, aspect: number | null, frameAspect: number): Crop {
  const box = c ?? FULL;
  if (aspect === null) return box;
  const r = aspect / frameAspect; // w / h in fractions
  const w = Math.min(box.w, box.h * r);
  const h = w / r;
  const cx = box.x + box.w / 2;
  const cy = box.y + box.h / 2;
  return { x: clamp(cx - w / 2, 0, 1 - w), y: clamp(cy - h / 2, 0, 1 - h), w, h };
}

/**
 * Drag `handle` by (dx, dy) (fractions of the frame). The opposite side or corner stays put; the box never leaves
 * the frame or gets thinner than [`MIN_SIDE`]; with an `aspect` the shape is kept.
 */
export function resizeCrop(c: Crop, handle: Handle, dx: number, dy: number, aspect: number | null, frameAspect: number): Crop {
  if (handle === "move") return moveCrop(c, dx, dy);
  const r = aspect === null ? null : aspect / frameAspect;
  // Edges of the box after the drag, free.
  let left = c.x;
  let right = c.x + c.w;
  let top = c.y;
  let bottom = c.y + c.h;
  if (handle.includes("w")) left = clamp(c.x + dx, 0, right - MIN_SIDE);
  if (handle.includes("e")) right = clamp(c.x + c.w + dx, left + MIN_SIDE, 1);
  if (handle.includes("n")) top = clamp(c.y + dy, 0, bottom - MIN_SIDE);
  if (handle.includes("s")) bottom = clamp(c.y + c.h + dy, top + MIN_SIDE, 1);
  if (r === null) return { x: left, y: top, w: right - left, h: bottom - top };

  const corner = handle.length === 2;
  const horizontal = handle === "e" || handle === "w";
  // The fixed point: the opposite corner, or the middle of the opposite edge.
  const anchorX = handle.includes("w") ? c.x + c.w : handle.includes("e") ? c.x : c.x + c.w / 2;
  const anchorY = handle.includes("n") ? c.y + c.h : handle.includes("s") ? c.y : c.y + c.h / 2;
  let w = right - left;
  let h = bottom - top;
  if (corner) {
    // Follow whichever side moved more, relative to its size.
    if (Math.abs(w - c.w) / c.w >= Math.abs(h - c.h) / c.h) h = w / r;
    else w = h * r;
  } else if (horizontal) {
    h = w / r;
  } else {
    w = h * r;
  }
  // How far the box may grow from the anchor in each direction.
  const room = (dir: number, a: number, centred: boolean) => (centred ? 2 * Math.min(a, 1 - a) : dir < 0 ? a : 1 - a);
  const maxW = room(handle.includes("w") ? -1 : 1, anchorX, !handle.includes("w") && !handle.includes("e"));
  const maxH = room(handle.includes("n") ? -1 : 1, anchorY, !handle.includes("n") && !handle.includes("s"));
  const scale = Math.min(1, maxW / w, maxH / h);
  w *= scale;
  h *= scale;
  if (w < MIN_SIDE || h < MIN_SIDE) {
    const up = Math.max(MIN_SIDE / w, MIN_SIDE / h);
    w *= up;
    h *= up;
  }
  const x = handle.includes("w") ? anchorX - w : handle.includes("e") ? anchorX : anchorX - w / 2;
  const y = handle.includes("n") ? anchorY - h : handle.includes("s") ? anchorY : anchorY - h / 2;
  return { x: clamp(x, 0, 1 - w), y: clamp(y, 0, 1 - h), w, h };
}

/** A box where `p` (fractions of the frame) sits on the box's edge or inside it: which handle, if any. */
export function hitHandle(c: Crop, px: number, py: number, reach: number): Handle | null {
  const near = (a: number, b: number) => Math.abs(a - b) <= reach;
  const left = near(px, c.x);
  const right = near(px, c.x + c.w);
  const top = near(py, c.y);
  const bottom = near(py, c.y + c.h);
  const insideX = px >= c.x - reach && px <= c.x + c.w + reach;
  const insideY = py >= c.y - reach && py <= c.y + c.h + reach;
  if (top && left) return "nw";
  if (top && right) return "ne";
  if (bottom && left) return "sw";
  if (bottom && right) return "se";
  if (top && insideX) return "n";
  if (bottom && insideX) return "s";
  if (left && insideY) return "w";
  if (right && insideY) return "e";
  if (px > c.x && px < c.x + c.w && py > c.y && py < c.y + c.h) return "move";
  return null;
}

/** The size an image of `w`×`h` takes inside `maxW`×`maxH` while keeping its shape (never bigger than 1:1 when `fill` is false). */
export function fitBox(maxW: number, maxH: number, w: number, h: number): { width: number; height: number } {
  if (w <= 0 || h <= 0 || maxW <= 0 || maxH <= 0) return { width: 0, height: 0 };
  const s = Math.min(maxW / w, maxH / h);
  return { width: Math.floor(w * s), height: Math.floor(h * s) };
}
