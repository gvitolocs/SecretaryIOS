import { useNavigate } from "@solidjs/router";
import { createEffect, createSignal, For, on, Show } from "solid-js";
import { toApiError } from "../api/client";
import { notebooksApi, type ArchivedFilter, type Notebook } from "../api/notebooks";
import { PlusIcon } from "../components/Icons";
import { Button, Card, EmptyState, ErrorState, ListRow, LoadingState, Page, PageGrid, PageHeader, PrimaryAction, RowList, SectionNav, Sheet } from "../components/page";
import { t } from "../i18n";
import { formatDate } from "../lib/units";
import "../styles/notebooks.css";

// Notebooks (NotebookLM style, ported from Open Notebook by Luis Novo, MIT): the list of
// notebooks. A notebook opens at /notebooks/:id (views/Notebook.tsx).

function counts(nb: Notebook): string {
  return [
    t("notebooks.nSources", { count: nb.counts.sources }),
    t("notebooks.nNotes", { count: nb.counts.notes }),
    t("notebooks.nChats", { count: nb.counts.chats }),
  ].join(" · ");
}

export default function NotebooksView() {
  const navigate = useNavigate();
  const [filter, setFilter] = createSignal<ArchivedFilter>("exclude");
  const [items, setItems] = createSignal<Notebook[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [creating, setCreating] = createSignal(false);
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [formError, setFormError] = createSignal<string | null>(null);
  let generation = 0;

  async function load() {
    const mine = ++generation;
    setError(null);
    try {
      const list = await notebooksApi.list(filter());
      if (mine === generation) setItems(list);
    } catch (e) {
      if (mine === generation) setError(toApiError(e).message);
    }
  }
  createEffect(
    on(filter, () => {
      setItems(null);
      void load();
    }),
  );

  function openCreate() {
    setName("");
    setDescription("");
    setFormError(null);
    setCreating(true);
  }

  async function create(e: Event) {
    e.preventDefault();
    if (!name().trim() || saving()) return;
    setSaving(true);
    setFormError(null);
    try {
      const nb = await notebooksApi.create({ name: name().trim(), description: description().trim() });
      setCreating(false);
      navigate(`/notebooks/${nb.id}`);
    } catch (err) {
      setFormError(toApiError(err).message);
    } finally {
      setSaving(false);
    }
  }

  const context = () => {
    const n = items()?.length;
    if (n === undefined) return undefined;
    return filter() === "only" ? t("notebooks.archivedCount", { count: n }) : t("notebooks.count", { count: n });
  };

  return (
    <Page class="nb-page">
      <PageHeader
        title={t("notebooks.title")}
        context={context()}
        nav={
          <SectionNav
            label={t("notebooks.title")}
            items={[
              { label: t("notebooks.active"), onSelect: () => setFilter("exclude"), current: filter() === "exclude" },
              { label: t("notebooks.archived"), onSelect: () => setFilter("only"), current: filter() === "only" },
            ]}
          />
        }
        action={
          <PrimaryAction icon={<PlusIcon />} onClick={openCreate}>
            {t("notebooks.newNotebook")}
          </PrimaryAction>
        }
      />
      <PageGrid layout="single">
        <Show when={error()}>
          <ErrorState message={error()!} retryLabel={t("common.tryAgain")} onRetry={() => void load()} />
        </Show>
        <Show when={!error() && items() === null}>
          <LoadingState label={t("notebooks.loading")} />
        </Show>
        <Show when={!error() && items()}>
          {(list) => (
            <Show
              when={list().length}
              fallback={
                <Card label={t("notebooks.title")}>
                  <EmptyState
                    title={filter() === "only" ? t("notebooks.noArchived") : t("notebooks.emptyTitle")}
                    text={filter() === "only" ? t("notebooks.noArchivedText") : t("notebooks.emptyText")}
                    action={
                      filter() === "exclude" ? (
                        <Button onClick={openCreate}>{t("notebooks.newNotebook")}</Button>
                      ) : undefined
                    }
                  />
                </Card>
              }
            >
              <Card list label={t("notebooks.title")}>
                <RowList label={t("notebooks.title")}>
                  <For each={list()}>
                    {(nb) => (
                      <ListRow
                        href={`/notebooks/${nb.id}`}
                        title={nb.name}
                        badge={nb.archived ? t("notebooks.archivedBadge") : undefined}
                        sub={nb.description ? `${nb.description} · ${counts(nb)}` : counts(nb)}
                        meta={<time dateTime={nb.updated_at}>{formatDate(Date.parse(nb.updated_at))}</time>}
                      />
                    )}
                  </For>
                </RowList>
              </Card>
            </Show>
          )}
        </Show>
        <p class="nb-credit">
          {t("notebooks.creditBefore")} <a href="https://github.com/lfnovo/open-notebook" target="_blank" rel="noopener noreferrer">{t("notebooks.creditName")}</a> {t("notebooks.creditAfter")}
        </p>
      </PageGrid>

      <Sheet
        open={creating()}
        onClose={() => setCreating(false)}
        title={t("notebooks.newNotebook")}
        class="nb-sheet"
        footer={
          <>
            <Button variant="ghost" onClick={() => setCreating(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" form="nb-new-form" disabled={!name().trim() || saving()}>
              {saving() ? t("common.saving") : t("notebooks.create")}
            </Button>
          </>
        }
      >
        <form id="nb-new-form" class="nb-form" onSubmit={create}>
          <Show when={formError()}>
            <p class="pg-notice" role="alert">{formError()}</p>
          </Show>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.name")}</span>
            <input class="pg-input" value={name()} maxLength={120} autocomplete="off" onInput={(e) => setName(e.currentTarget.value)} />
          </label>
          <label class="pg-field">
            <span class="pg-field-label">{t("notebooks.description")}</span>
            <textarea class="pg-input nb-textarea" rows={3} value={description()} onInput={(e) => setDescription(e.currentTarget.value)} />
          </label>
        </form>
      </Sheet>
    </Page>
  );
}
