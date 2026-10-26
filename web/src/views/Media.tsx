import { A } from "@solidjs/router";
import { createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { formatTime, mediaApi, type MediaItem, type MediaKind, type SubtitleTrack } from "../api/player";
import { BackIcon, CloseIcon, FilmIcon, MicIcon, SearchIcon } from "../components/Icons";
import { locale, t } from "../i18n";
import {
  attachElement,
  continueItems,
  isFinished,
  media,
  next,
  playQueue,
  previous,
  progressOf,
  ready,
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

function Poster(props: { url?: string | null; kind: string; class?: string }) {
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

export function Player() {
  let surface!: HTMLDivElement;
  onMount(() => onCleanup(attachElement(surface)));
  const item = () => media.current();
  const tracks = () => media.info()?.subtitles ?? [];
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") media.setOpen(false);
  };
  onMount(() => document.addEventListener("keydown", onKey));
  onCleanup(() => document.removeEventListener("keydown", onKey));
  return (
    <div class="media-player" data-kind={item()?.kind} role="dialog" aria-modal="true" aria-label={item()?.title}>
      <div class="media-player__bar">
        <button type="button" class="media-icon-btn" aria-label={t("media.close")} onClick={() => media.setOpen(false)}>
          <BackIcon />
        </button>
        <span class="media-player__title">{item()?.title}</span>
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
        <label class="media-select">
          <span class="sr-only">{t("media.speed")}</span>
          <select aria-label={t("media.speed")} value={String(media.rate())} onChange={(e) => setRate(Number(e.currentTarget.value))}>
            <For each={[0.75, 1, 1.25, 1.5, 2]}>{(r) => <option value={String(r)}>{`${r}×`}</option>}</For>
          </select>
        </label>
      </div>
      <div class="media-player__stage">
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
        <Show when={(media.info()?.bitmap_subtitles ?? 0) > 0}>
          <p class="media-muted">{t("media.hiddenImage", { count: media.info()!.bitmap_subtitles })}</p>
        </Show>
      </div>
      <div class="media-player__controls">
        <button type="button" class="media-text-btn" onClick={() => void previous()}>{t("media.previous")}</button>
        <button type="button" class="media-text-btn" disabled={!media.hasNext()} onClick={() => void next()}>{t("media.next")}</button>
      </div>
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
  const [kind, setKind] = createSignal<MediaKind>("video");
  const [query, setQuery] = createSignal("");
  const [photo, setPhoto] = createSignal<number | null>(null);
  const [items] = createResource(kind, (k) => mediaApi.allItems(k));
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

  return (
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
        <label class="media-search">
          <SearchIcon />
          <input type="search" placeholder={t("media.search")} aria-label={t("media.search")} value={query()} onInput={(e) => setQuery(e.currentTarget.value)} />
        </label>
      </div>

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

      <MiniPlayer />
      <Show when={media.open() && media.current()}>
        <Player />
      </Show>
      <Show when={photo() !== null}>
        <PhotoViewer photos={filtered()} index={photo()!} onClose={() => setPhoto(null)} onIndex={setPhoto} />
      </Show>
    </section>
  );
}
