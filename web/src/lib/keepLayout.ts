// The masonry grid: Keep places each card, in order, into the currently shortest column (the
// left-most on ties), so the order reads left to right across rows. CSS columns would read top to
// bottom per column, so the layout is computed here from the measured card heights.

export interface Placed {
  col: number;
  y: number;
}

export interface Layout {
  items: Placed[];
  height: number;
}

export function masonry(heights: number[], columns: number, gap: number): Layout {
  const cols = Math.max(1, Math.floor(columns));
  const tops = new Array<number>(cols).fill(0);
  const items: Placed[] = [];
  for (const h of heights) {
    let c = 0;
    for (let k = 1; k < cols; k++) if (tops[k]! < tops[c]!) c = k;
    items.push({ col: c, y: tops[c]! });
    tops[c] = tops[c]! + h + gap;
  }
  const tallest = Math.max(0, ...tops);
  return { items, height: tallest > 0 ? tallest - gap : 0 };
}

export interface GridMetrics {
  columns: number;
  colWidth: number;
  gap: number;
  /** Width of the whole grid, centred in the page. */
  width: number;
}

export const CARD_WIDTH = 240;
export const CARD_GAP = 16;
export const LIST_WIDTH = 600;
export const PHONE_MAX = 600;

/** Column count and card width for a container: 240 px cards and 16 px gaps; a 600 px column in list mode. */
export function gridMetrics(containerWidth: number, list: boolean): GridMetrics {
  const w = Math.max(0, containerWidth);
  if (w <= PHONE_MAX) {
    const gap = 8;
    const columns = list ? 1 : 2;
    return { columns, gap, colWidth: Math.max(0, (w - gap * (columns - 1)) / columns), width: w };
  }
  if (list) {
    const width = Math.min(LIST_WIDTH, w);
    return { columns: 1, gap: CARD_GAP, colWidth: width, width };
  }
  const columns = Math.max(1, Math.floor((w + CARD_GAP) / (CARD_WIDTH + CARD_GAP)));
  return { columns, gap: CARD_GAP, colWidth: CARD_WIDTH, width: columns * CARD_WIDTH + (columns - 1) * CARD_GAP };
}
