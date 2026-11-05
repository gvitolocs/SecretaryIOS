import { describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { formatBytes, formatMs, formatUptime, rssiQuality, turnBar } from "./voice";

describe("voice formatting", () => {
  it("formats bytes in human units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(812_000)).toBe("793 KB");
    expect(formatBytes(4_194_304)).toBe("4 MB");
    expect(formatBytes(2_650_000)).toBe("2.5 MB");
    expect(formatBytes(12.4 * 1024 * 1024)).toBe("12 MB");
    expect(formatBytes(3 * 1024 ** 3)).toBe("3 GB");
    // Rounding never prints 1024 of a unit.
    expect(formatBytes(1024 * 1024 - 10)).toBe("1 MB");
    expect(formatBytes(-5)).toBe("0 B");
  });

  it("follows the language for the decimal mark", () => {
    setLocale("it");
    expect(formatBytes(1536)).toBe("1,5 KB");
    expect(formatMs(12_340)).toBe("12,3 s");
  });

  it("formats milliseconds and uptimes", () => {
    expect(formatMs(420)).toBe("420 ms");
    expect(formatMs(1300)).toBe("1,300 ms");
    expect(formatMs(12_340)).toBe("12.3 s");
    expect(formatUptime(42_000)).toBe("42s");
    expect(formatUptime(725_000)).toBe("12m 05s");
    expect(formatUptime(11_100_000)).toBe("3h 05m");
    expect(formatUptime(200_000_000)).toBe("2d 7h");
  });

  it("labels the Wi-Fi signal", () => {
    expect([-45, -60, -61, -70, -75, -80, -90].map(rssiQuality)).toEqual(["excellent", "excellent", "good", "good", "fair", "fair", "weak"]);
  });
});

describe("turnBar", () => {
  it("turns the moments from the end of speech into stages, plus the rest of the reply", () => {
    const bar = turnBar({ stt: 380, lucina_first: 760, tts_first: 1010, first_audio: 1060, total: 3400 });
    expect(bar.segments).toEqual([
      { key: "stt", from: 0, ms: 380 },
      { key: "lucina", from: 380, ms: 380 },
      { key: "tts", from: 760, ms: 250 },
      { key: "audio", from: 1010, ms: 50 },
      { key: "rest", from: 1060, ms: 2340 },
    ]);
    expect(bar.end).toBe(3400);
  });

  it("skips moments that did not happen and never goes backwards", () => {
    expect(turnBar({ stt: 690, total: 8100 }).segments.map((s) => [s.key, s.ms])).toEqual([["stt", 690], ["rest", 7410]]);
    expect(turnBar({ stt: 500, lucina_first: 400, total: 450 }).segments.map((s) => [s.key, s.ms])).toEqual([["stt", 500], ["lucina", 0]]);
    expect(turnBar(undefined)).toEqual({ segments: [], end: 0 });
    expect(turnBar({ stt: null, total: null }).segments).toEqual([]);
  });
});
