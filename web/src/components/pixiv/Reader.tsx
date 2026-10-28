import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { isAdultOff, pixivApi, type PixivIllust, type PixivItem } from "../../api/pixiv";
import { t } from "../../i18n";
import { BackIcon, BookmarkIcon, CloseIcon, DownloadIcon, SearchIcon } from "../Icons";

// The Pixiv work viewer: every page of a work, one at a time. Swipe or the arrow keys turn pages; the
// zoom buttons, a double click or a two-finger pinch zoom (drag to pan); the next page is fetched in
// advance. It also bookmarks the work, saves it (or one page) to Lucina's Gallery, and lists its tags
// (a tag opens a search). Escape or the close button leaves; a zoomed page un-zooms first.

const MIN_ZOOM = 1;
const MAX_ZOOM = 5;
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
  // The fetcher never throws: reading a failed resource would throw inside every memo that reads it.
  const [result] = createResource(
    () => props.id,
    async (id): Promise<{ work?: PixivIllust; error?: unknown }> => {
      try {
        return { work: await pixivApi.illust(id) };
      } catch (error) {
        return { error };
      }
    },
  );
  const work = () => result()?.work;
  const loadError = () => result()?.error;
  const [index, setIndex] = createSignal(0);
  const [scale, setScale] = createSignal(1);
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
  const failedAdult = createMemo(() => !!loadError() && isAdultOff(loadError()));

  // A new work starts at its first page.
  createEffect(on(() => props.id, () => { setIndex(0); setBookmarked(null); setMessage(""); resetZoom(); }, { defer: true }));

  // The next page (and the previous one) are fetched before they are asked for.
  createEffect(() => {
    const ps = pages();
    for (const i of [index() + 1, index() - 1]) {
      const url = ps[i]?.url;
      if (url) new Image().src = url;
    }
  });

  function resetZoom() {
    setScale(1);
    setTx(0);
    setTy(0);
  }

  function go(to: number) {
    const n = clamp(to, 0, Math.max(0, total() - 1));
    if (n === index()) return;
    setIndex(n);
    resetZoom();
  }

  function bounds() {
    const w = stage?.clientWidth ?? 0;
    const h = stage?.clientHeight ?? 0;
    return { x: (w * (scale() - 1)) / 2, y: (h * (scale() - 1)) / 2 };
  }

  function setZoom(next: number) {
    const s = clamp(next, MIN_ZOOM, MAX_ZOOM);
    setScale(s);
    if (s === 1) {
      setTx(0);
      setTy(0);
    } else {
      const b = bounds();
      setTx(clamp(tx(), -b.x, b.x));
      setTy(clamp(ty(), -b.y, b.y));
    }
  }

  // ---- keys ---------------------------------------------------------------------------------
  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLElement && /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
    if (e.key === "Escape") {
      if (saveMenu()) setSaveMenu(false);
      else if (scale() > 1) resetZoom();
      else props.onClose();
    } else if (e.key === "ArrowRight" && scale() === 1) go(index() + 1);
    else if (e.key === "ArrowLeft" && scale() === 1) go(index() - 1);
    else if (e.key === "+" || e.key === "=") setZoom(scale() + 0.5);
    else if (e.key === "-") setZoom(scale() - 0.5);
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
  let pinch: { dist: number; scale: number } | null = null;

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
      pinch = { dist: distance(), scale: scale() };
    }
  }

  function move(e: PointerEvent) {
    if (!pointers.has(e.pointerId)) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 2 && pinch && pinch.dist > 0) {
      setZoom((pinch.scale * distance()) / pinch.dist);
    } else if (gesture && scale() > 1) {
      const b = bounds();
      setTx(clamp(gesture.tx + e.clientX - gesture.x, -b.x, b.x));
      setTy(clamp(gesture.ty + e.clientY - gesture.y, -b.y, b.y));
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
    if (scale() === 1 && Math.abs(dx) > SWIPE_PX && Math.abs(dx) > Math.abs(dy) * 1.5 && Date.now() - g.at < 900) {
      go(index() + (dx < 0 ? 1 : -1));
    }
  }

  function cancel(e: PointerEvent) {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size === 0) gesture = null;
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    setZoom(scale() * (e.deltaY < 0 ? 1.15 : 1 / 1.15));
  }

  // ---- actions ------------------------------------------------------------------------------
  async function toggleBookmark() {
    const want = !isBookmarked();
    setBookmarked(want);
    try {
      if (want) await pixivApi.addBookmark(props.id);
      else await pixivApi.removeBookmark(props.id);
      props.onBookmark(props.id, want);
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
          classList={{ "px-stage--zoomed": scale() > 1 }}
          onPointerDown={down}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={cancel}
          onDblClick={() => (scale() > 1 ? resetZoom() : setZoom(2.5))}
          onWheel={wheel}
        >
          <Show when={src()} fallback={<p class="px-stage__note">{t("pixiv.loading")}</p>}>
            <img
              class="px-stage__img"
              src={src()}
              alt={t("pixiv.pageOf", { page: index() + 1, total: total() })}
              draggable={false}
              style={{ transform: `translate(${tx()}px, ${ty()}px) scale(${scale()})` }}
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
        <p class="px-reader__msg" role="status" aria-live="polite">{message()}</p>
        <div class="px-zoom" role="group" aria-label={t("pixiv.zoomReset")}>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-label={t("pixiv.zoomOut")} disabled={scale() <= MIN_ZOOM} onClick={() => setZoom(scale() - 0.5)}>−</button>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-label={t("pixiv.zoomReset")} disabled={scale() === 1} onClick={resetZoom}>{Math.round(scale() * 100)}%</button>
          <button type="button" class="px-iconbtn px-iconbtn--text" aria-label={t("pixiv.zoomIn")} disabled={scale() >= MAX_ZOOM} onClick={() => setZoom(scale() + 0.5)}>+</button>
        </div>
        <p class="px-reader__count" aria-live="polite">{index() + 1} / {total()}</p>
      </footer>
    </div>
  );
}
