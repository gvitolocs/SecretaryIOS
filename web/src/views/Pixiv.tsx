import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { pixivApi, RANKING_MODES, RANKING_MODES_R18, type PixivContent, type PixivItem, type RankingMode, type SearchOrder, type SearchType } from "../api/pixiv";
import { BackIcon, BookmarkIcon, SearchIcon } from "../components/Icons";
import { Button, Card as PageCard, EmptyState, ErrorState, Page, PageGrid, PageHeader, SectionNav, useWide, type SectionNavItem } from "../components/page";
import { Reader } from "../components/pixiv/Reader";
import { t } from "../i18n";
import { showAdult } from "../lib/adult";
import { createFeed, type Feed } from "../lib/pixivFeed";
import "../styles/pixiv.css";

// Pixiv (/pixiv, a Plugins card; owner, 2026-10-28): the ranking, a search and the owner's bookmarks, with a
// reader for multi-page works and "Save to Lucina". The service is crates/secretary-pixiv (docs/pixiv.md).
// One Content filter (all ages, R-18, all) serves every tab: the search's mode, the ranking modes on offer and
// the bookmarks' filter. Its R-18 choices exist only while "Show adult plugins" is on; the service refuses them
// without `adult=1` in any case, and says (a notice) when its Pixiv session can't show R-18 at all.
// `/pixiv/<id>` opens a work over the list.

type Tab = "ranking" | "search" | "bookmarks";

const MODE_LABEL: Record<RankingMode, () => string> = {
  day: () => t("pixiv.modeDay"),
  week: () => t("pixiv.modeWeek"),
  month: () => t("pixiv.modeMonth"),
  day_male: () => t("pixiv.modeDayMale"),
  day_female: () => t("pixiv.modeDayFemale"),
  original: () => t("pixiv.modeOriginal"),
  rookie: () => t("pixiv.modeRookie"),
  day_r18: () => t("pixiv.modeDayR18"),
  week_r18: () => t("pixiv.modeWeekR18"),
  day_male_r18: () => t("pixiv.modeDayMaleR18"),
  day_female_r18: () => t("pixiv.modeDayFemaleR18"),
};

/** The notices the service may attach to a listing; any other value is ignored. */
const NOTICE: Record<string, () => string> = {
  r18_needs_session: () => t("pixiv.noticeNeedsSession"),
  r18_disabled_in_account: () => t("pixiv.noticeDisabled"),
  all_ages_only: () => t("pixiv.noticeAllAgesOnly"),
};

/** What a search's popular order turned out to be (`sort.used`); date orders say nothing. */
const SORT_NOTE: Record<string, () => string> = {
  pixiv_popular: () => t("pixiv.sortPopular"),
  users_tags: () => t("pixiv.sortUsersTags"),
  date_d_fallback: () => t("pixiv.sortFallback"),
};

/** 1000 → "1000+", 10000 → "10k+", 100000 → "100k+". */
export function tierLabel(n: number): string {
  return n >= 10_000 ? `${Math.floor(n / 1000)}k+` : `${n}+`;
}

function Card(props: { item: PixivItem; onOpen: (item: PixivItem) => void }) {
  return (
    <button type="button" class="px-card" data-work={props.item.id} aria-label={t("pixiv.openWork", { title: props.item.title, author: props.item.author })} onClick={() => props.onOpen(props.item)}>
      <span class="px-card__thumb">
        <Show when={props.item.thumbUrl}>
          <img src={props.item.thumbUrl} alt="" loading="lazy" decoding="async" draggable={false} />
        </Show>
        <Show when={props.item.pages > 1}>
          <span class="px-badge px-badge--pages">{props.item.pages}</span>
        </Show>
        <Show when={props.item.r18}>
          <span class="px-badge px-badge--r18">{t("pixiv.r18")}</span>
        </Show>
        <Show when={props.item.popularTier}>
          {(tier) => (
            <span class="px-badge px-badge--tier" role="img" aria-label={t("pixiv.tier", { count: tierLabel(tier()) })}>
              ♥ {tierLabel(tier())}
            </span>
          )}
        </Show>
        <Show when={props.item.bookmarked}>
          <span class="px-badge px-badge--mark" role="img" aria-label={t("pixiv.bookmarked")}>
            <BookmarkIcon filled />
          </span>
        </Show>
      </span>
      <span class="px-card__title">{props.item.title}</span>
      <span class="px-card__author">{props.item.author}</span>
    </button>
  );
}

/** The grid of works with its footer: loading, errors, "load more" and the end-of-list note. */
function FeedGrid(props: { feed: Feed; empty: string; onOpen: (item: PixivItem) => void; infinite?: boolean; wide?: boolean }) {
  let sentinel: HTMLDivElement | undefined;
  onMount(() => {
    if (typeof IntersectionObserver === "undefined" || !sentinel || props.infinite === false) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && props.feed.hasMore() && !props.feed.loading()) void props.feed.more();
      },
      { rootMargin: "600px 0px" },
    );
    io.observe(sentinel);
    onCleanup(() => io.disconnect());
  });
  // A page that does not fill the screen would never scroll to the sentinel: look again after each load.
  createEffect(
    on(
      () => props.feed.items().length,
      () => {
        if (!sentinel || typeof IntersectionObserver === "undefined") return;
        queueMicrotask(() => {
          const r = sentinel?.getBoundingClientRect();
          if (r && r.top < window.innerHeight + 600 && props.feed.hasMore() && !props.feed.loading()) void props.feed.more();
        });
      },
      { defer: true },
    ),
  );
  const notice = () => {
    const n = props.feed.notice();
    return n ? NOTICE[n]?.() ?? "" : "";
  };
  return (
    <>
      <Show when={notice()}>
        <p class="px-notice" role="status">{notice()}</p>
      </Show>
      <div class="px-grid" role="list" aria-busy={props.feed.loading()}>
        <For each={props.feed.items()}>{(item) => <div role="listitem" class="px-grid__cell"><Card item={item} onOpen={props.onOpen} /></div>}</For>
      </div>
      <div ref={sentinel} class="px-foot" aria-live="polite">
        <Show when={props.feed.error()}>
          <Show
            when={props.wide}
            fallback={
              <>
                <p class="px-note px-note--error" role="alert">{props.feed.error()}</p>
                <button type="button" class="px-btn" onClick={() => void props.feed.retry()}>{t("pixiv.retry")}</button>
              </>
            }
          >
            <ErrorState message={props.feed.error()!} retryLabel={t("pixiv.retry")} onRetry={() => void props.feed.retry()} />
          </Show>
        </Show>
        <Show when={props.feed.loading()}>
          <p class="px-note" role="status">{t("pixiv.loading")}</p>
        </Show>
        <Show when={!props.feed.loading() && !props.feed.error() && props.feed.hasMore()}>
          <Show when={props.wide} fallback={<button type="button" class="px-btn" onClick={() => void props.feed.more()}>{t("pixiv.loadMore")}</button>}>
            <Button onClick={() => void props.feed.more()}>{t("pixiv.loadMore")}</Button>
          </Show>
        </Show>
        <Show when={!props.feed.loading() && !props.feed.error() && props.feed.started() && props.feed.items().length === 0 && !notice()}>
          <Show when={props.wide} fallback={<p class="px-note">{props.empty}</p>}>
            <EmptyState text={props.empty} />
          </Show>
        </Show>
        <Show when={!props.feed.loading() && !props.feed.error() && !props.feed.hasMore() && props.feed.items().length > 0}>
          <p class="px-note px-note--end">{t("pixiv.end")}</p>
        </Show>
      </div>
    </>
  );
}

export default function PixivView() {
  const params = useParams<{ id?: string }>();
  const navigate = useNavigate();
  const [tab, setTab] = createSignal<Tab>("ranking");
  const [mode, setMode] = createSignal<RankingMode>("day");
  const [draft, setDraft] = createSignal("");
  const [query, setQuery] = createSignal("");
  const [order, setOrder] = createSignal<SearchOrder>("date_d");
  const [kind, setKind] = createSignal<SearchType>("all");
  const [content, setContent] = createSignal<PixivContent>("safe");
  const ranking = createFeed();
  const search = createFeed();
  const bookmarks = createFeed();
  /** The work opened from a card, so the reader has something to show before the work arrives. */
  const [preview, setPreview] = createSignal<PixivItem>();

  /** The content filter in force: R-18 choices vanish with the switch (read here, so no list asks for them first). */
  const contentNow = createMemo<PixivContent>(() => (showAdult() ? content() : "safe"));
  const modes = createMemo<RankingMode[]>(() => {
    const c = contentNow();
    return c === "safe" ? [...RANKING_MODES] : c === "r18" ? [...RANKING_MODES_R18] : [...RANKING_MODES, ...RANKING_MODES_R18];
  });
  /** The chosen ranking mode, or the first one the content filter shows when it hides that one. */
  const rankingMode = createMemo<RankingMode>(() => (modes().includes(mode()) ? mode() : modes()[0]!));

  createEffect(() => {
    if (!showAdult() && content() !== "safe") setContent("safe");
  });
  createEffect(() => setMode(rankingMode()));

  const startRanking = () => void ranking.start((page) => pixivApi.ranking(rankingMode(), page));
  const startSearch = () => {
    const q = query().trim();
    if (!q) return void search.start(null);
    const params = { q, order: order(), type: kind(), mode: contentNow() };
    void search.start((page) => pixivApi.search({ ...params, page }));
  };
  const startBookmarks = () => {
    const c = contentNow();
    void bookmarks.start(async () => ({ ...(await pixivApi.bookmarks(c)), page: 1, hasMore: false }));
  };

  // Ranking follows its mode (and the adult switch, since the service filters by it).
  createEffect(on([rankingMode, showAdult], () => (tab() === "ranking" ? startRanking() : void ranking.start(null))));
  createEffect(
    on(tab, (current) => {
      if (current === "ranking" && !ranking.started()) startRanking();
      if (current === "bookmarks") startBookmarks();
    }, { defer: true }),
  );
  // Search and bookmarks follow the content filter (ranking does through its modes).
  createEffect(
    on(contentNow, () => {
      if (query()) startSearch();
      if (tab() === "bookmarks") startBookmarks();
    }, { defer: true }),
  );
  const feedOf = (): Feed => (tab() === "ranking" ? ranking : tab() === "search" ? search : bookmarks);
  const openWork = (item: PixivItem) => {
    setPreview(item);
    navigate(`/pixiv/${encodeURIComponent(item.id)}`);
  };
  const closeWork = () => {
    setPreview(undefined);
    navigate("/pixiv");
  };
  const searchFor = (tag: string) => {
    setDraft(tag);
    setQuery(tag);
    setTab("search");
    startSearch();
    closeWork();
  };
  const onBookmark = (id: string, on: boolean) => {
    for (const feed of [ranking, search]) feed.patch(id, { bookmarked: on });
    if (tab() === "bookmarks" && !on) startBookmarks();
    else bookmarks.patch(id, { bookmarked: on });
  };
  const openId = () => (params.id ? decodeURIComponent(params.id) : undefined);

  const submitSearch = (e: Event) => {
    e.preventDefault();
    setQuery(draft().trim());
    startSearch();
  };

  const tabs: [Tab, () => string][] = [
    ["ranking", () => t("pixiv.tabRanking")],
    ["search", () => t("pixiv.tabSearch")],
    ["bookmarks", () => t("pixiv.tabBookmarks")],
  ];

  const { wide, track } = useWide();

  const sectionNavItems = (): SectionNavItem[] =>
    tabs.map(([id, label]) => ({
      label: label(),
      onSelect: () => setTab(id),
      current: tab() === id,
    }));

  const contentSelect = () => (
    <label class="px-select px-content">
      <span>{t("pixiv.safety")}</span>
      <select name="content" value={contentNow()} onChange={(e) => setContent(e.currentTarget.value as PixivContent)}>
        <option value="safe">{t("pixiv.safetySafe")}</option>
        <Show when={showAdult()}>
          <option value="r18">{t("pixiv.safetyR18")}</option>
          <option value="all">{t("pixiv.safetyAll")}</option>
        </Show>
      </select>
    </label>
  );
  const sortNote = () => {
    const used = search.sort()?.used;
    return used ? SORT_NOTE[used]?.() ?? "" : "";
  };

  const panel = (desk: boolean) => (
    <div id="px-panel" role="tabpanel" aria-labelledby={`px-tab-${tab()}`} class="px-panel">
      <Show when={tab() === "ranking"}>
        <div class="px-filters">{contentSelect()}</div>
        <div class="px-chips" role="group" aria-label={t("pixiv.modes")}>
          <For each={modes()}>
            {(m) => (
              <button type="button" class="px-chip" classList={{ "px-chip--on": rankingMode() === m, "px-chip--adult": m.endsWith("_r18") }} aria-pressed={rankingMode() === m} data-mode={m} onClick={() => setMode(m)}>
                {MODE_LABEL[m]()}
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={tab() === "search"}>
        <form class="px-search" role="search" onSubmit={submitSearch}>
          <label class="media-search px-search__field">
            <SearchIcon />
            <input type="search" name="q" placeholder={t("pixiv.searchPlaceholder")} aria-label={t("pixiv.searchLabel")} value={draft()} onInput={(e) => setDraft(e.currentTarget.value)} enterkeyhint="search" />
          </label>
          <Show when={desk} fallback={<button type="submit" class="px-btn px-btn--primary">{t("pixiv.searchGo")}</button>}>
            <Button type="submit">{t("pixiv.searchGo")}</Button>
          </Show>
          <label class="px-select">
            <span>{t("pixiv.order")}</span>
            <select name="order" value={order()} onChange={(e) => { setOrder(e.currentTarget.value as SearchOrder); if (query()) startSearch(); }}>
              <option value="date_d">{t("pixiv.orderNew")}</option>
              <option value="date">{t("pixiv.orderOld")}</option>
              <option value="popular">{t("pixiv.orderPopular")}</option>
            </select>
          </label>
          <label class="px-select">
            <span>{t("pixiv.type")}</span>
            <select name="type" value={kind()} onChange={(e) => { setKind(e.currentTarget.value as SearchType); if (query()) startSearch(); }}>
              <option value="all">{t("pixiv.typeAll")}</option>
              <option value="illust">{t("pixiv.typeIllust")}</option>
              <option value="manga">{t("pixiv.typeManga")}</option>
              <option value="ugoira">{t("pixiv.typeUgoira")}</option>
            </select>
          </label>
          {contentSelect()}
        </form>
        <Show when={query() && sortNote()}>
          <p class="px-note px-note--sort">{sortNote()}</p>
        </Show>
        <Show when={!query()}>
          <Show when={desk} fallback={<p class="px-note">{t("pixiv.searchHint")}</p>}>
            <EmptyState text={t("pixiv.searchHint")} />
          </Show>
        </Show>
      </Show>

      <Show when={tab() === "bookmarks"}>
        <div class="px-filters">{contentSelect()}</div>
      </Show>

      <Show when={tab() !== "search" || query()}>
        <FeedGrid feed={feedOf()} empty={tab() === "bookmarks" ? t("pixiv.emptyBookmarks") : t("pixiv.empty")} onOpen={openWork} infinite={tab() !== "bookmarks"} wide={desk} />
      </Show>
    </div>
  );

  return (
    <section class="pixiv" classList={{ "pixiv--wide": wide() }} ref={track} aria-labelledby={wide() ? undefined : "pixiv-title"}>
      <Show
        when={wide()}
        fallback={
          <>
            <A href="/plugins" class="media-back pixiv-back">
              <BackIcon /> {t("pixiv.back")}
            </A>
            <h1 id="pixiv-title">{t("pixiv.title")}</h1>

            <div class="px-tabs" role="tablist" aria-label={t("pixiv.tabs")}>
              <For each={tabs}>
                {([id, label]) => (
                  <button type="button" role="tab" id={`px-tab-${id}`} aria-selected={tab() === id} aria-controls="px-panel" class="px-tab" classList={{ "px-tab--on": tab() === id }} onClick={() => setTab(id)}>
                    {label()}
                  </button>
                )}
              </For>
            </div>

            {panel(false)}
          </>
        }
      >
        <Page class="pixiv-desk">
          <PageHeader title={t("pixiv.title")} nav={<SectionNav label={t("pixiv.tabs")} items={sectionNavItems()} />} />
          <Show when={openId()}>
            <a
              href="/pixiv"
              class="pg-link pg-back"
              onClick={(e) => {
                e.preventDefault();
                closeWork();
              }}
            >
              {t("pixiv.back")}
            </a>
          </Show>
          <PageGrid layout="full">
            <PageCard class="pixiv-surface">{panel(true)}</PageCard>
          </PageGrid>
        </Page>
      </Show>

      <Show when={openId()} keyed>
        {(id) => <Reader id={id} preview={preview()?.id === id ? preview() : undefined} onClose={closeWork} onTag={searchFor} onBookmark={onBookmark} />}
      </Show>
    </section>
  );
}
