// Thin JSON client for the Secretary server (same origin).
//
// Every state-changing request carries `X-Secretary-CSRF: 1`, cookies are sent
// with `credentials: 'same-origin'`, and every failure is normalised into an
// ApiError so the UI never has to inspect raw responses.

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryAfterSeconds: number | null;

  constructor(status: number, code: string, message: string, retryAfterSeconds: number | null = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.retryAfterSeconds = retryAfterSeconds;
  }

  /** Lucina (the AI backend behind the server) is offline or timed out. */
  get lucinaOffline(): boolean {
    return this.code === "lucina_unreachable" || this.code === "lucina_timeout";
  }

  get isNetwork(): boolean {
    return this.status === 0;
  }
}

export function toApiError(e: unknown): ApiError {
  if (e instanceof ApiError) return e;
  if (e instanceof Error) return new ApiError(0, "client_error", e.message);
  return new ApiError(0, "client_error", "Something went wrong.");
}

type Method = "GET" | "POST" | "PATCH" | "DELETE";

export interface RequestOptions {
  body?: unknown;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 20_000;

let onUnauthorized: (() => void) | null = null;
let onLucinaOffline: ((offline: ApiError) => void) | null = null;

/** Called on a 401 from any endpoint other than the session endpoint itself. */
export function setUnauthorizedHandler(fn: (() => void) | null): void {
  onUnauthorized = fn;
}

/** Called whenever the server reports that Lucina is unreachable. */
export function setLucinaOfflineHandler(fn: ((err: ApiError) => void) | null): void {
  onLucinaOffline = fn;
}

function statusMessage(status: number): string {
  if (status === 401) return "You are signed out.";
  if (status === 403) return "That isn't allowed.";
  if (status === 404) return "Not found.";
  if (status === 429) return "Too many requests. Please wait a moment.";
  if (status === 502 || status === 504) return "Lucina is offline right now.";
  if (status >= 500) return "The server had a problem. Please try again.";
  return `Request failed (${status}).`;
}

function parseRetryAfter(value: string | null): number | null {
  if (!value) return null;
  const n = Number(value);
  if (Number.isFinite(n) && n >= 0) return Math.ceil(n);
  const date = Date.parse(value);
  if (Number.isNaN(date)) return null;
  return Math.max(0, Math.ceil((date - Date.now()) / 1000));
}

async function readBody(res: Response): Promise<unknown> {
  const text = await res.text();
  if (!text) return undefined;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

export async function request<T>(method: Method, path: string, opts: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET") headers["X-Secretary-CSRF"] = "1";
  let body: string | undefined;
  if (opts.body !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(opts.body);
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);

  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      body,
      credentials: "same-origin",
      cache: "no-store",
      signal: controller.signal,
    });
  } catch (e) {
    clearTimeout(timer);
    if (controller.signal.aborted) {
      throw new ApiError(0, "timeout", "The request took too long. Please try again.");
    }
    throw new ApiError(0, "network", "Can't reach the server. Check your connection.");
  }

  let data: unknown;
  try {
    data = await readBody(res);
  } catch {
    data = undefined;
  } finally {
    clearTimeout(timer);
  }

  if (res.ok) return data as T;

  let code = `http_${res.status}`;
  let message = statusMessage(res.status);
  if (data && typeof data === "object") {
    const body = data as { error?: unknown; code?: unknown };
    const env = body.error;
    if (env && typeof env === "object") {
      const c = (env as { code?: unknown }).code;
      const m = (env as { message?: unknown }).message;
      if (typeof c === "string" && c) code = c;
      if (typeof m === "string" && m) message = m;
    } else if (typeof env === "string" && env) {
      // Engine-shaped body: {ok:false, code:"…", error:"…"}.
      message = env;
    }
    if (typeof body.code === "string" && body.code) code = body.code;
  }
  const err = new ApiError(res.status, code, message, parseRetryAfter(res.headers.get("Retry-After")));

  // /v1/session* answers 401 for wrong credentials; that is not a lost session.
  if (res.status === 401 && !path.startsWith("/v1/session")) onUnauthorized?.();
  if (err.lucinaOffline) onLucinaOffline?.(err);
  throw err;
}
