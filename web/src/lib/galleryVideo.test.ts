import { describe, expect, it } from "vitest";
import { dueReport, REPORT_EVERY_S, resumeSeconds, videoSource } from "./galleryVideo";

const play = (over: Partial<{ mode: "direct" | "remux" | "transcode"; url: string; hls_url: string | null }> = {}) => ({
  mode: "direct" as const,
  url: "/v1/media/s/tok/vid/file",
  hls_url: null,
  ...over,
});

describe("videoSource", () => {
  it("plays a direct file with the element itself", () => {
    expect(videoSource(play(), true, false)).toEqual({ type: "file", url: "/v1/media/s/tok/vid/file" });
    // Even when the server also offers a playlist: direct means the file.
    expect(videoSource(play({ hls_url: "/v1/media/s/tok/vid/index.m3u8" }), true, true)).toEqual({ type: "file", url: "/v1/media/s/tok/vid/file" });
  });

  it("uses hls.js for a remux or transcode wherever it runs, else the browser's own HLS", () => {
    const hls = play({ mode: "transcode", hls_url: "/v1/media/s/tok/vid/index.m3u8" });
    expect(videoSource(hls, true, true)).toEqual({ type: "hls", url: "/v1/media/s/tok/vid/index.m3u8" });
    expect(videoSource(hls, false, true)).toEqual({ type: "native-hls", url: "/v1/media/s/tok/vid/index.m3u8" });
    expect(videoSource(hls, false, false)).toEqual({ type: "unsupported" });
  });

  it("falls back to the file when a remux has no playlist, and gives up when there is nothing", () => {
    expect(videoSource(play({ mode: "remux" }), true, false)).toEqual({ type: "file", url: "/v1/media/s/tok/vid/file" });
    expect(videoSource(play({ mode: "remux", url: "" }), true, false)).toEqual({ type: "unsupported" });
  });
});

describe("dueReport", () => {
  it("reports every few seconds forward and at once after jumping back", () => {
    expect(dueReport(2, 0)).toBe(false);
    expect(dueReport(REPORT_EVERY_S, 0)).toBe(true);
    expect(dueReport(100, 96)).toBe(false);
    expect(dueReport(30, 90)).toBe(true);
  });
});

describe("resumeSeconds", () => {
  it("prefers the core's saved position, then the server's, and never goes negative", () => {
    expect(resumeSeconds(42_000, 10_000)).toBe(42);
    expect(resumeSeconds(undefined, 10_500)).toBe(10.5);
    expect(resumeSeconds("x", 8_000)).toBe(8);
    expect(resumeSeconds(0, 5_000)).toBe(0);
    expect(resumeSeconds(-5, 5_000)).toBe(0);
    expect(resumeSeconds(Number.NaN, Number.NaN)).toBe(0);
  });
});
