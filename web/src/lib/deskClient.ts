// One LucinaDesk socket: a ticket, a WebSocket speaking `lucinadesk.v1`, and a WebCodecs decoder
// whose frames the caller draws. The page keeps one of these per mode: a takeover opens a control
// socket and closes the watch one once the control socket has shown a frame (the contract has no
// in-place switch), so the picture never goes black.

import { deskApi, type DeskTicket } from "../api/desk";
import type { Sandbox } from "../api/computer";
import {
  CODEC_PREFERENCE,
  DESK_PROTOCOL,
  WEBCODECS,
  parseServer,
  parseVideo,
  type ClientMessage,
  type DeskCodec,
  type DeskDisplay,
  type ServerMessage,
  type WireFrame,
} from "./deskWire";

export type DeskMode = "watch" | "control";

export type DeskPhase = "ticket" | "connecting" | "relay" | "secured" | "live" | "closed";

export interface DeskSink {
  phase(p: DeskPhase): void;
  login(display: DeskDisplay, info: { hostname: string; platform: string; version: string }): void;
  /** A decoded frame: draw it, then the socket closes it. */
  draw(frame: VideoFrame): void;
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

/** The codecs WebCodecs can decode here, best first (probed once). */
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
      return ok;
    })();
  }
  return decodable;
}

export interface DeskStats {
  /** Ticket request → first frame on screen, ms (this socket). */
  firstFrameMs: number | null;
  frames: number;
  bytes: number;
  codec: DeskCodec | null;
  rttMs: number | null;
  via: "lan" | "p1" | null;
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
  readonly stats: DeskStats = { firstFrameMs: null, frames: 0, bytes: 0, codec: null, rttMs: null, via: null };
  private ws: WebSocket | null = null;
  private decoder: VideoDecoder | null = null;
  private decoderCodec: DeskCodec | null = null;
  private waitingKey = true;
  /** Timestamps (µs) of frames flagged "last": ack when they come out of the decoder. */
  private ackAt = new Set<number>();
  private t0 = 0;
  private pinger: number | undefined;
  private opened = false;
  private done = false;
  private pendingMove: ClientMessage | null = null;
  private moveRaf = 0;

  constructor(
    private readonly sandbox: Sandbox,
    mode: DeskMode,
    private readonly sink: DeskSink,
  ) {
    this.mode = mode;
  }

  /** Mint a ticket and connect (the home door first when offered, then p1). */
  async start(): Promise<void> {
    this.t0 = performance.now();
    this.sink.phase("ticket");
    const codecs = await decodableCodecs();
    let ticket = await deskApi.ticket(this.sandbox, this.mode, codecs);
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

  /** Resolves true once open (false if it failed first, or did not open within `timeoutMs`). */
  private connect(base: string, ticket: DeskTicket, timeoutMs: number): Promise<boolean> {
    this.sink.phase("connecting");
    return new Promise((resolve) => {
      let ws: WebSocket;
      try {
        ws = new WebSocket(base + ticket.wsPath, [DESK_PROTOCOL, `ticket.${ticket.ticket}`]);
      } catch {
        resolve(false);
        return;
      }
      ws.binaryType = "arraybuffer";
      let settled = false;
      const timer = timeoutMs
        ? window.setTimeout(() => {
            if (settled) return;
            settled = true;
            ws.onclose = null;
            ws.close();
            resolve(false);
          }, timeoutMs)
        : undefined;
      ws.onopen = () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        this.ws = ws;
        this.opened = true;
        if (this.done) {
          ws.close(1000);
          resolve(true);
          return;
        }
        this.pinger = window.setInterval(() => this.send({ type: "ping", t: performance.now() }), PING_MS);
        resolve(true);
      };
      ws.onmessage = (m) => this.onMessage(m.data);
      ws.onclose = (e) => {
        window.clearTimeout(timer);
        if (!settled) {
          settled = true;
          // Not opened: on the LAN try the caller falls back; on p1 report it.
          if (timeoutMs) {
            resolve(false);
            return;
          }
          resolve(false);
        }
        this.finish(e.code, e.reason);
      };
    });
  }

  private onMessage(data: unknown) {
    if (data instanceof ArrayBuffer) {
      const f = parseVideo(data);
      if (f) this.onVideo(f);
      return;
    }
    if (typeof data !== "string") return;
    const m = parseServer(data);
    if (m) this.onText(m);
  }

  private onText(m: ServerMessage) {
    switch (m.type) {
      case "status":
        if (m.phase === "relay" || m.phase === "secured") this.sink.phase(m.phase);
        break;
      case "login": {
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

  private closedReason = "";

  private onVideo(f: WireFrame) {
    this.stats.bytes += f.data.byteLength;
    if (f.codec !== this.decoderCodec) {
      if (!this.makeDecoder(f.codec)) return;
      this.waitingKey = true;
    }
    if (this.waitingKey && !f.key) {
      // Nothing to show until a key frame; keep the host going and ask for one.
      if (f.last) this.send({ type: "ack" });
      this.send({ type: "refresh" });
      return;
    }
    this.waitingKey = false;
    const timestamp = f.pts * 1000;
    if (f.last) this.ackAt.add(timestamp);
    try {
      this.decoder!.decode(new EncodedVideoChunk({ type: f.key ? "key" : "delta", timestamp, data: f.data }));
    } catch {
      this.recover();
    }
  }

  private makeDecoder(codec: DeskCodec): boolean {
    try {
      this.decoder?.close();
    } catch {
      // already closed
    }
    this.decoder = new VideoDecoder({
      output: (frame) => {
        if (this.done) {
          frame.close();
          return;
        }
        try {
          this.sink.draw(frame);
        } finally {
          frame.close();
        }
        this.stats.frames++;
        if (this.stats.firstFrameMs === null) {
          this.stats.firstFrameMs = Math.round(performance.now() - this.t0);
          this.sink.phase("live");
        }
        if (this.ackAt.delete(frame.timestamp)) this.send({ type: "ack" });
      },
      error: (e) => {
        if (e?.name === "NotSupportedError") {
          this.sink.unsupported(codec);
          return;
        }
        this.recover();
      },
    });
    try {
      this.decoder.configure({ codec: WEBCODECS[codec], optimizeForLatency: true, hardwareAcceleration: "no-preference" });
    } catch {
      this.sink.unsupported(codec);
      return false;
    }
    this.decoderCodec = codec;
    this.stats.codec = codec;
    return true;
  }

  /** A decoder error: start over at the next key frame, and let the host go on. */
  private recover() {
    this.decoderCodec = null;
    this.waitingKey = true;
    if (this.ackAt.size) this.send({ type: "ack" });
    this.ackAt.clear();
    this.send({ type: "refresh" });
  }

  get open(): boolean {
    return !!this.ws && this.ws.readyState === WebSocket.OPEN;
  }

  send(m: ClientMessage): void {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(m));
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
    const ws = this.ws;
    this.cleanup();
    if (ws) {
      ws.onclose = null;
      ws.onmessage = null;
      try {
        ws.close(1000);
      } catch {
        // gone
      }
    }
  }

  private cleanup() {
    window.clearInterval(this.pinger);
    if (this.moveRaf) cancelAnimationFrame(this.moveRaf);
    try {
      if (this.decoder && this.decoder.state !== "closed") this.decoder.close();
    } catch {
      // gone
    }
    this.decoder = null;
  }

  private finish(code: number, reason: string) {
    if (this.done) return;
    this.done = true;
    this.cleanup();
    this.sink.phase("closed");
    this.sink.closed(code, this.closedReason || reason, this.opened);
  }
}
