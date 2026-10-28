// Owner approvals: the pending requests this device may decide (polled while the page is visible), and the
// answer call shared by the pop-up, the /approvals list and the /approvals/:id page.

import { createSignal } from "solid-js";
import { toApiError } from "../api/client";
import { approvalsApi, type Approval, type Decision } from "../api/approvals";

/** Poll interval while the document is visible (contract: web and the Mac app poll every 5 s). */
export const APPROVAL_POLL_MS = 5_000;

const [items, setItems] = createSignal<Approval[]>([]);
/** Ids answered here, or put off with "Later": the pop-up skips them. */
const [skipped, setSkipped] = createSignal<ReadonlySet<string>>(new Set());

export { items as pendingApprovals };

/** What the pop-up shows next: pending, not answered here, not put off. */
export function queuedApprovals(): Approval[] {
  const skip = skipped();
  return items().filter((a) => !skip.has(a.id));
}

export function skipApproval(id: string): void {
  setSkipped((s) => new Set(s).add(id));
}

export function resetApprovals(): void {
  setItems([]);
  setSkipped(new Set<string>());
}

let inflight = false;

/**
 * Fetch the pending list. Quiet on purpose: while the server doesn't have the routes (404) or is unreachable,
 * the pop-up simply has nothing to show; it never raises an error banner.
 */
export async function refreshApprovals(onError?: () => void): Promise<void> {
  if (inflight) return;
  inflight = true;
  try {
    const list = await approvalsApi.listPending();
    // Keep the old object for an unchanged request, so an open card is not rebuilt (and does not lose its
    // scroll position or half-sent answer) on every poll.
    const old = new Map(items().map((a) => [a.id, a]));
    setItems(list.map((a) => {
      const prev = old.get(a.id);
      return prev && JSON.stringify(prev) === JSON.stringify(a) ? prev : a;
    }));
  } catch (e) {
    if (toApiError(e).status === 404) setItems([]);
    else onError?.();
  } finally {
    inflight = false;
  }
}

function visible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

/** Poll while the page is visible; returns the cleanup. Call once when signed in. */
export function startApprovals(): () => void {
  if (visible()) void refreshApprovals();
  const timer = setInterval(() => {
    if (visible()) void refreshApprovals();
  }, APPROVAL_POLL_MS);
  const onVisibility = () => {
    if (visible()) void refreshApprovals();
  };
  // A Web Push for an approval wakes the service worker, which tells open pages.
  const onMessage = (e: MessageEvent) => {
    const d = e.data as { type?: string; category?: string } | null;
    if (d && d.type === "push" && d.category === "approval") void refreshApprovals();
  };
  document.addEventListener("visibilitychange", onVisibility);
  const sw = typeof navigator !== "undefined" && "serviceWorker" in navigator ? navigator.serviceWorker : null;
  sw?.addEventListener("message", onMessage);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisibility);
    sw?.removeEventListener("message", onMessage);
    resetApprovals();
  };
}

/** How a decision ended. "error" and "notAllowed" leave the request open; the rest close it. */
export type Outcome =
  | { kind: "approved" }
  | { kind: "denied" }
  | { kind: "expired" }
  | { kind: "decided" }
  | { kind: "gone" }
  | { kind: "needsFaceId" }
  | { kind: "notMain" }
  | { kind: "changed" }
  | { kind: "error" };

export const CLOSED: readonly Outcome["kind"][] = ["approved", "denied", "expired", "decided", "gone"];

/** Send the owner's answer. The request's own `payloadHash` goes with it. */
export async function decideApproval(a: Approval, decision: Decision): Promise<Outcome> {
  try {
    await approvalsApi.decide(a.id, decision, a.payloadHash);
    skipApproval(a.id);
    void refreshApprovals();
    return { kind: decision === "approve" ? "approved" : "denied" };
  } catch (e) {
    const err = toApiError(e);
    let outcome: Outcome;
    if (err.status === 404) outcome = { kind: "gone" };
    else if (err.status === 410) outcome = { kind: "expired" };
    else if (err.status === 409 && err.code === "payload_mismatch") outcome = { kind: "changed" };
    else if (err.status === 409) outcome = { kind: "decided" };
    else if (err.status === 403 && err.code === "biometric_required") outcome = { kind: "needsFaceId" };
    else if (err.status === 403 && err.code === "not_main_device") outcome = { kind: "notMain" };
    else outcome = { kind: "error" };
    if (CLOSED.includes(outcome.kind)) skipApproval(a.id);
    void refreshApprovals();
    return outcome;
  }
}
