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
  function track(el: HTMLElement) {
    if (typeof ResizeObserver !== "function") return;
    observer?.disconnect();
    observer = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width ?? 0;
      setWide(width >= WIDE_MIN_PX);
    });
    observer.observe(el);
  }
  return { wide, track };
}
