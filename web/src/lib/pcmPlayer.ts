// Plays the voice socket's `audio` frames (s16le mono, 24 kHz) as they arrive: one AudioContext, one
// AudioBufferSourceNode per frame, started back to back on the context clock so there is no gap
// (and no overlap) between frames. The scheduling math is pure, for tests.

/** How far ahead of "now" the first frame (or the first after an underrun) is started, in seconds. */
export const START_LEAD_S = 0.04;

export interface Slot {
  /** Context time at which the frame starts. */
  at: number;
  /** Context time at which the next frame starts. */
  next: number;
}

/**
 * Where a frame of `samples` samples at `rate` goes: right after the previous one, or `lead` after
 * `now` when nothing is queued any more (the start, or the network was slower than playback).
 */
export function scheduleSlot(now: number, nextStart: number, samples: number, rate: number, lead = START_LEAD_S): Slot {
  const at = nextStart > now ? nextStart : now + lead;
  return { at, next: at + samples / rate };
}

/**
 * s16le bytes to floats. A sample may be split between frames: an odd trailing byte comes back in
 * `rest` and goes in front of the next frame.
 */
export function decodePcm16(bytes: Uint8Array, carry: number | null): { samples: Float32Array; rest: number | null } {
  let start = 0;
  let lead: number | null = null;
  if (carry !== null && bytes.length > 0) {
    lead = carry | (bytes[0]! << 8);
    start = 1;
  }
  const whole = Math.floor((bytes.length - start) / 2);
  const out = new Float32Array(whole + (lead !== null ? 1 : 0));
  let k = 0;
  const to = (lo: number, hi: number) => {
    const v = (lo | (hi << 8)) << 16 >> 16;
    return v / 32768;
  };
  if (lead !== null) out[k++] = to(lead & 0xff, lead >> 8);
  for (let i = 0; i < whole; i++) out[k++] = to(bytes[start + 2 * i]!, bytes[start + 2 * i + 1]!);
  const used = start + whole * 2;
  const rest = used < bytes.length ? bytes[used]! : carry !== null && bytes.length === 0 ? carry : null;
  return { samples: out, rest };
}

interface SourceLike {
  buffer: unknown;
  onended: (() => void) | null;
  connect(dest: unknown): unknown;
  start(when: number): void;
  stop(): void;
  disconnect(): void;
}

export interface ContextLike {
  readonly currentTime: number;
  readonly state: string;
  readonly destination: unknown;
  createBuffer(channels: number, length: number, rate: number): { copyToChannel(data: Float32Array, ch: number): void };
  createBufferSource(): SourceLike;
  resume(): Promise<void>;
  close(): Promise<void>;
}

export interface PcmPlayerOptions {
  createContext?: () => ContextLike;
  /** Playing (audio is scheduled or sounding) or not. */
  onPlaying?: (playing: boolean) => void;
}

export class PcmPlayer {
  private ctx: ContextLike | null = null;
  private next = 0;
  private carry: number | null = null;
  private live = new Set<SourceLike>();
  private finished = false;
  private playing = false;
  private onDrained: (() => void) | null = null;

  constructor(private readonly opts: PcmPlayerOptions = {}) {}

  private context(): ContextLike {
    if (!this.ctx) {
      this.ctx =
        this.opts.createContext?.() ??
        new ((globalThis as unknown as { AudioContext?: new () => ContextLike }).AudioContext ??
          (globalThis as unknown as { webkitAudioContext: new () => ContextLike }).webkitAudioContext)();
    }
    return this.ctx;
  }

  /** Call from a user gesture (iOS and Safari start the context only then). */
  async unlock(): Promise<void> {
    const c = this.context();
    if (c.state === "suspended") await c.resume();
  }

  private setPlaying(p: boolean) {
    if (this.playing === p) return;
    this.playing = p;
    this.opts.onPlaying?.(p);
  }

  /** Queue a frame of s16le mono at `rate`. */
  push(frame: ArrayBuffer, rate: number): void {
    if (this.finished) this.reset();
    const { samples, rest } = decodePcm16(new Uint8Array(frame), this.carry);
    this.carry = rest;
    if (samples.length === 0) return;
    const ctx = this.context();
    if (ctx.state === "suspended") void ctx.resume().catch(() => {});
    const buf = ctx.createBuffer(1, samples.length, rate);
    buf.copyToChannel(samples, 0);
    const slot = scheduleSlot(ctx.currentTime, this.next, samples.length, rate);
    this.next = slot.next;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(ctx.destination);
    src.onended = () => {
      this.live.delete(src);
      src.disconnect();
      this.checkDrained();
    };
    this.live.add(src);
    src.start(slot.at);
    this.setPlaying(true);
  }

  /** No more frames this turn; `cb` runs when what is queued has been played. */
  end(cb?: () => void): void {
    this.finished = true;
    this.onDrained = cb ?? null;
    this.checkDrained();
  }

  private checkDrained() {
    if (this.finished && this.live.size === 0) {
      this.setPlaying(false);
      const cb = this.onDrained;
      this.onDrained = null;
      cb?.();
    }
  }

  private reset() {
    this.finished = false;
    this.next = 0;
    this.carry = null;
  }

  /** Stop at once (cancel, mute): everything queued is dropped. */
  stop(): void {
    for (const s of this.live) {
      s.onended = null;
      try {
        s.stop();
      } catch {
        // Not started yet.
      }
      s.disconnect();
    }
    this.live.clear();
    this.reset();
    this.onDrained = null;
    this.setPlaying(false);
  }

  dispose(): void {
    this.stop();
    const c = this.ctx;
    this.ctx = null;
    void c?.close().catch(() => {});
  }

  get isPlaying(): boolean {
    return this.playing;
  }
}
