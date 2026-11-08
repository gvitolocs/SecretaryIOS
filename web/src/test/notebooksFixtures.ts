import { vi } from "vitest";
import type { Chat, ChatDetail, Citation, Insight, Message, Notebook, NotebookDetail, NoteLink, Podcast, Source, Transformation } from "../api/notebooks";

// Builders for the Notebooks API objects (docs/design/notebooks-api.md) and a fetch stub that can also
// answer with a text/event-stream body.

export interface NbCall {
  method: string;
  url: string;
  path: string;
  query: URLSearchParams;
  headers: Record<string, string>;
  /** The JSON body, or undefined (GET, multipart). */
  body: any;
  form?: Blob;
}

export interface NbReply {
  status?: number;
  body?: unknown;
  /** A text/event-stream reply: the raw chunks, in order. */
  sse?: string[];
}

const T = "2026-10-29T10:00:00Z";

export const notebook = (o: Partial<Notebook> = {}): Notebook => ({
  id: "nb1",
  name: "Research Methods",
  description: "Lecture notes",
  archived: false,
  created_at: T,
  updated_at: T,
  counts: { sources: 2, notes: 1, chats: 1, podcasts: 0 },
  ...o,
});

export const source = (o: Partial<Source> = {}): Source => ({
  id: "s1",
  kind: "pdf",
  title: "RM Class 2.pdf",
  origin: "upload:RM Class 2.pdf",
  status: "ready",
  error: null,
  chars: 4000,
  chunks: 8,
  pages: 22,
  duration_s: null,
  has_file: true,
  created_at: T,
  updated_at: T,
  context: "full",
  ...o,
});

export const noteLink = (o: Partial<NoteLink> = {}): NoteLink => ({
  slug: "rm-summary",
  title: "RM summary",
  excerpt: "Reliability vs validity",
  updated_at: T,
  indexed: true,
  ...o,
});

export const chat = (o: Partial<Chat> = {}): Chat => ({ id: "c1", notebook_id: "nb1", title: "Exam prep", created_at: T, updated_at: T, messages: 2, ...o });

export const citation = (o: Partial<Citation> = {}): Citation => ({
  ref: "source:s1-3",
  kind: "source",
  source_id: "s1",
  chunk: 3,
  title: "RM Class 2.pdf",
  excerpt: "Reliability is about consistency.",
  page: 7,
  start_s: null,
  ...o,
});

export const message = (o: Partial<Message> = {}): Message => ({ id: "m1", role: "assistant", text: "Hello", created_at: T, ...o });

export const insight = (o: Partial<Insight> = {}): Insight => ({
  id: "i1",
  source_id: "s1",
  transformation_id: "t-dense-summary",
  title: "Dense Summary",
  status: "ready",
  text: "A **dense** summary.",
  created_at: T,
  ...o,
});

export const transformation = (o: Partial<Transformation> = {}): Transformation => ({
  id: "t-dense-summary",
  name: "Dense Summary",
  description: "",
  prompt: "",
  apply_default: true,
  builtin: true,
  ...o,
});

export const podcast = (o: Partial<Podcast> = {}): Podcast => ({
  id: "p1",
  notebook_id: "nb1",
  title: "Episode one",
  episode_profile: "two-hosts",
  speaker_profile: "lucina-duo",
  briefing: "",
  status: "ready",
  error: null,
  outline: [],
  transcript: [{ speaker: "Lucina", text: "Welcome." }],
  duration_s: 312,
  audio: true,
  created_at: T,
  ...o,
});

export const detail = (o: Partial<NotebookDetail> = {}): NotebookDetail => ({
  ...notebook(),
  sources: [source(), source({ id: "s2", kind: "web", title: "Interview guide", origin: "https://guide.example/interviews", pages: null, chunks: 4, has_file: false })],
  notes: [noteLink()],
  chats: [chat()],
  podcasts: [],
  ...o,
});

export const chatDetail = (o: Partial<ChatDetail> = {}): ChatDetail => {
  const { messages: _count, ...base } = chat();
  return {
    ...base,
    messages: [
      message({ id: "m0", role: "user", text: "What is reliability?" }),
      message({ id: "m1", text: "It is consistency [source:s1-3].", citations: [citation()], model: "deepseek-flash" }),
    ],
    ...o,
  };
};

/** Stub global fetch with a handler that sees parsed calls and may answer with an event stream. */
export function stubNotebooks(handler: (call: NbCall) => NbReply | Promise<NbReply>): NbCall[] {
  const calls: NbCall[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries((init.headers ?? {}) as Record<string, string>)) headers[k.toLowerCase()] = v;
      const url = String(input);
      const u = new URL(url, "http://localhost");
      const call: NbCall = {
        method: init.method ?? "GET",
        url,
        path: u.pathname,
        query: u.searchParams,
        headers,
        body: typeof init.body === "string" ? JSON.parse(init.body) : undefined,
        form: init.body instanceof Blob ? init.body : undefined,
      };
      calls.push(call);
      const r = await handler(call);
      if (r.sse) {
        const enc = new TextEncoder();
        const stream = new ReadableStream<Uint8Array>({
          start(c) {
            for (const chunk of r.sse!) c.enqueue(enc.encode(chunk));
            c.close();
          },
        });
        return new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } });
      }
      const status = r.status ?? 200;
      return new Response(status === 204 ? null : r.body === undefined ? "" : JSON.stringify(r.body), {
        status,
        headers: { "Content-Type": "application/json" },
      });
    }),
  );
  return calls;
}

/** `event: <name>\ndata: <json>\n\n` */
export const sse = (event: string, data: unknown): string => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
