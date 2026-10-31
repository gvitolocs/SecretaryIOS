import { createEffect, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { nclientApi, nclientImageUrl, type NClientDetail } from "../api/nclient";
import { t } from "../i18n";

/** How many pages after the current one are fetched ahead of time. */
export const PRELOAD_PAGES = 3;
const MIN_ZOOM = 50;
const MAX_ZOOM = 400;
const SWIPE_PX = 60;

const clampZoom = (z: number) => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, Math.round(z)));
const distance = (a: Touch, b: Touch) => Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);

/**
 * A mounted reading session. It owns only the pages the service returned (signed same-origin URLs,
 * `nclientImageUrl`); nothing about the gallery goes to storage or history.
 *
 * Input: swipe left/right (touch, only while not zoomed), arrow keys / Page Up / Page Down / Home / End,
 * pinch (touch) or Ctrl + wheel (trackpad pinch) or the zoom controls, Escape to close.
 */
export function NClientReader(props: {
  gallery: NClientDetail;
  startPage: number;
  close: () => void;
  /** A fresh detail request (new signed URLs) after an image failed. */
  refresh: () => void;
  onPage: (page: number) => void;
  favorite: boolean;
  favoriteBusy: boolean;
  toggleFavorite: () => void;
  /** The "Save to Lucina" control, owned by the view so a job survives leaving the reader. */
  saveControl: JSX.Element;
}) {
  const gallery = props.gallery;
  const last = () => gallery.pages.length - 1;
  const [page, setPage] = createSignal(Math.max(0, Math.min(last(), props.startPage)));
  // Phones read portrait pages by width; wider screens by height.
  const [fit, setFit] = createSignal<"height" | "width" | "original">(typeof window !== "undefined" && window.innerWidth < 700 ? "width" : "height");
  const [zoom, setZoom] = createSignal(100);
  const [saveError, setSaveError] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [imageError, setImageError] = createSignal(false);
  // The next pages are fetched only once the current one is on screen, so they never queue
  // ahead of it on the service's one-image-per-second line.
  const [shown, setShown] = createSignal(false);
  let viewport: HTMLDivElement | undefined;
  let saveGeneration = 0;
  let disposed = false;
  onCleanup(() => { disposed = true; });

  // The API serializes writes; only the latest page controls the save indicator.
  async function saveProgress(value: number) {
    const turn = ++saveGeneration;
    setSaved(false);
    try {
      await nclientApi.saveProgress(gallery.id, value, gallery.pages.length);
      if (!disposed && turn === saveGeneration) { setSaveError(false); setSaved(true); }
    } catch { if (!disposed && turn === saveGeneration) setSaveError(true); }
  }
  createEffect(on(page, value => {
    props.onPage(value);
    void saveProgress(value);
    setImageError(false);
    setShown(false);
    viewport?.scrollTo?.(0, 0);
  }));

  const move = (delta: number) => setPage(p => Math.max(0, Math.min(last(), p + delta)));
  const zoomBy = (delta: number) => setZoom(z => clampZoom(z + delta));

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.altKey || e.ctrlKey || e.metaKey || (e.target instanceof HTMLElement && e.target.closest("input,select,textarea,[contenteditable=true]"))) return;
      if (e.key === "ArrowRight" || e.key === "PageDown") { e.preventDefault(); move(1); }
      else if (e.key === "ArrowLeft" || e.key === "PageUp") { e.preventDefault(); move(-1); }
      else if (e.key === "Home") { e.preventDefault(); setPage(0); }
      else if (e.key === "End") { e.preventDefault(); setPage(last()); }
      else if (e.key === "+" || e.key === "=") zoomBy(25);
      else if (e.key === "-") zoomBy(-25);
      else if (e.key === "Escape") props.close();
    };
    window.addEventListener("keydown", key);
    onCleanup(() => window.removeEventListener("keydown", key));
    // Ctrl + wheel is a trackpad pinch (or a mouse wheel with Ctrl): zoom the page, not the browser.
    const wheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      zoomBy(e.deltaY < 0 ? 10 : -10);
    };
    viewport?.addEventListener("wheel", wheel, { passive: false });
    onCleanup(() => viewport?.removeEventListener("wheel", wheel));
  });

  // Touch: one finger that moves sideways turns the page (only at 100 % or less); two fingers pinch.
  let swipe: { x: number; y: number } | undefined;
  let pinch: { start: number; zoom: number } | undefined;
  const touches = {
    start(e: TouchEvent) {
      const [a, b] = [e.touches[0], e.touches[1]];
      if (a && b) { pinch = { start: distance(a, b), zoom: zoom() }; swipe = undefined; }
      else if (a) swipe = { x: a.clientX, y: a.clientY };
    },
    move(e: TouchEvent) {
      const [a, b] = [e.touches[0], e.touches[1]];
      if (pinch && a && b && pinch.start > 0) setZoom(clampZoom(pinch.zoom * (distance(a, b) / pinch.start)));
    },
    end(e: TouchEvent) {
      const p = e.changedTouches[0];
      if (swipe && !pinch && p && zoom() <= 100) {
        const dx = p.clientX - swipe.x; const dy = p.clientY - swipe.y;
        if (Math.abs(dx) > SWIPE_PX && Math.abs(dx) > Math.abs(dy) * 1.5) move(dx < 0 ? 1 : -1);
      }
      if (e.touches.length === 0) { swipe = undefined; pinch = undefined; }
    },
    cancel() { swipe = undefined; pinch = undefined; },
  };

  const preload = () => gallery.pages.slice(page() + 1, page() + 1 + PRELOAD_PAGES);

  return <section class="nclient-reader" aria-label={t("nclient.title")}>
    <div class="nclient-bar">
      <button type="button" onClick={props.close}>{t("nclient.close")}</button>
      <span class="nclient-bar-page" aria-live="polite">{t("nclient.page", { page: page() + 1, pages: gallery.pages.length })}</span>
      <button type="button" class="nclient-heart" aria-pressed={props.favorite} disabled={props.favoriteBusy} onClick={props.toggleFavorite}
        aria-label={props.favorite ? t("nclient.unfavorite") : t("nclient.favorite")} title={props.favorite ? t("nclient.unfavorite") : t("nclient.favorite")}>
        {props.favorite ? "♥" : "♡"}
      </button>
      {props.saveControl}
    </div>
    <div class="nclient-bar">
      <label>{t("nclient.fit")} <select value={fit()} onChange={e => setFit(e.currentTarget.value as ReturnType<typeof fit>)}>
        <option value="height">{t("nclient.height")}</option>
        <option value="width">{t("nclient.width")}</option>
        <option value="original">{t("nclient.original")}</option>
      </select></label>
      <div class="nclient-zoom">
        <button type="button" aria-label={t("nclient.zoomOut")} disabled={zoom() <= MIN_ZOOM} onClick={() => zoomBy(-25)}>−</button>
        <input aria-label={t("nclient.zoom")} type="range" min={MIN_ZOOM} max={MAX_ZOOM} step="25" value={zoom()} onInput={e => setZoom(clampZoom(Number(e.currentTarget.value)))} />
        <button type="button" aria-label={t("nclient.zoomIn")} disabled={zoom() >= MAX_ZOOM} onClick={() => zoomBy(25)}>+</button>
        <span class="nclient-zoom-value">{zoom()}%</span>
      </div>
    </div>
    <div class="nclient-viewport" ref={viewport}
      onTouchStart={touches.start} onTouchMove={touches.move} onTouchEnd={touches.end} onTouchCancel={touches.cancel}>
      <Show keyed when={gallery.pages[page()]}>{current =>
        <Show when={nclientImageUrl(current.url)} fallback={<p role="alert" class="nclient-note">{t("nclient.imageError")}</p>}>
          <img class={`nclient-page nclient-page--${fit()}`}
            style={{ "--nc-zoom": String(zoom() / 100), "--nc-width": `${current.width}px` }}
            src={nclientImageUrl(current.url)} alt={t("nclient.pageOf", { page: page() + 1, pages: gallery.pages.length })}
            referrerpolicy="no-referrer" draggable={false}
            onLoad={() => setShown(true)} onError={() => setImageError(true)} />
        </Show>
      }</Show>
    </div>
    <Show when={imageError()}><p role="alert" class="nclient-note">{t("nclient.imageError")} <button type="button" onClick={props.refresh}>{t("nclient.refresh")}</button></p></Show>
    <div class="nclient-bar nclient-bar--nav">
      <button type="button" disabled={page() === 0} onClick={() => move(-1)}>{t("nclient.previous")}</button>
      <label>{t("nclient.readingPage")} <input aria-label={t("nclient.readingPage")} type="number" min="1" max={gallery.pages.length} value={page() + 1} onChange={e => {
        const value = Number(e.currentTarget.value);
        if (Number.isInteger(value) && value >= 1 && value <= gallery.pages.length) setPage(value - 1);
      }} /></label>
      <button type="button" disabled={page() >= last()} onClick={() => move(1)}>{t("nclient.next")}</button>
    </div>
    <Show when={saveError()}><p role="alert" class="nclient-note">{t("nclient.progressError")} <button type="button" onClick={() => void saveProgress(page())}>{t("nclient.retry")}</button></p></Show>
    <Show when={saved() && !saveError()}><small role="status" class="nclient-note">{t("nclient.progressSaved")}</small></Show>
    <div hidden aria-hidden="true" data-nclient-preload>
      <Show when={shown()}>
        <For each={preload()}>{next =>
          <Show when={nclientImageUrl(next.url)}>{url => <img src={url()} alt="" referrerpolicy="no-referrer" loading="eager" />}</Show>
        }</For>
      </Show>
    </div>
  </section>;
}
