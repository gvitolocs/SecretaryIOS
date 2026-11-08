import { createSignal, type Accessor } from "solid-js";
import { HOLD_MS, heldTooFar, LIVE_IDLE, liveStep, prefersReducedMotion, triggerAllowed, type LiveEvent, type LiveState, type LiveTrigger } from "../../../lib/galleryLive";

export interface LiveController {
  state: Accessor<LiveState>;
  /** Ask for the motion (ignored while a clip is going, and for a hover when the OS wants less motion). */
  press: (trigger: LiveTrigger) => void;
  /** The button, the hold or the key was let go. */
  release: (trigger: LiveTrigger) => void;
  /** The player's own events: ready, ended, failed, faded, reset. */
  dispatch: (event: Exclude<LiveEvent, { type: "press" | "release" }>) => void;
}

/** The state of one viewer's Live Photo playback, driven by the triggers and by the clip's own events. */
export function createLive(reducedMotion: () => boolean = prefersReducedMotion): LiveController {
  const [state, setState] = createSignal<LiveState>(LIVE_IDLE);
  const step = (event: LiveEvent) => setState((s) => liveStep(s, event));
  return {
    state,
    press(trigger) {
      if (triggerAllowed(trigger, reducedMotion())) step({ type: "press", trigger });
    },
    release: (trigger) => step({ type: "release", trigger }),
    dispatch: step,
  };
}

/**
 * Holding the picture: a press on the viewer's stage that stays put for HOLD_MS calls `onHold`; letting go calls
 * `onRelease`. Moving (a pan or a swipe), a second finger or a cancelled pointer ends it without a hold. Returns a
 * function that removes the listeners.
 */
export function trackHold(
  doc: Document,
  opts: { enabled: () => boolean; onHold: () => void; onRelease: () => void; holdMs?: number },
): () => void {
  let pointer: number | null = null;
  let startX = 0;
  let startY = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let held = false;

  const end = () => {
    clearTimeout(timer);
    timer = undefined;
    const wasHeld = held;
    held = false;
    pointer = null;
    if (wasHeld) opts.onRelease();
  };
  const down = (e: PointerEvent) => {
    if (pointer !== null) {
      // A second finger: this is a pinch, not a hold.
      end();
      return;
    }
    if (e.button > 0 || !opts.enabled() || !(e.target instanceof Element) || !e.target.closest(".pv-stage")) return;
    pointer = e.pointerId;
    startX = e.clientX;
    startY = e.clientY;
    timer = setTimeout(() => {
      held = true;
      opts.onHold();
    }, opts.holdMs ?? HOLD_MS);
  };
  const move = (e: PointerEvent) => {
    if (e.pointerId !== pointer || held) return;
    if (heldTooFar(e.clientX - startX, e.clientY - startY)) end();
  };
  const up = (e: PointerEvent) => {
    if (e.pointerId === pointer) end();
  };
  doc.addEventListener("pointerdown", down, true);
  doc.addEventListener("pointermove", move, true);
  doc.addEventListener("pointerup", up, true);
  doc.addEventListener("pointercancel", up, true);
  return () => {
    doc.removeEventListener("pointerdown", down, true);
    doc.removeEventListener("pointermove", move, true);
    doc.removeEventListener("pointerup", up, true);
    doc.removeEventListener("pointercancel", up, true);
    clearTimeout(timer);
    if (held) opts.onRelease();
  };
}
