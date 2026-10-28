// Pending decisions: the questions Lucina asked the owner (GET /v1/decisions?status=pending). One shared list, so
// the navigation badge, the /decisions page and the "Lucina needs your decision" banner agree. Polled while the
// tab is visible, and refreshed right after an answer. The count also goes to the desktop rail
// (window.secretaryShell.setBadge, Secretary Code).

import { createSignal } from "solid-js";
import { decisionsApi, type Decision } from "../api/decisions";

/** Poll interval while the document is visible. */
export const DECISIONS_POLL_MS = 30_000;

const [items, setItems] = createSignal<Decision[]>([]);
/** Ids the banner no longer shows: reviewed or dismissed. */
const [dismissed, setDismissed] = createSignal<ReadonlySet<string>>(new Set());

export { items as pendingDecisions };
export const pendingDecisionCount = (): number => items().length;

/** What the banner shows next: pending and not dismissed. */
export function unannouncedDecisions(): Decision[] {
  const skip = dismissed();
  return items().filter((d) => !skip.has(d.decision_id));
}

export function dismissDecision(id: string): void {
  setDismissed((s) => new Set(s).add(id));
}

function apply(list: Decision[]): void {
  // Keep the old objects for unchanged rows, so an open banner is not rebuilt on every poll.
  const old = new Map(items().map((d) => [d.decision_id, d]));
  setItems(list.map((d) => {
    const prev = old.get(d.decision_id);
    return prev && JSON.stringify(prev) === JSON.stringify(d) ? prev : d;
  }));
  try {
    window.secretaryShell?.setBadge?.("decisions", list.length);
  } catch {
    // The rail is not there or refused: the badge in the page is enough.
  }
}

/** The /decisions page just loaded the pending list itself: share it. */
export function setPendingDecisions(list: Decision[]): void {
  apply(list);
}

let inflight = false;

/** Quiet on purpose: while the server is unreachable or lacks the route, the old list stays and nothing errors. */
export async function refreshDecisions(): Promise<void> {
  if (inflight) return;
  inflight = true;
  try {
    apply(await decisionsApi.list("pending"));
  } catch {
    // Offline, or the route is not there: keep what we had.
  } finally {
    inflight = false;
  }
}

export function resetDecisions(): void {
  setItems([]);
  setDismissed(new Set<string>());
  try {
    window.secretaryShell?.setBadge?.("decisions", 0);
  } catch {
    // Ignore.
  }
}

function visible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

/** Poll while the page is visible; returns the cleanup. Call once when signed in. */
export function startDecisions(): () => void {
  if (visible()) void refreshDecisions();
  const timer = setInterval(() => {
    if (visible()) void refreshDecisions();
  }, DECISIONS_POLL_MS);
  const onVisibility = () => {
    if (visible()) void refreshDecisions();
  };
  // A Web Push (a decision is a Lucina message) wakes the service worker, which tells open pages.
  const onMessage = (e: MessageEvent) => {
    const d = e.data as { type?: string } | null;
    if (d && d.type === "push") void refreshDecisions();
  };
  document.addEventListener("visibilitychange", onVisibility);
  const sw = typeof navigator !== "undefined" && "serviceWorker" in navigator ? navigator.serviceWorker : null;
  sw?.addEventListener("message", onMessage);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisibility);
    sw?.removeEventListener("message", onMessage);
  };
}
