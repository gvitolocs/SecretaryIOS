// The Mac image viewer's entry point: Secretary Code (the desktop shell) opens the pictures
// Finder hands it in the gallery's photo viewer, with no second viewer:
//
//   window.lucinaViewer.open(files, startIndex, { device, onClose })
//
// - `files`: the double-clicked picture and the other pictures of its folder, in folder order
//   (swiping and the arrow keys move through them). Each is `{ name, url?, blob?, mime?,
//   takenAt?, assetId? }`; `url` is anything the page can load (a blob:, data: or the shell's
//   own file protocol URL), `blob` the bytes themselves.
// - `device`: this Mac (`{ id: "mac-<uuid>", name, kind: "mac" }`); with it the viewer shows
//   "Save to Lucina", which uploads the file into the gallery under that device.
// - `onClose`: the viewer was closed (the shell may close the window).
//
// The page also fires `lucinaviewer:ready` on window once the API exists, and
// `lucinaviewer:close` on every close. The /viewer route is an empty black page for a
// viewer window; any page of the app can open the viewer.

import { createSignal } from "solid-js";
import { validDeviceId, type LucinaDevice } from "../api/mediaUpload";

export interface DesktopFile {
  name: string;
  url?: string;
  blob?: Blob;
  mime?: string;
  /** When the picture was taken (ISO 8601), when the shell knows it. */
  takenAt?: string;
  /** Stable per file on this Mac (e.g. a hash of its path), so a re-save is the same asset. */
  assetId?: string;
}

export interface DesktopViewerOptions {
  device?: LucinaDevice;
  onClose?: () => void;
}

export interface OpenFile extends DesktopFile {
  /** What the viewer shows (an object URL for a blob). */
  src: string;
  /** Object URLs this session made, revoked on close. */
  owned: string[];
}

export interface DesktopSession {
  files: OpenFile[];
  index: number;
  options: DesktopViewerOptions;
}

const [session, setSession] = createSignal<DesktopSession | null>(null);
const [device, setDevice] = createSignal<LucinaDevice | null>(null);
export { session as desktopSession, device as desktopDevice };

const UNSAFE_URL = /^\s*(javascript|vbscript):/i;

function prepare(file: DesktopFile): OpenFile | null {
  if (!file || typeof file.name !== "string" || !file.name) return null;
  if (file.blob instanceof Blob) {
    const src = URL.createObjectURL(file.blob);
    return { ...file, mime: file.mime || file.blob.type || undefined, src, owned: [src] };
  }
  if (typeof file.url === "string" && file.url && !UNSAFE_URL.test(file.url)) return { ...file, src: file.url, owned: [] };
  return null;
}

function release(s: DesktopSession | null): void {
  for (const f of s?.files ?? []) for (const url of f.owned) URL.revokeObjectURL(url);
}

/** Replaces what an open file shows (e.g. a HEIC converted to JPEG for this browser). */
export function replaceShown(index: number, blob: Blob): void {
  const s = session();
  const file = s?.files[index];
  if (!s || !file) return;
  const src = URL.createObjectURL(blob);
  const files = s.files.slice();
  files[index] = { ...file, src, owned: [...file.owned, src] };
  setSession({ ...s, files });
}

export function setDesktopIndex(index: number): void {
  const s = session();
  if (s && index >= 0 && index < s.files.length) setSession({ ...s, index });
}

export const lucinaViewer = {
  version: 1,
  /** Opens `files` at `start`; returns false when none of them can be shown. */
  open(files: DesktopFile[], start = 0, options: DesktopViewerOptions = {}): boolean {
    const list = (Array.isArray(files) ? files : []).map(prepare);
    const startFile = list[start];
    const kept = list.filter((f): f is OpenFile => f !== null);
    if (!kept.length) return false;
    if (options.device) lucinaViewer.setDevice(options.device);
    release(session());
    const index = startFile ? kept.indexOf(startFile) : 0;
    setSession({ files: kept, index: Math.max(0, index), options });
    return true;
  },
  close(): void {
    const s = session();
    if (!s) return;
    release(s);
    setSession(null);
    s.options.onClose?.();
    window.dispatchEvent(new Event("lucinaviewer:close"));
  },
  /** The device "Save to Lucina" uploads as (this Mac). */
  setDevice(d: LucinaDevice | null): void {
    if (d && !validDeviceId(d.id)) throw new Error("invalid device id");
    setDevice(d);
  },
  isOpen(): boolean {
    return session() !== null;
  },
};

declare global {
  interface Window {
    lucinaViewer?: typeof lucinaViewer;
  }
}

export function installDesktopViewer(): void {
  window.lucinaViewer = lucinaViewer;
  window.dispatchEvent(new Event("lucinaviewer:ready"));
}
