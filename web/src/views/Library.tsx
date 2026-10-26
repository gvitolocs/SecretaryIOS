import { A, useParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { ApiError } from "../api/client";
import { libraryApi, type BookPatch, type Highlight, type Progress, type ReadingStatus } from "../api/library";
import { parseTags } from "../api/notes";
import { BackIcon, SearchIcon } from "../components/Icons";
import { locale, t } from "../i18n";

const statuses: ReadingStatus[] = ["reading", "completed", "abandoned", "not_started", "unknown"];
const statusLabel = (status: ReadingStatus) => t(`library.${statuses.includes(status) ? status : "unknown"}`);
const percent = (p: Progress | null) =>
  p?.percent == null ? "—" : `${new Intl.NumberFormat(locale(), { maximumFractionDigits: 1 }).format(p.percent)}%`;
function date(value: string) {
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? value : d.toLocaleString(locale());
}
function ErrorNotice(props: { retry: () => void }) {
  return (
    <div class="notice" role="alert">
      <span>{t("library.error")}</span>
      <button class="btn btn--small" onClick={props.retry}>
        {t("common.tryAgain")}
      </button>
    </div>
  );
}
function HighlightCard(props: { highlight: Highlight }) {
  const h = () => props.highlight;
  return (
    <li class="library-highlight" id={`highlight-${h().id}`}>
      <p class="library-text">{h().text}</p>
      <Show when={h().note}>{(note) => <p class="library-text">{note()}</p>}</Show>
      <div class="library-meta muted small">
        <Show when={h().location}>
          {(loc) => (
            <span>{t("library.location", { value: loc().start === loc().end ? String(loc().start) : `${loc().start}–${loc().end}` })}</span>
          )}
        </Show>
        <Show when={h().page}>{(page) => <span>{t("library.page", { value: page() })}</span>}</Show>
        {/* Keep local Kindle dates as supplied: converting to Date would invent a timezone. */}
        <span>{h().addedAtRaw || h().addedAt || t("library.unknownDate")}</span>
        <span>
          {t("library.source")}: {h().source}
        </span>
      </div>
    </li>
  );
}
function Activity() {
  const [data, { refetch }] = createResource(async () => {
    const [stats, sync, week, month] = await Promise.all([
      libraryApi.stats(),
      libraryApi.sync(),
      libraryApi.recent(7),
      libraryApi.recent(30),
    ]);
    return { stats, sync, week, month };
  });
  return (
    <section class="library-section" aria-label={t("library.activity")}>
      <h2>{t("library.activity")}</h2>
      <Show when={data.error}>
        <ErrorNotice retry={() => void refetch()} />
      </Show>
      <Show when={data.loading}>
        <p role="status" class="muted">
          {t("common.loading")}
        </p>
      </Show>
      <Show when={!data.error && data()}>
        {(d) => (
          <>
            <p>
              {t("library.lastImport")}:{" "}
              {d().sync.imports.length ? date(d().sync.imports[d().sync.imports.length - 1]!.time) : t("library.noImport")}
            </p>
            <dl class="library-counts">
              <For each={statuses}>
                {(s) => (
                  <div>
                    <dt>{statusLabel(s)}</dt>
                    <dd>{d().stats.byStatus[s] ?? 0}</dd>
                  </div>
                )}
              </For>
            </dl>
            <p>{t("library.recent", { days: 7, count: d().week.highlights.length })}</p>
            <p>{t("library.recent", { days: 30, count: d().month.highlights.length })}</p>
            <Show when={d().week.highlights.length >= 500 || d().month.highlights.length >= 500}>
              <p class="muted small">{t("library.recentLimit")}</p>
            </Show>
            <p class="muted small">{t("library.dates")}</p>
            <Show
              when={
                d().week.readingSessions === "unavailable_on_stock_kindle" || d().month.readingSessions === "unavailable_on_stock_kindle"
              }
            >
              <p class="muted">{t("library.noSessions")}</p>
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}
function LibraryHome() {
  const [query, setQuery] = createSignal("");
  const [search, setSearch] = createSignal("");
  const [status, setStatus] = createSignal<ReadingStatus | "">("");
  const [onKindle, setOnKindle] = createSignal(false);
  createEffect(() => {
    const q = query().trim();
    const timer = setTimeout(() => setSearch(q), 250);
    onCleanup(() => clearTimeout(timer));
  });
  const [data, { refetch }] = createResource(
    () => ({ q: search(), status: status() || undefined, onKindle: onKindle() || undefined }),
    async (filters) => {
      const [books, hits] = await Promise.all([
        libraryApi.list({ ...filters, limit: 500 }),
        filters.q ? libraryApi.search(filters.q, 500) : Promise.resolve([]),
      ]);
      return { books: books.filter((b) => b.kind !== "dictionary"), hits, capped: books.length >= 500 };
    },
  );
  return (
    <>
      <h1 class="view-title">{t("library.title")}</h1>
      <label class="notes-search">
        <SearchIcon />
        <span class="sr-only">{t("library.search")}</span>
        <input
          class="notes-search-input"
          type="search"
          maxlength={200}
          value={query()}
          placeholder={t("library.search")}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
      </label>
      <div class="chip-group library-filters" role="group" aria-label={t("library.status")}>
        <For each={["", "reading", "completed", "not_started", "unknown"] as const}>
          {(s) => (
            <button class="chip" classList={{ "chip--active": status() === s }} aria-pressed={status() === s} onClick={() => setStatus(s)}>
              {s ? statusLabel(s) : t("library.all")}
            </button>
          )}
        </For>
        <button class="chip" classList={{ "chip--active": onKindle() }} aria-pressed={onKindle()} onClick={() => setOnKindle(!onKindle())}>
          {t("library.onKindle")}
        </button>
      </div>
      <Show when={data.error}>
        <ErrorNotice retry={() => void refetch()} />
      </Show>
      <Show when={data.loading}>
        <p class="muted" role="status">
          {t("common.loading")}
        </p>
      </Show>
      <Show when={!data.error && data()}>
        {(d) => (
          <>
            <Show
              when={d().books.length}
              fallback={
                <Show
                  when={search() || status() || onKindle()}
                  fallback={
                    <div class="empty">
                      <p class="empty-title">{t("library.empty")}</p>
                      <p class="empty-sub">{t("library.emptySub")}</p>
                      <code class="library-command">lucina-ink import /Volumes/Kindle --server … --token-file …</code>
                    </div>
                  }
                >
                  <p class="muted">{t("library.noResults")}</p>
                </Show>
              }
            >
              <ul class="note-list">
                <For each={d().books}>
                  {(b) => (
                    <li class="library-card">
                      <A href={`/library/${encodeURIComponent(b.id)}`} class="library-book-link">
                        <h2>{b.title}</h2>
                        <p class="muted">{b.authors.join(", ")}</p>
                      </A>
                      <div class="library-meta small">
                        <span class="chip">{statusLabel(b.status)}</span>
                        <span>
                          {t("library.progress")}: {percent(b.progress)}
                        </span>
                        <span>{t("library.highlightCount", { count: b.highlightCount })}</span>
                        <Show when={b.onKindle}>
                          <span>{t("library.onKindle")}</span>
                        </Show>
                        <Show when={b.drm === "protected"}>
                          <span class="muted">{t("library.protected")}</span>
                        </Show>
                      </div>
                      <For each={d().hits.filter((hit) => hit.bookId === b.id && hit.highlightId)}>
                        {(hit) => (
                          <A
                            class="library-snippet muted small"
                            href={`/library/${encodeURIComponent(b.id)}#highlight-${encodeURIComponent(hit.highlightId!)}`}
                          >
                            {hit.snippet}
                          </A>
                        )}
                      </For>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <Show when={d().capped}>
              <p class="muted small">{t("library.listLimit")}</p>
            </Show>
          </>
        )}
      </Show>
      <Activity />
    </>
  );
}
function BookDetail(props: { id: string }) {
  const [data, { refetch, mutate }] = createResource(() => props.id, libraryApi.get);
  const [tags, setTags] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  createEffect(() => {
    const b = !data.error && data()?.book;
    if (b) setTags(b.tags.join(", "));
  });
  async function save(patch: BookPatch) {
    const b = !data.error && data()?.book;
    if (!b || busy()) return;
    setBusy(true);
    setMessage("");
    try {
      const book = await libraryApi.update(b.id, patch, b.updatedAt);
      mutate((old) => (old ? { ...old, book } : old));
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        try {
          await refetch();
          setMessage(t("library.conflict"));
        } catch {
          setMessage(t("library.error"));
        }
      } else setMessage(t("library.error"));
    } finally {
      setBusy(false);
    }
  }
  async function makeBrief() {
    if (busy()) return;
    setBusy(true);
    setMessage("");
    try {
      const brief = await libraryApi.brief(props.id);
      mutate((old) =>
        old ? { ...old, book: { ...old.book, briefs: [...old.book.briefs.filter((b) => b.id !== brief.id), brief] } } : old,
      );
      await refetch();
    } catch {
      setMessage(t("library.error"));
    } finally {
      setBusy(false);
    }
  }
  function saveTags() {
    const parsed = parseTags(tags());
    if (parsed.length > 16 || parsed.some((tag) => [...tag].length > 32)) {
      setMessage(t("library.tagsInvalid"));
      return;
    }
    void save({ tags: parsed });
  }
  return (
    <>
      <A href="/library" class="library-back">
        <BackIcon />
        {t("library.back")}
      </A>
      <Show when={data.error}>
        <ErrorNotice retry={() => void refetch()} />
      </Show>
      <Show when={data.loading}>
        <p role="status">{t("common.loading")}</p>
      </Show>
      <Show when={message()}>
        {(m) => (
          <p class="notice" role="alert">
            {m()}
          </p>
        )}
      </Show>
      <Show when={!data.error && data()}>
        {(d) => (
          <>
            <h1 class="view-title">{d().book.title}</h1>
            <p class="muted">{d().book.authors.join(", ")}</p>
            <section class="library-section">
              <h2>{t("library.metadata")}</h2>
              <dl class="library-metadata">
                <dt>{t("library.formats")}</dt>
                <dd>{d().book.formats.join(", ") || "—"}</dd>
                <For each={["isbn", "doi", "asin"] as const}>
                  {(key) => (
                    <Show when={d().book[key]}>
                      {(value) => (
                        <>
                          <dt>{t(`library.${key}`)}</dt>
                          <dd>{value()}</dd>
                        </>
                      )}
                    </Show>
                  )}
                </For>
              </dl>
              <Show when={d().book.drm === "protected"}>
                <p class="muted small">{t("library.protected")}</p>
              </Show>
              <h3>{t("library.presence")}</h3>
              <div class="library-meta">
                <Show when={d().book.owned}>
                  <span>{t("library.owned")}</span>
                </Show>
                <Show when={d().book.onKindle}>
                  <span>{t("library.onKindle")}</span>
                </Show>
                <Show when={d().book.removedFromKindleAt}>{(at) => <span>{t("library.removed", { date: date(at()) })}</span>}</Show>
                <Show when={!d().book.owned && !d().book.onKindle && d().highlights.length}>
                  <span>{t("library.clippingsOnly")}</span>
                </Show>
                <Show when={!d().book.owned && !d().book.onKindle && !d().highlights.length && !d().book.removedFromKindleAt}>
                  <span>{t("library.unknown")}</span>
                </Show>
              </div>
            </section>
            <section class="library-section library-controls">
              <label for="library-status">{t("library.status")}</label>
              <select
                id="library-status"
                class="text-input"
                disabled={busy()}
                value={d().book.status}
                onChange={(e) => void save({ status: e.currentTarget.value as ReadingStatus })}
              >
                <For each={statuses}>{(s) => <option value={s}>{statusLabel(s)}</option>}</For>
              </select>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  saveTags();
                }}
              >
                <label for="library-tags">{t("library.tags")}</label>
                <input
                  id="library-tags"
                  class="text-input"
                  value={tags()}
                  disabled={busy()}
                  onInput={(e) => setTags(e.currentTarget.value)}
                />
                <button class="btn btn--small" disabled={busy()}>
                  {t("library.saveTags")}
                </button>
              </form>
              <h2>{t("library.progress")}</h2>
              <p>{d().book.progress?.percent == null ? t("library.unknownProgress") : percent(d().book.progress)}</p>
              <Show when={d().book.progress?.location != null}>
                <p>{t("library.location", { value: String(d().book.progress!.location) })}</p>
              </Show>
              <Show when={d().book.progress?.page != null}>
                <p>{t("library.page", { value: String(d().book.progress!.page) })}</p>
              </Show>
            </section>
            <section class="library-section">
              <h2>{t("library.brief")}</h2>
              <button class="btn btn--primary btn--small" disabled={busy() || !d().highlights.length} onClick={() => void makeBrief()}>
                {t("library.makeBrief")}
              </button>
              <For each={d().book.briefs}>
                {(brief) => (
                  <article class="library-highlight">
                    <h3 class="muted small">{t("library.generated")}</h3>
                    <p class="library-text">{brief.text}</p>
                    <p class="muted small">{date(brief.generatedAt)}</p>
                  </article>
                )}
              </For>
            </section>
            <section class="library-section">
              <h2>{t("library.highlights")}</h2>
              <Show when={d().highlights.length} fallback={<p class="muted">{t("library.noHighlights")}</p>}>
                <ul class="note-list">
                  <For each={d().highlights}>{(h) => <HighlightCard highlight={h} />}</For>
                </ul>
              </Show>
            </section>
            <section class="library-section">
              <h2>{t("library.related")}</h2>
              <Show when={d().book.relatedNotes.length} fallback={<p class="muted">{t("library.unconnected")}</p>}>
                <ul>
                  <For each={d().book.relatedNotes}>
                    {(note) => (
                      <li>
                        {note.title}{" "}
                        <span class="muted small">
                          {note.vault}: {note.path}
                        </span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </section>
          </>
        )}
      </Show>
    </>
  );
}
export default function LibraryView() {
  const params = useParams<{ id?: string }>();
  return (
    <div class="library">
      <div class="library-inner">
        <Show when={params.id} keyed fallback={<LibraryHome />}>
          {(id) => <BookDetail id={id} />}
        </Show>
      </div>
    </div>
  );
}
