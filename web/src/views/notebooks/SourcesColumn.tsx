import { For, Show } from "solid-js";
import { toApiError } from "../../api/client";
import { notebooksApi, SOURCE_PENDING, type Source, type SourceContext } from "../../api/notebooks";
import { PlusIcon, TrashIcon } from "../../components/Icons";
import { Button, Card, EmptyState, ListRow, RowList } from "../../components/page";
import { t } from "../../i18n";
import { KIND_ICON, KIND_KEY, originLabel, SOURCE_STATUS_KEY, statusTone } from "./common";

const CONTEXTS: SourceContext[] = ["full", "summary", "off"];

function contextLabel(c: SourceContext): string {
  return c === "full" ? t("notebooks.ctxFull") : c === "summary" ? t("notebooks.ctxSummary") : t("notebooks.ctxOff");
}

/** The sources of a notebook: status, per-source context for the chat, retry, and the reader. */
export function SourcesColumn(props: {
  notebookId: string;
  sources: Source[];
  onOpen: (sourceId: string) => void;
  onAdd: () => void;
  onChanged: () => void;
  onError: (message: string) => void;
}) {
  async function act(f: () => Promise<unknown>) {
    try {
      await f();
    } catch (e) {
      props.onError(toApiError(e).message);
    }
    props.onChanged();
  }

  const sub = (s: Source) => {
    const bits: string[] = [t(KIND_KEY[s.kind]), originLabel(s.origin)];
    if (s.status === "ready" && s.chunks) bits.push(t("notebooks.nPassages", { count: s.chunks }));
    if (s.pages) bits.push(t("notebooks.nPages", { count: s.pages }));
    return bits.join(" · ");
  };

  return (
    <Card
      list
      class="nb-card"
      title={t("notebooks.sources")}
      actions={
        <Button variant="ghost" onClick={props.onAdd}>
          <PlusIcon /> {t("notebooks.add")}
        </Button>
      }
    >
      <Show
        when={props.sources.length}
        fallback={
          <EmptyState
            text={t("notebooks.sourcesEmpty")}
            action={
              <Button onClick={props.onAdd}>{t("notebooks.addSource")}</Button>
            }
          />
        }
      >
        <RowList label={t("notebooks.sources")}>
          <For each={props.sources}>
            {(s) => (
              <ListRow
                class="nb-src"
                leading={<span class="nb-kind">{KIND_ICON[s.kind]()}</span>}
                title={
                  <button type="button" class="nb-open" onClick={() => props.onOpen(s.id)} title={s.title}>
                    {s.title}
                  </button>
                }
                sub={
                  <>
                    <Show when={s.status !== "ready"}>
                      <span class="nb-status" data-tone={statusTone(s.status)} data-busy={SOURCE_PENDING.includes(s.status) ? "" : undefined}>
                        {t(SOURCE_STATUS_KEY[s.status])}
                      </span>
                      {" · "}
                    </Show>
                    {sub(s)}
                  </>
                }
                trailing={
                  <div class="nb-row-actions">
                    <Show when={s.status === "failed"}>
                      <p class="nb-error" role="alert">{s.error ?? t("notebooks.failedGeneric")}</p>
                      <Button onClick={() => void act(() => notebooksApi.retrySource(s.id))}>{t("notebooks.retry")}</Button>
                    </Show>
                    <label class="nb-ctx">
                      <span class="nb-ctx-label">{t("notebooks.context")}</span>
                      <select
                        class="pg-input"
                        aria-label={t("notebooks.contextFor", { title: s.title })}
                        onChange={(e) => void act(() => notebooksApi.setContext(props.notebookId, s.id, e.currentTarget.value as SourceContext))}
                      >
                        <For each={CONTEXTS}>
                          {(c) => (
                            <option value={c} selected={(s.context ?? "full") === c}>
                              {contextLabel(c)}
                            </option>
                          )}
                        </For>
                      </select>
                    </label>
                    <button
                      type="button"
                      class="pg-icon-btn"
                      aria-label={t("notebooks.removeSource", { title: s.title })}
                      title={t("notebooks.removeSourceHint")}
                      onClick={() => {
                        if (confirm(t("notebooks.removeSourceConfirm", { title: s.title }))) void act(() => notebooksApi.removeSource(props.notebookId, s.id));
                      }}
                    >
                      <TrashIcon />
                    </button>
                  </div>
                }
              />
            )}
          </For>
        </RowList>
      </Show>
    </Card>
  );
}
