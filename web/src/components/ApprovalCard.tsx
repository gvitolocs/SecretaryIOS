// One owner-approval request, Google-style: who is asking, what for, the exact details, a risk badge, how long it
// stays open, and Approve / Deny. Used by the global pop-up and by the /approvals pages.
//
// The web has no Face ID key, so a sensitive request can only be denied here: Approve stays disabled and says
// where to approve it.

import { createEffect, createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";
import type { Approval } from "../api/approvals";
import { t, type MessageKey } from "../i18n";
import { CLOSED, decideApproval, skipApproval, type Outcome } from "../state/approvals";
import "../styles/approvals.css";

const RISK_LABEL: Record<Approval["risk"], MessageKey> = {
  low: "approvals.riskLow",
  medium: "approvals.riskMedium",
  high: "approvals.riskHigh",
};

/** Seconds left, or null when the request has no readable expiry. */
export function secondsLeft(expiresAt: string, now: number): number | null {
  const end = Date.parse(expiresAt);
  return Number.isNaN(end) ? null : Math.max(0, Math.ceil((end - now) / 1000));
}

export function clock(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** The short line shown after a request is closed (answered here, answered elsewhere, expired or gone). */
export function outcomeText(o: Outcome): string {
  switch (o.kind) {
    case "approved":
      return t("approvals.approved");
    case "denied":
      return t("approvals.denied");
    case "expired":
      return t("approvals.doneExpired");
    case "decided":
      return t("approvals.doneDecided");
    default:
      return t("approvals.doneGone");
  }
}

export function ApprovalCard(props: {
  item: Approval;
  /** "1 of 3": where this one is in the queue. */
  position?: { index: number; total: number };
  /** The request was closed (see CLOSED in state/approvals). */
  onDone: (outcome: Outcome) => void;
  /** Put this one off (the pop-up only). */
  onLater?: () => void;
  /** Extra controls in the header corner. */
  children?: JSX.Element;
}) {
  const [busy, setBusy] = createSignal<"approve" | "deny" | null>(null);
  const [note, setNote] = createSignal<MessageKey | null>(null);
  const [now, setNow] = createSignal(Date.now());
  let timer: ReturnType<typeof setInterval> | undefined;
  let closed = false;

  onMount(() => {
    timer = setInterval(() => setNow(Date.now()), 1000);
  });
  onCleanup(() => timer !== undefined && clearInterval(timer));

  const left = () => secondsLeft(props.item.expiresAt, now());
  const expired = () => left() === 0;
  // Time ran out while the card was open: close it like any other ended request.
  createEffect(() => {
    if (expired() && !closed) {
      closed = true;
      skipApproval(props.item.id);
      props.onDone({ kind: "expired" });
    }
  });

  async function answer(decision: "approve" | "deny") {
    if (busy() || closed) return;
    setBusy(decision);
    setNote(null);
    const outcome = await decideApproval(props.item, decision);
    setBusy(null);
    if (CLOSED.includes(outcome.kind)) {
      if (closed) return;
      closed = true;
      props.onDone(outcome);
    } else if (outcome.kind === "needsFaceId") setNote("approvals.faceId");
    else if (outcome.kind === "notMain") setNote("approvals.errNotMain");
    else if (outcome.kind === "changed") setNote("approvals.errChanged");
    else setNote("approvals.errSend");
  }

  const canApprove = () => !props.item.sensitive && !busy() && !expired();
  const initial = () => (props.item.requester.name.trim()[0] ?? "?").toUpperCase();

  return (
    <article class="apv-card" classList={{ "apv-card--sensitive": props.item.sensitive }} aria-labelledby={`apv-sum-${props.item.id}`}>
      <header class="apv-head">
        <span class="apv-avatar" aria-hidden="true">
          {initial()}
        </span>
        <div class="apv-who">
          <p class="apv-asking">{t("approvals.asking", { name: props.item.requester.name || props.item.requester.kind })}</p>
          <Show when={props.item.requester.detail}>
            {(d) => <p class="apv-kind">{d()}</p>}
          </Show>
        </div>
        <Show when={props.position && props.position.total > 1}>
          <span class="apv-pos">{t("approvals.position", { index: props.position!.index, total: props.position!.total })}</span>
        </Show>
        {props.children}
      </header>

      <h2 class="apv-summary" id={`apv-sum-${props.item.id}`}>
        {props.item.summary}
      </h2>

      <div class="apv-badges">
        <span class={`apv-badge apv-badge--${props.item.risk}`}>{t(RISK_LABEL[props.item.risk])}</span>
        <Show when={props.item.sensitive}>
          <span class="apv-badge apv-badge--sensitive">{t("approvals.sensitive")}</span>
        </Show>
        <Show when={props.item.action}>
          <code class="apv-action">{props.item.action}</code>
        </Show>
        <Show when={left() !== null}>
          <span class="apv-expiry" classList={{ "apv-expiry--soon": (left() ?? 0) <= 30 }} data-testid="apv-expiry">
            {expired() ? t("approvals.expired") : t("approvals.expiresIn", { time: clock(left() ?? 0) })}
          </span>
        </Show>
      </div>

      <Show when={props.item.details}>
        <pre class="apv-details" tabindex="0" aria-label={t("approvals.detailsLabel")}>
          {props.item.details}
        </pre>
      </Show>

      <Show when={props.item.sensitive}>
        <p class="apv-faceid" id={`apv-face-${props.item.id}`} role="note">
          {t("approvals.faceId")}
        </p>
      </Show>
      <Show when={note()}>
        {(key) => (
          <p class="form-error" role="alert">
            {t(key())}
          </p>
        )}
      </Show>

      <div class="apv-actions">
        <button type="button" class="btn" disabled={busy() !== null || expired()} onClick={() => void answer("deny")}>
          {busy() === "deny" ? t("approvals.denying") : t("approvals.deny")}
        </button>
        <button
          type="button"
          class="btn btn--primary"
          disabled={!canApprove()}
          aria-describedby={props.item.sensitive ? `apv-face-${props.item.id}` : undefined}
          onClick={() => void answer("approve")}
        >
          {busy() === "approve" ? t("approvals.approving") : t("approvals.approve")}
        </button>
      </div>
      <Show when={props.onLater}>
        {(later) => (
          <button type="button" class="apv-later" onClick={() => later()()}>
            {t("approvals.later")}
          </button>
        )}
      </Show>
    </article>
  );
}

/** The short "Approved." / "Expired." state that replaces a card for a moment. */
export function ApprovalOutcome(props: { outcome: Outcome }) {
  return (
    <div class="apv-card apv-card--done" role="status">
      <span class="apv-done-icon" classList={{ "apv-done-icon--ok": props.outcome.kind === "approved" }} aria-hidden="true">
        {props.outcome.kind === "approved" ? "✓" : props.outcome.kind === "denied" ? "×" : "•"}
      </span>
      <p class="apv-done-text">{outcomeText(props.outcome)}</p>
    </div>
  );
}
