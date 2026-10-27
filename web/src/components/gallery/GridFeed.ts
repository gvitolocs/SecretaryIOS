import { batch, createEffect, createSignal, on, onCleanup, untrack, type Accessor } from "solid-js";
import { listGallery, type GalleryItem, type GalleryQuery } from "../../api/gallery";

/** First page size, and the size of the pages that follow while scrolling. */
export const FIRST_PAGE = 200;
/** The page size when seeking to a month far down (the server's maximum). */
export const SEEK_PAGE = 500;

export interface GalleryFeed {
  items: Accessor<GalleryItem[]>;
  /** A request is in flight. */
  loading: Accessor<boolean>;
  /** More pages exist (the last answer had a cursor). */
  hasMore: Accessor<boolean>;
  /** The last request failed (cleared by the next try). */
  failed: Accessor<boolean>;
  /** Bumped when the item order changed in a way that is not an append (so the layout must start over). */
  epoch: Accessor<number>;
  loadMore: (limit?: number) => Promise<void>;
  /** Start again from the first page (same query). */
  reload: () => void;
  /** Swap an item for its updated copy. */
  replace: (item: GalleryItem) => void;
  /** Change several items at once (their ids, and what each becomes). */
  update: (ids: Iterable<string>, change: (item: GalleryItem) => GalleryItem) => void;
  /** Drop items by id (moved to the trash, deleted, restored: they are no longer in this list). */
  remove: (ids: Iterable<string>) => void;
}

/**
 * A cursor-paged list of the Gallery for a query. A new query starts over; a stale answer (an earlier query's page,
 * or one that arrives after a reload) is dropped.
 */
export function createGalleryFeed(query: Accessor<GalleryQuery>): GalleryFeed {
  const [items, setItems] = createSignal<GalleryItem[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [started, setStarted] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [epoch, setEpoch] = createSignal(0);
  let generation = 0;
  let ids = new Set<string>();
  let inflight: Promise<void> | null = null;

  async function fetchPage(limit: number): Promise<void> {
    const gen = generation;
    setLoading(true);
    setFailed(false);
    try {
      const q = untrack(() => ({ ...query(), cursor: cursor() ?? undefined, limit }));
      const page = await listGallery(q);
      if (gen !== generation) return;
      const fresh = page.items.filter((i) => !ids.has(i.id));
      for (const i of fresh) ids.add(i.id);
      batch(() => {
        if (fresh.length) setItems((prev) => prev.concat(fresh));
        setCursor(page.nextCursor);
        setStarted(true);
      });
    } catch {
      if (gen === generation) setFailed(true);
    } finally {
      if (gen === generation) {
        setLoading(false);
        inflight = null;
      }
    }
  }

  function loadMore(limit = FIRST_PAGE): Promise<void> {
    if (inflight) return inflight;
    if (untrack(() => started() && cursor() === null)) return Promise.resolve();
    inflight = untrack(() => fetchPage(limit));
    return inflight;
  }

  function reload(): void {
    generation++;
    inflight = null;
    ids = new Set();
    batch(() => {
      setItems([]);
      setCursor(null);
      setStarted(false);
      setFailed(false);
      setLoading(false);
      setEpoch((e) => e + 1);
    });
    void loadMore();
  }

  createEffect(
    on(
      () => JSON.stringify(query()),
      () => reload(),
    ),
  );
  onCleanup(() => {
    generation++;
  });

  return {
    items,
    loading,
    hasMore: () => !started() || cursor() !== null,
    failed,
    epoch,
    loadMore,
    reload,
    replace(item) {
      const prev = untrack(items);
      const at = prev.findIndex((x) => x.id === item.id);
      if (at < 0) return;
      const next = prev.slice();
      next[at] = item;
      batch(() => {
        setItems(next);
        // A turned picture changes the shape of its tile; nothing else moves.
        if (prev[at]!.rotation !== item.rotation) setEpoch((e) => e + 1);
      });
    },
    update(changed, change) {
      const set = new Set(changed);
      if (!set.size) return;
      setItems((prev) => prev.map((i) => (set.has(i.id) ? change(i) : i)));
    },
    remove(removed) {
      const drop = new Set(removed);
      if (!drop.size) return;
      for (const id of drop) ids.delete(id);
      batch(() => {
        setItems((prev) => prev.filter((i) => !drop.has(i.id)));
        setEpoch((e) => e + 1);
      });
    },
  };
}
