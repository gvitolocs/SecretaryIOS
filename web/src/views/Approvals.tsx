import { A } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import type { Approval } from "../api/approvals";
import { ApprovalCard, ApprovalOutcome } from "../components/ApprovalCard";
import { t } from "../i18n";
import { pendingApprovals, refreshApprovals, type Outcome } from "../state/approvals";

// /approvals: every request this device may decide now. (The pop-up shows the same ones, one at a time, on any
// other page.)
export default function Approvals() {
  const [seen, setSeen] = createSignal<Approval[]>([]);
  const [closed, setClosed] = createSignal<Record<string, Outcome>>({});

  onMount(() => void refreshApprovals());
  // Remember the order requests first showed up in, so an answered one keeps its place while it says so.
  createEffect(() => {
    const now = pendingApprovals();
    setSeen((prev) => {
      const ids = new Set(prev.map((a) => a.id));
      const fresh = now.filter((a) => !ids.has(a.id));
      return fresh.length ? [...prev, ...fresh] : prev;
    });
  });
  const rows = createMemo(() => {
    const live = new Map(pendingApprovals().map((a) => [a.id, a]));
    return seen().flatMap((a) => {
      const cur = live.get(a.id);
      return cur ? [cur] : closed()[a.id] ? [a] : [];
    });
  });

  return (
    <section class="apv-page" aria-labelledby="apv-page-title">
      <h1 id="apv-page-title" class="apv-page-title">
        {t("approvals.title")}
      </h1>
      <p class="muted">{t("approvals.listSub")}</p>
      <Show when={rows().length > 0} fallback={<p class="apv-empty" role="status">{t("approvals.listEmpty")}</p>}>
        <ul class="apv-list">
          <For each={rows()}>
            {(item) => (
              <li>
                <Show
                  when={closed()[item.id]}
                  fallback={<ApprovalCard item={item} onDone={(o) => setClosed((c) => ({ ...c, [item.id]: o }))} />}
                >
                  {(o) => <ApprovalOutcome outcome={o()} />}
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <A class="btn" href="/">
        {t("login.backHome")}
      </A>
    </section>
  );
}
