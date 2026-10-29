import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HOLD_MS } from "../../../lib/galleryLive";
import { createLive, trackHold } from "./liveController";

const phase = (live: ReturnType<typeof createLive>) => live.state().phase;

describe("createLive", () => {
  it("runs the states from the triggers and the clip's events", () =>
    createRoot((dispose) => {
      const live = createLive(() => false);
      live.press("button");
      expect(phase(live)).toBe("loading");
      live.dispatch({ type: "ready" });
      expect(phase(live)).toBe("playing");
      live.dispatch({ type: "ended" });
      expect(phase(live)).toBe("fading");
      live.dispatch({ type: "faded" });
      expect(phase(live)).toBe("idle");
      dispose();
    }));

  it("ignores a hover when the OS asks for less motion", () =>
    createRoot((dispose) => {
      const live = createLive(() => true);
      live.press("hover");
      expect(phase(live)).toBe("idle");
      live.press("hold");
      expect(phase(live)).toBe("loading");
      dispose();
    }));

  it("a new picture resets it", () =>
    createRoot((dispose) => {
      const live = createLive(() => false);
      live.press("key");
      live.dispatch({ type: "ready" });
      live.dispatch({ type: "reset" });
      expect(phase(live)).toBe("idle");
      dispose();
    }));
});

describe("trackHold", () => {
  let stage: HTMLDivElement;
  let outside: HTMLDivElement;
  let enabled = true;
  const hold = vi.fn();
  const release = vi.fn();
  let stop: () => void;

  const ev = (type: string, target: Element, init: { id?: number; x?: number; y?: number; button?: number } = {}) => {
    const e = new Event(type, { bubbles: true, cancelable: true }) as Event & Record<string, unknown>;
    Object.assign(e, { pointerId: init.id ?? 1, clientX: init.x ?? 100, clientY: init.y ?? 100, button: init.button ?? 0 });
    target.dispatchEvent(e);
  };

  beforeEach(() => {
    vi.useFakeTimers();
    enabled = true;
    hold.mockReset();
    release.mockReset();
    document.body.innerHTML = '<div class="pv-stage"><img /></div><div class="other"></div>';
    stage = document.querySelector<HTMLDivElement>(".pv-stage")!;
    outside = document.querySelector<HTMLDivElement>(".other")!;
    stop = trackHold(document, { enabled: () => enabled, onHold: hold, onRelease: release });
  });
  afterEach(() => {
    stop();
    vi.useRealTimers();
    document.body.innerHTML = "";
  });

  it("holds after HOLD_MS on the stage and releases when the pointer is let go", () => {
    ev("pointerdown", stage.querySelector("img")!);
    vi.advanceTimersByTime(HOLD_MS - 1);
    expect(hold).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(hold).toHaveBeenCalledTimes(1);
    expect(release).not.toHaveBeenCalled();
    ev("pointerup", stage);
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("a tap is not a hold", () => {
    ev("pointerdown", stage);
    vi.advanceTimersByTime(100);
    ev("pointerup", stage);
    vi.advanceTimersByTime(1000);
    expect(hold).not.toHaveBeenCalled();
    expect(release).not.toHaveBeenCalled();
  });

  it("moving more than a few pixels (a pan or a swipe) is not a hold", () => {
    ev("pointerdown", stage, { x: 100, y: 100 });
    vi.advanceTimersByTime(200);
    ev("pointermove", stage, { x: 130, y: 100 });
    vi.advanceTimersByTime(1000);
    expect(hold).not.toHaveBeenCalled();
  });

  it("a second finger (a pinch) cancels it", () => {
    ev("pointerdown", stage, { id: 1 });
    ev("pointerdown", stage, { id: 2 });
    vi.advanceTimersByTime(1000);
    expect(hold).not.toHaveBeenCalled();
  });

  it("ignores presses outside the stage, secondary buttons and a disabled state", () => {
    ev("pointerdown", outside);
    ev("pointerup", outside);
    ev("pointerdown", stage, { button: 2 });
    ev("pointerup", stage, { button: 2 });
    enabled = false;
    ev("pointerdown", stage);
    vi.advanceTimersByTime(1000);
    expect(hold).not.toHaveBeenCalled();
  });

  it("releases a hold in progress when it is removed", () => {
    ev("pointerdown", stage);
    vi.advanceTimersByTime(HOLD_MS);
    stop();
    expect(release).toHaveBeenCalledTimes(1);
  });
});
