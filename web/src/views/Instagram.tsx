import { A, useNavigate, useParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, on, Show, type Accessor } from "solid-js";
import { ApiError } from "../api/client";
import { instagramApi, type IgItem, type IgPage, type IgProfileBundle } from "../api/instagram";
import { BackIcon, CheckIcon, ExternalIcon, SearchIcon } from "../components/Icons";
import InstagramViewer from "../components/InstagramViewer";
import { t } from "../i18n";
import { formatCount, parseTyped, slideIndexOf, slidesOf } from "../lib/instagram";
import "../styles/media.css";
import "../styles/instagram.css";

// Instagram (/instagram/:username?, a Plugins card; owner only): anonymous profiles, stories,
// highlights, posts and reels through the server's service (docs/instagram.md). Nothing is
// fetched from Instagram by the browser: every picture and video is a signed URL of our server.

type Tab = "stories" | "highlights" | "posts" | "reels";
const TABS: Tab[] = ["stories", "highlights", "posts", "reels"];

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
  error: Accessor<unknown>;
  loaded: Accessor<boolean>;
  load: () => Promise<void>;
  more: () => Promise<void>;
}

/** A list that is fetched a page at a time (optionally starting from a page we already have). */
function createFeed(fetchPage: (cursor: string | null) => Promise<IgPage>, initial?: IgPage): Feed {
  const [items, setItems] = createSignal<IgItem[]>(initial?.items ?? []);
  const [cursor, setCursor] = createSignal<string | null>(initial && !initial.exhausted ? initial.nextCursor : null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<unknown>(null);
  const [loaded, setLoaded] = createSignal(!!initial);
  async function run(from: string | null, append: boolean) {
    if (loading()) return;
    setLoading(true);
    setError(null);
    try {
      const page = await fetchPage(from);
      const seen = new Set(append ? items().map((i) => i.id) : []);
      const fresh = page.items.filter((i) => !seen.has(i.id));
      setItems(append ? [...items(), ...fresh] : fresh);
      setCursor(page.exhausted || fresh.length === 0 ? null : page.nextCursor);
      setLoaded(true);
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  }
  return {
    items,
    hasMore: () => cursor() !== null,
    loading,
    error,
    loaded,
    load: () => run(null, false),
    more: () => {
      const c = cursor();
      return c ? run(c, true) : Promise.resolve();
    },
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
    <button type="button" class="ig-tile" data-kind={props.item.kind} aria-label={props.label} onClick={props.onOpen}>
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
}

function ProfilePage(props: { username: string; open: (o: Opened) => void }) {
  // Not a createResource: reading one that failed throws, and a 404 is an ordinary answer here.
  const [bundle, setBundle] = createSignal<IgProfileBundle>();
  const [loadError, setLoadError] = createSignal<unknown>(null);
  const [fetching, setFetching] = createSignal(true);
  async function refetch() {
    setFetching(true);
    setLoadError(null);
    try {
      setBundle(await instagramApi.profile(props.username));
    } catch (e) {
      setLoadError(e);
    } finally {
      setFetching(false);
    }
  }
  void refetch();
  const [tab, setTab] = createSignal<Tab>("posts");
  const [notice, setNotice] = createSignal("");
  const [opening, setOpening] = createSignal<string | null>(null);

  let posts: Feed | undefined;
  const reels = createFeed((c) => instagramApi.reels(props.username, c));
  const stories = createFeed(() => instagramApi.stories(props.username));
  const postsFeed = () => (posts ??= createFeed((c) => instagramApi.posts(props.username, c), bundle()?.posts));

  const feedOf = (tb: Tab): Feed | undefined => (tb === "posts" ? (bundle() ? postsFeed() : undefined) : tb === "reels" ? reels : tb === "stories" ? stories : undefined);

  // Stories first when there are some; each tab is fetched the first time it is shown.
  createEffect(
    on(
      () => bundle()?.profile.username,
      (name) => {
        if (name) setTab(bundle()?.profile.hasStories ? "stories" : "posts");
      },
    ),
  );
  createEffect(() => {
    const f = bundle() ? feedOf(tab()) : undefined;
    if (f && !f.loaded() && !f.loading() && !f.error()) void f.load();
  });

  async function openHighlight(id: string) {
    setNotice("");
    setOpening(id);
    const feed = createFeed((c) => instagramApi.highlightItems(id, c));
    await feed.load();
    setOpening(null);
    if (feed.error()) setNotice(errorText(feed.error()));
    else if (feed.items().length === 0) setNotice(t("instagram.noHighlights"));
    else props.open({ feed, start: 0, username: props.username });
  }

  const tabLabel = (tb: Tab) => t(`instagram.${tb}`);
  const empty = (tb: Tab) => t(tb === "stories" ? "instagram.noStories" : tb === "highlights" ? "instagram.noHighlights" : tb === "reels" ? "instagram.noReels" : "instagram.noPosts");

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

      <Show when={bundle()}>
        {(b) => (
          <>
            <header class="ig-head">
              <Avatar url={b().profile.picUrl} name={b().profile.username} size="lg" />
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
              <Show when={b().profile.biography}>{(bio) => <p class="ig-bio">{bio()}</p>}</Show>
              <Show when={b().profile.externalUrl}>
                {(href) => (
                  <a class="ig-link" href={href()} target="_blank" rel="noopener noreferrer">
                    <ExternalIcon /> <span>{href().replace(/^https?:\/\//, "").replace(/\/$/, "")}</span>
                  </a>
                )}
              </Show>
            </header>

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
                <Show when={b().highlights.length > 0} fallback={<p class="ig-muted">{empty("highlights")}</p>}>
                  <ul class="ig-highlights">
                    <For each={b().highlights}>
                      {(h) => (
                        <li>
                          <button type="button" class="ig-highlight" aria-busy={opening() === h.id} disabled={opening() !== null} onClick={() => void openHighlight(h.id)}>
                            <Avatar url={h.coverUrl} name={h.title} size="lg" />
                            <span class="ig-highlight__title">{h.title}</span>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
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
                                    onOpen={() => props.open({ feed: f(), start: slideIndexOf(f().items(), i()), username: props.username })}
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
  const params = useParams<{ username?: string }>();
  const navigate = useNavigate();
  const [text, setText] = createSignal("");
  const [problem, setProblem] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [opened, setOpened] = createSignal<Opened | null>(null);
  const [recent] = createResource(() => (params.username ? null : true), () => instagramApi.recent().catch(() => []));

  async function submit(e: Event) {
    e.preventDefault();
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

  return (
    <section class="instagram" aria-labelledby="instagram-title">
      <A href="/plugins" class="media-back ig-back">
        <BackIcon /> {t("instagram.back")}
      </A>
      <h1 id="instagram-title">{t("instagram.title")}</h1>
      <form class="ig-find" onSubmit={submit} role="search">
        <label class="media-search">
          <SearchIcon />
          <input type="search" value={text()} placeholder={t("instagram.search")} aria-label={t("instagram.searchLabel")} autocapitalize="none" autocomplete="off" spellcheck={false} onInput={(e) => setText(e.currentTarget.value)} />
        </label>
        <button type="submit" class="ig-btn ig-btn--primary" disabled={busy()}>{t("instagram.open")}</button>
      </form>
      <Show when={problem()}>
        <p class="ig-note" role="alert">{problem()}</p>
      </Show>

      <Show keyed when={params.username} fallback={
        <section class="ig-recent" aria-labelledby="ig-recent-title">
          <h2 id="ig-recent-title" class="media-section">{t("instagram.recent")}</h2>
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
                          <Show when={r.isVerified}><Verified /></Show>
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
      }>
        {(username) => <ProfilePage username={username} open={setOpened} />}
      </Show>

      <Show when={opened()} keyed>
        {(o) => (
          <InstagramViewer
            slides={slidesOf(o.feed.items())}
            start={o.start}
            username={o.username}
            onClose={() => setOpened(null)}
            onNearEnd={() => void o.feed.more()}
          />
        )}
      </Show>
    </section>
  );
}
