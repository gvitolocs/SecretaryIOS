// The voice WebSocket protocol, `GET /v1/voice/ws`: docs/lucinavoice-protocol.md section 2 and
// section 10 (streaming input from every client). Pure: message types, the parser for what the
// server sends, and the builder for `start`.

export const VOICE_WS_PATH = "/v1/voice/ws";
/** The largest binary frame the server takes (section 2). */
export const MAX_FRAME_BYTES = 8 * 1024;
export const INPUT_RATE = 16_000;
/** Server audio frames are s16le mono at this rate unless an `audio` message says otherwise. */
export const DEFAULT_OUTPUT_RATE = 24_000;
/** A note may run 10 minutes of 16 kHz s16le (the server caps it too). */
export const NOTE_MAX_MS = 10 * 60_000;
export const NOTE_MAX_BYTES = (NOTE_MAX_MS / 1000) * INPUT_RATE * 2;

export type VoicePurpose = "command" | "note";

/** What the server sends as text (sections 2 and 10), typed. */
export type VoiceEvent =
  | { type: "listening"; purpose?: string; codec?: string; rate?: number; turn?: string }
  | { type: "endpoint" }
  | { type: "transcript"; text: string }
  | { type: "audio"; rate: number }
  | { type: "delta"; text: string }
  | { type: "answer"; text: string; replaces: boolean }
  | { type: "note"; shareId: string; userItemId: string }
  | { type: "reply"; text: string; shareId?: string; itemId?: string }
  | { type: "done"; shareId?: string; ms: Record<string, number | string> }
  | { type: "error"; code: string; message?: string; shareId?: string };

/** Things the client itself reports to a turn's handler. */
export type VoiceLocalEvent =
  /** `listening` came without `codec`: an older server. The turn was cancelled. */
  | { type: "unsupported" }
  /** The socket closed under the turn. */
  | { type: "closed" }
  /** The client stopped at its own recording cap and sent `end`. */
  | { type: "capped" }
  /** A binary audio frame (s16le mono at `rate`). */
  | { type: "frame"; pcm: ArrayBuffer; rate: number };

export type VoiceTurnEvent = VoiceEvent | VoiceLocalEvent;

const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const num = (v: unknown): number | undefined => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

/** Parse one server text message; null for anything unknown or malformed (it is ignored). */
export function parseServerMessage(raw: string): VoiceEvent | null {
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const m = v as Record<string, unknown>;
  switch (m.type) {
    case "listening": {
      const e: VoiceEvent = { type: "listening" };
      const purpose = str(m.purpose);
      const codec = str(m.codec);
      const rate = num(m.rate);
      const turn = str(m.turn);
      if (purpose !== undefined) e.purpose = purpose;
      if (codec !== undefined) e.codec = codec;
      if (rate !== undefined) e.rate = rate;
      if (turn !== undefined) e.turn = turn;
      return e;
    }
    case "endpoint":
      return { type: "endpoint" };
    case "transcript":
      return typeof m.text === "string" ? { type: "transcript", text: m.text } : null;
    case "audio":
      return { type: "audio", rate: num(m.rate) ?? DEFAULT_OUTPUT_RATE };
    case "delta":
      return typeof m.text === "string" ? { type: "delta", text: m.text } : null;
    case "answer":
      return typeof m.text === "string" ? { type: "answer", text: m.text, replaces: m.replaces === true } : null;
    case "note": {
      const shareId = str(m.shareId);
      const userItemId = str(m.userItemId);
      return shareId && userItemId ? { type: "note", shareId, userItemId } : null;
    }
    case "reply": {
      if (typeof m.text !== "string") return null;
      const e: VoiceEvent = { type: "reply", text: m.text };
      const shareId = str(m.shareId);
      const itemId = str(m.itemId);
      if (shareId) e.shareId = shareId;
      if (itemId) e.itemId = itemId;
      return e;
    }
    case "done": {
      const ms: Record<string, number | string> = {};
      if (m.ms && typeof m.ms === "object") {
        for (const [k, val] of Object.entries(m.ms as Record<string, unknown>)) {
          if (typeof val === "number" || typeof val === "string") ms[k] = val;
        }
      }
      const e: VoiceEvent = { type: "done", ms };
      const shareId = str(m.shareId);
      if (shareId) e.shareId = shareId;
      return e;
    }
    case "error": {
      const e: VoiceEvent = { type: "error", code: str(m.code) ?? "unknown" };
      const message = str(m.message);
      const shareId = str(m.shareId);
      if (message) e.message = message;
      if (shareId) e.shareId = shareId;
      return e;
    }
    default:
      return null;
  }
}

export interface StartOptions {
  mode: "ptt" | "vad";
  purpose: VoicePurpose;
  thread: "main" | "voice";
  source?: string;
  sessionId?: string;
  replyTo?: string;
  lang?: string;
  /** command only */
  text?: boolean;
  /** command only */
  speak?: boolean;
}

const ID = /^[A-Za-z0-9_-]{1,64}$/;
const SESSION = /^[A-Za-z0-9_-]{0,64}$/;
const SOURCE = /^[a-z0-9-]{1,32}$/;

/** The `start` message (10.1). Values the server would refuse are dropped here. */
export function buildStart(o: StartOptions): Record<string, unknown> {
  const m: Record<string, unknown> = {
    type: "start",
    mode: o.mode,
    purpose: o.purpose,
    codec: "pcm16",
    rate: INPUT_RATE,
    thread: o.thread,
    channel: "web",
  };
  if (o.lang) m.lang = o.lang;
  if (o.sessionId && SESSION.test(o.sessionId)) m.sessionId = o.sessionId;
  if (o.purpose === "note") {
    if (o.replyTo && ID.test(o.replyTo)) m.replyTo = o.replyTo;
    if (o.source && SOURCE.test(o.source)) m.source = o.source;
  } else {
    if (o.text) m.text = true;
    if (o.speak === false) m.speak = false;
  }
  return m;
}

/** Split a PCM buffer into frames of at most `max` bytes, on sample boundaries. */
export function splitFrames(pcm: ArrayBuffer, max = MAX_FRAME_BYTES): ArrayBuffer[] {
  const limit = max - (max % 2);
  if (pcm.byteLength <= limit) return [pcm];
  const out: ArrayBuffer[] = [];
  for (let at = 0; at < pcm.byteLength; at += limit) out.push(pcm.slice(at, Math.min(at + limit, pcm.byteLength)));
  return out;
}

export function voiceUrl(loc: Pick<Location, "protocol" | "host"> = location): string {
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}${VOICE_WS_PATH}`;
}

/** Milliseconds of 16 kHz s16le in `bytes`. */
export function inputMs(bytes: number): number {
  return Math.round((bytes / 2 / INPUT_RATE) * 1000);
}

/** Reconnect delay: 1, 2, 4 ... 30 s. `attempt` counts from 0. */
export function backoffMs(attempt: number): number {
  return Math.min(30_000, 1000 * 2 ** Math.max(0, attempt));
}
