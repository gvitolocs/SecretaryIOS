import { createEffect, createSignal, on, onCleanup, untrack, type Accessor } from "solid-js";
import { pageCache, type PageScope } from "./pageCache";

// Stale-while-revalidate over lib/pageCache for one value (a profile, a work, a notebook): the page's last copy at
// once (from memory on the first frame, else from IndexedDB a moment later) while the server is asked, then the
// server's answer, which replaces it and is stored. A failed refresh keeps the copy on screen; only a page with nothing
// to show reports the error. The paged lists (Instagram's feeds, lib/pixivFeed) do the same with their pages.

export interface Cached<T> {
  value: Accessor<T | undefined>;
  /** A copy shows while the server is asked. */
  updating: Accessor<boolean>;
  /** The server is asked and there is nothing to show yet. */
  loading: Accessor<boolean>;
  /** The last answer failed with nothing to show. */
  error: Accessor<unknown>;
  /** Asks the server again (the copy stays meanwhile). */
  refresh(): Promise<void>;
  /** A local change (a bookmark toggled): shown and stored. */
  set(value: T): void;
}

/** `key` null: nothing to load (the value is undefined). Each new key starts from its own copy. */
export function createCached<T>(scope: PageScope, key: Accessor<string | null>, fetch: (key: string) => Promise<T>): Cached<T> {
  const first = untrack(key);
  const [value, setValue] = createSignal<T | undefined>(first ? pageCache.peek<T>(first)?.value : undefined);
  const [updating, setUpdating] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<unknown>(null);
  let generation = 0;
  // The key whose value is on screen: a refresh of it keeps the value (even when its copy was just cleared).
  let shown = first;

  async function load(k: string | null): Promise<void> {
    const mine = ++generation;
    const revision = pageCache.revision(scope);
    const stale = () => mine !== generation || revision !== pageCache.revision(scope);
    setError(null);
    if (!k) {
      shown = null;
      setValue(undefined);
      setUpdating(false);
      setLoading(false);
      return;
    }
    // The request starts at once; the stored copy is read meanwhile.
    const answer = fetch(k).then(
      (fresh) => ({ fresh }),
      (failure: unknown) => ({ failure }),
    );
    const hit = pageCache.peek<T>(k);
    if (hit) setValue(() => hit.value);
    else if (k !== shown) setValue(undefined);
    shown = k;
    if (value() === undefined) {
      setLoading(true);
      const stored = await pageCache.get<T>(k);
      if (stale()) return;
      if (stored) {
        setValue(() => stored.value);
        setLoading(false);
      }
    }
    setUpdating(value() !== undefined);
    const result = await answer;
    if (stale()) return;
    if ("fresh" in result) {
      setValue(() => result.fresh);
      void pageCache.put(k, scope, result.fresh);
    } else if (value() === undefined) {
      setError(result.failure);
    }
    setLoading(false);
    setUpdating(false);
  }

  createEffect(on(key, (k) => void load(k)));
  onCleanup(() => {
    generation += 1;
  });

  return {
    value,
    updating,
    loading,
    error,
    refresh: () => load(untrack(key)),
    set(next: T) {
      const k = untrack(key);
      setValue(() => next);
      if (k) void pageCache.put(k, scope, next);
    },
  };
}
