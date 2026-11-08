// Notifications: the inbox behind the bell, its settings, this device's Web
// Push state, and the presence heartbeat that tells the server the owner is
// looking at the app (so Lucina's messages don't buzz the phone meanwhile).

import { createSignal } from "solid-js";
import { toApiError } from "../api/client";
import {
  notificationsApi,
  type AppNotification,
  type Category,
  type NotificationSettings,
  type SettingsView,
} from "../api/notifications";
import { locale, t, type MessageKey } from "../i18n";
import { disablePush, enablePush, pushState, setAppBadge, type PushState } from "../lib/push";
import { readString, removeKey, writeString } from "../lib/storage";

/** Inbox refresh while the app is visible. */
export const POLL_MS = 60_000;
/** Presence heartbeat while visible and focused (the server keeps it 90 s). */
export const PRESENCE_MS = 45_000;
/** localStorage: the id of this browser's push subscription. */
export const SUB_KEY = "secretary.pushSubscription";

const [items, setItems] = createSignal<AppNotification[]>([]);
const [unread, setUnread] = createSignal(0);
const [loadError, setLoadError] = createSignal<string | null>(null);
const [loaded, setLoaded] = createSignal(false);
const [view, setView] = createSignal<SettingsView | null>(null);
const [push, setPush] = createSignal<PushState>("unsupported");
const [pushBusy, setPushBusy] = createSignal(false);

export { items as notifications, unread as unreadCount, loadError as notificationsError, loaded as notificationsLoaded };
export { view as settingsView, push as pushStatus, pushBusy };

export const CATEGORY_TITLE: Record<Category, MessageKey> = {
  lucina_message: "notify.catLucina",
  reminder: "notify.catReminder",
  calendar: "notify.catCalendar",
  device_confirm: "notify.catDevice",
  agent: "notify.catAgent",
  shopping: "notify.catShopping",
  presence: "notify.catPresence",
  system: "notify.catSystem",
  approval: "notify.catApproval",
};

export const CATEGORY_DESC: Record<Category, MessageKey> = {
  lucina_message: "notify.descLucina",
  reminder: "notify.descReminder",
  calendar: "notify.descCalendar",
  device_confirm: "notify.descDevice",
  agent: "notify.descAgent",
  shopping: "notify.descShopping",
  presence: "notify.descPresence",
  system: "notify.descSystem",
  approval: "notify.descApproval",
};

export function titleOf(n: AppNotification): string {
  return n.title ?? t(CATEGORY_TITLE[n.category]);
}

function applyUnread(count: number): void {
  setUnread(Math.max(0, count));
  setAppBadge(count);
}

export async function refreshNotifications(): Promise<void> {
  try {
    const page = await notificationsApi.list();
    setItems(page.items);
    applyUnread(page.unread);
    setLoadError(null);
  } catch (e) {
    setLoadError(toApiError(e).message);
  } finally {
    setLoaded(true);
  }
}

export async function markRead(ids: string[]): Promise<void> {
  const fresh = ids.filter((id) => items().some((n) => n.id === id && !n.read));
  if (!fresh.length) return;
  const now = Date.now();
  setItems((list) => list.map((n) => (fresh.includes(n.id) ? { ...n, read: true, readAt: now } : n)));
  applyUnread(unread() - fresh.length);
  try {
    const r = await notificationsApi.markRead(fresh);
    applyUnread(r.unread);
  } catch {
    // The next refresh brings back the server's view.
  }
}

export async function markAllRead(): Promise<void> {
  const now = Date.now();
  setItems((list) => list.map((n) => (n.read ? n : { ...n, read: true, readAt: now })));
  applyUnread(0);
  try {
    const r = await notificationsApi.markAllRead();
    applyUnread(r.unread);
  } catch {
    void refreshNotifications();
  }
}

// -- settings ------------------------------------------------------------------

export async function loadSettings(): Promise<void> {
  try {
    setView(await notificationsApi.getSettings());
  } catch (e) {
    setLoadError(toApiError(e).message);
  }
}

/** Save settings (optimistic). Returns an error message, or null. */
export async function saveSettings(next: NotificationSettings): Promise<string | null> {
  const before = view();
  const settings = { ...next, language: locale() };
  if (before) setView({ ...before, settings });
  try {
    setView(await notificationsApi.putSettings(settings));
    return null;
  } catch (e) {
    if (before) setView(before);
    return toApiError(e).message;
  }
}

export async function updateSettings(change: (s: NotificationSettings) => NotificationSettings): Promise<string | null> {
  const v = view();
  if (!v) return null;
  return saveSettings(change(structuredClone(v.settings)));
}

export async function removeDevice(id: string): Promise<string | null> {
  try {
    await notificationsApi.removeSubscription(id);
    if (thisSubscriptionId() === id) {
      await disablePush().catch(() => undefined);
      removeKey("local", SUB_KEY);
      setPush(await pushState());
    }
    await loadSettings();
    return null;
  } catch (e) {
    return toApiError(e).message;
  }
}

// -- this device's push ---------------------------------------------------------

export function thisSubscriptionId(): string | null {
  return readString("local", SUB_KEY);
}

export async function refreshPushState(): Promise<void> {
  setPush(await pushState());
}

/** From a click only: asks for permission, subscribes, registers with the server. */
export async function turnOnPush(): Promise<string | null> {
  if (pushBusy()) return null;
  setPushBusy(true);
  try {
    const r = await enablePush(locale());
    setPush(r.state);
    if (r.subscriptionId) writeString("local", SUB_KEY, r.subscriptionId);
    if (r.state === "on") await loadSettings();
    return null;
  } catch (e) {
    setPush(await pushState());
    return t("notify.pushFailed", { error: toApiError(e).message });
  } finally {
    setPushBusy(false);
  }
}

export async function turnOffPush(): Promise<void> {
  if (pushBusy()) return;
  setPushBusy(true);
  try {
    setPush(await disablePush());
    removeKey("local", SUB_KEY);
    await loadSettings();
  } finally {
    setPushBusy(false);
  }
}

/** A test push to this device. Returns a status line for the UI. */
export async function sendTest(): Promise<{ ok: boolean; message: string }> {
  try {
    const id = thisSubscriptionId() ?? undefined;
    const r = await notificationsApi.test(id);
    const bad = r.results.find((x) => !x.ok);
    if (bad) {
      const error = bad.delivery.error ?? String(bad.delivery.status ?? bad.delivery.result);
      return { ok: false, message: t("notify.pushTestFailed", { error }) };
    }
    return { ok: true, message: t("notify.pushTestSent") };
  } catch (e) {
    return { ok: false, message: t("notify.pushTestFailed", { error: toApiError(e).message }) };
  }
}

// -- lifecycle -------------------------------------------------------------------

function visible(): boolean {
  try {
    return document.visibilityState === "visible";
  } catch {
    return true;
  }
}

function focused(): boolean {
  try {
    return visible() && (typeof document.hasFocus !== "function" || document.hasFocus());
  } catch {
    return true;
  }
}

let lastPresence: boolean | null = null;

async function sendPresence(active: boolean, force = false): Promise<void> {
  if (!force && lastPresence === active && !active) return;
  lastPresence = active;
  try {
    await notificationsApi.presence(active);
  } catch {
    // Best effort: the server forgets presence after 90 s anyway.
  }
}

/** A route a notification may open: an app path, never another origin. */
export function safeRoute(route: unknown): string {
  return typeof route === "string" && route.startsWith("/") && !route.startsWith("//") && !route.includes("\\")
    ? route
    : "/";
}

/** `?notification=<id>` (set by the service worker when it opened this window). */
function consumeLaunchParam(): void {
  try {
    const url = new URL(window.location.href);
    const id = url.searchParams.get("notification");
    if (!id) return;
    url.searchParams.delete("notification");
    window.history.replaceState(window.history.state, "", url.pathname + url.search + url.hash);
    void refreshNotifications().then(() => markRead([id]));
  } catch {
    // Ignore.
  }
}

/**
 * Start polling, presence and the service-worker bridge (call once when
 * signed in); returns the cleanup. `navigate` opens a notification's route.
 */
export function startNotifications(navigate: (route: string) => void): () => void {
  consumeLaunchParam();
  void refreshNotifications();
  void refreshPushState();
  if (focused()) void sendPresence(true, true);

  const poll = setInterval(() => {
    if (visible()) void refreshNotifications();
  }, POLL_MS);
  const heartbeat = setInterval(() => {
    if (focused()) void sendPresence(true, true);
  }, PRESENCE_MS);

  const onVisibility = () => {
    if (visible()) {
      void refreshNotifications();
      void refreshPushState();
      if (focused()) void sendPresence(true, true);
    } else {
      void sendPresence(false);
    }
  };
  const onFocus = () => void sendPresence(true, true);
  const onBlur = () => void sendPresence(false);
  const onMessage = (e: MessageEvent) => {
    const data = e.data as { type?: string; route?: unknown; id?: unknown } | null;
    if (!data || typeof data !== "object") return;
    if (data.type === "push") void refreshNotifications();
    if (data.type === "open") {
      navigate(safeRoute(data.route));
      if (typeof data.id === "string") void refreshNotifications().then(() => markRead([data.id as string]));
    }
  };

  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("focus", onFocus);
  window.addEventListener("blur", onBlur);
  const sw = typeof navigator !== "undefined" && "serviceWorker" in navigator ? navigator.serviceWorker : null;
  sw?.addEventListener("message", onMessage);

  return () => {
    clearInterval(poll);
    clearInterval(heartbeat);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("focus", onFocus);
    window.removeEventListener("blur", onBlur);
    sw?.removeEventListener("message", onMessage);
    void sendPresence(false);
  };
}

/** Forget everything (sign-out). */
export function resetNotifications(): void {
  setItems([]);
  applyUnread(0);
  setView(null);
  setLoaded(false);
  setLoadError(null);
  lastPresence = null;
}

/** Test helper. */
export function setNotificationsForTests(list: AppNotification[], count?: number): void {
  setItems(list);
  setUnread(count ?? list.filter((n) => !n.read).length);
  setLoaded(true);
}
