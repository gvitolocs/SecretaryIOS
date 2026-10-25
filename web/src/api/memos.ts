// Voice memos (docs/memos.md): /v1/memos on lucina-memo (nezopt), through p1's Caddy.
// Resumable upload: create (idempotent by clientId) → PUT chunks at Upload-Offset →
// complete with the SHA-256. The server answers 409 with its byte count when the offset
// is wrong, so a broken upload continues where the server is.

import { ApiError, request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };

export type MemoStatus = "uploading" | "queued" | "transcribing" | "summarizing" | "rendering" | "publishing" | "done" | "failed";

export interface Memo {
  id: string;
  clientId: string;
  title: string;
  recordedAt: string;
  durationSec: number;
  sizeBytes: number;
  receivedBytes: number;
  status: MemoStatus;
  error?: string | null;
  language?: string | null;
  preview?: string | null;
  actionItems?: number;
  tags?: string[];
  obsidianNote?: string | null;
}

export interface MemoItem {
  text: string;
  at?: number;
  owner?: string;
  due?: string;
}

export interface MemoSummary {
  title: string;
  language: string;
  overview: string;
  keyPoints: MemoItem[];
  decisions: MemoItem[];
  actionItems: MemoItem[];
  tags: string[];
}

export interface MemoSegment {
  start: number;
  end: number;
  text: string;
  speaker?: string;
}

export interface MemoTranscript {
  language: string;
  engine: string;
  durationSec: number;
  segments: MemoSegment[];
}

export interface MemoDetail {
  memo: Memo;
  summary: MemoSummary | null;
  pdf: boolean;
}

export const PROCESSING: MemoStatus[] = ["queued", "transcribing", "summarizing", "rendering", "publishing"];

export const memosApi = {
  list: () => request<{ memos: Memo[] }>("GET", "/v1/memos?limit=100").then((r) => r.memos ?? []),
  detail: (id: string) => request<MemoDetail>("GET", `/v1/memos/${encodeURIComponent(id)}`),
  transcript: (id: string) =>
    request<{ transcript: MemoTranscript }>("GET", `/v1/memos/${encodeURIComponent(id)}/transcript`).then((r) => r.transcript),
  create: (body: {
    clientId: string;
    title?: string;
    recordedAt: string;
    timezone: string;
    durationSec: number;
    mime: string;
    sizeBytes: number;
    source: string;
  }) => request<{ memo: Memo }>("POST", "/v1/memos", { body, headers: CLIENT }).then((r) => r.memo),
  complete: (id: string, sha256: string) =>
    request<{ memo: Memo }>("POST", `/v1/memos/${encodeURIComponent(id)}/complete`, { body: { sha256 }, headers: CLIENT, timeoutMs: 60_000 }).then(
      (r) => r.memo,
    ),
  rename: (id: string, title: string) =>
    request<{ memo: Memo }>("PATCH", `/v1/memos/${encodeURIComponent(id)}`, { body: { title }, headers: CLIENT }).then((r) => r.memo),
  remove: (id: string) => request<unknown>("DELETE", `/v1/memos/${encodeURIComponent(id)}`, { headers: CLIENT }),
  resummarize: (id: string, instructions?: string) =>
    request<{ memo: Memo }>("POST", `/v1/memos/${encodeURIComponent(id)}/resummarize`, { body: { instructions: instructions ?? "" }, headers: CLIENT }).then(
      (r) => r.memo,
    ),
  audioUrl: (id: string) => `/v1/memos/${encodeURIComponent(id)}/audio`,
  pdfUrl: (id: string) => `/v1/memos/${encodeURIComponent(id)}/pdf`,

  /** One chunk at `offset`; returns the server's byte count (also after a 409). */
  async upload(id: string, offset: number, chunk: Blob): Promise<number> {
    let res: Response;
    try {
      res = await fetch(`/v1/memos/${encodeURIComponent(id)}/audio`, {
        method: "PUT",
        headers: { "Upload-Offset": String(offset), "X-Secretary-CSRF": "1", "Content-Type": "application/octet-stream", ...CLIENT },
        body: chunk,
        credentials: "same-origin",
        cache: "no-store",
      });
    } catch {
      throw new ApiError(0, "network", "Can't reach the server. Check your connection.");
    }
    const data = (await res.json().catch(() => null)) as { receivedBytes?: number; error?: { code?: string; message?: string } } | null;
    if ((res.ok || res.status === 409) && typeof data?.receivedBytes === "number") return data.receivedBytes;
    throw new ApiError(res.status, data?.error?.code ?? `http_${res.status}`, data?.error?.message ?? "Upload failed.");
  },
};

export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(Number.isFinite(seconds) ? seconds : 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s / 60) % 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

export async function sha256Hex(blob: Blob): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", await blob.arrayBuffer());
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}
