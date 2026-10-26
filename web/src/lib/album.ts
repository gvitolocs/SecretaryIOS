// Telegram-style album geometry for an image attachment bubble.
//
//   1       one image, natural aspect ratio
//   2       two side by side
//   3       one large image left, two stacked right
//   4       2 x 2
//   5…10    rows of three (the last row may be short)

export const ALBUM_MAX = 10;

export interface AlbumCell {
  index: number;
  column: number;
  row: number;
  columnSpan: number;
  rowSpan: number;
}

export interface AlbumLayout {
  columns: number;
  rows: number;
  cells: AlbumCell[];
}

export function albumLayout(count: number): AlbumLayout {
  const n = Math.max(0, Math.min(ALBUM_MAX, Math.trunc(count)));
  if (n <= 1) {
    return { columns: 1, rows: n, cells: n ? [{ index: 0, column: 0, row: 0, columnSpan: 1, rowSpan: 1 }] : [] };
  }
  if (n === 2) {
    return { columns: 2, rows: 1, cells: [0, 1].map((index) => ({ index, column: index, row: 0, columnSpan: 1, rowSpan: 1 })) };
  }
  if (n === 3) {
    return {
      columns: 2,
      rows: 2,
      cells: [
        { index: 0, column: 0, row: 0, columnSpan: 1, rowSpan: 2 },
        { index: 1, column: 1, row: 0, columnSpan: 1, rowSpan: 1 },
        { index: 2, column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
      ],
    };
  }
  if (n === 4) {
    return {
      columns: 2,
      rows: 2,
      cells: [0, 1, 2, 3].map((index) => ({ index, column: index % 2, row: Math.floor(index / 2), columnSpan: 1, rowSpan: 1 })),
    };
  }
  return {
    columns: 3,
    rows: Math.ceil(n / 3),
    cells: Array.from({ length: n }, (_, index) => ({
      index,
      column: index % 3,
      row: Math.floor(index / 3),
      columnSpan: 1,
      rowSpan: 1,
    })),
  };
}

/** CSS grid placement for one album cell. */
export function albumCellStyle(cell: AlbumCell): Record<string, string> {
  return {
    "grid-column": `${cell.column + 1} / span ${cell.columnSpan}`,
    "grid-row": `${cell.row + 1} / span ${cell.rowSpan}`,
  };
}
