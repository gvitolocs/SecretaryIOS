import { describe, expect, it } from "vitest";
import {
  aspectOf,
  cellsInRect,
  daysLeft,
  DEFAULT_OPTIONS,
  formatDuration,
  formatSectionLabel,
  indexAtOffset,
  layoutGallery,
  locate,
  monthAtFraction,
  monthReached,
  neighbour,
  offsetOfIndex,
  rangeIds,
  scrollToReveal,
  scrubMarks,
  sectionAt,
  sectionForMonth,
  sectionKey,
  sourceIdOf,
  toggleId,
  visibleRows,
  visibleSections,
  windowFor,
  windowIndexRange,
  type Layout,
  type LayoutItem,
  type LayoutOptions,
} from "./galleryLayout";

/** A noon on a local calendar day, so the day is the same in every time zone. */
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m - 1, d, h).toISOString();

function item(day: [number, number, number], w: number | null = 4000, h: number | null = 3000, rotation = 0, hour = 12): LayoutItem {
  return { takenAt: at(...day, hour), addedAt: at(...day, hour), width: w, height: h, rotation };
}

/** `n` items per day over `days` days, newest first, with mixed shapes. */
function library(days: number, perDay: number, start: [number, number, number] = [2026, 10, 28]): LayoutItem[] {
  const shapes: [number, number][] = [[4000, 3000], [3000, 4000], [1920, 1080], [3000, 3000]];
  const out: LayoutItem[] = [];
  const base = new Date(start[0], start[1] - 1, start[2], 12);
  for (let d = 0; d < days; d++) {
    const day = new Date(base.getTime() - d * 86_400_000);
    for (let i = 0; i < perDay; i++) {
      const [w, h] = shapes[(d * perDay + i) % shapes.length]!;
      out.push(item([day.getFullYear(), day.getMonth() + 1, day.getDate()], w, h, 0, 20 - Math.min(i, 8)));
    }
  }
  return out;
}

const opts = (over: Partial<LayoutOptions> = {}): LayoutOptions => ({ ...DEFAULT_OPTIONS, ...over });

describe("section keys and shapes", () => {
  it("groups by local day or month, using the added date when nothing says when it was taken", () => {
    expect(sectionKey(item([2026, 10, 14]), "day")).toBe("2026-10-14");
    expect(sectionKey(item([2026, 10, 14]), "month")).toBe("2026-10");
    expect(sectionKey({ takenAt: null, addedAt: at(2025, 1, 2) }, "day")).toBe("2025-01-02");
    expect(sectionKey({ takenAt: null, addedAt: "nonsense" }, "day")).toBe("0000-00");
  });

  it("swaps width and height for a quarter turn and keeps the ratio within range", () => {
    expect(aspectOf({ width: 4000, height: 3000, rotation: 0 })).toBeCloseTo(4 / 3);
    expect(aspectOf({ width: 4000, height: 3000, rotation: 90 })).toBeCloseTo(3 / 4);
    expect(aspectOf({ width: 4000, height: 3000, rotation: 180 })).toBeCloseTo(4 / 3);
    expect(aspectOf({ width: null, height: null, rotation: 0 })).toBe(1);
    expect(aspectOf({ width: 12000, height: 1000, rotation: 0 })).toBe(2.4);
    expect(aspectOf({ width: 1000, height: 12000, rotation: 0 })).toBe(0.55);
  });
});

describe("layoutGallery", () => {
  it("fills every row but the last of each section to the full width, near the target height", () => {
    const o = opts({ width: 1000, target: 200, gap: 4 });
    const layout = layoutGallery(library(3, 17), o);
    expect(layout.sections.map((s) => s.key)).toEqual(["2026-10-28", "2026-10-27", "2026-10-26"]);
    for (const s of layout.sections) {
      s.rows.forEach((row, i) => {
        const last = row.cells[row.cells.length - 1]!;
        const right = last.left + last.width;
        if (i < s.rows.length - 1) {
          expect(right).toBeCloseTo(1000, 3);
          expect(row.height).toBeGreaterThan(110);
          expect(row.height).toBeLessThan(330);
        } else {
          expect(right).toBeLessThanOrEqual(1000.001);
          expect(row.height).toBeLessThanOrEqual(200.001);
        }
      });
    }
  });

  it("places every item exactly once, in order, without overlaps", () => {
    const items = library(5, 23);
    const layout = layoutGallery(items, opts({ width: 760, gap: 3, target: 160 }));
    const seen: number[] = [];
    for (const s of layout.sections) {
      expect(s.count).toBe(s.rows.reduce((n, r) => n + r.cells.length, 0));
      let prevBottom = opts().headerHeight - 1;
      for (const row of s.rows) {
        expect(row.top).toBeGreaterThan(prevBottom);
        prevBottom = row.top + row.height;
        let prevRight = -1;
        for (const c of row.cells) {
          expect(c.left).toBeGreaterThan(prevRight);
          prevRight = c.left + c.width;
          seen.push(c.index);
        }
      }
      expect(prevBottom).toBeLessThanOrEqual(s.height);
    }
    expect(seen).toEqual(items.map((_, i) => i));
    expect(layout.height).toBe(layout.sections.reduce((n, s) => n + s.height, 0));
  });

  it("lays out squares in whole columns on a phone", () => {
    const o = opts({ mode: "square", width: 375, gap: 2, target: 118 });
    const layout = layoutGallery(library(1, 10), o);
    const rows = layout.sections[0]!.rows;
    expect(rows).toHaveLength(4); // 3 columns
    expect(rows[0]!.cells).toHaveLength(3);
    expect(rows[3]!.cells).toHaveLength(1);
    const size = (375 - 2 * 2) / 3;
    expect(rows[0]!.height).toBeCloseTo(size);
    [0, size + 2, 2 * (size + 2)].forEach((left, i) => expect(rows[0]!.cells[i]!.left).toBeCloseTo(left, 5));
  });

  it("starts a new section when the day changes, and by month when asked to", () => {
    const items = [item([2026, 10, 3]), item([2026, 10, 3]), item([2026, 10, 2]), item([2026, 9, 30]), item([2026, 9, 1])];
    expect(layoutGallery(items, opts()).sections.map((s) => [s.key, s.count])).toEqual([["2026-10-03", 2], ["2026-10-02", 1], ["2026-09-30", 1], ["2026-09-01", 1]]);
    expect(layoutGallery(items, opts({ groupBy: "month" })).sections.map((s) => [s.key, s.count])).toEqual([["2026-10", 3], ["2026-09", 2]]);
  });

  it("gives a repeated key its own section id", () => {
    const items = [item([2026, 10, 3]), item([2026, 10, 2]), item([2026, 10, 3])];
    expect(layoutGallery(items, opts()).sections.map((s) => s.id)).toEqual(["2026-10-03", "2026-10-02", "2026-10-03#1"]);
  });

  it("turns a rotated picture's tile on its side", () => {
    const o = opts({ width: 1000, target: 200 });
    const flat = layoutGallery([item([2026, 10, 1])], o).sections[0]!.rows[0]!.cells[0]!;
    const turned = layoutGallery([item([2026, 10, 1], 4000, 3000, 90)], o).sections[0]!.rows[0]!.cells[0]!;
    expect(flat.width).toBeGreaterThan(turned.width);
  });

  it("keeps the earlier sections when items are appended, and matches a fresh layout", () => {
    const all = library(40, 9);
    const o = opts({ width: 900 });
    const first = layoutGallery(all.slice(0, 100), o);
    const next = layoutGallery(all.slice(0, 220), o, first);
    // Same section objects (the DOM keeps them), except the last one, which grew.
    for (let i = 0; i < first.sections.length - 1; i++) expect(next.sections[i]).toBe(first.sections[i]);
    expect(next.sections[first.sections.length - 1]).not.toBe(first.sections[first.sections.length - 1]);
    expect(next).toEqual(layoutGallery(all.slice(0, 220), o));
  });

  it("ignores the previous layout when the options changed", () => {
    const all = library(10, 9);
    const first = layoutGallery(all, opts({ width: 900 }));
    const next = layoutGallery(all, opts({ width: 500 }), first);
    expect(next.sections[0]).not.toBe(first.sections[0]);
    expect(next).toEqual(layoutGallery(all, opts({ width: 500 })));
  });

  it("handles no items", () => {
    const layout = layoutGallery([], opts());
    expect(layout).toMatchObject({ sections: [], height: 0, itemCount: 0 });
    expect(visibleSections(layout, windowFor(0, 800))).toEqual([]);
    expect(windowIndexRange(layout, windowFor(0, 800))).toBeNull();
    expect(sectionAt(layout, 10)).toBeNull();
  });
});

describe("the virtual window", () => {
  const items = library(2000, 10); // 20,000 items
  const o = opts({ width: 1100, target: 210 });
  const layout = layoutGallery(items, o);

  const drawn = (scrollTop: number, viewport = 800) => {
    const w = windowFor(scrollTop, viewport, 2);
    let rows = 0;
    let cells = 0;
    for (const s of visibleSections(layout, w)) for (const r of visibleRows(s, w)) (rows++, (cells += r.cells.length));
    return { rows, cells, w };
  };

  it("keeps only a few screens of 20,000 items in the DOM, wherever the scroll is", () => {
    expect(layout.itemCount).toBe(20_000);
    for (const top of [0, 1234, layout.height / 2, layout.height - 800]) {
      const { cells, rows } = drawn(top);
      expect(cells).toBeGreaterThan(0);
      expect(cells).toBeLessThan(260);
      expect(rows).toBeLessThan(60);
    }
  });

  it("covers the viewport and two screens either side", () => {
    const top = layout.height / 3;
    const { w } = drawn(top);
    expect(w.top).toBe(top - 1600);
    expect(w.bottom).toBe(top + 2400);
    // Every cell whose row touches the viewport is drawn.
    const drawnIndexes = new Set<number>();
    for (const s of visibleSections(layout, w)) for (const r of visibleRows(s, w)) for (const c of r.cells) drawnIndexes.add(c.index);
    for (const s of layout.sections) {
      if (s.top + s.height < top || s.top > top + 800) continue;
      for (const r of s.rows) if (s.top + r.top + r.height > top && s.top + r.top < top + 800) for (const c of r.cells) expect(drawnIndexes.has(c.index)).toBe(true);
    }
  });

  it("does not start before the top of the content", () => {
    expect(windowFor(100, 800, 2).top).toBe(0);
    expect(visibleSections(layout, windowFor(0, 800))[0]).toBe(layout.sections[0]);
  });

  it("returns the index range of what is drawn", () => {
    const r = windowIndexRange(layout, windowFor(0, 800))!;
    expect(r.first).toBe(0);
    expect(r.last).toBeGreaterThan(20);
    const far = windowIndexRange(layout, windowFor(layout.height - 800, 800))!;
    expect(far.last).toBe(19_999);
  });

  it("finds the section at an offset, and an item's row", () => {
    const s = layout.sections[500]!;
    expect(sectionAt(layout, s.top + 3)).toBe(s);
    expect(sectionAt(layout, layout.height + 5)).toBe(layout.sections[layout.sections.length - 1]);
    const found = locate(layout, 5001)!;
    expect(found.cell.index).toBe(5001);
    expect(found.section.start).toBeLessThanOrEqual(5001);
    expect(offsetOfIndex(layout, 5001)).toBe(found.section.top + found.row.top);
    expect(locate(layout, 20_000)).toBeNull();
    expect(indexAtOffset(layout, found.section.top + found.row.top + 1)).toBe(found.row.cells[0]!.index);
  });

  it("scrolls the least that shows a row, leaving room for the sticky header", () => {
    const y = offsetOfIndex(layout, 3000)!;
    expect(scrollToReveal(layout, 3000, y - 5, 800, 42)).toBe(y - 42); // just under the sticky header: nudge up
    expect(scrollToReveal(layout, 3000, y - 100, 800, 42)).toBe(y - 100); // already visible
    expect(scrollToReveal(layout, 3000, y + 2000, 800, 42)).toBe(y - 42); // above: top of its row under the header
    const row = locate(layout, 3000)!.row;
    expect(scrollToReveal(layout, 3000, y - 2000, 800, 42)).toBe(y + row.height - 800); // below: bottom edge at the bottom
  });

  it("maps a rubber band to the tiles it touches", () => {
    const s = layout.sections[3]!;
    const row = s.rows[1]!;
    const rowTop = s.top + row.top;
    const first = row.cells[0]!;
    const hit = cellsInRect(layout, { left: first.left + 2, right: first.left + 4, top: rowTop + 2, bottom: rowTop + 4 });
    expect(hit).toEqual([first.index]);
    const both = cellsInRect(layout, { left: 0, right: o.width, top: rowTop + 2, bottom: rowTop + 4 });
    expect(both).toEqual(row.cells.map((c) => c.index));
    expect(cellsInRect(layout, { left: 0, right: 10, top: layout.height + 10, bottom: layout.height + 20 })).toEqual([]);
  });
});

describe("keyboard neighbours", () => {
  const layout: Layout = layoutGallery(library(3, 12), opts({ width: 800, target: 180 }));

  it("moves left and right in reading order, clamped to the ends", () => {
    expect(neighbour(layout, 0, "left")).toBe(0);
    expect(neighbour(layout, 5, "right")).toBe(6);
    expect(neighbour(layout, 35, "right")).toBe(35);
  });

  it("moves up and down to the tile nearest in the column, across sections", () => {
    const down = neighbour(layout, 0, "down");
    const from = locate(layout, 0)!;
    const to = locate(layout, down)!;
    expect(to.row).not.toBe(from.row);
    expect(neighbour(layout, down, "up")).toBe(0);
    const lastOfFirstSection = layout.sections[0]!.count - 1;
    const next = neighbour(layout, lastOfFirstSection, "down");
    expect(locate(layout, next)!.section).toBe(layout.sections[1]);
    expect(neighbour(layout, 0, "up")).toBe(0);
    expect(neighbour(layout, 35, "down")).toBe(35);
  });
});

describe("labels", () => {
  it("formats a section heading in a language", () => {
    expect(formatSectionLabel("2026-10-14", "en")).toBe(
      new Intl.DateTimeFormat("en", { weekday: "long", day: "numeric", month: "long", year: "numeric" }).format(new Date(2026, 9, 14)),
    );
    expect(formatSectionLabel("2026-10-14", "en")).toContain("Wednesday");
    expect(formatSectionLabel("2026-10", "it").toLowerCase()).toContain("ottobre");
    expect(formatSectionLabel("2026-10", "da").toLowerCase()).toContain("oktober");
    expect(formatSectionLabel("0000-00", "en")).toBe("");
    expect(formatSectionLabel("garbage", "en")).toBe("");
  });

  it("formats a video's length", () => {
    expect(formatDuration(7_000)).toBe("0:07");
    expect(formatDuration(750_000)).toBe("12:30");
    expect(formatDuration(3_723_000)).toBe("1:02:03");
    expect(formatDuration(null)).toBe("");
    expect(formatDuration(-5)).toBe("");
  });

  it("counts the days left in the trash", () => {
    const now = Date.parse("2026-10-20T12:00:00Z");
    expect(daysLeft("2026-10-30T12:00:00Z", now)).toBe(10);
    expect(daysLeft("2026-10-20T13:00:00Z", now)).toBe(1);
    expect(daysLeft("2026-10-19T12:00:00Z", now)).toBe(0);
    expect(daysLeft(null, now)).toBeNull();
    expect(daysLeft("nope", now)).toBeNull();
  });

  it("names the source of an item", () => {
    expect(sourceIdOf({ device: "mac-1", source: "mac-upload" })).toBe("device:mac-1");
    expect(sourceIdOf({ device: null, source: "lucina-generated" })).toBe("lucina");
    expect(sourceIdOf({ device: null, source: "ios-chat" })).toBe("chat");
  });
});

describe("the month scrubber", () => {
  const months = [
    { month: "2026-10", count: 600 },
    { month: "2026-09", count: 300 },
    { month: "2025-12", count: 90 },
    { month: "2025-11", count: 10 },
  ];

  it("gives each month a share of the track by its count, starting at the top, and labels the years", () => {
    const marks = scrubMarks(months, 600);
    expect(marks.map((m) => m.month)).toEqual(months.map((m) => m.month));
    expect(marks[0]!.at).toBe(0);
    for (let i = 1; i < marks.length; i++) expect(marks[i]!.at).toBeGreaterThan(marks[i - 1]!.at);
    expect(marks.at(-1)!.at).toBeLessThan(1);
    expect(marks.filter((m) => m.year).map((m) => m.year)).toEqual(["2026", "2025"]);
  });

  it("keeps a floor so a small month stays reachable", () => {
    const marks = scrubMarks([{ month: "2026-02", count: 100000 }, { month: "2026-01", count: 1 }], 400);
    expect((1 - marks[1]!.at) * 400).toBeGreaterThan(4);
  });

  it("drops a year label that would sit on the previous one", () => {
    const marks = scrubMarks([{ month: "2026-01", count: 1 }, { month: "2025-12", count: 1000 }], 100);
    expect(marks.filter((m) => m.year).map((m) => m.year)).toEqual(["2026"]);
  });

  it("finds the month under a position", () => {
    const marks = scrubMarks(months, 600);
    expect(monthAtFraction(marks, 0)).toBe("2026-10");
    expect(monthAtFraction(marks, 0.5)).toBe("2026-10");
    expect(monthAtFraction(marks, marks[1]!.at + 0.001)).toBe("2026-09");
    expect(monthAtFraction(marks, 1)).toBe("2025-11");
    expect(monthAtFraction(marks, -3)).toBe("2026-10");
    expect(monthAtFraction([], 0.5)).toBeNull();
    expect(scrubMarks([], 600)).toEqual([]);
  });

  it("finds the section for a month, and whether the month is loaded", () => {
    const items = [item([2026, 10, 5]), item([2026, 10, 2]), item([2026, 8, 20]), item([2026, 8, 1])];
    const layout = layoutGallery(items, opts());
    expect(sectionForMonth(layout, "2026-10")!.key).toBe("2026-10-05");
    expect(sectionForMonth(layout, "2026-08")!.key).toBe("2026-08-20");
    // No picture in September: the next older one.
    expect(sectionForMonth(layout, "2026-09")!.key).toBe("2026-08-20");
    // Older than everything loaded: the last section.
    expect(sectionForMonth(layout, "2019-01")!.key).toBe("2026-08-01");
    // Loaded down to August: October and August are in, July is further down.
    expect(monthReached(layout, "2026-10")).toBe(true);
    expect(monthReached(layout, "2026-08")).toBe(true);
    expect(monthReached(layout, "2026-07")).toBe(false);
    expect(monthReached(layoutGallery([], opts()), "2026-07")).toBe(false);
  });
});

describe("selection helpers", () => {
  const ids = ["a", "b", "c", "d", "e"];

  it("selects a range from the anchor in either direction", () => {
    expect(rangeIds(ids, 1, 3)).toEqual(["b", "c", "d"]);
    expect(rangeIds(ids, 3, 1)).toEqual(["b", "c", "d"]);
    expect(rangeIds(ids, 2, 2)).toEqual(["c"]);
    expect(rangeIds(ids, -4, 99)).toEqual(ids);
    expect(rangeIds([], 0, 3)).toEqual([]);
  });

  it("toggles an id without changing the set it was given", () => {
    const set = new Set(["a"]);
    const added = toggleId(set, "b");
    expect([...added]).toEqual(["a", "b"]);
    expect([...set]).toEqual(["a"]);
    expect([...toggleId(added, "a")]).toEqual(["b"]);
  });
});
