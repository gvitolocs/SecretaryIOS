import { describe, expect, it } from "vitest";
import { ALBUM_MAX, albumCellStyle, albumLayout } from "./album";

describe("albumLayout", () => {
  it("keeps one image at its natural ratio", () => {
    const layout = albumLayout(1);
    expect(layout).toMatchObject({ columns: 1, rows: 1 });
    expect(layout.cells).toEqual([{ index: 0, column: 0, row: 0, columnSpan: 1, rowSpan: 1 }]);
  });

  it.each([
    [2, 2, 1],
    [3, 2, 2],
    [4, 2, 2],
    [5, 3, 2],
    [6, 3, 2],
    [7, 3, 3],
    [8, 3, 3],
    [9, 3, 3],
    [10, 3, 4],
  ])("lays out %i images as a %ix%i grid", (count, columns, rows) => {
    const layout = albumLayout(count);
    expect(layout.columns).toBe(columns);
    expect(layout.rows).toBe(rows);
    expect(layout.cells).toHaveLength(count);
  });

  it("gives the first image both rows in a three-image album", () => {
    const [first, ...rest] = albumLayout(3).cells;
    expect(first).toMatchObject({ column: 0, row: 0, columnSpan: 1, rowSpan: 2 });
    expect(rest.map((c) => [c.column, c.row])).toEqual([[1, 0], [1, 1]]);
  });

  it("never overlaps cells or leaves the grid", () => {
    for (let count = 1; count <= ALBUM_MAX; count++) {
      const layout = albumLayout(count);
      const used = new Set<string>();
      for (const cell of layout.cells) {
        for (let y = 0; y < cell.rowSpan; y++) {
          for (let x = 0; x < cell.columnSpan; x++) {
            const key = `${cell.column + x}:${cell.row + y}`;
            expect(used.has(key)).toBe(false);
            used.add(key);
            expect(cell.column + x).toBeLessThan(layout.columns);
            expect(cell.row + y).toBeLessThan(layout.rows);
          }
        }
      }
      expect(used.size).toBe(layout.cells.reduce((sum, cell) => sum + cell.columnSpan * cell.rowSpan, 0));
    }
  });

  it("caps the album at ten and handles empty/negative counts", () => {
    expect(albumLayout(12).cells).toHaveLength(ALBUM_MAX);
    expect(albumLayout(0)).toEqual({ columns: 1, rows: 0, cells: [] });
    expect(albumLayout(-3).cells).toEqual([]);
  });

  it("turns a cell into one-based CSS grid placement", () => {
    expect(albumCellStyle({ index: 0, column: 1, row: 0, columnSpan: 1, rowSpan: 2 })).toEqual({
      "grid-column": "2 / span 1",
      "grid-row": "1 / span 2",
    });
  });
});
