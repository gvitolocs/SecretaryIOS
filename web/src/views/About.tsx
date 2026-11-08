import { A } from "@solidjs/router";
import { createMemo, createSignal, For, onMount, Show } from "solid-js";
import { aboutApi, FACT_CATEGORIES, type Fact } from "../api/about";
import { toApiError } from "../api/client";
import { Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, useWide } from "../components/page";
import { locale, t, type MessageKey } from "../i18n";

const shortDate = (ts: string): string => {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleDateString(locale(), { day: "numeric", month: "short" });
};

const categoryLabel = (category: string): string => t(`about.category_${category}` as MessageKey);

export default function About() {
  const { wide, track } = useWide();
  const [facts, setFacts] = createSignal<Fact[]>([]);
  const [history, setHistory] = createSignal<Fact[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [loadError, setLoadError] = createSignal(false);
  const [error, setError] = createSignal("");
  const [editing, setEditing] = createSignal<string | null>(null);
  const [confirmRetract, setConfirmRetract] = createSignal<string | null>(null);
  const [expanded, setExpanded] = createSignal<string | null>(null);
  // Add-a-fact form.
  const [newText, setNewText] = createSignal("");
  const [newCategory, setNewCategory] = createSignal<string>("other");
  const [newUntilDate, setNewUntilDate] = createSignal("");
  const [newUntilCondition, setNewUntilCondition] = createSignal("");
  // Inline edit form.
  const [editText, setEditText] = createSignal("");
  const [editCategory, setEditCategory] = createSignal<string>("other");
  const [editUntilDate, setEditUntilDate] = createSignal("");
  const [editUntilCondition, setEditUntilCondition] = createSignal("");

  async function load() {
    setLoading(true);
    setLoadError(false);
    try {
      const data = await aboutApi.get();
      setFacts(data.facts);
      setHistory(data.history);
    } catch {
      setLoadError(true);
    } finally {
      setLoading(false);
    }
  }
  onMount(() => void load());

  const groups = createMemo(() => {
    const byCategory = new Map<string, Fact[]>();
    for (const f of facts()) {
      const list = byCategory.get(f.category) ?? [];
      list.push(f);
      byCategory.set(f.category, list);
    }
    return FACT_CATEGORIES.filter(c => byCategory.has(c)).map(c => ({ category: c, items: byCategory.get(c)! }));
  });

  const showError = (e: unknown) => {
    const err = toApiError(e);
    // A refused secret carries Lucina's own message (secrets go to Infisical): show it as is.
    setError(err.code === "secret_refused" ? err.message : t("about.saveError"));
  };

  async function submitAdd() {
    setError("");
    try {
      await aboutApi.add({
        text: newText().trim(),
        category: newCategory(),
        ...(newUntilDate() ? { validUntil: newUntilDate() } : {}),
        ...(newUntilCondition().trim() ? { validUntilCondition: newUntilCondition().trim() } : {}),
      });
      setNewText("");
      setNewUntilDate("");
      setNewUntilCondition("");
      await load();
    } catch (e) {
      showError(e);
    }
  }

  function startEdit(f: Fact) {
    setEditing(f.id);
    setConfirmRetract(null);
    setEditText(f.text);
    setEditCategory(FACT_CATEGORIES.includes(f.category as never) ? f.category : "other");
    setEditUntilDate(f.validUntil?.date ?? "");
    setEditUntilCondition(f.validUntil?.condition ?? "");
  }

  async function submitEdit(id: string) {
    setError("");
    try {
      await aboutApi.update(id, {
        text: editText().trim(),
        category: editCategory(),
        validUntil: editUntilDate() || "",
        validUntilCondition: editUntilCondition().trim() || "",
      });
      setEditing(null);
      await load();
    } catch (e) {
      showError(e);
    }
  }

  async function endFact(id: string) {
    setError("");
    try {
      await aboutApi.end(id);
      await load();
    } catch (e) {
      showError(e);
    }
  }

  async function retractFact(id: string) {
    setError("");
    try {
      await aboutApi.retract(id);
      setConfirmRetract(null);
      await load();
    } catch (e) {
      showError(e);
    }
  }

  const untilLabel = (f: Fact): string | null => {
    if (f.validUntil?.date) return t("about.untilDate", { date: f.validUntil.date });
    if (f.validUntil?.condition) return t("about.untilCondition", { condition: f.validUntil.condition });
    return null;
  };

  const factRow = (f: Fact, inHistory: boolean) => (
    <li class={inHistory && f.status === "retracted" ? "about-row about-retracted" : "about-row"}>
      <Show when={editing() === f.id} fallback={
        <div class="about-row-main">
          <span class="about-text">{f.text}</span>
          <Show when={untilLabel(f)}>{u => <span class="muted">{u()}</span>}</Show>
          <Show when={inHistory && f.endedAt}>{e => <span class="muted">{t("about.endedUntil", { date: shortDate(e()) })}</span>}</Show>
          <span class="muted">{t("about.fromSource", { kind: f.source.kind, date: shortDate(f.learnedAt) })}</span>
          <Show when={!inHistory}>
            <span class="about-actions">
              <button type="button" class="btn" onClick={() => startEdit(f)}>{t("about.edit")}</button>
              <button type="button" class="btn" onClick={() => void endFact(f.id)}>{t("about.end")}</button>
              <Show when={confirmRetract() === f.id} fallback={
                <button type="button" class="btn" onClick={() => setConfirmRetract(f.id)}>{t("about.retract")}</button>
              }>
                <button type="button" class="btn btn--danger" onClick={() => void retractFact(f.id)}>{t("about.retractConfirm")}</button>
              </Show>
              <button type="button" class="btn" onClick={() => setExpanded(expanded() === f.id ? null : f.id)}>{t("about.changes")}</button>
            </span>
          </Show>
        </div>
      }>
        <div class="about-edit">
          <label>{t("about.addText")}<input type="text" value={editText()} onInput={e => setEditText(e.currentTarget.value)} /></label>
          <label>{t("about.addCategory")}<select value={editCategory()} onChange={e => setEditCategory(e.currentTarget.value)}>
            <For each={FACT_CATEGORIES}>{c => <option value={c}>{categoryLabel(c)}</option>}</For>
          </select></label>
          <label>{t("about.addUntilDate")}<input type="date" value={editUntilDate()} onInput={e => setEditUntilDate(e.currentTarget.value)} /></label>
          <label>{t("about.addUntilCondition")}<input type="text" value={editUntilCondition()} onInput={e => setEditUntilCondition(e.currentTarget.value)} /></label>
          <span class="about-actions">
            <button type="button" class="btn btn--primary" onClick={() => void submitEdit(f.id)}>{t("about.save")}</button>
            <button type="button" class="btn" onClick={() => setEditing(null)}>{t("about.cancel")}</button>
          </span>
        </div>
      </Show>
      <Show when={expanded() === f.id && f.changes.length}>
        <ul class="about-changes">
          <For each={f.changes}>{c => (
            <li>
              {t("about.changeLine", { date: shortDate(c.at), by: c.by, action: c.action, note: c.note ? ` — ${c.note}` : "" })}
              <Show when={c.before}>{b => <span class="muted"> {t("about.changeBefore", { text: b() })}</span>}</Show>
            </li>
          )}</For>
        </ul>
      </Show>
    </li>
  );

  const content = () => (
    <>
      <Show when={error()}><p role="alert">{error()}</p></Show>
      <Card label={t("about.addTitle")}>
        <div class="about-edit">
          <label>{t("about.addText")}<input type="text" value={newText()} onInput={e => setNewText(e.currentTarget.value)} /></label>
          <label>{t("about.addCategory")}<select value={newCategory()} onChange={e => setNewCategory(e.currentTarget.value)}>
            <For each={FACT_CATEGORIES}>{c => <option value={c}>{categoryLabel(c)}</option>}</For>
          </select></label>
          <label>{t("about.addUntilDate")}<input type="date" value={newUntilDate()} onInput={e => setNewUntilDate(e.currentTarget.value)} /></label>
          <label>{t("about.addUntilCondition")}<input type="text" value={newUntilCondition()} onInput={e => setNewUntilCondition(e.currentTarget.value)} /></label>
          <button type="button" class="btn btn--primary" disabled={!newText().trim()} onClick={() => void submitAdd()}>{t("about.addSubmit")}</button>
        </div>
      </Card>
      <Show when={loading()}><LoadingState label={t("about.loading")} /></Show>
      <Show when={loadError()}><ErrorState message={t("about.loadError")} retryLabel={t("about.retry")} onRetry={() => void load()} /></Show>
      <Show when={!loading() && !loadError()}>
        <Show when={groups().length} fallback={<Card label={t("about.title")}><EmptyState text={t("about.empty")} /></Card>}>
          <For each={groups()}>{g => (
            <Card label={categoryLabel(g.category)}>
              <ul class="about-list">{g.items.map(f => factRow(f, false))}</ul>
            </Card>
          )}</For>
        </Show>
        <Show when={history().length}>
          <details class="about-history">
            <summary class="pg-link">{t("about.history")}</summary>
            <ul class="about-list">{history().map(f => factRow(f, true))}</ul>
          </details>
        </Show>
      </Show>
    </>
  );

  return <div ref={track}><Show when={wide()} fallback={<section class="apv-page" aria-labelledby="about-title">
    <h1 id="about-title" class="apv-page-title">{t("about.title")}</h1>
    <p class="muted">{t("about.sub")}</p>
    {content()}
    <A class="btn" href="/">{t("login.backHome")}</A>
  </section>}><Page>
    <PageHeader title={t("about.title")} context={t("about.sub")} />
    <PageGrid layout="single">{content()}</PageGrid>
  </Page></Show></div>;
}
