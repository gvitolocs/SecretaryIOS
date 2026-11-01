import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { getMediaItem, playMedia, type MediaItem } from "../api/media";
import type { Attachment } from "../api/timeline";
import { t } from "../i18n";
import { albumCellStyle, albumLayout, ALBUM_MAX } from "../lib/album";
import { formatBytes, formatCoordinate, formatDuration } from "../lib/attachments";
import { chooseGridUrl, choosePreviewUrl, mediaOriginalUrl, supportsJxl } from "../lib/jxl";
import { safeHref } from "../lib/linkify";
import { hasPoint, placeFromMapsLink, type PlaceInfo } from "../lib/mapKit";
import { linkHost } from "../lib/share";
import { thumbHashToImage } from "../lib/thumbhash";
import { trayIcon } from "./AttachmentTray";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";
import { PlaceCard } from "./PlaceCard";
import "../styles/share.css";

const LABELS: Record<Attachment["kind"], "image" | "video" | "audio" | "pdf" | "document" | "file" | "link" | "text" | "contact" | "location" | "voice"> = {
  image: "image",
  video: "video",
  voice: "voice",
  audio: "audio",
  pdf: "pdf",
  document: "document",
  file: "file",
  link: "link",
  text: "text",
  contact: "contact",
  location: "location",
};

function labelFor(a: Attachment): string {
  if (a.name) return a.name;
  const key = LABELS[a.kind];
  return key === "image" ? t("chat.photo") : key === "voice" ? t("chat.voice") : t(`share.${key}`);
}

function kindLabel(a: Attachment): string {
  const key = LABELS[a.kind];
  return key === "image" ? t("chat.photo")
    : key === "voice" ? t("chat.voice")
    : t(`share.${key}`);
}

/** The server's media item JSON, fetched only when the attachment lacks what we need. */
function useMediaItem(item: () => Attachment, needed: () => boolean) {
  const [info, setInfo] = createSignal<MediaItem | null>(null);
  createEffect(() => {
    const id = item().mediaId;
    if (!id || !needed()) return;
    let live = true;
    void getMediaItem(id).then((value) => {
      if (live) setInfo(value);
    });
    onCleanup(() => {
      live = false;
    });
  });
  return info;
}

function MediaThumb(props: { item: Attachment; alt: string }) {
  // null until the one-time JPEG XL feature test answers: never fetch both renditions.
  const [jxl, setJxl] = createSignal<boolean | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  onMount(() => {
    void supportsJxl().then(setJxl);
  });
  // ThumbHash and size come with the timeline attachment; older items ask the media item.
  const info = useMediaItem(() => props.item, () => !props.item.previewUrl && !props.item.thumbhash);
  const decoded = createMemo(() => thumbHashToImage(props.item.thumbhash ?? info()?.thumbhash));
  const placeholder = () => (loaded() ? null : decoded());
  const src = () => {
    if (props.item.previewUrl) return props.item.previewUrl;
    const supported = jxl();
    if (!props.item.mediaId || supported === null) return null;
    return chooseGridUrl(props.item.mediaId, supported);
  };
  const size = createMemo((): [number, number] => {
    const width = props.item.width ?? info()?.width;
    const height = props.item.height ?? info()?.height;
    if (width && height) return [width, height];
    const hash = decoded();
    if (hash) return [hash.width, hash.height];
    return [4, 3];
  });
  return (
    <div
      class="media-frame"
      style={{ "aspect-ratio": `${size()[0]} / ${size()[1]}`, "--media-ar": String(size()[0] / size()[1]) }}
    >
      <Show when={placeholder()}>
        {(hash) => <img class="media-thumbhash" src={hash().url} alt="" aria-hidden="true" draggable="false" />}
      </Show>
      <Show when={src()}>
        {(url) => (
          <Show
            when={props.item.kind === "video" && props.item.previewUrl}
            fallback={
              <img
                class="media-img"
                classList={{ "is-loaded": loaded() }}
                src={url()}
                alt={props.alt}
                loading="lazy"
                draggable="false"
                onLoad={() => setLoaded(true)}
                onError={() => setLoaded(true)}
              />
            }
          >
            {/* Just-picked video: its first frame from the local blob (an <img> cannot decode it). */}
            <video class="media-img is-loaded" src={url()} muted playsinline preload="metadata" aria-label={props.alt} />
          </Show>
        )}
      </Show>
    </div>
  );
}

function ImageAlbum(props: { items: Attachment[]; onOpen: (item: Attachment) => void }) {
  const layout = () => albumLayout(props.items.length);
  return (
    <ul class={`attachments attachments--album attachments--n${Math.min(props.items.length, ALBUM_MAX)}`} aria-label={t("chat.attachments")}>
      <For each={props.items}>{(item, index) => (
        <li class="attachment attachment--media" style={albumCellStyle(layout().cells[index()]!)}>
          <button
            type="button"
            class="media-button"
            aria-label={t("share.openImage", { name: labelFor(item) })}
            onClick={() => props.onOpen(item)}
          >
            <MediaThumb item={item} alt={labelFor(item)} />
          </button>
        </li>
      )}</For>
    </ul>
  );
}

function VideoCard(props: { item: Attachment }) {
  const [url, setUrl] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  async function play() {
    if (url() || busy()) return;
    setBusy(true);
    setFailed(false);
    try {
      const id = props.item.mediaId;
      const next = id ? await playMedia(id) : safeHref(props.item.url ?? "");
      if (!next) throw new Error("no url");
      setUrl(next);
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  }
  return (
    <li class="attachment attachment--video">
      <Show
        when={url()}
        fallback={
          <button
            type="button"
            class="media-button video-poster"
            aria-label={t("share.playVideo", { name: labelFor(props.item) })}
            onClick={() => void play()}
            disabled={busy()}
          >
            <MediaThumb item={props.item} alt={labelFor(props.item)} />
            <span class="media-play" aria-hidden="true">▶</span>
            <Show when={props.item.durationMs}>{(ms) => <span class="media-duration">{formatDuration(ms())}</span>}</Show>
            <Show when={failed()}><span class="media-failed" role="alert">{t("share.playFailed")}</span></Show>
          </button>
        }
      >
        {(src) => <video class="media-video" src={src()} controls autoplay playsinline preload="metadata" />}
      </Show>
      <span class="attachment-caption">
        <span aria-hidden="true">{trayIcon("video")} </span>
        {labelFor(props.item)}
        <Show when={props.item.bytes !== undefined}><small> · {formatBytes(props.item.bytes)}</small></Show>
      </span>
    </li>
  );
}

function AudioCard(props: { item: Attachment }) {
  const [url, setUrl] = createSignal<string | null>(null);
  const [playing, setPlaying] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [current, setCurrent] = createSignal(0);
  const [duration, setDuration] = createSignal((props.item.durationMs ?? 0) / 1000);
  let audio: HTMLAudioElement | undefined;
  async function toggle() {
    if (!url()) {
      setBusy(true);
      try {
        const id = props.item.mediaId;
        const next = id ? await playMedia(id) : safeHref(props.item.url ?? "");
        if (!next) return;
        setUrl(next);
        queueMicrotask(() => void audio?.play().catch(() => setPlaying(false)));
      } catch {
        // Leave the player paused; pressing play again retries.
      } finally {
        setBusy(false);
      }
      return;
    }
    if (audio?.paused) await audio.play().catch(() => undefined);
    else audio?.pause();
  }
  const progress = () => (duration() > 0 ? Math.min(1, current() / duration()) : 0);
  return (
    <li class="attachment attachment--audio">
      <div class="audio-player">
        <button type="button" class="audio-toggle" disabled={busy()} aria-label={playing() ? t("share.pause") : t("share.play")} onClick={() => void toggle()}>
          {playing() ? "❚❚" : "▶"}
        </button>
        <span class="audio-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(progress() * 100)}>
          <span style={{ width: `${progress() * 100}%` }} />
        </span>
        <span class="audio-time">{formatDuration((current() || duration()) * 1000)}</span>
        <Show when={url()}>
          {(src) => (
            <audio
              ref={audio}
              src={src()}
              preload="metadata"
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onEnded={() => setPlaying(false)}
              onTimeUpdate={(e) => setCurrent(e.currentTarget.currentTime)}
              onLoadedMetadata={(e) => {
                if (Number.isFinite(e.currentTarget.duration)) setDuration(e.currentTarget.duration);
              }}
            />
          )}
        </Show>
      </div>
      <span class="attachment-caption">
        <span aria-hidden="true">{trayIcon("audio")} </span>
        {labelFor(props.item)}
      </span>
    </li>
  );
}

function FileCard(props: { item: Attachment }) {
  const href = () => (props.item.mediaId ? mediaOriginalUrl(props.item.mediaId) : safeHref(props.item.url ?? ""));
  const content = () => (
    <>
      <span class="file-icon" aria-hidden="true">{trayIcon(props.item.kind)}</span>
      <span class="file-body">
        <span class="file-name">{labelFor(props.item)}</span>
        <small>
          {kindLabel(props.item)}
          <Show when={props.item.bytes !== undefined}> · {formatBytes(props.item.bytes)}</Show>
        </small>
      </span>
      <span class="file-download" aria-hidden="true">↓</span>
    </>
  );
  return (
    <li class={`attachment attachment--${props.item.kind}`}>
      <Show when={href()} fallback={<div class="file-card">{content()}</div>}>
        {(url) => (
          <a class="file-card" href={url()} download={props.item.name} target="_blank" rel="noopener noreferrer" aria-label={t("share.download", { name: labelFor(props.item) })}>
            {content()}
          </a>
        )}
      </Show>
    </li>
  );
}

function LinkCard(props: { item: Attachment }) {
  const href = () => safeHref(props.item.url ?? "");
  const title = () => props.item.name || (props.item.url ? linkHost(props.item.url) : t("share.link"));
  return (
    <li class="attachment attachment--link">
      <Show when={href()} fallback={<div class="link-card"><span class="link-title">{title()}</span></div>}>
        {(url) => (
          <a class="link-card" href={url()} target="_blank" rel="noopener noreferrer">
            <span class="link-title">{title()}</span>
            <small>{linkHost(props.item.url ?? url())}</small>
          </a>
        )}
      </Show>
    </li>
  );
}

/** A shared location, or a shared Apple/Google Maps or `geo:` link, as a place. */
export function attachmentPlace(item: Attachment): PlaceInfo | null {
  if (item.kind === "location") {
    const point = { lat: item.lat, lng: item.lon };
    if (!hasPoint(point)) return null;
    return { title: item.name || `${formatCoordinate(point.lat)}, ${formatCoordinate(point.lng)}`, ...point, kind: "place" };
  }
  if (item.kind === "link" && item.url) {
    const place = placeFromMapsLink(item.url);
    if (!place) return null;
    // a link's own title (e.g. "Folkehuset") beats the bare coordinates
    const title = item.name && !/^https?:|^geo:/i.test(item.name) && !place.address ? item.name : place.title;
    return { ...place, title, kind: "place" };
  }
  return null;
}

function LocationCard(props: { item: Attachment }) {
  const title = () => props.item.name || t("share.location");
  return (
    <li class="attachment attachment--location">
      <div class="location-card">
        <span class="location-pin" aria-hidden="true">{trayIcon("location")}</span>
        <span class="location-body">
          <span class="location-name">{title()}</span>
        </span>
      </div>
    </li>
  );
}

/** The chat's images in the gallery's photo viewer (PhotoViewer). */
function Lightbox(props: { items: Attachment[]; index: number; onIndex: (index: number) => void; onClose: () => void }) {
  const current = () => props.items[props.index];
  const info = useMediaItem(() => current() ?? { kind: "image" }, () => !current()?.previewUrl);
  const previewOf = (item: Attachment, isCurrent: boolean) => {
    if (item.previewUrl) return item.previewUrl;
    if (!item.mediaId) return safeHref(item.url ?? "");
    const loaded = isCurrent ? info() : undefined;
    return choosePreviewUrl(item.mediaId, loaded?.id === item.mediaId ? loaded.urls?.preview : undefined);
  };
  const items = createMemo<ViewerItem[]>(() =>
    props.items.map((item, i) => ({ src: previewOf(item, i === props.index), name: labelFor(item), mime: item.mime })),
  );
  return (
    <PhotoViewer
      items={items()}
      index={props.index}
      onIndex={props.onIndex}
      onClose={props.onClose}
      wrap
      label={(index, count) => t("share.lightbox", { index, count })}
    />
  );
}

/**
 * The timeline's attachment bubble: image albums with ThumbHash placeholders,
 * inline video/audio, file cards, link cards and shared locations.
 */
export function Attachments(props: { items: Attachment[] }) {
  const [lightbox, setLightbox] = createSignal<number | null>(null);
  const images = createMemo(() => props.items.filter((a) => a.kind === "image"));
  const others = createMemo(() => props.items.filter((a) => a.kind !== "image"));
  const chunks = createMemo(() => {
    const list = images();
    const out: Attachment[][] = [];
    for (let i = 0; i < list.length; i += ALBUM_MAX) out.push(list.slice(i, i + ALBUM_MAX));
    return out;
  });
  const openImage = (item: Attachment) => {
    const index = images().indexOf(item);
    if (index >= 0) setLightbox(index);
  };
  return (
    <>
      <For each={chunks()}>{(chunk) => <ImageAlbum items={chunk} onOpen={openImage} />}</For>
      <Show when={others().length}>
        <ul class="attachments attachments--list" aria-label={t("chat.attachments")}>
          <For each={others()}>{(item) => {
            if (item.kind === "video") return <VideoCard item={item} />;
            if (item.kind === "audio" || item.kind === "voice") return <AudioCard item={item} />;
            const place = attachmentPlace(item);
            if (place) {
              return (
                <li class="attachment attachment--place">
                  <PlaceCard place={place} />
                </li>
              );
            }
            if (item.kind === "link") return <LinkCard item={item} />;
            if (item.kind === "location") return <LocationCard item={item} />;
            return <FileCard item={item} />;
          }}</For>
        </ul>
      </Show>
      {/* Index 0 is a valid image: test for null, not truthiness. */}
      <Show when={lightbox() !== null}>
        <Lightbox
          items={images()}
          index={lightbox() ?? 0}
          onIndex={setLightbox}
          onClose={() => setLightbox(null)}
        />
      </Show>
    </>
  );
}
