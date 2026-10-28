import { A } from "@solidjs/router";
import { createSignal, For, onMount, Show } from "solid-js";
import { decisionsApi, type Decision } from "../api/decisions";
import { DecisionSummary } from "../components/DecisionSummary";
import { Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, RowList, useWide } from "../components/page";
import { relativeTime } from "../components/NotificationBell";
import { t } from "../i18n";

export default function Decisions() {
  const { wide, track } = useWide();
  const [pending, setPending] = createSignal<Decision[]>([]);
  const [earlier, setEarlier] = createSignal<Decision[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal(false);
  async function load() {
    setLoading(true);
    setError(false);
    try {
      // Fetch pending separately so a busy settled history cannot hide unanswered questions.
      const [waiting, ...settled] = await Promise.all([
        decisionsApi.list("pending"),
        decisionsApi.list("decided", 20),
        decisionsApi.list("expired", 20),
        decisionsApi.list("withdrawn", 20),
      ]);
      setPending(waiting);
      setEarlier(settled.flat().sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at)).slice(0, 20));
    } catch { setError(true); }
    finally { setLoading(false); }
  }
  onMount(() => void load());
  const rows = (items: Decision[]) => <ul class="apv-list"><For each={items}>{item => <li><A class="apv-card dc-link" href={`/decisions/${item.decision_id}`}><DecisionSummary item={item} /></A></li>}</For></ul>;
  const deskRows = (items: Decision[], label: string) => <Card list label={label}><RowList label={label}><For each={items}>{item => <ListRow
    title={item.question} href={`/decisions/${item.decision_id}`}
    sub={<>{t("decisions.asking", { name: item.asking_client + (item.asking_session ? ` (${item.asking_session})` : "") })} · <time dateTime={item.created_at}>{relativeTime(Date.parse(item.created_at))}</time> · {t(({ low: "decisions.impactLow", medium: "decisions.impactMedium", high: "decisions.impactHigh" } as const)[item.impact])} · {t(({ low: "decisions.urgencyLow", normal: "decisions.urgencyNormal", high: "decisions.urgencyHigh" } as const)[item.urgency])}</>}
    meta={t(({ pending: "decisions.pending", decided: "decisions.decided", expired: "decisions.expired", withdrawn: "decisions.withdrawn" } as const)[item.status])}
  />}</For></RowList></Card>;
  return <div ref={track}><Show when={wide()} fallback={<section class="apv-page" aria-labelledby="decisions-title">
    <h1 id="decisions-title" class="apv-page-title">{t("decisions.title")}</h1>
    <p class="muted">{t("decisions.listSub")}</p>
    <Show when={loading()}><p role="status">{t("decisions.loading")}</p></Show>
    <Show when={error()}><p role="alert">{t("decisions.loadError")}</p><button class="btn" onClick={() => void load()}>{t("decisions.retry")}</button></Show>
    <Show when={!loading() && !error()}>
      <Show when={pending().length} fallback={<p role="status">{t("decisions.empty")}</p>}>{rows(pending())}</Show>
      <Show when={earlier().length}><details><summary>{t("decisions.earlier")}</summary>{rows(earlier())}</details></Show>
    </Show>
    <A class="btn" href="/">{t("login.backHome")}</A>
  </section>}><Page>
    <PageHeader title={t("decisions.title")} context={t("decisions.listSub")} />
    <PageGrid layout="single">
      <Show when={loading()}><LoadingState label={t("decisions.loading")} /></Show>
      <Show when={error()}><ErrorState message={t("decisions.loadError")} retryLabel={t("decisions.retry")} onRetry={() => void load()} /></Show>
      <Show when={!loading() && !error()}>
        <Show when={pending().length} fallback={<Card label={t("decisions.title")}><EmptyState text={t("decisions.empty")} /></Card>}>{deskRows(pending(), t("decisions.title"))}</Show>
        <Show when={earlier().length}><details><summary class="pg-link">{t("decisions.earlier")}</summary>{deskRows(earlier(), t("decisions.earlier"))}</details></Show>
      </Show>
      <A class="pg-link" href="/">{t("login.backHome")}</A>
    </PageGrid>
  </Page></Show></div>;
}
