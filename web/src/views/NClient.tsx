import { A } from "@solidjs/router";
import { createResource, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { nclientApi, nclientImageUrl, type NClientDetail, type NClientSearch, type NClientSummary, type NClientTag } from "../api/nclient";
import { Button, Card, EmptyState, ErrorState, LoadingState, Page, PageGrid, PageHeader, SectionNav, useWide, type SectionNavItem } from "../components/page";
import { NClientReader } from "../components/NClientReader";
import { createSaves, SaveControl } from "../components/NClientSave";
import { t } from "../i18n";
import { setShowAdult, showAdult } from "../lib/adult";
import "../styles/nclient.css";

// NClient (docs/nclient-web.md): the owner's private reader for nhentai, in the Plugins page behind the
// "Show adult plugins" switch. The service (secretary-nclient) owns the child-safety filter; this page has
// no way around it. Searches, filters, gallery ids and page numbers live in memory only: the route is always
// /nclient, nothing goes to history or storage, and every image is a signed URL the service returned.

const THUMBS_STEP = 24;

function Cover(props: { url: string | undefined }) {
  return <Show when={nclientImageUrl(props.url)} fallback={<span class="nclient-thumb nclient-thumb--empty" aria-hidden="true" />}>
    {url => <img class="nclient-thumb" src={url()} alt="" loading="lazy" referrerpolicy="no-referrer" />}
  </Show>;
}

function Grid(props: { items: NClientSummary[]; open: (id: number) => void }) {
  return <div class="nclient-grid"><For each={props.items}>{item =>
    <button class="nclient-card" type="button" onClick={() => props.open(item.id)}>
      <span class="nclient-card-art">
        <Cover url={item.thumbUrl} />
        <Show when={item.favorite}><span class="nclient-badge" aria-label={t("nclient.inFavorites")}>♥</span></Show>
        <Show when={item.progress && item.pages > 0}>
          <span class="nclient-progress" aria-hidden="true"><span style={{ width: `${Math.min(100, Math.round(((item.progress!.page + 1) / item.pages) * 100))}%` }} /></span>
        </Show>
      </span>
      <span class="nclient-card-title">{item.title}</span>
      <small>{t("nclient.pages", { count: item.pages })}</small>
    </button>
  }</For></div>;
}

function EnabledNClient(props: { wide: () => boolean }) {
  const [query, setQuery] = createSignal("");
  const [sort, setSort] = createSignal<NClientSearch["sort"]>("recent");
  const [include, setInclude] = createSignal<NClientTag[]>([]);
  const [exclude, setExclude] = createSignal<NClientTag[]>([]);
  const [tagQuery, setTagQuery] = createSignal("");
  const [tagSearch, setTagSearch] = createSignal<string>();
  const [tags, { refetch: retryTags }] = createResource(tagSearch, nclientApi.tags);
  const [mode, setMode] = createSignal<"search" | "favorites">("search");
  const [search, setSearch] = createSignal<NClientSearch>({ q: "", sort: "recent", page: 1, tags: [], exclude: [] });
  const [list, { refetch }] = createResource(() => ({ mode: mode(), search: search() }), async source => {
    if (source.mode === "favorites") return { ...(await nclientApi.favorites()), page: 1, numPages: 1 };
    return nclientApi.search(source.search);
  });
  const [detail, setDetail] = createSignal<NClientDetail>();
  const [reading, setReading] = createSignal(false);
  const [startPage, setStartPage] = createSignal(0);
  const [thumbs, setThumbs] = createSignal(THUMBS_STEP);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal(false);
  const [favoriteBusy, setFavoriteBusy] = createSignal(false);
  const saves = createSaves();
  let generation = 0;
  let tagTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => { generation++; clearTimeout(tagTimer); });

  async function open(id?: number) {
    const turn = ++generation;
    setBusy(true); setError(false); setReading(false); setDetail(undefined); setThumbs(THUMBS_STEP);
    try {
      const result = await (id === undefined ? nclientApi.random() : nclientApi.detail(id));
      if (turn === generation) setDetail(result);
    } catch { if (turn === generation) setError(true); }
    finally { if (turn === generation) setBusy(false); }
  }
  function back() { generation++; setDetail(undefined); setReading(false); setBusy(false); setError(false); }
  async function toggleFavorite() {
    const gallery = detail(); if (!gallery || favoriteBusy()) return;
    setFavoriteBusy(true); setError(false);
    try {
      const result = await nclientApi.favorite(gallery.id, !gallery.favorite);
      setDetail(current => current?.id === gallery.id ? { ...current, favorite: result.favorite } : current);
      void refetch();
    } catch { setError(true); }
    finally { setFavoriteBusy(false); }
  }
  function readFrom(page: number) { setStartPage(page); setReading(true); }
  function chooseTag(tag: NClientTag, excluded: boolean) {
    setInclude(list => list.filter(x => x.id !== tag.id)); setExclude(list => list.filter(x => x.id !== tag.id));
    if (excluded) setExclude(list => [...list, tag]); else setInclude(list => [...list, tag]);
  }
  function searchNow() {
    setSearch({ q: query().trim(), sort: sort(), page: 1, tags: include().map(x => x.id), exclude: exclude().map(x => x.id) });
    setMode("search"); back();
  }
  function typeTag(value: string) {
    setTagQuery(value);
    clearTimeout(tagTimer);
    tagTimer = setTimeout(() => setTagSearch(value.trim()), 250);
  }
  const unchosen = () => (tags()?.tags ?? []).filter(tag => !include().some(x => x.id === tag.id) && !exclude().some(x => x.id === tag.id));

  const sectionNavItems = (): SectionNavItem[] => [
    { label: t("nclient.search"), onSelect: () => { setMode("search"); back(); }, current: mode() === "search" && !detail() && !reading() },
    { label: t("nclient.favorites"), onSelect: () => { setMode("favorites"); back(); void refetch(); }, current: mode() === "favorites" && !detail() && !reading() },
    { label: t("nclient.random"), onSelect: () => void open(), current: false },
  ];

  const searchFields = () => (
    <>
      <form class="nclient-search" onSubmit={e => { e.preventDefault(); searchNow(); }}>
        <label class="nclient-search-q">{t("nclient.query")}<input type="search" value={query()} onInput={e => setQuery(e.currentTarget.value)} autocomplete="off" autocapitalize="off" spellcheck={false} /></label>
        <label>{t("nclient.sort")}<select value={sort()} onChange={e => setSort(e.currentTarget.value as NClientSearch["sort"])}>
          <option value="recent">{t("nclient.recent")}</option><option value="popular">{t("nclient.popular")}</option>
          <option value="popular-week">{t("nclient.week")}</option><option value="popular-today">{t("nclient.today")}</option><option value="popular-month">{t("nclient.month")}</option>
        </select></label>
        <Show when={props.wide()} fallback={<button type="submit" class="nclient-primary">{t("nclient.search")}</button>}>
          <Button type="submit">{t("nclient.search")}</Button>
        </Show>
      </form>
      <details class="nclient-filters" onToggle={e => { if (e.currentTarget.open && tagSearch() === undefined) setTagSearch(""); }}>
        <summary>{t("nclient.filters")}<Show when={include().length + exclude().length}> · {include().length + exclude().length}</Show></summary>
        <label>{t("nclient.tags")}<input type="search" value={tagQuery()} onInput={e => typeTag(e.currentTarget.value)} autocomplete="off" autocapitalize="off" spellcheck={false} /></label>
        <Show when={tags.loading}><p role="status" class="nclient-note">{t("common.loading")}</p></Show>
        <Show when={tags.error}><p role="alert" class="nclient-note">{t("nclient.error")} <button type="button" onClick={() => void retryTags()}>{t("nclient.retry")}</button></p></Show>
        <ul class="nclient-tags">
          <For each={unchosen()}>{tag => <li>
            <span class="nclient-tag-name">{tag.name}</span>
            <button type="button" aria-label={`${t("nclient.include")} ${tag.name}`} onClick={() => chooseTag(tag, false)}>+</button>
            <button type="button" aria-label={`${t("nclient.exclude")} ${tag.name}`} onClick={() => chooseTag(tag, true)}>−</button>
          </li>}</For>
        </ul>
      </details>
      <Show when={include().length + exclude().length}>
        <div class="nclient-chips">
          <For each={include()}>{tag => <button type="button" class="nclient-chip" aria-label={`${t("nclient.remove")}: +${tag.name}`} onClick={() => setInclude(l => l.filter(x => x.id !== tag.id))}>+{tag.name} ×</button>}</For>
          <For each={exclude()}>{tag => <button type="button" class="nclient-chip nclient-chip--minus" aria-label={`${t("nclient.remove")}: −${tag.name}`} onClick={() => setExclude(l => l.filter(x => x.id !== tag.id))}>−{tag.name} ×</button>}</For>
        </div>
      </Show>
    </>
  );

  const searchForm = (desk: boolean) => (
    <Show when={mode() === "search"}>
      <Show when={desk} fallback={searchFields()}>
        <Card title={t("nclient.search")} class="nclient-desk-search">{searchFields()}</Card>
      </Show>
    </Show>
  );

  const listBody = (desk: boolean) => (
    <Show when={detail()} fallback={<Show when={!busy()}>
      {!desk && searchForm(false)}
      <Show when={desk}>{searchForm(true)}</Show>
      <Show when={list.loading}>
        <Show when={desk} fallback={<p role="status" class="nclient-note">{t("common.loading")}</p>}>
          <LoadingState label={t("common.loading")} />
        </Show>
      </Show>
      <Show when={list.error}>
        <Show when={desk} fallback={<p role="alert" class="nclient-note">{t("nclient.error")} <button type="button" onClick={() => void refetch()}>{t("nclient.retry")}</button></p>}>
          <ErrorState message={t("nclient.error")} retryLabel={t("nclient.retry")} onRetry={() => void refetch()} />
        </Show>
      </Show>
      <Show when={!list.loading && !list.error}>
        <Show when={list()?.items.length} fallback={
          <Show when={desk} fallback={<p class="nclient-note">{t("nclient.empty")}</p>}>
            <EmptyState text={t("nclient.empty")} />
          </Show>
        }>
          <Grid items={list()?.items ?? []} open={id => void open(id)} />
        </Show>
        <Show when={mode() === "search" && (list()?.numPages ?? 0) > 1}>
          <Show when={desk} fallback={
            <div class="nclient-bar nclient-bar--nav">
              <button type="button" disabled={search().page <= 1} onClick={() => setSearch(s => ({ ...s, page: s.page - 1 }))}>{t("nclient.previous")}</button>
              <span>{t("nclient.page", { page: list()?.page ?? 1, pages: list()?.numPages ?? 1 })}</span>
              <button type="button" disabled={search().page >= (list()?.numPages ?? 1)} onClick={() => setSearch(s => ({ ...s, page: s.page + 1 }))}>{t("nclient.next")}</button>
            </div>
          }>
            <div class="nclient-desk-pager">
              <Button disabled={search().page <= 1} onClick={() => setSearch(s => ({ ...s, page: s.page - 1 }))}>{t("nclient.previous")}</Button>
              <span class="nclient-bar-page">{t("nclient.page", { page: list()?.page ?? 1, pages: list()?.numPages ?? 1 })}</span>
              <Button disabled={search().page >= (list()?.numPages ?? 1)} onClick={() => setSearch(s => ({ ...s, page: s.page + 1 }))}>{t("nclient.next")}</Button>
            </div>
          </Show>
        </Show>
      </Show>
    </Show>}>
      {gallery => <Show when={reading()} fallback={<>
        <Show when={desk} fallback={<button type="button" class="nclient-back" onClick={back}>‹ {t("nclient.back")}</button>}>
          <button type="button" class="pg-link pg-back" onClick={back}>{t("nclient.back")}</button>
          <h2 class="trd-detail-title">{gallery().title}</h2>
        </Show>
        <div class="nclient-detail">
          <Show when={!desk}>
            <Show when={nclientImageUrl(gallery().coverUrl)}>{url => <img class="nclient-cover" src={url()} alt="" referrerpolicy="no-referrer" />}</Show>
          </Show>
          <div class="nclient-detail-main">
            <Show when={!desk}><h2>{gallery().title}</h2></Show>
            <p class="nclient-meta">{t("nclient.pages", { count: gallery().pages.length })}{gallery().language ? ` · ${gallery().language}` : ""}</p>
            <Show when={gallery().tags.length}>
              <ul class="nclient-taglist" aria-label={t("nclient.tags")}><For each={gallery().tags}>{tag => <li>{tag.name}</li>}</For></ul>
            </Show>
            <div class="nclient-bar">
              <Show when={desk} fallback={<button type="button" class="nclient-primary" disabled={!gallery().pages.length} onClick={() => readFrom(gallery().progress ? gallery().progress!.page : 0)}>{gallery().progress ? t("nclient.resume") : t("nclient.read")}</button>}>
                <Button disabled={!gallery().pages.length} onClick={() => readFrom(gallery().progress ? gallery().progress!.page : 0)}>{gallery().progress ? t("nclient.resume") : t("nclient.read")}</Button>
              </Show>
              <Show when={desk} fallback={<button type="button" disabled={favoriteBusy()} aria-pressed={gallery().favorite} onClick={() => void toggleFavorite()}>{gallery().favorite ? t("nclient.unfavorite") : t("nclient.favorite")}</button>}>
                <Button disabled={favoriteBusy()} onClick={() => void toggleFavorite()}>{gallery().favorite ? t("nclient.unfavorite") : t("nclient.favorite")}</Button>
              </Show>
            </div>
            <SaveControl saves={saves} id={gallery().id} />
          </div>
          <Show when={desk && nclientImageUrl(gallery().coverUrl)}>{url => <img class="nclient-cover" src={url()} alt="" referrerpolicy="no-referrer" />}</Show>
        </div>
        <Show when={gallery().pages.length}>
          <h3>{t("nclient.pagesHeading")}</h3>
          <div class="nclient-pages">
            <For each={gallery().pages.slice(0, thumbs())}>{(page, index) =>
              <button type="button" class="nclient-pagethumb" aria-label={t("nclient.openPage", { page: index() + 1 })} onClick={() => readFrom(index())}>
                <Cover url={page.thumbUrl} /><small>{index() + 1}</small>
              </button>
            }</For>
          </div>
          <Show when={thumbs() < gallery().pages.length}>
            <Show when={desk} fallback={<button type="button" class="nclient-more" onClick={() => setThumbs(n => n + THUMBS_STEP)}>{t("nclient.morePages", { count: gallery().pages.length - thumbs() })}</button>}>
              <Button onClick={() => setThumbs(n => n + THUMBS_STEP)}>{t("nclient.morePages", { count: gallery().pages.length - thumbs() })}</Button>
            </Show>
          </Show>
        </Show>
        <Show when={gallery().related.length}>
          <h3>{t("nclient.related")}</h3><Grid items={gallery().related} open={id => void open(id)} />
        </Show>
      </>}>
        <NClientReader gallery={gallery()} startPage={startPage()} close={() => { setReading(false); void refetch(); }} refresh={() => void open(gallery().id)}
          onPage={page => setDetail(d => d ? { ...d, progress: { page, pages: d.pages.length, updated_at: Math.floor(Date.now() / 1000) } } : d)}
          favorite={gallery().favorite} favoriteBusy={favoriteBusy()} toggleFavorite={() => void toggleFavorite()}
          saveControl={<SaveControl saves={saves} id={gallery().id} compact />} />
      </Show>}
    </Show>
  );

  const core = () => (
    <>
      <Show when={!reading() && !props.wide()}>
        <nav class="nclient-bar" aria-label={t("nclient.title")}>
          <button type="button" aria-pressed={mode() === "search" && !detail()} onClick={() => { setMode("search"); back(); }}>{t("nclient.search")}</button>
          <button type="button" aria-pressed={mode() === "favorites" && !detail()} onClick={() => { setMode("favorites"); back(); void refetch(); }}>{t("nclient.favorites")}</button>
          <button type="button" disabled={busy()} onClick={() => void open()}>{t("nclient.random")}</button>
        </nav>
      </Show>
      <Show when={busy()}>
        <Show when={props.wide()} fallback={<p role="status" class="nclient-note">{t("common.loading")}</p>}>
          <LoadingState label={t("common.loading")} />
        </Show>
      </Show>
      <Show when={error()}>
        <Show when={props.wide()} fallback={<p role="alert" class="nclient-note">{t("nclient.error")} <button type="button" onClick={back}>{t("nclient.back")}</button></p>}>
          <ErrorState message={t("nclient.error")} retryLabel={t("nclient.back")} onRetry={back} />
        </Show>
      </Show>
      {listBody(props.wide())}
    </>
  );

  return (
    <Show when={props.wide()} fallback={core()}>
      <>
        <PageHeader title={t("nclient.title")} nav={<SectionNav label={t("nclient.title")} items={sectionNavItems()} />} />
        <PageGrid layout="full">
          <Card class="nclient-desk-main">{core()}</Card>
        </PageGrid>
      </>
    </Show>
  );
}

function AdultShell(props: { wide: () => boolean; children: JSX.Element }) {
  return (
    <Show when={showAdult()} fallback={
      <Show when={props.wide()} fallback={
        <div class="nclient-note">
          <p>{t("nclient.hidden")}</p>
          <button type="button" onClick={() => setShowAdult(true)}>{t("nclient.showAdult")}</button>
        </div>
      }>
        <EmptyState text={t("nclient.hidden")} action={<Button onClick={() => setShowAdult(true)}>{t("nclient.showAdult")}</Button>} />
      </Show>
    }>
      {props.children}
    </Show>
  );
}

export default function NClientView() {
  const { wide, track } = useWide();
  const [status, { refetch }] = createResource(() => showAdult() || undefined, () => nclientApi.status());
  onMount(() => {
    const previous = document.title;
    document.title = "NClient";
    onCleanup(() => { document.title = previous; });
  });

  const statusBody = () => (
    <>
      <Show when={status.loading}>
        <Show when={wide()} fallback={<p role="status" class="nclient-note">{t("common.loading")}</p>}>
          <LoadingState label={t("common.loading")} />
        </Show>
      </Show>
      <Show when={status.error}>
        <Show when={wide()} fallback={<p role="alert" class="nclient-note">{t("nclient.error")} <button type="button" onClick={() => void refetch()}>{t("nclient.retry")}</button></p>}>
          <ErrorState message={t("nclient.error")} retryLabel={t("nclient.retry")} onRetry={() => void refetch()} />
        </Show>
      </Show>
      <Show when={!status.loading && !status.error && status() && !status()!.enabled}>
        <Show when={wide()} fallback={<p class="nclient-note">{t("nclient.unavailable")}</p>}>
          <EmptyState text={t("nclient.unavailable")} />
        </Show>
      </Show>
      <Show when={!status.error && status()?.enabled}>
        <EnabledNClient wide={wide} />
      </Show>
    </>
  );

  return (
    <section class="nclient" classList={{ "nclient--wide": wide() }} ref={track} aria-labelledby={wide() ? undefined : "nclient-title"}>
      <Show when={wide()} fallback={
        <>
          <A href="/plugins" class="media-back">{t("media.back")}</A>
          <h1 id="nclient-title">{t("nclient.title")}</h1>
          <AdultShell wide={wide}>{statusBody()}</AdultShell>
        </>
      }>
        <Page class="nclient-desk">
          <Show when={!(showAdult() && !status.loading && !status.error && status()?.enabled)}>
            <PageHeader title={t("nclient.title")} />
          </Show>
          <AdultShell wide={wide}>{statusBody()}</AdultShell>
        </Page>
      </Show>
    </section>
  );
}
