// Notes (docs/server-api.md, "Notes"): GET/POST /v1/notes, GET/PATCH/DELETE
// /v1/notes/{id|slug}, POST /v1/notes/{key}/append, the trash and restore.

import { request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };
/** The server's limit on a note's text (UTF-8 bytes). */
export const NOTE_MAX_BYTES = 256 * 1024;
export const NOTE_TITLE_MAX = 200;

export interface NoteSummary {
  id: string;
  slug: string;
  title: string;
  tags: string[];
  source: string;
  pinned: boolean;
  createdAt: string;
  updatedAt: string;
  size: number;
  excerpt: string;
  deletedAt?: string;
  purgeAfter?: string;
}

export interface Note {
  id: string;
  slug: string;
  title: string;
  content: string;
  tags: string[];
  source: string;
  pinned: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface NoteDraft {
  title: string;
  content: string;
  tags: string[];
  slug?: string;
  pinned?: boolean;
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const tags = (v: unknown): string[] => (Array.isArray(v) ? v.filter((t): t is string => typeof t === "string") : []);

function parseSummary(v: unknown): NoteSummary | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  const id = str(r.id);
  const slug = str(r.slug);
  if (!id || !slug) return null;
  const s: NoteSummary = {
    id,
    slug,
    title: str(r.title) || slug,
    tags: tags(r.tags),
    source: str(r.source) || "owner",
    pinned: r.pinned === true,
    createdAt: str(r.createdAt),
    updatedAt: str(r.updatedAt),
    size: typeof r.size === "number" ? r.size : 0,
    excerpt: str(r.excerpt),
  };
  if (str(r.deletedAt)) s.deletedAt = str(r.deletedAt);
  if (str(r.purgeAfter)) s.purgeAfter = str(r.purgeAfter);
  return s;
}

export function parseNote(v: unknown): Note {
  const r = (v && typeof v === "object" ? v : {}) as Record<string, unknown>;
  return {
    id: str(r.id),
    slug: str(r.slug),
    title: str(r.title) || str(r.slug),
    content: str(r.content),
    tags: tags(r.tags),
    source: str(r.source) || "owner",
    pinned: r.pinned === true,
    createdAt: str(r.createdAt),
    updatedAt: str(r.updatedAt),
  };
}

function parseList(v: unknown): NoteSummary[] {
  const arr = v && typeof v === "object" ? (v as { notes?: unknown }).notes : undefined;
  return Array.isArray(arr) ? arr.map(parseSummary).filter((n): n is NoteSummary => n !== null) : [];
}

const enc = (key: string) => encodeURIComponent(key);

export const notesApi = {
  list: async (q = "", tag = ""): Promise<NoteSummary[]> => {
    const params = new URLSearchParams();
    if (q.trim()) params.set("q", q.trim());
    if (tag) params.set("tag", tag);
    const qs = params.toString();
    return parseList(await request<unknown>("GET", `/v1/notes${qs ? `?${qs}` : ""}`));
  },
  get: async (key: string): Promise<Note> => parseNote(await request<unknown>("GET", `/v1/notes/${enc(key)}`)),
  create: async (d: NoteDraft): Promise<Note> =>
    parseNote(
      await request<unknown>("POST", "/v1/notes", {
        body: { title: d.title, content: d.content, tags: d.tags, pinned: d.pinned ?? false, ...(d.slug ? { slug: d.slug } : {}) },
        headers: CLIENT,
      }),
    ),
  update: async (
    key: string,
    patch: Partial<Pick<Note, "title" | "content" | "tags" | "pinned" | "slug">>,
    baseUpdatedAt?: string,
  ): Promise<Note> =>
    parseNote(
      await request<unknown>("PATCH", `/v1/notes/${enc(key)}`, {
        body: { ...patch, ...(baseUpdatedAt ? { baseUpdatedAt } : {}) },
        headers: CLIENT,
      }),
    ),
  remove: async (key: string): Promise<void> => {
    await request<unknown>("DELETE", `/v1/notes/${enc(key)}`, { headers: CLIENT });
  },
  trash: async (): Promise<NoteSummary[]> => parseList(await request<unknown>("GET", "/v1/notes/trash")),
  restore: async (id: string): Promise<Note> =>
    parseNote(await request<unknown>("POST", `/v1/notes/trash/${enc(id)}/restore`, { headers: CLIENT })),
};

/** "a, b , #c" → ["a", "b", "c"] (the server normalises again). */
export function parseTags(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(",")) {
    const t = raw.trim().replace(/^#/, "").replace(/\s+/g, " ").toLowerCase();
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

export function utf8Bytes(s: string): number {
  return new TextEncoder().encode(s).length;
}

/**
 * A list excerpt as plain text: markdown marks removed, and a leading
 * heading that repeats the title dropped.
 */
export function plainExcerpt(excerpt: string, title = ""): string {
  let s = excerpt
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/(^|\s)#{1,6}\s+/g, "$1")
    .replace(/(^|\s)>\s+/g, "$1")
    .replace(/(^|\s)(?:[-*+]|\d{1,3}[.)])\s+(?:\[[ xX]\]\s+)?/g, "$1")
    .replace(/\*\*|__|~~|`+/g, "")
    .replace(/(^|[\s(])[*_]([^*_\s][^*_]*?)[*_](?=[\s).,;:!?]|$)/g, "$1$2")
    .replace(/(^|[\s(])[*_]+(?=\S)|(\S)[*_]+(?=[\s).,;:!?]|$)/g, "$1$2")
    .replace(/\|/g, " ")
    .replace(/(^|\s)-{3,}(?=\s|$)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
  const tt = title.trim();
  if (tt && s.toLowerCase().startsWith(tt.toLowerCase())) s = s.slice(tt.length).trim();
  return s;
}
