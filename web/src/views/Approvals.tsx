import { A } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, onMount, Show } from "solid-js";
import type { Approval } from "../api/approvals";
import { ApprovalCard, ApprovalOutcome } from "../components/ApprovalCard";
import { Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, RowList, useWide } from "../components/page";
import { relativeTime } from "../components/NotificationBell";
import { t } from "../i18n";
import { pendingApprovals, refreshApprovals, type Outcome } from "../state/approvals";

// /approvals: every request this device may decide now. (The pop-up shows the same ones, one at a time, on any
// other page.)
export default function Approvals() {
  const { wide, track } = useWide();
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal(false);
  async function load() {
    setLoading(true);
    setError(false);
    await refreshApprovals(() => setError(true));
    setLoading(false);
  }
  const [seen, setSeen] = createSignal<Approval[]>([]);
  const [closed, setClosed] = createSignal<Record<string, Outcome>>({});

  onMount(() => void load());
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
    <div ref={track}><Show when={wide()} fallback={<section class="apv-page" aria-labelledby="apv-page-title">
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
    </section>}><Page>
      <PageHeader title={t("approvals.title")} context={t("approvals.listSub")} />
      <PageGrid layout="single">
        <Show when={loading()}><LoadingState label={t("approvals.loading")} /></Show>
        <Show when={error()}><ErrorState message={t("approvals.loadError")} retryLabel={t("approvals.retry")} onRetry={() => void load()} /></Show>
        <Show when={!loading() && !error()}>
          <Card list label={t("approvals.title")}>
            <Show when={rows().length} fallback={<EmptyState text={t("approvals.listEmpty")} />}>
              <RowList label={t("approvals.title")}><For each={rows()}>{item => <ListRow
                title={item.summary} href={`/approvals/${encodeURIComponent(item.id)}`}
                sub={<>{t("approvals.asking", { name: item.requester.name || item.requester.kind })} · <time dateTime={item.createdAt}>{relativeTime(Date.parse(item.createdAt))}</time></>}
                meta={t("approvals.pending")}
              />}</For></RowList>
            </Show>
          </Card>
        </Show>
        <A class="pg-link" href="/">{t("login.backHome")}</A>
      </PageGrid>
    </Page></Show></div>
  );
}
