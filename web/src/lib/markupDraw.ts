// The Markup tab's drawing rules as pure functions: which width a "thin" pen is, how a dragged pointer becomes a stroke
// (points closer than a couple of pixels are dropped), and how a spot on the shown picture maps to the frame the
// marks are stored in (the crop box moves, the marks stay on the picture).

import { MAX_MARKS, MAX_POINTS, type Crop, type Mark, type Point } from "./editList";

export type Tool = "pen" | "highlighter" | "arrow" | "text";
export type Size = "thin" | "medium" | "thick";

export const TOOLS: Tool[] = ["pen", "highlighter", "arrow", "text"];
export const SIZES: Size[] = ["thin", "medium", "thick"];
export const COLORS = ["#ffffff", "#000000", "#ff3b30", "#ff9500", "#ffcc00", "#34c759", "#0a84ff", "#bf5af2"];

/** Widths and text sizes are fractions of the frame's width (edit.rs). */
const WIDTHS: Record<Tool, Record<Size, number>> = {
  pen: { thin: 0.004, medium: 0.008, thick: 0.016 },
  highlighter: { thin: 0.02, medium: 0.035, thick: 0.06 },
  arrow: { thin: 0.005, medium: 0.009, thick: 0.015 },
  text: { thin: 0.035, medium: 0.055, thick: 0.085 },
};

export function sizeOf(tool: Tool, size: Size): number {
  return WIDTHS[tool][size];
}

/** The Rust renderer's reading of a crop box: a spot (u, v) on the shown picture (0..1 each) in frame fractions. */
export function toFrame(u: number, v: number, crop: Crop | null): Point {
  const c = crop ?? { x: 0, y: 0, w: 1, h: 1 };
  return [c.x + u * c.w, c.y + v * c.h];
}

/** How many marks and points the list holds, against the server's limits. */
export function room(marks: Mark[]): { marks: boolean; points: boolean } {
  const points = marks.reduce((n, m) => n + (m.t === "pen" || m.t === "highlighter" ? m.points.length : m.t === "arrow" ? 2 : 0), 0);
  return { marks: marks.length < MAX_MARKS, points: points < MAX_POINTS - 1 };
}

/** A new mark under the pointer. */
export function startMark(tool: Exclude<Tool, "text">, color: string, size: Size, at: Point): Mark {
  const width = sizeOf(tool, size);
  return tool === "arrow" ? { t: "arrow", color, width, from: at, to: at } : { t: tool, color, width, points: [at] };
}

/**
 * The mark with the pointer at `at`. Strokes gain a point unless it is less than `minDistance` (fractions of the
 * frame width, the same unit as the marks) from the last one; an arrow moves its tip.
 */
export function extendMark(mark: Mark, at: Point, minDistance: number): Mark {
  if (mark.t === "arrow") return { ...mark, to: at };
  if (mark.t !== "pen" && mark.t !== "highlighter") return mark;
  const last = mark.points[mark.points.length - 1];
  if (last && Math.hypot(at[0] - last[0], at[1] - last[1]) < minDistance) return mark;
  return { ...mark, points: [...mark.points, at] };
}

export function textMark(color: string, size: Size, at: Point, text: string): Mark | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  return { t: "text", color, size: sizeOf("text", size), x: at[0], y: at[1], text: trimmed.slice(0, 300) };
}
