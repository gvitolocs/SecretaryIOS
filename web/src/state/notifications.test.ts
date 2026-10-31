import { afterEach, describe, expect, it, vi } from "vitest";
import { parsePage } from "../api/notifications";
import { setLocale } from "../i18n";
import { deviceLabel, pushSupport, urlBase64ToBytes } from "../lib/push";
import { stubFetch } from "../test/fetch";
import {
  loadSettings,
  markAllRead,
  markRead,
  notifications,
  refreshNotifications,
  resetNotifications,
  safeRoute,
  saveSettings,
  settingsView,
  unreadCount,
  startNotifications,
} from "./notifications";

const SETTINGS = {
  categories: {
    lucina_message: true,
    reminder: true,
    calendar: true,
    device_confirm: true,
    agent: true,
    shopping: true,
    presence: false,
    system: false,
  },
  quietHours: { enabled: false, start: "22:30", end: "07:30" },
  timeZone: "Europe/Rome",
  dndUntil: null,
  calendarLeadMinutes: 10,
  remindersBypassQuiet: true,
  digest: true,
  previews: true,
  language: "en",
  devices: {},
};

const VIEW = {
  settings: SETTINGS,
  subscriptions: [],
  currentDevice: { id: "dev_1", name: "web" },
  webPush: { enabled: true, publicKey: "BAAA" },
  apns: { enabled: false, environment: null },
  present: false,
};

afterEach(() => resetNotifications());

describe("notifications state", () => {
  it("parses pages defensively", () => {
    const page = parsePage({
      items: [
        { id: "n_1", ts: 1, category: "reminder", body: "x", route: "//evil.example", read: false },
        { id: "n_2", ts: 2, category: "weather", body: "x", route: "/", read: false },
        { id: 3, ts: 3, category: "reminder" },
        { id: "n_4", ts: 4, category: "shopping", body: "y", route: "/shopping", read: true, title: " Custom " },
      ],
      unread: 7,
      nextBefore: "n_1",
    });
    expect(page.items.map((n) => n.id)).toEqual(["n_1", "n_4"]);
    expect(page.items[0]!.route).toBe("/");
    expect(page.items[1]!.title).toBe("Custom");
    expect(page.unread).toBe(7);
    expect(parsePage(null)).toEqual({ items: [], unread: 0, nextBefore: null });
  });

  it("marks read optimistically and takes the server's count", async () => {
    const calls = stubFetch((c) => {
      if (c.url.startsWith("/v1/notifications?")) {
        return {
          body: {
            items: [
              { id: "n_2", ts: 2, category: "lucina_message", body: "b", route: "/", read: false },
              { id: "n_1", ts: 1, category: "reminder", body: "a", route: "/", read: false },
            ],
            unread: 2,
            nextBefore: null,
          },
        };
      }
      if (c.url === "/v1/notifications/read") return { body: c.body && (c.body as { all?: boolean }).all ? { updated: 1, unread: 0 } : { updated: 1, unread: 1 } };
      return { status: 404 };
    });
    await refreshNotifications();
    expect(unreadCount()).toBe(2);
    await markRead(["n_2", "n_2", "nope"]);
    expect(notifications().find((n) => n.id === "n_2")!.read).toBe(true);
    expect(unreadCount()).toBe(1);
    await markRead(["n_2"]); // already read: no request
    expect(calls.filter((c) => c.url === "/v1/notifications/read")).toHaveLength(1);
    await markAllRead();
    expect(unreadCount()).toBe(0);
    expect(calls.at(-1)!.body).toEqual({ all: true });
  });

  it("saves settings with the UI language and rolls back on failure", async () => {
    let fail = false;
    const calls = stubFetch((c) => {
      if (c.url === "/v1/notifications/settings" && c.method === "GET") return { body: VIEW };
      if (c.url === "/v1/notifications/settings" && c.method === "PUT") {
        if (fail) return { status: 400, body: { error: { code: "bad_request", message: "quietHours.end: expected HH:MM" } } };
        return { body: { ...VIEW, settings: c.body } };
      }
      return { status: 404 };
    });
    await loadSettings();
    setLocale("it");
    expect(await saveSettings({ ...SETTINGS, previews: false })).toBeNull();
    const put = calls.find((c) => c.method === "PUT")!;
    expect(put.headers["x-secretary-csrf"]).toBe("1");
    expect(put.body).toMatchObject({ previews: false, language: "it" });
    expect(settingsView()!.settings.previews).toBe(false);
    fail = true;
    expect(await saveSettings({ ...SETTINGS, previews: true })).toContain("quietHours.end");
    expect(settingsView()!.settings.previews).toBe(false);
    setLocale(null);
  });

  it("navigates to a decision from a service-worker open message", () => {
    stubFetch(() => ({ status: 404, body: {} }));
    const sw = new EventTarget();
    vi.stubGlobal("navigator", { serviceWorker: sw });
    const navigate = vi.fn();
    const stop = startNotifications(navigate);
    try {
      sw.dispatchEvent(new MessageEvent("message", { data: { type: "open", route: "/decisions/dc-0123456789ab" } }));
      expect(navigate).toHaveBeenCalledWith("/decisions/dc-0123456789ab");
    } finally { stop(); }
  });

  it("only opens app routes", () => {
    expect(safeRoute("/shopping")).toBe("/shopping");
    expect(safeRoute("//evil.example/x")).toBe("/");
    expect(safeRoute("https://evil.example")).toBe("/");
    expect(safeRoute("/\\evil")).toBe("/");
    expect(safeRoute(42)).toBe("/");
  });
});

describe("push helpers", () => {
  it("decodes the VAPID key", () => {
    expect(Array.from(urlBase64ToBytes("AQID_-8"))).toEqual([1, 2, 3, 255, 239]);
  });

  it("labels devices", () => {
    const iphone = {
      userAgent:
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
      maxTouchPoints: 5,
    } as Navigator;
    expect(deviceLabel(iphone)).toBe("iPhone · Safari");
    const mac = {
      userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Safari/537.36",
      maxTouchPoints: 0,
    } as Navigator;
    expect(deviceLabel(mac)).toBe("Mac · Chrome");
  });

  it("asks iPhone Safari tabs to add the app to the Home Screen", () => {
    // jsdom: no PushManager, not standalone.
    const ua = Object.getOwnPropertyDescriptor(Navigator.prototype, "userAgent");
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      get: () => "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Safari/604.1",
    });
    try {
      expect(pushSupport()).toBe("needs-install");
    } finally {
      delete (navigator as unknown as Record<string, unknown>).userAgent;
      if (ua) Object.defineProperty(Navigator.prototype, "userAgent", ua);
    }
    expect(pushSupport()).toBe("unsupported");
  });
});
