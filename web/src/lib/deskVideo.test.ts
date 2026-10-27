import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DeskVideo } from "./deskVideo";
import { encodeVideo, type DeskCodec } from "./deskWire";

// A WebCodecs decoder that records its configuration and can refuse a codec string, like a browser
// whose decoder does not take that H.264 profile (reported through `error`, not by `configure`).
let decoders: FakeDecoder[] = [];
let refuse = new Set<string>();
class FakeDecoder {
  state = "unconfigured";
  decodeQueueSize = 0;
  codec = "";
  decoded = 0;
  constructor(private readonly init: { output: (f: unknown) => void; error: (e: unknown) => void }) {
    decoders.push(this);
  }
  configure(c: { codec: string }) {
    this.codec = c.codec;
    this.state = "configured";
  }
  decode(chunk: { timestamp: number }) {
    if (refuse.has(this.codec)) {
      this.init.error(Object.assign(new Error("no"), { name: "NotSupportedError" }));
      return;
    }
    this.decoded++;
    this.init.output({ timestamp: chunk.timestamp, displayWidth: 1280, displayHeight: 800, close() {} });
  }
  close() {
    this.state = "closed";
  }
}

const frame = (codec: DeskCodec, key: boolean, pts: number) => encodeVideo({ codec, key, last: true, display: 0, pts, data: new Uint8Array([1]) });

function setup() {
  const sent: unknown[] = [];
  const out: number[] = [];
  const unsupported: string[] = [];
  const v = new DeskVideo({ send: (m) => sent.push(m), output: (f) => out.push(f.timestamp), unsupported: (c) => unsupported.push(c) });
  return { v, sent, out, unsupported };
}

beforeEach(() => {
  decoders = [];
  refuse = new Set();
  vi.stubGlobal("VideoDecoder", FakeDecoder);
  vi.stubGlobal("EncodedVideoChunk", class { timestamp: number; constructor(i: { timestamp: number }) { this.timestamp = i.timestamp; } });
});
afterEach(() => vi.unstubAllGlobals());

describe("DeskVideo", () => {
  it("switches decoder when the codec byte changes, starting again at a key frame", () => {
    const { v, out } = setup();
    v.push(frame("vp9", true, 1));
    v.push(frame("vp9", false, 2));
    expect(decoders.map((d) => d.codec)).toEqual(["vp09.00.41.08"]);
    // The host moves to H.264: a delta first is skipped, the key frame starts the new decoder.
    v.push(frame("h264", false, 3));
    v.push(frame("h264", true, 4));
    v.push(frame("h264", false, 5));
    expect(decoders.map((d) => d.codec)).toEqual(["vp09.00.41.08", "avc1.640033"]);
    expect(decoders[0]!.state).toBe("closed");
    expect(out).toEqual([1000, 2000, 4000, 5000]);
  });

  it("H.264: falls back from High to Main to Baseline when the decoder refuses, asking for a key frame each time", () => {
    const { v, sent, out, unsupported } = setup();
    refuse = new Set(["avc1.640033", "avc1.4D0033"]);
    v.push(frame("h264", true, 1));
    v.push(frame("h264", true, 2));
    v.push(frame("h264", true, 3));
    expect(decoders.map((d) => d.codec)).toEqual(["avc1.640033", "avc1.4D0033", "avc1.42E033"]);
    expect(sent.filter((m) => (m as { type: string }).type === "refresh")).toHaveLength(2);
    expect(out).toEqual([3000]);
    expect(unsupported).toEqual([]);
  });

  it("reports a codec it cannot decode at all", () => {
    const { v, unsupported } = setup();
    refuse = new Set(["vp09.00.41.08"]);
    v.push(frame("vp9", true, 1));
    expect(unsupported).toEqual(["vp9"]);
  });

  it("never asks for a key frame while waiting for one; acks keep the host going", () => {
    const { v, sent } = setup();
    v.push(frame("vp9", false, 1));
    v.push(frame("vp9", false, 2));
    expect(sent).toEqual([{ type: "ack" }, { type: "ack" }]);
  });
});
