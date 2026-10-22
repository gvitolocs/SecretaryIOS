import { request } from "./client";
import type { TimelinePage } from "./timeline";

export interface SessionInfo {
  authenticated: boolean;
  deviceName?: string | null;
}

export type JobStatus = "queued" | "running" | "done" | "error";

export interface JobRef {
  id: string | number;
  status: JobStatus | string;
}

export interface ChatResponse {
  reply: string;
  action?: string | null;
  job: JobRef | null;
  /** Timeline ids of the recorded user message and Lucina's reply (newer servers). */
  userItemId?: string | number | null;
  lucinaItemId?: string | number | null;
  /** Lucina message format v1 (docs/lucina-message-format.md); `reply` is then the plain fallback. */
  format?: "v1";
  lang?: string;
  bubbles?: unknown;
  blocks?: unknown;
  actions?: unknown;
}

export interface JobInfo {
  id: string | number;
  status: JobStatus | string;
  action?: string | null;
  result?: unknown;
  error?: string | null;
  imageUrl?: string | null;
  createdAt?: string;
  updatedAt?: string;
}

export interface HealthInfo {
  ok: boolean;
  version?: string;
  lucina?: { reachable: boolean; latencyMs?: number | null; error?: string };
}

/** Lucina can take up to 120 s; leave headroom for the server's own timeout. */
export const CHAT_TIMEOUT_MS = 130_000;
/** Server limit for a chat message (docs/server-api.md). */
export const CHAT_MAX_CHARS = 12000;
/** Page size for GET /v1/timeline. */
export const TIMELINE_PAGE = 50;

export const api = {
  getSession: () => request<SessionInfo>("GET", "/v1/session"),
  login: (email: string, password: string) => request<unknown>("POST", "/v1/session", { body: { email, password } }),
  changePassword: (currentPassword: string, newPassword: string) =>
    request<unknown>("POST", "/v1/session/password", { body: { currentPassword, newPassword } }),
  logout: () => request<unknown>("DELETE", "/v1/session"),

  getHealth: () => request<HealthInfo>("GET", "/v1/health", { timeoutMs: 10_000 }),

  sendChat: (body: { message: string; sessionId: string; clientMessageId: string }) =>
    request<ChatResponse>("POST", "/v1/chat", {
      body,
      timeoutMs: CHAT_TIMEOUT_MS,
      headers: { "X-Secretary-Client": "web" },
    }),
  /** Newest first; `before` is the `nextBefore` cursor of the previous page. */
  getTimeline: (before: string | null = null, limit = TIMELINE_PAGE) =>
    request<TimelinePage>(
      "GET",
      `/v1/timeline?${before !== null ? `before=${encodeURIComponent(before)}&` : ""}limit=${limit}`,
    ),
  getJob: (id: string) => request<JobInfo>("GET", `/v1/jobs/${encodeURIComponent(id)}`),

  listShopping: () => request<unknown>("GET", "/v1/shopping"),
  addShopping: (rawName: string) => request<unknown>("POST", "/v1/shopping", { body: { rawName } }),
  patchShopping: (id: string, body: Record<string, unknown>) =>
    request<unknown>("PATCH", `/v1/shopping/${encodeURIComponent(id)}`, { body }),
};
