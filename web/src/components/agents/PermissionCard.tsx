import { A } from "@solidjs/router";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import type { PermissionRequest, PermissionView } from "../../api/agents";
import { t } from "../../i18n";
import { permissionText } from "../../lib/agentsTimeline";
import { answerPermission, agentsState } from "../../state/agents";
import { clock, secondsLeft } from "../ApprovalCard";

/**
 * One pending permission: what the agent (or an external caller) wants, the exact input, Allow and Deny.
 * When the request is also filed with the owner approvals (`approvalId`), a link opens it there: that is the
 * same pop-up the phone and Secretary Code show, with Face ID for sensitive ones.
 */
export function PermissionCard(props: { permission: PermissionView; request?: PermissionRequest; showAgent?: boolean }) {
  const [busy, setBusy] = createSignal<"allow" | "deny" | null>(null);
  const [note, setNote] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => clearInterval(timer));
  });

  const p = () => props.permission;
  const [closedBy, setClosedBy] = createSignal<string | null>(null);
  const pending = () => p().status === "pending" && closedBy() === null;
  const left = () => secondsLeft(p().expiresAt, now());
  const body = () => permissionText(p().details, props.request);
  const agentName = () => (p().agentId ? (agentsState.items[p().agentId!]?.title ?? agentsState.items[p().agentId!]?.shortId ?? p().agentId) : p().source);

  async function answer(behavior: "allow" | "deny") {
    if (busy()) return;
    setBusy(behavior);
    setNote(null);
    const r = await answerPermission(p(), behavior);
    setBusy(null);
    if (!r.ok && r.closed) setClosedBy(r.message ?? t("agents.permGone"));
    else if (!r.ok) setNote(r.message ?? t("agents.failed"));
  }

  return (
    <article class="ag-perm" data-status={p().status} aria-label={p().title}>
      <header class="ag-perm__head">
        <span class="ag-perm__kind">{p().kind === "external" ? t("agents.permExternal") : p().name || t("agents.permTool")}</span>
        <Show when={props.showAgent && agentName()}>
          <span class="ag-perm__who">{agentName()}</span>
        </Show>
        <Show when={pending() && left() !== null}>
          <span class="ag-perm__left" aria-label={t("agents.permLeft", { time: clock(left()!) })}>{clock(left()!)}</span>
        </Show>
      </header>
      <h3 class="ag-perm__title">{p().title}</h3>
      <Show when={p().description}>
        <p class="ag-perm__desc">{p().description}</p>
      </Show>
      <Show when={body()}>
        <pre class="ag-code ag-perm__body">{body()}</pre>
      </Show>
      <Show
        when={pending()}
        fallback={
          <p class="ag-perm__done" role="status">
            <Show
              when={closedBy() === null}
              fallback={closedBy()}
            >
              {p().status === "allowed" ? t("agents.permAllowed") : p().status === "denied" ? t("agents.permDenied") : t("agents.permExpired")}
              <Show when={p().resolution?.message}>{` · ${p().resolution!.message}`}</Show>
            </Show>
          </p>
        }
      >
        <div class="ag-perm__actions">
          <button type="button" class="ag-btn ag-btn--primary" disabled={busy() !== null} onClick={() => void answer("allow")}>
            {busy() === "allow" ? t("agents.working") : t("agents.allow")}
          </button>
          <button type="button" class="ag-btn ag-btn--danger" disabled={busy() !== null} onClick={() => void answer("deny")}>
            {busy() === "deny" ? t("agents.working") : t("agents.deny")}
          </button>
          <Show when={p().approvalId}>
            {(id) => (
              <A class="ag-link" href={`/approvals/${encodeURIComponent(id())}`}>
                {t("agents.openInApprovals")}
              </A>
            )}
          </Show>
        </div>
      </Show>
      <Show when={note()}>
        <p class="ag-note" role="alert">{note()}</p>
      </Show>
    </article>
  );
}
