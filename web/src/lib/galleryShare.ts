// Share links and file sharing for the Gallery (components/gallery/share/*): the pure rules, no UI.

import type { GalleryItem, GalleryShare } from "../api/gallery";

/** The host the public pages live on (p1 maps `/a/*` to the library); used when this page is not served from it. */
export const PUBLIC_HOST = "https://getsecretary.tech";

export type ExpiryChoice = "1" | "7" | "30" | "never";
export const EXPIRY_CHOICES: ExpiryChoice[] = ["1", "7", "30", "never"];
export const DEFAULT_EXPIRY: ExpiryChoice = "7";

/** `"7"` -> 7 days, `"never"` -> null (the request then carries no expiry). */
export function expiryDays(choice: ExpiryChoice): number | null {
  return choice === "never" ? null : Number(choice);
}

const LOCAL_HOST = /^(localhost|127\.|\[::1\]|.*\.localhost$|.*\.test$)/i;

/**
 * The address to hand out for a link. The public page is served by the Secretary host, so a page served from
 * somewhere else (a dev server, the Mac shell's local origin, a private address) uses the public host instead of
 * giving out a link that only works on this machine.
 */
export function shareUrl(share: Pick<GalleryShare, "publicPath">, location: Pick<Location, "protocol" | "host" | "origin">): string {
  const here = location.protocol === "https:" && !LOCAL_HOST.test(location.host.split(":")[0] ?? "") ? location.origin : PUBLIC_HOST;
  return `${here}${share.publicPath}`;
}

export type ExpiryState =
  | { kind: "off" }
  | { kind: "expired" }
  | { kind: "never" }
  | { kind: "days"; days: number };

/**
 * What a link's row says about its life: turned off, expired, never expires, or about this many days left (rounded to
 * the nearest day, at least 1: a clock a second off must not turn a fresh 7-day link into 8).
 */
export function expiryState(share: Pick<GalleryShare, "revokedAt" | "expiresAt">, now: number): ExpiryState {
  if (share.revokedAt) return { kind: "off" };
  if (!share.expiresAt) return { kind: "never" };
  const at = Date.parse(share.expiresAt);
  if (Number.isNaN(at) || at <= now) return { kind: "expired" };
  return { kind: "days", days: Math.max(1, Math.round((at - now) / 86_400_000)) };
}

/** Live links first (newest first within each group), then the ones that are over. */
export function orderShares<T extends Pick<GalleryShare, "live" | "createdAt">>(list: T[]): T[] {
  return [...list].sort((a, b) => Number(b.live) - Number(a.live) || b.createdAt.localeCompare(a.createdAt));
}

/** How a file should be called when saved: the original name, else the title, else the id. */
export function fileNameOf(item: Pick<GalleryItem, "id" | "title" | "origin" | "mime">): string {
  const name = item.origin.filename || item.title;
  if (name) return name;
  const ext = item.mime.split("/")[1]?.replace("jpeg", "jpg") ?? "";
  return ext ? `${item.id}.${ext}` : item.id;
}

/** Whether the browser can hand these files to the system share sheet. */
export function canShareFiles(files: File[], nav: Pick<Navigator, "canShare" | "share"> | undefined = typeof navigator === "undefined" ? undefined : navigator): boolean {
  return !!nav && typeof nav.share === "function" && typeof nav.canShare === "function" && files.length > 0 && nav.canShare({ files });
}

/** Whether `navigator.share` exists at all (a link can be shared even when files cannot). */
export function canShareLinks(nav: Pick<Navigator, "share"> | undefined = typeof navigator === "undefined" ? undefined : navigator): boolean {
  return !!nav && typeof nav.share === "function";
}

/** More files than this are not fetched into memory for the share sheet; the link is shared instead. */
export const SHARE_FILES_MAX = 10;
export const SHARE_BYTES_MAX = 100 * 1024 * 1024;

/** Whether a selection is small enough to hand over as files. */
export function fitsAsFiles(items: Pick<GalleryItem, "size">[]): boolean {
  return items.length > 0 && items.length <= SHARE_FILES_MAX && items.reduce((n, i) => n + i.size, 0) <= SHARE_BYTES_MAX;
}

/** Items that can be shared: photos and videos (the server refuses the rest). */
export function shareable(items: GalleryItem[]): GalleryItem[] {
  return items.filter((i) => i.kind === "photo" || i.kind === "video");
}

export interface DownloadEntry {
  href: string;
  name: string;
}

/** The original of each item (`urls.download`, else `urls.original`) with the name to save it under. */
export function downloadEntries(items: GalleryItem[]): DownloadEntry[] {
  return items
    .map((item) => ({ href: item.urls.download ?? item.urls.original ?? "", name: fileNameOf(item) }))
    .filter((e) => e.href !== "");
}

/**
 * Starts the downloads one after another, `gap` ms apart (browsers drop a burst of them). `start` begins one;
 * `wait` pauses. Stops early when `cancelled()` says so. Resolves with how many were started.
 */
export async function downloadSequentially(
  entries: DownloadEntry[],
  start: (entry: DownloadEntry) => void,
  wait: (ms: number) => Promise<void>,
  gap = 600,
  cancelled: () => boolean = () => false,
): Promise<number> {
  let started = 0;
  for (const entry of entries) {
    if (cancelled()) break;
    if (started > 0) await wait(gap);
    if (cancelled()) break;
    start(entry);
    started += 1;
  }
  return started;
}

/** Begins a browser download of `entry` (an anchor click). */
export function startDownload(entry: DownloadEntry, doc: Document = document): void {
  const a = doc.createElement("a");
  a.href = entry.href;
  a.download = entry.name;
  a.rel = "noopener";
  a.style.display = "none";
  doc.body.append(a);
  a.click();
  a.remove();
}
