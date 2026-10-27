// The Gallery grid's maths, with no DOM: sections (days or months), justified or square rows, the virtual window, the
// month scrubber and the selection rules. views/Gallery.tsx and components/gallery/Grid*.tsx draw what this returns.
//
// Everything is in CSS pixels relative to the top-left corner of the grid's content box. A layout is built once per
// (items, width, options); appending a page of items reuses every section but the last, so the rows already on screen
// keep their identity and the DOM is not rebuilt while the owner scrolls.

export type GroupBy = "day" | "month";
export type GridMode = "justified" | "square";

/** What the layout needs from a gallery item. */
export interface LayoutItem {
  takenAt: string | null;
  addedAt: string;
  width: number | null;
  height: number | null;
  rotation: number;
}

export interface LayoutOptions {
  /** Width of the content box. */
  width: number;
  /** Space between tiles. */
  gap: number;
  /** The row height justified rows aim for, and the tile size square rows aim for. */
  target: number;
  mode: GridMode;
  groupBy: GroupBy;
  /** Height of a section header (it is sticky inside its section). */
  headerHeight: number;
  /** Space after a section. */
  sectionGap: number;
}

export interface Cell {
  /** Index into the items array. */
  index: number;
  left: number;
  width: number;
}

export interface Row {
  /** Relative to the top of its section (the header is above the first row). */
  top: number;
  height: number;
  cells: Cell[];
}

export interface Section {
  /** `2026-10-14` (day) or `2026-10` (month); repeated keys get a `#n` suffix so they stay unique. */
  id: string;
  /** The date key without the suffix. */
  key: string;
  start: number;
  count: number;
  top: number;
  height: number;
  rows: Row[];
}

export interface Layout {
  sections: Section[];
  height: number;
  itemCount: number;
  options: LayoutOptions;
}

export const DEFAULT_OPTIONS: LayoutOptions = {
  width: 1000,
  gap: 3,
  target: 200,
  mode: "justified",
  groupBy: "day",
  headerHeight: 40,
  sectionGap: 14,
};

const MIN_ASPECT = 0.55;
const MAX_ASPECT = 2.4;

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** The local calendar day or month an item belongs to (taken date, else the date it was added). */
export function sectionKey(item: Pick<LayoutItem, "takenAt" | "addedAt">, groupBy: GroupBy): string {
  const ms = Date.parse(item.takenAt ?? item.addedAt);
  if (!Number.isFinite(ms)) return "0000-00";
  const d = new Date(ms);
  const month = `${d.getFullYear()}-${pad2(d.getMonth() + 1)}`;
  return groupBy === "month" ? month : `${month}-${pad2(d.getDate())}`;
}

/** Width over height as it is shown (a quarter turn swaps them), kept within a sane range for the rows. */
export function aspectOf(item: Pick<LayoutItem, "width" | "height" | "rotation">): number {
  let w = item.width ?? 0;
  let h = item.height ?? 0;
  if (w <= 0 || h <= 0) return 1;
  if (item.rotation === 90 || item.rotation === 270) [w, h] = [h, w];
  return Math.min(MAX_ASPECT, Math.max(MIN_ASPECT, w / h));
}

function layoutRows(items: readonly LayoutItem[], start: number, end: number, o: LayoutOptions): { rows: Row[]; height: number } {
  const rows: Row[] = [];
  let y = o.headerHeight;
  const push = (cells: Cell[], height: number) => {
    rows.push({ top: y, height, cells });
    y += height + o.gap;
  };
  if (o.mode === "square") {
    const cols = Math.max(2, Math.round((o.width + o.gap) / (o.target + o.gap)));
    const size = (o.width - o.gap * (cols - 1)) / cols;
    for (let i = start; i < end; i += cols) {
      const cells: Cell[] = [];
      for (let c = 0; c < cols && i + c < end; c++) cells.push({ index: i + c, left: c * (size + o.gap), width: size });
      push(cells, size);
    }
  } else {
    let row: { index: number; aspect: number }[] = [];
    let sum = 0;
    const close = (height: number) => {
      const cells: Cell[] = [];
      let left = 0;
      for (const r of row) {
        const w = r.aspect * height;
        cells.push({ index: r.index, left, width: w });
        left += w + o.gap;
      }
      push(cells, height);
      row = [];
      sum = 0;
    };
    const fit = (aspectSum: number, n: number) => (o.width - o.gap * (n - 1)) / aspectSum;
    for (let i = start; i < end; i++) {
      const aspect = aspectOf(items[i]!);
      if (row.length) {
        const without = fit(sum, row.length);
        const withIt = fit(sum + aspect, row.length + 1);
        // Take the tile only when the row ends up closer to the target height with it than without it.
        if (withIt < o.target && o.target - withIt > without - o.target) close(without);
      }
      row.push({ index: i, aspect });
      sum += aspect;
      if (fit(sum, row.length) <= o.target) close(fit(sum, row.length));
    }
    // The last row of a section is not stretched to the edge.
    if (row.length) close(Math.min(o.target, fit(sum, row.length)));
  }
  const height = rows.length ? y - o.gap : o.headerHeight;
  return { rows, height: height + o.sectionGap };
}

function sameOptions(a: LayoutOptions, b: LayoutOptions): boolean {
  return (
    a.width === b.width && a.gap === b.gap && a.target === b.target && a.mode === b.mode && a.groupBy === b.groupBy &&
    a.headerHeight === b.headerHeight && a.sectionGap === b.sectionGap
  );
}

/**
 * Lay `items` out (they are already sorted; items of one day or month are next to each other).
 * `prev` is a layout of the first `prev.itemCount` of the same items with the same options; every section of it
 * but the last is kept as it is. Pass it only when the items were appended to.
 */
export function layoutGallery(items: readonly LayoutItem[], options: LayoutOptions, prev?: Layout | null): Layout {
  const reuse = prev && prev.itemCount <= items.length && prev.sections.length > 0 && sameOptions(prev.options, options) ? prev : null;
  const sections: Section[] = reuse ? reuse.sections.slice(0, -1) : [];
  let top = sections.length ? sections[sections.length - 1]!.top + sections[sections.length - 1]!.height : 0;
  let i = reuse ? reuse.sections[reuse.sections.length - 1]!.start : 0;
  const seen = new Map<string, number>();
  for (const s of sections) seen.set(s.key, (seen.get(s.key) ?? 0) + 1);
  while (i < items.length) {
    const key = sectionKey(items[i]!, options.groupBy);
    let end = i + 1;
    while (end < items.length && sectionKey(items[end]!, options.groupBy) === key) end++;
    const n = seen.get(key) ?? 0;
    seen.set(key, n + 1);
    const { rows, height } = layoutRows(items, i, end, options);
    sections.push({ id: n ? `${key}#${n}` : key, key, start: i, count: end - i, top, height, rows });
    top += height;
    i = end;
  }
  return { sections, height: top, itemCount: items.length, options };
}

// ---- the virtual window ------------------------------------------------------------------------------------------

/** The first index in `arr` whose `end(x)` is greater than `y` (binary search; arr is ordered by position). */
function firstEndingAfter<T>(arr: readonly T[], end: (x: T) => number, y: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (end(arr[mid]!) > y) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

export interface Window {
  top: number;
  bottom: number;
}

/** The part of the content to keep in the DOM: the viewport plus `overscan` viewports above and below. */
export function windowFor(scrollTop: number, viewport: number, overscan = 2): Window {
  const top = Math.max(0, scrollTop - viewport * overscan);
  return { top, bottom: scrollTop + viewport * (overscan + 1) };
}

/** The sections that touch the window. */
export function visibleSections(layout: Layout, w: Window): Section[] {
  const s = layout.sections;
  const first = firstEndingAfter(s, (x) => x.top + x.height, w.top);
  let last = first;
  while (last < s.length && s[last]!.top < w.bottom) last++;
  return s.slice(first, last);
}

/** The rows of one section that touch the window. */
export function visibleRows(section: Section, w: Window): Row[] {
  const top = w.top - section.top;
  const bottom = w.bottom - section.top;
  const first = firstEndingAfter(section.rows, (r) => r.top + r.height, top);
  let last = first;
  while (last < section.rows.length && section.rows[last]!.top < bottom) last++;
  return section.rows.slice(first, last);
}

/** The first and last item index drawn in the window, or null when nothing is. */
export function windowIndexRange(layout: Layout, w: Window): { first: number; last: number } | null {
  let first = -1;
  let last = -1;
  for (const s of visibleSections(layout, w)) {
    const rows = visibleRows(s, w);
    if (!rows.length) continue;
    const a = rows[0]!.cells[0]!.index;
    const b = rows[rows.length - 1]!.cells[rows[rows.length - 1]!.cells.length - 1]!.index;
    if (first < 0) first = a;
    last = b;
  }
  return first < 0 ? null : { first, last };
}

/** The section at a content offset (the one whose header is sticky there). */
export function sectionAt(layout: Layout, y: number): Section | null {
  const s = layout.sections;
  if (!s.length) return null;
  const i = firstEndingAfter(s, (x) => x.top + x.height, y);
  return s[Math.min(i, s.length - 1)] ?? null;
}

/** Which section and row hold an item index. */
export function locate(layout: Layout, index: number): { section: Section; row: Row; cell: Cell } | null {
  const s = layout.sections;
  let lo = 0;
  let hi = s.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (s[mid]!.start <= index) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  const section = s[found];
  if (!section || index >= section.start + section.count) return null;
  const rows = section.rows;
  let a = 0;
  let b = rows.length - 1;
  let r = -1;
  while (a <= b) {
    const mid = (a + b) >> 1;
    if (rows[mid]!.cells[0]!.index <= index) {
      r = mid;
      a = mid + 1;
    } else b = mid - 1;
  }
  const row = rows[r];
  const cell = row?.cells.find((c) => c.index === index);
  return row && cell ? { section, row, cell } : null;
}

/** The content offset of an item's row (to scroll it into view, or to keep it in place when the width changes). */
export function offsetOfIndex(layout: Layout, index: number): number | null {
  const at = locate(layout, index);
  return at ? at.section.top + at.row.top : null;
}

/** The scroll offset that brings an item's row fully into a viewport, changing the scroll as little as it can. */
export function scrollToReveal(layout: Layout, index: number, scrollTop: number, viewport: number, stickyHeader: number): number {
  const at = locate(layout, index);
  if (!at) return scrollTop;
  const top = at.section.top + at.row.top;
  const bottom = top + at.row.height;
  if (top - stickyHeader < scrollTop) return Math.max(0, top - stickyHeader);
  if (bottom > scrollTop + viewport) return bottom - viewport;
  return scrollTop;
}

export type Direction = "left" | "right" | "up" | "down";

/** The item index a keyboard arrow moves to (left and right follow the reading order; up and down stay in the column). */
export function neighbour(layout: Layout, index: number, dir: Direction): number {
  if (dir === "left") return Math.max(0, index - 1);
  if (dir === "right") return Math.min(layout.itemCount - 1, index + 1);
  const at = locate(layout, index);
  if (!at) return index;
  const centre = at.cell.left + at.cell.width / 2;
  const rows = at.section.rows;
  const ri = rows.indexOf(at.row);
  let target: Row | undefined;
  if (dir === "up") {
    target = rows[ri - 1];
    if (!target) {
      const prev = layout.sections[layout.sections.indexOf(at.section) - 1];
      target = prev?.rows[prev.rows.length - 1];
    }
  } else {
    target = rows[ri + 1];
    if (!target) target = layout.sections[layout.sections.indexOf(at.section) + 1]?.rows[0];
  }
  if (!target) return index;
  let best = target.cells[0]!;
  for (const c of target.cells) if (Math.abs(c.left + c.width / 2 - centre) < Math.abs(best.left + best.width / 2 - centre)) best = c;
  return best.index;
}

export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The item indexes whose tiles touch a rectangle (the rubber band), in content coordinates. */
export function cellsInRect(layout: Layout, rect: Rect): number[] {
  const out: number[] = [];
  for (const s of visibleSections(layout, { top: rect.top, bottom: rect.bottom })) {
    for (const row of visibleRows(s, { top: rect.top, bottom: rect.bottom })) {
      const rowTop = s.top + row.top;
      if (rowTop + row.height < rect.top || rowTop > rect.bottom) continue;
      for (const c of row.cells) if (c.left + c.width >= rect.left && c.left <= rect.right) out.push(c.index);
    }
  }
  return out;
}

// ---- labels ------------------------------------------------------------------------------------------------------

/** `2026-10-14` or `2026-10` as a heading in a language ("Wednesday, 14 October 2026" / "October 2026"). */
export function formatSectionLabel(key: string, locale: string): string {
  const m = /^(\d{4})-(\d{2})(?:-(\d{2}))?$/.exec(key);
  if (!m || m[1] === "0000") return "";
  const date = new Date(Number(m[1]), Number(m[2]) - 1, m[3] ? Number(m[3]) : 1);
  const opts: Intl.DateTimeFormatOptions = m[3]
    ? { weekday: "long", day: "numeric", month: "long", year: "numeric" }
    : { month: "long", year: "numeric" };
  try {
    return new Intl.DateTimeFormat(locale, opts).format(date);
  } catch {
    return key;
  }
}

/** A video's length: `0:07`, `12:30`, `1:02:03`. */
export function formatDuration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return "";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h ? `${h}:${pad2(m)}:${pad2(s)}` : `${m}:${pad2(s)}`;
}

/** Whole days left before the trash empties it (at least 0). */
export function daysLeft(expiresAt: string | null, now = Date.now()): number | null {
  if (!expiresAt) return null;
  const ms = Date.parse(expiresAt);
  if (!Number.isFinite(ms)) return null;
  return Math.max(0, Math.ceil((ms - now) / 86_400_000));
}

// ---- sources -----------------------------------------------------------------------------------------------------

/** The source id an item belongs to in the switcher (`device:<id>`, `chat` or `lucina`). */
export function sourceIdOf(item: { device: string | null; source: string }): string {
  if (item.device) return `device:${item.device}`;
  return item.source.startsWith("lucina") ? "lucina" : "chat";
}

// ---- the month scrubber ------------------------------------------------------------------------------------------

export interface MonthCount {
  month: string;
  count: number;
}

export interface ScrubMark {
  month: string;
  /** Position of the start of the month on the track, 0 to 1. */
  at: number;
  /** A year label ("2026") at the first month of a year that has room for it. */
  year: string | null;
}

/** The marks of a scrubber: a month's share of the track is its share of the photos, with a floor so none vanishes. */
export function scrubMarks(months: readonly MonthCount[], trackPx: number, minGapPx = 22): ScrubMark[] {
  const total = months.reduce((n, m) => n + m.count, 0);
  if (!months.length || total <= 0) return [];
  const floor = 1 / Math.max(1, months.length) / 4;
  const weights = months.map((m) => Math.max(m.count / total, floor));
  const sum = weights.reduce((a, b) => a + b, 0);
  const marks: ScrubMark[] = [];
  let cum = 0;
  let lastYearAt = -Infinity;
  let lastYear = "";
  months.forEach((m, i) => {
    const at = cum / sum;
    const year = m.month.slice(0, 4);
    let label: string | null = null;
    if (year !== lastYear) {
      if ((at - lastYearAt) * trackPx >= minGapPx || lastYearAt === -Infinity) {
        label = year;
        lastYearAt = at;
      }
      lastYear = year;
    }
    marks.push({ month: m.month, at, year: label });
    cum += weights[i]!;
  });
  return marks;
}

/** The month under a position of the track (0 to 1). */
export function monthAtFraction(marks: readonly ScrubMark[], f: number): string | null {
  if (!marks.length) return null;
  const x = Math.min(1, Math.max(0, f));
  let lo = 0;
  let hi = marks.length - 1;
  let found = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (marks[mid]!.at <= x) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return marks[found]!.month;
}

/**
 * The section to scroll to for a month `YYYY-MM`: the first one of that month, or when the month has none (a filter
 * hides them all) the first one older than it, else the last one. Null only for an empty layout.
 */
export function sectionForMonth(layout: Layout, month: string): Section | null {
  const s = layout.sections;
  return s.find((x) => x.key.slice(0, 7) <= month) ?? s[s.length - 1] ?? null;
}

/** The first item of the row at a content offset (what to keep in place when the width, and so the rows, change). */
export function indexAtOffset(layout: Layout, y: number): number | null {
  const s = sectionAt(layout, y);
  if (!s || !s.rows.length) return s ? s.start : null;
  const rel = y - s.top;
  const first = firstEndingAfter(s.rows, (r) => r.top + r.height, rel);
  return (s.rows[Math.min(first, s.rows.length - 1)] ?? s.rows[0]!).cells[0]!.index;
}

/** Whether items older than `month` are already loaded (so the month's section, if it has any, exists). */
export function monthReached(layout: Layout, month: string): boolean {
  const last = layout.sections[layout.sections.length - 1];
  return !!last && last.key.slice(0, 7) <= month;
}

// ---- selection ---------------------------------------------------------------------------------------------------

/** The ids from the anchor to the target index, both included, in either direction. */
export function rangeIds(ids: readonly string[], anchor: number, target: number): string[] {
  if (!ids.length) return [];
  const a = Math.max(0, Math.min(anchor, ids.length - 1));
  const b = Math.max(0, Math.min(target, ids.length - 1));
  return ids.slice(Math.min(a, b), Math.max(a, b) + 1);
}

export function toggleId(set: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(set);
  if (!next.delete(id)) next.add(id);
  return next;
}
