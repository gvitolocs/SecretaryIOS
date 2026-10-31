import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, on, onCleanup, onMount, Show, type Accessor } from "solid-js";
import { ApiError } from "../api/client";
import { instagramApi, type IgHighlight, type IgItem, type IgPage } from "../api/instagram";
import {
  instagramWatched,
  instagramWatchSettings,
  loadInstagramWatchSettings,
  refreshInstagramBadges,
  refreshInstagramWatched,
  setInstagramBadges,
  setInstagramWatchSettings,
} from "../state/instagramBadge";
import { BackIcon, BellIcon, CheckIcon, ExternalIcon, SearchIcon } from "../components/Icons";
import {
  Card,
  EmptyState,
  ListRow,
  LoadingState,
  Page,
  PageGrid,
  PageHeader,
  PrimaryAction,
  RowList,
  useWide,
} from "../components/page";
import InstagramOpened from "../components/InstagramOpened";
import { PhotoViewer } from "../components/PhotoViewer";
import { locale, t } from "../i18n";
import { formatCount, nextCheckText, parseTyped, slideIndexOf, slidesOf } from "../lib/instagram";
import { mergeFirstPage, pageCache } from "../lib/pageCache";
import { rememberScroll } from "../lib/pageScroll";
import { createCached } from "../lib/swr";
import "../styles/media.css";
import "../styles/instagram.css";

// Instagram (/instagram/:username?, a Plugins card; owner only): anonymous profiles, stories,
// highlights, posts and reels through the server's service (docs/instagram.md). Nothing is
// fetched from Instagram by the browser: every picture and video is a signed URL of our server.

type Tab = "stories" | "highlights" | "posts" | "reels" | "archive";
const TABS: Tab[] = ["stories", "highlights", "posts", "reels", "archive"];

/** What to tell the owner about a failed request. */
export function errorText(e: unknown, missing?: string): string {
  if (e instanceof ApiError) {
    if (e.status === 404) return missing ?? t("instagram.notFound");
    if (e.status === 429) return t("instagram.rateLimited", { seconds: e.retryAfterSeconds ?? 60 });
  }
  return t("instagram.failed");
}

interface Feed {
  items: Accessor<IgItem[]>;
  hasMore: Accessor<boolean>;
  loading: Accessor<boolean>;
  /** Cached items show while the first page is asked again. */
  updating: Accessor<boolean>;
  error: Accessor<unknown>;
  loaded: Accessor<boolean>;
  /** The first page: fetched, or (with items on screen) fetched again and merged in. */
  load: () => Promise<void>;
  more: () => Promise<void>;
  /** A first page that came with something else (the profile bundle's posts), merged like a refresh. */
  absorb: (page: IgPage) => void;
}

/** What a feed keeps in lib/pageCache: the items loaded so far and where the next page starts. */
interface FeedCopy {
  items: IgItem[];
  nextCursor: string | null;
}

/**
 * A list that is fetched a page at a time (optionally starting from a page we already have). With `cacheKey` it opens
 * on its last copy (stale-while-revalidate): `load` then asks for the first page and merges it in place, and "load
 * more" goes on from where the copy ended.
 */
function createFeed(fetchPage: (cursor: string | null) => Promise<IgPage>, initial?: IgPage, cacheKey?: string): Feed {
  const copy = cacheKey ? pageCache.peek<FeedCopy>(cacheKey)?.value : undefined;
  const [items, setItems] = createSignal<IgItem[]>(copy?.items ?? initial?.items ?? []);
  const [cursor, setCursor] = createSignal<string | null>(copy ? copy.nextCursor : initial && !initial.exhausted ? initial.nextCursor : null);
  const [loading, setLoading] = createSignal(false);
  const [updating, setUpdating] = createSignal(false);
  const [error, setError] = createSignal<unknown>(null);
  const [loaded, setLoaded] = createSignal(!!copy || !!initial);
  const revision = pageCache.revision("instagram");
  const current = () => revision === pageCache.revision("instagram");
  function store() {
    if (cacheKey && current()) void pageCache.put(cacheKey, "instagram", { items: items().slice(0, 300), nextCursor: cursor() } satisfies FeedCopy);
  }
  function absorb(page: IgPage) {
    if (loaded()) setItems((old) => mergeFirstPage(old, page.items));
    else {
      setItems(page.items);
      setCursor(page.exhausted ? null : page.nextCursor);
      setLoaded(true);
    }
    store();
  }
  // Started from a page in hand with no copy in memory: a longer copy from an earlier visit may be stored.
  if (cacheKey && initial && !copy) {
    void pageCache.get<FeedCopy>(cacheKey).then((stored) => {
      if (!stored || !current() || stored.value.items.length <= items().length) return;
      setItems((now) => mergeFirstPage(stored.value.items, now));
      setCursor(stored.value.nextCursor);
    });
  }
  async function run(from: string | null, append: boolean) {
    if (loading() || updating()) return;
    setError(null);
    const answer = fetchPage(from).then(
      (page) => ({ page }),
      (failure: unknown) => ({ failure }),
    );
    if (!append && !loaded() && cacheKey) {
      setLoading(true);
      const stored = await pageCache.get<FeedCopy>(cacheKey);
      if (stored && current()) {
        setItems(stored.value.items);
        setCursor(stored.value.nextCursor);
        setLoaded(true);
        setLoading(false);
      }
    }
    const refreshing = !append && loaded();
    if (refreshing) setUpdating(true);
    else setLoading(true);
    const result = await answer;
    try {
      if (!current()) return;
      if ("failure" in result) throw result.failure;
      if (refreshing) absorb(result.page);
      else {
        const page = result.page;
        const seen = new Set(append ? items().map((i) => i.id) : []);
        const fresh = page.items.filter((i) => !seen.has(i.id));
        setItems(append ? [...items(), ...fresh] : fresh);
        setCursor(page.exhausted || fresh.length === 0 ? null : page.nextCursor);
        setLoaded(true);
        store();
      }
    } catch (e) {
      // A failed refresh keeps the copy on screen; a failed first or next page says so.
      if (!refreshing) setError(e);
    } finally {
      setLoading(false);
      setUpdating(false);
    }
  }
  return {
    items,
    hasMore: () => cursor() !== null,
    loading,
    updating,
    error,
    loaded,
    load: () => run(null, false),
    more: () => {
      const c = cursor();
      return c ? run(c, true) : Promise.resolve();
    },
    absorb,
  };
}

function PlayBadge() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M8 5.5v13l11-6.5z" fill="currentColor" stroke="none" />
    </svg>
  );
}

function StackBadge() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="8" y="8" width="12" height="12" rx="2.5" />
      <path d="M16 5.5V5.2A1.7 1.7 0 0 0 14.3 3.5H5.2A1.7 1.7 0 0 0 3.5 5.2v9.1A1.7 1.7 0 0 0 5.2 16h.3" />
    </svg>
  );
}

function Tile(props: { item: IgItem; label: string; onOpen: () => void }) {
  const src = () => props.item.thumbUrl ?? (props.item.kind === "image" ? props.item.url : null);
  return (
    <button
      type="button"
      class="ig-tile"
      data-kind={props.item.kind}
      data-media-src={props.item.kind === "image" ? props.item.url : undefined}
      data-media-link={props.item.permalink}
      aria-label={props.label}
      onClick={props.onOpen}
    >
      <Show when={src()} fallback={<span class="ig-tile__blank" />}>
        {(url) => <img src={url()} alt="" loading="lazy" referrerpolicy="no-referrer" />}
      </Show>
      <Show when={props.item.kind !== "image"}>
        <span class="ig-tile__badge">{props.item.kind === "video" ? <PlayBadge /> : <StackBadge />}</span>
      </Show>
    </button>
  );
}

function Avatar(props: { url: string | null; name: string; size?: "lg" }) {
  return (
    <span class="ig-avatar" classList={{ "ig-avatar--lg": props.size === "lg" }} aria-hidden="true">
      <Show when={props.url} fallback={<span>{props.name.slice(0, 1).toUpperCase()}</span>}>
        {(url) => <img src={url()} alt="" referrerpolicy="no-referrer" />}
      </Show>
    </span>
  );
}

/**
 * The profile picture in the header: a button that opens it in the viewer (zoom, pan, swipe down or Esc to close). The
 * backend gives only Instagram's small rendition (100 px, 2026-10-30): its CDN refuses any other size of the signed
 * URL and Instagram's own profile API wants a login, so the viewer shows that one, not an HD copy.
 */
function ProfilePicture(props: { url: string | null; username: string }) {
  const [open, setOpen] = createSignal(false);
  return (
    <Show when={props.url} fallback={<Avatar url={null} name={props.username} size="lg" />}>
      {(url) => (
        <>
          <button
            type="button"
            class="ig-head__pic"
            aria-label={t("instagram.openProfilePicture", { name: props.username })}
            aria-haspopup="dialog"
            onClick={() => setOpen(true)}
          >
            <Avatar url={url()} name={props.username} size="lg" />
          </button>
          <Show when={open()}>
            <PhotoViewer
              items={[{ src: url(), kind: "photo", name: `@${props.username}`, id: `pfp:${props.username}` }]}
              index={0}
              onIndex={() => {}}
              onClose={() => setOpen(false)}
              label={() => t("instagram.profilePicture", { name: props.username })}
            />
          </Show>
        </>
      )}
    </Show>
  );
}

function Verified() {
  return (
    <span class="ig-verified" role="img" aria-label={t("instagram.verified")}>
      <CheckIcon />
    </span>
  );
}

interface Opened {
  feed: Feed;
  start: number;
  username: string;
  /** From the Archive: already in the Gallery. */
  archived?: boolean;
  /** Open on the tiles (a highlight), with this title and cover. */
  tiles?: boolean;
  title?: string;
  cover?: string | null;
}

function ProfilePage(props: { username: string; open: (o: Opened) => void }) {
  // Not a createResource: reading one that failed throws, and a 404 is an ordinary answer here. The profile opens on
  // its last copy (lib/swr) while it is asked again; so do the highlight tray, the tab and the scroll offset.
  const key = `instagram:profile:${props.username}`;
  const trayKey = `instagram:tray:${props.username}`;
  const profile = createCached("instagram", () => key, () => instagramApi.profile(props.username));
  const bundle = profile.value;
  const loadError = profile.error;
  const fetching = () => profile.loading() || profile.updating();
  const refetch = () => profile.refresh();
  const [highlights, setHighlights] = createSignal<IgHighlight[]>(pageCache.peek<IgHighlight[]>(trayKey)?.value ?? []);
  const [highlightsLoading, setHighlightsLoading] = createSignal(false);
  const [highlightsError, setHighlightsError] = createSignal<unknown>(null);
  let posts: Feed | undefined;
  // A tray from an earlier visit shows at once but is asked again once: its covers are signed URLs that expire within
  // the hour (docs/instagram.md), so an old copy would show blank covers.
  let trayFresh = false;
  async function loadHighlights() {
    if (highlightsLoading() || bundle()?.profile.isPrivate) return;
    const shown = highlights().length > 0;
    if (shown && trayFresh) return;
    trayFresh = true;
    if (!shown) {
      setHighlightsLoading(true);
      setHighlightsError(null);
    }
    try {
      const tray = await instagramApi.highlights(props.username);
      if (tray.length || !shown) setHighlights(tray);
      if (tray.length) void pageCache.put(trayKey, "instagram", tray);
    } catch (e) {
      // A failed refresh keeps the copy on screen.
      if (!shown) setHighlightsError(e);
    } finally {
      setHighlightsLoading(false);
    }
  }
  // The bundle's own tray, when it has one; an empty one never hides the tray fetched on its own.
  createEffect(
    on(bundle, (b) => {
      if (!b) return;
      if (b.highlights.length) {
        trayFresh = true;
        setHighlights(b.highlights);
        void pageCache.put(trayKey, "instagram", b.highlights);
      }
      posts?.absorb(b.posts);
    }),
  );
  const tabSlot = `${key}:tab`;
  const savedTab = pageCache.snapshots.get(tabSlot) as Tab | undefined;
  const [tab, setTab] = createSignal<Tab>(savedTab ?? "posts");
  createEffect(on(tab, (tb) => pageCache.snapshots.set(tabSlot, tb)));
  rememberScroll(() => key, () => !!bundle());
  const [notice, setNotice] = createSignal("");
  const [opening, setOpening] = createSignal<string | null>(null);
  const watchEntry = () => instagramWatched().find((w) => w.username === props.username);
  // A check that is due now (the first one, seconds after Watch) says "in a moment"; once it has run, the list is asked
  // again so the line shows the next one ("tomorrow 15:40") instead of a countdown that went stale on screen.
  const nextCheckLine = (at: number) => {
    const when = nextCheckText(at, Date.now(), locale());
    return when ? t("instagram.nextCheck", { when }) : t("instagram.firstCheck");
  };
  createEffect(
    on(() => watchEntry()?.nextCheck, (at) => {
      if (!at) return;
      const wait = at * 1000 - Date.now();
      if (wait > 120_000) return;
      const timer = setTimeout(() => void refreshInstagramWatched(), Math.max(5_000, wait + 8_000));
      onCleanup(() => clearTimeout(timer));
    }),
  );

  // Opening a watched account clears its red dot.
  createEffect(
    on(
      () => bundle()?.profile.username,
      async (name) => {
        if (!name) return;
        await refreshInstagramWatched();
        if (!watchEntry()) return;
        try {
          setInstagramBadges(await instagramApi.opened(props.username));
          await refreshInstagramWatched();
        } catch {
          // The dot stays until the next time.
        }
      },
    ),
  );

  void loadInstagramWatchSettings();
  const intervalHours = () => instagramWatchSettings()?.intervalHours ?? 23;
  const intervalChoices = () => [...(instagramWatchSettings()?.choices ?? [6, 12, 23])].sort((a, b) => b - a);
  const intervalLabel = (n: number) => (n === 23 ? t("instagram.watchDaily") : t("instagram.watchEveryHours", { n }));

  async function changeInterval(select: HTMLSelectElement) {
    setNotice("");
    try {
      setInstagramWatchSettings(await instagramApi.setWatchSettings(Number(select.value)));
      await refreshInstagramWatched();
    } catch {
      select.value = String(intervalHours());
      setNotice(t("instagram.watchIntervalFailed"));
    }
  }

  async function changeWatch(run: () => Promise<unknown>) {
    setNotice("");
    try {
      await run();
      await Promise.all([refreshInstagramWatched(), refreshInstagramBadges()]);
    } catch {
      setNotice(t("instagram.watchFailed"));
    }
  }

  const feedKey = (tb: Tab) => `instagram:feed:${props.username}:${tb}`;
  const reels = createFeed((c) => instagramApi.reels(props.username, c), undefined, feedKey("reels"));
  const stories = createFeed(() => instagramApi.stories(props.username), undefined, feedKey("stories"));
  const archive = createFeed((c) => instagramApi.archive(props.username, c), undefined, feedKey("archive"));
  const postsFeed = () => (posts ??= createFeed((c) => instagramApi.posts(props.username, c), bundle()?.posts, feedKey("posts")));

  const feedOf = (tb: Tab): Feed | undefined => (tb === "posts" ? (bundle() ? postsFeed() : undefined) : tb === "reels" ? reels : tb === "stories" ? stories : tb === "archive" ? archive : undefined);

  // Stories first when there are some (unless Back restored a tab); each tab is asked once per visit when it is first
  // shown: fetched, or refreshed over its copy. Posts come with the profile, which is refreshed anyway.
  createEffect(
    on(
      () => bundle()?.profile.username,
      (name) => {
        if (name && !savedTab) setTab(bundle()?.profile.hasStories ? "stories" : "posts");
      },
    ),
  );
  const asked = new Set<Feed>();
  createEffect(() => {
    const f = bundle() ? feedOf(tab()) : undefined;
    if (!f || asked.has(f)) return;
    asked.add(f);
    if (f !== posts || !f.loaded()) void f.load();
  });

  // Track tab entries only: an empty response stays unknown, without a request loop.
  createEffect(on([bundle, tab], ([b, tb]) => {
    if (b && tb === "highlights") void loadHighlights();
  }));

  async function openHighlight(id: string, title: string, cover: string | null) {
    setNotice("");
    setOpening(id);
    // Its copy from an earlier visit (also after a restart) opens the tiles at once, and the feed refreshes it.
    const cacheKey = `instagram:highlight:${id}`;
    await pageCache.get(cacheKey);
    const feed = createFeed((c) => instagramApi.highlightItems(id, c), undefined, cacheKey);
    if (feed.loaded() && feed.items().length) {
      setOpening(null);
      props.open({ feed, start: 0, username: props.username, tiles: true, title, cover });
      void feed.load();
      return;
    }
    await feed.load();
    setOpening(null);
    if (feed.error()) setNotice(errorText(feed.error()));
    else if (feed.items().length === 0) setNotice(t("instagram.noHighlights"));
    else props.open({ feed, start: 0, username: props.username, tiles: true, title, cover });
  }

  const tabLabel = (tb: Tab) => t(`instagram.${tb}`);
  const empty = (tb: Tab) =>
    t(tb === "stories" ? "instagram.noStories" : tb === "highlights" ? "instagram.noHighlights" : tb === "reels" ? "instagram.noReels" : tb === "archive" ? "instagram.noArchive" : "instagram.noPosts");

  return (
    <div class="ig-profile">
      <Show when={loadError()}>
        <div class="ig-state" role="alert">
          <p>{errorText(loadError())}</p>
          <Show when={!(loadError() instanceof ApiError && (loadError() as ApiError).status === 404)}>
            <button type="button" class="ig-btn" onClick={() => void refetch()}>{t("instagram.retry")}</button>
          </Show>
        </div>
      </Show>
      <Show when={fetching() && !bundle()}>
        <p class="ig-muted" role="status">{t("instagram.loading")}</p>
      </Show>
      <Show when={profile.updating()}>
        <p class="swr-updating" role="status">{t("common.updating")}</p>
      </Show>

      <Show when={bundle()}>
        {(b) => (
          <>
            <header class="ig-head">
              <ProfilePicture url={b().profile.picUrl} username={b().profile.username} />
              <div class="ig-head__text">
                <h2 class="ig-head__name">
                  <span>{b().profile.fullName || b().profile.username}</span>
                  <Show when={b().profile.isVerified}><Verified /></Show>
                </h2>
                <p class="ig-head__user">@{b().profile.username}</p>
              </div>
              <ul class="ig-stats">
                <li><strong>{formatCount(b().profile.mediaCount)}</strong> <span>{t("instagram.posts")}</span></li>
                <li><strong>{formatCount(b().profile.followerCount)}</strong> <span>{t("instagram.followers")}</span></li>
                <li><strong>{formatCount(b().profile.followingCount)}</strong> <span>{t("instagram.following")}</span></li>
              </ul>
              <Show when={b().profile.biography}>
                {(bio) => (
                  <p class="ig-bio">
                    <span class="ig-bio__label">{t("instagram.bio")}</span>
                    <span>{bio()}</span>
                  </p>
                )}
              </Show>
              <Show when={b().profile.externalUrl}>
                {(href) => (
                  <a class="ig-link" href={href()} target="_blank" rel="noopener noreferrer">
                    <ExternalIcon /> <span>{href().replace(/^https?:\/\//, "").replace(/\/$/, "")}</span>
                  </a>
                )}
              </Show>
            </header>

            <div class="ig-watch">
              <Show
                when={watchEntry()}
                fallback={
                  <button type="button" class="ig-btn" onClick={() => void changeWatch(() => instagramApi.watch(props.username))}>
                    <BellIcon /> <span>{t("instagram.watch")}</span>
                  </button>
                }
              >
                {(w) => (
                  <>
                    <button type="button" class="ig-btn ig-btn--on" aria-pressed="true" onClick={() => void changeWatch(() => instagramApi.unwatch(props.username))}>
                      <CheckIcon /> <span>{t("instagram.watching")}</span>
                    </button>
                    <label class="ig-switch">
                      <input type="checkbox" checked={w().autoSave} onChange={(e) => void changeWatch(() => instagramApi.watch(props.username, { autoSave: e.currentTarget.checked }))} />
                      <span>{t("instagram.autoSave")}</span>
                    </label>
                    <label class="ig-interval">
                      <span>{t("instagram.watchInterval")}</span>
                      <select value={String(intervalHours())} aria-describedby="ig-interval-note" onChange={(e) => void changeInterval(e.currentTarget)}>
                        <For each={intervalChoices()}>{(n) => <option value={String(n)}>{intervalLabel(n)}</option>}</For>
                      </select>
                    </label>
                    <p class="ig-muted ig-watch__hint">
                      {intervalHours() === 23 ? t("instagram.watchHintDay") : t("instagram.watchHintHours", { n: intervalHours() })}
                      <Show when={w().nextCheck}>{(at) => <> {nextCheckLine(at())}</>}</Show>
                      {" "}<span id="ig-interval-note">{t("instagram.watchIntervalNote")}</span>
                    </p>
                    <Show when={w().gone}>
                      <p class="ig-note" role="status">{t("instagram.gone")}</p>
                    </Show>
                  </>
                )}
              </Show>
            </div>

            <Show when={b().profile.isPrivate}>
              <p class="ig-note">{t("instagram.private")}</p>
            </Show>

            <div class="ig-tabs" role="tablist" aria-label={t("instagram.tabs")}>
              <For each={TABS}>
                {(tb) => (
                  <button type="button" role="tab" id={`ig-tab-${tb}`} aria-selected={tab() === tb} aria-controls="ig-panel" classList={{ "ig-tab": true, "ig-tab--on": tab() === tb }} onClick={() => { setNotice(""); setTab(tb); }}>
                    {tabLabel(tb)}
                  </button>
                )}
              </For>
            </div>

            <Show when={notice()}>
              <p class="ig-note" role="alert">{notice()}</p>
            </Show>

            <div id="ig-panel" role="tabpanel" aria-labelledby={`ig-tab-${tab()}`} class="ig-panel">
              <Show when={tab() === "highlights"}>
                <Show when={!highlightsLoading()} fallback={<p role="status" class="ig-muted">{t("instagram.loading")}</p>}>
                  <Show when={!highlightsError()} fallback={<p role="alert">{errorText(highlightsError())}</p>}>
                    <Show when={highlights().length > 0} fallback={<p class="ig-muted">{empty("highlights")}</p>}>
                      <ul class="ig-highlights">
                        <For each={highlights()}>
                          {(h) => (
                            <li>
                              <button type="button" class="ig-highlight" aria-busy={opening() === h.id} disabled={opening() !== null} onClick={() => void openHighlight(h.id, h.title, h.coverUrl)}>
                                <Avatar url={h.coverUrl} name={h.title} size="lg" />
                                <span class="ig-highlight__title">{h.title}</span>
                                <Show when={h.mediaCount}>{(n) => <span class="ig-highlight__count">{t("instagram.items", { count: n() })}</span>}</Show>
                              </button>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </Show>
                  <Show when={!highlights().length && !b().profile.isPrivate}>
                    <button type="button" class="btn" onClick={() => void loadHighlights()}>{t("instagram.retry")}</button>
                  </Show>
                </Show>
              </Show>

              <For each={TABS.filter((x) => x !== "highlights" && x === tab())}>
                {(tb) => {
                  const feed = () => feedOf(tb);
                  return (
                    <Show when={feed()}>
                      {(f) => (
                        <>
                          <Show when={f().items().length > 0}>
                            <div class="ig-grid" classList={{ "ig-grid--tall": tb !== "posts" }}>
                              <For each={f().items()}>
                                {(item, i) => (
                                  <Tile
                                    item={item}
                                    label={`${tabLabel(tb)} ${i() + 1}${item.caption ? `: ${item.caption.slice(0, 60)}` : ""}`}
                                    onOpen={() => props.open({ feed: f(), start: slideIndexOf(f().items(), i()), username: props.username, archived: tb === "archive" })}
                                  />
                                )}
                              </For>
                            </div>
                          </Show>
                          <Show when={f().loaded() && f().items().length === 0 && !f().error()}>
                            <p class="ig-muted">{empty(tb)}</p>
                          </Show>
                          <Show when={f().loading()}>
                            <p class="ig-muted" role="status">{t("instagram.loading")}</p>
                          </Show>
                          <Show when={f().updating()}>
                            <p class="swr-updating" role="status">{t("common.updating")}</p>
                          </Show>
                          <Show when={f().error()}>
                            <div class="ig-state" role="alert">
                              <p>{errorText(f().error(), empty(tb))}</p>
                              <button type="button" class="ig-btn" onClick={() => void (f().loaded() ? f().more() : f().load())}>{t("instagram.retry")}</button>
                            </div>
                          </Show>
                          <Show when={f().hasMore() && !f().loading() && !f().error()}>
                            <button type="button" class="ig-btn ig-more" onClick={() => void f().more()}>{t("instagram.loadMore")}</button>
                          </Show>
                        </>
                      )}
                    </Show>
                  );
                }}
              </For>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}

export default function InstagramView() {
  const measured = useWide();
  const wide = measured.wide;
  const params = useParams<{ username?: string }>();
  const navigate = useNavigate();
  const [text, setText] = createSignal("");
  const [problem, setProblem] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [opened, setOpened] = createSignal<Opened | null>(null);
  onMount(() => void refreshInstagramWatched());
  const [recent] = createResource(() => (params.username ? null : true), () => instagramApi.recent().catch(() => []));

  async function openTyped(e?: Event) {
    e?.preventDefault();
    setProblem("");
    const typed = parseTyped(text());
    if (!typed) return setProblem(t("instagram.invalid"));
    if (typed.kind === "user") {
      setText("");
      navigate(`/instagram/${encodeURIComponent(typed.username)}`);
      return;
    }
    setBusy(true);
    try {
      const page = await instagramApi.resolve(typed.url);
      const feed = createFeed(async () => page, page);
      if (feed.items().length === 0) setProblem(t("instagram.notFound"));
      else {
        setText("");
        setOpened({ feed, start: 0, username: typed.username ?? "instagram" });
      }
    } catch (err) {
      setProblem(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const findForm = () => (
    <form class="ig-find" onSubmit={openTyped} role="search">
      <label class="media-search">
        <SearchIcon />
        <input
          type="search"
          class={wide() ? "pg-input" : undefined}
          value={text()}
          placeholder={t("instagram.search")}
          aria-label={t("instagram.searchLabel")}
          autocapitalize="none"
          autocomplete="off"
          spellcheck={false}
          onInput={(e) => setText(e.currentTarget.value)}
        />
      </label>
      <Show
        when={wide()}
        fallback={
          <button type="submit" class="ig-btn ig-btn--primary" disabled={busy()}>
            {t("instagram.open")}
          </button>
        }
      >
        <PrimaryAction disabled={busy()} onClick={() => void openTyped()}>
          {t("instagram.open")}
        </PrimaryAction>
      </Show>
    </form>
  );

  const watchedRail = () => (
    <Show when={instagramWatched().length > 0}>
      <Card list title={t("instagram.watchList")}>
        <RowList label={t("instagram.watchList")}>
          <For each={instagramWatched()}>
            {(w) => {
              const fresh = () => w.newStories + w.newPosts;
              return (
                <ListRow
                  leading={<Avatar url={w.picUrl ?? null} name={w.username} />}
                  title={
                    <button type="button" class="pg-row-name" onClick={() => navigate(`/instagram/${encodeURIComponent(w.username)}`)}>
                      @{w.username}
                    </button>
                  }
                  badge={fresh() > 0 ? fresh() : undefined}
                />
              );
            }}
          </For>
        </RowList>
      </Card>
    </Show>
  );

  const recentMain = () => (
    <Card list title={t("instagram.recent")}>
      <Show when={recent.loading}>
        <LoadingState label={t("instagram.loading")} />
      </Show>
      <Show when={!recent.loading && (recent() ?? []).length === 0}>
        <EmptyState text={t("instagram.recentEmpty")} />
      </Show>
      <Show when={(recent() ?? []).length > 0}>
        <RowList label={t("instagram.recent")}>
          <For each={recent()}>
            {(r) => (
              <ListRow
                leading={<Avatar url={r.picUrl} name={r.username} />}
                title={
                  <button type="button" class="pg-row-name" onClick={() => navigate(`/instagram/${encodeURIComponent(r.username)}`)}>
                    {r.fullName || r.username}
                  </button>
                }
                sub={`@${r.username}`}
                badge={r.isVerified ? <Verified /> : undefined}
              />
            )}
          </For>
        </RowList>
      </Show>
    </Card>
  );

  return (
    <div ref={measured.track}>
      <Show
        when={wide()}
        fallback={
          <section class="instagram" aria-labelledby="instagram-title">
            <A href="/plugins" class="media-back ig-back">
              <BackIcon /> {t("instagram.back")}
            </A>
            <h1 id="instagram-title">{t("instagram.title")}</h1>
            {findForm()}
            <Show when={problem()}>
              <p class="ig-note" role="alert">
                {problem()}
              </p>
            </Show>

            <Show
              keyed
              when={params.username}
              fallback={
                <>
                  <Show when={instagramWatched().length > 0}>
                    <section class="ig-watched" aria-labelledby="ig-watched-title">
                      <h2 id="ig-watched-title" class="media-section">
                        {t("instagram.watchList")}
                      </h2>
                      <ul class="ig-chips">
                        <For each={instagramWatched()}>
                          {(w) => {
                            const fresh = () => w.newStories + w.newPosts;
                            return (
                              <li>
                                <button
                                  type="button"
                                  class="ig-chip"
                                  aria-label={fresh() > 0 ? t("instagram.chipLabel", { name: w.username, count: fresh() }) : `@${w.username}`}
                                  onClick={() => navigate(`/instagram/${encodeURIComponent(w.username)}`)}
                                >
                                  <Avatar url={w.picUrl ?? null} name={w.username} />
                                  <span class="ig-chip__name">@{w.username}</span>
                                  <Show when={fresh() > 0}>
                                    <span class="ig-dot" aria-hidden="true">
                                      {fresh()}
                                    </span>
                                  </Show>
                                </button>
                              </li>
                            );
                          }}
                        </For>
                      </ul>
                    </section>
                  </Show>
                  <section class="ig-recent" aria-labelledby="ig-recent-title">
                    <h2 id="ig-recent-title" class="media-section">
                      {t("instagram.recent")}
                    </h2>
                    <Show when={(recent() ?? []).length > 0} fallback={<p class="ig-muted">{recent.loading ? t("instagram.loading") : t("instagram.recentEmpty")}</p>}>
                      <ul class="ig-recent__list">
                        <For each={recent()}>
                          {(r) => (
                            <li>
                              <button type="button" class="ig-person" onClick={() => navigate(`/instagram/${encodeURIComponent(r.username)}`)}>
                                <Avatar url={r.picUrl} name={r.username} />
                                <span class="ig-person__text">
                                  <span class="ig-person__name">
                                    <span>{r.fullName || r.username}</span>
                                    <Show when={r.isVerified}>
                                      <Verified />
                                    </Show>
                                  </span>
                                  <small>@{r.username}</small>
                                </span>
                              </button>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                    <p class="ig-muted ig-hint">{t("instagram.hint")}</p>
                  </section>
                </>
              }
            >
              {(username) => <ProfilePage username={username} open={setOpened} />}
            </Show>
          </section>
        }
      >
        <Page class="instagram-desk">
          <PageHeader
            title={t("instagram.title")}
            context={params.username ? undefined : t("instagram.hint")}
            action={findForm()}
          />
          <Show when={problem()}>
            <p class="pg-notice" role="alert">
              {problem()}
            </p>
          </Show>
          <Show
            keyed
            when={params.username}
            fallback={
              <PageGrid layout="rail" railLabel={t("instagram.watchList")} rail={watchedRail()}>
                {recentMain()}
              </PageGrid>
            }
          >
            {(username) => (
              <>
                <a href="/instagram" class="pg-link pg-back">
                  {t("instagram.backList")}
                </a>
                <PageGrid layout="full">
                  <ProfilePage username={username} open={setOpened} />
                </PageGrid>
              </>
            )}
          </Show>
        </Page>
      </Show>

      <Show when={opened()} keyed>
        {(o) => (
          <InstagramOpened
            slides={slidesOf(o.feed.items())}
            start={o.start}
            tiles={o.tiles}
            username={o.username}
            title={o.title}
            cover={o.cover}
            archived={o.archived}
            hasMore={o.feed.hasMore()}
            loading={o.feed.loading()}
            onClose={() => setOpened(null)}
            onNearEnd={() => void o.feed.more()}
          />
        )}
      </Show>
    </div>
  );
}
