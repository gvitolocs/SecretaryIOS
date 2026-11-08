import { describe, expect, it } from "vitest";
import { FADE_MS, HOLD_SLOP, LIVE_IDLE, fadeMs, heldTooFar, isLive, liveStep, motionVisible, stopsOnRelease, triggerAllowed, type LiveEvent, type LiveState } from "./galleryLive";

const run = (events: LiveEvent[], from: LiveState = LIVE_IDLE) => events.reduce(liveStep, from);

describe("isLive", () => {
  it("needs a photo with a pair", () => {
    expect(isLive({ kind: "photo", livePair: "m1" })).toBe(true);
    expect(isLive({ kind: "photo", livePair: null })).toBe(false);
    expect(isLive({ kind: "video", livePair: "m1" })).toBe(false);
    expect(isLive(undefined)).toBe(false);
  });
});

describe("the play and fade states", () => {
  it("plays once and fades back to the still", () => {
    let s = run([{ type: "press", trigger: "button" }]);
    expect(s).toEqual({ phase: "loading", trigger: "button" });
    expect(motionVisible(s)).toBe(false);
    s = liveStep(s, { type: "ready" });
    expect(s.phase).toBe("playing");
    expect(motionVisible(s)).toBe(true);
    s = liveStep(s, { type: "ended" });
    expect(s.phase).toBe("fading");
    expect(motionVisible(s)).toBe(false);
    expect(liveStep(s, { type: "faded" })).toEqual(LIVE_IDLE);
  });

  it("a press on the button plays to the end even if it is let go", () => {
    const s = run([{ type: "press", trigger: "button" }, { type: "ready" }, { type: "release", trigger: "button" }]);
    expect(s.phase).toBe("playing");
    expect(stopsOnRelease("button")).toBe(false);
  });

  it("holding stops when let go: before the clip is ready it just cancels, while it plays it fades", () => {
    expect(run([{ type: "press", trigger: "hold" }, { type: "release", trigger: "hold" }])).toEqual(LIVE_IDLE);
    expect(run([{ type: "press", trigger: "hold" }, { type: "ready" }, { type: "release", trigger: "hold" }]).phase).toBe("fading");
    expect(run([{ type: "press", trigger: "key" }, { type: "ready" }, { type: "release", trigger: "key" }]).phase).toBe("fading");
    expect(run([{ type: "press", trigger: "hover" }, { type: "ready" }, { type: "release", trigger: "hover" }]).phase).toBe("fading");
  });

  it("only the trigger that started the clip can stop it", () => {
    const s = run([{ type: "press", trigger: "hold" }, { type: "ready" }, { type: "release", trigger: "key" }]);
    expect(s.phase).toBe("playing");
  });

  it("a click while a hover is playing lets it play out", () => {
    const s = run([{ type: "press", trigger: "hover" }, { type: "ready" }, { type: "press", trigger: "button" }, { type: "release", trigger: "hover" }]);
    expect(s).toEqual({ phase: "playing", trigger: "button" });
  });

  it("ignores presses while a clip is going and events out of order", () => {
    const playing = run([{ type: "press", trigger: "key" }, { type: "ready" }]);
    expect(liveStep(playing, { type: "press", trigger: "hold" })).toBe(playing);
    expect(liveStep(LIVE_IDLE, { type: "ready" })).toBe(LIVE_IDLE);
    expect(liveStep(LIVE_IDLE, { type: "ended" })).toBe(LIVE_IDLE);
    expect(liveStep(playing, { type: "faded" })).toBe(playing);
  });

  it("a clip that fails or a new picture returns to the still", () => {
    expect(run([{ type: "press", trigger: "button" }, { type: "failed" }])).toEqual(LIVE_IDLE);
    expect(run([{ type: "press", trigger: "button" }, { type: "ready" }, { type: "reset" }])).toEqual(LIVE_IDLE);
  });
});

describe("reduced motion and holding", () => {
  it("does not start from a hover when the OS wants less motion, but does from a deliberate request", () => {
    expect(triggerAllowed("hover", true)).toBe(false);
    for (const t of ["button", "hold", "key"] as const) expect(triggerAllowed(t, true)).toBe(true);
    expect(triggerAllowed("hover", false)).toBe(true);
  });

  it("fades at once with reduced motion", () => {
    expect(fadeMs(true)).toBe(0);
    expect(fadeMs(false)).toBe(FADE_MS);
  });

  it("a hold that moves more than the slop is a drag", () => {
    expect(heldTooFar(HOLD_SLOP, 0)).toBe(false);
    expect(heldTooFar(6, 6)).toBe(true);
  });
});
