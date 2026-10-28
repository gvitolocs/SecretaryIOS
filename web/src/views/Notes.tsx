import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { toApiError } from "../api/client";
import { NOTE_MAX_BYTES, NOTE_TITLE_MAX, notesApi, parseTags, plainExcerpt, utf8Bytes, type Note, type NoteSummary } from "../api/notes";
import { BackIcon, NoteIcon, PinIcon, PlusIcon, SearchIcon, TrashIcon } from "../components/Icons";
import { Markdown } from "../components/Markdown";
import {
  Card,
  EmptyState,
  ErrorState,
  ListRow,
  LoadingState,
  Page,
  PageGrid,
  PageHeader,
  PrimaryAction,
  RowList,
  useWide,
} from "../components/page";
import { t, type MessageKey } from "../i18n";
import { formatDate, formatDateTime } from "../lib/units";
import { onTimelineItem } from "../state/chat";

// Notes: briefings and documents from the owner, Lucina, ChatGPT, Grok and
// Claude Code (docs/server-api.md, "Notes"). One view for the list and the
// note: side by side on wide screens, one at a time on phones.
// Routes: /notes, /notes/new, /notes/trash, /notes/:key (slug or id).

const SEARCH_DEBOUNCE_MS = 250;

const SOURCE_KEYS: Record<string, MessageKey> = {
  owner: "notes.sourceOwner",
  web: "notes.sourceOwner",
  ios: "notes.sourceOwner",
  lucina: "notes.sourceLucina",
  chatgpt: "notes.sourceChatgpt",
  grok: "notes.sourceGrok",
  "claude-code": "notes.sourceClaude",
  claude: "notes.sourceClaude",
};

export function sourceLabel(source: string): string {
  const key = SOURCE_KEYS[source];
  return key ? t(key) : source;
}

export function SourceBadge(props: { source: string }) {
  return (
    <span class="src-badge note-src" data-source={props.source} title={`${t("notes.by")}: ${sourceLabel(props.source)}`}>
      <span class="src-label">{sourceLabel(props.source)}</span>
    </span>
  );
}

const ms = (iso: string) => {
  const n = Date.parse(iso);
  return Number.isFinite(n) ? n : 0;
};

// ---------------------------------------------------------------- list

function NoteCard(props: { note: NoteSummary; active: boolean }) {
  const excerpt = createMemo(() => plainExcerpt(props.note.excerpt, props.note.title));
  return (
    <li>
      <A
        href={`/notes/${props.note.slug}`}
        class="note-card"
        classList={{ "note-card--active": props.active, "note-card--pinned": props.note.pinned }}
        aria-current={props.active ? "page" : undefined}
      >
        <span class="note-card-head">
          <Show when={props.note.pinned}>
            <span class="note-pin" aria-label={t("notes.pinned")}>
              <PinIcon />
            </span>
          </Show>
          <span class="note-card-title">{props.note.title}</span>
        </span>
        <Show when={excerpt()}>
          <span class="note-card-excerpt">{excerpt()}</span>
        </Show>
        <span class="note-card-meta">
          <SourceBadge source={props.note.source} />
          <span class="note-card-date">{formatDate(ms(props.note.updatedAt))}</span>
          <For each={props.note.tags.slice(0, 3)}>{(tag) => <span class="note-tag">#{tag}</span>}</For>
        </span>
      </A>
    </li>
  );
}

function NoteDeskRow(props: { note: NoteSummary; active: boolean }) {
  const excerpt = createMemo(() => plainExcerpt(props.note.excerpt, props.note.title));
  return (
    <ListRow
      class={props.active ? "notes-desk-row--active" : undefined}
      href={`/notes/${props.note.slug}`}
      title={props.note.title}
      leading={
        <Show when={props.note.pinned}>
          <span class="note-pin" aria-label={t("notes.pinned")}>
            <PinIcon />
          </span>
        </Show>
      }
      sub={excerpt()}
      meta={
        <>
          <SourceBadge source={props.note.source} />
          {" · "}
          {formatDate(ms(props.note.updatedAt))}
        </>
      }
    />
  );
}

// ---------------------------------------------------------------- editor

function NoteEditor(props: { note?: Note; onSaved: (n: Note) => void; onCancel: () => void }) {
  const [title, setTitle] = createSignal(props.note?.title ?? "");
  const [content, setContent] = createSignal(props.note?.content ?? "");
  const [tagsText, setTagsText] = createSignal((props.note?.tags ?? []).join(", "));
  const [tab, setTab] = createSignal<"write" | "preview">("write");
  const [error, setError] = createSignal<string | null>(null);
  const [conflict, setConflict] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const bytes = createMemo(() => utf8Bytes(content()));
  const chars = createMemo(() => [...content()].length);

  async function save(e: Event) {
    e.preventDefault();
    if (pending()) return;
    const tt = title().trim();
    if (!tt) return setError(t("notes.errTitle"));
    if (bytes() > NOTE_MAX_BYTES) return setError(t("notes.errTooLarge"));
    setError(null);
    setConflict(false);
    setPending(true);
    try {
      const tags = parseTags(tagsText());
      const saved = props.note
        ? await notesApi.update(props.note.id, { title: tt, content: content(), tags }, props.note.updatedAt)
        : await notesApi.create({ title: tt, content: content(), tags });
      props.onSaved(saved);
    } catch (err) {
      const e2 = toApiError(err);
      if (e2.status === 409 && e2.code === "conflict") setConflict(true);
      else setError(e2.code === "note_too_large" ? t("notes.errTooLarge") : e2.message);
    } finally {
      setPending(false);
    }
  }

  return (
    <form class="note-editor card" onSubmit={(e) => void save(e)} aria-label={props.note ? t("notes.edit") : t("notes.newNote")}>
      <label class="field-label" for="note-title">
        {t("notes.titleLabel")}
      </label>
      <input
        id="note-title"
        class="text-input"
        type="text"
        value={title()}
        maxlength={NOTE_TITLE_MAX}
        onInput={(e) => setTitle(e.currentTarget.value)}
        disabled={pending()}
        autocomplete="off"
      />
      <label class="field-label" for="note-tags">
        {t("notes.tagsLabel")}
      </label>
      <input
        id="note-tags"
        class="text-input"
        type="text"
        value={tagsText()}
        onInput={(e) => setTagsText(e.currentTarget.value)}
        disabled={pending()}
        autocomplete="off"
        placeholder="work, idee"
      />
      <div class="note-editor-tabs">
        <span class="field-label" id="note-content-label">
          {t("notes.contentLabel")}
        </span>
        <div class="segmented" role="tablist">
          <button type="button" role="tab" class="segmented-btn" classList={{ "segmented-btn--active": tab() === "write" }} aria-selected={tab() === "write"} onClick={() => setTab("write")}>
            {t("notes.write")}
          </button>
          <button type="button" role="tab" class="segmented-btn" classList={{ "segmented-btn--active": tab() === "preview" }} aria-selected={tab() === "preview"} onClick={() => setTab("preview")}>
            {t("notes.preview")}
          </button>
        </div>
      </div>
      <Show
        when={tab() === "write"}
        fallback={
          <div class="note-preview">
            <Show when={content().trim()} fallback={<p class="muted">{t("notes.previewEmpty")}</p>}>
              <Markdown text={content()} />
            </Show>
          </div>
        }
      >
        <textarea
          class="text-input note-textarea"
          aria-labelledby="note-content-label"
          value={content()}
          onInput={(e) => setContent(e.currentTarget.value)}
          disabled={pending()}
          spellcheck={true}
        />
      </Show>
      <p class="note-editor-count muted small" classList={{ "note-editor-count--over": bytes() > NOTE_MAX_BYTES }}>
        {t("notes.size", { count: chars() })}
      </p>
      <Show when={error()}>
        {(m) => (
          <p class="form-error" role="alert">
            {m()}
          </p>
        )}
      </Show>
      <Show when={conflict()}>
        <p class="form-error" role="alert">
          {t("notes.conflict")}
        </p>
      </Show>
      <div class="note-editor-actions">
        <button type="button" class="btn" onClick={() => props.onCancel()} disabled={pending()}>
          {t("common.cancel")}
        </button>
        <button type="submit" class="btn btn--primary" disabled={pending()}>
          {pending() ? t("common.saving") : t("common.save")}
        </button>
      </div>
    </form>
  );
}

// ---------------------------------------------------------------- one note

function NoteDetail(props: { noteKey: string; version: number; onChanged: () => void; onDeleted: (n: Note) => void }) {
  const navigate = useNavigate();
  const [note, setNote] = createSignal<Note | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [missing, setMissing] = createSignal(false);
  const [editing, setEditing] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  async function load() {
    setError(null);
    try {
      const n = await notesApi.get(props.noteKey);
      setMissing(false);
      setNote(n);
    } catch (err) {
      const e = toApiError(err);
      if (e.status === 404) {
        setMissing(true);
        setNote(null);
      } else setError(e.message);
    }
  }

  createEffect(
    on(
      () => props.noteKey,
      () => {
        setNote(null);
        setEditing(false);
        setConfirming(false);
        void load();
      },
    ),
  );
  // Lucina added to it (live timeline): refresh unless the owner is editing.
  createEffect(
    on(
      () => props.version,
      () => {
        if (!editing()) void load();
      },
      { defer: true },
    ),
  );

  async function togglePin() {
    const n = note();
    if (!n || busy()) return;
    setBusy(true);
    try {
      setNote(await notesApi.update(n.id, { pinned: !n.pinned }));
      props.onChanged();
    } catch (err) {
      setError(toApiError(err).message);
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    const n = note();
    if (!n || busy()) return;
    setBusy(true);
    try {
      await notesApi.remove(n.id);
      props.onDeleted(n);
    } catch (err) {
      setError(toApiError(err).message);
      setBusy(false);
    }
  }

  return (
    <div class="note-detail">
      <A href="/notes" class="note-back">
        <BackIcon />
        <span>{t("notes.backToList")}</span>
      </A>
      <Show when={error()}>
        {(m) => (
          <div class="notice" role="alert">
            <span>{m()}</span>
            <button class="btn btn--small" type="button" onClick={() => void load()}>
              {t("notes.reload")}
            </button>
          </div>
        )}
      </Show>
      <Show when={missing()}>
        <div class="empty" role="status">
          <p class="empty-title">{t("notes.notFound")}</p>
          <A class="btn" href="/notes">
            {t("notes.backToList")}
          </A>
        </div>
      </Show>
      <Show when={note()}>
        {(n) => (
          <Show
            when={!editing()}
            fallback={
              <NoteEditor
                note={n()}
                onCancel={() => setEditing(false)}
                onSaved={(saved) => {
                  setNote(saved);
                  setEditing(false);
                  props.onChanged();
                  if (saved.slug !== props.noteKey) navigate(`/notes/${saved.slug}`, { replace: true });
                }}
              />
            }
          >
            <article class="note-article card">
              <header class="note-article-head">
                <h1 class="note-title">{n().title}</h1>
                <div class="note-actions">
                  <button
                    type="button"
                    class="icon-btn"
                    classList={{ "icon-btn--accent": n().pinned }}
                    onClick={() => void togglePin()}
                    disabled={busy()}
                    aria-pressed={n().pinned}
                    aria-label={n().pinned ? t("notes.unpin") : t("notes.pin")}
                    title={n().pinned ? t("notes.unpin") : t("notes.pin")}
                  >
                    <PinIcon />
                  </button>
                  <button type="button" class="btn btn--small" onClick={() => setEditing(true)}>
                    {t("notes.edit")}
                  </button>
                  <button
                    type="button"
                    class="icon-btn"
                    onClick={() => setConfirming(true)}
                    disabled={busy()}
                    aria-label={t("notes.delete")}
                    title={t("notes.delete")}
                  >
                    <TrashIcon />
                  </button>
                </div>
              </header>
              <div class="note-meta">
                <SourceBadge source={n().source} />
                <span class="note-meta-text">{t("notes.updated", { date: formatDateTime(ms(n().updatedAt)) })}</span>
                <span class="note-meta-text note-slug">{t("notes.name", { slug: n().slug })}</span>
              </div>
              <Show when={n().tags.length}>
                <div class="note-tags">
                  <For each={n().tags}>{(tag) => <span class="note-tag">#{tag}</span>}</For>
                </div>
              </Show>
              <Show when={confirming()}>
                <div class="note-confirm" role="alertdialog" aria-label={t("common.confirmDelete")}>
                  <p>{t("notes.deleteConfirm", { title: n().title })}</p>
                  <div class="note-editor-actions">
                    <button type="button" class="btn btn--small" onClick={() => setConfirming(false)} disabled={busy()}>
                      {t("common.cancel")}
                    </button>
                    <button type="button" class="btn btn--small btn--danger" onClick={() => void remove()} disabled={busy()}>
                      {t("notes.delete")}
                    </button>
                  </div>
                </div>
              </Show>
              <Markdown text={n().content} class="note-body" skipHeading={n().title} />
              <p class="note-hint muted small">{t("notes.askLucina", { slug: n().slug })}</p>
            </article>
          </Show>
        )}
      </Show>
      <Show when={!note() && !missing() && !error()}>
        <p class="muted center" role="status">
          {t("notes.loading")}
        </p>
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------- trash

function TrashPanel(props: { onRestored: (n: Note) => void }) {
  const [items, setItems] = createSignal<NoteSummary[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const load = async () => {
    try {
      setItems(await notesApi.trash());
    } catch (err) {
      setError(toApiError(err).message);
    }
  };
  onMount(() => void load());
  async function restore(id: string) {
    try {
      const n = await notesApi.restore(id);
      props.onRestored(n);
    } catch (err) {
      setError(toApiError(err).message);
    }
  }
  return (
    <div class="note-detail">
      <A href="/notes" class="note-back">
        <BackIcon />
        <span>{t("notes.backToList")}</span>
      </A>
      <section class="card note-trash" aria-labelledby="trash-title">
        <h1 class="note-title" id="trash-title">
          {t("notes.trashTitle")}
        </h1>
        <p class="muted small">{t("notes.trashHint", { days: 30 })}</p>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
        <Show when={items()} fallback={<p class="muted">{t("notes.loading")}</p>}>
          {(list) => (
            <Show when={list().length} fallback={<p class="muted">{t("notes.trashEmpty")}</p>}>
              <ul class="note-trash-list">
                <For each={list()}>
                  {(n) => (
                    <li class="note-trash-row">
                      <div class="note-trash-text">
                        <span class="note-card-title">{n.title}</span>
                        <span class="muted small">{t("notes.purgeOn", { date: formatDate(ms(n.purgeAfter ?? "")) })}</span>
                      </div>
                      <button type="button" class="btn btn--small" onClick={() => void restore(n.id)}>
                        {t("notes.restore")}
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          )}
        </Show>
      </section>
    </div>
  );
}

// ---------------------------------------------------------------- view

export default function NotesView() {
  const params = useParams<{ key?: string }>();
  const navigate = useNavigate();
  const [notes, setNotes] = createSignal<NoteSummary[] | null>(null);
  const [tagsAll, setTagsAll] = createSignal<string[]>([]);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [query, setQuery] = createSignal("");
  const [tag, setTag] = createSignal("");
  const [version, setVersion] = createSignal(0);
  const [flash, setFlash] = createSignal<{ text: string; undoId?: string } | null>(null);
  let gen = 0;

  const key = () => params.key ?? "";
  const mode = createMemo(() => (key() === "new" ? "new" : key() === "trash" ? "trash" : key() ? "note" : "none"));

  async function refresh() {
    const my = ++gen;
    try {
      const list = await notesApi.list(query(), tag());
      if (my !== gen) return;
      setNotes(list);
      setLoadError(null);
      if (!query() && !tag()) {
        const seen = new Set<string>();
        for (const n of list) for (const tg of n.tags) seen.add(tg);
        setTagsAll([...seen].sort());
      }
    } catch (err) {
      if (my === gen) setLoadError(toApiError(err).message);
    }
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on([query, tag], () => {
      clearTimeout(timer);
      timer = setTimeout(() => void refresh(), notes() === null ? 0 : SEARCH_DEBOUNCE_MS);
    }),
  );
  onCleanup(() => clearTimeout(timer));

  onMount(() => {
    // Lucina saved or added to a note somewhere: refresh what is on screen.
    const off = onTimelineItem((item) => {
      if (item.role === "lucina" && (item.meta?.kind === "note" || item.meta?.kind === "notes")) {
        void refresh();
        setVersion((v) => v + 1);
      }
    });
    onCleanup(off);
  });

  const pinned = createMemo(() => (notes() ?? []).filter((n) => n.pinned));
  const others = createMemo(() => (notes() ?? []).filter((n) => !n.pinned));
  const activeKey = (n: NoteSummary) => key() === n.slug || key() === n.id;

  async function undoDelete(id: string) {
    setFlash(null);
    try {
      const n = await notesApi.restore(id);
      await refresh();
      navigate(`/notes/${n.slug}`);
    } catch (err) {
      setLoadError(toApiError(err).message);
    }
  }

  const measured = useWide();
  const wide = measured.wide;

  const flashNotice = (
    <Show when={flash()}>
      {(f) => (
        <div class={wide() ? "pg-notice" : "notice"} role="status">
          <span>{f().text}</span>
          <Show when={f().undoId}>
            {(id) => (
              <button class={wide() ? "pg-btn" : "btn btn--small"} type="button" onClick={() => void undoDelete(id())}>
                {t("notes.undo")}
              </button>
            )}
          </Show>
          <button class="banner-close" type="button" onClick={() => setFlash(null)} aria-label={t("common.dismiss")}>
            ×
          </button>
        </div>
      )}
    </Show>
  );

  const tagFilters = (
    <Show when={tagsAll().length}>
      <div class="chip-group notes-tags" role="group" aria-label={t("notes.filterTag")}>
        <button type="button" class="chip" classList={{ "chip--active": !tag() }} aria-pressed={!tag()} onClick={() => setTag("")}>
          {t("notes.all")}
        </button>
        <For each={tagsAll()}>
          {(tg) => (
            <button type="button" class="chip" classList={{ "chip--active": tag() === tg }} aria-pressed={tag() === tg} onClick={() => setTag(tag() === tg ? "" : tg)}>
              #{tg}
            </button>
          )}
        </For>
      </div>
    </Show>
  );

  const noteRows = (desk: boolean) => (
    <>
      <Show when={pinned().length}>
        <Show when={desk}>
          <p class="notes-desk-group-label">{t("notes.pinned")}</p>
        </Show>
        <Show when={!desk}>
          <h2 class="notes-section hud">{t("notes.pinned")}</h2>
        </Show>
        <Show
          when={desk}
          fallback={
            <ul class="note-list">
              <For each={pinned()}>{(n) => <NoteCard note={n} active={activeKey(n)} />}</For>
            </ul>
          }
        >
          <RowList label={t("notes.pinned")}>
            <For each={pinned()}>{(n) => <NoteDeskRow note={n} active={activeKey(n)} />}</For>
          </RowList>
        </Show>
      </Show>
      <Show when={others().length}>
        <Show when={pinned().length}>
          <Show when={desk}>
            <p class="notes-desk-group-label">{t("notes.others")}</p>
          </Show>
          <Show when={!desk}>
            <h2 class="notes-section hud">{t("notes.others")}</h2>
          </Show>
        </Show>
        <Show
          when={desk}
          fallback={
            <ul class="note-list">
              <For each={others()}>{(n) => <NoteCard note={n} active={activeKey(n)} />}</For>
            </ul>
          }
        >
          <RowList label={t("notes.others")}>
            <For each={others()}>{(n) => <NoteDeskRow note={n} active={activeKey(n)} />}</For>
          </RowList>
        </Show>
      </Show>
    </>
  );

  const listBody = (desk: boolean) => (
    <Show
      when={notes()}
      fallback={
        <Show when={!loadError()}>
          <Show when={desk} fallback={<p class="muted center" role="status">{t("notes.loading")}</p>}>
            <LoadingState label={t("notes.loading")} />
          </Show>
        </Show>
      }
    >
      {(list) => (
        <Show
          when={list().length}
          fallback={
            <Show
              when={query() || tag()}
              fallback={
                <Show
                  when={desk}
                  fallback={
                    <div class="empty notes-empty">
                      <span class="notes-empty-icon" aria-hidden="true">
                        <NoteIcon />
                      </span>
                      <p class="empty-title">{t("notes.empty")}</p>
                      <p class="empty-sub">{t("notes.emptySub")}</p>
                    </div>
                  }
                >
                  <EmptyState title={t("notes.empty")} text={t("notes.emptySub")} />
                </Show>
              }
            >
              <Show when={desk} fallback={<p class="muted center">{t("notes.noResults", { q: query() || `#${tag()}` })}</p>}>
                <EmptyState text={t("notes.noResults", { q: query() || `#${tag()}` })} />
              </Show>
            </Show>
          }
        >
          {noteRows(desk)}
        </Show>
      )}
    </Show>
  );

  const detailBody = (desk: boolean) => (
    <>
      <Show when={mode() === "none"}>
        <Show when={desk} fallback={<div class="notes-placeholder" aria-hidden="true"><NoteIcon /></div>}>
          <EmptyState text={t("notes.pickNote")} />
        </Show>
      </Show>
      <Show when={mode() === "new"}>
        <div class="note-detail">
          <Show when={!desk}>
            <A href="/notes" class="note-back">
              <BackIcon />
              <span>{t("notes.backToList")}</span>
            </A>
          </Show>
          <NoteEditor
            onCancel={() => navigate("/notes")}
            onSaved={(n) => {
              void refresh();
              navigate(`/notes/${n.slug}`, { replace: true });
            }}
          />
        </div>
      </Show>
      <Show when={mode() === "trash"}>
        <TrashPanel
          onRestored={(n) => {
            setFlash({ text: t("notes.restored", { title: n.title }) });
            void refresh();
            navigate(`/notes/${n.slug}`);
          }}
        />
      </Show>
      <Show when={mode() === "note" && key()}>
        {(k) => (
          <NoteDetail
            noteKey={k()}
            version={version()}
            onChanged={() => void refresh()}
            onDeleted={(n) => {
              setFlash({ text: t("notes.movedToTrash", { title: n.title }), undoId: n.id });
              void refresh();
              navigate("/notes", { replace: true });
            }}
          />
        )}
      </Show>
    </>
  );

  const deskList = (
    <Card list label={t("notes.title")}>
      <div class="notes-desk-list-tools">
        <input
          class="pg-input"
          type="search"
          placeholder={t("notes.search")}
          aria-label={t("notes.search")}
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
          autocomplete="off"
        />
        {tagFilters}
      </div>
      <Show when={loadError()}>
        {(m) => <ErrorState message={m()} retryLabel={t("common.tryAgain")} onRetry={() => void refresh()} />}
      </Show>
      <Show when={!loadError()}>{listBody(true)}</Show>
    </Card>
  );

  return (
    <div class="notes" classList={{ "notes--open": mode() !== "none" && !wide(), "notes--desk": wide() }} ref={measured.track}>
      <Show
        when={wide()}
        fallback={
          <div class="notes-inner">
            <aside class="notes-pane notes-pane--list" aria-label={t("notes.title")}>
              <header class="notes-head">
                <h1 class="view-title">{t("notes.title")}</h1>
                <div class="notes-head-actions">
                  <A href="/notes/trash" class="icon-btn" aria-label={t("notes.trash")} title={t("notes.trash")}>
                    <TrashIcon />
                  </A>
                  <A href="/notes/new" class="btn btn--primary btn--small">
                    <PlusIcon />
                    <span>{t("notes.newNote")}</span>
                  </A>
                </div>
              </header>
              {flashNotice}
              <label class="notes-search">
                <SearchIcon />
                <span class="sr-only">{t("notes.search")}</span>
                <input
                  class="notes-search-input"
                  type="search"
                  placeholder={t("notes.search")}
                  value={query()}
                  onInput={(e) => setQuery(e.currentTarget.value)}
                  autocomplete="off"
                />
              </label>
              {tagFilters}
              <Show when={loadError()}>
                {(m) => (
                  <div class="notice" role="alert">
                    <span>{m()}</span>
                    <button class="btn btn--small" type="button" onClick={() => void refresh()}>
                      {t("common.tryAgain")}
                    </button>
                  </div>
                )}
              </Show>
              {listBody(false)}
            </aside>
            <section class="notes-pane notes-pane--detail">{detailBody(false)}</section>
          </div>
        }
      >
        <Page class="notes-desk">
          <PageHeader
            title={t("notes.title")}
            action={
              <>
                <A href="/notes/trash" class="pg-icon-btn" aria-label={t("notes.trash")} title={t("notes.trash")}>
                  <TrashIcon />
                </A>
                <PrimaryAction href="/notes/new" icon={<PlusIcon />}>
                  {t("notes.newNote")}
                </PrimaryAction>
              </>
            }
          />
          {flashNotice}
          <PageGrid layout="split" railLabel={t("notes.title")} rail={deskList}>
            {detailBody(true)}
          </PageGrid>
        </Page>
      </Show>
    </div>
  );
}
