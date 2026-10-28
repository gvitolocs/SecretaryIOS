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
// GET  /v1/instagram/highlights/items?id=&cursor=  -> Page
// GET  /v1/instagram/resolve?url=                  -> Page (one post or reel)
// GET  /v1/instagram/recent                        -> {items}
// POST /v1/instagram/save {items:[{url,kind,username,takenAt?,caption?}]} -> {saved, failed}

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
}

export interface IgPage {
  items: IgItem[];
  nextCursor: string | null;
  /** The backend returned the first page again: there is nothing more. */
  exhausted?: boolean;
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
  mediaCount: number;
}

export interface IgProfileBundle {
  profile: IgProfile;
  posts: IgPage;
  highlights: IgHighlight[];
}

export interface IgRecent {
  username: string;
  fullName: string;
  isVerified: boolean;
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

const BASE = "/v1/instagram";

async function get<T>(path: string): Promise<T> {
  const r = await request<{ ok: boolean; data: T }>("GET", `${BASE}${path}`);
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
  stories: (username: string) => get<IgPage>(`/stories/${user(username)}`),
  highlightItems: (id: string, cursor?: string | null) =>
    get<IgPage>(`/highlights/items?id=${encodeURIComponent(id)}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`),
  resolve: (url: string) => get<IgPage>(`/resolve?url=${encodeURIComponent(url)}`),
  recent: () => get<{ items: IgRecent[] }>("/recent").then((d) => d.items),
  async save(items: IgSaveItem[]): Promise<IgSaveResult> {
    const r = await request<{ ok: boolean; data: IgSaveResult }>("POST", `${BASE}/save`, { body: { items } });
    return r.data;
  },
};
