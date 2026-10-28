import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { instagramApi, type IgSaveItem } from "../api/instagram";
import { ApiError } from "../api/client";
import { t } from "../i18n";
import { formatTaken, type Slide } from "../lib/instagram";
import { BackIcon, CheckIcon, CloseIcon, DownloadIcon, ExternalIcon } from "./Icons";

// The full-screen viewer for stories, highlights, posts and reels: swipe (touch) or arrow keys,
// video playback, and "Save to Lucina" on every slide (POST /v1/instagram/save). It sits in a
// Portal on <body>: the page transition transforms <main>, which would break `position: fixed`.

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
}) {
  const [index, setIndex] = createSignal(Math.max(0, Math.min(props.start, props.slides.length - 1)));
  const [saves, setSaves] = createSignal<Record<string, SaveState>>({});
  const [toast, setToast] = createSignal("");
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let closeButton: HTMLButtonElement | undefined;
  const opener = typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null;

  const slide = createMemo(() => props.slides[index()]);
  const total = () => props.slides.length;
  const stateOf = (key: string): SaveState => saves()[key] ?? "idle";
  const siblings = createMemo(() => {
    const s = slide();
    return s ? props.slides.filter((x) => x.item.id === s.item.id) : [];
  });

  function move(delta: number) {
    setIndex((i) => Math.max(0, Math.min(total() - 1, i + delta)));
  }

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
    const items: IgSaveItem[] = fresh.map((s) => ({
      url: s.url,
      kind: s.kind,
      username: props.username,
      takenAt: s.item.takenAt ?? null,
      caption: s.item.caption ?? null,
    }));
    try {
      const result = await instagramApi.save(items);
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

  onMount(() => {
    closeButton?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.altKey || e.ctrlKey || e.metaKey) return;
      if (e.key === "ArrowRight") {
        e.preventDefault();
        move(1);
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        move(-1);
      } else if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      }
    };
    window.addEventListener("keydown", key);
    onCleanup(() => window.removeEventListener("keydown", key));
  });
  onCleanup(() => {
    clearTimeout(toastTimer);
    opener?.focus?.();
  });

  createEffect(on(index, (i) => total() - i <= 3 && props.onNearEnd?.()));

  let touch: { x: number; y: number } | undefined;
  const onTouchStart = (e: TouchEvent) => {
    const p = e.touches[0];
    touch = e.touches.length === 1 && p ? { x: p.clientX, y: p.clientY } : undefined;
  };
  const onTouchEnd = (e: TouchEvent) => {
    const p = e.changedTouches[0];
    if (touch && p) {
      const dx = p.clientX - touch.x;
      const dy = p.clientY - touch.y;
      if (Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(dy) * 1.5) move(dx < 0 ? 1 : -1);
      else if (dy > 90 && Math.abs(dy) > Math.abs(dx) * 1.5) props.onClose();
    }
    touch = undefined;
  };

  return (
    <Portal>
      <div class="ig-viewer" role="dialog" aria-modal="true" aria-label={t("instagram.viewer")} onTouchStart={onTouchStart} onTouchEnd={onTouchEnd} onTouchCancel={() => (touch = undefined)}>
        <header class="ig-viewer__bar">
          <button type="button" class="ig-viewer__icon" ref={closeButton} aria-label={t("instagram.close")} onClick={() => props.onClose()}>
            <CloseIcon />
          </button>
          <span class="ig-viewer__who">
            @{props.username}
            <Show when={formatTaken(slide()?.item.takenAt)}>{(d) => <small> · {d()}</small>}</Show>
          </span>
          <span class="ig-viewer__count" aria-live="polite">{t("instagram.position", { n: index() + 1, total: total() })}</span>
        </header>

        <div class="ig-viewer__stage">
          <button type="button" class="ig-viewer__nav ig-viewer__nav--prev" aria-label={t("instagram.previous")} disabled={index() === 0} onClick={() => move(-1)}>
            <BackIcon />
          </button>
          <Show keyed when={slide()}>
            {(s) =>
              s.kind === "video" ? (
                <video
                  class="ig-viewer__media"
                  src={s.url}
                  poster={s.thumbUrl ?? undefined}
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
                <img class="ig-viewer__media" src={s.url} alt={t("instagram.alt", { name: props.username })} referrerpolicy="no-referrer" />
              )
            }
          </Show>
          <button type="button" class="ig-viewer__nav ig-viewer__nav--next" aria-label={t("instagram.next")} disabled={index() >= total() - 1} onClick={() => move(1)}>
            <BackIcon />
          </button>
        </div>

        <Show when={siblings().length > 1}>
          <div class="ig-viewer__dots" aria-hidden="true">
            <For each={siblings()}>{(s) => <span classList={{ "ig-viewer__dot": true, "ig-viewer__dot--on": s.key === slide()?.key }} />}</For>
          </div>
        </Show>

        <footer class="ig-viewer__foot">
          <Show when={slide()?.item.caption}>{(c) => <p class="ig-viewer__caption">{c()}</p>}</Show>
          <div class="ig-viewer__actions">
            <Show when={props.archived ? undefined : slide()}>
              {(s) => (
                <button type="button" class="ig-btn ig-btn--primary" disabled={stateOf(s().key) === "saving"} onClick={() => void save([s()])}>
                  <Show when={stateOf(s().key) === "saved"} fallback={<DownloadIcon />}>
                    <CheckIcon />
                  </Show>
                  <span>{stateOf(s().key) === "saving" ? t("instagram.saving") : stateOf(s().key) === "saved" ? t("instagram.saved") : t("instagram.save")}</span>
                </button>
              )}
            </Show>
            <Show when={siblings().length > 1 && !props.archived}>
              <button type="button" class="ig-btn" disabled={siblings().some((s) => stateOf(s.key) === "saving")} onClick={() => void save(siblings())}>
                {t("instagram.saveAll", { count: siblings().length })}
              </button>
            </Show>
            <Show when={slide()?.item.permalink}>
              {(href) => (
                <a class="ig-btn" href={href()} target="_blank" rel="noopener noreferrer">
                  <ExternalIcon />
                  <span>{t("instagram.openOnInstagram")}</span>
                </a>
              )}
            </Show>
          </div>
        </footer>

        <div class="ig-toast" role="status" aria-live="polite">
          {toast()}
        </div>
      </div>
    </Portal>
  );
}
