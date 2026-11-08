import { A, useParams } from "@solidjs/router";
import { createEffect, createSignal, For, Show } from "solid-js";
import { decisionsApi, type Decision } from "../api/decisions";
import { toApiError } from "../api/client";
import { CHAT_MAX_CHARS } from "../api/secretary";
import { DecisionSummary } from "../components/DecisionSummary";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { t, type MessageKey } from "../i18n";
import { refreshDecisions } from "../state/decisions";

const TIERS: Record<number, MessageKey> = { 1: "decisions.tier1", 2: "decisions.tier2", 3: "decisions.tier3", 4: "decisions.tier4" };

export default function DecisionDetail() {
  const { wide, track } = useWide();
  const params = useParams();
  const [item, setItem] = createSignal<Decision | null>(null);
  const [phase, setPhase] = createSignal<"loading" | "ready" | "missing" | "error">("loading");
  const [busy, setBusy] = createSignal(false);
  const [sent, setSent] = createSignal(false);
  const [note, setNote] = createSignal<MessageKey | null>(null);
  const [words, setWords] = createSignal("");
  let generation = 0;

  async function load(id: string, version = generation) {
    setPhase("loading");
    try {
      const result = await decisionsApi.get(id);
      if (generation !== version) return;
      setItem(result);
      setNote(sent() && result.status === "pending" ? "decisions.sent" : null);
      setPhase("ready");
    } catch (e) {
      if (generation !== version) return;
      setPhase(toApiError(e).status === 404 ? "missing" : "error");
    }
  }
  createEffect(() => {
    const id = params.id;
    generation++;
    setItem(null);
    setBusy(false);
    setSent(false);
    setNote(null);
    setWords("");
    if (id) void load(id);
    else setPhase("missing");
  });

  async function submit(response: string | { n: number; option: string }) {
    const current = item();
    if (!current || current.status !== "pending" || busy() || sent()) return;
    const version = generation;
    setBusy(true);
    setNote(null);
    try {
      await decisionsApi.answer(current.decision_id, response);
    } catch {
      if (generation === version) { setNote("decisions.sendError"); setBusy(false); }
      return;
    }
    if (generation !== version) return;
    // A successful send must never be repeated because the following GET failed.
    setSent(true);
    void refreshDecisions();
    try {
      const updated = await decisionsApi.get(current.decision_id);
      if (generation !== version) return;
      setItem(updated);
      if (updated.status === "pending") setNote("decisions.sent");
    } catch {
      if (generation === version) setNote("decisions.refreshError");
    } finally {
      if (generation === version) setBusy(false);
    }
  }

  const content = (decision: Decision) => {
    const d = () => decision;
    return <>
      <Show when={d().reason}><h3 class="apv-summary">{t("decisions.reason")}</h3><p class="dc-reason">{d().reason}</p></Show>
      <Show when={d().status === "pending"} fallback={
        <div role="status">
          <p class="apv-done-text">{t(({ pending: "decisions.pending", decided: "decisions.decided", expired: "decisions.expired", withdrawn: "decisions.withdrawn" } as const)[d().status])}</p>
          <Show when={d().chosen_option}><p>{d().chosen_option}</p></Show>
          <Show when={d().decided_by}><p>{t(d().decided_by === "owner" ? "decisions.byOwner" : "decisions.byLucina")}</p></Show>
        </div>
      }>
        <div class="dc-options" aria-busy={busy()}><For each={d().options}>{(option, index) =>
          <button type="button" class={wide() ? "pg-btn" : "btn btn--primary"} disabled={busy() || sent()} onClick={() => void submit({ n: index() + 1, option })}>
            {option}{d().recommendation === option ? ` (${t("decisions.recommended")})` : ""}
          </button>
        }</For></div>
        <form class={wide() ? "dc-answer dc-answer-desk" : "dc-answer"} onSubmit={e => { e.preventDefault(); if (words().trim()) void submit(words().trim()); }}>
          <label for="decision-words">{t("decisions.ownWords")}</label>
          <textarea class={wide() ? "pg-input" : undefined} id="decision-words" value={words()} onInput={e => setWords(e.currentTarget.value)} disabled={busy() || sent()} maxLength={CHAT_MAX_CHARS - `Decision ${d().decision_id}: `.length} />
          <button class={wide() ? "pg-btn" : "btn"} type="submit" disabled={busy() || sent() || !words().trim()}>{busy() ? t("decisions.sending") : t("decisions.answerOwn")}</button>
        </form>
      </Show>
      <Show when={note()}>{key => <p class={wide() ? "pg-notice" : undefined} role={key() === "decisions.sent" ? "status" : "alert"}>{t(key())}</p>}</Show>
      <Show when={sent() && !busy() && d().status === "pending"}><button class={wide() ? "pg-btn" : "btn"} onClick={() => void load(params.id ?? "")}>{t("decisions.retry")}</button></Show>
      <Show when={d().evidence?.length}><details><summary>{t("decisions.evidence")}</summary><ul><For each={d().evidence}>{e => <li><span class="apv-badge">{TIERS[e.tier] ? t(TIERS[e.tier]!) : e.kind}</span><p class="dc-reason">{e.text}</p></li>}</For></ul></details></Show>
    </>;
  };

  return <div ref={track}><Show when={wide()} fallback={<section class="apv-page" aria-labelledby="decision-title">
    <h1 id="decision-title" class="apv-page-title">{t("decisions.title")}</h1>
    <Show when={phase() === "loading"}><p role="status">{t("decisions.loading")}</p></Show>
    <Show when={phase() === "missing"}><p role="alert">{t("decisions.notFound")}</p></Show>
    <Show when={phase() === "error"}><p role="alert">{t("decisions.loadError")}</p><button class="btn" onClick={() => void load(params.id ?? "")}>{t("decisions.retry")}</button></Show>
    <Show when={phase() === "ready" && item()}>{d => <article class="apv-card">
      <DecisionSummary item={d()} detail />
      {content(d())}
    </article>}</Show>
    <A class="btn" href="/decisions">{t("decisions.all")}</A>
  </section>}><Page>
    <PageHeader title={t("decisions.title")} context={t("decisions.listSub")} />
    <A class="pg-link pg-back" href="/decisions">{t("decisions.all")}</A>
    <Show when={item()}>{d => <h2 class="dc-detail-title">{d().question}</h2>}</Show>
    <PageGrid layout="single">
      <Show when={phase() === "loading"}><LoadingState label={t("decisions.loading")} /></Show>
      <Show when={phase() === "missing"}><Card><EmptyState text={t("decisions.notFound")} /></Card></Show>
      <Show when={phase() === "error"}><ErrorState message={t("decisions.loadError")} retryLabel={t("decisions.retry")} onRetry={() => void load(params.id ?? "")} /></Show>
      <Show when={phase() === "ready" && item()}>{d => <Card label={d().question}>
        <DecisionSummary item={d()} detail hideTitle />
        {content(d())}
      </Card>}</Show>
    </PageGrid>
  </Page></Show></div>;
}
