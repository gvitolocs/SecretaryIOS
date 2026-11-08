/**
 * Camera filters (Custom Cast's CameraConfigUI FilterType): sepia, monotone and vignette, each
 * with a strength. Live: a CSS filter on the stage canvases plus a vignette overlay (no extra GPU
 * pass). Photos: the same maths on the snapshot's pixels, so the saved picture matches the view.
 */

export const FILTERS = ['none', 'sepia', 'mono', 'vignette'] as const;
export type FilterId = (typeof FILTERS)[number];

export interface FilterState {
  id: FilterId;
  /** 0..1 */
  strength: number;
}

export function normalizeFilter(f: Partial<FilterState> | null | undefined): FilterState {
  const id = (FILTERS as readonly string[]).includes(f?.id ?? '') ? (f!.id as FilterId) : 'none';
  const s = Number(f?.strength);
  return { id, strength: Number.isFinite(s) ? Math.min(1, Math.max(0, s)) : 0.8 };
}

/** The CSS `filter` for the live view ('' for none and vignette, which is an overlay). */
export function cssFilter(f: FilterState): string {
  if (f.id === 'sepia') return `sepia(${f.strength.toFixed(3)})`;
  if (f.id === 'mono') return `grayscale(${f.strength.toFixed(3)})`;
  return '';
}

/** The vignette overlay's background ('' when off). */
export function vignetteCss(f: FilterState): string {
  if (f.id !== 'vignette' || f.strength <= 0) return '';
  const a = (0.75 * f.strength).toFixed(3);
  return `radial-gradient(ellipse at center, rgba(0,0,0,0) ${Math.round(62 - 22 * f.strength)}%, rgba(0,0,0,${a}) 100%)`;
}

/**
 * Applies the filter in place to RGBA pixels (straight alpha), the CSS Filter Effects maths:
 * sepia and grayscale matrices mixed by strength; the vignette darkens toward the corners.
 */
export function applyFilterPixels(data: Uint8ClampedArray | Uint8Array, w: number, h: number, f: FilterState): void {
  if (f.id === 'none' || f.strength <= 0) return;
  const s = f.strength;
  if (f.id === 'sepia' || f.id === 'mono') {
    const k = 1 - s;
    const m =
      f.id === 'sepia'
        ? [
            0.393 + 0.607 * k, 0.769 - 0.769 * k, 0.189 - 0.189 * k,
            0.349 - 0.349 * k, 0.686 + 0.314 * k, 0.168 - 0.168 * k,
            0.272 - 0.272 * k, 0.534 - 0.534 * k, 0.131 + 0.869 * k,
          ]
        : [
            0.2126 + 0.7874 * k, 0.7152 - 0.7152 * k, 0.0722 - 0.0722 * k,
            0.2126 - 0.2126 * k, 0.7152 + 0.2848 * k, 0.0722 - 0.0722 * k,
            0.2126 - 0.2126 * k, 0.7152 - 0.7152 * k, 0.0722 + 0.9278 * k,
          ];
    for (let i = 0; i < data.length; i += 4) {
      const r = data[i]!;
      const g = data[i + 1]!;
      const b = data[i + 2]!;
      data[i] = Math.min(255, m[0]! * r + m[1]! * g + m[2]! * b);
      data[i + 1] = Math.min(255, m[3]! * r + m[4]! * g + m[5]! * b);
      data[i + 2] = Math.min(255, m[6]! * r + m[7]! * g + m[8]! * b);
    }
    return;
  }
  // Vignette: same falloff as the overlay (ellipse, inner radius shrinks with strength).
  const inner = (62 - 22 * s) / 100;
  const maxA = 0.75 * s;
  const cx = w / 2;
  const cy = h / 2;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      // CSS "ellipse at center" with farthest-corner sizing: the corner is at radius 1.
      const dx = (x + 0.5 - cx) / (cx * Math.SQRT2);
      const dy = (y + 0.5 - cy) / (cy * Math.SQRT2);
      const d = Math.sqrt(dx * dx + dy * dy);
      if (d <= inner) continue;
      const a = maxA * Math.min(1, (d - inner) / (1 - inner));
      const i = (y * w + x) * 4;
      const k = 1 - a;
      data[i] = data[i]! * k;
      data[i + 1] = data[i + 1]! * k;
      data[i + 2] = data[i + 2]! * k;
      // Transparent canvases: the darkening also shows where nothing was drawn.
      if (data[i + 3]! < 255) data[i + 3] = Math.min(255, data[i + 3]! + a * (255 - data[i + 3]!));
    }
  }
}

/** Snapshot post-process: decode the PNG, filter, re-encode. Returns the input when there is nothing to do. */
export async function filterBlob(blob: Blob, f: FilterState): Promise<Blob> {
  if (f.id === 'none' || f.strength <= 0 || typeof document === 'undefined') return blob;
  const bmp = await loadImage(blob);
  const c = document.createElement('canvas');
  c.width = bmp.width;
  c.height = bmp.height;
  const g = c.getContext('2d');
  if (!g) return blob;
  g.drawImage(bmp, 0, 0);
  const img = g.getImageData(0, 0, c.width, c.height);
  applyFilterPixels(img.data, c.width, c.height, f);
  g.putImageData(img, 0, 0);
  return new Promise((res) => c.toBlob((b) => res(b ?? blob), 'image/png'));
}

export function loadImage(blob: Blob): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error('image decode failed'));
    };
    img.src = url;
  });
}
