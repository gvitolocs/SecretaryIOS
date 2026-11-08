import { vi } from "vitest";

/** A ResizeObserver that reports every observed element as `width` wide. */
function observerOf(width: number) {
  return class {
    constructor(private cb: ResizeObserverCallback) {}
    observe() {
      this.cb([{ contentRect: { width } } as ResizeObserverEntry], this as unknown as ResizeObserver);
    }
    unobserve() {}
    disconnect() {}
  };
}

/** Make every page measure as desktop-wide (useWide); undo with vi.unstubAllGlobals(). */
export function stubWide(width = 1200): void {
  vi.stubGlobal("ResizeObserver", observerOf(width));
}
