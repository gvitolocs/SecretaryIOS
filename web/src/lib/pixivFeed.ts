import { createSignal } from "solid-js";
import { isAdultOff, type PixivItem, type PixivListing } from "../api/pixiv";
import { ApiError } from "../api/client";
import { t } from "../i18n";
import { mergeFirstPage, pageCache } from "./pageCache";

// A paged list of works (ranking, search): `start` begins a new list with a page loader, `more` asks for the
// next page. A slow answer for an earlier list is dropped, and a work never appears twice. With a cache key a list
// opens on its last copy and refreshes its first page in the background (stale-while-revalidate).

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

/** What a list keeps in lib/pageCache: its works so far and its paging, so "more" goes on after a cached start. */
interface FeedCopy {
  items: PixivItem[];
  page: number;
  hasMore: boolean;
  notice: string | null;
  sort: PixivListing["sort"];
}

export function createFeed() {
  const [items, setItems] = createSignal<PixivItem[]>([]);
  const [hasMore, setHasMore] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  // A cached list shows while its first page is asked again.
  const [updating, setUpdating] = createSignal(false);
  const [error, setError] = createSignal("");
  const [started, setStarted] = createSignal(false);
  // The newest page's notice and sort, so the screen can say why the list looks as it does.
  const [notice, setNotice] = createSignal<string | null>(null);
  const [sort, setSort] = createSignal<PixivListing["sort"]>(null);
  let loader: Loader | null = null;
  let page = 0;
  let generation = 0;
  let cacheKey: string | undefined;
  let revision = 0;

  const current = (mine: number) => mine === generation && revision === pageCache.revision("pixiv");
  function store() {
    if (!cacheKey || revision !== pageCache.revision("pixiv")) return;
    const copy: FeedCopy = { items: items().slice(0, 300), page, hasMore: hasMore(), notice: notice(), sort: sort() };
    void pageCache.put(cacheKey, "pixiv", copy);
  }
  function show(copy: FeedCopy) {
    setItems(copy.items);
    page = copy.page;
    setHasMore(copy.hasMore);
    setNotice(copy.notice);
    setSort(copy.sort);
  }

  async function next(): Promise<void> {
    if (!loader || loading() || updating()) return;
    const mine = generation;
    const run = loader;
    setLoading(true);
    setError("");
    try {
      const listing = await run(page + 1);
      if (!current(mine)) return;
      page = listing.page || page + 1;
      setHasMore(listing.hasMore);
      setNotice(listing.notice);
      setSort(listing.sort);
      setItems((old) => {
        const seen = new Set(old.map((i) => i.id));
        return [...old, ...listing.items.filter((i) => !seen.has(i.id))];
      });
      store();
    } catch (e) {
      if (mine !== generation) return;
      setError(feedMessage(e));
      setHasMore(false);
    } finally {
      if (mine === generation) setLoading(false);
    }
  }

  type Answer = { listing: PixivListing } | { failure: unknown };
  const ask = (run: Loader, n: number): Promise<Answer> =>
    run(n).then(
      (listing) => ({ listing }),
      (failure: unknown) => ({ failure }),
    );

  /** A list with a copy on screen: its first page again, merged in place; paging goes on from the copy. */
  async function refresh(mine: number, answer: Promise<Answer>): Promise<void> {
    setUpdating(true);
    const result = await answer;
    if (mine !== generation) return;
    setUpdating(false);
    // A failed refresh keeps the copy on screen.
    if (!current(mine) || "failure" in result) return;
    setItems((old) => mergeFirstPage(old, result.listing.items));
    setNotice(result.listing.notice);
    setSort(result.listing.sort);
    store();
  }

  /** A list with no copy: its first page, asked while the store was read. */
  async function firstPage(mine: number, answer: Promise<Answer>): Promise<void> {
    const result = await answer;
    if (mine !== generation) return;
    setLoading(false);
    if ("failure" in result) {
      setError(feedMessage(result.failure));
      setHasMore(false);
      return;
    }
    if (!current(mine)) return;
    page = result.listing.page || 1;
    setHasMore(result.listing.hasMore);
    setNotice(result.listing.notice);
    setSort(result.listing.sort);
    setItems(result.listing.items);
    store();
  }

  return {
    items,
    hasMore,
    loading,
    updating,
    error,
    started,
    notice,
    sort,
    /**
     * Begin a new list (or `null` to empty it). With `key` it opens on its copy in lib/pageCache (memory at once, else
     * IndexedDB a moment later) and refreshes it in the background; without a copy it loads as before.
     */
    async start(l: Loader | null, key?: string): Promise<void> {
      const mine = ++generation;
      loader = l;
      cacheKey = l ? key : undefined;
      revision = pageCache.revision("pixiv");
      page = 0;
      setItems([]);
      setHasMore(false);
      setError("");
      setNotice(null);
      setSort(null);
      setLoading(false);
      setUpdating(false);
      setStarted(l !== null);
      if (!l) return;
      if (!key) return next();
      const inMemory = pageCache.peek<FeedCopy>(key)?.value;
      if (inMemory) {
        show(inMemory);
        return refresh(mine, ask(l, 1));
      }
      // The first page is asked at once; the stored copy, if any, shows meanwhile.
      setLoading(true);
      const answer = ask(l, 1);
      const stored = (await pageCache.get<FeedCopy>(key))?.value;
      if (!current(mine)) return;
      if (!stored) return firstPage(mine, answer);
      setLoading(false);
      show(stored);
      return refresh(mine, answer);
    },
    more: next,
    /** Retry after an error. */
    retry: next,
    /** Patch one work in place (a bookmark toggled in the reader), in its stored copy too. */
    patch(id: string, change: Partial<PixivItem>) {
      setItems((old) => old.map((i) => (i.id === id ? { ...i, ...change } : i)));
      store();
    },
  };
}

export type Feed = ReturnType<typeof createFeed>;
