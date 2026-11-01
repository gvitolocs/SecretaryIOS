import { showAdult } from "../lib/adult";
import { ApiError, request } from "./client";

// The Pixiv plugin's service (docs/pixiv.md, `/v1/pixiv`). Answers are {ok:true,data}; every image URL is a
// signed /v1/pixiv/s/... path the <img> tag can load as it is. `adult=1` goes out only while "Show adult
// plugins" is on, and only the service decides what that unlocks (R-18 modes, works and list items).

const BASE = "/v1/pixiv";

export const RANKING_MODES = ["day", "week", "month", "day_male", "day_female", "original", "rookie"] as const;
export const RANKING_MODES_R18 = ["day_r18", "week_r18", "day_male_r18", "day_female_r18"] as const;
export type RankingMode = (typeof RANKING_MODES)[number] | (typeof RANKING_MODES_R18)[number];
export type SearchOrder = "date_d" | "date" | "popular";
export type SearchType = "all" | "illust" | "manga" | "ugoira";
export type SearchSafety = "safe" | "all" | "r18";
/** What a list holds: `safe` only all-ages works, `r18` only R-18 works, `all` both (the service enforces it). */
export type PixivContent = SearchSafety;

export interface PixivSettings {
  defaultContent: PixivContent;
  defaultSort: "date_d" | "popular";
}
const CONNECTION_ERRORS = ["approval_denied", "approval_expired", "approval_unavailable", "save_failed", "remove_denied", "remove_failed", "token_rejected", "broker_unavailable", "broker_unconfigured"] as const;
export interface PixivConnection {
  state: "disconnected" | "connected" | "expired" | "unavailable";
  user: { id: string; name: string; account: string } | null;
  premium: boolean | null;
  r18: boolean | null;
  r18g: boolean | null;
  store: "saved" | "waiting_approval" | null;
  removal: "waiting_approval" | null;
  error: (typeof CONNECTION_ERRORS)[number] | null;
  connectedAt: string | null;
}
export interface PixivStatus {
  [key: string]: unknown;
  connection: PixivConnection;
  settings: PixivSettings;
  account: { session: "none" | "ok" | "expired"; premium: boolean | null; r18: boolean | null };
}
export interface PixivConnectStart { loginUrl: string; state: string; expiresInSecs: number }

const object = (v: unknown): Record<string, unknown> => v && typeof v === "object" ? v as Record<string, unknown> : {};
const nullableBool = (v: unknown): boolean | null => typeof v === "boolean" ? v : null;
export function parseConnection(v: unknown): PixivConnection {
  const o = object(v), u = object(o.user);
  return {
    state: o.state === "connected" || o.state === "disconnected" || o.state === "expired" ? o.state : "unavailable",
    user: o.user && typeof o.user === "object" ? { id: str(u.id), name: str(u.name), account: str(u.account) } : null,
    premium: nullableBool(o.premium), r18: nullableBool(o.r18), r18g: nullableBool(o.r18g),
    store: o.store === "saved" || o.store === "waiting_approval" ? o.store : null,
    removal: o.removal === "waiting_approval" ? o.removal : null,
    error: CONNECTION_ERRORS.find(error => error === o.error) ?? null,
    connectedAt: typeof o.connectedAt === "string" ? o.connectedAt : null,
  };
}
export function parseSettings(v: unknown): PixivSettings {
  const o = object(v);
  return { defaultContent: o.defaultContent === "r18" || o.defaultContent === "all" ? o.defaultContent : "safe", defaultSort: o.defaultSort === "popular" ? "popular" : "date_d" };
}
export function parseStatus(v: unknown): PixivStatus {
  const o = object(v), a = object(o.account);
  return { ...o, connection: parseConnection(o.connection), settings: parseSettings(o.settings),
    account: { session: a.session === "ok" || a.session === "expired" ? a.session : "none", premium: nullableBool(a.premium), r18: nullableBool(a.r18) } };
}

/** The work's page on pixiv.net (the media menu's Copy link). */
export function pixivWorkUrl(id: string): string {
  return `https://www.pixiv.net/artworks/${encodeURIComponent(id)}`;
}

export interface PixivItem {
  id: string;
  title: string;
  author: string;
  authorId: string;
  tags: string[];
  thumbUrl: string;
  pages: number;
  r18: boolean;
  bookmarked: boolean;
  /** Pixiv's "<N>users入り" bookmark tier (1000 for "1000users入り"), when the work has one. */
  popularTier: number | null;
  addedAt?: number;
}

export interface PixivListing {
  items: PixivItem[];
  page: number;
  hasMore: boolean;
  total: number | null;
  numPages: number | null;
  /** Why the list is what it is: `r18_needs_session`, `r18_disabled_in_account`, `all_ages_only` (others: ignore). */
  notice: string | null;
  /** The order a search asked for and the one the service could use (`pixiv_popular`, `users_tags`, ...). */
  sort: { requested: string; used: string } | null;
}

export interface PixivTag {
  tag: string;
  en: string;
}

export interface PixivPage {
  index: number;
  thumbUrl: string;
  url: string;
  originalUrl: string;
}

export interface PixivIllust {
  id: string;
  title: string;
  author: string;
  authorId: string;
  created: string;
  kind: "illust" | "manga" | "ugoira";
  ugoira: boolean;
  r18: boolean;
  description: string;
  stats: { views: number; likes: number; bookmarks: number; comments: number };
  pageCount: number;
  coverUrl: string;
  tags: PixivTag[];
  pages: PixivPage[];
  bookmarked: boolean;
}

export interface SaveResult {
  saved: { page: number; itemId: string; duplicate: boolean }[];
  failed: { page: number; error: string }[];
}

export interface SearchParams {
  q: string;
  order?: SearchOrder;
  type?: SearchType;
  mode?: SearchSafety;
  page?: number;
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const num = (v: unknown, d = 0): number => (typeof v === "number" && Number.isFinite(v) ? v : d);

export function parseItem(v: unknown): PixivItem {
  const o = (v ?? {}) as Record<string, unknown>;
  return {
    id: str(o.id),
    title: str(o.title),
    author: str(o.author),
    authorId: str(o.authorId),
    tags: Array.isArray(o.tags) ? o.tags.filter((t): t is string => typeof t === "string") : [],
    thumbUrl: str(o.thumbUrl),
    pages: Math.max(1, num(o.pages, 1)),
    r18: o.r18 === true,
    bookmarked: o.bookmarked === true,
    popularTier: typeof o.popularTier === "number" && Number.isFinite(o.popularTier) && o.popularTier > 0 ? o.popularTier : null,
    ...(typeof o.addedAt === "number" ? { addedAt: o.addedAt } : {}),
  };
}

export function parseListing(v: unknown): PixivListing {
  const o = (v ?? {}) as Record<string, unknown>;
  return {
    items: (Array.isArray(o.items) ? o.items : []).map(parseItem).filter((i) => i.id),
    page: num(o.page, 1),
    hasMore: o.hasMore === true,
    total: typeof o.total === "number" ? o.total : null,
    numPages: typeof o.numPages === "number" ? o.numPages : null,
    notice: typeof o.notice === "string" && o.notice ? o.notice : null,
    sort: parseSort(o.sort),
  };
}

function parseSort(v: unknown): PixivListing["sort"] {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  return typeof o.used === "string" && o.used ? { requested: str(o.requested), used: o.used } : null;
}

export function parseIllust(v: unknown): PixivIllust {
  const o = (v ?? {}) as Record<string, unknown>;
  const stats = (o.stats ?? {}) as Record<string, unknown>;
  const kind = o.kind === "manga" || o.kind === "ugoira" ? o.kind : "illust";
  return {
    id: str(o.id),
    title: str(o.title),
    author: str(o.author),
    authorId: str(o.authorId),
    created: str(o.created),
    kind,
    ugoira: o.ugoira === true,
    r18: o.r18 === true,
    description: str(o.description),
    stats: { views: num(stats.views), likes: num(stats.likes), bookmarks: num(stats.bookmarks), comments: num(stats.comments) },
    pageCount: Math.max(1, num(o.pageCount, 1)),
    coverUrl: str(o.coverUrl),
    tags: (Array.isArray(o.tags) ? o.tags : []).map((t) => ({ tag: str((t as PixivTag)?.tag), en: str((t as PixivTag)?.en) })).filter((t) => t.tag),
    pages: (Array.isArray(o.pages) ? o.pages : []).map((p, i) => {
      const q = (p ?? {}) as Record<string, unknown>;
      return { index: num(q.index, i), thumbUrl: str(q.thumbUrl), url: str(q.url), originalUrl: str(q.originalUrl) };
    }),
    bookmarked: o.bookmarked === true,
  };
}

/** `?adult=1` (or `&adult=1`) only while the switch is on. */
function withAdult(params: URLSearchParams): string {
  if (showAdult()) params.set("adult", "1");
  const q = params.toString();
  return q ? `?${q}` : "";
}

async function data<T>(promise: Promise<{ ok?: boolean; data?: unknown }>, parse: (v: unknown) => T): Promise<T> {
  const body = await promise;
  if (!body || body.ok === false) throw new ApiError(502, "pixiv_bad_answer", "Pixiv answered unexpectedly.");
  return parse(body.data);
}

/** The service refuses R-18 with 403 `adult_off`; the screens show a gentler message for it. */
export function isAdultOff(e: unknown): boolean {
  return e instanceof ApiError && e.status === 403 && /adult_off/.test(e.message);
}

export const pixivApi = {
  status() { return data(request("GET", `${BASE}/status`), parseStatus); },
  connectStart(): Promise<PixivConnectStart> {
    return data(request("POST", `${BASE}/connect/start`), v => {
      const o = object(v);
      return { loginUrl: str(o.loginUrl), state: str(o.state), expiresInSecs: num(o.expiresInSecs) };
    });
  },
  connectFinish(state: string, code: string) { return data(request("POST", `${BASE}/connect/finish`, { body: { state, code } }), parseConnection); },
  disconnect() { return data(request("POST", `${BASE}/disconnect`), parseConnection); },
  settings() { return data(request("GET", `${BASE}/settings`), parseSettings); },
  /** Fresh signed links for the tokens of old ones (lib/pixivImages): token -> URL ("" when refused). */
  resign(tokens: string[]): Promise<Record<string, string>> {
    return data(request("POST", `${BASE}/resign`, { body: { tokens } }), (v) => {
      const urls = (v as { urls?: unknown })?.urls;
      const out: Record<string, string> = {};
      if (urls && typeof urls === "object") for (const [k, u] of Object.entries(urls)) if (typeof u === "string") out[k] = u;
      return out;
    });
  },
  saveSettings(patch: Partial<PixivSettings>) { return data(request("PUT", `${BASE}/settings`, { body: patch }), parseSettings); },
  ranking(mode: RankingMode, page = 1): Promise<PixivListing> {
    const p = new URLSearchParams({ mode, page: String(page) });
    return data(request("GET", `${BASE}/ranking${withAdult(p)}`), parseListing);
  },

  search(params: SearchParams): Promise<PixivListing> {
    const p = new URLSearchParams({
      q: params.q,
      order: params.order ?? "date_d",
      type: params.type ?? "all",
      mode: params.mode ?? "safe",
      page: String(params.page ?? 1),
    });
    return data(request("GET", `${BASE}/search${withAdult(p)}`), parseListing);
  },

  illust(id: string): Promise<PixivIllust> {
    return data(request("GET", `${BASE}/illust/${encodeURIComponent(id)}${withAdult(new URLSearchParams())}`), parseIllust);
  },

  /** The owner's bookmarks, filtered by `content` (the whole list on one page). */
  bookmarks(content: PixivContent = "safe"): Promise<PixivListing> {
    return data(request("GET", `${BASE}/bookmarks${withAdult(new URLSearchParams({ content }))}`), parseListing);
  },

  addBookmark(id: string): Promise<boolean> {
    return data(request("PUT", `${BASE}/bookmarks/${encodeURIComponent(id)}${withAdult(new URLSearchParams())}`), () => true);
  },

  removeBookmark(id: string): Promise<boolean> {
    return data(request("DELETE", `${BASE}/bookmarks/${encodeURIComponent(id)}`), () => false);
  },

  /** Saves the whole work, or only the given zero-based pages, into Lucina's Gallery (album "Pixiv"). */
  save(id: string, pages?: number[]): Promise<SaveResult> {
    const body: { id: string; pages?: number[] } = { id };
    if (pages && pages.length > 0) body.pages = pages;
    return data(request("POST", `${BASE}/save${withAdult(new URLSearchParams())}`, { body, timeoutMs: 180_000 }), (v) => {
      const o = (v ?? {}) as Record<string, unknown>;
      return {
        saved: (Array.isArray(o.saved) ? o.saved : []).map((s) => {
          const q = s as Record<string, unknown>;
          return { page: num(q.page), itemId: str(q.itemId), duplicate: q.duplicate === true };
        }),
        failed: (Array.isArray(o.failed) ? o.failed : []).map((s) => {
          const q = s as Record<string, unknown>;
          return { page: num(q.page), error: str(q.error) };
        }),
      };
    });
  },
};
