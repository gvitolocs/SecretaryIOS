import "@testing-library/jest-dom/vitest";
import { cleanup } from "@solidjs/testing-library";
import { afterEach, vi } from "vitest";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
  try {
    sessionStorage.clear();
    localStorage.clear();
  } catch {
    // ignore
  }
});

// jsdom lacks these; the chat view uses them for scrolling.
if (typeof window !== "undefined") {
  window.requestAnimationFrame ??= (cb: FrameRequestCallback) => setTimeout(() => cb(Date.now()), 0) as unknown as number;
  if (!Element.prototype.scrollTo) Element.prototype.scrollTo = function () {};
  window.matchMedia ??= ((q: string) =>
    ({ matches: false, media: q, addEventListener() {}, removeEventListener() {} }) as unknown as MediaQueryList);
}
