import { A, useParams } from "@solidjs/router";
import {
  createEffect,
  createResource,
  createSignal,
  For,
  onCleanup,
  Show,
  type JSX,
} from "solid-js";
import { ApiError } from "../api/client";
import {
  libraryApi,
  type BookPatch,
  type Highlight,
  type Progress,
  type ReadingStatus,
} from "../api/library";
import { parseTags } from "../api/notes";
import { BackIcon, SearchIcon } from "../components/Icons";
import {
  Page,
  PageHeader,
  PageGrid,
  Card,
  RowList,
  ListRow,
  EmptyState,
  LoadingState,
  ErrorState,
  useWide,
} from "../components/page";
import { locale, t } from "../i18n";

const statuses: ReadingStatus[] = [
  "reading",
  "completed",
  "abandoned",
  "not_started",
  "unknown",
];
const statusLabel = (status: ReadingStatus) =>
  t(`library.${statuses.includes(status) ? status : "unknown"}`);
const percent = (p: Progress | null) =>
  p?.percent == null
    ? "—"
    : `${new Intl.NumberFormat(locale(), { maximumFractionDigits: 1 }).format(p.percent)}%`;
function date(value: string) {
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? value : d.toLocaleString(locale());
}
function LibrarySection(props: {
  wide?: boolean;
  title?: string;
  class?: string;
  list?: boolean;
  label?: string;
  children: JSX.Element;
}) {
  return (
    <Show
      when={props.wide}
      fallback={
        <section class={props.class ?? "library-section"} aria-label={props.label}>
          <Show when={props.title}>
            <h2>{props.title}</h2>
          </Show>
          {props.children}
        </section>
      }
    >
      <Card title={props.title} list={props.list}>
        {props.children}
      </Card>
    </Show>
  );
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
function HighlightCard(props: { highlight: Highlight; wide?: boolean }) {
  const h = () => props.highlight;
  return (
    <Show when={props.wide} fallback={phone()}>
      <ListRow class="library-highlight-row"
        title={<span id={`highlight-${h().id}`}>{h().text}</span>}
        sub={
          <>
            {h().note}
            <span class="pg-muted">
              {" "}
              ·{" "}
              <Show when={h().location}>
                {(loc) =>
                  t("library.location", {
                    value:
                      loc().start === loc().end
                        ? String(loc().start)
                        : `${loc().start}–${loc().end}`,
                  })
                }
              </Show>
              <Show when={h().page}>
                {(page) => ` · ${t("library.page", { value: page() })}`}
              </Show>{" "}
              · {h().addedAtRaw || h().addedAt || t("library.unknownDate")} ·{" "}
              {t("library.source")}: {h().source}
            </span>
          </>
        }
      />
    </Show>
  );
  function phone() {
    return (
      <li
        class={props.wide ? "pg-row" : "library-highlight"}
        id={`highlight-${h().id}`}
      >
        <p class="library-text">{h().text}</p>
        <Show when={h().note}>
          {(note) => <p class="library-text">{note()}</p>}
        </Show>
        <div class="library-meta muted small">
          <Show when={h().location}>
            {(loc) => (
              <span>
                {t("library.location", {
                  value:
                    loc().start === loc().end
                      ? String(loc().start)
                      : `${loc().start}–${loc().end}`,
                })}
              </span>
            )}
          </Show>
          <Show when={h().page}>
            {(page) => <span>{t("library.page", { value: page() })}</span>}
          </Show>
          {/* Keep local Kindle dates as supplied: converting to Date would invent a timezone. */}
          <span>
            {h().addedAtRaw || h().addedAt || t("library.unknownDate")}
          </span>
          <span>
            {t("library.source")}: {h().source}
          </span>
        </div>
      </li>
    );
  }
}
function Activity(props: {
  wide?: boolean;
  retryVersion?: number;
  onError?: (error: boolean) => void;
}) {
  const [data, { refetch }] = createResource(
    () => props.retryVersion ?? 0,
    async () => {
      const [stats, sync, week, month] = await Promise.all([
        libraryApi.stats(),
        libraryApi.sync(),
        libraryApi.recent(7),
        libraryApi.recent(30),
      ]);
      return { stats, sync, week, month };
    },
  );
  createEffect(() => props.onError?.(Boolean(data.error)));
  return (
    <LibrarySection wide={props.wide} title={t("library.activity")} label={t("library.activity")}>
      <Show when={data.error && !props.wide}>
        <ErrorNotice retry={() => void refetch()} />
      </Show>
      <Show when={data.loading}>
        <Show
          when={props.wide}
          fallback={
            <p role="status" class="muted">
              {t("common.loading")}
            </p>
          }
        >
          <LoadingState label={t("common.loading")} />
        </Show>
      </Show>
      <Show when={!data.error && data()}>
        {(d) => (
          <>
            <p>
              {t("library.lastImport")}:{" "}
              {d().sync.imports.length
                ? date(d().sync.imports[d().sync.imports.length - 1]!.time)
                : t("library.noImport")}
            </p>
            <dl class={props.wide ? "pg-stats" : "library-counts"}>
              <For each={statuses}>
                {(s) => (
                  <div class={props.wide ? "pg-stat" : undefined}>
                    <dt>{statusLabel(s)}</dt>
                    <dd>{d().stats.byStatus[s] ?? 0}</dd>
                  </div>
                )}
              </For>
            </dl>
            <p>
              {t("library.recent", {
                days: 7,
                count: d().week.highlights.length,
              })}
            </p>
            <p>
              {t("library.recent", {
                days: 30,
                count: d().month.highlights.length,
              })}
            </p>
            <Show
              when={
                d().week.highlights.length >= 500 ||
                d().month.highlights.length >= 500
              }
            >
              <p class="muted small">{t("library.recentLimit")}</p>
            </Show>
            <p class="muted small">{t("library.dates")}</p>
            <Show
              when={
                d().week.readingSessions === "unavailable_on_stock_kindle" ||
                d().month.readingSessions === "unavailable_on_stock_kindle"
              }
            >
              <p class="muted">{t("library.noSessions")}</p>
            </Show>
          </>
        )}
      </Show>
    </LibrarySection>
  );
}
function LibraryHome(props: { wide: boolean }) {
  const [activityError, setActivityError] = createSignal(false);
  const [retryVersion, setRetryVersion] = createSignal(0);
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
    () => ({
      q: search(),
      status: status() || undefined,
      onKindle: onKindle() || undefined,
    }),
    async (filters) => {
      const [books, hits] = await Promise.all([
        libraryApi.list({ ...filters, limit: 500 }),
        filters.q ? libraryApi.search(filters.q, 500) : Promise.resolve([]),
      ]);
      return {
        books: books.filter((b) => b.kind !== "dictionary"),
        hits,
        capped: books.length >= 500,
      };
    },
  );
  const filters = () => (
    <>
      {" "}
      <label class={props.wide ? "pg-field" : "notes-search"}>
        <SearchIcon />
        <span class="sr-only">{t("library.search")}</span>
        <input
          class={props.wide ? "pg-input" : "notes-search-input"}
          type="search"
          maxlength={200}
          value={query()}
          placeholder={t("library.search")}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
      </label>
      <div
        class={props.wide ? "library-filter-row" : "chip-group library-filters"}
        role="group"
        aria-label={t("library.status")}
      >
        <For
          each={["", "reading", "completed", "not_started", "unknown"] as const}
        >
          {(s) => (
            <button
              class={props.wide ? "pg-btn" : "chip"}
              classList={{ "chip--active": !props.wide && status() === s }}
              aria-pressed={status() === s}
              onClick={() => setStatus(s)}
            >
              {s ? statusLabel(s) : t("library.all")}
            </button>
          )}
        </For>
        <button
          class={props.wide ? "pg-btn" : "chip"}
          classList={{ "chip--active": !props.wide && onKindle() }}
          aria-pressed={onKindle()}
          onClick={() => setOnKindle(!onKindle())}
        >
          {t("library.onKindle")}
        </button>
      </div>
    </>
  );
  return (
    <Show when={props.wide} fallback={phone()}>
      <PageGrid
        layout="rail"
        rail={
          <Activity
            wide
            retryVersion={retryVersion()}
            onError={setActivityError}
          />
        }
      >
        {filters()}
        <Show when={data.error || activityError()}>
          <ErrorState
            message={t("library.error")}
            retryLabel={t("common.tryAgain")}
            onRetry={() => {
              void refetch();
              setRetryVersion((n) => n + 1);
            }}
          />
        </Show>
        <Show when={data.loading}>
          <LoadingState label={t("common.loading")} />
        </Show>
        <Show when={!data.error && data()}>
          {(d) => (
            <Card list title={t("library.title")}>
              <Show
                when={d().books.length}
                fallback={
                  <EmptyState
                    title={
                      search() || status() || onKindle()
                        ? undefined
                        : t("library.empty")
                    }
                    text={
                      search() || status() || onKindle()
                        ? t("library.noResults")
                        : t("library.emptySub")
                    }
                  />
                }
              >
                <RowList>
                  <For each={d().books}>
                    {(b) => (
                      <ListRow
                        title={b.title}
                        class="library-book-row"
                        href={`/library/${encodeURIComponent(b.id)}`}
                        leading={
                          <span class="library-cover" aria-hidden="true">
                            {b.title.slice(0, 1)}
                          </span>
                        }
                        sub={
                          <>
                            {b.authors.join(", ")} · {statusLabel(b.status)} ·{" "}
                            {t("library.highlightCount", {
                              count: b.highlightCount,
                            })}
                            <Show when={b.onKindle}>
                              {" "}
                              · {t("library.onKindle")}
                            </Show>
                            <Show when={b.drm === "protected"}>
                              {" "}
                              · {t("library.protected")}
                            </Show>
                            <For
                              each={d().hits.filter(
                                (hit) => hit.bookId === b.id && hit.highlightId,
                              )}
                            >
                              {(hit) => (
                                <a
                                  class="pg-link"
                                  href={`/library/${encodeURIComponent(b.id)}#highlight-${encodeURIComponent(hit.highlightId!)}`}
                                >
                                  {hit.snippet}
                                </a>
                              )}
                            </For>
                          </>
                        }
                        meta={`${t("library.progress")}: ${percent(b.progress)}`}
                      />
                    )}
                  </For>
                </RowList>
              </Show>
              <Show
                when={
                  !d().books.length && !search() && !status() && !onKindle()
                }
              >
                <code class="library-command">
                  lucina-ink import /Volumes/Kindle --server … --token-file …
                </code>
              </Show>
              <Show when={d().capped}>
                <p class="pg-muted">{t("library.listLimit")}</p>
              </Show>
            </Card>
          )}
        </Show>
      </PageGrid>
    </Show>
  );
  function phone() {
    return (
      <>
        <h1 class="view-title">{t("library.title")}</h1>
        {filters()}
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
                        <code class="library-command">
                          lucina-ink import /Volumes/Kindle --server …
                          --token-file …
                        </code>
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
                        <A
                          href={`/library/${encodeURIComponent(b.id)}`}
                          class="library-book-link"
                        >
                          <h2>{b.title}</h2>
                          <p class="muted">{b.authors.join(", ")}</p>
                        </A>
                        <div class="library-meta small">
                          <span class="chip">{statusLabel(b.status)}</span>
                          <span>
                            {t("library.progress")}: {percent(b.progress)}
                          </span>
                          <span>
                            {t("library.highlightCount", {
                              count: b.highlightCount,
                            })}
                          </span>
                          <Show when={b.onKindle}>
                            <span>{t("library.onKindle")}</span>
                          </Show>
                          <Show when={b.drm === "protected"}>
                            <span class="muted">{t("library.protected")}</span>
                          </Show>
                        </div>
                        <For
                          each={d().hits.filter(
                            (hit) => hit.bookId === b.id && hit.highlightId,
                          )}
                        >
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
}

function BookDetail(props: { id: string; wide: boolean }) {
  const [data, { refetch, mutate }] = createResource(
    () => props.id,
    libraryApi.get,
  );
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
        old
          ? {
              ...old,
              book: {
                ...old.book,
                briefs: [
                  ...old.book.briefs.filter((b) => b.id !== brief.id),
                  brief,
                ],
              },
            }
          : old,
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
      <A
        href="/library"
        class={props.wide ? "pg-link pg-back" : "library-back"}
      >
        <BackIcon />
        {t("library.back")}
      </A>
      <Show when={data.error}>
        <Show
          when={props.wide}
          fallback={<ErrorNotice retry={() => void refetch()} />}
        >
          <ErrorState
            message={t("library.error")}
            retryLabel={t("common.tryAgain")}
            onRetry={() => void refetch()}
          />
        </Show>
      </Show>
      <Show when={data.loading}>
        <Show
          when={props.wide}
          fallback={<p role="status">{t("common.loading")}</p>}
        >
          <LoadingState label={t("common.loading")} />
        </Show>
      </Show>
      <Show when={message()}>
        {(m) => (
          <p class={props.wide ? "pg-notice" : "notice"} role="alert">
            {m()}
          </p>
        )}
      </Show>
      <Show when={!data.error && data()}>
        {(d) => {
          const facts = () => (
            <LibrarySection wide={props.wide} title={t("library.metadata")}>
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
              <h3 class={props.wide ? "pg-card-title" : undefined}>{t("library.presence")}</h3>
              <div class="library-meta">
                <Show when={d().book.owned}>
                  <span>{t("library.owned")}</span>
                </Show>
                <Show when={d().book.onKindle}>
                  <span>{t("library.onKindle")}</span>
                </Show>
                <Show when={d().book.removedFromKindleAt}>
                  {(at) => (
                    <span>{t("library.removed", { date: date(at()) })}</span>
                  )}
                </Show>
                <Show
                  when={
                    !d().book.owned &&
                    !d().book.onKindle &&
                    d().highlights.length
                  }
                >
                  <span>{t("library.clippingsOnly")}</span>
                </Show>
                <Show
                  when={
                    !d().book.owned &&
                    !d().book.onKindle &&
                    !d().highlights.length &&
                    !d().book.removedFromKindleAt
                  }
                >
                  <span>{t("library.unknown")}</span>
                </Show>
              </div>
            </LibrarySection>
          );
          const reading = () => (
            <LibrarySection
              wide={props.wide}
              class="library-section library-controls"
              title={props.wide ? t("library.status") : undefined}
            >
              <label for="library-status">{t("library.status")}</label>
              <select
                id="library-status"
                class={props.wide ? "pg-input" : "text-input"}
                disabled={busy()}
                value={d().book.status}
                onChange={(e) =>
                  void save({ status: e.currentTarget.value as ReadingStatus })
                }
              >
                <For each={statuses}>
                  {(s) => <option value={s}>{statusLabel(s)}</option>}
                </For>
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
                  class={props.wide ? "pg-input" : "text-input"}
                  value={tags()}
                  disabled={busy()}
                  onInput={(e) => setTags(e.currentTarget.value)}
                />
                <button
                  class={props.wide ? "pg-btn" : "btn btn--small"}
                  disabled={busy()}
                >
                  {t("library.saveTags")}
                </button>
              </form>
              <h2 class={props.wide ? "pg-card-title" : undefined}>
                {t("library.progress")}
              </h2>
              <p>
                {d().book.progress?.percent == null
                  ? t("library.unknownProgress")
                  : percent(d().book.progress)}
              </p>
              <Show when={d().book.progress?.location != null}>
                <p>
                  {t("library.location", {
                    value: String(d().book.progress!.location),
                  })}
                </p>
              </Show>
              <Show when={d().book.progress?.page != null}>
                <p>
                  {t("library.page", {
                    value: String(d().book.progress!.page),
                  })}
                </p>
              </Show>
            </LibrarySection>
          );
          const briefs = () => (
            <LibrarySection wide={props.wide} title={t("library.brief")}>
              <button
                class={props.wide ? "pg-btn" : "btn btn--primary btn--small"}
                disabled={busy() || !d().highlights.length}
                onClick={() => void makeBrief()}
              >
                {t("library.makeBrief")}
              </button>
              <For each={d().book.briefs}>
                {(brief) => (
                  <article
                    class={props.wide ? "pg-row-main" : "library-highlight"}
                  >
                    <h3 class={props.wide ? "pg-card-title" : "muted small"}>{t("library.generated")}</h3>
                    <p class="library-text">{brief.text}</p>
                    <p class="muted small">{date(brief.generatedAt)}</p>
                  </article>
                )}
              </For>
            </LibrarySection>
          );
          const highlights = () => (
            <LibrarySection
              wide={props.wide}
              title={t("library.highlights")}
              list={props.wide}
            >
              <Show
                when={d().highlights.length}
                fallback={
                  <Show
                    when={props.wide}
                    fallback={<p class="muted">{t("library.noHighlights")}</p>}
                  >
                    <EmptyState text={t("library.noHighlights")} />
                  </Show>
                }
              >
                <ul class={props.wide ? "pg-rows" : "note-list"}>
                  <For each={d().highlights}>
                    {(h) => <HighlightCard highlight={h} wide={props.wide} />}
                  </For>
                </ul>
              </Show>
            </LibrarySection>
          );
          const related = () => (
            <LibrarySection
              wide={props.wide}
              title={t("library.related")}
              list={props.wide}
            >
              <Show
                when={d().book.relatedNotes.length}
                fallback={
                  <Show
                    when={props.wide}
                    fallback={<p class="muted">{t("library.unconnected")}</p>}
                  >
                    <EmptyState text={t("library.unconnected")} />
                  </Show>
                }
              >
                <ul class={props.wide ? "pg-rows" : undefined}>
                  <For each={d().book.relatedNotes}>
                    {(note) => (
                      <Show when={props.wide} fallback={<li>
                        {note.title}{" "}
                        <span class="muted small">
                          {note.vault}: {note.path}
                        </span>
                      </li>}><ListRow title={note.title} sub={`${note.vault}: ${note.path}`} /></Show>
                    )}
                  </For>
                </ul>
              </Show>
            </LibrarySection>
          );
          return (
            <>
              <Show
                when={props.wide}
                fallback={<h1 class="view-title">{d().book.title}</h1>}
              >
                <h2 class="library-detail-title">{d().book.title}</h2>
              </Show>
              <p class={props.wide ? "pg-muted" : "muted"}>
                {d().book.authors.join(", ")}
              </p>
              <Show
                when={props.wide}
                fallback={
                  <>
                    {facts()}
                    {reading()}
                    {briefs()}
                    {highlights()}
                    {related()}
                  </>
                }
              >
                <PageGrid
                  layout="rail"
                  rail={
                    <>
                      {facts()}
                      {reading()}
                      {related()}
                    </>
                  }
                >
                  {highlights()}
                  {briefs()}
                </PageGrid>
              </Show>
            </>
          );
        }}
      </Show>
    </>
  );
}
export default function LibraryView() {
  const { wide, track } = useWide();
  const params = useParams<{ id?: string }>();
  return (
    <div class="library" ref={track}>
      <Show
        when={wide()}
        fallback={<div class="library-inner">{content()}</div>}
      >
        <Page class="library-desk">
          <PageHeader title={t("library.title")} />
          {content()}
        </Page>
      </Show>
    </div>
  );
  function content() {
    return (
      <>
        <Show when={params.id} keyed fallback={<LibraryHome wide={wide()} />}>
          {(id) => <BookDetail id={id} wide={wide()} />}
        </Show>
      </>
    );
  }
}
