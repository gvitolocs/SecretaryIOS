import { vi } from "vitest";

export interface Call {
  method: string;
  url: string;
  headers: Record<string, string>;
  body: unknown;
  credentials: RequestCredentials | undefined;
}

export type Handler = (call: Call) => { status?: number; body?: unknown; headers?: Record<string, string> } | Promise<{ status?: number; body?: unknown; headers?: Record<string, string> }>;

/** Stub global fetch with a single handler; returns the recorded calls. */
export function stubFetch(handler: Handler): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries((init.headers ?? {}) as Record<string, string>)) headers[k.toLowerCase()] = v;
      const call: Call = {
        method: init.method ?? "GET",
        url: String(input),
        headers,
        body: typeof init.body === "string" ? JSON.parse(init.body) : undefined,
        credentials: init.credentials,
      };
      calls.push(call);
      const r = await handler(call);
      const status = r.status ?? 200;
      const text = r.body === undefined ? "" : JSON.stringify(r.body);
      return new Response(status === 204 ? null : text, {
        status,
        headers: { "Content-Type": "application/json", ...(r.headers ?? {}) },
      });
    }),
  );
  return calls;
}

export function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}
