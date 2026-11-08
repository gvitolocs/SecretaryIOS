// The Pixiv reader's zoom math (owner, 2026-10-29: "on mac i can only zoom the image and it's already zoomed").
// A page opens in Fit: the whole image inside the stage, letterboxed, never enlarged past its own pixels. The zoom goes
// from Fit up to 400 % of the image's pixels; "Fit width" fills the stage's width for tall pages (manga, long works) and
// scrolls vertically. Pan offsets are the image centre's distance from the stage centre, in screen pixels, clamped so
// a zoomed image never shows a gap on an axis where it is larger than the stage.

export interface Size {
  w: number;
  h: number;
}

export interface Pan {
  x: number;
  y: number;
}

/** The largest zoom: 400 % of the image's own pixels. */
export const MAX_ZOOM = 4;
/** One zoom step of the buttons and keys. */
export const ZOOM_STEP = 1.25;

const usable = (s: Size | null | undefined): s is Size => !!s && s.w > 0 && s.h > 0;

/** The whole image inside the stage, at most `cap` times its own size (1: never enlarged). */
export function fitScale(img: Size | null | undefined, view: Size | null | undefined, cap = 1): number {
  if (!usable(img) || !usable(view)) return 1;
  return Math.min(view.w / img.w, view.h / img.h, cap);
}

/** The image as wide as the stage (at most its own size); taller pages then scroll. */
export function widthScale(img: Size | null | undefined, view: Size | null | undefined): number {
  if (!usable(img) || !usable(view)) return 1;
  return Math.min(view.w / img.w, 1);
}

/** A portrait page much taller than the stage at its width (manga, long works): worth offering "Fit width". */
export function isTall(img: Size | null | undefined, view: Size | null | undefined): boolean {
  if (!usable(img) || !usable(view)) return false;
  return (
    img.h >= img.w * 1.3 &&
    img.h * widthScale(img, view) > view.h * 1.25 &&
    widthScale(img, view) > fitScale(img, view) * 1.1
  );
}

/** How far the image may move at `scale` on each axis (0 where it fits). */
export function panLimit(img: Size, view: Size, scale: number): Pan {
  return { x: Math.max(0, (img.w * scale - view.w) / 2), y: Math.max(0, (img.h * scale - view.h) / 2) };
}

export function clampPan(pan: Pan, img: Size | null | undefined, view: Size | null | undefined, scale: number): Pan {
  if (!usable(img) || !usable(view)) return { x: 0, y: 0 };
  const l = panLimit(img, view, scale);
  // `|| 0` turns -0 into 0.
  return { x: Math.min(l.x, Math.max(-l.x, pan.x)) || 0, y: Math.min(l.y, Math.max(-l.y, pan.y)) || 0 };
}

/** Whether the image at `scale` is larger than the stage on some axis (dragging and scrolling then pan). */
export function pannable(img: Size | null | undefined, view: Size | null | undefined, scale: number): boolean {
  if (!usable(img) || !usable(view)) return false;
  const l = panLimit(img, view, scale);
  return l.x > 0.5 || l.y > 0.5;
}

/**
 * The pan that keeps the image point under `at` (relative to the stage centre) in place while the scale goes from
 * `from` to `to`.
 */
export function zoomAround(pan: Pan, at: Pan, from: number, to: number): Pan {
  if (from <= 0) return pan;
  const k = to / from;
  return { x: at.x - (at.x - pan.x) * k, y: at.y - (at.y - pan.y) * k };
}

/** A zoom clamped to [min, MAX_ZOOM]; within 1 % of min snaps to it. */
export function clampZoom(scale: number, min: number): number {
  const lo = Math.min(min, MAX_ZOOM);
  const s = Math.min(MAX_ZOOM, Math.max(lo, scale));
  return Math.abs(s - lo) / lo < 0.01 ? lo : s;
}

/** One button or key step from `scale` (up or down), between Fit (`min`) and MAX_ZOOM. */
export function stepZoom(scale: number, up: boolean, min: number): number {
  return clampZoom(up ? scale * ZOOM_STEP : scale / ZOOM_STEP, min);
}

/** Wheel (⌘/ctrl + scroll, or a trackpad pinch) to a zoom factor; one mouse notch is about one step. */
export function wheelFactor(deltaY: number): number {
  return Math.exp(-Math.max(-50, Math.min(50, deltaY)) * 0.0045);
}

/** The pan that shows the top of the page (Fit width starts at the top of a tall page). */
export function topPan(img: Size | null | undefined, view: Size | null | undefined, scale: number): Pan {
  if (!usable(img) || !usable(view)) return { x: 0, y: 0 };
  return { x: 0, y: panLimit(img, view, scale).y };
}

/** The label of the zoom button: "Fit" (given) at Fit, else the scale as a percentage of the image's pixels. */
export function zoomLabel(atFit: boolean, scale: number, fitText: string): string {
  return atFit ? fitText : `${Math.round(scale * 100)}%`;
}
