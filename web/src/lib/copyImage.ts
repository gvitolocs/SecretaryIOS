import type { GalleryItem } from "../api/gallery";
import { desktopShell } from "./desktopShell";

// "Copy image" and the files Share and "Ask Lucina" hand on: the picture as the browser can decode it. The original
// when it is a JPEG, PNG, WebP or GIF of a sane size (or the edited copy when the photo has edits), else the 1024 px
// JPEG rendition, never the JPEG XL ones (not every browser decodes them). The clipboard gets a PNG, the one image
// type every browser and app accepts there.

const DECODABLE = new Set(["image/jpeg", "image/png", "image/webp", "image/gif"]);
const MAX_ORIGINAL = 30 * 1024 * 1024;

/** The URL of the picture to copy or hand on, or null when the item has none (a video's frame: its poster). */
export function pictureUrl(item: GalleryItem): string | null {
  if (item.kind === "video") return item.urls.poster ?? null;
  if (item.edited && item.urls.edited) return item.urls.edited;
  if (item.urls.original && DECODABLE.has(item.mime) && item.size > 0 && item.size <= MAX_ORIGINAL) return item.urls.original;
  return item.urls.large ?? item.urls.thumb ?? null;
}

async function fetchPicture(url: string): Promise<Blob> {
  const res = await fetch(url, { credentials: "include" });
  if (!res.ok) throw new Error(`picture ${res.status}`);
  return res.blob();
}

/** Decodes a picture: createImageBitmap, or an <img> where it refuses (SVG, some older engines). */
async function decode(blob: Blob): Promise<{ source: CanvasImageSource; width: number; height: number; done: () => void }> {
  try {
    const bitmap = await createImageBitmap(blob);
    return { source: bitmap, width: bitmap.width, height: bitmap.height, done: () => bitmap.close?.() };
  } catch {
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.decoding = "async";
    img.src = url;
    try {
      await img.decode();
    } catch (e) {
      URL.revokeObjectURL(url);
      throw e;
    }
    return { source: img, width: img.naturalWidth, height: img.naturalHeight, done: () => URL.revokeObjectURL(url) };
  }
}

/** Re-encodes any decodable picture as PNG. */
export async function toPng(blob: Blob): Promise<Blob> {
  if (blob.type === "image/png") return blob;
  const picture = await decode(blob);
  const canvas = document.createElement("canvas");
  canvas.width = picture.width;
  canvas.height = picture.height;
  const g = canvas.getContext("2d");
  if (!g) throw new Error("no 2d canvas");
  g.drawImage(picture.source, 0, 0);
  picture.done();
  return new Promise((resolve, reject) => canvas.toBlob((png) => (png ? resolve(png) : reject(new Error("png encode"))), "image/png"));
}

function dataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(blob);
  });
}

/**
 * Puts the item's picture on the clipboard as a PNG. Call it straight from the click: the ClipboardItem is made at
 * once with a promise of the PNG, which Safari requires (it refuses a write that starts after an await). Without the
 * async clipboard (or when it refuses), Secretary Code's own clipboard takes it. True when the picture was copied.
 */
export async function copyPicture(item: GalleryItem): Promise<boolean> {
  const url = pictureUrl(item);
  if (!url) return false;
  const png = fetchPicture(url).then(toPng);
  const clip = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
  if (clip?.write && typeof ClipboardItem !== "undefined") {
    try {
      await clip.write([new ClipboardItem({ "image/png": png })]);
      return true;
    } catch {
      // Denied or unsupported here: try the app's clipboard below.
    }
  }
  const shell = desktopShell();
  if (!shell?.copyImage) return false;
  try {
    return await shell.copyImage(await dataUrl(await png));
  } catch {
    return false;
  }
}

/** The item's picture as a File (Share's file, the chat attachment for "Ask Lucina"). */
export async function pictureFile(item: GalleryItem): Promise<File | null> {
  const url = pictureUrl(item);
  if (!url) return null;
  const blob = await fetchPicture(url);
  const type = blob.type || "image/jpeg";
  const ext = type === "image/png" ? "png" : type === "image/webp" ? "webp" : type === "image/gif" ? "gif" : "jpg";
  const base = (item.origin.filename ?? item.title ?? item.id).replace(/\.[A-Za-z0-9]+$/, "") || item.id;
  return new File([blob], `${base}.${ext}`, { type });
}
