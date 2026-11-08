import "@testing-library/jest-dom/vitest";
import { cleanup } from "@solidjs/testing-library";
import { afterEach, vi } from "vitest";
import { clearChatDB } from "../lib/chatdb";
import { pageCache } from "../lib/pageCache";
import { reloadLocale } from "../i18n";

afterEach(async () => {
  await clearChatDB();
  cleanup();
  await pageCache.clear();
  vi.unstubAllGlobals();
  vi.useRealTimers();
  try {
    sessionStorage.clear();
    localStorage.clear();
  } catch {
    // ignore
  }
  // Back to the device language (jsdom: en-US) with no stored choice.
  reloadLocale();
});

// jsdom lacks these; the chat view uses them for scrolling.
if (typeof window !== "undefined") {
  window.requestAnimationFrame ??= (cb: FrameRequestCallback) => setTimeout(() => cb(Date.now()), 0) as unknown as number;
  if (!Element.prototype.scrollTo) Element.prototype.scrollTo = function () {};
  window.matchMedia ??= ((q: string) =>
    ({ matches: false, media: q, addEventListener() {}, removeEventListener() {} }) as unknown as MediaQueryList);
}
