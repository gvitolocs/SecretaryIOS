import { createEffect, on, onCleanup, type Accessor } from "solid-js";
import { pageCache } from "./pageCache";

// Back lands where the page was: its scroll offset in the shell's scroller (main.content), kept in memory per route
// key while the app runs (pageCache's snapshots, cleared with it). The offset is noted on scroll, not on leaving: by
// then the next page may already have reset the scroller.

function scroller(): HTMLElement | null {
  const main = document.querySelector<HTMLElement>("main.content");
  if (main && main.scrollHeight > main.clientHeight) return main;
  return main ?? (document.scrollingElement as HTMLElement | null);
}

/**
 * Remembers the scroll offset under `key` (it must start with the page's cache scope, e.g. "instagram:"), and puts it
 * back once `ready` says the cached content is on screen.
 */
export function rememberScroll(key: Accessor<string>, ready: Accessor<boolean>): void {
  createEffect(
    on(key, (current) => {
      const el = scroller();
      if (!el) return;
      const slot = `${current}:scroll`;
      const note = () => pageCache.snapshots.set(slot, el.scrollTop);
      let restored = false;
      let frame = 0;
      createEffect(() => {
        if (restored || !ready()) return;
        restored = true;
        const y = pageCache.snapshots.get(slot);
        if (typeof y === "number" && y > 0) frame = requestAnimationFrame(() => (el.scrollTop = y));
      });
      el.addEventListener("scroll", note, { passive: true });
      onCleanup(() => {
        cancelAnimationFrame(frame);
        el.removeEventListener("scroll", note);
      });
    }),
  );
}
