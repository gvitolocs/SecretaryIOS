// A phone-friendly mouse for the Simple view (screenshots + input batches).
//
// The Live view's Cua viewer already has this model (tap = click, 550 ms press = right click,
// drag = left-button drag, two fingers = scroll); this is the same one for Simple mode, where we
// build the events ourselves. What is missing there and added here: a two-finger tap for a right
// click, and local pinch-zoom (never sent to the desktop).
//
// The mapping:
//   tap                     → left click where the finger was
//   long press (500 ms)     → right click
//   one-finger drag         → left button down, moves, button up
//   two-finger swipe        → scroll (one event on release: the direction it went, how far)
//   two-finger tap          → right click
//   pinch                   → zoom the local picture (`onZoom`), nothing sent to the desktop
//
// Points handed to the sink are already in the sandbox's screen pixels, so this file knows
// nothing about scaling or letterboxing: the caller maps with `toScreen`.

export type TouchButton = "left" | "right";
export type ScrollDirection = "up" | "down" | "left" | "right";

export interface Point {
  x: number;
  y: number;
}

export interface TouchSink {
  click(at: Point, button: TouchButton): void;
  down(at: Point): void;
  move(at: Point): void;
  up(at: Point): void;
  scroll(at: Point, direction: ScrollDirection, amount: number): void;
}

export interface TouchMouseOptions {
  /** A press held this long is a right click. */
  longPressMs?: number;
  /** Movement (CSS px) that turns a press into a drag. */
  slopPx?: number;
  /** Zoom while pinching: a scale above 1, and the midpoint drift in CSS px. */
  onZoom?: (zoom: { scale: number; dx: number; dy: number }) => void;
  /** A click landed (for the marker): the point in CSS px, and which button. */
  onMark?: (at: { clientX: number; clientY: number }, button: TouchButton) => void;
}

export interface TouchMouse {
  down(e: PointerEvent, at: Point): void;
  move(e: PointerEvent, at: Point): void;
  up(e: PointerEvent, at: Point): void;
  cancel(): void;
  /** A drag is in progress (the desktop has the left button down). */
  dragging(): boolean;
  /** Forget every pointer, e.g. when the view goes away. */
  reset(): void;
}

const LONG_PRESS_MS = 500;
const SLOP_PX = 10;
const PINCH_RATIO = 0.12;
const SCROLL_PER_60PX = 1;

function distance(a: { clientX: number; clientY: number }, b: { clientX: number; clientY: number }): number {
  return Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
}

function idOf(e: PointerEvent): number {
  return e.pointerId ?? 0;
}

export function createTouchMouse(sink: TouchSink, options: TouchMouseOptions = {}): TouchMouse {
  const longPressMs = options.longPressMs ?? LONG_PRESS_MS;
  const slop = options.slopPx ?? SLOP_PX;

  interface Held {
    at: Point;
    clientX: number;
    clientY: number;
    startX: number;
    startY: number;
    since: number;
    timer: number | null;
    longPressed: boolean;
  }

  const held = new Map<number, Held>();
  let drag = false;
  let last: Point | null = null;
  let two: { d0: number; mid: { clientX: number; clientY: number }; moved: number; since: number } | null = null;
  let pinching = false;

  const clearTimer = (h: Held) => {
    if (h.timer !== null) window.clearTimeout(h.timer);
    h.timer = null;
  };

  const cancelLongPress = () => {
    for (const h of held.values()) clearTimer(h);
  };

  return {
    dragging: () => drag,

    down(e, at) {
      if (e.pointerType && e.pointerType !== "touch") return;
      const id = idOf(e);
      const h: Held = {
        at,
        clientX: e.clientX,
        clientY: e.clientY,
        startX: e.clientX,
        startY: e.clientY,
        since: Date.now(),
        timer: null,
        longPressed: false,
      };
      held.set(id, h);
      if (held.size === 1) {
        drag = false;
        last = null;
        h.timer = window.setTimeout(() => {
          h.longPressed = true;
          sink.click(h.at, "right");
          options.onMark?.({ clientX: h.startX, clientY: h.startY }, "right");
        }, longPressMs);
      } else if (held.size === 2) {
        cancelLongPress();
        if (drag) {
          // A second finger during a drag: finish the drag where it is.
          if (last) sink.up(last);
          drag = false;
        }
        const [a, b] = [...held.values()] as [Held, Held];
        two = {
          d0: Math.max(distance(a, b), 1),
          mid: { clientX: (a.clientX + b.clientX) / 2, clientY: (a.clientY + b.clientY) / 2 },
          moved: 0,
          since: Math.min(a.since, b.since),
        };
        pinching = false;
      }
    },

    move(e, at) {
      if (e.pointerType && e.pointerType !== "touch") return;
      const h = held.get(idOf(e));
      if (!h) return;
      h.clientX = e.clientX;
      h.clientY = e.clientY;
      if (held.size === 2 && two) {
        const [a, b] = [...held.values()] as [Held, Held];
        const d = Math.max(distance(a, b), 1);
        const mid = { clientX: (a.clientX + b.clientX) / 2, clientY: (a.clientY + b.clientY) / 2 };
        two.moved = Math.max(two.moved, distance(mid, two.mid));
        // A pinch changes the fingers' distance more than it moves their midpoint; a two-finger
        // swipe is the other way round (fingers never move in perfect step).
        if (!pinching && Math.abs(d / two.d0 - 1) > PINCH_RATIO && Math.abs(d - two.d0) > distance(mid, two.mid)) {
          pinching = true;
        }
        if (pinching) {
          options.onZoom?.({
            scale: d / two.d0,
            dx: mid.clientX - two.mid.clientX,
            dy: mid.clientY - two.mid.clientY,
          });
        }
        return;
      }
      if (held.size !== 1 || two) return;
      const travelled = Math.hypot(e.clientX - h.startX, e.clientY - h.startY);
      if (!drag && travelled > slop) {
        if (h.longPressed) return; // a long press already right-clicked
        clearTimer(h);
        drag = true;
        sink.down(h.at);
      }
      if (!drag) return;
      last = at;
      sink.move(at);
    },

    up(e, at) {
      if (e.pointerType && e.pointerType !== "touch") return;
      const id = idOf(e);
      const h = held.get(id);
      if (!h) return;
      const heldBoth = held.size === 2 ? [...held.values()] : null;
      if (held.size === 1) {
        clearTimer(h);
        if (drag) {
          drag = false;
          last = null;
          sink.up(at);
        } else if (!h.longPressed && Date.now() - h.since < longPressMs) {
          sink.click(h.at, "left");
          options.onMark?.({ clientX: h.startX, clientY: h.startY }, "left");
        }
      } else if (heldBoth && two) {
        const [a, b] = heldBoth as [Held, Held];
        const mid = { clientX: (a.clientX + b.clientX) / 2, clientY: (a.clientY + b.clientY) / 2 };
        const drift = distance(mid, two.mid);
        if (!pinching && drift < 20 && Date.now() - two.since < longPressMs) {
          // Two-finger tap: the right click Cua's own model leaves out.
          sink.click(at, "right");
          options.onMark?.({ clientX: mid.clientX, clientY: mid.clientY }, "right");
        } else if (!pinching && drift >= 20) {
          const dx = mid.clientX - two.mid.clientX;
          const dy = mid.clientY - two.mid.clientY;
          const [direction, distancePx] =
            Math.abs(dx) > Math.abs(dy)
              ? [dx > 0 ? "right" : "left", Math.abs(dx)]
              : [dy > 0 ? "down" : "up", Math.abs(dy)];
          const amount = Math.min(20, Math.max(1, Math.round(distancePx / 60) * SCROLL_PER_60PX));
          sink.scroll(at, direction as ScrollDirection, amount);
        }
      }
      held.delete(id);
      if (held.size < 2) {
        two = null;
        pinching = false;
      }
      if (held.size === 0) {
        drag = false;
        last = null;
      }
    },

    cancel() {
      if (drag) {
        drag = false;
        last = null;
      }
      for (const h of held.values()) clearTimer(h);
      held.clear();
      two = null;
      pinching = false;
    },

    reset() {
      this.cancel();
    },
  };
}
