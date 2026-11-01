import { describe, expect, it } from "vitest";
import { parseItem, parseVoiceMeta } from "./timeline";

describe("voice thread items", () => {
  it("keep the voice channel and parse meta.voice", () => {
    const item = parseItem({
      id: "tl_1",
      ts: 1_761_048_000_000,
      channel: "voice",
      role: "lucina",
      text: "Sono le dieci.",
      meta: {
        device: "dev_x",
        deviceName: "Kitchen speaker",
        voice: {
          turnId: "voice-1-ab",
          ms: { stt: 80, lucina_first: 300, tts_first: 600, first_audio: 650, total: 2000, route: 250, junk: 5 },
          route: { intent: "temporal_query", fast: true, source: "jev", confidence: 0.93, jev_ms: 212, layers: [{ layer: "intent", verdict: "temporal_query", confidence: 0.93, ms: 212, used: true }] },
        },
      },
    });
    expect(item?.channel).toBe("voice");
    expect(item?.meta?.voice).toEqual({
      turnId: "voice-1-ab",
      ms: { stt: 80, lucina_first: 300, tts_first: 600, first_audio: 650, total: 2000, route: 250 },
      route: { intent: "temporal_query", fast: true, source: "jev", confidence: 0.93, jev_ms: 212 },
      layers: [{ layer: "intent", verdict: "temporal_query", confidence: 0.93, ms: 212, used: true }],
    });
  });

  it("reads the route the way Lucina stores it (jevMs) and the speech-to-text time in ms.stt", () => {
    const m = parseVoiceMeta({
      turnId: "voice-1-ab",
      ms: { stt: 84 },
      route: { intent: "chat", source: "jev", fast: false, confidence: 0.88, jevMs: 214, layers: [{ layer: "intent", verdict: "chat", confidence: 0.88, ms: 214, used: true }, { layer: "vocab", verdict: "ok", ms: 120, used: true }] },
    });
    expect(m).toEqual({
      turnId: "voice-1-ab",
      ms: { stt: 84 },
      route: { intent: "chat", source: "jev", fast: false, confidence: 0.88, jev_ms: 214 },
      layers: [{ layer: "intent", verdict: "chat", confidence: 0.88, ms: 214, used: true }, { layer: "vocab", verdict: "ok", ms: 120, used: true }],
    });
  });

  it("prefers voice.layers (the finished turn) over route.layers and cleans them", () => {
    const m = parseVoiceMeta({
      sttMs: 90,
      ambiguous: 2,
      corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }, { from: "", to: "x" }],
      route: { layers: [{ layer: "intent" }] },
      layers: [{ layer: "vocab", verdict: true, confidence: 7, ms: -1, used: false }, { layer: "bad layer!" }, { verdict: "x" }, 3],
    });
    expect(m).toEqual({ sttMs: 90, ambiguous: 2, corrections: [{ from: "skibi gard", to: "Skibigårdsvej" }], layers: [{ layer: "vocab", verdict: "true", used: false }] });
  });

  it("gives nothing for junk", () => {
    expect(parseVoiceMeta(null)).toBeUndefined();
    expect(parseVoiceMeta("x")).toBeUndefined();
    expect(parseVoiceMeta({ ms: "slow", route: [], layers: "none" })).toBeUndefined();
    expect(parseItem({ id: "1", ts: 1, channel: "voice", role: "user", text: "x", meta: { voice: 3 } })?.meta).toBeUndefined();
  });
});
