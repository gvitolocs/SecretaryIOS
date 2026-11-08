// The streaming voice client: one WebSocket to `/v1/voice/ws` (cookie auth on the same origin; the
// server checks `Origin` against `Host`), opened when a voice UI shows and kept while one is mounted.
// Protocol: docs/lucinavoice-protocol.md sections 2 and 10, parsed in voiceProtocol.ts.
//
//   const release = voiceSocket.acquire();        // a voice UI mounted
//   const turn = voiceSocket.startTurn(opts, (e) => ...);
//   turn.audio(pcm) ... turn.end();               // or turn.cancel()
//   release();                                    // unmounted; the socket closes 60 s after the last release
//
// One turn at a time. A note frees the device at `note`; its `reply` and `done` then come keyed by
// `shareId` and go to the handler the note was started with.
//
// Timings (end -> transcript / first delta or answer / first audio / note / reply) are logged with
// console.debug under the debug flag (localStorage "secretary.voice.debug" = "1"). Never audio,
// text, ids or cookies.

import {
  backoffMs,
  buildStart,
  DEFAULT_OUTPUT_RATE,
  NOTE_MAX_BYTES,
  parseServerMessage,
  splitFrames,
  voiceUrl,
  type StartOptions,
  type VoiceEvent,
  type VoiceTurnEvent,
} from "./voiceProtocol";

export const IDLE_CLOSE_MS = 60_000;
/** The server watches at most this many notes per connection. */
export const MAX_PENDING_NOTES = 4;
const DEBUG_KEY = "secretary.voice.debug";
const OPEN = 1;

export interface WebSocketLike {
  readyState: number;
  binaryType: string;
  onopen: ((e?: unknown) => void) | null;
  onmessage: ((e: { data: unknown }) => void) | null;
  onclose: ((e?: unknown) => void) | null;
  onerror: ((e?: unknown) => void) | null;
  send(data: string | ArrayBuffer): void;
  close(): void;
}

export type SocketStatus = "closed" | "connecting" | "open";
export type TurnHandler = (e: VoiceTurnEvent) => void;

export interface Timings {
  /** ms from `end` to each milestone; absent until it happened. */
  transcript?: number;
  firstText?: number;
  firstAudio?: number;
  note?: number;
  reply?: number;
}

export interface Turn {
  /** Send mic audio (s16le, 16 kHz). Ignored once the turn ended. Stops itself at the note cap. */
  audio(pcm: ArrayBuffer): void;
  /** The owner let go (or pressed again): the server starts work now. */
  end(): void;
  /** Drop it: nothing is stored for a note not yet posted; stops audio at once. */
  cancel(): void;
  readonly purpose: "command" | "note";
  readonly timings: Timings;
  readonly state: TurnState;
}

export type TurnState = "streaming" | "ended" | "released" | "finished";

export interface VoiceSocketOptions {
  url?: () => string;
  socket?: (url: string) => WebSocketLike;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (id: unknown) => void;
  debug?: () => boolean;
  idleCloseMs?: number;
}

interface TurnRec {
  id: number;
  purpose: "command" | "note";
  handler: TurnHandler;
  state: TurnState;
  sentBytes: number;
  capBytes: number;
  tEnd: number | null;
  timings: Timings;
  shareId?: string;
}

function readDebug(): boolean {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem(DEBUG_KEY) === "1";
  } catch {
    return false;
  }
}

export class VoiceSocket {
  private ws: WebSocketLike | null = null;
  private refs = 0;
  private status_: SocketStatus = "closed";
  private attempt = 0;
  private reconnectTimer: unknown = null;
  private idleTimer: unknown = null;
  private current: TurnRec | null = null;
  private pending = new Map<string, TurnRec>();
  private seq = 0;
  private statusListeners = new Set<(s: SocketStatus) => void>();
  private oldServer_ = false;
  private outRate = DEFAULT_OUTPUT_RATE;

  constructor(private readonly o: VoiceSocketOptions = {}) {}

  private now = () => (this.o.now ? this.o.now() : performance.now());
  private setTimer = (fn: () => void, ms: number) => (this.o.setTimer ? this.o.setTimer(fn, ms) : setTimeout(fn, ms));
  private clearTimer = (id: unknown) => (this.o.clearTimer ? this.o.clearTimer(id) : clearTimeout(id as ReturnType<typeof setTimeout>));

  get status(): SocketStatus {
    return this.status_;
  }

  /** The server answered `listening` without `codec`: it needs an update for streaming voice. */
  get oldServer(): boolean {
    return this.oldServer_;
  }

  /** A turn is running (not a note that is already with the server). */
  get busy(): boolean {
    return this.current !== null;
  }

  onStatus(cb: (s: SocketStatus) => void): () => void {
    this.statusListeners.add(cb);
    return () => this.statusListeners.delete(cb);
  }

  private setStatus(s: SocketStatus) {
    if (this.status_ === s) return;
    this.status_ = s;
    for (const cb of this.statusListeners) cb(s);
  }

  /** A voice UI is showing: connect and keep the socket. Returns the release for when it unmounts. */
  acquire(): () => void {
    this.refs++;
    if (this.idleTimer !== null) {
      this.clearTimer(this.idleTimer);
      this.idleTimer = null;
    }
    if (!this.ws && this.reconnectTimer === null) this.connect();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.refs = Math.max(0, this.refs - 1);
      if (this.refs === 0) {
        if (this.reconnectTimer !== null) {
          this.clearTimer(this.reconnectTimer);
          this.reconnectTimer = null;
        }
        if (this.idleTimer === null) this.idleTimer = this.setTimer(() => this.idleClose(), this.o.idleCloseMs ?? IDLE_CLOSE_MS);
      }
    };
  }

  private idleClose() {
    this.idleTimer = null;
    if (this.refs > 0) return;
    this.closeSocket();
  }

  private closeSocket() {
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        // Already closing.
      }
    }
    this.failTurns("closed");
    this.setStatus("closed");
  }

  private connect() {
    if (this.ws) return;
    const url = this.o.url ? this.o.url() : voiceUrl();
    let ws: WebSocketLike;
    try {
      ws = this.o.socket ? this.o.socket(url) : (new WebSocket(url) as unknown as WebSocketLike);
    } catch {
      this.scheduleReconnect();
      return;
    }
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    this.setStatus("connecting");
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.attempt = 0;
      this.oldServer_ = false;
      this.setStatus("open");
    };
    ws.onmessage = (e) => {
      if (this.ws === ws) this.onData(e.data);
    };
    ws.onerror = () => {
      // The close that follows does the work.
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.failTurns("closed");
      this.setStatus("closed");
      if (this.refs > 0) this.scheduleReconnect();
    };
  }

  private scheduleReconnect() {
    if (this.reconnectTimer !== null || this.refs === 0) return;
    const delay = backoffMs(this.attempt++);
    this.reconnectTimer = this.setTimer(() => {
      this.reconnectTimer = null;
      if (this.refs > 0) this.connect();
    }, delay);
  }

  /** The delay the next reconnect would wait (tests, and a "reconnecting in" hint). */
  get nextBackoffMs(): number {
    return backoffMs(this.attempt);
  }

  private failTurns(why: "closed") {
    const cur = this.current;
    this.current = null;
    if (cur && cur.state !== "finished") {
      cur.state = "finished";
      cur.handler({ type: why });
    }
    // A note already with the server stays durable there; its follow-up is just not watched any more.
    this.pending.clear();
  }

  private send(data: string | ArrayBuffer): boolean {
    if (this.ws && this.ws.readyState === OPEN) {
      this.ws.send(data);
      return true;
    }
    return false;
  }

  /**
   * Start a turn. Returns null when it cannot start: the socket is not open, or another turn is running.
   * The caller then shows "offline" or "busy".
   */
  startTurn(opts: StartOptions, handler: TurnHandler): Turn | null {
    if (this.current || !this.ws || this.ws.readyState !== OPEN) return null;
    const rec: TurnRec = {
      id: ++this.seq,
      purpose: opts.purpose,
      handler,
      state: "streaming",
      sentBytes: 0,
      capBytes: opts.purpose === "note" ? NOTE_MAX_BYTES : Number.POSITIVE_INFINITY,
      tEnd: null,
      timings: {},
    };
    this.current = rec;
    this.outRate = DEFAULT_OUTPUT_RATE;
    this.send(JSON.stringify(buildStart(opts)));
    const self = this;
    return {
      purpose: rec.purpose,
      get timings() {
        return rec.timings;
      },
      get state() {
        return rec.state;
      },
      audio(pcm) {
        if (rec.state !== "streaming" || self.current !== rec) return;
        const room = rec.capBytes - rec.sentBytes;
        let buf = pcm;
        if (buf.byteLength > room) buf = buf.slice(0, Math.max(0, room - (room % 2)));
        for (const f of splitFrames(buf)) {
          if (f.byteLength === 0) continue;
          self.send(f);
          rec.sentBytes += f.byteLength;
        }
        if (rec.sentBytes >= rec.capBytes) {
          self.endTurn(rec);
          rec.handler({ type: "capped" });
        }
      },
      end() {
        self.endTurn(rec);
      },
      cancel() {
        self.cancelTurn(rec);
      },
    };
  }

  private endTurn(rec: TurnRec) {
    if (rec.state !== "streaming" || this.current !== rec) return;
    rec.state = "ended";
    rec.tEnd = this.now();
    this.send(JSON.stringify({ type: "end" }));
  }

  private cancelTurn(rec: TurnRec) {
    if (rec.state === "finished" || rec.state === "released" || this.current !== rec) return;
    rec.state = "finished";
    this.current = null;
    this.send(JSON.stringify({ type: "cancel" }));
  }

  private finish(rec: TurnRec) {
    rec.state = "finished";
    if (this.current === rec) this.current = null;
    if (rec.shareId) this.pending.delete(rec.shareId);
  }

  private mark(rec: TurnRec, key: keyof Timings) {
    if (rec.timings[key] !== undefined || rec.tEnd === null) return;
    rec.timings[key] = Math.round(this.now() - rec.tEnd);
  }

  private log(rec: TurnRec) {
    if (!(this.o.debug ? this.o.debug() : readDebug())) return;
    console.debug("[voice]", rec.purpose, { ...rec.timings });
  }

  private onData(data: unknown) {
    if (typeof data === "string") {
      const ev = parseServerMessage(data);
      if (ev) this.route(ev);
      return;
    }
    if (data instanceof ArrayBuffer) {
      const rec = this.current;
      if (!rec || rec.purpose !== "command" || rec.state === "finished") return;
      this.mark(rec, "firstAudio");
      rec.handler({ type: "frame", pcm: data, rate: this.outRate });
    }
  }

  private route(ev: VoiceEvent) {
    // Anything about a note after `note` carries its shareId (10.4).
    const byShare = "shareId" in ev && ev.shareId ? this.pending.get(ev.shareId) : undefined;
    if (byShare) {
      this.routeNoteFollowUp(byShare, ev);
      return;
    }
    const rec = this.current;
    if (!rec) return;
    if ("shareId" in ev && ev.shareId && ev.type !== "note") return; // a note we no longer watch
    switch (ev.type) {
      case "listening":
        if (ev.codec === undefined) {
          // An older server (before section 10): drop the turn and let the caller use its old path.
          this.oldServer_ = true;
          this.cancelTurn(rec);
          rec.handler({ type: "unsupported" });
          return;
        }
        break;
      case "audio":
        this.outRate = ev.rate;
        break;
      case "transcript":
        this.mark(rec, "transcript");
        break;
      case "delta":
      case "answer":
        this.mark(rec, "firstText");
        break;
      case "endpoint":
        // The server decided the end (VAD or its cap): it is as if `end` had been sent.
        if (rec.state === "streaming") {
          rec.state = "ended";
          rec.tEnd = this.now();
        }
        break;
      case "note":
        this.mark(rec, "note");
        rec.shareId = ev.shareId;
        rec.state = "released";
        this.current = null;
        this.pending.set(ev.shareId, rec);
        while (this.pending.size > MAX_PENDING_NOTES) {
          const oldest = this.pending.keys().next().value;
          if (oldest === undefined) break;
          this.pending.delete(oldest);
        }
        break;
      case "reply":
        this.mark(rec, "reply");
        break;
      case "done":
        this.log(rec);
        rec.handler(ev);
        this.finish(rec);
        return;
      case "error":
        this.log(rec);
        rec.handler(ev);
        this.finish(rec);
        return;
    }
    rec.handler(ev);
  }

  private routeNoteFollowUp(rec: TurnRec, ev: VoiceEvent) {
    if (ev.type === "reply") this.mark(rec, "reply");
    rec.handler(ev);
    if (ev.type === "done" || ev.type === "error") {
      this.log(rec);
      this.finish(rec);
    }
  }
}

let shared: VoiceSocket | null = null;

/** The one socket of the app. */
export function voiceSocket(): VoiceSocket {
  return (shared ??= new VoiceSocket());
}

/** Tests: use another socket object (null: back to a fresh default). */
export function setVoiceSocketForTests(s: VoiceSocket | null): void {
  shared = s;
}
