import { A } from "@solidjs/router";
import { createSignal, For, onMount, Show } from "solid-js";
import { decisionsApi, type Decision } from "../api/decisions";
import { DecisionSummary } from "../components/DecisionSummary";
import { t } from "../i18n";

export default function Decisions() {
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
  return <section class="apv-page" aria-labelledby="decisions-title">
    <h1 id="decisions-title" class="apv-page-title">{t("decisions.title")}</h1>
    <p class="muted">{t("decisions.listSub")}</p>
    <Show when={loading()}><p role="status">{t("decisions.loading")}</p></Show>
    <Show when={error()}><p role="alert">{t("decisions.loadError")}</p><button class="btn" onClick={() => void load()}>{t("decisions.retry")}</button></Show>
    <Show when={!loading() && !error()}>
      <Show when={pending().length} fallback={<p role="status">{t("decisions.empty")}</p>}>{rows(pending())}</Show>
      <Show when={earlier().length}><details><summary>{t("decisions.earlier")}</summary>{rows(earlier())}</details></Show>
    </Show>
    <A class="btn" href="/">{t("login.backHome")}</A>
  </section>;
}
