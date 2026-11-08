// Live Photos in the viewer, with no DOM: which pictures are live, and the small state machine behind the LIVE button,
// holding the picture and the L key. The motion half is played muted, once, laid exactly over the still, then faded out.
// components/gallery/live/LiveVideo.tsx draws it; GalleryViewer wires the triggers.

import type { GalleryItem } from "../api/gallery";

/** How long a press on the picture lasts before it counts as holding it. */
export const HOLD_MS = 450;
/** A hold that moves more than this many pixels is a drag (pan or swipe), not a hold. */
export const HOLD_SLOP = 8;
/** The motion fades back into the still over this long. */
export const FADE_MS = 240;

export function isLive(item: Pick<GalleryItem, "kind" | "livePair"> | null | undefined): item is Pick<GalleryItem, "kind" | "livePair"> & { livePair: string } {
  return !!item && item.kind === "photo" && !!item.livePair;
}

/** What asked for the motion: the LIVE button, the mouse over it, a hold on the picture, or the L key. */
export type LiveTrigger = "button" | "hover" | "hold" | "key";

/** A press on the button plays to the end; the others play for as long as they last. */
export const stopsOnRelease = (trigger: LiveTrigger): boolean => trigger !== "button";

/** Hovering is not a deliberate request: with reduced motion only a press, a hold or a key starts it. */
export const triggerAllowed = (trigger: LiveTrigger, reducedMotion: boolean): boolean => !(reducedMotion && trigger === "hover");

export const fadeMs = (reducedMotion: boolean): number => (reducedMotion ? 0 : FADE_MS);

export const heldTooFar = (dx: number, dy: number): boolean => Math.hypot(dx, dy) > HOLD_SLOP;

export type LivePhase = "idle" | "loading" | "playing" | "fading";

export interface LiveState {
  phase: LivePhase;
  trigger: LiveTrigger | null;
}

export const LIVE_IDLE: LiveState = { phase: "idle", trigger: null };

export type LiveEvent =
  | { type: "press"; trigger: LiveTrigger }
  | { type: "release"; trigger: LiveTrigger }
  /** The first frame is on screen and the clip plays. */
  | { type: "ready" }
  | { type: "ended" }
  | { type: "failed" }
  /** The fade is over. */
  | { type: "faded" }
  /** Another picture is shown, or the viewer closed. */
  | { type: "reset" };

/** The next state. Presses while a clip is going are ignored, except that a press on the button lets a hover play out. */
export function liveStep(state: LiveState, event: LiveEvent): LiveState {
  switch (event.type) {
    case "press":
      if (state.phase === "idle") return { phase: "loading", trigger: event.trigger };
      if ((state.phase === "loading" || state.phase === "playing") && event.trigger === "button" && state.trigger && stopsOnRelease(state.trigger)) {
        return { ...state, trigger: "button" };
      }
      return state;
    case "release":
      // Only the trigger that started it can stop it (letting go of L must not cut a clip the button started).
      if (state.trigger !== event.trigger || !stopsOnRelease(event.trigger)) return state;
      if (state.phase === "loading") return LIVE_IDLE;
      if (state.phase === "playing") return { ...state, phase: "fading" };
      return state;
    case "ready":
      return state.phase === "loading" ? { ...state, phase: "playing" } : state;
    case "ended":
      return state.phase === "playing" ? { ...state, phase: "fading" } : state;
    case "faded":
      return state.phase === "fading" ? LIVE_IDLE : state;
    case "failed":
    case "reset":
      return LIVE_IDLE;
  }
}

/** The motion is visible only while it plays. */
export const motionVisible = (state: LiveState): boolean => state.phase === "playing";

/** True when the OS asks for less motion (false where `matchMedia` is missing). */
export function prefersReducedMotion(): boolean {
  try {
    return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}
