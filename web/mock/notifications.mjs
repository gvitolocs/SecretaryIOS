// Mock notifications API (docs/server-api.md, "Notifications"), in memory.
//   GET  /v1/notifications?limit=&before=&unread=1
//   POST /v1/notifications/read {ids?|all}
//   GET|PUT /v1/notifications/settings
//   POST /v1/notifications/presence {active}
//   GET  /v1/push/key, POST|DELETE /v1/push/subscribe, DELETE /v1/push/subscriptions/{id}, POST /v1/push/test
// Test hook: POST /v1/_mock/notify {category, body, title?, route?} adds one (CSRF header needed).
// Lucina items added to the mock timeline also become `lucina_message` notifications.
// MOCK_NOTIFY_EMPTY=1 starts with an empty inbox.

import { randomUUID } from "node:crypto";

const CATEGORIES = ["lucina_message", "reminder", "calendar", "device_confirm", "shopping", "presence", "system"];
// A fixed example P-256 point (not a real server key).
const PUBLIC_KEY = "BEl62iUYgUivxIkv69yViEuiBIa-Ib9-SkvMeAtA3LFgDzkrxZJjSgSnfckjBJuBkr3qBUYIHBQFLXYp5Nksh8U";

const settings = {
  categories: Object.fromEntries(CATEGORIES.map((c) => [c, c !== "presence" && c !== "system"])),
  quietHours: { enabled: true, start: "22:30", end: "07:30" },
  timeZone: "Europe/Rome",
  dndUntil: null,
  calendarLeadMinutes: 10,
  remindersBypassQuiet: true,
  digest: true,
  previews: true,
  language: "en",
  devices: {},
};

const subscriptions = [
  {
    id: "ps_iphone",
    kind: "webpush",
    deviceId: "dev_web",
    deviceName: "web",
    label: "iPhone · Safari (app)",
    service: "web.push.apple.com",
    createdAt: Date.now() - 6 * 86_400_000,
    lastSuccessAt: Date.now() - 42 * 60_000,
    failures: 0,
    lastError: null,
  },
];

let seq = 0;
const inbox = [];

function nid(ts) {
  seq += 1;
  return `n_${String(ts).padStart(13, "0")}${String(seq).padStart(8, "0")}`;
}

export function addNotification(fields, ts = Date.now()) {
  const n = { id: nid(ts), ts, read: false, route: "/", ...fields };
  inbox.push(n);
  inbox.sort((a, b) => (a.id < b.id ? -1 : 1));
  return n;
}

function seed() {
  if (process.env.MOCK_NOTIFY_EMPTY === "1") return;
  const now = Date.now();
  const M = 60_000;
  addNotification({ category: "reminder", body: "Call the dentist about Thursday", channel: "telegram" }, now - 4 * M);
  addNotification({ category: "lucina_message", body: "Done: I moved your gym session to 18:30 and added protein bars to the list.", channel: "discord" }, now - 18 * M);
  addNotification({ category: "calendar", body: "Team stand-up · 10:00", data: { eventId: "E1", start: now + 10 * M } }, now - 52 * M);
  addNotification({ category: "device_confirm", body: "Delete the 7:00 alarm on your iPhone?", data: { commandId: "c1", type: "alarm.delete" } }, now - 3 * 60 * M, );
  addNotification({ category: "shopping", body: "Lucina added oat milk and bananas to “Casa”.", route: "/shopping", read: true }, now - 26 * 60 * M);
  addNotification({ category: "lucina_message", body: "Good morning! Rain after 15:00, take an umbrella.", channel: "telegram", read: true }, now - 30 * 60 * M);
  for (const n of inbox) if (n.read) n.readAt = n.ts + 60_000;
}
seed();

function view(deviceName) {
  return {
    settings,
    categories: CATEGORIES.map((id) => ({ id, defaultOn: id !== "presence" && id !== "system" })),
    subscriptions,
    currentDevice: { id: "dev_web", name: deviceName ?? "web" },
    webPush: { enabled: true, publicKey: PUBLIC_KEY },
    apns: { enabled: false, environment: null },
    present: true,
  };
}

const unread = () => inbox.filter((n) => !n.read).length;

/** Returns true when it handled the request. */
export async function handleNotifications(req, res, { path, method, url, send, fail, readJson }) {
  if (path === "/v1/_mock/notify" && method === "POST") {
    const b = await readJson(req);
    const category = CATEGORIES.includes(b.category) ? b.category : "lucina_message";
    send(res, 201, addNotification({ category, body: String(b.body ?? "Hello from the mock."), ...(b.title ? { title: b.title } : {}), ...(b.route ? { route: b.route } : {}) }));
    return true;
  }
  if (path === "/v1/notifications" && method === "GET") {
    const limit = Math.min(200, Math.max(1, Number(url.searchParams.get("limit") ?? 50) || 50));
    const before = url.searchParams.get("before");
    const onlyUnread = url.searchParams.get("unread") === "1";
    const all = [...inbox].reverse().filter((n) => (!before || n.id < before) && (!onlyUnread || !n.read));
    const items = all.slice(0, limit);
    send(res, 200, { items, unread: unread(), nextBefore: all.length > limit ? items.at(-1).id : null });
    return true;
  }
  if (path === "/v1/notifications/read" && method === "POST") {
    const b = await readJson(req);
    let updated = 0;
    for (const n of inbox) {
      if (!n.read && (b.all === true || (Array.isArray(b.ids) && b.ids.includes(n.id)))) {
        n.read = true;
        n.readAt = Date.now();
        updated += 1;
      }
    }
    send(res, 200, { updated, unread: unread() });
    return true;
  }
  if (path === "/v1/notifications/settings") {
    if (method === "GET") {
      send(res, 200, view());
      return true;
    }
    if (method === "PUT") {
      const b = await readJson(req);
      if (b?.quietHours && !/^\d\d:\d\d$/.test(b.quietHours.start ?? "")) {
        fail(res, 400, "bad_request", "quietHours.start: expected HH:MM");
        return true;
      }
      Object.assign(settings, b);
      send(res, 200, view());
      return true;
    }
  }
  if (path === "/v1/notifications/presence" && method === "POST") {
    send(res, 200, { present: true });
    return true;
  }
  if (path === "/v1/push/key" && method === "GET") {
    send(res, 200, { publicKey: PUBLIC_KEY });
    return true;
  }
  if (path === "/v1/push/subscribe") {
    const b = await readJson(req);
    if (method === "POST") {
      const sub = {
        id: `ps_${randomUUID().slice(0, 8)}`,
        kind: "webpush",
        deviceId: "dev_web",
        deviceName: "web",
        label: String(b.label ?? "Browser"),
        service: (() => {
          try {
            return new URL(b.endpoint).host;
          } catch {
            return "";
          }
        })(),
        createdAt: Date.now(),
        lastSuccessAt: null,
        failures: 0,
        lastError: null,
      };
      subscriptions.push(sub);
      send(res, 201, sub);
      return true;
    }
    if (method === "DELETE") {
      send(res, 200, { removed: 0 });
      return true;
    }
  }
  const m = path.match(/^\/v1\/push\/subscriptions\/([^/]+)$/);
  if (m && method === "DELETE") {
    const i = subscriptions.findIndex((s) => s.id === m[1]);
    if (i < 0) {
      fail(res, 404, "subscription_not_found", "no such push subscription");
      return true;
    }
    subscriptions.splice(i, 1);
    send(res, 200, { removed: 1 });
    return true;
  }
  if (path === "/v1/push/test" && method === "POST") {
    send(res, 200, { results: subscriptions.map((s) => ({ id: s.id, ok: true, delivery: { result: "sent", status: 201 } })) });
    return true;
  }
  return false;
}
