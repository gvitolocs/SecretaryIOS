// NClient plugin client (docs/nclient.md, docs/nclient-web.md). Same-origin, session cookie; the
// service answers `{ok, data}` and p1 adds the gateway header. Nothing here is persisted.
import { request } from "./client";

export type NClientSort = "recent" | "popular" | "popular-week" | "popular-today" | "popular-month";
export interface NClientTag { id: number; name: string; type: string; count: number }
export interface NClientProgress { page: number; pages: number; updated_at: number }
export interface NClientSummary {
  id: number; title: string; pages: number; language: string | null; tags: NClientTag[];
  thumbUrl: string; favorite: boolean; progress: NClientProgress | null;
}
export interface NClientPage { index: number; width: number; height: number; url: string; thumbUrl: string }
export interface NClientDetail extends Omit<NClientSummary, "pages"> {
  coverUrl: string; pages: NClientPage[]; related: NClientSummary[];
  titles?: { english: string; japanese: string; pretty: string };
  uploaded?: number; favorites?: number;
}
export interface NClientResults { items: NClientSummary[]; page: number; numPages: number }
export interface NClientSearch { q: string; page: number; sort: NClientSort; tags: number[]; exclude: number[] }
export interface NClientStatus { enabled: boolean; save?: { gallery: boolean; library: boolean } }

export type SaveState = "queued" | "running" | "done" | "failed" | "cancelled";
export interface SaveStatus {
  job: string; state: SaveState; total: number; done: number;
  saved: { index: number; itemId: string; duplicate: boolean }[];
  failed: { index: number; error: string }[];
  error?: string;
}
export const saveFinished = (s: SaveState) => s === "done" || s === "failed" || s === "cancelled";

const ROOT = "/v1/nclient";
async function get<T>(path: string): Promise<T> {
  return (await request<{ ok: true; data: T }>("GET", ROOT + path)).data;
}

/** Only display signed, same-origin proxy URLs actually returned by the service.
 * Never reconstruct URLs, fall back to an upstream host, or persist content. */
export function nclientImageUrl(value: string | undefined): string | undefined {
  if (!value || !/^\/v1\/nclient\/s\/\d+\/(thumb|cover|page|pthumb)\/\d+\/\d+\/[A-Za-z0-9_-]+$/.test(value)) return undefined;
  return value;
}

// Pending requests only, shared across mounted reading sessions. An older write
// must finish before a newer one for the same gallery can reach the server.
const progressWrites = new Map<number, Promise<unknown>>();
function saveProgress(id: number, page: number, pages: number): Promise<unknown> {
  const previous = progressWrites.get(id) ?? Promise.resolve();
  const write = previous.catch(() => undefined).then(() =>
    request<unknown>("PUT", `${ROOT}/progress/${id}`, { body: { page, pages } }),
  );
  progressWrites.set(id, write);
  const clear = () => { if (progressWrites.get(id) === write) progressWrites.delete(id); };
  void write.then(clear, clear);
  return write;
}

export const nclientApi = {
  status: () => get<NClientStatus>("/status"),
  search: (search: NClientSearch) => {
    const query = new URLSearchParams({ q: search.q, page: String(search.page), sort: search.sort });
    if (search.tags.length) query.set("tags", search.tags.join(","));
    if (search.exclude.length) query.set("exclude", search.exclude.join(","));
    return get<NClientResults>(`/search?${query}`);
  },
  tags: (q: string) => get<{ tags: NClientTag[] }>(`/tags?type=tag&q=${encodeURIComponent(q)}&limit=30`),
  detail: (id: number) => get<NClientDetail>(`/galleries/${id}`),
  random: () => get<NClientDetail>("/random"),
  related: (id: number) => get<{ items: NClientSummary[] }>(`/galleries/${id}/related`),
  favorites: () => get<{ items: NClientSummary[] }>("/favorites"),
  favorite: async (id: number, favorite: boolean) =>
    (await request<{ data: { favorite: boolean } }>(favorite ? "PUT" : "DELETE", `${ROOT}/favorites/${id}`)).data,
  saveProgress,
  /** Save every page of a gallery into Lucina's Gallery (a background job on the service). */
  save: async (id: number) =>
    (await request<{ data: SaveStatus }>("POST", `${ROOT}/galleries/${id}/save`, { body: { target: "gallery" } })).data,
  saveStatus: (job: string) => get<SaveStatus>(`/saves/${encodeURIComponent(job)}`),
  cancelSave: (job: string) => request<unknown>("DELETE", `${ROOT}/saves/${encodeURIComponent(job)}`),
};
