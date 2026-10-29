import { createSignal } from "solid-js";
import { isAdultOff, type PixivItem, type PixivListing } from "../api/pixiv";
import { ApiError } from "../api/client";
import { t } from "../i18n";

// A paged list of works (ranking, search): `start` begins a new list with a page loader, `more` asks for the
// next page. A slow answer for an earlier list is dropped, and a work never appears twice.

export type Loader = (page: number) => Promise<PixivListing>;

/** What a failed load says on screen. */
export function feedMessage(e: unknown): string {
  if (isAdultOff(e)) return t("pixiv.adultOff");
  if (e instanceof ApiError && e.status === 429) return t("pixiv.slowDown", { seconds: e.retryAfterSeconds ?? 30 });
  return t("pixiv.failed");
}

/** What a search's popular order turned out to be (`sort.used`); date orders say nothing. */
const SORT_NOTE: Record<string, () => string> = {
  pixiv_popular_preview: () => t("pixiv.sortPopularPreview"),
  pixiv_popular: () => t("pixiv.sortPopular"),
  users_tags: () => t("pixiv.sortUsersTags"),
  date_d_fallback: () => t("pixiv.sortFallback"),
};

/** A label for the sort the service actually used; unknown orders stay quiet. */
export function sortMessage(used: string): string { return SORT_NOTE[used]?.() ?? ""; }

export function createFeed() {
  const [items, setItems] = createSignal<PixivItem[]>([]);
  const [hasMore, setHasMore] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [started, setStarted] = createSignal(false);
  // The newest page's notice and sort, so the screen can say why the list looks as it does.
  const [notice, setNotice] = createSignal<string | null>(null);
  const [sort, setSort] = createSignal<PixivListing["sort"]>(null);
  let loader: Loader | null = null;
  let page = 0;
  let generation = 0;

  async function next(): Promise<void> {
    if (!loader || loading()) return;
    const mine = generation;
    const run = loader;
    setLoading(true);
    setError("");
    try {
      const listing = await run(page + 1);
      if (mine !== generation) return;
      page = listing.page || page + 1;
      setHasMore(listing.hasMore);
      setNotice(listing.notice);
      setSort(listing.sort);
      setItems((old) => {
        const seen = new Set(old.map((i) => i.id));
        return [...old, ...listing.items.filter((i) => !seen.has(i.id))];
      });
    } catch (e) {
      if (mine !== generation) return;
      setError(feedMessage(e));
      setHasMore(false);
    } finally {
      if (mine === generation) setLoading(false);
    }
  }

  return {
    items,
    hasMore,
    loading,
    error,
    started,
    notice,
    sort,
    /** Begin a new list (or `null` to empty it). */
    start(l: Loader | null): Promise<void> {
      generation += 1;
      loader = l;
      page = 0;
      setItems([]);
      setHasMore(false);
      setError("");
      setNotice(null);
      setSort(null);
      setLoading(false);
      setStarted(l !== null);
      return l ? next() : Promise.resolve();
    },
    more: next,
    /** Retry after an error. */
    retry: next,
    /** Patch one work in place (a bookmark toggled in the reader). */
    patch(id: string, change: Partial<PixivItem>) {
      setItems((old) => old.map((i) => (i.id === id ? { ...i, ...change } : i)));
    },
  };
}

export type Feed = ReturnType<typeof createFeed>;
