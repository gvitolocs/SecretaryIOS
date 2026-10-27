import { describe, expect, it } from "vitest";
import { gridMetrics, masonry } from "./keepLayout";

describe("masonry", () => {
  it("fills the first row left to right, then the shortest column", () => {
    // Heights of cards 0..5 in order, three columns.
    const l = masonry([100, 300, 50, 60, 80, 40], 3, 10);
    expect(l.items.map((i) => i.col)).toEqual([0, 1, 2, 2, 0, 2]);
    // Card 3 (60) went under card 2 (50), the shortest column at that point.
    expect(l.items[3]).toEqual({ col: 2, y: 60 });
    // Card 4 went to column 0 (100+10 = 110) rather than 2 (50+10+60+10 = 130).
    expect(l.items[4]).toEqual({ col: 0, y: 110 });
  });

  it("breaks ties to the left-most column, so order reads across rows", () => {
    const l = masonry([10, 10, 10, 10, 10, 10, 10], 3, 4);
    expect(l.items.map((i) => i.col)).toEqual([0, 1, 2, 0, 1, 2, 0]);
  });

  it("measures the grid's height without the trailing gap", () => {
    expect(masonry([100, 50], 2, 16).height).toBe(100);
    expect(masonry([100, 50, 30], 2, 16).height).toBe(100);
    expect(masonry([], 3, 16).height).toBe(0);
  });

  it("never uses fewer than one column", () => {
    expect(masonry([5, 5], 0, 2).items.map((i) => i.col)).toEqual([0, 0]);
  });
});

describe("gridMetrics", () => {
  it("fits 240 px cards with 16 px gaps into the container", () => {
    expect(gridMetrics(1000, false)).toMatchObject({ columns: 3, colWidth: 240, gap: 16, width: 3 * 240 + 2 * 16 });
    expect(gridMetrics(1400, false).columns).toBe(5);
    expect(gridMetrics(700, false).columns).toBe(2);
    expect(gridMetrics(601, false).columns).toBe(2);
    expect(gridMetrics(272, false).columns).toBe(2); // phone widths: two columns
  });

  it("uses one 600 px column in list mode", () => {
    expect(gridMetrics(1400, true)).toMatchObject({ columns: 1, colWidth: 600 });
    expect(gridMetrics(640, true).colWidth).toBe(600);
  });

  it("is two columns on a phone, one in list mode, filling the width", () => {
    const g = gridMetrics(390, false);
    expect(g.columns).toBe(2);
    expect(g.colWidth * 2 + g.gap).toBeCloseTo(390);
    expect(gridMetrics(390, true).columns).toBe(1);
  });
});
