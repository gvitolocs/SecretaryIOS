import { describe, expect, it } from "vitest";
import { chunkBytes, chunkSamples, workletSource } from "./micCapture";
import { decodePcm16, PcmPlayer, scheduleSlot, START_LEAD_S, type ContextLike } from "./pcmPlayer";
import { MAX_FRAME_BYTES } from "./voiceProtocol";

describe("scheduleSlot", () => {
  it("starts the first frame a little after now", () => {
    const s = scheduleSlot(10, 0, 2400, 24000);
    expect(s.at).toBeCloseTo(10 + START_LEAD_S);
    expect(s.next).toBeCloseTo(10 + START_LEAD_S + 0.1);
  });

  it("puts the next frame exactly after the previous one, with no gap and no overlap", () => {
    let next = 0;
    const starts: number[] = [];
    const now = 5;
    for (let i = 0; i < 10; i++) {
      const s = scheduleSlot(now, next, 4096, 24000);
      starts.push(s.at);
      next = s.next;
    }
    for (let i = 1; i < starts.length; i++) expect(starts[i]! - starts[i - 1]!).toBeCloseTo(4096 / 24000, 9);
  });

  it("after an underrun it starts again just after now", () => {
    const first = scheduleSlot(1, 0, 2400, 24000);
    const late = scheduleSlot(first.next + 3, first.next, 2400, 24000);
    expect(late.at).toBeCloseTo(first.next + 3 + START_LEAD_S);
  });
});

describe("decodePcm16", () => {
  it("decodes s16le to floats", () => {
    const b = new Uint8Array([0x00, 0x00, 0x00, 0x40, 0x00, 0xc0, 0xff, 0x7f, 0x00, 0x80]);
    const { samples, rest } = decodePcm16(b, null);
    expect(rest).toBeNull();
    expect(Array.from(samples)).toEqual([0, 0.5, -0.5, 32767 / 32768, -1]);
  });

  it("keeps a sample split between frames", () => {
    const whole = new Uint8Array([0x00, 0x40, 0x00, 0xc0]); // 0.5, -0.5
    const a = decodePcm16(whole.subarray(0, 3), null);
    expect(Array.from(a.samples)).toEqual([0.5]);
    expect(a.rest).toBe(0x00);
    const b = decodePcm16(whole.subarray(3), a.rest);
    expect(Array.from(b.samples)).toEqual([-0.5]);
    expect(b.rest).toBeNull();
  });

  it("carries a lone byte across an empty frame", () => {
    expect(decodePcm16(new Uint8Array([]), 7).rest).toBe(7);
  });
});

class FakeSource {
  buffer: unknown = null;
  onended: (() => void) | null = null;
  startedAt: number | null = null;
  stopped = false;
  connect() {}
  disconnect() {}
  start(when: number) {
    this.startedAt = when;
  }
  stop() {
    this.stopped = true;
  }
}

class FakeCtx implements ContextLike {
  currentTime = 0;
  state = "running";
  destination = {};
  sources: FakeSource[] = [];
  lengths: number[] = [];
  rates: number[] = [];
  createBuffer(_c: number, length: number, rate: number) {
    this.lengths.push(length);
    this.rates.push(rate);
    return { copyToChannel() {} };
  }
  createBufferSource() {
    const s = new FakeSource();
    this.sources.push(s);
    return s;
  }
  async resume() {
    this.state = "running";
  }
  async close() {}
}

describe("PcmPlayer", () => {
  const make = () => {
    const ctx = new FakeCtx();
    const states: boolean[] = [];
    const player = new PcmPlayer({ createContext: () => ctx, onPlaying: (p) => states.push(p) });
    return { ctx, states, player };
  };

  it("schedules frames back to back on the context clock at the frame rate", () => {
    const { ctx, player, states } = make();
    ctx.currentTime = 2;
    player.push(new ArrayBuffer(9600), 24000); // 4800 samples = 0.2 s
    player.push(new ArrayBuffer(9600), 24000);
    player.push(new ArrayBuffer(4800), 24000);
    expect(ctx.rates).toEqual([24000, 24000, 24000]);
    expect(ctx.lengths).toEqual([4800, 4800, 2400]);
    const at = ctx.sources.map((s) => s.startedAt!);
    expect(at[0]).toBeCloseTo(2 + START_LEAD_S);
    expect(at[1]! - at[0]!).toBeCloseTo(0.2, 9);
    expect(at[2]! - at[1]!).toBeCloseTo(0.2, 9);
    expect(states).toEqual([true]);
  });

  it("joins a sample split across frames and plays nothing for a lone byte", () => {
    const { ctx, player } = make();
    player.push(new Uint8Array([0x00]).buffer, 24000);
    expect(ctx.sources).toHaveLength(0);
    player.push(new Uint8Array([0x40, 0x00, 0x40]).buffer, 24000);
    expect(ctx.lengths).toEqual([2]); // 0x4000 (split) and 0x4000
  });

  it("reports playing until what is queued has played, then runs the end callback once", () => {
    const { ctx, player, states } = make();
    player.push(new ArrayBuffer(960), 24000);
    player.push(new ArrayBuffer(960), 24000);
    let done = 0;
    player.end(() => done++);
    expect(done).toBe(0);
    ctx.sources[0]!.onended?.();
    expect(done).toBe(0);
    ctx.sources[1]!.onended?.();
    expect(done).toBe(1);
    expect(states).toEqual([true, false]);
  });

  it("runs the end callback at once when nothing was queued", () => {
    const { player } = make();
    let done = 0;
    player.end(() => done++);
    expect(done).toBe(1);
  });

  it("stop drops everything queued and the end callback", () => {
    const { ctx, player, states } = make();
    player.push(new ArrayBuffer(960), 24000);
    player.push(new ArrayBuffer(960), 24000);
    let done = 0;
    player.end(() => done++);
    player.stop();
    expect(ctx.sources.every((s) => s.stopped)).toBe(true);
    expect(states).toEqual([true, false]);
    expect(done).toBe(0);
    // A new turn starts fresh.
    ctx.currentTime = 9;
    player.push(new ArrayBuffer(960), 24000);
    expect(ctx.sources[2]!.startedAt).toBeCloseTo(9 + START_LEAD_S);
  });
});

describe("microphone chunks", () => {
  it("60 ms is 960 samples, 1920 bytes: inside the 20-100 ms and 8 KiB limits", () => {
    expect(chunkSamples(60)).toBe(960);
    expect(chunkBytes(60)).toBe(1920);
    expect(chunkBytes(60)).toBeLessThanOrEqual(MAX_FRAME_BYTES);
    expect(chunkBytes(100)).toBe(3200);
    expect(chunkBytes(20)).toBe(640);
  });

  it("the worklet posts chunks of exactly that size", () => {
    const src = workletSource("mic-tap-60", chunkSamples(60));
    expect(src).toContain("new Int16Array(960)");
    expect(src).toContain("this.k === 960");
    expect(src).toContain('registerProcessor("mic-tap-60"');
    expect(src).toContain("sampleRate / 16000");
  });
});
