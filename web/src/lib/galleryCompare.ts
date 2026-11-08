// Server copy / Device copy and the compare view (components/gallery/compare/*): the pure rules, no UI.

import type { DeviceUpload, ItemRenditions, RenditionName, StoredFile } from "../api/gallery";
import type { CoreView, ViewRequest } from "./imageCore";

/** Which stored form a pane shows: the current original, an older version, or a display rendition. */
export type SourceId = "original" | `version:${number}` | RenditionName;

export interface CompareSource {
  id: SourceId;
  file: StoredFile;
  /** The browser can draw it as an `<img>` (HEIC and RAW can't; JPEG XL only where supported). */
  decodable: boolean;
  /** For `version:N`. */
  version?: number;
}

const BROWSER_IMAGE = new Set(["image/jpeg", "image/png", "image/webp", "image/gif", "image/avif", "image/bmp"]);

/** Whether an `<img>` can draw this type here. */
export function decodable(mime: string, jxl: boolean): boolean {
  const m = mime.toLowerCase();
  return BROWSER_IMAGE.has(m) || (m === "image/jxl" && jxl);
}

/** Display renditions worth comparing, best first. The poster belongs to videos, the grid tile is a crop. */
const RENDITION_ORDER: RenditionName[] = ["preview", "large", "thumb", "device_preview", "grid"];

/**
 * Every form of a photo a pane can be set to: the original, the older versions (newest first), then the
 * renditions. Sources the browser can't draw are listed (so the pane can say why) but marked `decodable: false`.
 */
export function compareSources(r: ItemRenditions, jxl: boolean): CompareSource[] {
  const out: CompareSource[] = [{ id: "original", file: r.original, decodable: decodable(r.original.mime, jxl) && r.original.reachable }];
  for (const v of [...r.versions].sort((a, b) => b.version - a.version)) {
    out.push({ id: `version:${v.version}`, file: v, decodable: decodable(v.mime, jxl) && v.reachable, version: v.version });
  }
  for (const name of RENDITION_ORDER) {
    const rendition = r.renditions.find((x) => x.name === name);
    if (rendition) out.push({ id: name, file: rendition, decodable: decodable(rendition.mime, jxl) });
  }
  return out;
}

/**
 * The two sources a compare opens on: the server's original against the best display rendition; when the original
 * can't be drawn (HEIC) the phone's own rendering stands in for it. `null` when fewer than two forms can be drawn.
 */
export function defaultPair(sources: CompareSource[]): [SourceId, SourceId] | null {
  const ok = sources.filter((s) => s.decodable);
  if (ok.length < 2) return null;
  const pick = (...ids: SourceId[]) => ids.map((id) => ok.find((s) => s.id === id)).find((s) => s !== undefined);
  const left = pick("original", "device_preview", "large") ?? ok[0]!;
  const right = ["preview", "large", "thumb"].map((id) => pick(id as SourceId)).find((s) => s && s.id !== left.id) ?? ok.find((s) => s.id !== left.id)!;
  return [left.id, right.id];
}

// ---- the view the two panes share --------------------------------------------------------------------------------

/**
 * The core request that puts `other` where `master` is: the same relative zoom and the same offset in points,
 * clamped for `other`'s own picture. Both panes have the same viewport, so equal scale and offset show the same part
 * of two pictures of the same shape, whatever their pixel sizes.
 */
export function followRequest(master: CoreView, other: CoreView): ViewRequest {
  return {
    view: { ...other, scale: master.scale, offset_x: master.offset_x, offset_y: master.offset_y },
    gesture: { op: "resize", image_w: other.image_w, image_h: other.image_h, viewport_w: other.viewport_w, viewport_h: other.viewport_h },
  };
}

export const SPLIT_MIN = 0.02;
export const SPLIT_MAX = 0.98;
export const SPLIT_STEP = 0.05;

/** The slider's position kept inside the stage (a divider can't leave one side with nothing to see), to a tenth of a percent. */
export function clampSplit(x: number): number {
  return Number.isFinite(x) ? Math.round(Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, x)) * 1000) / 1000 : 0.5;
}

/** The divider as a percentage with no floating-point tail (0.55 -> 55). */
export function splitPercent(x: number): number {
  return Math.round(x * 1000) / 10;
}

/** Where a pointer at `clientX` puts the divider of a stage spanning `left`..`left + width`. */
export function splitFromPointer(clientX: number, left: number, width: number): number {
  return width > 0 ? clampSplit((clientX - left) / width) : 0.5;
}

// ---- the Server copy / Device copy rows ---------------------------------------------------------------------------

/** "heic" -> "HEIC", "jxl" -> "JPEG XL". */
export function formatName(format: string): string {
  const f = format.toLowerCase();
  if (f === "jxl") return "JPEG XL";
  if (f === "jpeg") return "JPEG";
  return f.toUpperCase();
}

/** First and last characters of a checksum: enough to eyeball, the whole value is in the tooltip. */
export function shortHash(hash: string | null | undefined): string {
  if (!hash) return "";
  return hash.length <= 20 ? hash : `${hash.slice(0, 10)}…${hash.slice(-6)}`;
}

export type Verdict = "same" | "differs" | "sameSize" | "unknown";

/**
 * What the device's last upload says about the server's copy: the same checksum (the same bytes), a different one,
 * or, when the app sent no checksum, only whether the sizes agree.
 */
export function deviceVerdict(r: ItemRenditions): Verdict {
  const latest: DeviceUpload | null = r.device?.latest ?? null;
  if (!latest) return "unknown";
  if (latest.blake3) return latest.blake3.toLowerCase() === (r.original.blake3 ?? "").toLowerCase() ? "same" : "differs";
  if (latest.bytes !== null && r.original.bytes !== null) return latest.bytes === r.original.bytes ? "sameSize" : "differs";
  return "unknown";
}

/** Pixel size as "4032 × 3024", or "". */
export function dimensionsText(file: Pick<StoredFile, "width" | "height">): string {
  return file.width && file.height ? `${file.width} × ${file.height}` : "";
}
