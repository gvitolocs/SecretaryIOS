import { createSignal, onCleanup } from "solid-js";

/** Content width from which a page switches to its desktop layout (styles/page.css uses the same 880 px). */
export const WIDE_MIN_PX = 880;

/**
 * Whether the element given to `track` is desktop-wide. Measures the page's
 * own slot (the shell's rail and Lucina's dock take part of the window), and
 * stays false where ResizeObserver is missing (jsdom), so tests see the phone
 * layout unless they stub it (test/wide.ts).
 */
export function useWide() {
  const [wide, setWide] = createSignal(false);
  let observer: ResizeObserver | undefined;
  onCleanup(() => observer?.disconnect());
  function observe(el: HTMLElement, width: (entry: ResizeObserverEntry) => number) {
    if (typeof ResizeObserver !== "function") return;
    observer?.disconnect();
    observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      const w = entry ? width(entry) : 0;
      // 0 is a page a Suspense boundary took off the screen, not a narrow one. Switching layout would rebuild it, and
      // what it loads again would suspend it again, for ever.
      if (w > 0) setWide(w >= WIDE_MIN_PX);
    });
    observer.observe(el);
  }
  function track(el: HTMLElement) {
    observe(el, (entry) => entry.contentRect.width);
  }
  /**
   * Measure the route slot the element sits in (its parent) instead of the
   * element itself: for pages whose phone column is capped (max-width), which
   * would otherwise never measure wide. The parent is read on every resize,
   * since a page inside a Suspense boundary is attached after its ref runs.
   */
  function trackSlot(el: HTMLElement) {
    observe(el, (entry) => Math.max(entry.contentRect.width, el.parentElement?.getBoundingClientRect().width ?? 0));
  }
  return { wide, track, trackSlot };
}
