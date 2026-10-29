import { ApiError, request } from "./client";

// Lucina's own computers: /v1/computer/* (secretary-server → Lucina → the Cua sandboxes on nezopt).
// The live view is Cua's own web viewer, served under /v1/computer/view/<id>/ (a capability URL
// minted for this device); the takeover is a lease the page renews while the owner drives.
// The viewer's own files live at /v1/computer/viewer-assets/<version>/ (immutable, no ticket), so
// only index.html and the media socket carry one. A ticket is renewed or switched watch↔control by
// `viewTicket` on the same view id: the page hands it to the running viewer, which never reloads.

export type Sandbox = "workspace" | "android";
export type Driver = "owner" | "lucina" | "agent" | "none";

export interface ControlState {
  sandbox: Sandbox;
  driver: Driver;
  epoch: number;
  owner: { device: string; since: number; expiresInMs: number } | null;
  task: {
    who: string;
    what: string;
    since: number;
    stopping: boolean;
    /** The engine's latest step ("step 3 · clicked … · 1.4 s"), while Lucina works. */
    progress?: { line: string; at: number } | null;
  } | null;
  lastAgent: { who: string; at: number } | null;
  leaseSecs: number;
}

export interface AuditEntry {
  at: number;
  sandbox: Sandbox;
  event: string;
  device?: string;
  mode?: string;
  why?: string;
  /** Clipboard sync: how much text went one way (never the text). */
  bytes?: number;
  /** Clipboard sync: the PNG's size, when an image went along. */
  imageBytes?: number;
  /** A file sent either way: its name (never its content). */
  name?: string;
  stoppedTask?: string | null;
  /** jev_step: what the step did. */
  what?: string;
  batch?: { via?: string; events?: number; kinds?: string[]; textChars?: number; bytes?: number } | null;
}

export interface View {
  viewId: string;
  viewUrl: string;
  sandbox: Sandbox;
  mode: "watch" | "control";
  ttlSecs: number;
  /**
   * Lucina's home-network door, offered only to the owner and only from the house's own public IP:
   * the same view straight from nezopt (`url` + `viewUrl`) instead of through the server.
   */
  lan?: { url: string } | null;
}

/** A view's ticket again: renewed, or the other mode on the same view id. */
export interface ViewTicket {
  ticket: string;
  mode: "watch" | "control";
  sandbox: Sandbox;
  ttlSecs: number;
  epoch: number;
  /** The mode really changed (the viewer has to re-attach its media session). */
  changed: boolean;
}

export type InputEvent =
  | { kind: "click"; x: number; y: number; button?: "left" | "right" | "middle"; count?: number }
  /** A drag: a press, moves, a release (Simple mode's one-finger gesture). */
  | { kind: "down"; x: number; y: number; button?: "left" | "right" | "middle" }
  | { kind: "up"; x: number; y: number; button?: "left" | "right" | "middle" }
  | { kind: "scroll"; x: number; y: number; direction: "up" | "down" | "left" | "right"; amount?: number }
  | { kind: "move"; x: number; y: number }
  | { kind: "type"; text: string }
  | { kind: "key"; key: string; modifiers?: ("ctrl" | "shift" | "alt" | "super")[] }
  | { kind: "clipboard_write"; text: string }
  | { kind: "clipboard_read" };

export const computerApi = {
  control: (sandbox: Sandbox) =>
    request<{ state: ControlState; recent: AuditEntry[]; viewer?: { assets?: string } }>(
      "GET",
      `/v1/computer/control?sandbox=${sandbox}`,
    ),
  take: (sandbox: Sandbox) =>
    request<{ state: ControlState; stoppedTask: string | null }>("POST", "/v1/computer/control", { body: { sandbox, action: "take" } }),
  /** `active`: real input (keys, mouse, touch) since the last one; two idle minutes give the desktop back. */
  heartbeat: (sandbox: Sandbox, active: boolean) =>
    request<{ state: ControlState }>("POST", "/v1/computer/control", { body: { sandbox, action: "heartbeat", active } }),
  release: (sandbox: Sandbox) =>
    request<{ state: ControlState }>("POST", "/v1/computer/control", { body: { sandbox, action: "release" } }),
  view: (sandbox: Sandbox, mode: "watch" | "control") => request<View>("POST", "/v1/computer/view", { body: { sandbox, mode } }),
  /** The same view's ticket again (renewal, or watch↔control): no new view, no reload. */
  viewTicket: (viewId: string, mode: "watch" | "control") =>
    request<ViewTicket>("POST", `/v1/computer/view/${viewId}/ticket`, { body: { mode } }),
  input: (sandbox: Sandbox, events: InputEvent[]) =>
    request<{ done: number; outputs: { tool: string; text: string }[]; error?: { code: string; message: string } }>(
      "POST",
      "/v1/computer/input",
      { body: { sandbox, events }, timeoutMs: 60_000 },
    ),
  /** The current screen (PNG), with a cache-buster. */
  screenshotUrl: (sandbox: Sandbox, tick: number) => `/v1/computer/screenshot?sandbox=${sandbox}&t=${tick}`,
  /**
   * A file from this computer to `~/Desktop/From Mac/` on her desktop (while driving; 100 MiB at
   * most). It does not touch her clipboard: call `setClipboardFiles` with the paths of one paste.
   */
  uploadFile: (sandbox: Sandbox, file: File, timeoutMs = 15 * 60_000) => uploadFile(sandbox, file, timeoutMs),
  /** Puts files sent with `uploadFile` on her clipboard, so Ctrl+V pastes them there. */
  setClipboardFiles: (sandbox: Sandbox, paths: string[]) =>
    request<{ generation: number }>("POST", "/v1/computer/clipboard/files", { body: { sandbox, paths } }),
  /** The files on her clipboard (copied in her file manager); `generation` changes with every copy. */
  copiedFiles: (sandbox: Sandbox) =>
    request<{ generation: number; files: CopiedFile[] }>("GET", `/v1/computer/clipboard/files?sandbox=${sandbox}`),
  /** Downloads one of `copiedFiles` (only while it is on her clipboard). */
  fileDownloadUrl: (sandbox: Sandbox, path: string) =>
    `/v1/computer/files?sandbox=${sandbox}&path=${encodeURIComponent(path)}`,
};

/** A file on her clipboard. */
export interface CopiedFile {
  path: string;
  name: string;
  bytes: number;
}

/** Largest file sent either way (lucina-server's cap). */
export const FILE_LIMIT = 100 * 1024 * 1024;

async function uploadFile(sandbox: Sandbox, file: File, timeoutMs: number): Promise<{ path: string; name: string; bytes: number }> {
  if (file.size > FILE_LIMIT) throw new ApiError(413, "too_large", "Files over 100 MB are not sent.");
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  let res: Response;
  try {
    res = await fetch(`/v1/computer/files?sandbox=${sandbox}&name=${encodeURIComponent(file.name || "pasted")}`, {
      method: "POST",
      headers: { "Content-Type": "application/octet-stream", "X-Secretary-CSRF": "1", Accept: "application/json" },
      body: file,
      credentials: "same-origin",
      cache: "no-store",
      signal: controller.signal,
    });
  } catch {
    clearTimeout(timer);
    if (controller.signal.aborted) throw new ApiError(0, "timeout", "The upload took too long.");
    throw new ApiError(0, "network", "Can't reach the server. Check your connection.");
  }
  clearTimeout(timer);
  const data = (await res.json().catch(() => null)) as { error?: { code?: string; message?: string } } | null;
  if (!res.ok) {
    throw new ApiError(res.status, data?.error?.code ?? `http_${res.status}`, data?.error?.message ?? `Upload failed (${res.status}).`);
  }
  return data as unknown as { path: string; name: string; bytes: number };
}

/** How long the home-network probe may take before the view goes through the server. */
export const LAN_PROBE_MS = 400;

/**
 * Whether the home-network door answers from here (the house's public IP is not proof: a phone on
 * mobile data behind the same carrier NAT, or a router that refuses the private DNS answer).
 */
export async function probeLan(url: string, timeoutMs = LAN_PROBE_MS): Promise<boolean> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const r = await fetch(`${url}/v1/computer/lan-probe`, { mode: "cors", credentials: "omit", cache: "no-store", signal: ctl.signal });
    return r.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

/** Where the viewer's immutable files live for a bundle version. */
export function viewerAsset(version: string, file: string): string {
  return `/v1/computer/viewer-assets/${version}/${file}`;
}

/** Give the desktop back when the page goes away (keepalive survives navigation). */
export function releaseOnExit(sandbox: Sandbox): void {
  try {
    void fetch("/v1/computer/control", {
      method: "POST",
      keepalive: true,
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "X-Secretary-CSRF": "1" },
      body: JSON.stringify({ sandbox, action: "release" }),
    }).catch(() => undefined);
  } catch {
    // The 45 s lease ends the takeover anyway.
  }
}

/** Pixel in the sandbox's screen for a point on the scaled screenshot. */
export function toScreen(
  e: { offsetX: number; offsetY: number },
  shown: { width: number; height: number },
  natural: { width: number; height: number },
): { x: number; y: number } {
  const sx = natural.width / Math.max(shown.width, 1);
  const sy = natural.height / Math.max(shown.height, 1);
  const clamp = (v: number, max: number) => Math.min(Math.max(Math.round(v), 0), Math.max(max - 1, 0));
  return { x: clamp(e.offsetX * sx, natural.width), y: clamp(e.offsetY * sy, natural.height) };
}
