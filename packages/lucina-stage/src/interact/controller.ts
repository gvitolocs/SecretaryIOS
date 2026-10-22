/**
 * Wires the stage canvas to the orbit, the gestures and the touch reactions.
 *
 * - Listens only on the canvas (pointer events + wheel), which gets `touch-action: none`: touches
 *   that start on her never scroll or zoom the page; everything else on the page scrolls as usual.
 *   The wheel is taken only while the pointer is over the canvas.
 * - Taps on her body react at once (so pats can follow quickly); a double-tap anywhere else resets
 *   the view (Custom Cast's double-tap reset).
 * - While a pointer is down, `gaze` is its position: her eyes and head follow it, and ease back to
 *   the camera 0.6 s after release.
 */
import { DoubleTap, GestureTracker } from './gestures';
import type { Orbit } from './orbit';

export interface InteractionDeps {
  canvas: HTMLCanvasElement;
  orbit: Orbit;
  /** Body tap at client coordinates; returns true when it hit her (and reacted). */
  touch(clientX: number, clientY: number): boolean;
  /** Something changed: make sure the render loop runs. */
  kick(): void;
}

export interface InteractionController {
  /** Client coordinates her gaze should follow, or null (camera). */
  readonly gaze: { x: number; y: number } | null;
  /** A pointer is down on the stage. */
  readonly pressed: boolean;
  enabled: boolean;
  update(dt: number): void;
  dispose(): void;
}

export const GAZE_RELEASE_MS = 600;

export function createInteraction(d: InteractionDeps): InteractionController {
  const { canvas, orbit } = d;
  const dbl = new DoubleTap(320, 32);
  let enabled = true;
  let gaze: { x: number; y: number } | null = null;
  let gazeUntil = 0;
  let pressed = false;

  const size = () => {
    const r = canvas.getBoundingClientRect();
    return { w: r.width || canvas.clientWidth || 1, h: r.height || canvas.clientHeight || 1 };
  };

  const tracker = new GestureTracker({
    grab: () => orbit.grab(),
    rotate: (dx, dy) => {
      orbit.rotate(dx, dy, size().w);
      d.kick();
    },
    pan: (dx, dy) => {
      orbit.pan(dx, dy, size().h);
      d.kick();
    },
    pinch: (ratio) => {
      orbit.pinch(ratio);
      d.kick();
    },
    press: (x, y) => {
      pressed = true;
      gaze = { x, y };
      gazeUntil = 0;
    },
    release: () => {
      pressed = false;
      orbit.release();
      gazeUntil = performance.now() + GAZE_RELEASE_MS;
      d.kick();
    },
    tap: (x, y, t) => {
      if (d.touch(x, y)) {
        dbl.clear();
        return;
      }
      if (dbl.tap(x, y, t)) orbit.reset(true);
      d.kick();
    },
  });

  const sample = (e: PointerEvent) => ({ id: e.pointerId, x: e.clientX, y: e.clientY, t: e.timeStamp || performance.now(), type: e.pointerType, button: e.button, shift: e.shiftKey });

  const onDown = (e: PointerEvent): void => {
    if (!enabled) return;
    if (e.pointerType === 'mouse' && e.button > 2) return;
    try {
      canvas.setPointerCapture(e.pointerId);
    } catch {
      /* not capturable (synthetic events) */
    }
    tracker.down(sample(e));
    if (e.pointerType !== 'mouse') e.preventDefault();
  };
  const onMove = (e: PointerEvent): void => {
    if (!enabled || !tracker.active) return;
    // Coalesced events give the full path on fast drags; one per frame is enough here.
    tracker.move(sample(e));
  };
  const onUp = (e: PointerEvent): void => {
    if (!tracker.active) return;
    tracker.up(sample(e));
  };
  const onCancel = (e: PointerEvent): void => tracker.cancel(e.pointerId);
  const onWheel = (e: WheelEvent): void => {
    if (!enabled) return;
    e.preventDefault();
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? size().h : 1;
    // Trackpad pinch arrives as ctrl+wheel with small deltas: make it as strong as a real pinch.
    const k = e.ctrlKey ? 0.01 : 0.0018;
    orbit.zoomBy(-e.deltaY * unit * k);
    d.kick();
  };
  const onContext = (e: Event): void => {
    if (enabled) e.preventDefault();
  };
  const onDblClick = (e: Event): void => e.preventDefault();

  const style = canvas.style;
  const saved = { touchAction: style.touchAction, userSelect: style.userSelect, callout: style.getPropertyValue('-webkit-touch-callout') };
  style.touchAction = 'none';
  style.userSelect = 'none';
  style.setProperty('-webkit-user-select', 'none');
  style.setProperty('-webkit-touch-callout', 'none');
  style.setProperty('-webkit-tap-highlight-color', 'transparent');

  canvas.addEventListener('pointerdown', onDown);
  canvas.addEventListener('pointermove', onMove);
  canvas.addEventListener('pointerup', onUp);
  canvas.addEventListener('pointercancel', onCancel);
  canvas.addEventListener('lostpointercapture', onCancel);
  canvas.addEventListener('wheel', onWheel, { passive: false });
  canvas.addEventListener('contextmenu', onContext);
  canvas.addEventListener('dblclick', onDblClick);

  return {
    get gaze() {
      if (gaze && !pressed && gazeUntil && performance.now() > gazeUntil) {
        gaze = null;
        gazeUntil = 0;
      }
      return gaze;
    },
    get pressed() {
      return pressed;
    },
    get enabled() {
      return enabled;
    },
    set enabled(v: boolean) {
      enabled = !!v;
      if (!enabled) tracker.reset();
    },
    update(dt) {
      orbit.update(dt);
    },
    dispose() {
      tracker.reset();
      canvas.removeEventListener('pointerdown', onDown);
      canvas.removeEventListener('pointermove', onMove);
      canvas.removeEventListener('pointerup', onUp);
      canvas.removeEventListener('pointercancel', onCancel);
      canvas.removeEventListener('lostpointercapture', onCancel);
      canvas.removeEventListener('wheel', onWheel);
      canvas.removeEventListener('contextmenu', onContext);
      canvas.removeEventListener('dblclick', onDblClick);
      style.touchAction = saved.touchAction;
      style.userSelect = saved.userSelect;
      style.setProperty('-webkit-touch-callout', saved.callout);
    },
  };
}
