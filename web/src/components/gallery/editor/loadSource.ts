// Getting a picture into the editor: fetch the working copy, decode it in the browser and hand back RGBA pixels at
// most `PREVIEW_PX` on the long side (the preview renders on this downscaled copy; the server renders the full-size
// derivative from the original), plus a small copy for the filter tiles.

export const PREVIEW_PX = 1600;
/** The copy rendered while a control is being dragged: a smaller picture follows the finger faster. */
export const DRAFT_PX = 960;
export const TILE_PX = 160;

export interface Raster {
  width: number;
  height: number;
  rgba: Uint8ClampedArray;
}

export interface LoadedSource {
  full: Raster;
  /** At most `DRAFT_PX` on the long side; the same picture as `full` when that is already small. */
  draft: Raster;
  tile: Raster;
}

/** The size of a `w` x `h` picture scaled down so its long side is at most `max` (never scaled up). */
export function scaleToFit(w: number, h: number, max: number): { width: number; height: number } {
  const long = Math.max(w, h);
  if (long <= max) return { width: w, height: h };
  const s = max / long;
  return { width: Math.max(1, Math.round(w * s)), height: Math.max(1, Math.round(h * s)) };
}

async function decode(blob: Blob): Promise<{ source: CanvasImageSource; width: number; height: number; close: () => void }> {
  if (typeof createImageBitmap === "function") {
    try {
      const bitmap = await createImageBitmap(blob);
      return { source: bitmap, width: bitmap.width, height: bitmap.height, close: () => bitmap.close() };
    } catch {
      // Fall through to an <img>: some browsers refuse a format here that an image element reads.
    }
  }
  const href = URL.createObjectURL(blob);
  const img = new Image();
  img.src = href;
  try {
    await img.decode();
  } catch (err) {
    URL.revokeObjectURL(href);
    throw err;
  }
  return { source: img, width: img.naturalWidth, height: img.naturalHeight, close: () => URL.revokeObjectURL(href) };
}

function raster(source: CanvasImageSource, from: { width: number; height: number }, max: number): Raster {
  const size = scaleToFit(from.width, from.height, max);
  const canvas = document.createElement("canvas");
  canvas.width = size.width;
  canvas.height = size.height;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("no 2d canvas");
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(source, 0, 0, size.width, size.height);
  const data = ctx.getImageData(0, 0, size.width, size.height);
  return { width: size.width, height: size.height, rgba: data.data };
}

export async function loadSource(url: string, signal?: AbortSignal): Promise<LoadedSource> {
  const response = await fetch(url, { signal, credentials: "same-origin" });
  if (!response.ok) throw new Error(`picture ${response.status}`);
  const decoded = await decode(await response.blob());
  try {
    const full = raster(decoded.source, decoded, PREVIEW_PX);
    const draft = Math.max(full.width, full.height) > DRAFT_PX * 1.25 ? raster(decoded.source, decoded, DRAFT_PX) : full;
    return { full, draft, tile: raster(decoded.source, decoded, TILE_PX) };
  } finally {
    decoded.close();
  }
}
