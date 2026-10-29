import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show, untrack, type Accessor } from "solid-js";
import { tileUrl, type GalleryItem, type SourceKind } from "../../api/gallery";
import { locale, t } from "../../i18n";
import {
  cellsInRect,
  daysLeft,
  formatDuration,
  formatSectionLabel,
  indexAtOffset,
  layoutGallery,
  monthReached,
  neighbour,
  offsetOfIndex,
  scrollToReveal,
  sectionAt,
  sectionForMonth,
  visibleRows,
  visibleSections,
  windowFor,
  windowIndexRange,
  type Cell,
  type Direction,
  type GroupBy,
  type Layout,
  type LayoutOptions,
  type MonthCount,
  type Section,
  type Window,
} from "../../lib/galleryLayout";
import { thumbHashToImage } from "../../lib/thumbhash";
import { StackIcon } from "./stacks/StackIcons";
import { isLive } from "../../lib/galleryLive";
import { GridScrubber } from "./GridScrubber";
import { LiveIcon } from "./live/LiveIcons";
import { SourceIcon } from "./SourceBar";

// The Gallery's virtual grid: its own scroll container (a definite box inside the app shell), sticky day or month
// headers, justified rows on wide areas and square tiles on phones. Only the sections and rows near the viewport are in
// the DOM (two screens either side), so 20,000 pictures cost what 100 do.

export interface TileClick {
  index: number;
  shift: boolean;
  toggle: boolean;
}

export type GridCommand = "delete" | "clear" | "all" | "favorite";

export interface GridViewProps {
  items: GalleryItem[];
  /** Changes when the items are not just appended to (so the rows start over). */
  epoch: number;
  /** Changes when the query changes (the grid scrolls back to the top). */
  resetKey: string;
  groupBy: GroupBy;
  selected: ReadonlySet<string>;
  jxl: boolean;
  /** Show where each picture came from (the All view). */
  showSource: boolean;
  /** The kind of a source for a tile's badge. */
  sourceKind: (item: GalleryItem) => SourceKind;
  /** The Trash: tiles show the days left. */
  trash?: boolean;
  loading: boolean;
  hasMore: boolean;
  failed: boolean;
  months: MonthCount[];
  /** Hide the scrubber (it counts the whole library, so it is wrong under a search or a date range). */
  scrubber: boolean;
  onNeedMore: () => void;
  onRetry: () => void;
  /** Load pages until the month is in. */
  onSeek: (month: string) => Promise<void>;
  onTile: (click: TileClick) => void;
  onToggle: (index: number) => void;
  onLongPress: (index: number) => void;
  onSection: (start: number, count: number, on: boolean) => void;
  onBand: (phase: "start" | "move" | "end", indexes: number[], additive: boolean) => void;
  onCommand: (command: GridCommand) => void;
  /** Focus and reveal this tile when the grid mounts (back from a viewer, e.g. the Instagram tiles). */
  focusIndex?: number;
}

interface Pad {
  left: number;
  right: number;
}

/** Layout options for a content width: squares on a phone, justified rows elsewhere. */
export function optionsFor(width: number, groupBy: GroupBy): LayoutOptions {
  const narrow = width < 520;
  const flat = groupBy === "none";
  return {
    width,
    gap: narrow ? 2 : 3,
    target: narrow ? 118 : width < 900 ? 160 : 210,
    mode: narrow ? "square" : "justified",
    groupBy,
    headerHeight: flat ? 0 : narrow ? 36 : 42,
    sectionGap: flat ? 0 : narrow ? 8 : 14,
  };
}

const SCRUBBER_MIN_WIDTH = 640;
const SCRUBBER_SPACE = 34;
const FOOTER = 64;
const hashCache = new Map<string, string | null>();

function placeholder(hash: string | null): string | null {
  if (!hash) return null;
  if (hashCache.has(hash)) return hashCache.get(hash)!;
  const url = thumbHashToImage(hash)?.url ?? null;
  if (hashCache.size > 4000) hashCache.clear();
  hashCache.set(hash, url);
  return url;
}

const dateFormats = new Map<string, Intl.DateTimeFormat>();
function formatDay(iso: string | null, loc: string): string {
  const ms = iso ? Date.parse(iso) : NaN;
  if (!Number.isFinite(ms)) return "";
  let f = dateFormats.get(loc);
  if (!f) {
    f = new Intl.DateTimeFormat(loc, { dateStyle: "medium" });
    dateFormats.set(loc, f);
  }
  return f.format(new Date(ms));
}

interface Ctx {
  items: Accessor<GalleryItem[]>;
  selected: Accessor<ReadonlySet<string>>;
  selecting: Accessor<boolean>;
  jxl: Accessor<boolean>;
  showSource: Accessor<boolean>;
  sourceKind: (item: GalleryItem) => SourceKind;
  trash: Accessor<boolean>;
  tabStop: Accessor<number>;
  click: (index: number, ev: MouseEvent) => void;
  toggle: (index: number) => void;
  longPress: (index: number) => void;
  focused: (index: number) => void;
}

function tileLabel(item: GalleryItem, trash: boolean): string {
  const loc = locale();
  const parts: string[] = [t(item.kind === "video" ? "gallery.video" : isLive(item) ? "galleryLive.photo" : "gallery.photo")];
  if (item.kind === "video" && item.durationMs !== null) parts.push(formatDuration(item.durationMs));
  const day = formatDay(item.takenAt ?? item.addedAt, loc);
  if (day) parts.push(day);
  if (item.deviceName) parts.push(item.deviceName);
  if (item.favorite) parts.push(t("gallery.favoriteTag"));
  if (item.stack) parts.push(t("galleryDedup.stackBadge", { count: item.stack.count }));
  if (trash) {
    const left = daysLeft(item.trashExpiresAt);
    if (left !== null) parts.push(t("gallery.daysLeft", { count: left }));
  }
  return parts.join(", ");
}

function Tile(props: { cell: Cell; ctx: Ctx; rowHeight: number }) {
  const ctx = props.ctx;
  const item = createMemo(() => ctx.items()[props.cell.index]);
  const selected = () => {
    const i = item();
    return !!i && ctx.selected().has(i.id);
  };
  const [loaded, setLoaded] = createSignal(false);
  const [broken, setBroken] = createSignal(false);
  const src = () => {
    const i = item();
    return i ? tileUrl(i, ctx.jxl()) : null;
  };
  const hash = () => placeholder(item()?.thumbhash ?? null);
  const turned = () => {
    const r = item()?.rotation ?? 0;
    return r === 90 || r === 270;
  };
  // A picture turned a quarter keeps its file's shape: draw it in a box of the swapped size, rotated about the middle.
  const imgStyle = () => {
    const r = item()?.rotation ?? 0;
    if (!r) return undefined;
    if (!turned()) return { transform: `rotate(${r}deg)` };
    return {
      width: `${props.rowHeight}px`,
      height: `${props.cell.width}px`,
      left: "50%",
      top: "50%",
      transform: `translate(-50%, -50%) rotate(${r}deg)`,
    };
  };

  let timer: ReturnType<typeof setTimeout> | undefined;
  let startX = 0;
  let startY = 0;
  let touched = false;
  let fired = false;
  const cancel = () => clearTimeout(timer);
  onCleanup(cancel);

  return (
    <Show when={item()}>
      {(it) => (
        <div
          class="g-tile"
          classList={{ "g-tile--selected": selected(), "g-tile--selecting": ctx.selecting() }}
          role="listitem"
          data-index={props.cell.index}
          data-id={it().id}
          style={{ left: `${props.cell.left}px`, width: `${props.cell.width}px` }}
        >
          <button
            type="button"
            class="g-open"
            aria-label={tileLabel(it(), ctx.trash())}
            tabindex={ctx.tabStop() === props.cell.index ? 0 : -1}
            onFocus={() => ctx.focused(props.cell.index)}
            onClick={(e) => {
              if (fired) {
                fired = false;
                e.preventDefault();
                return;
              }
              ctx.click(props.cell.index, e);
            }}
            onKeyDown={(e) => {
              if (e.key === " " && !e.metaKey && !e.ctrlKey) {
                e.preventDefault();
                ctx.toggle(props.cell.index);
              }
            }}
            onPointerDown={(e) => {
              touched = e.pointerType !== "mouse";
              if (!touched) return;
              fired = false;
              startX = e.clientX;
              startY = e.clientY;
              cancel();
              timer = setTimeout(() => {
                fired = true;
                try {
                  navigator.vibrate?.(8);
                } catch {
                  // No haptics here.
                }
                ctx.longPress(props.cell.index);
              }, 480);
            }}
            onPointerMove={(e) => {
              if (Math.hypot(e.clientX - startX, e.clientY - startY) > 8) cancel();
            }}
            onPointerUp={cancel}
            onPointerCancel={cancel}
            onPointerLeave={cancel}
            onContextMenu={(e) => {
              if (touched || fired) e.preventDefault();
            }}
          >
            <span class="g-ph" style={hash() ? { "background-image": `url(${hash()})` } : undefined} aria-hidden="true" />
            <Show when={src() && !broken()}>
              <img
                class="g-img"
                classList={{ "g-img--in": loaded(), "g-img--turned": turned() }}
                style={imgStyle()}
                src={src()!}
                alt=""
                draggable={false}
                decoding="async"
                loading="lazy"
                onLoad={() => setLoaded(true)}
                onError={() => setBroken(true)}
              />
            </Show>
            <Show when={it().kind === "video"}>
              <span class="g-badge g-badge--video" aria-hidden="true">
                <svg class="g-badge__play" viewBox="0 0 12 12">
                  <path d="M3 1.8v8.4L10.2 6z" />
                </svg>
                <Show when={it().durationMs !== null}>{formatDuration(it().durationMs)}</Show>
              </span>
            </Show>
            <Show when={isLive(it())}>
              <span class="g-badge g-badge--live" aria-hidden="true">
                <LiveIcon class="g-badge__live" />
              </span>
            </Show>
            <Show when={it().stack}>
              {(stack) => (
                <span class="g-badge g-badge--stack" aria-hidden="true">
                  <StackIcon />
                  {stack().count}
                </span>
              )}
            </Show>
            <Show when={ctx.showSource()}>
              <span class="g-badge g-badge--source" aria-hidden="true" title={it().deviceName ?? undefined}>
                <SourceIcon kind={ctx.sourceKind(it())} />
              </span>
            </Show>
            <Show when={it().favorite}>
              <span class="g-badge g-badge--fav" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <path d="M12 20.4s-7.5-4.6-7.5-10.2A4.3 4.3 0 0 1 12 7.6a4.3 4.3 0 0 1 7.5 2.6c0 5.6-7.5 10.2-7.5 10.2z" />
                </svg>
              </span>
            </Show>
            <Show when={ctx.trash() && daysLeft(it().trashExpiresAt) !== null}>
              <span class="g-badge g-badge--days" aria-hidden="true">
                {t("gallery.daysLeftShort", { count: daysLeft(it().trashExpiresAt)! })}
              </span>
            </Show>
          </button>
          <button
            type="button"
            class="g-check"
            role="checkbox"
            aria-checked={selected()}
            aria-label={t("gallery.select")}
            tabindex="-1"
            onClick={(e) => {
              e.stopPropagation();
              ctx.toggle(props.cell.index);
            }}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="m6 12.5 4 4 8-9" />
            </svg>
          </button>
        </div>
      )}
    </Show>
  );
}

function SectionView(props: { section: Section; win: Accessor<Window>; ctx: Ctx; onSection: GridViewProps["onSection"] }) {
  const rows = createMemo(() => visibleRows(props.section, props.win()));
  const flat = () => props.section.key === "all";
  const label = () => (flat() ? t("gallery.title") : formatSectionLabel(props.section.key, locale()) || t("gallery.noDate"));
  const allSelected = createMemo(() => {
    const sel = props.ctx.selected();
    if (!sel.size) return false;
    const items = props.ctx.items();
    const { start, count } = props.section;
    for (let i = start; i < start + count; i++) if (!sel.has(items[i]!.id)) return false;
    return true;
  });
  const listId = `g-sec-${props.section.id}`;
  return (
    <section
      class="g-section"
      style={{ top: `${props.section.top}px`, height: `${props.section.height}px` }}
      aria-labelledby={flat() ? undefined : `${listId}-h`}
      data-section={props.section.id}
    >
      <Show when={!flat()}>
        <h2 class="g-head" id={`${listId}-h`}>
          <button
            type="button"
            class="g-head__check"
            role="checkbox"
            aria-checked={allSelected()}
            aria-label={t("gallery.selectSection", { label: label() })}
            onClick={() => props.onSection(props.section.start, props.section.count, !allSelected())}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="m6 12.5 4 4 8-9" />
            </svg>
          </button>
          <span class="g-head__label">{label()}</span>
          <span class="g-head__count">{t("gallery.itemCount", { count: props.section.count })}</span>
        </h2>
      </Show>
      <div role="list" aria-label={label()}>
        <For each={rows()}>
          {(row) => (
            <div class="g-row" style={{ top: `${row.top}px`, height: `${row.height}px` }}>
              <For each={row.cells}>{(cell) => <Tile cell={cell} ctx={props.ctx} rowHeight={row.height} />}</For>
            </div>
          )}
        </For>
      </div>
    </section>
  );
}

export function GridView(props: GridViewProps) {
  let scroller!: HTMLDivElement;
  let content!: HTMLDivElement;
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  const [scrollTop, setScrollTop] = createSignal(0);
  const [active, setActive] = createSignal(0);
  const [band, setBand] = createSignal<{ left: number; top: number; width: number; height: number } | null>(null);

  const showScrubber = () => props.scrubber && props.months.length > 1 && size().w >= SCRUBBER_MIN_WIDTH;
  const pad = (): Pad => {
    const w = size().w;
    const edge = w < 520 ? 2 : 12;
    return { left: edge, right: edge + (showScrubber() ? SCRUBBER_SPACE : 0) };
  };
  const contentWidth = () => Math.max(0, size().w - pad().left - pad().right);

  // The layout, kept between renders so that appending a page leaves the sections already drawn alone.
  let prevLayout: Layout | null = null;
  let prevEpoch = -1;
  let anchor: number | null = null;
  const layout = createMemo<Layout>(() => {
    const items = props.items;
    const epoch = props.epoch;
    const options = optionsFor(contentWidth(), props.groupBy);
    let reuse: Layout | null = epoch === prevEpoch ? prevLayout : null;
    if (reuse && reuse.itemCount === items.length && JSON.stringify(reuse.options) === JSON.stringify(options)) return reuse;
    // A new width reshapes every row: keep the row at the top of the viewport at the top of the viewport.
    const top = untrack(scrollTop);
    if (prevLayout && top > 0 && JSON.stringify(prevLayout.options) !== JSON.stringify(options) && prevLayout.itemCount > 0) {
      anchor = indexAtOffset(prevLayout, top);
    }
    const next = layoutGallery(items, options, reuse);
    prevLayout = next;
    prevEpoch = epoch;
    return next;
  });

  createEffect(() => {
    const l = layout();
    if (anchor === null) return;
    const idx = anchor;
    anchor = null;
    const y = offsetOfIndex(l, idx);
    if (y === null || !scroller) return;
    requestAnimationFrame(() => {
      scroller.scrollTop = y;
      setScrollTop(scroller.scrollTop);
    });
  });

  const win = createMemo(() => windowFor(scrollTop(), size().h, 2));
  const sections = createMemo(() => visibleSections(layout(), win()));
  const range = createMemo(() => windowIndexRange(layout(), win()));
  const tabStop = () => {
    const r = range();
    if (!r) return -1;
    const a = active();
    return a >= r.first && a <= r.last ? a : r.first;
  };
  const selecting = () => props.selected.size > 0;

  let suppressClick = false;

  // ---- measuring and scrolling ------------------------------------------------------------------------------------

  function measure() {
    if (!scroller) return;
    setSize({ w: scroller.clientWidth, h: scroller.clientHeight });
  }
  let frame = 0;
  function onScroll() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      setScrollTop(scroller.scrollTop);
    });
  }
  onMount(() => {
    measure();
    const start = props.focusIndex;
    if (start !== undefined) requestAnimationFrame(() => focusIndex(start));
    // The click that ends a rubber-band drag must not open the tile under the pointer.
    const swallow = (e: Event) => {
      if (suppressClick) {
        e.stopPropagation();
        e.preventDefault();
      }
    };
    scroller.addEventListener("click", swallow, true);
    onCleanup(() => scroller.removeEventListener("click", swallow, true));
    let ro: ResizeObserver | undefined;
    if (typeof ResizeObserver !== "undefined") {
      ro = new ResizeObserver(measure);
      ro.observe(scroller);
    }
    window.addEventListener("resize", measure);
    onCleanup(() => {
      ro?.disconnect();
      window.removeEventListener("resize", measure);
      if (frame) cancelAnimationFrame(frame);
    });
  });
  createEffect(
    on(
      () => props.resetKey,
      () => {
        if (!scroller) return;
        scroller.scrollTop = 0;
        setScrollTop(0);
        setActive(0);
      },
      { defer: true },
    ),
  );

  function scrollToY(y: number) {
    scroller.scrollTop = y;
    setScrollTop(scroller.scrollTop);
  }

  // The next page when the viewer comes within three screens of the end.
  createEffect(() => {
    const near = scrollTop() + size().h * 3 >= layout().height;
    if (near && props.hasMore && !props.loading && !props.failed && size().h > 0) props.onNeedMore();
  });

  // ---- keyboard ---------------------------------------------------------------------------------------------------

  function focusIndex(index: number) {
    const l = layout();
    if (index < 0 || index >= l.itemCount) return;
    setActive(index);
    const y = scrollToReveal(l, index, scroller.scrollTop, size().h, optionsFor(contentWidth(), props.groupBy).headerHeight);
    if (y !== scroller.scrollTop) scrollToY(y);
    const el = content.querySelector<HTMLElement>(`[data-index="${index}"] .g-open`);
    el?.focus({ preventScroll: true });
  }

  function onKeyDown(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, select")) return;
    const tile = target.closest<HTMLElement>(".g-tile");
    const index = tile ? Number(tile.dataset.index) : active();
    const arrow: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
    const mod = e.metaKey || e.ctrlKey;
    if (arrow[e.key] && tile && !mod) {
      e.preventDefault();
      focusIndex(neighbour(layout(), index, arrow[e.key]!));
    } else if (e.key === "Home" && tile) {
      e.preventDefault();
      focusIndex(0);
    } else if (e.key === "End" && tile) {
      e.preventDefault();
      focusIndex(layout().itemCount - 1);
    } else if (e.key === "Escape" && selecting()) {
      e.preventDefault();
      props.onCommand("clear");
    } else if ((e.key === "a" || e.key === "A") && mod) {
      e.preventDefault();
      props.onCommand("all");
    } else if ((e.key === "Delete" || e.key === "Backspace") && selecting() && !mod) {
      e.preventDefault();
      props.onCommand("delete");
    }
  }

  // ---- rubber band ------------------------------------------------------------------------------------------------

  function onPointerDown(e: PointerEvent) {
    if (e.pointerType !== "mouse" || e.button !== 0) return;
    const box = scroller.getBoundingClientRect();
    if (e.clientX - box.left >= scroller.clientWidth) return; // the scrollbar
    if ((e.target as HTMLElement).closest(".g-check, .g-head__check, .g-foot, button.g-retry")) return;
    const origin = content.getBoundingClientRect();
    const startX = e.clientX - origin.left;
    const startY = e.clientY - origin.top;
    const additive = e.metaKey || e.ctrlKey || e.shiftKey;
    let engaged = false;
    let lastX = e.clientX;
    let lastY = e.clientY;
    let timer: ReturnType<typeof setInterval> | undefined;

    const update = () => {
      const c = content.getBoundingClientRect();
      const x = Math.min(Math.max(lastX - c.left, 0), c.width);
      const y = Math.min(Math.max(lastY - c.top, 0), c.height);
      const rect = { left: Math.min(startX, x), right: Math.max(startX, x), top: Math.min(startY, y), bottom: Math.max(startY, y) };
      setBand({ left: rect.left, top: rect.top, width: rect.right - rect.left, height: rect.bottom - rect.top });
      props.onBand("move", cellsInRect(layout(), rect), additive);
    };
    const move = (ev: PointerEvent) => {
      lastX = ev.clientX;
      lastY = ev.clientY;
      if (!engaged) {
        if (Math.hypot(ev.clientX - e.clientX, ev.clientY - e.clientY) < 6) return;
        engaged = true;
        props.onBand("start", [], additive);
        // Near the top or bottom edge the grid scrolls under the pointer.
        timer = setInterval(() => {
          const b = scroller.getBoundingClientRect();
          const edge = 44;
          const speed = lastY < b.top + edge ? -(b.top + edge - lastY) / 3 : lastY > b.bottom - edge ? (lastY - (b.bottom - edge)) / 3 : 0;
          if (speed) {
            scrollToY(scroller.scrollTop + speed);
            update();
          }
        }, 30);
      }
      ev.preventDefault();
      update();
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      clearInterval(timer);
      if (engaged) {
        suppressClick = true;
        setTimeout(() => (suppressClick = false), 0);
        setBand(null);
        props.onBand("end", [], additive);
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  }

  // ---- scrubber ---------------------------------------------------------------------------------------------------

  const currentMonth = createMemo(() => sectionAt(layout(), scrollTop() + 1)?.key.slice(0, 7) ?? null);
  let seeking = 0;
  async function seek(month: string) {
    const ticket = ++seeking;
    if (!monthReached(layout(), month) && props.hasMore) await props.onSeek(month);
    if (ticket !== seeking) return;
    const s = sectionForMonth(layout(), month);
    if (s) scrollToY(s.top);
  }

  const ctx: Ctx = {
    items: () => props.items,
    selected: () => props.selected,
    selecting,
    jxl: () => props.jxl,
    showSource: () => props.showSource,
    sourceKind: (i) => props.sourceKind(i),
    trash: () => !!props.trash,
    tabStop,
    click: (index, ev) => props.onTile({ index, shift: ev.shiftKey, toggle: ev.metaKey || ev.ctrlKey }),
    toggle: (index) => props.onToggle(index),
    longPress: (index) => props.onLongPress(index),
    focused: (index) => setActive(index),
  };

  return (
    <div class="g-body">
      <div
        class="g-scroll"
        ref={scroller}
        onScroll={onScroll}
        onKeyDown={onKeyDown}
        onPointerDown={onPointerDown}
        data-testid="gallery-scroll"
      >
        <div
          class="g-content"
          ref={content}
          style={{
            height: `${layout().height + FOOTER}px`,
            "margin-left": `${pad().left}px`,
            width: `${contentWidth()}px`,
          }}
        >
          <For each={sections()}>{(section) => <SectionView section={section} win={win} ctx={ctx} onSection={props.onSection} />}</For>
          <Show when={band()}>{(b) => <div class="g-band" style={{ left: `${b().left}px`, top: `${b().top}px`, width: `${b().width}px`, height: `${b().height}px` }} />}</Show>
          <div class="g-foot" style={{ top: `${layout().height}px`, height: `${FOOTER}px` }}>
            <Show when={props.failed && props.items.length > 0}>
              <span role="alert">{t("gallery.loadMoreError")}</span>
              <button type="button" class="g-retry" onClick={props.onRetry}>
                {t("gallery.retry")}
              </button>
            </Show>
            <Show when={!props.failed && props.loading && props.items.length > 0}>
              <span class="g-spinner" role="status" aria-label={t("gallery.loading")} />
            </Show>
          </div>
        </div>
      </div>
      <Show when={showScrubber()}>
        <GridScrubber
          months={props.months}
          current={currentMonth()}
          onSeek={(m) => void seek(m)}
        />
      </Show>
    </div>
  );
}
