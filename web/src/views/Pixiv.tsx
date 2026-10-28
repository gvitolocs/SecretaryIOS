import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { pixivApi, RANKING_MODES, RANKING_MODES_R18, type PixivItem, type RankingMode, type SearchOrder, type SearchSafety, type SearchType } from "../api/pixiv";
import { BackIcon, BookmarkIcon, SearchIcon } from "../components/Icons";
import { Reader } from "../components/pixiv/Reader";
import { t } from "../i18n";
import { showAdult } from "../lib/adult";
import { createFeed, type Feed } from "../lib/pixivFeed";
import "../styles/pixiv.css";

// Pixiv (/pixiv, a Plugins card; owner, 2026-10-28): the ranking, a search and the owner's bookmarks, with a
// reader for multi-page works and "Save to Lucina". The service is crates/secretary-pixiv (docs/pixiv.md).
// R-18 ranking modes and the R-18 search filter exist only while "Show adult plugins" is on; the service
// refuses them without `adult=1` in any case. `/pixiv/<id>` opens a work over the list.

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
function FeedGrid(props: { feed: Feed; empty: string; onOpen: (item: PixivItem) => void; infinite?: boolean }) {
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
  return (
    <>
      <div class="px-grid" role="list" aria-busy={props.feed.loading()}>
        <For each={props.feed.items()}>{(item) => <div role="listitem" class="px-grid__cell"><Card item={item} onOpen={props.onOpen} /></div>}</For>
      </div>
      <div ref={sentinel} class="px-foot" aria-live="polite">
        <Show when={props.feed.error()}>
          <p class="px-note px-note--error" role="alert">{props.feed.error()}</p>
          <button type="button" class="px-btn" onClick={() => void props.feed.retry()}>{t("pixiv.retry")}</button>
        </Show>
        <Show when={props.feed.loading()}>
          <p class="px-note" role="status">{t("pixiv.loading")}</p>
        </Show>
        <Show when={!props.feed.loading() && !props.feed.error() && props.feed.hasMore()}>
          <button type="button" class="px-btn" onClick={() => void props.feed.more()}>{t("pixiv.loadMore")}</button>
        </Show>
        <Show when={!props.feed.loading() && !props.feed.error() && props.feed.started() && props.feed.items().length === 0}>
          <p class="px-note">{props.empty}</p>
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
  const [safety, setSafety] = createSignal<SearchSafety>("safe");
  const ranking = createFeed();
  const search = createFeed();
  const bookmarks = createFeed();
  /** The work opened from a card, so the reader has something to show before the work arrives. */
  const [preview, setPreview] = createSignal<PixivItem>();

  const modes = createMemo<RankingMode[]>(() => (showAdult() ? [...RANKING_MODES, ...RANKING_MODES_R18] : [...RANKING_MODES]));

  // R-18 choices vanish with the switch.
  createEffect(() => {
    if (showAdult()) return;
    if (mode().endsWith("_r18")) setMode("day");
    if (safety() === "r18") setSafety("safe");
  });

  const startRanking = () => void ranking.start((page) => pixivApi.ranking(mode(), page));
  const startSearch = () => {
    const q = query().trim();
    if (!q) return void search.start(null);
    const params = { q, order: order(), type: kind(), mode: safety() };
    void search.start((page) => pixivApi.search({ ...params, page }));
  };

  // Ranking follows its mode (and the adult switch, since the service filters by it).
  createEffect(on([mode, showAdult], () => (tab() === "ranking" ? startRanking() : void ranking.start(null))));
  createEffect(
    on(tab, (current) => {
      if (current === "ranking" && !ranking.started()) startRanking();
      if (current === "bookmarks") void bookmarks.start(async () => ({ items: await pixivApi.bookmarks(), page: 1, hasMore: false, total: null, numPages: null }));
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
    if (tab() === "bookmarks" && !on) void bookmarks.start(async () => ({ items: await pixivApi.bookmarks(), page: 1, hasMore: false, total: null, numPages: null }));
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

  return (
    <section class="pixiv" aria-labelledby="pixiv-title">
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

      <div id="px-panel" role="tabpanel" aria-labelledby={`px-tab-${tab()}`} class="px-panel">
        <Show when={tab() === "ranking"}>
          <div class="px-chips" role="group" aria-label={t("pixiv.modes")}>
            <For each={modes()}>
              {(m) => (
                <button type="button" class="px-chip" classList={{ "px-chip--on": mode() === m, "px-chip--adult": m.endsWith("_r18") }} aria-pressed={mode() === m} data-mode={m} onClick={() => setMode(m)}>
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
            <button type="submit" class="px-btn px-btn--primary">{t("pixiv.searchGo")}</button>
            <label class="px-select">
              <span>{t("pixiv.order")}</span>
              <select name="order" value={order()} onChange={(e) => { setOrder(e.currentTarget.value as SearchOrder); if (query()) startSearch(); }}>
                <option value="date_d">{t("pixiv.orderNew")}</option>
                <option value="date">{t("pixiv.orderOld")}</option>
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
            <label class="px-select">
              <span>{t("pixiv.safety")}</span>
              <select name="safety" value={safety()} onChange={(e) => { setSafety(e.currentTarget.value as SearchSafety); if (query()) startSearch(); }}>
                <option value="safe">{t("pixiv.safetySafe")}</option>
                <option value="all">{t("pixiv.safetyAll")}</option>
                <Show when={showAdult()}>
                  <option value="r18">{t("pixiv.safetyR18")}</option>
                </Show>
              </select>
            </label>
          </form>
          <Show when={!query()}>
            <p class="px-note">{t("pixiv.searchHint")}</p>
          </Show>
        </Show>

        <Show when={tab() !== "search" || query()}>
          <FeedGrid feed={feedOf()} empty={tab() === "bookmarks" ? t("pixiv.emptyBookmarks") : t("pixiv.empty")} onOpen={openWork} infinite={tab() !== "bookmarks"} />
        </Show>
      </div>

      <Show when={openId()} keyed>
        {(id) => <Reader id={id} preview={preview()?.id === id ? preview() : undefined} onClose={closeWork} onTag={searchFor} onBookmark={onBookmark} />}
      </Show>
    </section>
  );
}
