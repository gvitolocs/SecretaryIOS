// Notebooks (docs/design/notebooks-api.md): /v1/notebooks on p1, through Link to lucina-notebooks on
// nezopt. A NotebookLM-style workspace ported from Open Notebook by Luis Novo (MIT): sources, linked
// Keep notes, chats that cite passages, insights, and podcasts.

import { ApiError, request } from "./client";

export type SourceKind = "pdf" | "youtube" | "web" | "text" | "audio" | "file" | "note";
export type SourceStatus = "queued" | "extracting" | "embedding" | "ready" | "failed";
export type SourceContext = "full" | "summary" | "off";
export type InsightStatus = "queued" | "running" | "ready" | "failed";
export type PodcastStatus = "queued" | "outline" | "script" | "voicing" | "mixing" | "ready" | "failed";
export type ChatModel = "deepseek" | "qwen";
export type ArchivedFilter = "exclude" | "include" | "only";

export interface NotebookCounts {
  sources: number;
  notes: number;
  chats: number;
  podcasts: number;
}

export interface Notebook {
  id: string;
  name: string;
  description: string;
  archived: boolean;
  created_at: string;
  updated_at: string;
  counts: NotebookCounts;
}

export interface Source {
  id: string;
  kind: SourceKind;
  title: string;
  origin: string;
  status: SourceStatus;
  error: string | null;
  chars: number;
  chunks: number;
  pages: number | null;
  duration_s: number | null;
  has_file: boolean;
  created_at: string;
  updated_at: string;
  notebooks?: string[];
  /** Only in a notebook's listing. */
  context?: SourceContext;
  /** Only with ?text=1. */
  text?: string;
}

export interface NoteLink {
  slug: string;
  title: string;
  excerpt: string;
  updated_at: string;
  indexed: boolean;
}

export interface Chat {
  id: string;
  notebook_id: string;
  title: string;
  created_at: string;
  updated_at: string;
  messages: number;
}

export interface Citation {
  /** "source:<chunk id>", "note:<slug>" or "insight:<id>", as in the message text. */
  ref: string;
  kind: "source" | "note" | "insight";
  source_id?: string | null;
  chunk?: number | null;
  title: string;
  excerpt: string;
  page?: number | null;
  start_s?: number | null;
}

export interface Message {
  id: string;
  role: "user" | "assistant";
  text: string;
  citations?: Citation[];
  model?: string | null;
  created_at: string;
}

export interface ChatDetail extends Omit<Chat, "messages"> {
  messages: Message[];
}

export interface Chunk {
  id: string;
  ordinal: number;
  text: string;
  page?: number | null;
  start_s?: number | null;
}

export interface Transformation {
  id: string;
  name: string;
  description: string;
  prompt: string;
  apply_default: boolean;
  builtin: boolean;
}

export interface Insight {
  id: string;
  source_id: string;
  transformation_id: string;
  title: string;
  status: InsightStatus;
  text: string;
  created_at: string;
}

export interface OutlineItem {
  name: string;
  description: string;
  size: string;
}

export interface Podcast {
  id: string;
  notebook_id: string;
  title: string;
  episode_profile: string;
  speaker_profile: string;
  briefing: string;
  status: PodcastStatus;
  error: string | null;
  outline: OutlineItem[];
  transcript: { speaker: string; text: string }[];
  duration_s: number | null;
  audio: boolean;
  created_at: string;
}

export interface PodcastProfile {
  id: string;
  name: string;
  description?: string;
}

export interface NotebookDetail extends Notebook {
  sources: Source[];
  notes: NoteLink[];
  chats: Chat[];
  podcasts: Podcast[];
}

export interface SearchHit {
  ref: string;
  kind: "source" | "note" | "insight";
  source_id?: string | null;
  slug?: string | null;
  title: string;
  excerpt: string;
  score: number;
  notebook_id?: string;
}

export type NewSource =
  | { kind: "url"; url: string }
  | { kind: "text"; title?: string; text: string }
  | { kind: "sandbox"; path: string }
  | { kind: "note"; slug: string }
  | { kind: "source"; id: string };

export const SOURCE_PENDING: SourceStatus[] = ["queued", "extracting", "embedding"];
export const INSIGHT_PENDING: InsightStatus[] = ["queued", "running"];
export const PODCAST_PENDING: PodcastStatus[] = ["queued", "outline", "script", "voicing", "mixing"];

/** Whether anything in the notebook is still being worked on (the page polls until it is not). */
export function hasPending(nb: Pick<NotebookDetail, "sources" | "podcasts">): boolean {
  return nb.sources.some((s) => SOURCE_PENDING.includes(s.status)) || nb.podcasts.some((p) => PODCAST_PENDING.includes(p.status));
}

const BASE = "/v1/notebooks";
const enc = encodeURIComponent;
const LONG_MS = 120_000;
const UPLOAD_MS = 10 * 60_000;

/** `?a=1&b=2` from the defined values; "" when there are none. */
function query(params: Record<string, string | number | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** A multipart/form-data body as a Blob and its Content-Type, for client.request's raw body. */
export function multipart(fields: { name: string; value: string | File }[]): { body: Blob; contentType: string } {
  const boundary = `----secretary${Math.random().toString(16).slice(2)}${Date.now().toString(16)}`;
  const quote = (s: string) => s.replace(/[\r\n]+/g, " ").replace(/"/g, "%22");
  const parts: BlobPart[] = [];
  for (const f of fields) {
    if (typeof f.value === "string") {
      parts.push(`--${boundary}\r\nContent-Disposition: form-data; name="${quote(f.name)}"\r\n\r\n`, f.value, "\r\n");
    } else {
      parts.push(
        `--${boundary}\r\nContent-Disposition: form-data; name="${quote(f.name)}"; filename="${quote(f.value.name)}"\r\nContent-Type: ${f.value.type || "application/octet-stream"}\r\n\r\n`,
        f.value,
        "\r\n",
      );
    }
  }
  parts.push(`--${boundary}--\r\n`);
  return { body: new Blob(parts), contentType: `multipart/form-data; boundary=${boundary}` };
}

// ---------------------------------------------------------------- streaming

export interface SseEvent {
  event: string;
  data: string;
}

/**
 * Split a text/event-stream buffer into complete events. Returns the events and what is left
 * (an event that has not ended yet), to be put in front of the next chunk.
 */
export function parseSse(buffer: string): { events: SseEvent[]; rest: string } {
  const text = buffer.replace(/\r\n?/g, "\n");
  const parts = text.split("\n\n");
  const rest = parts.pop() ?? "";
  const events: SseEvent[] = [];
  for (const part of parts) {
    let event = "message";
    const data: string[] = [];
    for (const line of part.split("\n")) {
      if (line.startsWith(":")) continue;
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
    }
    if (data.length) events.push({ event, data: data.join("\n") });
  }
  return { events, rest };
}

export interface Exchange {
  user: Message;
  reply: Message;
}

async function errorFrom(res: Response): Promise<ApiError> {
  let code = `http_${res.status}`;
  let message = `Request failed (${res.status}).`;
  try {
    const body = (await res.json()) as { error?: { code?: unknown; message?: unknown } };
    if (typeof body.error?.code === "string") code = body.error.code;
    if (typeof body.error?.message === "string") message = body.error.message;
  } catch {
    // not JSON: keep the generic message
  }
  return new ApiError(res.status, code, message);
}

/**
 * Send a chat message. Asks for text/event-stream and calls `onDelta` with each piece of the reply;
 * a server that answers plain JSON (no streaming) works too. Resolves with the stored exchange.
 */
export async function sendMessage(
  chatId: string,
  body: { text: string; model?: ChatModel },
  onDelta?: (text: string) => void,
): Promise<Exchange> {
  let res: Response;
  try {
    res = await fetch(`${BASE}/chats/${enc(chatId)}/messages`, {
      method: "POST",
      headers: { Accept: "text/event-stream, application/json", "Content-Type": "application/json", "X-Secretary-CSRF": "1" },
      body: JSON.stringify(body),
      credentials: "same-origin",
      cache: "no-store",
    });
  } catch {
    throw new ApiError(0, "network", "Can't reach the server. Check your connection.");
  }
  if (!res.ok) throw await errorFrom(res);
  if (!(res.headers.get("Content-Type") ?? "").includes("text/event-stream") || !res.body) {
    return (await res.json()) as Exchange;
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let done: Exchange | null = null;
  for (;;) {
    const { value, done: end } = await reader.read();
    buffer += decoder.decode(value, { stream: !end });
    const parsed = parseSse(end ? `${buffer}\n\n` : buffer);
    buffer = parsed.rest;
    for (const ev of parsed.events) {
      let data: unknown;
      try {
        data = JSON.parse(ev.data);
      } catch {
        continue;
      }
      if (ev.event === "delta") {
        const piece = (data as { text?: unknown }).text;
        if (typeof piece === "string") onDelta?.(piece);
      } else if (ev.event === "message") {
        done = data as Exchange;
      } else if (ev.event === "error") {
        const e = (data as { error?: { code?: string; message?: string } }).error ?? (data as { code?: string; message?: string });
        throw new ApiError(500, e.code ?? "internal", e.message ?? "The reply failed.");
      }
    }
    if (end) break;
  }
  if (!done) throw new ApiError(0, "stream", "The reply ended early. Try again.");
  return done;
}

// ---------------------------------------------------------------- the API

export const notebooksApi = {
  list: (archived: ArchivedFilter = "exclude") =>
    request<{ notebooks: Notebook[] }>("GET", `${BASE}${query({ archived: archived === "exclude" ? undefined : archived })}`).then((r) => r.notebooks ?? []),
  create: (body: { name: string; description?: string }) => request<Notebook>("POST", BASE, { body }),
  get: (id: string) => request<NotebookDetail>("GET", `${BASE}/${enc(id)}`),
  update: (id: string, body: { name?: string; description?: string; archived?: boolean }) => request<Notebook>("PATCH", `${BASE}/${enc(id)}`, { body }),
  remove: (id: string, sources: "keep" | "delete-exclusive" = "keep") =>
    request<{ deleted: boolean; sources_deleted: number }>("DELETE", `${BASE}/${enc(id)}${query({ sources })}`),

  // sources
  addSource: (id: string, body: NewSource & { transformations?: string[] }) => request<Source>("POST", `${BASE}/${enc(id)}/sources`, { body }),
  uploadSource: (id: string, file: File, title?: string) => {
    const fields: { name: string; value: string | File }[] = [{ name: "file", value: file }];
    if (title) fields.push({ name: "title", value: title });
    return request<Source>("POST", `${BASE}/${enc(id)}/sources/upload`, { raw: multipart(fields), timeoutMs: UPLOAD_MS });
  },
  setContext: (id: string, sourceId: string, context: SourceContext) =>
    request<Source>("PATCH", `${BASE}/${enc(id)}/sources/${enc(sourceId)}`, { body: { context } }),
  removeSource: (id: string, sourceId: string) =>
    request<{ removed: boolean; deleted: boolean }>("DELETE", `${BASE}/${enc(id)}/sources/${enc(sourceId)}`),
  source: (sourceId: string, text = false) => request<Source>("GET", `${BASE}/sources/${enc(sourceId)}${text ? "?text=1" : ""}`),
  chunks: (sourceId: string, range: { from?: number; limit?: number; around?: number } = {}) =>
    request<{ chunks: Chunk[] }>("GET", `${BASE}/sources/${enc(sourceId)}/chunks${query(range)}`).then((r) => r.chunks ?? []),
  retrySource: (sourceId: string) => request<Source>("POST", `${BASE}/sources/${enc(sourceId)}/retry`, { body: {} }),
  fileUrl: (sourceId: string) => `${BASE}/sources/${enc(sourceId)}/file`,

  // insights and transformations
  insights: (sourceId: string) => request<{ insights: Insight[] }>("GET", `${BASE}/sources/${enc(sourceId)}/insights`).then((r) => r.insights ?? []),
  runTransformation: (sourceId: string, transformationId: string) =>
    request<Insight>("POST", `${BASE}/sources/${enc(sourceId)}/insights`, { body: { transformation_id: transformationId } }),
  insightToNote: (insightId: string) => request<NoteLink>("POST", `${BASE}/insights/${enc(insightId)}/note`, { body: {} }),
  removeInsight: (insightId: string) => request<{ deleted: boolean }>("DELETE", `${BASE}/insights/${enc(insightId)}`),
  transformations: () => request<{ transformations: Transformation[] }>("GET", `${BASE}/transformations`).then((r) => r.transformations ?? []),

  // notes (Keep notes linked to the notebook)
  linkNote: (id: string, slug: string) => request<NoteLink>("POST", `${BASE}/${enc(id)}/notes`, { body: { slug } }),
  writeNote: (id: string, title: string, text: string) => request<NoteLink>("POST", `${BASE}/${enc(id)}/notes`, { body: { title, text } }),
  unlinkNote: (id: string, slug: string) => request<{ removed: boolean }>("DELETE", `${BASE}/${enc(id)}/notes/${enc(slug)}`),

  search: (id: string, q: string, limit = 20) =>
    request<{ hits: SearchHit[] }>("GET", `${BASE}/${enc(id)}/search${query({ q, limit })}`).then((r) => r.hits ?? []),

  // chats
  chats: (id: string) => request<{ chats: Chat[] }>("GET", `${BASE}/${enc(id)}/chats`).then((r) => r.chats ?? []),
  createChat: (id: string, title?: string) => request<Chat>("POST", `${BASE}/${enc(id)}/chats`, { body: title ? { title } : {} }),
  chat: (chatId: string) => request<ChatDetail>("GET", `${BASE}/chats/${enc(chatId)}`),
  renameChat: (chatId: string, title: string) => request<Chat>("PATCH", `${BASE}/chats/${enc(chatId)}`, { body: { title } }),
  removeChat: (chatId: string) => request<{ deleted: boolean }>("DELETE", `${BASE}/chats/${enc(chatId)}`),
  sendMessage,
  messageToNote: (chatId: string, messageId: string, title?: string) =>
    request<NoteLink>("POST", `${BASE}/chats/${enc(chatId)}/messages/${enc(messageId)}/note`, { body: title ? { title } : {}, timeoutMs: LONG_MS }),

  // podcasts
  podcastProfiles: () =>
    request<{ episode_profiles?: unknown[]; speaker_profiles?: unknown[] }>("GET", `${BASE}/podcast-profiles`).then((r) => ({
      episode_profiles: profiles(r.episode_profiles),
      speaker_profiles: profiles(r.speaker_profiles),
    })),
  podcasts: (id: string) => request<{ podcasts: Podcast[] }>("GET", `${BASE}/${enc(id)}/podcasts`).then((r) => r.podcasts ?? []),
  createPodcast: (id: string, body: { episode_profile?: string; speaker_profile?: string; briefing?: string; minutes?: number }) =>
    request<Podcast>("POST", `${BASE}/${enc(id)}/podcasts`, { body }),
  removePodcast: (podcastId: string) => request<{ deleted: boolean }>("DELETE", `${BASE}/podcasts/${enc(podcastId)}`),
  audioUrl: (podcastId: string) => `${BASE}/podcasts/${enc(podcastId)}/audio`,
};

// ---------------------------------------------------------------- helpers

/** Profiles as the server lists them: plain names, or objects with a name (and maybe an id and a description). */
export function profiles(list: unknown[] | undefined): PodcastProfile[] {
  const out: PodcastProfile[] = [];
  for (const raw of list ?? []) {
    if (typeof raw === "string") out.push({ id: raw, name: raw });
    else if (raw && typeof raw === "object") {
      const o = raw as Record<string, unknown>;
      const id = [o.id, o.name, o.slug].find((v): v is string => typeof v === "string" && v !== "");
      const name = [o.name, o.title, o.id].find((v): v is string => typeof v === "string" && v !== "");
      if (id) out.push({ id, name: name ?? id, ...(typeof o.description === "string" ? { description: o.description } : {}) });
    }
  }
  return out;
}

/** The chunk ordinal of a `source:<source id>-<ordinal>` ref, or null. */
export function chunkOrdinal(ref: string): number | null {
  const m = /-(\d+)$/.exec(ref);
  return m ? Number(m[1]) : null;
}

/** The source id of a `source:<source id>-<ordinal>` ref (the chunk id without its ordinal), or null. */
export function refSourceId(ref: string): string | null {
  const m = /^source:(.+)-\d+$/.exec(ref);
  return m ? m[1]! : null;
}
