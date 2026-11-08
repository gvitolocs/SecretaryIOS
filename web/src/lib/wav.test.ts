import { describe, expect, it } from "vitest";
import { encodeWav16k, mixToMono, WAV_RATE } from "./wav";

const bytes = async (b: Blob) => new Uint8Array(await b.arrayBuffer());

describe("encodeWav16k", () => {
  it("writes the canonical 44-byte header and 16-bit little-endian samples", async () => {
    const wav = encodeWav16k(new Float32Array([0, 1, -1, 0.5, 2, -2, Number.NaN]));
    expect(wav.type).toBe("audio/wav");
    const b = await bytes(wav);
    const v = new DataView(b.buffer);
    const text = (at: number, n: number) => String.fromCharCode(...b.slice(at, at + n));
    expect(b.length).toBe(44 + 7 * 2);
    expect([text(0, 4), text(8, 4), text(12, 4), text(36, 4)]).toEqual(["RIFF", "WAVE", "fmt ", "data"]);
    expect(v.getUint32(4, true)).toBe(36 + 14);
    expect([v.getUint16(20, true), v.getUint16(22, true), v.getUint32(24, true), v.getUint32(28, true)]).toEqual([1, 1, WAV_RATE, WAV_RATE * 2]);
    expect([v.getUint16(32, true), v.getUint16(34, true), v.getUint32(40, true)]).toEqual([2, 16, 14]);
    // 0, full scale both ways, half, clamped, and a NaN that becomes silence
    const samples = Array.from({ length: 7 }, (_, i) => v.getInt16(44 + i * 2, true));
    expect(samples).toEqual([0, 32767, -32768, 16384, 32767, -32768, 0]);
  });

  it("makes an empty file for no samples", async () => {
    expect((await bytes(encodeWav16k(new Float32Array(0)))).length).toBe(44);
  });
});

describe("mixToMono", () => {
  it("averages the channels", () => {
    expect([...mixToMono([new Float32Array([1, 0]), new Float32Array([0, 1])])]).toEqual([0.5, 0.5]);
    const one = new Float32Array([0.25]);
    expect(mixToMono([one])).toBe(one);
    expect(mixToMono([]).length).toBe(0);
  });
});
