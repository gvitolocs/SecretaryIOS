import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { isAdultOff, pixivApi, type PixivIllust, type PixivItem } from "../../api/pixiv";
import { t } from "../../i18n";
import { showAdult } from "../../lib/adult";
import { createCached } from "../../lib/swr";
import { BackIcon, BookmarkIcon, CloseIcon, DownloadIcon, SearchIcon } from "../Icons";
import { clampPan, clampZoom, fitScale, isTall, MAX_ZOOM, pannable, stepZoom, topPan, wheelFactor, widthScale, zoomAround, zoomLabel, type Pan, type Size } from "./zoom";

// The Pixiv work viewer: every page of a work, one at a time. Swipe or the arrow keys turn pages; the
// next page is fetched in advance. A page opens in Fit (the whole image, letterboxed; zoom.ts) and re-fits when the
// window changes. The zoom goes from Fit to 400 % of the image's pixels: the buttons and +/- keys, a double click
// (Fit <-> 100 %), ⌘/ctrl + scroll and a pinch zoom around the cursor; a zoomed page pans by dragging or scrolling.
// Tall pages offer "Fit width", which scrolls vertically. It also bookmarks the work, saves it (or one page) to
// Lucina's Gallery, and lists its tags (a tag opens a search). Escape or the close button leaves; a zoomed page
// goes back to Fit first.

const SWIPE_PX = 50;

export interface ReaderProps {
  id: string;
  /** What the list already knew: shown while the work loads. */
  preview?: PixivItem;
  onClose: () => void;
  onTag: (tag: string) => void;
  onBookmark: (id: string, on: boolean) => void;
}

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

export function Reader(props: ReaderProps) {
  // The work opens on its copy from an earlier visit while it is asked again (lib/swr); the key carries the adult
  // switch, so an R-18 copy never shows with it off.
  const cached = createCached<PixivIllust>("pixiv", () => `pixiv:illust:${props.id}:${showAdult() ? 1 : 0}`, () => pixivApi.illust(props.id));
  const work = cached.value;
  const loadError = cached.error;
  const [index, setIndex] = createSignal(0);
  // The shown image's own pixels and the stage's size; null until known (the image then fills the stage, contained).
  const [natural, setNatural] = createSignal<Size | null>(null);
  const [view, setView] = createSignal<Size | null>(null);
  const [mode, setMode] = createSignal<"fit" | "width" | "free">("fit");
  const [free, setFree] = createSignal(1);
  const [tx, setTx] = createSignal(0);
  const [ty, setTy] = createSignal(0);
  const [details, setDetails] = createSignal(false);
  const [saveMenu, setSaveMenu] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [bookmarked, setBookmarked] = createSignal<boolean | null>(null);
  let stage: HTMLDivElement | undefined;
  let closeButton: HTMLButtonElement | undefined;

  const pages = () => work()?.pages ?? [];
  const total = () => work()?.pageCount ?? props.preview?.pages ?? 1;
  const current = () => pages()[index()];
  const isBookmarked = () => bookmarked() ?? work()?.bookmarked ?? props.preview?.bookmarked ?? false;
  const title = () => work()?.title ?? props.preview?.title ?? "";
  const author = () => work()?.author ?? props.preview?.author ?? "";
  const src = () => current()?.url || props.preview?.thumbUrl || "";
  // The list's small thumbnail stands in while the page loads: it may be enlarged to fill the stage.
  const fit = () => fitScale(natural(), view(), current()?.url ? 1 : Number.POSITIVE_INFINITY);
  const scale = () => (mode() === "fit" ? fit() : mode() === "width" ? widthScale(natural(), view()) : free());
  const atFit = () => mode() === "fit" || scale() <= fit() + 1e-9;
  const canPan = () => pannable(natural(), view(), scale());
  const tall = () => !!current()?.url && isTall(natural(), view());
  const failedAdult = createMemo(() => !!loadError() && isAdultOff(loadError()));

  // A new work starts at its first page.
  createEffect(on(() => props.id, () => { setIndex(0); setBookmarked(null); setMessage(""); setNatural(null); resetZoom(); }, { defer: true }));

  // The next page (and the previous one) are fetched before they are asked for.
  createEffect(() => {
    const ps = pages();
    for (const i of [index() + 1, index() - 1]) {
      const url = ps[i]?.url;
      if (url) new Image().src = url;
    }
  });

  function resetZoom() {
    setMode("fit");
    setTx(0);
    setTy(0);
  }

  function go(to: number) {
    const n = clamp(to, 0, Math.max(0, total() - 1));
    if (n === index()) return;
    setIndex(n);
    setNatural(null);
    resetZoom();
  }

  /** Move the image, kept inside its limits at the current scale. */
  function setPan(p: Pan) {
    const c = clampPan(p, natural(), view(), scale());
    setTx(c.x);
    setTy(c.y);
  }

  /** Zoom to `next` (clamped to Fit..400 %), keeping the image point under `at` (stage-centre coordinates) in place. */
  function zoomTo(next: number, at?: Pan) {
    const from = scale();
    const s = clampZoom(next, fit());
    if (s <= fit() + 1e-9) return resetZoom();
    const pan = at ? zoomAround({ x: tx(), y: ty() }, at, from, s) : { x: (tx() * s) / from, y: (ty() * s) / from };
    setFree(s);
    setMode("free");
    setPan(pan);
  }

  function toggleFitWidth() {
    if (mode() === "width") return resetZoom();
    setMode("width");
    const top = topPan(natural(), view(), scale());
    setTx(top.x);
    setTy(top.y);
  }

  /** A pointer's position relative to the stage centre. */
  function at(e: { clientX: number; clientY: number }): Pan {
    const r = stage?.getBoundingClientRect();
    if (!r || !r.width) return { x: 0, y: 0 };
    return { x: e.clientX - r.left - r.width / 2, y: e.clientY - r.top - r.height / 2 };
  }

  // The stage's size: Fit and Fit width follow it (window resize, full screen, the details panel).
  function measure() {
    if (!stage) return;
    const w = stage.clientWidth;
    const h = stage.clientHeight;
    if (!w || !h) return;
    const prev = view();
    if (prev && prev.w === w && prev.h === h) return;
    setView({ w, h });
    if (mode() === "free" && free() <= fit() + 1e-9) resetZoom();
    else setPan({ x: tx(), y: ty() });
  }
  onMount(() => {
    measure();
    if (typeof ResizeObserver !== "undefined" && stage) {
      const ro = new ResizeObserver(() => measure());
      ro.observe(stage);
      onCleanup(() => ro.disconnect());
    } else {
      window.addEventListener("resize", measure);
      onCleanup(() => window.removeEventListener("resize", measure));
    }
  });

  function loaded(e: Event) {
    const img = e.currentTarget as HTMLImageElement;
    if (img.naturalWidth > 0 && img.naturalHeight > 0) setNatural({ w: img.naturalWidth, h: img.naturalHeight });
    measure();
  }

  // ---- keys ---------------------------------------------------------------------------------
  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLElement && /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
    if (e.key === "Escape") {
      if (saveMenu()) setSaveMenu(false);
      else if (mode() !== "fit") resetZoom();
      else props.onClose();
    } else if (e.key === "ArrowRight" && !canPan()) go(index() + 1);
    else if (e.key === "ArrowLeft" && !canPan()) go(index() - 1);
    else if (e.key === "+" || e.key === "=") zoomTo(stepZoom(scale(), true, fit()));
    else if (e.key === "-") zoomTo(stepZoom(scale(), false, fit()));
    else if (e.key === "0") resetZoom();
    else return;
    e.preventDefault();
  }
  onMount(() => {
    window.addEventListener("keydown", onKey);
    closeButton?.focus();
  });
  onCleanup(() => window.removeEventListener("keydown", onKey));

  // ---- pointers: swipe, pan, pinch ----------------------------------------------------------
  const pointers = new Map<number, { x: number; y: number }>();
  let gesture: { x: number; y: number; at: number; tx: number; ty: number; moved: boolean } | null = null;
  let pinch: { dist: number; scale: number; at: Pan } | null = null;

  const distance = () => {
    const [a, b] = [...pointers.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };

  function down(e: PointerEvent) {
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    try {
      (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    } catch {
      // Not every browser (or test DOM) can capture.
    }
    if (pointers.size === 1) gesture = { x: e.clientX, y: e.clientY, at: Date.now(), tx: tx(), ty: ty(), moved: false };
    else if (pointers.size === 2) {
      gesture = null;
      const [a, b] = [...pointers.values()];
      pinch = { dist: distance(), scale: scale(), at: a && b ? at({ clientX: (a.x + b.x) / 2, clientY: (a.y + b.y) / 2 }) : { x: 0, y: 0 } };
    }
  }

  function move(e: PointerEvent) {
    if (!pointers.has(e.pointerId)) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 2 && pinch && pinch.dist > 0) {
      zoomTo((pinch.scale * distance()) / pinch.dist, pinch.at);
    } else if (gesture && canPan()) {
      setPan({ x: gesture.tx + e.clientX - gesture.x, y: gesture.ty + e.clientY - gesture.y });
      gesture.moved = true;
    }
  }

  function up(e: PointerEvent) {
    const g = gesture;
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (!g || pointers.size > 0) return;
    gesture = null;
    const dx = e.clientX - g.x;
    const dy = e.clientY - g.y;
    if (!canPan() && Math.abs(dx) > SWIPE_PX && Math.abs(dx) > Math.abs(dy) * 1.5 && Date.now() - g.at < 900) {
      go(index() + (dx < 0 ? 1 : -1));
    }
  }

  function cancel(e: PointerEvent) {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size === 0) gesture = null;
  }

  // ⌘/ctrl + scroll (and a trackpad pinch, which arrives as ctrl + wheel) zooms around the cursor; a plain scroll
  // pans a page that is larger than the stage (Fit width scrolls this way).
  function wheel(e: WheelEvent) {
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      zoomTo(scale() * wheelFactor(e.deltaY), at(e));
    } else if (canPan()) {
      e.preventDefault();
      setPan({ x: tx() - e.deltaX, y: ty() - e.deltaY });
    }
  }

  // Double click: Fit -> 100 % at the cursor (200 % when Fit is already the image's own size), anything else -> Fit.
  function doubleClick(e: MouseEvent) {
    if (!atFit()) return resetZoom();
    zoomTo(fit() < 1 ? 1 : Math.min(2, MAX_ZOOM), at(e));
  }

  // ---- actions ------------------------------------------------------------------------------
  async function toggleBookmark() {
    const want = !isBookmarked();
    setBookmarked(want);
    try {
      if (want) await pixivApi.addBookmark(props.id);
      else await pixivApi.removeBookmark(props.id);
      props.onBookmark(props.id, want);
      const w = work();
      if (w) cached.set({ ...w, bookmarked: want });
    } catch {
      setBookmarked(!want);
      setMessage(t("pixiv.failed"));
    }
  }

  async function save(whole: boolean) {
    setSaveMenu(false);
    setBusy(true);
    setMessage(t("pixiv.saving"));
    try {
      const r = await pixivApi.save(props.id, whole ? undefined : [index()]);
      const fresh = r.saved.filter((s) => !s.duplicate).length;
      const dup = r.saved.length - fresh;
      const parts = [t("pixiv.savedMsg", { count: fresh })];
      if (dup > 0) parts.push(t("pixiv.savedDup", { count: dup }));
      if (r.failed.length > 0) parts.push(t("pixiv.saveFailed", { count: r.failed.length }));
      setMessage(parts.join(" "));
    } catch {
      setMessage(t("pixiv.failed"));
    } finally {
      setBusy(false);
    }
  }

  const onSaveClick = () => (total() > 1 ? setSaveMenu(!saveMenu()) : void save(true));

  return (
    <div class="px-reader" role="dialog" aria-modal="true" aria-label={t("pixiv.reader")}>
      <header class="px-reader__bar">
        <button ref={closeButton} type="button" class="px-iconbtn" aria-label={t("pixiv.close")} onClick={() => props.onClose()}>
          <CloseIcon />
        </button>
        <div class="px-reader__heading">
          <h2 class="px-reader__title">{title()}</h2>
          <p class="px-reader__author">{t("pixiv.by", { author: author() })}</p>
        </div>
        <div class="px-reader__actions">
          <button type="button" class="px-iconbtn" classList={{ "px-iconbtn--on": isBookmarked() }} aria-pressed={isBookmarked()} aria-label={isBookmarked() ? t("pixiv.unbookmark") : t("pixiv.bookmark")} disabled={!work()} onClick={() => void toggleBookmark()}>
            <BookmarkIcon filled={isBookmarked()} />
          </button>
          <div class="px-savewrap">
            <button type="button" class="px-iconbtn" aria-haspopup={total() > 1 ? "menu" : undefined} aria-expanded={total() > 1 ? saveMenu() : undefined} aria-label={t("pixiv.save")} title={t("pixiv.save")} disabled={!work() || busy()} onClick={onSaveClick}>
              <DownloadIcon />
            </button>
            <Show when={saveMenu()}>
              <div class="px-menu" role="menu">
                <button type="button" role="menuitem" onClick={() => void save(true)}>{t("pixiv.saveWork")}</button>
                <button type="button" role="menuitem" onClick={() => void save(false)}>{t("pixiv.savePage")}</button>
              </div>
            </Show>
          </div>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-pressed={details()} onClick={() => setDetails(!details())}>
            {t("pixiv.details")}
          </button>
        </div>
      </header>

      <div class="px-reader__body" classList={{ "px-reader__body--details": details() }}>
        <div
          ref={stage}
          class="px-stage"
          classList={{ "px-stage--zoomed": canPan() }}
          onPointerDown={down}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={cancel}
          onDblClick={doubleClick}
          onWheel={wheel}
        >
          <Show when={src()} fallback={<p class="px-stage__note">{t("pixiv.loading")}</p>}>
            <img
              class="px-stage__img"
              classList={{ "px-stage__img--sized": !!natural() && !!view() }}
              src={src()}
              alt={t("pixiv.pageOf", { page: index() + 1, total: total() })}
              draggable={false}
              onLoad={loaded}
              style={
                natural() && view()
                  ? {
                      width: `${natural()!.w}px`,
                      height: `${natural()!.h}px`,
                      transform: `translate(-50%, -50%) translate(${tx()}px, ${ty()}px) scale(${scale()})`,
                    }
                  : {}
              }
            />
          </Show>
          <Show when={failedAdult()}>
            <p class="px-stage__note" role="alert">{t("pixiv.adultOff")}</p>
          </Show>
          <Show when={loadError() && !failedAdult()}>
            <p class="px-stage__note" role="alert">{t("pixiv.failed")}</p>
          </Show>
          <Show when={total() > 1 && index() > 0}>
            <button type="button" class="px-nav px-nav--prev" aria-label={t("pixiv.previous")} onClick={(e) => { e.stopPropagation(); go(index() - 1); }} onPointerDown={(e) => e.stopPropagation()}>
              <BackIcon />
            </button>
          </Show>
          <Show when={total() > 1 && index() < total() - 1}>
            <button type="button" class="px-nav px-nav--next" aria-label={t("pixiv.next")} onClick={(e) => { e.stopPropagation(); go(index() + 1); }} onPointerDown={(e) => e.stopPropagation()}>
              <BackIcon />
            </button>
          </Show>
        </div>

        <Show when={details()}>
          <aside class="px-info" aria-label={t("pixiv.details")}>
            <Show when={work()}>
              {(w) => (
                <>
                  <dl class="px-stats">
                    <div><dt>{t("pixiv.views")}</dt><dd>{w().stats.views.toLocaleString()}</dd></div>
                    <div><dt>{t("pixiv.likes")}</dt><dd>{w().stats.likes.toLocaleString()}</dd></div>
                    <div><dt>{t("pixiv.bookmarksStat")}</dt><dd>{w().stats.bookmarks.toLocaleString()}</dd></div>
                  </dl>
                  <Show when={w().description}>
                    <p class="px-info__text">{w().description}</p>
                  </Show>
                  <Show when={w().ugoira}>
                    <p class="px-info__text">{t("pixiv.ugoira")}</p>
                  </Show>
                  <h3 class="px-info__heading">{t("pixiv.tags")}</h3>
                  <ul class="px-tags">
                    <For each={w().tags}>
                      {(tag) => (
                        <li>
                          <button type="button" class="px-tag" aria-label={t("pixiv.tagSearch", { tag: tag.tag })} onClick={() => props.onTag(tag.tag)}>
                            <SearchIcon />
                            <span>{tag.tag}</span>
                            <Show when={tag.en}>
                              <span class="px-tag__en">{tag.en}</span>
                            </Show>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </>
              )}
            </Show>
          </aside>
        </Show>
      </div>

      <footer class="px-reader__foot">
        <p class="px-reader__msg" role="status" aria-live="polite">{message() || (cached.updating() ? t("common.updating") : "")}</p>
        <div class="px-zoom" role="group" aria-label={t("pixiv.zoomReset")}>
          <Show when={tall()}>
            <button type="button" class="px-iconbtn px-iconbtn--text" aria-pressed={mode() === "width"} onClick={toggleFitWidth}>{t("pixiv.fitWidth")}</button>
          </Show>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-label={t("pixiv.zoomOut")} disabled={atFit()} onClick={() => zoomTo(stepZoom(scale(), false, fit()))}>−</button>
          <button type="button" class="px-iconbtn px-iconbtn--text px-zoom__level" aria-label={t("pixiv.zoomReset")} disabled={mode() === "fit"} onClick={resetZoom}>{zoomLabel(atFit(), scale(), t("pixiv.zoomFit"))}</button>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-label={t("pixiv.zoomIn")} disabled={scale() >= MAX_ZOOM} onClick={() => zoomTo(stepZoom(scale(), true, fit()))}>+</button>
        </div>
        <p class="px-reader__count" aria-live="polite">{index() + 1} / {total()}</p>
      </footer>
    </div>
  );
}
