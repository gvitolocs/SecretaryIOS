// The service worker (public/sw.js) in a fake ServiceWorkerGlobalScope:
// it must never intercept or cache /v1, cache only static files, show
// pushes and open only app routes.

import { describe, expect, it, vi } from "vitest";
import SOURCE from "../public/sw.js?raw";

const ORIGIN = "https://getsecretary.tech";

type Listener = (event: Record<string, unknown>) => void;

function load(opts: { online?: boolean; windows?: Array<{ url: string }> } = {}) {
  const listeners = new Map<string, Listener>();
  const store = new Map<string, Response>();
  const putKeys: string[] = [];
  const cache = {
    match: vi.fn(async (req: Request | string) => store.get(typeof req === "string" ? req : new URL(req.url).pathname)?.clone()),
    put: vi.fn(async (req: Request | string, res: Response) => {
      const key = typeof req === "string" ? req : new URL(req.url).pathname;
      putKeys.push(key);
      store.set(key, res);
    }),
    addAll: vi.fn(async (keys: string[]) => {
      for (const k of keys) store.set(k, new Response(`cached ${k}`, { headers: { "content-type": "text/html" } }));
    }),
  };
  const caches = { open: vi.fn(async () => cache), keys: vi.fn(async () => ["secretary-static-v0"]), delete: vi.fn(async () => true) };
  const fetchMock = vi.fn(async (req: Request) => {
    if (opts.online === false) throw new TypeError("offline");
    const res = new Response(`net ${new URL(req.url).pathname}`, { status: 200, headers: { "content-type": "text/html" } });
    Object.defineProperty(res, "type", { value: "basic" });
    return res;
  });
  const shown: Array<{ title: string; options: Record<string, unknown> }> = [];
  const windows = (opts.windows ?? []).map((w) => ({ ...w, focus: vi.fn(async () => undefined), postMessage: vi.fn() }));
  const self = {
    location: new URL(ORIGIN + "/sw.js"),
    addEventListener: (type: string, fn: Listener) => listeners.set(type, fn),
    skipWaiting: vi.fn(async () => undefined),
    clients: { claim: vi.fn(), matchAll: vi.fn(async () => windows), openWindow: vi.fn(async () => null) },
    registration: {
      showNotification: vi.fn(async (title: string, options: Record<string, unknown>) => {
        shown.push({ title, options });
      }),
    },
    navigator: {},
  };
  new Function("self", "caches", "fetch", SOURCE)(self, caches, fetchMock);

  function dispatch(type: string, fields: Record<string, unknown>) {
    let responded: Promise<Response> | null = null;
    const waits: Promise<unknown>[] = [];
    listeners.get(type)!({
      ...fields,
      respondWith: (p: Promise<Response>) => (responded = p),
      waitUntil: (p: Promise<unknown>) => waits.push(p),
    });
    return { responded: responded as Promise<Response> | null, done: Promise.all(waits) };
  }

  function request(path: string, init: RequestInit & { mode?: RequestMode } = {}) {
    const req = new Request(ORIGIN + path, { method: init.method ?? "GET", headers: init.headers });
    if (init.mode) Object.defineProperty(req, "mode", { value: init.mode });
    return req;
  }

  return { dispatch, request, fetchMock, cache, putKeys, shown, self, windows };
}

describe("service worker", () => {
  it("never intercepts or caches the API", async () => {
    const sw = load();
    for (const path of ["/v1/notifications", "/v1/timeline/stream", "/v1/assets/lucina.glb", "/v1"]) {
      expect(sw.dispatch("fetch", { request: sw.request(path) }).responded).toBeNull();
    }
    expect(sw.dispatch("fetch", { request: sw.request("/assets/app.js", { method: "POST" }) }).responded).toBeNull();
    expect(sw.dispatch("fetch", { request: new Request("https://fcm.googleapis.com/x") }).responded).toBeNull();
    expect(sw.fetchMock).not.toHaveBeenCalled();
    expect(sw.putKeys).toEqual([]);
  });

  it("caches hashed assets cache-first and icons", async () => {
    const sw = load();
    const first = await sw.dispatch("fetch", { request: sw.request("/assets/app-3f2a.js") }).responded!;
    expect(await first.text()).toBe("net /assets/app-3f2a.js");
    const second = await sw.dispatch("fetch", { request: sw.request("/assets/app-3f2a.js") }).responded!;
    expect(await second.text()).toBe("net /assets/app-3f2a.js");
    expect(sw.fetchMock).toHaveBeenCalledTimes(1);
    expect(sw.dispatch("fetch", { request: sw.request("/icon-192.png") }).responded).not.toBeNull();
    expect(sw.dispatch("fetch", { request: sw.request("/robots.txt") }).responded).toBeNull();
  });

  it("serves the cached shell for page loads when offline", async () => {
    const sw = load({ online: false });
    await sw.dispatch("install", {}).done;
    const page = await sw.dispatch("fetch", { request: sw.request("/shopping", { mode: "navigate" }) }).responded!;
    expect(await page.text()).toBe("cached /index.html");
  });

  it("shows a push and tells open windows", async () => {
    const sw = load({ windows: [{ url: ORIGIN + "/" }] });
    const payload = { id: "n_1", category: "reminder", title: "Promemoria", body: "Dentista", route: "//evil.example", tag: "x", ts: 5, unread: 3 };
    await sw.dispatch("push", { data: { json: () => payload, text: () => JSON.stringify(payload) } }).done;
    expect(sw.shown).toHaveLength(1);
    expect(sw.shown[0]!.title).toBe("Promemoria");
    expect(sw.shown[0]!.options).toMatchObject({ body: "Dentista", tag: "x", renotify: true, data: { id: "n_1", route: "/" } });
    expect(sw.windows[0]!.postMessage).toHaveBeenCalledWith({ type: "push", id: "n_1", category: "reminder" });
  });

  it("opens the route on click: focuses a window or opens one", async () => {
    const closed = vi.fn();
    const notification = { close: closed, data: { id: "n_9", route: "/shopping" } };
    const none = load();
    await none.dispatch("notificationclick", { notification }).done;
    expect(closed).toHaveBeenCalled();
    expect(none.self.clients.openWindow).toHaveBeenCalledWith("/shopping?notification=n_9");

    const open = load({ windows: [{ url: ORIGIN + "/gym" }] });
    await open.dispatch("notificationclick", { notification }).done;
    expect(open.windows[0]!.focus).toHaveBeenCalled();
    expect(open.windows[0]!.postMessage).toHaveBeenCalledWith({ type: "open", route: "/shopping", id: "n_9" });
    expect(open.self.clients.openWindow).not.toHaveBeenCalled();
  });
});
