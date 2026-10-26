// Attachments for the chat composer: turning files, clipboard/drop lists and
// a shared location into the DraftAttachment the tray renders and the share
// request uploads.

import type { Attachment } from "../api/timeline";
import { uuid } from "./ids";
import { uploadFile } from "./share";

export interface ShareLocation {
  lat: number;
  lon: number;
}

export interface DraftAttachment {
  id: string;
  file: File;
  kind: Attachment["kind"];
  name: string;
  size: number;
  mime: string;
  /** Local object URL for the tray/optimistic bubble. Never leaves the tab. */
  previewUrl?: string;
  width?: number;
  height?: number;
  durationMs?: number;
  thumbhash?: string;
  mediaId?: string;
}

export interface ShareMetaFile {
  index: number;
  width?: number;
  height?: number;
  durationMs?: number;
}

/**
 * The `meta` form field: one entry per `file` part (its 0-based index plus the
 * dimensions/duration the browser could read) and the shared location.
 * Undefined when there is neither a file nor a location.
 */
export function buildShareMeta(files: readonly DraftAttachment[], location?: ShareLocation | null): string | undefined {
  if (!files.length && !location) return undefined;
  const entries = files.map((file, index): ShareMetaFile => {
    const entry: ShareMetaFile = { index };
    if (positive(file.width)) entry.width = Math.round(file.width);
    if (positive(file.height)) entry.height = Math.round(file.height);
    if (typeof file.durationMs === "number" && Number.isFinite(file.durationMs) && file.durationMs >= 0) {
      entry.durationMs = Math.round(file.durationMs);
    }
    return entry;
  });
  return JSON.stringify({
    ...(entries.length ? { files: entries } : {}),
    ...(location ? { location: { lat: location.lat, lon: location.lon } } : {}),
  });
}

function positive(value: number | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

export function fileKind(file: File): Attachment["kind"] {
  const type = (file.type || "").toLowerCase();
  const name = file.name.toLowerCase();
  if (type.startsWith("image/")) return "image";
  if (type.startsWith("video/")) return "video";
  if (type.startsWith("audio/")) return "audio";
  if (type === "application/pdf" || name.endsWith(".pdf")) return "pdf";
  if (
    type === "application/msword" ||
    type.includes("officedocument") ||
    type === "application/rtf" ||
    /\.(docx?|xlsx?|pptx?|odt|ods|odp|rtf)$/.test(name)
  ) {
    return "document";
  }
  return "file";
}

function isImageOrVideo(file: File): boolean {
  return file.type.startsWith("image/") || file.type.startsWith("video/");
}

export function draftFor(file: File): DraftAttachment {
  const draft: DraftAttachment = {
    id: uuid(),
    file,
    kind: fileKind(file),
    name: file.name || "attachment",
    size: file.size,
    mime: file.type || "application/octet-stream",
  };
  if (isImageOrVideo(file) && typeof URL !== "undefined" && typeof URL.createObjectURL === "function") {
    try {
      draft.previewUrl = URL.createObjectURL(file);
    } catch {
      // Preview is a convenience; the upload still works.
    }
  }
  return draft;
}

export function releaseDraft(draft: DraftAttachment): void {
  if (!draft.previewUrl) return;
  try {
    URL.revokeObjectURL(draft.previewUrl);
  } catch {
    // Ignore.
  }
}

/** Files present on a ClipboardEvent's DataTransfer. */
export function filesFromClipboard(data: DataTransfer | null | undefined): File[] {
  if (!data) return [];
  const fromFiles = data.files ? Array.from(data.files) : [];
  if (fromFiles.length) return fromFiles.filter((f) => f instanceof File);
  return filesFromItems(data.items);
}

/** Files present on a drag-and-drop DataTransfer. */
export function filesFromDataTransfer(data: DataTransfer | null | undefined): File[] {
  if (!data) return [];
  const fromFiles = data.files ? Array.from(data.files) : [];
  if (fromFiles.length) return fromFiles.filter((f) => f instanceof File);
  return filesFromItems(data.items);
}

function filesFromItems(items: DataTransferItemList | DataTransferItem[] | null | undefined): File[] {
  if (!items) return [];
  const out: File[] = [];
  for (const item of Array.from(items)) {
    if (item.kind !== "file") continue;
    const file = item.getAsFile?.();
    if (file) out.push(file);
  }
  return out;
}

export function hasFileTransfer(data: DataTransfer | null | undefined): boolean {
  if (!data) return false;
  if (data.files?.length) return true;
  if (data.types && Array.from(data.types).includes("Files")) return true;
  return Array.from(data.items ?? []).some((item) => item.kind === "file");
}

function withTimeout<T>(promise: Promise<T>, ms: number, fallback: T): Promise<T> {
  return new Promise<T>((resolve) => {
    let done = false;
    const finish = (value: T) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      resolve(value);
    };
    const timer = setTimeout(() => finish(fallback), ms);
    promise.then(finish, () => finish(fallback));
  });
}

async function probeImage(file: File): Promise<{ width: number; height: number } | null> {
  if (typeof Image === "undefined") return null;
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    const loaded = new Promise<{ width: number; height: number } | null>((resolve) => {
      img.onload = () => resolve(img.naturalWidth > 0 ? { width: img.naturalWidth, height: img.naturalHeight } : null);
      img.onerror = () => resolve(null);
      img.src = url;
    });
    return await withTimeout(loaded, 1200, null);
  } finally {
    try {
      URL.revokeObjectURL(url);
    } catch {
      // Ignore.
    }
  }
}

async function probeMedia(file: File, tag: "video" | "audio"): Promise<{ width?: number; height?: number; durationMs: number } | null> {
  if (typeof document === "undefined") return null;
  const url = URL.createObjectURL(file);
  const el = document.createElement(tag);
  try {
    const loaded = new Promise<{ width?: number; height?: number; durationMs: number } | null>((resolve) => {
      el.onloadedmetadata = () => {
        const result: { width?: number; height?: number; durationMs: number } = {
          durationMs: Number.isFinite(el.duration) ? Math.round(el.duration * 1000) : 0,
        };
        if (tag === "video" && el instanceof HTMLVideoElement && el.videoWidth > 0) {
          result.width = el.videoWidth;
          result.height = el.videoHeight;
        }
        resolve(result);
      };
      el.onerror = () => resolve(null);
      el.preload = "metadata";
      el.src = url;
    });
    return await withTimeout(loaded, 1200, null);
  } finally {
    el.removeAttribute("src");
    try {
      URL.revokeObjectURL(url);
    } catch {
      // Ignore.
    }
  }
}

/** Read the dimensions/duration the `meta` field wants, when the browser can. */
export async function probeDraft(draft: DraftAttachment): Promise<DraftAttachment> {
  if (draft.kind === "image") {
    const size = await probeImage(draft.file);
    if (size) return { ...draft, ...size };
  } else if (draft.kind === "video" || draft.kind === "audio") {
    const info = await probeMedia(draft.file, draft.kind === "video" ? "video" : "audio");
    if (info) return { ...draft, ...info };
  }
  return draft;
}

export interface PrepareOptions {
  /** Convert HEIC to JPEG before uploading (default true). */
  convert?: boolean;
  /** Read dimensions/duration (default true). */
  probe?: boolean;
}

/** Files picked/pasted/dropped by the owner, ready for the tray. */
export async function prepareDraftFiles(files: readonly File[], options: PrepareOptions = {}): Promise<DraftAttachment[]> {
  const out: DraftAttachment[] = [];
  for (const original of files) {
    let file = original;
    if (options.convert !== false) {
      try {
        file = await uploadFile(original);
      } catch {
        file = original;
      }
    }
    const draft = draftFor(file);
    out.push(options.probe === false ? draft : await probeDraft(draft));
  }
  return out;
}

/** The optimistic timeline attachment shown before the server answers. */
export function draftAttachment(draft: DraftAttachment): Attachment {
  const attachment: Attachment = {
    kind: draft.kind,
    name: draft.name,
    mime: draft.mime,
    bytes: draft.size,
  };
  if (draft.previewUrl) attachment.previewUrl = draft.previewUrl;
  if (draft.width) attachment.width = draft.width;
  if (draft.height) attachment.height = draft.height;
  if (draft.durationMs !== undefined) attachment.durationMs = draft.durationMs;
  if (draft.thumbhash) attachment.thumbhash = draft.thumbhash;
  if (draft.mediaId) attachment.mediaId = draft.mediaId;
  return attachment;
}

export function locationAttachment(location: ShareLocation): Attachment {
  return {
    kind: "location",
    name: `${formatCoordinate(location.lat)}, ${formatCoordinate(location.lon)}`,
    lat: location.lat,
    lon: location.lon,
  };
}

export function formatCoordinate(value: number): string {
  return value.toFixed(5).replace(/\.?0+$/, "");
}

export function formatBytes(bytes: number | undefined): string {
  if (bytes === undefined || !Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(bytes < 10 * 1024 ** 2 ? 1 : 0)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function formatDuration(ms: number | undefined): string {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return "";
  const total = Math.round(ms / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

export function googleMapsUrl(lat: number, lon: number): string {
  return `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(`${lat},${lon}`)}`;
}
