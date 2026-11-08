// A mock of the voice WebSocket for tests: records what the client sends, lets the test push what the
// server says.

import { VoiceSocket, type VoiceSocketOptions, type WebSocketLike } from "../lib/voiceSocket";

export class MockVoiceWs implements WebSocketLike {
  readyState = 0;
  binaryType = "blob";
  onopen: ((e?: unknown) => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: ((e?: unknown) => void) | null = null;
  onerror: ((e?: unknown) => void) | null = null;
  /** Text frames the client sent, parsed. */
  json: Record<string, unknown>[] = [];
  /** Binary frames the client sent. */
  frames: ArrayBuffer[] = [];
  closed = false;

  constructor(readonly url: string) {}

  send(data: string | ArrayBuffer): void {
    if (typeof data === "string") this.json.push(JSON.parse(data) as Record<string, unknown>);
    else this.frames.push(data);
  }

  close(): void {
    this.closed = true;
    this.readyState = 3;
  }

  // --- the test's side ---
  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }

  say(msg: Record<string, unknown>): void {
    this.onmessage?.({ data: JSON.stringify(msg) });
  }

  sayRaw(raw: string): void {
    this.onmessage?.({ data: raw });
  }

  audio(bytes: number): void {
    this.onmessage?.({ data: new ArrayBuffer(bytes) });
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.();
  }

  get types(): string[] {
    return this.json.map((m) => String(m.type));
  }

  get framedBytes(): number {
    return this.frames.reduce((n, f) => n + f.byteLength, 0);
  }
}

export interface Rig {
  vs: VoiceSocket;
  sockets: MockVoiceWs[];
  last(): MockVoiceWs;
  /** Opens a socket and returns it. */
  connect(): MockVoiceWs;
  clock: { t: number };
}

export function rig(extra: VoiceSocketOptions = {}): Rig {
  const sockets: MockVoiceWs[] = [];
  const clock = { t: 0 };
  const vs = new VoiceSocket({
    url: () => "wss://example.test/v1/voice/ws",
    socket: (url) => {
      const s = new MockVoiceWs(url);
      sockets.push(s);
      return s;
    },
    now: () => clock.t,
    ...extra,
  });
  return {
    vs,
    sockets,
    clock,
    last: () => sockets[sockets.length - 1]!,
    connect() {
      vs.acquire();
      const s = sockets[sockets.length - 1]!;
      s.open();
      return s;
    },
  };
}

export const pcm = (bytes: number): ArrayBuffer => new ArrayBuffer(bytes);
