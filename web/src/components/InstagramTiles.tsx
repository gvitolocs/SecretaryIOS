import { createEffect, createMemo, For, on, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { locale, t } from "../i18n";
import type { Slide } from "../lib/instagram";
import { BackIcon } from "./Icons";
import "../styles/instagram.css";

// A highlight, story or feed as tiles: a dense square grid like Instagram's profile grid (3 across on a phone, up to 6
// on a wide window), under a header with the cover, the title and the count. A month chip marks the first tile of each
// month instead of a heading per day. A tile opens the viewer at that slide; the grid stays mounted under the viewer, so
// closing it comes back to the same place. Arrow keys move between tiles, G opens the focused one, Esc closes the tiles.

/** "Jun 2022": the month a slide was taken in, or "" without a date. */
export function monthOf(seconds: number | null | undefined, lang?: string): string {
  if (!seconds) return "";
  try {
    return new Date(seconds * 1000).toLocaleDateString(lang, { month: "short", year: "numeric" });
  } catch {
    return "";
  }
}

/** Per slide, the month chip it carries: the month when it differs from the slide before, else "". */
export function monthChips(slides: Slide[], lang?: string): string[] {
  let last = "";
  return slides.map((s) => {
    const m = monthOf(s.item.takenAt, lang);
    if (!m || m === last) return "";
    last = m;
    return m;
  });
}

function PlayBadge() {
  return (
    <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M8 5.5v13l11-6.5z" fill="currentColor" stroke="none" />
    </svg>
  );
}

export default function InstagramTiles(props: {
  slides: Slide[];
  username: string;
  /** The highlight's title, when it is one. */
  title?: string;
  /** The highlight's cover picture (a signed URL). */
  cover?: string | null;
  /** The tile to focus (the slide the viewer was on). */
  focus: number;
  /** False while the viewer is open over the tiles: no keys, nothing focusable. */
  active?: boolean;
  hasMore?: boolean;
  loading?: boolean;
  onOpen: (index: number) => void;
  onClose: () => void;
  onNearEnd?: () => void;
}) {
  let panel: HTMLDivElement | undefined;
  let grid: HTMLUListElement | undefined;
  let scroller: HTMLDivElement | undefined;
  let sentinel: HTMLDivElement | undefined;
  const active = () => props.active !== false;
  const chips = createMemo(() => monthChips(props.slides, locale()));
  const span = createMemo(() => {
    const dated = props.slides.filter((s) => !!s.item.takenAt).map((s) => s.item.takenAt as number);
    if (dated.length === 0) return "";
    const first = monthOf(Math.min(...dated), locale());
    const last = monthOf(Math.max(...dated), locale());
    return first === last ? first : `${first} – ${last}`;
  });

  const tileAt = (i: number) => grid?.querySelector<HTMLButtonElement>(`.ig-hl__tile[data-index="${i}"]`) ?? null;
  const clamp = (i: number) => Math.max(0, Math.min(i, props.slides.length - 1));
  function columns(): number {
    if (!grid) return 1;
    return Math.max(1, getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length);
  }
  function focusTile(i: number) {
    const tile = tileAt(clamp(i));
    if (!tile) return;
    tile.focus({ preventScroll: true });
    tile.scrollIntoView?.({ block: "nearest" });
  }

  /** The focused tile, else the one the viewer was on. */
  function current(): number {
    const tile = (document.activeElement as Element | null)?.closest?.<HTMLElement>(".ig-hl__tile");
    const i = tile && panel?.contains(tile) ? Number(tile.dataset.index) : props.focus;
    return Number.isFinite(i) ? clamp(i) : 0;
  }

  onMount(() => {
    if (!panel?.contains(document.activeElement)) panel?.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent) => {
      if (!active() || e.metaKey || e.ctrlKey || e.altKey) return;
      if ((e.target as Element | null)?.closest?.("input, textarea, select")) return;
      const step: Record<string, number> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -columns(), ArrowDown: columns() };
      if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      } else if ((e.key === "g" || e.key === "G") && props.slides.length > 0) {
        e.preventDefault();
        props.onOpen(current());
      } else if (e.key in step && props.slides.length > 0) {
        e.preventDefault();
        const inGrid = !!(document.activeElement as Element | null)?.closest?.(".ig-hl__tile");
        focusTile(inGrid ? current() + step[e.key]! : props.focus);
      } else if ((e.key === "Home" || e.key === "End") && props.slides.length > 0) {
        e.preventDefault();
        focusTile(e.key === "Home" ? 0 : props.slides.length - 1);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));

    // Near the end of what is loaded: ask for more.
    if (sentinel && scroller && typeof IntersectionObserver !== "undefined") {
      const io = new IntersectionObserver(
        (entries) => {
          if (entries.some((e) => e.isIntersecting) && props.hasMore && !props.loading) props.onNearEnd?.();
        },
        { root: scroller, rootMargin: "600px 0px" },
      );
      io.observe(sentinel);
      onCleanup(() => io.disconnect());
    }
  });

  // Under the viewer the tiles keep their scroll position but take no focus; back from it, the slide the viewer was on
  // gets the focus (scrolled into view only if it is not on screen).
  createEffect(
    on(active, (now, before) => {
      panel?.toggleAttribute("inert", !now);
      if (now) panel?.removeAttribute("aria-hidden");
      else panel?.setAttribute("aria-hidden", "true");
      if (now && before === false) focusTile(props.focus);
    }),
  );

  return (
    <Portal>
      <div
        ref={panel}
        class="ig-tiles"
        role="dialog"
        aria-modal="true"
        aria-label={t("instagram.tilesLabel", { name: props.title ?? `@${props.username}` })}
        tabindex="-1"
      >
        <div class="ig-hl__bar">
          <button type="button" class="ig-hl__back" aria-label={t("instagram.closeTiles")} onClick={() => props.onClose()}>
            <BackIcon />
          </button>
          <span class="ig-hl__bar-title">{props.title ?? `@${props.username}`}</span>
        </div>
        <div ref={scroller} class="ig-hl__scroll">
          <div class="ig-hl__body">
            <header class="ig-hl__head">
              <span class="ig-hl__cover" aria-hidden="true">
                <Show when={props.cover ?? props.slides[0]?.thumbUrl} fallback={<span>{(props.title ?? props.username).slice(0, 1).toUpperCase()}</span>}>
                  {(url) => <img src={url()} alt="" referrerpolicy="no-referrer" />}
                </Show>
              </span>
              <div class="ig-hl__text">
                <h2>{props.title ?? `@${props.username}`}</h2>
                <p>
                  {props.title ? `@${props.username} · ` : ""}
                  {t("instagram.items", { count: props.slides.length })}
                  <Show when={span()}>{(s) => <> · {s()}</>}</Show>
                </p>
              </div>
            </header>
            <ul ref={grid} class="ig-hl__grid">
              <For each={props.slides}>
                {(slide, i) => {
                  const src = () => slide.thumbUrl ?? (slide.kind === "video" ? null : slide.url);
                  const when = () => monthOf(slide.item.takenAt, locale());
                  return (
                    <li>
                      <button
                        type="button"
                        class="ig-tile ig-hl__tile"
                        data-index={i()}
                        data-kind={slide.kind}
                        aria-label={[slide.kind === "video" ? t("instagram.video") : t("instagram.photo"), when()].filter(Boolean).join(", ")}
                        onClick={() => props.onOpen(i())}
                      >
                        <Show when={src()} fallback={<span class="ig-tile__blank" />}>
                          {(url) => (
                            <img
                              src={url()}
                              alt=""
                              loading="lazy"
                              decoding="async"
                              referrerpolicy="no-referrer"
                              onLoad={(e) => e.currentTarget.classList.add("is-loaded")}
                            />
                          )}
                        </Show>
                        <Show when={slide.kind === "video"}>
                          <span class="ig-tile__badge"><PlayBadge /></span>
                        </Show>
                        <Show when={chips()[i()]}>{(m) => <span class="ig-hl__month">{m()}</span>}</Show>
                      </button>
                    </li>
                  );
                }}
              </For>
            </ul>
            <div ref={sentinel} class="ig-hl__end" aria-hidden="true" />
            <Show when={props.loading}>
              <p class="ig-muted ig-hl__status" role="status">{t("instagram.loading")}</p>
            </Show>
          </div>
        </div>
      </div>
    </Portal>
  );
}
