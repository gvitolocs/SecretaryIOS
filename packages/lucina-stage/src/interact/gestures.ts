/**
 * Pointer gestures for the stage, without the DOM (the controller feeds it pointer events):
 *
 * - one finger or the left mouse button: **rotate** (after a 6 px slop);
 * - two fingers: **pinch** (distance ratio) and, when both move the same way (dot of the
 *   normalised deltas ≥ 0.8, Custom Cast's test), **pan** by the midpoint;
 * - right/middle mouse button, or shift + left: **pan**;
 * - a short press that barely moves: **tap** (the controller decides single/double and what was hit);
 * - **press** / **release**: a pointer is down on the stage (her eyes follow it).
 *
 * After two fingers, the gesture ends only when every finger is up: lifting one finger never
 * turns a pinch into a rotate jump.
 */

export interface PointerSample {
  id: number;
  x: number;
  y: number;
  /** ms */
  t: number;
  type: 'mouse' | 'touch' | 'pen' | string;
  /** 0 left, 1 middle, 2 right. */
  button?: number;
  shift?: boolean;
}

export interface GestureHandlers {
  rotate?(dx: number, dy: number): void;
  pan?(dx: number, dy: number): void;
  pinch?(ratio: number, cx: number, cy: number): void;
  tap?(x: number, y: number, t: number, type: string): void;
  /** The first pointer went down (`x`, `y`), or the gaze point moved. */
  press?(x: number, y: number): void;
  /** Every pointer is up. `gestured`: it was a drag/pinch, not a tap. */
  release?(gestured: boolean): void;
  /** Begin of a drag/pinch (stop the glide). */
  grab?(): void;
}

export const TAP_SLOP = 10;
export const DRAG_SLOP = 6;
export const TAP_MAX_MS = 350;
export const PARALLEL_DOT = 0.8;

interface Tracked {
  id: number;
  x: number;
  y: number;
  sx: number;
  sy: number;
  t0: number;
  type: string;
  mode: 'rotate' | 'pan';
}

export class GestureTracker {
  private readonly pointers = new Map<number, Tracked>();
  /** The current gesture has had two fingers (or moved past the slop). */
  private multi = false;
  private dragging = false;
  private tapCandidate = true;
  private readonly lastDelta = new Map<number, { dx: number; dy: number; t: number }>();

  constructor(private readonly h: GestureHandlers) {}

  get active(): boolean {
    return this.pointers.size > 0;
  }

  get count(): number {
    return this.pointers.size;
  }

  down(p: PointerSample): void {
    if (this.pointers.has(p.id)) return;
    const mode: Tracked['mode'] = p.type === 'mouse' && (p.button === 1 || p.button === 2 || p.shift) ? 'pan' : 'rotate';
    const first = this.pointers.size === 0;
    this.pointers.set(p.id, { id: p.id, x: p.x, y: p.y, sx: p.x, sy: p.y, t0: p.t, type: p.type, mode });
    if (first) {
      this.multi = false;
      this.dragging = false;
      this.tapCandidate = true;
      this.h.grab?.();
      this.h.press?.(p.x, p.y);
    } else {
      // A second finger: this is a pinch/pan now, never a tap.
      this.multi = true;
      this.tapCandidate = false;
      this.h.grab?.();
    }
  }

  move(p: PointerSample): void {
    const cur = this.pointers.get(p.id);
    if (!cur) return;
    if (this.pointers.size >= 2) {
      const others = [...this.pointers.values()].filter((q) => q.id !== p.id);
      const o = others[0]!;
      const prevD = Math.hypot(cur.x - o.x, cur.y - o.y);
      const nextD = Math.hypot(p.x - o.x, p.y - o.y);
      // The other finger has not moved in this event: the midpoint moves by half of this one's delta.
      const dx = p.x - cur.x;
      const dy = p.y - cur.y;
      cur.x = p.x;
      cur.y = p.y;
      if (prevD > 4 && nextD > 4) this.h.pinch?.(nextD / prevD, (p.x + o.x) / 2, (p.y + o.y) / 2);
      // Pan when both fingers recently moved the same way.
      const od = this.lastDelta.get(o.id);
      if (od && Math.abs(p.t - od.t) < 80) {
        const m1 = Math.hypot(dx, dy);
        const m2 = Math.hypot(od.dx, od.dy);
        if (m1 > 0.5 && m2 > 0.5 && (dx * od.dx + dy * od.dy) / (m1 * m2) >= PARALLEL_DOT) this.h.pan?.(dx / 2, dy / 2);
      }
      this.lastDelta.set(p.id, { dx, dy, t: p.t });
      return;
    }
    if (this.multi) {
      // One finger left after a pinch: hold still until it lifts.
      cur.x = p.x;
      cur.y = p.y;
      return;
    }
    const dx = p.x - cur.x;
    const dy = p.y - cur.y;
    if (!this.dragging) {
      if (Math.hypot(p.x - cur.sx, p.y - cur.sy) < DRAG_SLOP) return;
      this.dragging = true;
      this.tapCandidate = false;
      // Start from the slop point so the view does not jump.
    }
    cur.x = p.x;
    cur.y = p.y;
    if (cur.mode === 'pan') this.h.pan?.(dx, dy);
    else this.h.rotate?.(dx, dy);
    this.h.press?.(p.x, p.y);
  }

  up(p: PointerSample): void {
    const cur = this.pointers.get(p.id);
    if (!cur) return;
    this.pointers.delete(p.id);
    this.lastDelta.delete(p.id);
    if (this.pointers.size > 0) return;
    const moved = Math.hypot(p.x - cur.sx, p.y - cur.sy);
    const isTap = this.tapCandidate && !this.multi && moved <= TAP_SLOP && p.t - cur.t0 <= TAP_MAX_MS;
    const gestured = this.dragging || this.multi;
    this.multi = false;
    this.dragging = false;
    if (isTap) this.h.tap?.(p.x, p.y, p.t, cur.type);
    this.h.release?.(gestured);
  }

  /** The browser took the pointer (scroll, system gesture): end it without a tap. */
  cancel(id: number): void {
    if (!this.pointers.delete(id)) return;
    this.lastDelta.delete(id);
    this.tapCandidate = false;
    if (this.pointers.size === 0) {
      const gestured = this.dragging || this.multi;
      this.multi = false;
      this.dragging = false;
      this.h.release?.(gestured);
    }
  }

  reset(): void {
    const had = this.pointers.size > 0;
    this.pointers.clear();
    this.lastDelta.clear();
    this.multi = false;
    this.dragging = false;
    if (had) this.h.release?.(true);
  }
}

/** Double-tap detection: two taps within `ms` and `px` of each other. */
export class DoubleTap {
  private last: { x: number; y: number; t: number } | null = null;

  constructor(
    readonly ms = 300,
    readonly px = 28,
  ) {}

  /** Returns true when this tap completes a double-tap (and forgets it). */
  tap(x: number, y: number, t: number): boolean {
    const l = this.last;
    if (l && t - l.t <= this.ms && Math.hypot(x - l.x, y - l.y) <= this.px) {
      this.last = null;
      return true;
    }
    this.last = { x, y, t };
    return false;
  }

  clear(): void {
    this.last = null;
  }
}
