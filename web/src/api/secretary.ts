import { request } from "./client";

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
export const CHAT_MAX_CHARS = 4000;

export const api = {
  getSession: () => request<SessionInfo>("GET", "/v1/session"),
  login: (token: string) => request<unknown>("POST", "/v1/session", { body: { token } }),
  logout: () => request<unknown>("DELETE", "/v1/session"),

  getHealth: () => request<HealthInfo>("GET", "/v1/health", { timeoutMs: 10_000 }),

  sendChat: (body: { message: string; sessionId: string; clientMessageId: string }) =>
    request<ChatResponse>("POST", "/v1/chat", { body, timeoutMs: CHAT_TIMEOUT_MS }),
  getJob: (id: string) => request<JobInfo>("GET", `/v1/jobs/${encodeURIComponent(id)}`),

  listShopping: () => request<unknown>("GET", "/v1/shopping"),
  addShopping: (rawName: string) => request<unknown>("POST", "/v1/shopping", { body: { rawName } }),
  patchShopping: (id: string, body: Record<string, unknown>) =>
    request<unknown>("PATCH", `/v1/shopping/${encodeURIComponent(id)}`, { body }),
};
