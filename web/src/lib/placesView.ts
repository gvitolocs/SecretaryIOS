// Places (views/GalleryPlaces.tsx): the numbers between the map and lucina-media's `items/places`. Pure, so the
// rules (what the server is asked for, what a cluster click opens) are tested without a map.

import type { BBox, PlaceCluster } from "../api/gallery";

/** What the map shows now. */
export interface Viewport {
  bbox: BBox;
  /** The MapLibre zoom (512 px tiles). */
  zoom: number;
}

const wrapLon = (lon: number): number => ((((lon + 180) % 360) + 360) % 360) - 180;
const clamp = (x: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, x));

/**
 * The server's window for a map's bounds. A map zoomed out past the world is the whole world; a window that crosses
 * the date line comes back as `west > east`, which the server understands; latitudes stay inside the Mercator range.
 */
export function viewportBBox(west: number, south: number, east: number, north: number): BBox {
  const s = clamp(Math.min(south, north), -85.0511, 85.0511);
  const n = clamp(Math.max(south, north), -85.0511, 85.0511);
  if (east - west >= 360) return [-180, s, 180, n];
  const w = wrapLon(west);
  const e = wrapLon(east);
  // 180 itself wraps to -180: a window ending exactly on the date line still ends at 180.
  return [w, s, east === 180 || (e === -180 && east > west) ? 180 : e, n];
}

/**
 * The zoom the server clusters at. The server's cells are `CELL_PX` wide on a 256 px-tile map; MapLibre's zoom counts
 * 512 px tiles, one level lower for the same picture, so the same screen size takes one more.
 */
export function serverZoom(mapZoom: number): number {
  return clamp(mapZoom + 1, 0, 24);
}

/** Two viewports that ask the server the same question: same zoom (to a quarter) and nearly the same window. */
export function sameQuery(a: Viewport | null, b: Viewport | null): boolean {
  if (!a || !b) return a === b;
  if (Math.round(a.zoom * 4) !== Math.round(b.zoom * 4)) return false;
  const span = Math.max(1e-6, Math.abs(a.bbox[2] - a.bbox[0]), Math.abs(a.bbox[3] - a.bbox[1]));
  return a.bbox.every((v, i) => Math.abs(v - b.bbox[i]!) < span * 0.02);
}

/**
 * The window that lists exactly a cluster's pictures in the grid (`items?bbox=`), a hair wider than the pictures'
 * own box so that a cluster of one (a point) and rounded coordinates still match.
 */
export function clusterWindow(c: Pick<PlaceCluster, "bbox">): string {
  const pad = 0.00002;
  const [w, s, e, n] = c.bbox;
  const round = (x: number) => Number(x.toFixed(6));
  return [round(Math.max(-180, w - pad)), round(Math.max(-90, s - pad)), round(Math.min(180, e + pad)), round(Math.min(90, n + pad))].join(",");
}

/** The grid link for a cluster. */
export function clusterHref(c: Pick<PlaceCluster, "bbox">): string {
  return `/gallery?bbox=${encodeURIComponent(clusterWindow(c))}`;
}

/** "Copenhagen", "Rome, Lazio" or nothing, from the cover picture's place. */
export function clusterName(c: Pick<PlaceCluster, "cover">): string {
  const p = c.cover.place;
  if (!p) return "";
  const parts = [p.name, p.admin1].filter((x): x is string => !!x);
  return parts.length === 2 && parts[0] === parts[1] ? parts[0]! : parts.join(", ");
}

/** "9", "1.2k", "34k": a count that fits a map badge. */
export function badgeCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(Math.floor(n / 100) / 10).toString().replace(/\.0$/, "")}k`;
  return `${Math.floor(n / 1000)}k`;
}
