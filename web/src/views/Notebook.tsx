import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createSignal, on, onCleanup, Show } from "solid-js";
import { ApiError, toApiError } from "../api/client";
import { hasPending, notebooksApi, type NotebookDetail } from "../api/notebooks";
import { BackIcon, GearIcon, MicIcon, PlusIcon, SearchIcon } from "../components/Icons";
import { Button, ErrorState, LoadingState, Page, PageHeader, PrimaryAction, SectionNav, Sheet, useWide } from "../components/page";
import { t } from "../i18n";
import { AddSourceSheet } from "./notebooks/AddSource";
import { ChatColumn } from "./notebooks/ChatColumn";
import { NotesColumn } from "./notebooks/NotesColumn";
import { PodcastSheet } from "./notebooks/Podcasts";
import { SearchSheet } from "./notebooks/SearchSheet";
import { SourceReader } from "./notebooks/SourceReader";
import { SourcesColumn } from "./notebooks/SourcesColumn";
import "../styles/notebooks.css";

// One notebook (NotebookLM style, ported from Open Notebook by Luis Novo, MIT): sources, linked Keep
// notes and the chat side by side on a desktop, three tabs on a phone. Route: /notebooks/:id.

type Tab = "sources" | "notes" | "chat";
const POLL_MS = 3000;

export default function NotebookView() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const { wide, track } = useWide();
  const [nb, setNb] = createSignal<NotebookDetail | null>(null);
  const [error, setError] = createSignal<ApiError | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [tab, setTab] = createSignal<Tab>("sources");
  const [adding, setAdding] = createSignal(false);
  const [podcasting, setPodcasting] = createSignal(false);
  const [searching, setSearching] = createSignal(false);
  const [settings, setSettings] = createSignal(false);
  const [reader, setReader] = createSignal<{ id: string; around: number | null } | null>(null);
  let generation = 0;

  async function load() {
    const mine = generation;
    try {
      const detail = await notebooksApi.get(params.id);
      if (mine === generation) {
        setNb(detail);
        setError(null);
      }
    } catch (e) {
      if (mine === generation) setError(toApiError(e));
    }
  }

  createEffect(
    on(
      () => params.id,
      () => {
        generation++;
        setNb(null);
        setError(null);
        setNotice(null);
        setReader(null);
        void load();
      },
    ),
  );

  // Follow sources that are being read and podcasts that are being made.
  const poll = setInterval(() => {
    const current = nb();
    if (current && hasPending(current)) void load();
  }, POLL_MS);
  onCleanup(() => clearInterval(poll));

  const openSource = (id: string, around: number | null = null) => setReader({ id, around });
  const readySources = () => (nb()?.sources ?? []).some((s) => s.status === "ready");

  const columns = (d: () => NotebookDetail) => {
    const show = (which: Tab) => wide() || tab() === which;
    return (
      <div class="nb-cols" classList={{ "nb-cols--wide": wide() }}>
        <Show when={show("sources")}>
          <SourcesColumn
            notebookId={d().id}
            sources={d().sources}
            onOpen={(id) => openSource(id)}
            onAdd={() => setAdding(true)}
            onChanged={() => void load()}
            onError={setNotice}
          />
        </Show>
        <Show when={show("notes")}>
          <NotesColumn notebookId={d().id} notes={d().notes} onChanged={() => void load()} onError={setNotice} />
        </Show>
        <Show when={show("chat")}>
          <ChatColumn
            notebookId={d().id}
            chats={d().chats}
            hasSources={readySources()}
            onCite={(id, ordinal) => openSource(id, ordinal)}
            onNoteSaved={() => void load()}
          />
        </Show>
      </div>
    );
  };

  const tabs = () => [
    { id: "sources" as const, label: t("notebooks.sources") },
    { id: "notes" as const, label: t("notebooks.notes") },
    { id: "chat" as const, label: t("notebooks.chat") },
  ];

  return (
    <div class="nb-slot" ref={track}>
      <Page class="nb-page nb-detail" label={nb()?.name}>
        <Show when={error() && !nb()}>
          <A href="/notebooks" class="pg-link pg-back">
            <BackIcon /> {t("notebooks.title")}
          </A>
          <Show
            when={error()!.status !== 404}
            fallback={<p class="pg-muted">{t("notebooks.notFound")}</p>}
          >
            <ErrorState message={error()!.message} retryLabel={t("common.tryAgain")} onRetry={() => void load()} />
          </Show>
        </Show>
        <Show when={!error() && !nb()}>
          <LoadingState label={t("notebooks.loading")} />
        </Show>
        <Show when={nb()}>
          {(d) => (
            <>
              <A href="/notebooks" class="pg-link pg-back">
                <BackIcon /> {t("notebooks.title")}
              </A>
              <PageHeader
                title={d().name}
                context={
                  <>
                    {d().description ? `${d().description} · ` : ""}
                    {t("notebooks.nSources", { count: d().counts.sources })} · {t("notebooks.nNotes", { count: d().counts.notes })}
                    <Show when={d().archived}> · {t("notebooks.archivedBadge")}</Show>
                  </>
                }
                nav={
                  <Show when={!wide()}>
                    <SectionNav
                      label={d().name}
                      items={tabs().map((x) => ({ label: x.label, onSelect: () => setTab(x.id), current: tab() === x.id }))}
                    />
                  </Show>
                }
                action={
                  <>
                    <button type="button" class="pg-icon-btn" aria-label={t("notebooks.search")} title={t("notebooks.search")} onClick={() => setSearching(true)}>
                      <SearchIcon />
                    </button>
                    <Button onClick={() => setPodcasting(true)}>
                      <MicIcon /> {t("notebooks.podcast")}
                    </Button>
                    <button type="button" class="pg-icon-btn" aria-label={t("notebooks.settings")} title={t("notebooks.settings")} onClick={() => setSettings(true)}>
                      <GearIcon />
                    </button>
                    <PrimaryAction icon={<PlusIcon />} onClick={() => setAdding(true)}>
                      {t("notebooks.addSource")}
                    </PrimaryAction>
                  </>
                }
              />
              <Show when={notice()}>
                <div class="pg-notice" role="alert">
                  <span>{notice()}</span>
                  <Button variant="ghost" onClick={() => setNotice(null)}>{t("common.dismiss")}</Button>
                </div>
              </Show>
              {columns(d)}
              <p class="nb-credit">
                {t("notebooks.creditBefore")} <a href="https://github.com/lfnovo/open-notebook" target="_blank" rel="noopener noreferrer">{t("notebooks.creditName")}</a> {t("notebooks.creditAfter")}
              </p>

              <AddSourceSheet open={adding()} notebookId={d().id} onClose={() => setAdding(false)} onAdded={() => void load()} />
              <PodcastSheet open={podcasting()} notebookId={d().id} podcasts={d().podcasts} onClose={() => setPodcasting(false)} onChanged={() => void load()} />
              <SearchSheet open={searching()} notebookId={d().id} onClose={() => setSearching(false)} onOpenSource={openSource} />
              <SettingsSheet
                open={settings()}
                notebook={d()}
                onClose={() => setSettings(false)}
                onChanged={() => void load()}
                onDeleted={() => navigate("/notebooks", { replace: true })}
              />
              <SourceReader sourceId={reader()?.id ?? null} around={reader()?.around ?? null} onClose={() => setReader(null)} onNoteSaved={() => void load()} />
            </>
          )}
        </Show>
      </Page>
    </div>
  );
}

/** Rename, archive, delete (keeping the sources, or deleting the ones no other notebook uses). */
function SettingsSheet(props: { open: boolean; notebook: NotebookDetail; onClose: () => void; onChanged: () => void; onDeleted: () => void }) {
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  const [sources, setSources] = createSignal<"keep" | "delete-exclusive">("keep");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  createEffect(() => {
    if (!props.open) return;
    setName(props.notebook.name);
    setDescription(props.notebook.description);
    setSources("keep");
    setError(null);
    setBusy(false);
  });

  async function run(f: () => Promise<unknown>, done: () => void) {
    setBusy(true);
    setError(null);
    try {
      await f();
      done();
    } catch (e) {
      setError(toApiError(e).message);
    } finally {
      setBusy(false);
    }
  }

  const id = () => props.notebook.id;

  return (
    <Sheet
      open={props.open}
      onClose={props.onClose}
      title={t("notebooks.settings")}
      class="nb-sheet"
      footer={
        <Button type="submit" form="nb-settings-form" disabled={busy() || !name().trim()}>
          {t("common.save")}
        </Button>
      }
    >
      <form
        id="nb-settings-form"
        class="nb-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (!name().trim() || busy()) return;
          void run(() => notebooksApi.update(id(), { name: name().trim(), description: description().trim() }), () => {
            props.onChanged();
            props.onClose();
          });
        }}
      >
        <Show when={error()}>
          <p class="pg-notice" role="alert">{error()}</p>
        </Show>
        <label class="pg-field">
          <span class="pg-field-label">{t("notebooks.name")}</span>
          <input class="pg-input" value={name()} maxLength={120} onInput={(e) => setName(e.currentTarget.value)} />
        </label>
        <label class="pg-field">
          <span class="pg-field-label">{t("notebooks.description")}</span>
          <textarea class="pg-input nb-textarea" rows={3} value={description()} onInput={(e) => setDescription(e.currentTarget.value)} />
        </label>
      </form>

      <section class="nb-danger-zone" aria-labelledby="nb-archive-h">
        <h3 id="nb-archive-h" class="nb-reader-h">{props.notebook.archived ? t("notebooks.unarchive") : t("notebooks.archive")}</h3>
        <p class="pg-muted">{props.notebook.archived ? t("notebooks.unarchiveHint") : t("notebooks.archiveHint")}</p>
        <Button
          disabled={busy()}
          onClick={() =>
            void run(() => notebooksApi.update(id(), { archived: !props.notebook.archived }), () => {
              props.onChanged();
              props.onClose();
            })
          }
        >
          {props.notebook.archived ? t("notebooks.unarchive") : t("notebooks.archive")}
        </Button>
      </section>

      <section class="nb-danger-zone" aria-labelledby="nb-delete-h">
        <h3 id="nb-delete-h" class="nb-reader-h">{t("notebooks.deleteNotebook")}</h3>
        <p class="pg-muted">{t("notebooks.deleteHint")}</p>
        <fieldset class="nb-radios">
          <legend class="pg-field-label">{t("notebooks.deleteSources")}</legend>
          <label>
            <input type="radio" name="nb-del-sources" checked={sources() === "keep"} onChange={() => setSources("keep")} />
            {t("notebooks.keepSources")}
          </label>
          <label>
            <input type="radio" name="nb-del-sources" checked={sources() === "delete-exclusive"} onChange={() => setSources("delete-exclusive")} />
            {t("notebooks.deleteExclusive")}
          </label>
        </fieldset>
        <Button
          variant="danger"
          disabled={busy()}
          onClick={() => {
            if (!confirm(t("notebooks.deleteConfirm", { name: props.notebook.name }))) return;
            void run(() => notebooksApi.remove(id(), sources()), props.onDeleted);
          }}
        >
          {t("notebooks.deleteNotebook")}
        </Button>
      </section>
    </Sheet>
  );
}
