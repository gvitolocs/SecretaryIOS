// Secretary service worker: Web Push and a small static-asset cache.
//
// Rules:
// - Never touches /v1/ (the authenticated API): those requests go to the
//   network untouched and nothing from them is ever stored.
// - Caches only same-origin static files: the content-hashed build output
//   (/assets/*, cache-first) and the icons/manifest (stale-while-revalidate).
//   Page loads go to the network first; the last good /index.html is kept so
//   the installed app still opens offline.
// - Shows pushes (encrypted end to end; the payload is JSON from the server)
//   and opens the right route when one is tapped.

const VERSION = "v1";
const CACHE = `secretary-static-${VERSION}`;
const SHELL = "/index.html";
const ICON = "/icon-192.png";
const BADGE = "/favicon-32.png";
const STATIC_FILES = /^\/(icon-\d+\.png|apple-touch-icon\.png|favicon[^/]*|manifest\.webmanifest)$/;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll([SHELL, ICON]))
      .catch(() => undefined)
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("secretary-static-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

/** Whether a response may be stored: a plain same-origin 200, not a redirect. */
function cacheable(response) {
  return !!response && response.ok && response.status === 200 && response.type === "basic" && !response.redirected;
}

function isApi(url) {
  return url.pathname === "/v1" || url.pathname.startsWith("/v1/");
}

async function cacheFirst(request) {
  const cache = await caches.open(CACHE);
  const hit = await cache.match(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (cacheable(response)) await cache.put(request, response.clone());
  return response;
}

async function staleWhileRevalidate(request) {
  const cache = await caches.open(CACHE);
  const hit = await cache.match(request);
  const fresh = fetch(request)
    .then((response) => {
      if (cacheable(response)) void cache.put(request, response.clone());
      return response;
    })
    .catch(() => hit);
  return hit || fresh;
}

async function networkFirstPage(request) {
  const cache = await caches.open(CACHE);
  try {
    const response = await fetch(request);
    if (cacheable(response) && (response.headers.get("content-type") || "").includes("text/html")) {
      await cache.put(SHELL, response.clone());
    }
    return response;
  } catch (err) {
    const shell = await cache.match(SHELL);
    if (shell) return shell;
    throw err;
  }
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  // The API (cookies, Bearer tokens, private data): never intercepted, never cached.
  if (isApi(url)) return;
  if (request.headers.has("authorization") || request.headers.has("range")) return;
  if (request.mode === "navigate") {
    event.respondWith(networkFirstPage(request));
    return;
  }
  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(cacheFirst(request));
    return;
  }
  if (STATIC_FILES.test(url.pathname)) {
    event.respondWith(staleWhileRevalidate(request));
  }
  // Anything else: the browser's default handling.
});

/** An app path from the payload; anything else opens the home route. */
function safeRoute(route) {
  return typeof route === "string" && route.startsWith("/") && !route.startsWith("//") && !route.includes("\\")
    ? route
    : "/";
}

function parsePayload(event) {
  try {
    return event.data ? event.data.json() : {};
  } catch {
    try {
      return { body: event.data ? event.data.text() : "" };
    } catch {
      return {};
    }
  }
}

self.addEventListener("push", (event) => {
  const p = parsePayload(event) || {};
  const title = typeof p.title === "string" && p.title ? p.title : "Secretary";
  const options = {
    body: typeof p.body === "string" ? p.body : "",
    icon: ICON,
    badge: BADGE,
    tag: typeof p.tag === "string" && p.tag ? p.tag : undefined,
    renotify: typeof p.tag === "string" && p.tag ? true : undefined,
    timestamp: typeof p.ts === "number" ? p.ts : Date.now(),
    data: { id: typeof p.id === "string" ? p.id : null, route: safeRoute(p.route), category: p.category || null },
  };
  event.waitUntil(
    (async () => {
      await self.registration.showNotification(title, options);
      if (typeof p.unread === "number" && self.navigator && "setAppBadge" in self.navigator) {
        try {
          if (p.unread > 0) await self.navigator.setAppBadge(p.unread);
          else await self.navigator.clearAppBadge();
        } catch {
          // Not supported.
        }
      }
      const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const w of windows) w.postMessage({ type: "push", id: options.data.id, category: options.data.category });
    })(),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const data = event.notification.data || {};
  const route = safeRoute(data.route);
  const id = typeof data.id === "string" ? data.id : null;
  event.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const mine = windows.find((w) => new URL(w.url).origin === self.location.origin);
      if (mine) {
        await mine.focus();
        mine.postMessage({ type: "open", route, id });
        return;
      }
      const target = new URL(route, self.location.origin);
      if (id) target.searchParams.set("notification", id);
      await self.clients.openWindow(target.pathname + target.search);
    })(),
  );
});

// The push service rotated the subscription: subscribe again with the same
// server key and tell the server (cookie session + CSRF header, same origin).
self.addEventListener("pushsubscriptionchange", (event) => {
  event.waitUntil(
    (async () => {
      const old = event.oldSubscription;
      const key = old && old.options ? old.options.applicationServerKey : null;
      if (!key) return;
      const sub = await self.registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
      const json = sub.toJSON();
      await fetch("/v1/push/subscribe", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-Secretary-CSRF": "1" },
        body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys, label: "Browser" }),
      });
      if (old) {
        await fetch("/v1/push/subscribe", {
          method: "DELETE",
          credentials: "same-origin",
          headers: { "Content-Type": "application/json", "X-Secretary-CSRF": "1" },
          body: JSON.stringify({ endpoint: old.endpoint }),
        }).catch(() => undefined);
      }
    })().catch(() => undefined),
  );
});
