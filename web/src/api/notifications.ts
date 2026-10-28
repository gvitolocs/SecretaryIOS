// Notifications API (docs/server-api.md, "Notifications"): the inbox, its
// settings, presence heartbeats and Web Push subscriptions.

import { request } from "./client";

export const CATEGORIES = [
  "lucina_message",
  "reminder",
  "calendar",
  "device_confirm",
  "agent",
  "shopping",
  "presence",
  "system",
  "approval",
] as const;
export type Category = (typeof CATEGORIES)[number];

export interface AppNotification {
  id: string;
  ts: number;
  category: Category;
  title?: string;
  body: string;
  route: string;
  channel?: string;
  timelineId?: string;
  data?: Record<string, unknown>;
  read: boolean;
  readAt?: number;
}

export interface NotificationPage {
  items: AppNotification[];
  unread: number;
  nextBefore: string | null;
}

export interface DevicePrefs {
  enabled: boolean;
  categories: Partial<Record<Category, boolean>>;
}

export interface NotificationSettings {
  /** Older servers have no "approval" entry: those requests are always on. */
  categories: Partial<Record<Category, boolean>>;
  quietHours: { enabled: boolean; start: string; end: string };
  timeZone: string;
  dndUntil: number | null;
  calendarLeadMinutes: number;
  remindersBypassQuiet: boolean;
  digest: boolean;
  previews: boolean;
  language: string;
  devices: Record<string, DevicePrefs>;
}

export interface PushSubscriptionInfo {
  id: string;
  kind: "webpush" | "apns";
  deviceId: string;
  deviceName: string;
  label: string;
  service: string;
  createdAt: number;
  lastSuccessAt: number | null;
  failures: number;
  lastError: string | null;
}

export interface SettingsView {
  settings: NotificationSettings;
  subscriptions: PushSubscriptionInfo[];
  currentDevice: { id: string; name: string };
  webPush: { enabled: boolean; publicKey: string };
  apns: { enabled: boolean; environment: string | null };
  present: boolean;
}

export interface TestResult {
  id: string;
  ok: boolean;
  delivery: { result: "sent" | "gone" | "failed"; status?: number; error?: string };
}

function isCategory(v: unknown): v is Category {
  return typeof v === "string" && (CATEGORIES as readonly string[]).includes(v);
}

/** Network data is untrusted: keep well-formed entries only. */
export function parseNotification(v: unknown): AppNotification | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  if (typeof o.id !== "string" || !o.id || !isCategory(o.category)) return null;
  if (typeof o.ts !== "number" || !Number.isFinite(o.ts)) return null;
  const route = typeof o.route === "string" && o.route.startsWith("/") && !o.route.startsWith("//") ? o.route : "/";
  const n: AppNotification = {
    id: o.id,
    ts: o.ts,
    category: o.category,
    body: typeof o.body === "string" ? o.body : "",
    route,
    read: o.read === true,
  };
  if (typeof o.title === "string" && o.title.trim()) n.title = o.title.trim();
  if (typeof o.channel === "string") n.channel = o.channel;
  if (typeof o.timelineId === "string") n.timelineId = o.timelineId;
  if (o.data && typeof o.data === "object") n.data = o.data as Record<string, unknown>;
  if (typeof o.readAt === "number") n.readAt = o.readAt;
  return n;
}

export function parsePage(v: unknown): NotificationPage {
  const o = (v && typeof v === "object" ? v : {}) as Record<string, unknown>;
  const items = Array.isArray(o.items) ? o.items.map(parseNotification).filter((n): n is AppNotification => !!n) : [];
  return {
    items,
    unread: typeof o.unread === "number" ? o.unread : items.filter((n) => !n.read).length,
    nextBefore: typeof o.nextBefore === "string" ? o.nextBefore : null,
  };
}

export const notificationsApi = {
  list: async (before: string | null = null, limit = 50) =>
    parsePage(
      await request<unknown>(
        "GET",
        `/v1/notifications?limit=${limit}${before ? `&before=${encodeURIComponent(before)}` : ""}`,
      ),
    ),
  markRead: (ids: string[]) =>
    request<{ updated: number; unread: number }>("POST", "/v1/notifications/read", { body: { ids } }),
  markAllRead: () =>
    request<{ updated: number; unread: number }>("POST", "/v1/notifications/read", { body: { all: true } }),
  getSettings: () => request<SettingsView>("GET", "/v1/notifications/settings"),
  putSettings: (settings: NotificationSettings) =>
    request<SettingsView>("PUT", "/v1/notifications/settings", { body: settings }),
  presence: (active: boolean) =>
    request<{ present: boolean }>("POST", "/v1/notifications/presence", { body: { active }, timeoutMs: 8_000 }),
  pushKey: () => request<{ publicKey: string }>("GET", "/v1/push/key"),
  subscribe: (sub: PushSubscriptionJSON, label: string, lang: string) =>
    request<PushSubscriptionInfo>("POST", "/v1/push/subscribe", {
      body: { endpoint: sub.endpoint, expirationTime: sub.expirationTime ?? null, keys: sub.keys, label, lang },
    }),
  unsubscribe: (endpoint: string) => request<{ removed: number }>("DELETE", "/v1/push/subscribe", { body: { endpoint } }),
  removeSubscription: (id: string) =>
    request<{ removed: number }>("DELETE", `/v1/push/subscriptions/${encodeURIComponent(id)}`),
  test: (id?: string) =>
    request<{ results: TestResult[] }>("POST", "/v1/push/test", { body: id ? { id } : {}, timeoutMs: 60_000 }),
};
