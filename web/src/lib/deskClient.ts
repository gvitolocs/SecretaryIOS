// One LucinaDesk socket: a ticket, a WebSocket speaking `lucinadesk.v1`, and a WebCodecs decoder.
// The page keeps one of these per mode: a takeover opens a control socket and closes the watch one
// once the control socket has shown a frame (the contract has no in-place switch), so the picture
// never goes black.
//
// With a pipe (lib/deskPipe.ts) the socket and the decoder run in the player's worker, which draws on
// the canvas itself; this side keeps the ticket, the text messages and the input. Without one (no
// OffscreenCanvas, tests) it is all here, and `sink.draw` gets the frames.

import { deskApi, type DeskTicket } from "../api/desk";
import type { Sandbox } from "../api/computer";
import type { DeskPipe } from "./deskPipe";
import { DeskVideo } from "./deskVideo";
import {
  CODEC_PREFERENCE,
  DESK_PROTOCOL,
  WEBCODECS,
  parseServer,
  type ClientMessage,
  type DeskCodec,
  type DeskDisplay,
  type ServerMessage,
} from "./deskWire";

export type DeskMode = "watch" | "control";

export type DeskPhase = "ticket" | "connecting" | "relay" | "secured" | "live" | "closed";

export interface DeskSink {
  phase(p: DeskPhase): void;
  login(display: DeskDisplay, info: { hostname: string; platform: string; version: string }): void;
  /** A decoded frame to draw (no pipe); the socket closes it afterwards. */
  draw(frame: VideoFrame): void;
  /** A frame is on the canvas (either way): its size, for mapping the pointer. */
  drawn(width: number, height: number): void;
  cursor?(x: number, y: number): void;
  refused?(what: string, why: string): void;
  /** The socket is gone. `code` per CONTRACT §6 (1006 also for a refused ticket). */
  closed(code: number, reason: string, wasOpen: boolean): void;
  /** The host sends a codec this browser cannot decode. */
  unsupported(codec: DeskCodec): void;
}

/** True when this browser can show LucinaDesk at all (WebCodecs + WebSocket). */
export function deskCapable(): boolean {
  return typeof window !== "undefined" && typeof window.VideoDecoder === "function" && typeof window.EncodedVideoChunk === "function" && typeof WebSocket === "function";
}

let decodable: Promise<DeskCodec[]> | null = null;
const CODECS_KEY = "desk.codecs";
/** A first visit's guess, before the probe has answered: what every browser we target decodes. */
const DEFAULT_CODECS: DeskCodec[] = ["vp9", "av1", "h264"];

/** The codecs WebCodecs can decode here, best first (probed once per page; remembered per device). */
export function decodableCodecs(): Promise<DeskCodec[]> {
  if (!decodable) {
    decodable = (async () => {
      if (!deskCapable()) return [];
      const ok: DeskCodec[] = [];
      for (const c of CODEC_PREFERENCE) {
        try {
          const r = await VideoDecoder.isConfigSupported({ codec: WEBCODECS[c], optimizeForLatency: true });
          if (r.supported) ok.push(c);
        } catch {
          // not this one
        }
      }
      try {
        localStorage.setItem(CODECS_KEY, ok.join(","));
      } catch {
        // private mode: probed again next time
      }
      return ok;
    })();
  }
  return decodable;
}

/**
 * The codecs to ask for now, without waiting: Chrome's first `isConfigSupported` takes about half a
 * second (it starts the media stack), which would come before the first picture. This device's last
 * probe, else a guess; the probe runs meanwhile, and a wrong guess gets one retry (`DeskPlayer`).
 */
export function knownCodecs(): DeskCodec[] {
  void decodableCodecs();
  try {
    const saved = localStorage.getItem(CODECS_KEY);
    if (saved) {
      const list = saved.split(",").filter((c): c is DeskCodec => (CODEC_PREFERENCE as string[]).includes(c));
      if (list.length) return list;
    }
  } catch {
    // no storage
  }
  return DEFAULT_CODECS;
}

export interface DeskStats {
  /** Ticket request → first frame on screen, ms (this socket). */
  firstFrameMs: number | null;
  frames: number;
  bytes: number;
  codec: DeskCodec | null;
  rttMs: number | null;
  via: "lan" | "p1" | null;
  /** Milestones since `start()`, ms: for finding where the first picture's time goes. */
  marks: { ticket?: number; pipe?: number; open?: number; login?: number; frame?: number };
}

const LAN_TIMEOUT_MS = 1000;
const PING_MS = 2000;
/** The home door failed: go straight to p1 for a while (a closed port times out rather than refusing). */
const LAN_RETRY_MS = 10 * 60_000;
const LAN_KEY = "desk.lanFailedAt";

function lanBroken(): boolean {
  try {
    return Date.now() - Number(localStorage.getItem(LAN_KEY) ?? 0) < LAN_RETRY_MS;
  } catch {
    return false;
  }
}

function markLanBroken(): void {
  try {
    localStorage.setItem(LAN_KEY, String(Date.now()));
  } catch {
    // private mode: this page tries the door again next time
  }
}

function wsBase(): string {
  return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}`;
}

export class DeskSocket {
  readonly mode: DeskMode;
  readonly stats: DeskStats = { firstFrameMs: null, frames: 0, bytes: 0, codec: null, rttMs: null, via: null, marks: {} };
  /** No pipe: the socket and decoder here. */
  private ws: WebSocket | null = null;
  private video: DeskVideo | null = null;
  /** With a pipe: this socket's id in the worker. */
  private pipe: DeskPipe | null = null;
  private pipeId = 0;
  private current = false;
  private isOpen = false;
  private t0 = 0;
  private pinger: number | undefined;
  private opened = false;
  private done = false;
  private closedReason = "";
  private pendingMove: ClientMessage | null = null;
  private moveRaf = 0;

  constructor(
    private readonly sandbox: Sandbox,
    mode: DeskMode,
    private readonly sink: DeskSink,
    private readonly pipeReady: Promise<DeskPipe | null> = Promise.resolve(null),
  ) {
    this.mode = mode;
  }

  /** Mint a ticket and connect (the home door first when offered, then p1). */
  async start(): Promise<void> {
    this.t0 = performance.now();
    this.sink.phase("ticket");
    const codecs = knownCodecs();
    const mark = (k: keyof DeskStats["marks"]) => <T,>(v: T) => ((this.stats.marks[k] ??= Math.round(performance.now() - this.t0)), v);
    let [ticket, pipe] = await Promise.all([deskApi.ticket(this.sandbox, this.mode, codecs).then(mark("ticket")), this.pipeReady.then(mark("pipe"))]);
    this.pipe = pipe;
    if (this.done) return;
    if (ticket.lan?.url && !lanBroken()) {
      const lan = ticket.lan.url.replace(/^http/, "ws");
      if (await this.connect(lan, ticket, LAN_TIMEOUT_MS)) {
        this.stats.via = "lan";
        return;
      }
      markLanBroken();
      if (this.done) return;
      // The failed attempt may have used the ticket up: a new one for p1.
      ticket = await deskApi.ticket(this.sandbox, this.mode, codecs);
      if (this.done) return;
    }
    this.stats.via = "p1";
    await this.connect(wsBase(), ticket, 0);
  }

  /** This socket's frames are the ones on the canvas (the player promoted it). */
  makeCurrent(): void {
    this.current = true;
    if (this.pipe && this.pipeId) this.pipe.setCurrent(this.pipeId);
  }

  /** Resolves true once open (false if it failed first, or did not open within `timeoutMs`). */
  private connect(base: string, ticket: DeskTicket, timeoutMs: number): Promise<boolean> {
    this.sink.phase("connecting");
    const url = base + ticket.wsPath;
    const protocols = [DESK_PROTOCOL, `ticket.${ticket.ticket}`];
    return new Promise((resolve) => {
      let settled = false;
      let drop = () => undefined as void;
      const timer = timeoutMs
        ? window.setTimeout(() => {
            if (settled) return;
            settled = true;
            drop();
            resolve(false);
          }, timeoutMs)
        : undefined;
      const opened = () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        this.stats.marks.open ??= Math.round(performance.now() - this.t0);
        this.isOpen = true;
        this.opened = true;
        if (this.done) {
          this.shut();
          resolve(true);
          return;
        }
        this.pinger = window.setInterval(() => this.send({ type: "ping", t: performance.now() }), PING_MS);
        resolve(true);
      };
      const closed = (code: number, reason: string) => {
        window.clearTimeout(timer);
        this.isOpen = false;
        if (!settled) {
          settled = true;
          resolve(false);
          // Not opened on the home door: the caller falls back to p1, nothing to report.
          if (timeoutMs) return;
        }
        this.finish(code, reason);
      };

      if (this.pipe) {
        const pipe = this.pipe;
        const id = pipe.open(url, protocols, {
          open: opened,
          text: (data) => this.onText(data),
          frame: (f) => this.onFrame(f.width, f.height, f.codec, f.bytes),
          closed,
          unsupported: (codec) => this.sink.unsupported(codec),
        });
        this.pipeId = id;
        if (this.current) pipe.setCurrent(id);
        drop = () => pipe.close(id);
        return;
      }

      let ws: WebSocket;
      try {
        ws = new WebSocket(url, protocols);
      } catch {
        settled = true;
        window.clearTimeout(timer);
        resolve(false);
        if (!timeoutMs) this.finish(1006, "");
        return;
      }
      ws.binaryType = "arraybuffer";
      this.ws = ws;
      const video = new DeskVideo({
        send: (m) => this.send(m),
        output: (frame) => {
          if (this.done) return;
          this.sink.draw(frame);
          this.onFrame(frame.displayWidth, frame.displayHeight, video.codec, video.bytes);
        },
        unsupported: (codec) => this.sink.unsupported(codec),
      });
      this.video = video;
      drop = () => {
        ws.onclose = null;
        ws.close();
        video.close();
      };
      ws.onopen = opened;
      ws.onmessage = (m) => {
        if (m.data instanceof ArrayBuffer) video.push(m.data);
        else if (typeof m.data === "string") this.onText(m.data);
      };
      ws.onclose = (e) => closed(e.code, e.reason);
    });
  }

  private onFrame(width: number, height: number, codec: DeskCodec | null, bytes: number) {
    if (this.done) return;
    this.stats.frames++;
    this.stats.codec = codec;
    this.stats.bytes = bytes;
    this.sink.drawn(width, height);
    if (this.stats.firstFrameMs === null) {
      this.stats.firstFrameMs = Math.round(performance.now() - this.t0);
      this.stats.marks.frame = this.stats.firstFrameMs;
      this.sink.phase("live");
    }
  }

  private onText(data: string) {
    const m: ServerMessage | null = parseServer(data);
    if (!m) return;
    switch (m.type) {
      case "status":
        if (m.phase === "relay" || m.phase === "secured") this.sink.phase(m.phase);
        break;
      case "login": {
        this.stats.marks.login ??= Math.round(performance.now() - this.t0);
        const d = m.displays[m.currentDisplay] ?? m.displays[0]!;
        this.sink.login(d, { hostname: m.hostname, platform: m.platform, version: m.version });
        break;
      }
      case "cursor":
        this.sink.cursor?.(m.x, m.y);
        break;
      case "refused":
        this.sink.refused?.(m.what, m.why);
        break;
      case "pong":
        if (typeof m.t === "number") this.stats.rttMs = Math.round(performance.now() - m.t);
        break;
      case "closed":
        this.closedReason = m.reason;
        break;
      default:
        break;
    }
  }

  get open(): boolean {
    return this.isOpen && !this.done;
  }

  send(m: ClientMessage): void {
    if (!this.isOpen) return;
    if (this.pipe) this.pipe.send(this.pipeId, m);
    else if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(m));
  }

  /** Pointer moves, at most one per animation frame (the last one wins). */
  sendMove(m: ClientMessage): void {
    this.pendingMove = m;
    if (this.moveRaf) return;
    this.moveRaf = requestAnimationFrame(() => {
      this.moveRaf = 0;
      if (this.pendingMove) this.send(this.pendingMove);
      this.pendingMove = null;
    });
  }

  /** Flush a pending move first, so a click lands where the pointer is. */
  sendNow(m: ClientMessage): void {
    if (this.pendingMove) {
      this.send(this.pendingMove);
      this.pendingMove = null;
    }
    this.send(m);
  }

  close(): void {
    if (this.done) return;
    this.done = true;
    this.shut();
  }

  /** Close the transport without reporting it (we closed it). */
  private shut() {
    this.cleanup();
    if (this.pipe && this.pipeId) this.pipe.close(this.pipeId);
    const ws = this.ws;
    if (ws) {
      ws.onclose = null;
      ws.onmessage = null;
      try {
        ws.close(1000);
      } catch {
        // gone
      }
    }
    this.isOpen = false;
  }

  private cleanup() {
    window.clearInterval(this.pinger);
    if (this.moveRaf) cancelAnimationFrame(this.moveRaf);
    this.video?.close();
  }

  private finish(code: number, reason: string) {
    if (this.done) return;
    this.done = true;
    this.cleanup();
    this.sink.phase("closed");
    this.sink.closed(code, this.closedReason || reason, this.opened);
  }
}
