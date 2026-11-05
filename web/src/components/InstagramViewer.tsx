import { createEffect, createMemo, createSignal, For, on, onCleanup, Show, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import { instagramApi, type IgSaveItem } from "../api/instagram";
import { ApiError } from "../api/client";
import { t } from "../i18n";
import { formatTaken, type Slide } from "../lib/instagram";
import { freshUrl, isExpired } from "../lib/instagramImages";
import { CheckIcon, DownloadIcon, ExternalIcon, GridIcon } from "./Icons";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

// The full-size viewer for stories, highlights, posts and reels: the Gallery's PhotoViewer (pinch,
// ⌘/ctrl + wheel, double tap or click, drag while zoomed, swipe, arrows, Esc, full screen) with
// "Save to Lucina" on every slide (POST /v1/instagram/save) and the post's caption in its panel.
//
// Stories and highlights (`story`) keep the whole viewer for the picture (owner, 2026-11-01: "I like the gallery views
// more"): no panel under it; who, when and the caption are an overlay that comes and goes with the controls on a tap.
// Everything else is the Gallery viewer's own (zoom, pinch, swipe, Full screen = the shell's full tab).

type SaveState = "idle" | "saving" | "saved";

function reasonOf(code: string): string {
  if (code === "expired") return t("instagram.reasonExpired");
  if (code === "rate_limited") return t("instagram.reasonBusy");
  return t("instagram.reasonGeneric");
}

export default function InstagramViewer(props: {
  slides: Slide[];
  start: number;
  username: string;
  /** Items already in the Gallery (the Archive): no save buttons. */
  archived?: boolean;
  onClose: () => void;
  /** Called when the viewer gets near the end of the slides, so the page can load more. */
  onNearEnd?: () => void;
  /** The slide shown changed (so the tiles can come back on it). */
  onIndex?: (index: number) => void;
  /** Back to the tiles (the top bar's grid button and G); without it there is no grid button. */
  onGrid?: () => void;
  /** Stories and highlights: no panel, the info as an overlay with the controls. */
  story?: boolean;
}) {
  const [index, setIndex] = createSignal(Math.max(0, Math.min(props.start, props.slides.length - 1)));
  const [saves, setSaves] = createSignal<Record<string, SaveState>>({});
  const [toast, setToast] = createSignal("");
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  const slide = createMemo(() => props.slides[index()]);
  const total = () => props.slides.length;
  const stateOf = (key: string): SaveState => saves()[key] ?? "idle";
  const siblings = createMemo(() => {
    const s = slide();
    return s ? props.slides.filter((x) => x.item.id === s.item.id) : [];
  });
  // Links re-signed since these slides came (a copy from an earlier visit, a page open for over an hour).
  const [signed, setSigned] = createSignal<Record<string, string>>({});
  const live = (url: string | null): string | null => (url ? (signed()[url] ?? url) : url);
  const items = createMemo<ViewerItem[]>(() =>
    props.slides.map((s) => ({
      src: live(s.url),
      kind: s.kind === "video" ? "video" : "photo",
      poster: live(s.thumbUrl),
      name: `@${props.username}`,
      id: s.key,
    })),
  );
  // The slide on screen and its neighbours are never shown on an expired link.
  createEffect(() => {
    const i = index();
    for (const s of props.slides.slice(Math.max(0, i - 1), i + 2)) {
      for (const url of [s.url, s.thumbUrl]) {
        if (!url || !isExpired(url) || untrack(signed)[url]) continue;
        void freshUrl(url).then((fresh) => setSigned((m) => ({ ...m, [url]: fresh })));
      }
    }
  });

  function say(message: string) {
    setToast(message);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => setToast(""), 3500);
  }

  async function save(list: Slide[]) {
    const fresh = list.filter((s) => stateOf(s.key) !== "saving");
    if (fresh.length === 0) return;
    const mark = (state: SaveState, only?: Slide[]) =>
      setSaves((m) => ({ ...m, ...Object.fromEntries((only ?? fresh).map((s) => [s.key, state])) }));
    mark("saving");
    const body: IgSaveItem[] = fresh.map((s) => ({
      url: s.url,
      kind: s.kind,
      username: props.username,
      takenAt: s.item.takenAt ?? null,
      caption: s.item.caption ?? null,
    }));
    try {
      const result = await instagramApi.save(body);
      const savedAt = new Set(result.saved.map((r) => r.index));
      mark("saved", fresh.filter((_, i) => savedAt.has(i)));
      mark("idle", fresh.filter((_, i) => !savedAt.has(i)));
      const failed = result.failed[0];
      if (failed) {
        say(t("instagram.saveFailed", { reason: reasonOf(failed.error) }));
      } else if (result.saved.length > 0 && result.saved.every((r) => r.duplicate)) {
        say(t("instagram.duplicate"));
      } else if (result.saved.length > 1) {
        say(t("instagram.savedMany", { count: result.saved.length }));
      } else {
        say(t("instagram.saved"));
      }
    } catch (e) {
      mark("idle");
      say(t("instagram.saveFailed", { reason: e instanceof ApiError && e.status === 429 ? t("instagram.reasonBusy") : t("instagram.reasonGeneric") }));
    }
  }

  onCleanup(() => clearTimeout(toastTimer));

  createEffect(on(index, (i) => total() - i <= 3 && props.onNearEnd?.()));
  createEffect(on(index, (i) => props.onIndex?.(i), { defer: true }));

  function onKeyDown(e: KeyboardEvent): boolean {
    if ((e.key !== "g" && e.key !== "G") || !props.onGrid || e.metaKey || e.ctrlKey || e.altKey) return false;
    if ((e.target as Element | null)?.closest?.("input, textarea, select")) return false;
    e.preventDefault();
    props.onGrid();
    return true;
  }

  const Toolbar = () => (
    <>
      <Show when={props.onGrid}>
        <button type="button" class="pv-btn ig-pv-grid" aria-label={t("instagram.showTiles")} title={t("instagram.showTiles")} onClick={() => props.onGrid?.()}>
          <GridIcon />
        </button>
      </Show>
      <Show when={formatTaken(slide()?.item.takenAt)}>{(d) => <span class="ig-pv-date">{d()}</span>}</Show>
      <Show when={slide()?.item.permalink}>
        {(href) => (
          <a class="pv-btn" href={href()} target="_blank" rel="noopener noreferrer" aria-label={t("instagram.openOnInstagram")} title={t("instagram.openOnInstagram")}>
            <ExternalIcon />
          </a>
        )}
      </Show>
      <Show when={siblings().length > 1 && !props.archived}>
        <button type="button" class="pv-btn pv-text" disabled={siblings().some((s) => stateOf(s.key) === "saving")} onClick={() => void save(siblings())}>
          {t("instagram.saveAll", { count: siblings().length })}
        </button>
      </Show>
      <Show when={props.archived ? undefined : slide()}>
        {(s) => (
          <button
            type="button"
            class="pv-btn pv-text ig-pv-save"
            classList={{ "pv-save--done": stateOf(s().key) === "saved" }}
            disabled={stateOf(s().key) === "saving"}
            onClick={() => void save([s()])}
          >
            <Show when={stateOf(s().key) === "saved"} fallback={<DownloadIcon />}>
              <CheckIcon />
            </Show>
            <span>{stateOf(s().key) === "saving" ? t("instagram.saving") : stateOf(s().key) === "saved" ? t("instagram.saved") : t("instagram.save")}</span>
          </button>
        )}
      </Show>
    </>
  );

  // The caption and a carousel's frames, beside the picture (a short sheet on a phone).
  const hasPanel = () => !!slide()?.item.caption || siblings().length > 1;
  const Panel = () => (
    <div class="ig-pv-panel">
      <p class="ig-pv-who">
        @{props.username}
        <Show when={formatTaken(slide()?.item.takenAt)}>{(d) => <small> · {d()}</small>}</Show>
      </p>
      <Show when={siblings().length > 1}>
        <div class="ig-pv-dots" aria-hidden="true">
          <For each={siblings()}>{(s) => <span classList={{ "ig-pv-dot": true, "ig-pv-dot--on": s.key === slide()?.key }} />}</For>
        </div>
      </Show>
      <Show when={slide()?.item.caption}>{(c) => <p class="ig-pv-caption">{c()}</p>}</Show>
    </div>
  );

  // A story's info, over the picture with the controls: who and when, the caption.
  const StoryOverlay = () => (
    <div class="ig-story">
      <div class="ig-story__info">
        <p class="ig-pv-who">
          @{props.username}
          <Show when={formatTaken(slide()?.item.takenAt)}>{(d) => <small> · {d()}</small>}</Show>
        </p>
        <Show when={slide()?.item.caption}>{(c) => <p class="ig-pv-caption">{c()}</p>}</Show>
      </div>
    </div>
  );

  return (
    <>
      <PhotoViewer
        class={props.story ? "ig-story-viewer" : undefined}
        overlay={props.story ? <StoryOverlay /> : undefined}
        items={items()}
        index={index()}
        onIndex={setIndex}
        onClose={() => props.onClose()}
        label={(n, count) => t("instagram.position", { n, total: count })}
        onKeyDown={onKeyDown}
        toolbar={<Toolbar />}
        panel={!props.story && hasPanel() ? <Panel /> : undefined}
        renderVideo={({ item, current }) =>
          current ? (
            <video
              class="pv-video"
              src={item.src ?? undefined}
              poster={item.poster ?? undefined}
              controls
              autoplay
              loop
              playsinline
              preload="auto"
              ref={(el) => {
                el.muted = true;
              }}
            />
          ) : (
            <Show when={item.poster}>{(poster) => <img class="pv-img" src={poster()} alt="" draggable={false} />}</Show>
          )
        }
      />
      <Portal>
        <div class="ig-toast" role="status" aria-live="polite">
          {toast()}
        </div>
      </Portal>
    </>
  );
}
