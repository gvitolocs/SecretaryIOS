import { A } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { formatTime, mediaApi, type MediaItem, type MediaKind, type SubtitleTrack } from "../api/player";
import { BackIcon, CloseIcon, ExitFullscreenIcon, FilmIcon, FullscreenIcon, MicIcon, SearchIcon } from "../components/Icons";
import { onFocusModeChange, setFocusMode } from "../lib/focusMode";
import {
  Card,
  EmptyState,
  ErrorState,
  ListRow,
  LoadingState,
  Page,
  PageGrid,
  PageHeader,
  RowList,
  SectionNav,
  useWide,
} from "../components/page";
import { locale, t } from "../i18n";
import {
  attachElement,
  continueItems,
  isFinished,
  media,
  mediaElement,
  next,
  playQueue,
  previous,
  progressOf,
  ready,
  seek,
  selectSubtitle,
  setRate,
  stop,
  syncProgress,
  toggle,
} from "../state/media";
import "../styles/media.css";

// The media library (/media, a Plugins card): videos, audio and photos on the owner's server,
// and the player. Video and audio play in the page's one <video> element (state/media.ts),
// which keeps playing when the owner leaves the page; the mini bar brings it back.

const KINDS: { kind: MediaKind; label: () => string }[] = [
  { kind: "video", label: () => t("media.videos") },
  { kind: "audio", label: () => t("media.audio") },
  { kind: "photo", label: () => t("media.photos") },
];

/** "Danish", "dansk" or the track's own label, in the page's language. */
export function trackName(track: SubtitleTrack, lang = locale()): string {
  let name: string | undefined;
  try {
    name = track.lang ? new Intl.DisplayNames([lang], { type: "language" }).of(track.lang) : undefined;
  } catch {
    name = undefined;
  }
  if (!name || name === track.lang) return track.label;
  const cap = name.charAt(0).toUpperCase() + name.slice(1);
  return track.label.toLowerCase().includes(name.toLowerCase()) || track.label.length <= 3 ? cap : `${cap} · ${track.label}`;
}

export function trackDetail(track: SubtitleTrack): string {
  const parts = [track.source === "file" ? t("media.fromFile") : t("media.inVideo")];
  if (track.kind === "image") parts.push(t("media.ocr"));
  if (track.forced) parts.push(t("media.forced"));
  return parts.join(" · ");
}

export function Poster(props: { url?: string | null; kind: string; class?: string }) {
  const [failed, setFailed] = createSignal(false);
  return (
    <Show
      when={props.url && !failed()}
      fallback={
        <span class={`media-poster media-poster--empty ${props.class ?? ""}`} aria-hidden="true">
          {props.kind === "audio" ? <MicIcon /> : <FilmIcon />}
        </span>
      }
    >
      <img class={`media-poster ${props.class ?? ""}`} src={props.url!} alt="" loading="lazy" onError={() => setFailed(true)} />
    </Show>
  );
}

function DeskRow(props: { item: MediaItem; list: MediaItem[]; onPhoto?: (index: number) => void; photoIndex?: number }) {
  const progress = () => progressOf(props.item);
  const isCurrent = () => media.current()?.id === props.item.id;
  const play = () => {
    if (props.item.kind === "photo") {
      props.onPhoto?.(props.photoIndex ?? 0);
      return;
    }
    void playQueue(props.list, props.list.indexOf(props.item));
    media.setOpen(true);
  };
  const meta = () => {
    if (props.item.kind === "photo") return props.item.album ?? "";
    if (props.item.duration_ms) return formatTime((props.item.duration_ms ?? 0) / 1000);
    return "";
  };
  const badge = () => {
    if (isFinished(props.item)) return t("media.watched");
    if (isCurrent()) return media.playing() ? "▶" : "❚❚";
    return undefined;
  };
  return (
    <ListRow
      leading={
        <button type="button" class="media-desk-thumb" onClick={play} aria-label={props.item.title}>
          <Poster url={props.item.poster_url ?? props.item.thumb_url} kind={props.item.kind} />
        </button>
      }
      title={
        <button type="button" class="pg-row-name" onClick={play}>
          {props.item.title}
        </button>
      }
      badge={badge()}
      meta={meta()}
      trailing={
        <Show when={progress() !== null && props.item.kind !== "photo"}>
          <span class="media-progress media-progress--row" aria-hidden="true">
            <span style={{ width: `${Math.round((progress() ?? 0) * 100)}%` }} />
          </span>
        </Show>
      }
    />
  );
}

function Row(props: { item: MediaItem; list: MediaItem[] }) {
  const progress = () => progressOf(props.item);
  const isCurrent = () => media.current()?.id === props.item.id;
  return (
    <li>
      <button
        type="button"
        class="media-row"
        data-kind={props.item.kind}
        data-media-id={props.item.id}
        onClick={() => {
          void playQueue(props.list, props.list.indexOf(props.item));
          media.setOpen(true);
        }}
      >
        <Poster url={props.item.poster_url} kind={props.item.kind} />
        <span class="media-row__text">
          <span class="media-row__title">{props.item.title}</span>
          <span class="media-row__meta">
            <Show when={props.item.duration_ms}>{formatTime((props.item.duration_ms ?? 0) / 1000)}</Show>
            <Show when={isFinished(props.item)}>
              <span class="media-badge">{t("media.watched")}</span>
            </Show>
            <Show when={isCurrent()}>
              <span class="media-badge media-badge--now">{media.playing() ? "▶" : "❚❚"}</span>
            </Show>
          </span>
          <Show when={progress() !== null}>
            <span class="media-progress" aria-hidden="true">
              <span style={{ width: `${Math.round((progress() ?? 0) * 100)}%` }} />
            </span>
          </Show>
        </span>
      </button>
    </li>
  );
}

/** Full screen keeps the controls this long after the last mouse move or tap, then hides them (and the cursor). */
const FULL_CONTROLS_MS = 2000;

const typing = (e: KeyboardEvent) => {
  const el = e.target as HTMLElement | null;
  return !!el && (el.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName));
};

export function Player() {
  let surface!: HTMLDivElement;
  onMount(() => onCleanup(attachElement(surface)));
  const item = () => media.current();
  const tracks = () => media.info()?.subtitles ?? [];

  // Full screen is "full tab" (owner, 2026-10-31): the video fills the page edge to edge, the player's header and
  // footer step aside and the controls float over the picture. Inside Secretary Code the shell's focus mode also
  // hides its rail and titlebar; the browser's Fullscreen API is never used (in the Mac app it would put the whole
  // window into macOS full screen).
  const [full, setFull] = createSignal(false);
  const [controls, setControls] = createSignal(true);
  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  const wake = () => {
    setControls(true);
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (media.playing()) setControls(false);
    }, FULL_CONTROLS_MS);
  };
  const leave = () => {
    setFull(false);
    setControls(true);
    clearTimeout(hideTimer);
    mediaElement().controls = true;
  };
  const enterFull = () => {
    if (full()) return;
    setFull(true);
    mediaElement().controls = false;
    setFocusMode(true);
    wake();
  };
  const exitFull = () => {
    if (!full()) return;
    leave();
    setFocusMode(false);
  };
  const toggleFull = () => (full() ? exitFull() : enterFull());
  // Paused, the controls stay up; playing again, they hide after the delay.
  createEffect(() => {
    if (!full()) return;
    if (!media.playing()) {
      clearTimeout(hideTimer);
      setControls(true);
    } else wake();
  });
  onMount(() => {
    // Our full screen replaces the element's own (macOS full screen of the whole window in Secretary Code).
    const v = mediaElement();
    v.setAttribute("controlslist", "nofullscreen");
    v.disablePictureInPicture = true;
    // Where the element's own button still shows (owner, 2026-10-31: "full screen doesn't do anything" in the Mac app,
    // whose web view ignores element full screen): whatever puts the video itself into full screen ends up in ours.
    const native = () => {
      if (document.fullscreenElement !== v) return;
      void document.exitFullscreen().catch(() => undefined);
      enterFull();
    };
    const blocked = (e: Event) => {
      if (e.target === v) enterFull();
    };
    document.addEventListener("fullscreenchange", native);
    document.addEventListener("fullscreenerror", blocked);
    onCleanup(() => {
      document.removeEventListener("fullscreenchange", native);
      document.removeEventListener("fullscreenerror", blocked);
    });
    // The shell leaving focus mode by itself (its Esc, Exit focus, another view) ends ours too.
    onCleanup(onFocusModeChange((on) => !on && full() && leave()));
  });
  onCleanup(() => {
    clearTimeout(hideTimer);
    if (full()) {
      mediaElement().controls = true;
      setFocusMode(false);
    }
  });

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      if (full()) {
        e.preventDefault();
        exitFull();
      } else media.setOpen(false);
      return;
    }
    if (typing(e) || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "f" || e.key === "F") {
      e.preventDefault();
      toggleFull();
    } else if (full() && e.key === " ") {
      e.preventDefault();
      toggle();
      wake();
    } else if (full()) wake();
  };
  onMount(() => document.addEventListener("keydown", onKey));
  onCleanup(() => document.removeEventListener("keydown", onKey));

  const subtitleSelect = () => (
    <label class="media-select">
      <span class="sr-only">{t("media.subtitles")}</span>
      <select
        aria-label={t("media.subtitles")}
        value={media.subtitle()?.id ?? ""}
        onChange={(e) => void selectSubtitle(tracks().find((x) => x.id === e.currentTarget.value) ?? null)}
      >
        <option value="">{`${t("media.subtitles")}: ${t("media.off")}`}</option>
        <For each={tracks()}>{(track) => <option value={track.id}>{`${trackName(track)} (${trackDetail(track)})`}</option>}</For>
      </select>
    </label>
  );
  const speedSelect = () => (
    <label class="media-select">
      <span class="sr-only">{t("media.speed")}</span>
      <select aria-label={t("media.speed")} value={String(media.rate())} onChange={(e) => setRate(Number(e.currentTarget.value))}>
        <For each={[0.75, 1, 1.25, 1.5, 2]}>{(r) => <option value={String(r)}>{`${r}×`}</option>}</For>
      </select>
    </label>
  );

  return (
    <div
      class="media-player"
      classList={{ "media-player--full": full(), "media-player--idle": full() && !controls() }}
      data-kind={item()?.kind}
      role="dialog"
      aria-modal="true"
      aria-label={item()?.title}
      onPointerMove={() => full() && wake()}
    >
      <Show when={!full()}>
        <div class="media-player__bar">
          <button type="button" class="media-icon-btn" aria-label={t("media.close")} onClick={() => media.setOpen(false)}>
            <BackIcon />
          </button>
          <span class="media-player__title">{item()?.title}</span>
          {subtitleSelect()}
          {speedSelect()}
          <Show when={item()?.kind === "video"}>
            <button type="button" class="media-icon-btn" aria-label={t("media.fullScreen")} title={`${t("media.fullScreen")} (F)`} onClick={enterFull}>
              <FullscreenIcon />
            </button>
          </Show>
        </div>
      </Show>
      <div
        class="media-player__stage"
        onDblClick={(e) => {
          if (item()?.kind !== "video") return;
          e.preventDefault();
          toggleFull();
        }}
        onPointerDown={(e) => {
          // A tap on the picture shows the controls (and hides them again if they were up).
          if (!full() || e.target !== e.currentTarget && !(e.target as HTMLElement).matches("video, .media-player__surface")) return;
          if (e.pointerType === "touch" && controls()) setControls(false);
          else wake();
        }}
      >
        <Show when={item()?.kind === "audio"}>
          <Poster url={media.info()?.poster_url ?? item()?.poster_url} kind="audio" class="media-player__art" />
        </Show>
        <div class="media-player__surface" ref={surface} />
        <Show when={media.subtitleStatus()}>
          <p class="media-player__status" role="status">
            {media.subtitleStatus() === "reading" ? t("media.readingSubtitles") : t("media.loadingSubtitles")}
          </p>
        </Show>
        <Show when={media.error()}>
          <p class="media-player__error" role="alert">{media.error()}</p>
        </Show>
        <Show when={!full() && (media.info()?.bitmap_subtitles ?? 0) > 0}>
          <p class="media-muted">{t("media.hiddenImage", { count: media.info()!.bitmap_subtitles })}</p>
        </Show>
        <Show when={full()}>
          <div class="media-full" classList={{ "media-full--on": controls() }} role="toolbar" aria-label={item()?.title}>
            <div class="media-full__top">
              <span class="media-full__title">{item()?.title}</span>
              <button type="button" class="media-full__btn" aria-label={t("media.exitFullScreen")} title={`${t("media.exitFullScreen")} (Esc)`} onClick={exitFull}>
                <ExitFullscreenIcon />
              </button>
            </div>
            <div class="media-full__bottom">
              <button type="button" class="media-full__btn media-full__play" aria-label={media.playing() ? t("media.pause") : t("media.play")} onClick={toggle}>
                {media.playing() ? "❚❚" : "▶"}
              </button>
              <span class="media-full__time">{formatTime(media.position())}</span>
              <input
                class="media-full__seek"
                type="range"
                min="0"
                max={String(Math.max(1, Math.round(media.duration())))}
                step="1"
                value={String(Math.round(media.position()))}
                aria-label={t("media.seek")}
                onInput={(e) => seek(Number(e.currentTarget.value))}
              />
              <span class="media-full__time">{formatTime(media.duration())}</span>
              {subtitleSelect()}
              {speedSelect()}
              <button type="button" class="media-full__btn" aria-label={t("media.exitFullScreen")} onClick={exitFull}>
                <ExitFullscreenIcon />
              </button>
            </div>
          </div>
        </Show>
      </div>
      <Show when={!full()}>
        <div class="media-player__controls">
          <button type="button" class="media-text-btn" onClick={() => void previous()}>{t("media.previous")}</button>
          <button type="button" class="media-text-btn" disabled={!media.hasNext()} onClick={() => void next()}>{t("media.next")}</button>
        </div>
      </Show>
    </div>
  );
}

export function MiniPlayer() {
  return (
    <Show when={media.current() && !media.open()}>
      <div class="media-mini">
        <button type="button" class="media-mini__open" aria-label={t("media.open")} onClick={() => media.setOpen(true)}>
          <Poster url={media.current()?.poster_url} kind={media.current()?.kind ?? "video"} />
          <span class="media-mini__text">
            <span class="media-row__title">{media.current()?.title}</span>
            <span class="media-row__meta">{`${formatTime(media.position())} / ${formatTime(media.duration())}`}</span>
          </span>
        </button>
        <button type="button" class="media-icon-btn" aria-label={media.playing() ? t("media.pause") : t("media.play")} onClick={toggle}>
          {media.playing() ? "❚❚" : "▶"}
        </button>
        <button type="button" class="media-icon-btn" aria-label={t("media.stop")} onClick={stop}>
          <CloseIcon />
        </button>
      </div>
    </Show>
  );
}

function PhotoViewer(props: { photos: MediaItem[]; index: number; onClose: () => void; onIndex: (i: number) => void }) {
  const photo = () => props.photos[props.index];
  // `/v1/media/s/{token}/{id}/thumb.jpg` → the same token's original: `.../file`.
  const original = () => photo()?.thumb_url?.replace(/\/[^/]+$/, "/file");
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") props.onClose();
    if (e.key === "ArrowLeft" && props.index > 0) props.onIndex(props.index - 1);
    if (e.key === "ArrowRight" && props.index < props.photos.length - 1) props.onIndex(props.index + 1);
  };
  onMount(() => document.addEventListener("keydown", onKey));
  onCleanup(() => document.removeEventListener("keydown", onKey));
  return (
    <div class="media-photo" role="dialog" aria-modal="true" aria-label={photo()?.title}>
      <button type="button" class="media-icon-btn media-photo__close" aria-label={t("media.close")} onClick={() => props.onClose()}>
        <CloseIcon />
      </button>
      <Show when={props.index > 0}>
        <button type="button" class="media-photo__nav media-photo__nav--prev" aria-label={t("media.previousPhoto")} onClick={() => props.onIndex(props.index - 1)}>
          ‹
        </button>
      </Show>
      <img src={original()} alt={photo()?.title ?? ""} />
      <Show when={props.index < props.photos.length - 1}>
        <button type="button" class="media-photo__nav media-photo__nav--next" aria-label={t("media.nextPhoto")} onClick={() => props.onIndex(props.index + 1)}>
          ›
        </button>
      </Show>
    </div>
  );
}

export default function MediaView() {
  const measured = useWide();
  const wide = measured.wide;
  const [kind, setKind] = createSignal<MediaKind>("video");
  const [query, setQuery] = createSignal("");
  const [photo, setPhoto] = createSignal<number | null>(null);
  const [items, { refetch }] = createResource(kind, (k) => mediaApi.allItems(k));
  onMount(() => {
    void ready();
    void syncProgress();
  });

  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    const all = items() ?? [];
    return q ? all.filter((i) => i.title.toLowerCase().includes(q) || (i.album ?? "").toLowerCase().includes(q)) : all;
  });
  const albums = createMemo(() => {
    const groups = new Map<string, MediaItem[]>();
    for (const item of filtered()) {
      const album = item.album ?? "";
      if (!groups.has(album)) groups.set(album, []);
      groups.get(album)!.push(item);
    }
    return [...groups.entries()];
  });
  const resume = () => (query() ? [] : continueItems(items() ?? []));
  const empty = () => ({ video: t("media.noVideos"), audio: t("media.noAudio"), photo: t("media.noPhotos") })[kind()];
  const deskList = createMemo(() => {
    const all = filtered();
    const cont = resume();
    const seen = new Set(cont.map((i) => i.id));
    return [...cont, ...all.filter((i) => !seen.has(i.id))];
  });
  const sectionNav = () => (
    <SectionNav
      label={t("media.sections")}
      items={KINDS.map((k) => ({
        label: k.label(),
        onSelect: () => setKind(k.kind),
        current: kind() === k.kind,
      }))}
    />
  );
  const searchField = () => (
    <label class="media-search">
      <SearchIcon />
      <input
        type="search"
        class={wide() ? "pg-input" : undefined}
        placeholder={t("media.search")}
        aria-label={t("media.search")}
        value={query()}
        onInput={(e) => setQuery(e.currentTarget.value)}
      />
    </label>
  );
  const phoneBody = () => (
    <>
      <Show when={items.error}>
        <p class="media-muted" role="alert">{String(items.error?.message ?? items.error)}</p>
      </Show>
      <Show when={!items.loading && (items() ?? []).length === 0 && !items.error}>
        <p class="media-muted">{empty()}</p>
      </Show>
      <Show
        when={kind() === "photo"}
        fallback={
          <>
            <Show when={resume().length > 0}>
              <h2 class="media-section">{t("media.continue")}</h2>
              <ul class="media-list">
                <For each={resume()}>{(item) => <Row item={item} list={resume()} />}</For>
              </ul>
            </Show>
            <For each={albums()}>
              {([album, entries]) => (
                <>
                  <h2 class="media-section">{album || t("media.library")}</h2>
                  <ul class="media-list">
                    <For each={entries}>{(item) => <Row item={item} list={entries} />}</For>
                  </ul>
                </>
              )}
            </For>
          </>
        }
      >
        <div class="media-grid">
          <For each={filtered()}>
            {(item, i) => (
              <button type="button" class="media-thumb" aria-label={item.title} onClick={() => setPhoto(i())}>
                <Poster url={item.thumb_url} kind="photo" />
              </button>
            )}
          </For>
        </div>
      </Show>
    </>
  );

  return (
    <div ref={measured.track}>
      <Show
        when={wide()}
        fallback={
          <section class="media" aria-labelledby="media-title">
            <A href="/plugins" class="media-back">
              <BackIcon /> {t("media.back")}
            </A>
            <h1 id="media-title">{t("media.title")}</h1>
            <div class="media-tools">
              <div class="media-chips" role="tablist">
                <For each={KINDS}>
                  {(k) => (
                    <button type="button" role="tab" class="media-chip" aria-selected={kind() === k.kind} onClick={() => setKind(k.kind)}>
                      {k.label()}
                    </button>
                  )}
                </For>
              </div>
              {searchField()}
            </div>
            {phoneBody()}
          </section>
        }
      >
        <Page class="media-desk">
          <PageHeader title={t("media.title")} nav={sectionNav()} action={searchField()} />
          <Show when={items.error}>
            <ErrorState
              message={String(items.error?.message ?? items.error)}
              retryLabel={t("common.tryAgain")}
              onRetry={() => void refetch()}
            />
          </Show>
          <Show when={items.loading && !items() && !items.error}>
            <LoadingState label={t("common.loading")} />
          </Show>
          <Show when={!items.loading && (items() ?? []).length === 0 && !items.error}>
            <EmptyState text={empty()} />
          </Show>
          <Show when={(items() ?? []).length > 0}>
            <PageGrid layout="single">
              <Card list title={KINDS.find((k) => k.kind === kind())!.label()} label={t("media.library")}>
                <RowList label={t("media.library")}>
                  <For each={deskList()}>
                    {(item) => (
                      <DeskRow
                        item={item}
                        list={deskList()}
                        onPhoto={(index) => setPhoto(index)}
                        photoIndex={filtered().findIndex((i) => i.id === item.id)}
                      />
                    )}
                  </For>
                </RowList>
              </Card>
            </PageGrid>
          </Show>
        </Page>
      </Show>

      <MiniPlayer />
      <Show when={media.open() && media.current()}>
        <Player />
      </Show>
      <Show when={photo() !== null}>
        <PhotoViewer photos={filtered()} index={photo()!} onClose={() => setPhoto(null)} onIndex={setPhoto} />
      </Show>
    </div>
  );
}
