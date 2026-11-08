// Files between this computer and Lucina's desktop while the owner drives it, for any viewer (the Cua
// viewer page and LucinaDesk's player): files pasted or dropped here land in `~/Desktop/From Mac/` and
// on her clipboard, so Ctrl+V there pastes them; files copied in her file manager are offered as
// downloads. The server checks the takeover lease, names, paths and the 100 MB cap.

import { computerApi, FILE_LIMIT, type CopiedFile, type Sandbox } from "../api/computer";

/** How often her clipboard's files are checked while driving, and right after a copy. */
export const COPIED_POLL_MS = 2_000;
export const COPIED_SOON_MS = [300, 900];

/**
 * Uploads `files` one by one, then puts them all on her clipboard. `onFile` hears each one start.
 * Throws the first failure (nothing goes on her clipboard then).
 */
export async function sendFiles(sandbox: Sandbox, files: File[], onFile?: (file: File, i: number) => void): Promise<string[]> {
  const paths: string[] = [];
  for (const [i, file] of files.entries()) {
    onFile?.(file, i);
    const sent = await computerApi.uploadFile(sandbox, file);
    paths.push(sent.path);
  }
  if (paths.length) await computerApi.setClipboardFiles(sandbox, paths);
  return paths;
}

/** Files bigger than the cap, which `sendFiles` would refuse. */
export function tooLarge(files: File[]): File[] {
  return files.filter((f) => f.size > FILE_LIMIT);
}

/**
 * Watches her clipboard for files while `active()` (the owner drives): `onOffer` hears each new copy
 * that holds files, never the one already there when watching starts. `soon()` checks right away and
 * twice more (after a Ctrl+C / Ctrl+X).
 */
export function watchCopiedFiles(sandbox: () => Sandbox, active: () => boolean, onOffer: (files: CopiedFile[]) => void) {
  let seen: number | null = null;
  let busy = false;
  const check = async () => {
    if (busy || !active() || (typeof document !== "undefined" && document.hidden)) return;
    busy = true;
    try {
      const r = await computerApi.copiedFiles(sandbox());
      const first = seen === null;
      if (r.generation !== seen) {
        seen = r.generation;
        if (!first && r.files.length) onOffer(r.files);
      }
    } catch {
      // Not driving any more, or Lucina away: the next tick tries again.
    } finally {
      busy = false;
    }
  };
  const timer = setInterval(() => void check(), COPIED_POLL_MS);
  const soon: ReturnType<typeof setTimeout>[] = [];
  return {
    soon() {
      void check();
      for (const ms of COPIED_SOON_MS) soon.push(setTimeout(() => void check(), ms));
    },
    /** Forget the last copy (a new takeover starts fresh). */
    reset() {
      seen = null;
    },
    stop() {
      clearInterval(timer);
      soon.forEach(clearTimeout);
    },
  };
}

/** Saves one file copied on her desktop to this computer (the browser's download). */
export function downloadFile(sandbox: Sandbox, file: CopiedFile): void {
  if (typeof document === "undefined") return;
  const a = document.createElement("a");
  a.href = computerApi.fileDownloadUrl(sandbox, file.path);
  a.download = file.name;
  a.rel = "noopener";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
}
