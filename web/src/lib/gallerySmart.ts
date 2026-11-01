// Smart search in the Gallery, with no DOM: when a query is answered by meaning rather than by words, what the "looked
// at N photos" hint says, and how a page falls back to the words search when the server has smart search switched off.

import { listGallery, SmartUnavailableError, type EmbeddingStatus, type GalleryPage, type GalleryQuery } from "../api/gallery";

export type SearchMode = "smart" | "words";

/** Smart search can answer: it is on and has looked at something. */
export const smartReady = (status: EmbeddingStatus | null | undefined): boolean => !!status && status.enabled && status.indexed > 0;

/** A query runs by meaning when the owner has not chosen words, the server is ready and has not refused. */
export function usesSmart(opts: { q: string; mode: SearchMode; status: EmbeddingStatus | null | undefined; refused: boolean }): boolean {
  return !!opts.q.trim() && opts.mode === "smart" && !opts.refused && smartReady(opts.status);
}

/** How many photos the index has looked at, when it has not reached all of them yet; else null (no hint). */
export function lookedAt(status: EmbeddingStatus | null | undefined): number | null {
  return status && status.enabled && status.indexed > 0 && status.pending > 0 ? status.indexed : null;
}

/** The query as the Gallery asks for it: smart and similar-to ride on the ordinary filters. */
export function withSearch(query: GalleryQuery, search: { smart: boolean; similarTo?: string | null }): GalleryQuery {
  return { ...query, smart: search.smart || undefined, similarTo: search.similarTo || undefined };
}

export interface SmartFetchHooks {
  /** The server said smart search is off: from now on use words. */
  onUnavailable: () => void;
  /** The server's index status came with a page. */
  onStatus?: (status: EmbeddingStatus) => void;
}

/**
 * The page loader for the Gallery's feed. A smart query that the server refuses (503) is answered with the words
 * search on the spot and tells the Gallery to stay on words; a refused "similar" tells it too and fails (the
 * Gallery leaves that view). Anything else (offline, a 500) is an ordinary failure.
 */
export function smartPager(
  smart: (q: GalleryQuery) => Promise<GalleryPage & { status?: EmbeddingStatus | null }>,
  hooks: SmartFetchHooks,
  words: (q: GalleryQuery) => Promise<GalleryPage> = listGallery,
): (q: GalleryQuery) => Promise<GalleryPage> {
  return async (q) => {
    if (!q.similarTo && !(q.smart && q.q?.trim())) return words(q);
    try {
      const page = await smart(q);
      if (page.status) hooks.onStatus?.(page.status);
      return { items: page.items, nextCursor: page.nextCursor };
    } catch (e) {
      if (!(e instanceof SmartUnavailableError)) throw e;
      hooks.onUnavailable();
      if (q.similarTo) throw e;
      return words({ ...q, smart: false });
    }
  };
}
