// JPEG XL support: detected once with a 2x2 image, then cached for the tab.
// Chat thumbnails use the 256 px JXL grid when it decodes, and the JPEG
// `/thumb?size=256` when it does not (older Android WebViews, iOS Safari).

const TINY_JXL =
  "data:image/jxl;base64,/woIkAEAEogCANQAXeUSAAAVKqOMG7yc6/nyQ4fFtI3rDG21bWFJosg4NkkncEgSi8DGBXgDA4Axc9YAD5SokgQ=";

export const MEDIA_GRID_WIDTH = 256;
export const MEDIA_THUMB_WIDTH = 256;
export const MEDIA_PREVIEW_WIDTH = 1024;

let detected: Promise<boolean> | null = null;

export function mediaItemPath(id: string): string {
  return `/v1/media/items/${encodeURIComponent(id)}`;
}

export function mediaGridUrl(id: string): string {
  return `${mediaItemPath(id)}/r/grid`;
}

export function mediaThumbUrl(id: string, size = MEDIA_THUMB_WIDTH): string {
  return `${mediaItemPath(id)}/thumb?size=${size}`;
}

export function mediaOriginalUrl(id: string): string {
  return `${mediaItemPath(id)}/original`;
}

/** The image to use in a bubble: JXL grid when supported, otherwise JPEG. */
export function chooseGridUrl(id: string, jxl: boolean): string {
  return jxl ? mediaGridUrl(id) : mediaThumbUrl(id, MEDIA_GRID_WIDTH);
}

/** The image to use in the lightbox: the item's `urls.preview`, else a 1024 px JPEG thumb. */
export function choosePreviewUrl(id: string, preview?: string): string {
  return preview || mediaThumbUrl(id, MEDIA_PREVIEW_WIDTH);
}

async function decodeTinyJxl(): Promise<boolean> {
  try {
    if (typeof Image === "undefined") return false;
    return await new Promise<boolean>((resolve) => {
      const img = new Image();
      let done = false;
      const finish = (ok: boolean) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        resolve(ok);
      };
      const timer = setTimeout(() => finish(false), 1500);
      img.onload = () => finish(img.naturalWidth > 0 || img.width > 0);
      img.onerror = () => finish(false);
      img.src = TINY_JXL;
    });
  } catch {
    return false;
  }
}

/** Resolves once: can this browser decode JPEG XL? */
export function supportsJxl(): Promise<boolean> {
  detected ??= decodeTinyJxl();
  return detected;
}

/** Test helper: forget the detection so a new Image implementation is used. */
export function resetJxlSupport(): void {
  detected = null;
}
