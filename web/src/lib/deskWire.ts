// LucinaDesk's browser wire (`lucinadesk.v1`, lucinadesk repo docs/CONTRACT.md): what the page and the
// desk socket say to each other. Plain JSON text frames both ways, plus one binary frame per encoded
// video frame from the server. The browser runs no RustDesk code and never sees its password: the
// gateway on nezopt does the protocol, this is only its small, owner-facing face.
//
// Everything here is pure (no sockets, no DOM), so it is unit-tested; lib/deskClient.ts drives it.

export const DESK_PROTOCOL = "lucinadesk.v1";

export type DeskCodec = "vp9" | "h264" | "h265" | "vp8" | "av1";

/** The wire's codec byte → our name. */
const CODEC_BY_ID: Record<number, DeskCodec> = { 1: "vp9", 2: "h264", 3: "h265", 4: "vp8", 5: "av1" };

/**
 * WebCodecs codec strings. H.264/H.265 arrive as Annex B, so they are configured without a
 * `description`. The profile/level strings only gate `isConfigSupported`; the bitstream carries the
 * real ones.
 */
export const WEBCODECS: Record<DeskCodec, string> = {
  vp9: "vp09.00.41.08",
  av1: "av01.0.08M.08",
  h264: "avc1.42E033",
  h265: "hvc1.1.6.L123.B0",
  vp8: "vp8",
};

/** The order we would like the host to pick, best first (a hint for the ticket; the host decides). */
export const CODEC_PREFERENCE: DeskCodec[] = ["vp9", "av1", "h264", "vp8", "h265"];

export interface WireFrame {
  codec: DeskCodec;
  key: boolean;
  /** Last frame of the host's batch: `ack` once it is decoded. */
  last: boolean;
  display: number;
  /** Host clock, milliseconds. */
  pts: number;
  data: Uint8Array;
}

const HEADER = 12;

/** One binary message → a video frame, or null for a kind/codec this page does not know. */
export function parseVideo(buf: ArrayBuffer): WireFrame | null {
  if (buf.byteLength < HEADER) return null;
  const v = new DataView(buf);
  if (v.getUint8(0) !== 1) return null;
  const codec = CODEC_BY_ID[v.getUint8(1)];
  if (!codec) return null;
  const flags = v.getUint8(2);
  return {
    codec,
    key: (flags & 1) !== 0,
    last: (flags & 2) !== 0,
    display: v.getUint8(3),
    pts: Number(v.getBigInt64(4, true)),
    data: new Uint8Array(buf, HEADER),
  };
}

/** For tests and the dev stand-in: the binary frame the server sends. */
export function encodeVideo(f: Omit<WireFrame, "codec"> & { codec: DeskCodec }): ArrayBuffer {
  const id = Number(Object.entries(CODEC_BY_ID).find(([, c]) => c === f.codec)?.[0] ?? 1);
  const out = new Uint8Array(HEADER + f.data.byteLength);
  const v = new DataView(out.buffer);
  v.setUint8(0, 1);
  v.setUint8(1, id);
  v.setUint8(2, (f.key ? 1 : 0) | (f.last ? 2 : 0));
  v.setUint8(3, f.display);
  v.setBigInt64(4, BigInt(Math.trunc(f.pts)), true);
  out.set(f.data, HEADER);
  return out.buffer;
}

export interface DeskDisplay {
  x: number;
  y: number;
  width: number;
  height: number;
  name?: string;
  cursorEmbedded?: boolean;
}

export type ServerMessage =
  | { type: "hello"; v: number; mode: "watch" | "control"; sandbox: string }
  | { type: "status"; phase: "relay" | "secured" | string }
  | {
      type: "login";
      hostname: string;
      platform: string;
      version: string;
      currentDisplay: number;
      displays: DeskDisplay[];
    }
  | { type: "cursor"; x: number; y: number }
  | { type: "refused"; what: string; why: string }
  | { type: "pong"; t: number }
  | { type: "closed"; reason: string };

const KNOWN = new Set(["hello", "status", "login", "cursor", "refused", "pong", "closed"]);

/** A text frame → a message we understand, or null (unknown types are ignored, per the contract). */
export function parseServer(text: string): ServerMessage | null {
  let m: unknown;
  try {
    m = JSON.parse(text);
  } catch {
    return null;
  }
  if (!m || typeof m !== "object") return null;
  const type = (m as { type?: unknown }).type;
  if (typeof type !== "string" || !KNOWN.has(type)) return null;
  if (type === "login") {
    const ds = (m as { displays?: unknown }).displays;
    if (!Array.isArray(ds) || ds.length === 0) return null;
  }
  return m as ServerMessage;
}

// ------------------------------------------------------------------ client → server

export type MouseKind = "move" | "down" | "up" | "wheel";
export type MouseButton = "left" | "right" | "middle";

const KIND: Record<MouseKind, number> = { move: 0, down: 1, up: 2, wheel: 3 };
const BUTTON: Record<MouseButton, number> = { left: 1, right: 2, middle: 4 };

/** `mask = (buttons << 3) | kind`. */
export function mouseMask(kind: MouseKind, button?: MouseButton): number {
  return ((button ? BUTTON[button] : 0) << 3) | KIND[kind];
}

export type ClientMessage =
  | { type: "ack" }
  | { type: "refresh" }
  | { type: "ping"; t: number }
  | { type: "mouse"; mask: number; x: number; y: number }
  | { type: "text"; text: string }
  | { type: "key"; key?: string; char?: string; modifiers?: string[] };

export const mouse = (kind: MouseKind, x: number, y: number, button?: MouseButton): ClientMessage => ({
  type: "mouse",
  mask: mouseMask(kind, button),
  x: Math.round(x),
  y: Math.round(y),
});

/** Wheel steps: `y` > 0 scrolls up, like RustDesk. */
export const wheel = (stepsX: number, stepsY: number): ClientMessage => ({ type: "mouse", mask: mouseMask("wheel"), x: stepsX, y: stepsY });

/** The contract caps `text` at 1024 characters: longer pastes go as several messages. */
export function textMessages(text: string): ClientMessage[] {
  const out: ClientMessage[] = [];
  const chars = Array.from(text);
  for (let i = 0; i < chars.length; i += 1024) out.push({ type: "text", text: chars.slice(i, i + 1024).join("") });
  return out;
}

// ------------------------------------------------------------------ keyboard

/** DOM `KeyboardEvent.key` → RustDesk ControlKey name. Printable keys are not here: they go as text or `char`. */
const CONTROL_KEYS: Record<string, string> = {
  Enter: "Return",
  Backspace: "Backspace",
  Tab: "Tab",
  Escape: "Escape",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowLeft: "LeftArrow",
  ArrowRight: "RightArrow",
  ArrowUp: "UpArrow",
  ArrowDown: "DownArrow",
  CapsLock: "CapsLock",
  ContextMenu: "Menu",
  " ": "Space",
};
for (let i = 1; i <= 12; i++) CONTROL_KEYS[`F${i}`] = `F${i}`;

const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "OS", "Hyper", "Super", "Fn"]);

export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  isComposing?: boolean;
}

/**
 * A keydown → what to send, or null to let it be (a bare modifier, an IME composition, a key we do
 * not know). Lucina's computer is Linux, the owner's is a Mac: ⌘ is sent as Control so ⌘C/⌘L/⌘T do
 * there what they do here. Plain typing (no Control/⌘/Alt) goes as `text`, so the layout the owner
 * types on is the one that counts.
 */
export function keyMessage(e: KeyLike): ClientMessage | null {
  if (e.isComposing || MODIFIER_KEYS.has(e.key) || e.key === "Unidentified" || e.key === "Dead") return null;
  const mods: string[] = [];
  if (e.ctrlKey || e.metaKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  const control = CONTROL_KEYS[e.key];
  if (control) {
    // A plain space is text (it repeats, it composes); with a modifier it is a key.
    if (control === "Space" && mods.length === 0) return { type: "text", text: " " };
    if (e.shiftKey) mods.push("Shift");
    return mods.length ? { type: "key", key: control, modifiers: mods } : { type: "key", key: control };
  }
  if (Array.from(e.key).length !== 1) return null;
  if (mods.length === 0) return { type: "text", text: e.key };
  if (e.shiftKey) mods.push("Shift");
  return { type: "key", char: e.key.toLowerCase(), modifiers: mods };
}

/** A key button of the touch bar: a ControlKey name, or a char with modifiers. */
export function keyButton(key: string, modifiers?: string[]): ClientMessage {
  const control = Object.values(CONTROL_KEYS).includes(key);
  const msg: ClientMessage = control ? { type: "key", key } : { type: "key", char: key };
  if (modifiers?.length) msg.modifiers = modifiers;
  return msg;
}

// ------------------------------------------------------------------ geometry

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Where a `natural`-sized picture shows inside `box` when fitted (`object-fit: contain`). */
export function fitRect(box: Rect, natural: { width: number; height: number }): Rect {
  if (natural.width <= 0 || natural.height <= 0) return box;
  const scale = Math.min(box.width / natural.width, box.height / natural.height);
  const width = natural.width * scale;
  const height = natural.height * scale;
  return { left: box.left + (box.width - width) / 2, top: box.top + (box.height - height) / 2, width, height };
}

/**
 * A client point on the shown picture → absolute host pixels (the display's offset included),
 * clamped to the display. `shown` is the picture's visual rect (transforms included).
 */
export function toHost(client: { clientX: number; clientY: number }, shown: Rect, display: DeskDisplay): { x: number; y: number } {
  const clamp = (v: number, max: number) => Math.min(Math.max(Math.round(v), 0), Math.max(max - 1, 0));
  const x = clamp(((client.clientX - shown.left) * display.width) / Math.max(shown.width, 1), display.width);
  const y = clamp(((client.clientY - shown.top) * display.height) / Math.max(shown.height, 1), display.height);
  return { x: display.x + x, y: display.y + y };
}

/** Wheel deltas → whole steps, accumulating trackpad fractions. */
export function createWheel(pxPerStep = 50) {
  let ax = 0;
  let ay = 0;
  return (deltaX: number, deltaY: number, deltaMode = 0): { x: number; y: number } | null => {
    const unit = deltaMode === 1 ? pxPerStep / 3 : deltaMode === 2 ? pxPerStep * 10 : 1;
    ax += deltaX * unit;
    ay += deltaY * unit;
    const sx = Math.trunc(ax / pxPerStep);
    const sy = Math.trunc(ay / pxPerStep);
    if (sx === 0 && sy === 0) return null;
    ax -= sx * pxPerStep;
    ay -= sy * pxPerStep;
    // DOM: positive deltaY scrolls down; the wire: positive y scrolls up.
    return { x: -sx || 0, y: -sy || 0 };
  };
}
