import { createSignal, onCleanup, type Accessor } from "solid-js";

/** Reactive `matchMedia(query).matches`; call inside a component or root. */
export function createMediaQuery(query: string): Accessor<boolean> {
  let mql: MediaQueryList | null = null;
  try {
    mql = typeof matchMedia === "function" ? matchMedia(query) : null;
  } catch {
    mql = null;
  }
  const [matches, setMatches] = createSignal(!!mql?.matches);
  if (mql && typeof mql.addEventListener === "function") {
    const list = mql;
    const onChange = (e: MediaQueryListEvent) => setMatches(e.matches);
    list.addEventListener("change", onChange);
    onCleanup(() => list.removeEventListener("change", onChange));
  }
  return matches;
}

/** The desktop layout: Lucina gets a column of her own on the right. */
export const WIDE_QUERY = "(min-width: 1024px)";
