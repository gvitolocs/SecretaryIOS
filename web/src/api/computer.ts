import { request } from "./client";

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
  task: { who: string; what: string; since: number; stopping: boolean } | null;
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
  stoppedTask?: string | null;
  batch?: { via?: string; events?: number; kinds?: string[]; textChars?: number; bytes?: number } | null;
}

export interface View {
  viewId: string;
  viewUrl: string;
  sandbox: Sandbox;
  mode: "watch" | "control";
  ttlSecs: number;
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
  heartbeat: (sandbox: Sandbox) =>
    request<{ state: ControlState }>("POST", "/v1/computer/control", { body: { sandbox, action: "heartbeat" } }),
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
};

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
