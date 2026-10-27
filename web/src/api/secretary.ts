import { request } from "./client";
import type { TimelinePage } from "./timeline";

export interface SessionInfo {
  deviceId?: string;
  authenticated: boolean;
  deviceName?: string | null;
}

/** `POST /v1/session/qr`: a QR sign-in request (signed-out browser). */
export interface QrStart {
  code: string;
  pollToken: string;
  approveUrl: string;
  expiresIn: number;
}

export type QrStatus = "pending" | "approved" | "denied" | "expired";

/** `GET /v1/session/qr/info`: who asked, for the approving device. */
export interface QrInfo {
  ip: string;
  userAgent: string;
  createdAt: string;
  expiresIn: number;
}

export type JobStatus = "queued" | "running" | "done" | "error";

export interface JobRef {
  id: string | number;
  status: JobStatus | string;
}

export interface ChatResponse {
  /** Private OTP reply: keep only in RAM, never speak or cache. */
  sensitive?: boolean;
  expiresAt?: number;
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

  // QR sign-in (server: qr_login.rs). Start, image and poll are public; info and approve need a session.
  qrStart: () => request<QrStart>("POST", "/v1/session/qr"),
  qrImageUrl: (code: string) => `/v1/session/qr/image?c=${encodeURIComponent(code)}`,
  qrPoll: (pollToken: string) =>
    request<{ status: QrStatus }>("GET", `/v1/session/qr/poll?t=${encodeURIComponent(pollToken)}`),
  qrInfo: (code: string) => request<QrInfo>("GET", `/v1/session/qr/info?c=${encodeURIComponent(code)}`),
  qrApprove: (code: string, approve: boolean) =>
    request<{ status: "approved" | "denied" }>("POST", "/v1/session/qr/approve", { body: { code, approve } }),

  getHealth: () => request<HealthInfo>("GET", "/v1/health", { timeoutMs: 10_000 }),

  sendChat: (body: { message: string; sessionId: string; clientMessageId: string; replyTo?: string }) =>
    request<ChatResponse>("POST", "/v1/chat", {
      body,
      timeoutMs: CHAT_TIMEOUT_MS,
      headers: { "X-Secretary-Client": "web" },
    }),
  /** Newest first; `before` is the `nextBefore` cursor of the previous page. */
  getTimeline: (before: string | null = null, limit = TIMELINE_PAGE, filter: { scope?: string; conversation?: string; after?: string } = {}) =>
    request<TimelinePage>(
      "GET",
      `/v1/timeline?${before !== null ? `before=${encodeURIComponent(before)}&` : ""}limit=${limit}${Object.entries(filter).map(([k, v]) => `&${k}=${encodeURIComponent(v)}`).join("")}`,
    ),
  getConversations: () => request<{ items: import("../lib/chatSync").Conversation[] }>("GET", "/v1/timeline/conversations"),
  getJob: (id: string) => request<JobInfo>("GET", `/v1/jobs/${encodeURIComponent(id)}`),

  listShopping: () => request<unknown>("GET", "/v1/shopping"),
  addShopping: (rawName: string) => request<unknown>("POST", "/v1/shopping", { body: { rawName } }),
  patchShopping: (id: string, body: Record<string, unknown>) =>
    request<unknown>("PATCH", `/v1/shopping/${encodeURIComponent(id)}`, { body }),
};
