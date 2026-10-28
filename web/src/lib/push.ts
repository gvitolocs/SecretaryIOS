// Web Push on this device: the service worker (/sw.js), permission and the
// PushManager subscription. Permission is only ever asked from a click
// (enablePush), never on load.
//
// iPhone/iPad: Web Push works only for a web app added to the Home Screen
// (iOS 16.4+). In Safari tabs `PushManager` is missing, so `pushSupport()`
// answers "needs-install" and the UI explains how to add it.

import { notificationsApi } from "../api/notifications";

export type PushSupport = "supported" | "needs-install" | "unsupported" | "desktop-app";
export type PushState = "on" | "off" | "denied" | "needs-install" | "unsupported" | "desktop-app";

export const SW_URL = "/sw.js";

function nav(): Navigator | null {
  return typeof navigator === "undefined" ? null : navigator;
}

export function isIos(n: Navigator | null = nav()): boolean {
  if (!n) return false;
  const ua = n.userAgent || "";
  // iPadOS reports itself as a Mac with touch.
  return /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && (n.maxTouchPoints ?? 0) > 1);
}

export function isStandalone(): boolean {
  try {
    if ((nav() as Navigator & { standalone?: boolean })?.standalone === true) return true;
    return typeof matchMedia === "function" && matchMedia("(display-mode: standalone)").matches;
  } catch {
    return false;
  }
}

/**
 * Secretary Code (the Mac app) runs this page in Electron, which has no push service: Web Push registration
 * fails there ("push service not available"). The app shows Lucina's notifications as Mac notifications itself.
 */
export function isDesktopApp(n: Navigator | null = nav()): boolean {
  if (typeof window !== "undefined" && window.secretaryShell) return true;
  return /\bElectron\//.test(n?.userAgent ?? "");
}

export function pushSupport(): PushSupport {
  if (isDesktopApp()) return "desktop-app";
  const n = nav();
  const hasSw = !!n && "serviceWorker" in n;
  const hasPush = typeof window !== "undefined" && "PushManager" in window && "Notification" in window;
  if (hasSw && hasPush) return "supported";
  if (isIos(n) && !isStandalone()) return "needs-install";
  return "unsupported";
}

/** "iPhone · Safari (app)": how this subscription shows up in the device list. */
export function deviceLabel(n: Navigator | null = nav()): string {
  const ua = n?.userAgent ?? "";
  const device = /iPhone/.test(ua)
    ? "iPhone"
    : /iPad/.test(ua) || (/Macintosh/.test(ua) && (n?.maxTouchPoints ?? 0) > 1)
      ? "iPad"
      : /Android/.test(ua)
        ? "Android"
        : /Macintosh|Mac OS X/.test(ua)
          ? "Mac"
          : /Windows/.test(ua)
            ? "Windows"
            : /Linux/.test(ua)
              ? "Linux"
              : "Browser";
  const browser = /Edg\//.test(ua)
    ? "Edge"
    : /Firefox\//.test(ua)
      ? "Firefox"
      : /Chrome\//.test(ua) || /CriOS\//.test(ua)
        ? "Chrome"
        : /Safari\//.test(ua)
          ? "Safari"
          : "";
  const base = browser ? `${device} · ${browser}` : device;
  return isStandalone() ? `${base} (app)` : base;
}

/** base64url → bytes (the VAPID public key for `applicationServerKey`). */
export function urlBase64ToBytes(s: string): Uint8Array<ArrayBuffer> {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const b64 = (s + pad).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

let registration: Promise<ServiceWorkerRegistration | null> | null = null;

/** Register /sw.js once (on app start). Never throws. */
export function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (registration) return registration;
  const n = nav();
  if (!n || !("serviceWorker" in n)) return (registration = Promise.resolve(null));
  registration = n.serviceWorker.register(SW_URL, { scope: "/" }).catch(() => null);
  return registration;
}

async function readyRegistration(): Promise<ServiceWorkerRegistration | null> {
  const reg = await registerServiceWorker();
  if (!reg) return null;
  try {
    return (await nav()?.serviceWorker.ready) ?? reg;
  } catch {
    return reg;
  }
}

export async function currentSubscription(): Promise<PushSubscription | null> {
  if (pushSupport() !== "supported") return null;
  try {
    const reg = await readyRegistration();
    return (await reg?.pushManager.getSubscription()) ?? null;
  } catch {
    return null;
  }
}

export async function pushState(): Promise<PushState> {
  const support = pushSupport();
  if (support !== "supported") return support;
  if (Notification.permission === "denied") return "denied";
  if (Notification.permission !== "granted") return "off";
  return (await currentSubscription()) ? "on" : "off";
}

let cachedKey: string | null = null;

/** Fetch the VAPID key ahead of the click, so the click handler asks first. */
export async function prefetchPushKey(): Promise<void> {
  if (cachedKey || pushSupport() !== "supported") return;
  try {
    cachedKey = (await notificationsApi.pushKey()).publicKey;
  } catch {
    // Fetched again on enable.
  }
}

/**
 * Ask for permission and subscribe. Call this straight from a click handler:
 * Safari only shows the prompt for a user gesture, so the permission request
 * comes before any network wait.
 */
export async function enablePush(lang: string): Promise<{ state: PushState; subscriptionId?: string }> {
  const support = pushSupport();
  if (support !== "supported") return { state: support };
  const permission = await Notification.requestPermission();
  if (permission === "denied") return { state: "denied" };
  if (permission !== "granted") return { state: "off" };
  const reg = await readyRegistration();
  if (!reg) return { state: "unsupported" };
  const key = cachedKey ?? (await notificationsApi.pushKey()).publicKey;
  cachedKey = key;
  let sub = await reg.pushManager.getSubscription();
  const applicationServerKey = urlBase64ToBytes(key);
  if (sub && !sameKey(sub, applicationServerKey)) {
    // Subscribed with another server key (keys were rotated): start over.
    await sub.unsubscribe().catch(() => false);
    sub = null;
  }
  sub ??= await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey });
  const saved = await notificationsApi.subscribe(sub.toJSON(), deviceLabel(), lang);
  return { state: "on", subscriptionId: saved.id };
}

function sameKey(sub: PushSubscription, key: Uint8Array): boolean {
  const k = sub.options?.applicationServerKey;
  if (!k) return true;
  const a = new Uint8Array(k);
  return a.length === key.length && a.every((b, i) => b === key[i]);
}

export async function disablePush(): Promise<PushState> {
  const sub = await currentSubscription();
  if (sub) {
    const endpoint = sub.endpoint;
    await sub.unsubscribe().catch(() => false);
    await notificationsApi.unsubscribe(endpoint).catch(() => undefined);
  }
  return pushState();
}

/** The endpoint of this browser's subscription (to mark "this device" in the list). */
export async function currentEndpoint(): Promise<string | null> {
  return (await currentSubscription())?.endpoint ?? null;
}

/** Home-screen badge (installed web apps); ignored elsewhere. */
export function setAppBadge(count: number): void {
  const n = nav() as (Navigator & { setAppBadge?: (n?: number) => Promise<void>; clearAppBadge?: () => Promise<void> }) | null;
  try {
    if (count > 0) void n?.setAppBadge?.(count)?.catch(() => undefined);
    else void n?.clearAppBadge?.()?.catch(() => undefined);
  } catch {
    // Not supported.
  }
}
