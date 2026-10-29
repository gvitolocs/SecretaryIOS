import { createMemo, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import type { GalleryItem } from "../api/gallery";
import { t } from "../i18n";
import type { Slide } from "../lib/instagram";
import { BackIcon } from "./Icons";
import { GridView } from "./gallery/GridView";
import "../styles/gallery.css";
import "../styles/instagram.css";

// A highlight, story or feed as tiles: the Gallery's own grid (GridView: day headers, video badges, lazy pictures,
// arrow keys between tiles) over the slides, without its selection. A tile opens the viewer at that slide; G opens it at
// the focused tile and Esc closes the tiles.

/** A slide as the Gallery grid's item: only what the grid draws and lays out. */
export function tileItem(slide: Slide): GalleryItem {
  const taken = slide.item.takenAt ? new Date(slide.item.takenAt * 1000).toISOString() : null;
  const thumb = slide.thumbUrl ?? (slide.kind === "video" ? undefined : slide.url);
  return {
    id: slide.key,
    kind: slide.kind === "video" ? "video" : "photo",
    title: "",
    caption: slide.item.caption ?? null,
    mime: slide.kind === "video" ? "video/mp4" : "image/jpeg",
    size: 0,
    takenAt: taken,
    addedAt: taken ?? "",
    // No size: the grid lays every tile out square, like the Instagram grid.
    width: null,
    height: null,
    durationMs: null,
    place: null,
    camera: null,
    source: "instagram",
    device: null,
    deviceName: null,
    origin: { filename: null, folder: null, importedFrom: "instagram" },
    favorite: false,
    rotation: 0,
    edited: false,
    albums: [],
    trashedAt: null,
    trashExpiresAt: null,
    thumbhash: null,
    livePair: null,
    urls: thumb ? { thumb } : {},
  };
}

const none = new Set<string>();

export default function InstagramTiles(props: {
  slides: Slide[];
  username: string;
  /** The highlight's title, when it is one. */
  title?: string;
  /** The tile to focus (the slide the viewer was on). */
  focus: number;
  hasMore?: boolean;
  loading?: boolean;
  onOpen: (index: number) => void;
  onClose: () => void;
  onNearEnd?: () => void;
}) {
  let panel: HTMLDivElement | undefined;
  const items = createMemo(() => props.slides.map((s) => tileItem(s)));
  const dated = () => props.slides.some((s) => !!s.item.takenAt);

  /** The focused tile, else the one the viewer was on. */
  function current(): number {
    const tile = (document.activeElement as Element | null)?.closest?.<HTMLElement>(".g-tile");
    const i = tile && panel?.contains(tile) ? Number(tile.dataset.index) : props.focus;
    return Number.isFinite(i) ? Math.max(0, Math.min(i, props.slides.length - 1)) : 0;
  }

  onMount(() => {
    if (!panel?.contains(document.activeElement)) panel?.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as Element | null)?.closest?.("input, textarea, select")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      } else if ((e.key === "g" || e.key === "G") && !e.metaKey && !e.ctrlKey && !e.altKey && props.slides.length > 0) {
        e.preventDefault();
        props.onOpen(current());
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <Portal>
      <div
        ref={panel}
        class="gallery ig-tiles"
        role="dialog"
        aria-modal="true"
        aria-label={t("instagram.tilesLabel", { name: props.title ?? `@${props.username}` })}
        tabindex="-1"
      >
        <div class="g-top">
          <div class="g-bar">
            <button type="button" class="g-back" aria-label={t("instagram.closeTiles")} onClick={() => props.onClose()}>
              <BackIcon />
            </button>
            <div class="ig-tiles__title">
              <strong>{props.title ?? `@${props.username}`}</strong>
              <small>
                {props.title ? `@${props.username} · ` : ""}
                {t("instagram.items", { count: props.slides.length })}
              </small>
            </div>
          </div>
        </div>
        <div class="g-main">
          <GridView
            items={items()}
            epoch={0}
            resetKey="instagram"
            groupBy={dated() ? "day" : "none"}
            selected={none}
            jxl={false}
            showSource={false}
            sourceKind={() => "other"}
            loading={!!props.loading}
            hasMore={!!props.hasMore}
            failed={false}
            months={[]}
            scrubber={false}
            focusIndex={props.focus}
            onNeedMore={() => props.onNearEnd?.()}
            onRetry={() => props.onNearEnd?.()}
            onSeek={async () => {}}
            onTile={(click) => props.onOpen(click.index)}
            onToggle={(index) => props.onOpen(index)}
            onLongPress={() => {}}
            onSection={() => {}}
            onBand={() => {}}
            onCommand={() => {}}
          />
        </div>
      </div>
    </Portal>
  );
}
