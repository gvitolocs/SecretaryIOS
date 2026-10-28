// Images from a paste or a drop into a Keep note: which clipboard entries are images, and whether a paste is
// an image paste at all (copying from Excel or Word also puts a picture of the text on the clipboard).

import { ALLOWED_IMAGE_TYPES } from "../api/keep";

export interface PickedImages {
  /** Images the server accepts, in clipboard order. */
  files: File[];
  /** Images of a type it does not (TIFF, BMP, SVG…). */
  rejected: number;
}

const NONE: PickedImages = { files: [], rejected: 0 };

/** Chromium names every clipboard bitmap "image.png"; a macOS screenshot pasted from the clipboard has no name of its own. */
const GENERIC_NAME = /^(image|blob|untitled)?(\.[a-z0-9]+)?$/i;

const pad = (n: number) => String(n).padStart(2, "0");

/** "Image 2026-10-28 22.41.png", like a macOS screenshot name, for a pasted image without a real name. */
export function pastedName(type: string, at: Date, index = 0): string {
  const ext = type === "image/jpeg" ? "jpg" : type.slice("image/".length) || "png";
  const stamp = `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}.${pad(at.getMinutes())}.${pad(at.getSeconds())}`;
  return `Image ${stamp}${index ? ` (${index + 1})` : ""}.${ext}`;
}

/** The image files in a DataTransfer, from its items when it has them (a paste), else from its files (a drop). */
export function imageFiles(dt: DataTransfer | null | undefined, now: () => Date = () => new Date()): PickedImages {
  if (!dt) return NONE;
  const found: File[] = [];
  const items = dt.items ? [...dt.items] : [];
  for (const it of items) {
    if (it.kind !== "file" || !it.type.startsWith("image/")) continue;
    const f = it.getAsFile();
    if (f && !found.includes(f)) found.push(f);
  }
  if (!found.length) for (const f of [...(dt.files ?? [])]) if (f.type.startsWith("image/") && !found.includes(f)) found.push(f);
  if (!found.length) return NONE;

  const at = now();
  const files: File[] = [];
  let rejected = 0;
  for (const f of found) {
    if (!ALLOWED_IMAGE_TYPES.includes(f.type)) {
      rejected++;
      continue;
    }
    files.push(GENERIC_NAME.test(f.name) ? new File([f], pastedName(f.type, at, files.length), { type: f.type, lastModified: f.lastModified }) : f);
  }
  return { files, rejected };
}

/** True when the HTML is nothing but images (what a browser's "Copy image" puts beside the bitmap). */
function htmlIsOnlyImages(html: string): boolean {
  if (!/<img\b/i.test(html)) return false;
  const text = html
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<(style|script|head)\b[\s\S]*?<\/\1>/gi, "")
    .replace(/<[^>]*>/g, "")
    .replace(/&nbsp;/g, " ");
  return text.trim() === "";
}

/**
 * The images a paste should attach, or none when the paste is text. Images win when there is no text, when the
 * text is only the copied files' names (Finder), or when the HTML holds only images (a browser's "Copy image").
 * Text with its own picture beside it (Excel, Word, Pages) stays a text paste.
 */
export function pastedImages(dt: DataTransfer | null | undefined, now?: () => Date): PickedImages {
  if (!dt) return NONE;
  const picked = imageFiles(dt, now);
  if (!picked.files.length && !picked.rejected) return NONE;
  const text = (dt.getData("text/plain") ?? "").trim();
  if (!text) return picked;
  const names = new Set([...(dt.files ?? [])].map((f) => f.name));
  if (text.split(/\r?\n/).every((l) => names.has(l.trim()))) return picked;
  const html = dt.getData("text/html") ?? "";
  if (html && htmlIsOnlyImages(html)) return picked;
  return NONE;
}

/** True while a drag carries files from outside the page (not a note card being moved). */
export function dragHasFiles(dt: DataTransfer | null | undefined): boolean {
  return !!dt && [...(dt.types ?? [])].includes("Files");
}
