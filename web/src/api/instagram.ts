import { request } from "./client";

// The Instagram plugin's wire types, per docs/instagram.md (`/v1/instagram`, owner only).
// Success is `{ok:true, data}`; errors come through `request` as an ApiError (429 carries
// retryAfterSeconds). Every `url` / `thumbUrl` / `picUrl` / `coverUrl` is a signed
// `/v1/instagram/s/…` URL of our own server: the browser never contacts Instagram.
//
// GET  /v1/instagram/profile/{username}            -> {profile, posts, highlights}
// GET  /v1/instagram/posts/{username}?cursor=      -> Page (the backend ignores the cursor: `exhausted`)
// GET  /v1/instagram/reels/{username}?cursor=      -> Page
// GET  /v1/instagram/stories/{username}            -> Page
// GET  /v1/instagram/highlights/{username}         -> {items: IgHighlight[]}
// GET  /v1/instagram/highlights/items?id=&cursor=  -> Page
// GET  /v1/instagram/resolve?url=                  -> Page (one post or reel)
// GET  /v1/instagram/recent                        -> {items}
// POST /v1/instagram/save {items:[{url,kind,username,takenAt?,caption?}]} -> {saved, failed}
// POST /v1/instagram/resign {urls}                 -> {urls: {old: new}} (only links the service signed)
// GET  /v1/instagram/watch                         -> [IgWatch]
// PUT  /v1/instagram/watch/{username} {autoSave?}  -> IgWatch; DELETE removes it
// GET  /v1/instagram/watch/{username}/new, POST …/opened, GET /v1/instagram/badges, GET /v1/instagram/archive/{username}
// GET  /v1/instagram/watch-settings                -> IgWatchSettings
// PUT  /v1/instagram/watch-settings {intervalHours} -> IgWatchSettings (1-23; all watched accounts)

export type IgKind = "image" | "video";

export interface IgChild {
  id: string;
  kind: IgKind;
  url: string;
  thumbUrl: string | null;
}

export interface IgItem {
  id: string;
  kind: IgKind | "carousel";
  url: string;
  thumbUrl: string | null;
  /** Unix seconds; null for reels and carousels. */
  takenAt?: number | null;
  caption?: string | null;
  likeCount?: number;
  commentCount?: number;
  permalink?: string;
  children?: IgChild[];
  /** A story no longer up on Instagram, playing from the local copy. */
  expired?: boolean;
}

export interface IgPage {
  items: IgItem[];
  nextCursor: string | null;
  /** The backend returned the first page again: there is nothing more. */
  exhausted?: boolean;
  /** This answer came from the server's store and it is asking the backend behind it: ask again shortly. */
  refreshing?: boolean;
}

export interface IgProfile {
  username: string;
  fullName: string;
  biography: string | null;
  picUrl: string | null;
  profileUrl: string;
  mediaCount: number;
  followerCount: number;
  followingCount: number;
  isPrivate: boolean;
  hasStories: boolean;
  isVerified: boolean;
  userId: string;
  externalUrl: string | null;
}

export interface IgHighlight {
  id: string;
  title: string;
  coverUrl: string | null;
  /** null when the backend doesn't say (it answers 0 for highlights with items). */
  mediaCount: number | null;
}

export interface IgProfileBundle {
  profile: IgProfile;
  posts: IgPage;
  highlights: IgHighlight[];
  /** The server is asking the backend behind this answer: ask again shortly. */
  refreshing?: boolean;
  /** The account stopped answering; this is what was kept. */
  gone?: boolean;
}

export interface IgRecent {
  username: string;
  fullName: string;
  isVerified: boolean;
  /** Private when last opened (absent from older servers: not known). */
  isPrivate?: boolean;
  picUrl: string | null;
  openedAt: number;
}

export interface IgSaveItem {
  url: string;
  kind: IgKind;
  username: string;
  takenAt?: number | null;
  caption?: string | null;
}

export interface IgSaveResult {
  saved: { index: number; itemId: string; duplicate: boolean }[];
  failed: { index: number; error: string }[];
}

/** A watched account (GET /v1/instagram/watch). */
export interface IgWatch {
  username: string;
  autoSave: boolean;
  /** Unix seconds of the last refresh; null before the first. */
  lastCheck: number | null;
  /** Unix seconds of the next refresh. */
  nextCheck?: number | null;
  newStories: number;
  newPosts: number;
  /** null until the first refresh. */
  private: boolean | null;
  fullName?: string;
  picUrl?: string | null;
  isVerified?: boolean;
  gone?: boolean;
  status?: string | null;
  archived?: number;
}

/** How often watched accounts are checked (GET /v1/instagram/watch-settings). */
export interface IgWatchSettings {
  /** The owner's choice, else `defaultHours`. */
  intervalHours: number;
  defaultHours: number;
  /** Each check lands this much before or after the interval, at random. */
  jitterMinutes: number;
  choices: number[];
}

export interface IgBadges {
  total: number;
  accounts: Record<string, number>;
}

const BASE = "/v1/instagram";

// Repeat visits answer from the server's store at once. The first visit to an account waits for the backend, 10-35 s
// (a cold profile bundle, with its retries), longer than the client's default 20 s. The views show "Loading" meanwhile.
const TIMEOUT_MS = 60_000;

async function get<T>(path: string): Promise<T> {
  const r = await request<{ ok: boolean; data: T }>("GET", `${BASE}${path}`, { timeoutMs: TIMEOUT_MS });
  return r.data;
}

function cursorQuery(cursor?: string | null): string {
  return cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
}

const user = (username: string) => encodeURIComponent(username);

export const instagramApi = {
  profile: (username: string) => get<IgProfileBundle>(`/profile/${user(username)}`),
  posts: (username: string, cursor?: string | null) => get<IgPage>(`/posts/${user(username)}${cursorQuery(cursor)}`),
  reels: (username: string, cursor?: string | null) => get<IgPage>(`/reels/${user(username)}${cursorQuery(cursor)}`),
  stories: (username: string, cursor?: string | null) => get<IgPage>(`/stories/${user(username)}${cursorQuery(cursor)}`),
  highlights: (username: string) => get<{ items: IgHighlight[] }>(`/highlights/${user(username)}`).then((d) => d.items),
  highlightItems: (id: string, cursor?: string | null) =>
    get<IgPage>(`/highlights/items?id=${encodeURIComponent(id)}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`),
  resolve: (url: string) => get<IgPage>(`/resolve?url=${encodeURIComponent(url)}`),
  recent: () => get<{ items: IgRecent[] }>("/recent").then((d) => d.items),
  watchList: () => get<IgWatch[]>("/watch"),
  async watch(username: string, body: { autoSave?: boolean } = {}): Promise<IgWatch> {
    const r = await request<{ ok: boolean; data: IgWatch }>("PUT", `${BASE}/watch/${user(username)}`, { body });
    return r.data;
  },
  async unwatch(username: string): Promise<void> {
    await request("DELETE", `${BASE}/watch/${user(username)}`);
  },
  /** The owner opened the account: its badge clears. Answers the badges that are left. */
  async opened(username: string): Promise<IgBadges> {
    const r = await request<{ ok: boolean; data: IgBadges }>("POST", `${BASE}/watch/${user(username)}/opened`);
    return r.data;
  },
  watchSettings: () => get<IgWatchSettings>("/watch-settings"),
  /** For all watched accounts; applies to their next check. */
  async setWatchSettings(intervalHours: number): Promise<IgWatchSettings> {
    const r = await request<{ ok: boolean; data: IgWatchSettings }>("PUT", `${BASE}/watch-settings`, { body: { intervalHours } });
    return r.data;
  },
  /** New links for old ones; a link the service did not sign is left out of the answer. */
  async resign(urls: string[]): Promise<Record<string, string>> {
    const r = await request<{ ok: boolean; data: { urls: Record<string, string> } }>("POST", `${BASE}/resign`, { body: { urls } });
    return r.data.urls;
  },
  badges: () => get<IgBadges>("/badges"),
  /** What was saved to the Gallery for this account (newest first); `cursor` is an offset. */
  archive: (username: string, cursor?: string | null) => get<IgPage>(`/archive/${user(username)}${cursorQuery(cursor)}`),
  async save(items: IgSaveItem[]): Promise<IgSaveResult> {
    const r = await request<{ ok: boolean; data: IgSaveResult }>("POST", `${BASE}/save`, { body: { items } });
    return r.data;
  },
};
