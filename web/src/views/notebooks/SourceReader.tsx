import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { toApiError } from "../../api/client";
import {
  INSIGHT_PENDING,
  notebooksApi,
  SOURCE_PENDING,
  type Chunk,
  type Insight,
  type Source,
  type Transformation,
} from "../../api/notebooks";
import { Markdown } from "../../components/Markdown";
import { Button, LoadingState, Sheet } from "../../components/page";
import { t } from "../../i18n";
import { clock, INSIGHT_STATUS_KEY, KIND_KEY, originLabel, SOURCE_STATUS_KEY, statusTone } from "./common";

const PAGE = 40;

/**
 * The reader sheet: a source's text by passages (opened at `around` when a citation was clicked),
 * its insights, and the transformations that make new ones.
 */
export function SourceReader(props: {
  sourceId: string | null;
  /** The passage to scroll to (a citation's ordinal). */
  around: number | null;
  onClose: () => void;
  onNoteSaved: () => void;
}) {
  const [source, setSource] = createSignal<Source | null>(null);
  const [chunks, setChunks] = createSignal<Chunk[]>([]);
  const [insights, setInsights] = createSignal<Insight[]>([]);
  const [transformations, setTransformations] = createSignal<Transformation[]>([]);
  const [chosen, setChosen] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [saved, setSaved] = createSignal<Record<string, string>>({});
  let body: HTMLDivElement | undefined;
  let generation = 0;

  const fail = (e: unknown) => setError(toApiError(e).message);

  function scrollTo(ordinal: number | null) {
    if (ordinal === null) return;
    requestAnimationFrame(() => {
      body?.querySelector<HTMLElement>(`[data-ordinal="${ordinal}"]`)?.scrollIntoView?.({ block: "center" });
    });
  }

  async function loadChunks(id: string, around: number | null, mine: number) {
    try {
      const list = await notebooksApi.chunks(id, around !== null ? { around } : { from: 0, limit: PAGE });
      if (mine !== generation) return;
      setChunks(list);
      scrollTo(around);
    } catch (e) {
      if (mine === generation) fail(e);
    }
  }

  async function open(id: string, around: number | null) {
    const mine = ++generation;
    setSource(null);
    setChunks([]);
    setInsights([]);
    setSaved({});
    setError(null);
    setLoading(true);
    const [s, i, tr] = await Promise.allSettled([notebooksApi.source(id), notebooksApi.insights(id), notebooksApi.transformations()]);
    if (mine !== generation) return;
    if (s.status === "fulfilled") setSource(s.value);
    else fail(s.reason);
    if (i.status === "fulfilled") setInsights(i.value);
    if (tr.status === "fulfilled") {
      setTransformations(tr.value);
      if (!chosen()) setChosen(tr.value.find((x) => x.id === "t-dense-summary")?.id ?? tr.value[0]?.id ?? "");
    }
    setLoading(false);
    if (s.status === "fulfilled" && s.value.status === "ready") await loadChunks(id, around, mine);
  }

  createEffect(
    on(
      () => [props.sourceId, props.around] as const,
      ([id, around]) => {
        if (id) void open(id, around);
        else generation++;
      },
    ),
  );

  // Follow a source that is still being read, and insights that are still being written.
  const timer = setInterval(async () => {
    const id = props.sourceId;
    if (!id) return;
    const mine = generation;
    try {
      const s = source();
      if (s && SOURCE_PENDING.includes(s.status)) {
        const next = await notebooksApi.source(id);
        if (mine !== generation) return;
        setSource(next);
        if (next.status === "ready") await loadChunks(id, props.around, mine);
      }
      if (insights().some((i) => INSIGHT_PENDING.includes(i.status))) {
        const next = await notebooksApi.insights(id);
        if (mine === generation) setInsights(next);
      }
    } catch {
      // the next tick tries again
    }
  }, 3000);
  onCleanup(() => clearInterval(timer));

  async function run() {
    const id = props.sourceId;
    if (!id || !chosen()) return;
    setError(null);
    try {
      const insight = await notebooksApi.runTransformation(id, chosen());
      setInsights([insight, ...insights()]);
    } catch (e) {
      fail(e);
    }
  }

  async function saveAsNote(insight: Insight) {
    setError(null);
    try {
      const link = await notebooksApi.insightToNote(insight.id);
      setSaved({ ...saved(), [insight.id]: link.slug });
      props.onNoteSaved();
    } catch (e) {
      fail(e);
    }
  }

  async function removeInsight(insight: Insight) {
    if (!confirm(t("notebooks.deleteInsightConfirm", { title: insight.title }))) return;
    try {
      await notebooksApi.removeInsight(insight.id);
      setInsights(insights().filter((i) => i.id !== insight.id));
    } catch (e) {
      fail(e);
    }
  }

  async function more(direction: "earlier" | "later") {
    const id = props.sourceId;
    const list = chunks();
    if (!id || !list.length) return;
    const mine = generation;
    try {
      if (direction === "later") {
        const next = await notebooksApi.chunks(id, { from: list[list.length - 1]!.ordinal + 1, limit: PAGE });
        if (mine === generation) setChunks([...list, ...next]);
      } else {
        const first = list[0]!.ordinal;
        const from = Math.max(0, first - PAGE);
        const prev = await notebooksApi.chunks(id, { from, limit: first - from });
        if (mine === generation) setChunks([...prev, ...list]);
      }
    } catch (e) {
      fail(e);
    }
  }

  const hasEarlier = () => (chunks()[0]?.ordinal ?? 0) > 0;
  const hasLater = () => {
    const last = chunks()[chunks().length - 1];
    return !!last && last.ordinal + 1 < (source()?.chunks ?? 0);
  };
  const originLink = () => {
    const s = source();
    return s && /^https?:\/\//i.test(s.origin) ? s.origin : null;
  };

  return (
    <Sheet open={!!props.sourceId} onClose={props.onClose} size="wide" class="nb-sheet nb-reader" title={source()?.title ?? t("notebooks.reader")}>
      <div ref={body} class="nb-reader-body">
        <Show when={error()}>
          <p class="pg-notice" role="alert">{error()}</p>
        </Show>
        <Show when={loading() && !source()}>
          <LoadingState label={t("notebooks.loading")} />
        </Show>
        <Show when={source()}>
          {(s) => (
            <>
              <p class="nb-reader-meta pg-muted">
                {t(KIND_KEY[s().kind])} · {originLabel(s().origin)}
                <Show when={s().pages}>{` · ${t("notebooks.nPages", { count: s().pages! })}`}</Show>
                <Show when={s().duration_s}>{` · ${clock(s().duration_s!)}`}</Show>
                {" · "}
                <span class="nb-status" data-tone={statusTone(s().status)}>{t(SOURCE_STATUS_KEY[s().status])}</span>
                <Show when={originLink()}>
                  {(href) => (
                    <>
                      {" · "}
                      <a class="pg-link" href={href()} target="_blank" rel="noopener noreferrer">{t("notebooks.openOriginal")}</a>
                    </>
                  )}
                </Show>
                <Show when={s().has_file}>
                  {" · "}
                  <a class="pg-link" href={notebooksApi.fileUrl(s().id)} target="_blank" rel="noopener noreferrer">{t("notebooks.openFile")}</a>
                </Show>
              </p>
              <Show when={s().status === "failed"}>
                <p class="nb-error" role="alert">{s().error ?? t("notebooks.failedGeneric")}</p>
              </Show>

              <section class="nb-reader-section" aria-labelledby="nb-insights-h">
                <h3 id="nb-insights-h" class="nb-reader-h">{t("notebooks.insights")}</h3>
                <div class="nb-run">
                  <label class="nb-run-pick">
                    <span class="pg-field-label">{t("notebooks.transformation")}</span>
                    <select class="pg-input" value={chosen()} onChange={(e) => setChosen(e.currentTarget.value)} disabled={s().status !== "ready"}>
                      <For each={transformations()}>
                        {(tr) => (
                          <option value={tr.id} selected={chosen() === tr.id} title={tr.description}>
                            {tr.name}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                  <Button onClick={() => void run()} disabled={!chosen() || s().status !== "ready"}>
                    {t("notebooks.runTransformation")}
                  </Button>
                </div>
                <Show when={insights().length} fallback={<p class="pg-muted">{t("notebooks.insightsEmpty")}</p>}>
                  <ul class="nb-insights">
                    <For each={insights()}>
                      {(i) => (
                        <li class="nb-insight">
                          <div class="nb-insight-head">
                            <strong>{i.title}</strong>
                            <span class="nb-status" data-tone={statusTone(i.status)} data-busy={INSIGHT_PENDING.includes(i.status) ? "" : undefined}>
                              {t(INSIGHT_STATUS_KEY[i.status])}
                            </span>
                          </div>
                          <Show when={i.status === "ready"}>
                            <Markdown text={i.text} class="nb-insight-text" />
                            <div class="nb-insight-actions">
                              <Show
                                when={saved()[i.id]}
                                fallback={
                                  <Button onClick={() => void saveAsNote(i)}>{t("notebooks.saveAsNote")}</Button>
                                }
                              >
                                {(slug) => (
                                  <a class="pg-link" href={`/keep/note/${slug()}`}>{t("notebooks.savedOpen")}</a>
                                )}
                              </Show>
                              <Button variant="ghost" onClick={() => void removeInsight(i)}>
                                {t("common.delete")}
                              </Button>
                            </div>
                          </Show>
                          <Show when={i.status === "failed"}>
                            <div class="nb-insight-actions">
                              <Button variant="ghost" onClick={() => void removeInsight(i)}>
                                {t("common.delete")}
                              </Button>
                            </div>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </section>

              <section class="nb-reader-section" aria-labelledby="nb-text-h">
                <h3 id="nb-text-h" class="nb-reader-h">{t("notebooks.passages")}</h3>
                <Show when={hasEarlier()}>
                  <Button variant="ghost" onClick={() => void more("earlier")}>{t("notebooks.earlier")}</Button>
                </Show>
                <Show when={chunks().length} fallback={<Show when={s().status === "ready" && !loading()}><p class="pg-muted">{t("notebooks.noPassages")}</p></Show>}>
                  <ol class="nb-chunks">
                    <For each={chunks()}>
                      {(c) => (
                        <li class="nb-chunk" data-ordinal={c.ordinal} data-current={props.around === c.ordinal ? "" : undefined}>
                          <span class="nb-chunk-ref">
                            {c.page ? t("notebooks.pageN", { n: c.page }) : c.start_s !== null && c.start_s !== undefined ? clock(c.start_s) : `#${c.ordinal + 1}`}
                          </span>
                          <p>{c.text}</p>
                        </li>
                      )}
                    </For>
                  </ol>
                </Show>
                <Show when={hasLater()}>
                  <Button variant="ghost" onClick={() => void more("later")}>{t("notebooks.moreText")}</Button>
                </Show>
              </section>
            </>
          )}
        </Show>
      </div>
    </Sheet>
  );
}
