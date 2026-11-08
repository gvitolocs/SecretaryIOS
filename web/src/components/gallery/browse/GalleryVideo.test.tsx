import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PlayInfo } from "../../../api/player";
import { setLocale } from "../../../i18n";
import { GalleryVideo } from "./GalleryVideo";

const h = vi.hoisted(() => ({
  play: vi.fn(),
  request: vi.fn(),
  pushProgress: vi.fn(),
  hls: [] as { source?: string; startPosition?: number; destroyed: boolean; handlers: Record<string, (e: unknown, d: unknown) => void> }[],
}));

vi.mock("../../../api/player", async (original) => ({
  ...(await original<typeof import("../../../api/player")>()),
  mediaApi: { play: h.play },
}));
vi.mock("../../../lib/playerCore", () => ({ playerCore: () => Promise.resolve({ request: h.request }) }));
vi.mock("../../../state/media", () => ({ pushProgress: h.pushProgress }));
vi.mock("hls.js", () => ({
  default: class {
    static Events = { ERROR: "hlsError" };
    static isSupported() {
      return true;
    }
    state: (typeof h.hls)[number];
    constructor(config: { startPosition: number }) {
      this.state = { startPosition: config.startPosition, destroyed: false, handlers: {} };
      h.hls.push(this.state);
    }
    loadSource(url: string) {
      this.state.source = url;
    }
    attachMedia() {}
    on(event: string, fn: (e: unknown, d: unknown) => void) {
      this.state.handlers[event] = fn;
    }
    destroy() {
      this.state.destroyed = true;
    }
  },
}));

const info = (over: Partial<PlayInfo> = {}): PlayInfo => ({
  id: "v1",
  kind: "video",
  title: "clip",
  mode: "direct",
  url: "/v1/media/s/tok/v1/file",
  file_url: "/v1/media/s/tok/v1/file",
  hls_url: null,
  poster_url: "/v1/media/items/v1/poster?v=1",
  duration_ms: 60_000,
  subtitles: [],
  bitmap_subtitles: 0,
  expires_at: 0,
  progress: null,
  resume_ms: 0,
  ...over,
});

const video = () => document.querySelector<HTMLVideoElement>("video")!;

beforeEach(() => {
  setLocale("en");
  h.play.mockReset();
  h.request.mockReset();
  h.pushProgress.mockReset();
  h.hls.length = 0;
  h.request.mockImplementation((c: { cmd: string }) => (c.cmd === "resume" ? { ok: true, position_ms: 0 } : { ok: true }));
  // A real element rewinds to zero when it is reloaded: the position must be read before that.
  vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(function (this: HTMLMediaElement) {
    this.currentTime = 0;
  });
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
});

describe("GalleryVideo", () => {
  it("shows the poster and asks the server for nothing while it is not the current item", () => {
    render(() => <GalleryVideo id="v1" poster="/p.jpg" active={false} />);
    expect(video().getAttribute("poster")).toBe("/p.jpg");
    expect(video().hasAttribute("src")).toBe(false);
    expect(h.play).not.toHaveBeenCalled();
    expect(video().controls).toBe(true);
  });

  it("plays a direct file from the tokenised URL, with the server's poster, and does not start by itself", async () => {
    h.play.mockResolvedValue(info());
    const playSpy = vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
    render(() => <GalleryVideo id="v1" poster="/p.jpg" active />);
    await waitFor(() => expect(video().getAttribute("src")).toBe("/v1/media/s/tok/v1/file"));
    expect(h.play).toHaveBeenCalledWith("v1");
    expect(video().getAttribute("poster")).toBe("/v1/media/items/v1/poster?v=1");
    expect(playSpy).not.toHaveBeenCalled();
    expect(h.hls).toHaveLength(0);
  });

  it("resumes at the core's position once the metadata is known", async () => {
    h.play.mockResolvedValue(info({ resume_ms: 5_000, progress: { id: "v1", position_ms: 5_000, duration_ms: 60_000, finished: false, updated_at: 1 } }));
    h.request.mockImplementation((c: { cmd: string }) => (c.cmd === "resume" ? { ok: true, position_ms: 42_500 } : { ok: true }));
    render(() => <GalleryVideo id="v1" active />);
    await waitFor(() => expect(video().getAttribute("src")).toBeTruthy());
    expect(h.request).toHaveBeenCalledWith(expect.objectContaining({ cmd: "merge" }));
    fireEvent(video(), new Event("loadedmetadata"));
    expect(video().currentTime).toBe(42.5);
  });

  it("plays an HLS playlist through hls.js, starting at the resume point", async () => {
    h.play.mockResolvedValue(info({ mode: "transcode", hls_url: "/v1/media/s/tok/v1/index.m3u8" }));
    h.request.mockImplementation((c: { cmd: string }) => (c.cmd === "resume" ? { ok: true, position_ms: 12_000 } : { ok: true }));
    render(() => <GalleryVideo id="v1" active />);
    await waitFor(() => expect(h.hls).toHaveLength(1));
    expect(h.hls[0]).toMatchObject({ source: "/v1/media/s/tok/v1/index.m3u8", startPosition: 12 });
    expect(video().hasAttribute("src")).toBe(false);
  });

  it("tells the player core where it stopped, and the server when the core asks to sync", async () => {
    h.play.mockResolvedValue(info());
    h.request.mockImplementation((c: { cmd: string }) => (c.cmd === "resume" ? { ok: true, position_ms: 0 } : c.cmd === "position" ? { ok: true, sync: true } : { ok: true }));
    render(() => <GalleryVideo id="v1" active />);
    await waitFor(() => expect(video().getAttribute("src")).toBeTruthy());
    video().currentTime = 20;
    fireEvent.pause(video());
    await waitFor(() => expect(h.pushProgress).toHaveBeenCalled());
    expect(h.request).toHaveBeenCalledWith({ cmd: "position", id: "v1", position_ms: 20_000, duration_ms: 60_000, playing: false, device: "web" });
  });

  it("lets go when it stops being the current item, and reports the position on the way out", async () => {
    h.play.mockResolvedValue(info({ mode: "remux", hls_url: "/v1/media/s/tok/v1/index.m3u8" }));
    const [active, setActive] = createSignal(true);
    render(() => <GalleryVideo id="v1" active={active()} />);
    await waitFor(() => expect(h.hls).toHaveLength(1));
    video().currentTime = 7;
    setActive(false);
    await waitFor(() => expect(h.hls[0]!.destroyed).toBe(true));
    expect(video().hasAttribute("src")).toBe(false);
    await waitFor(() => expect(h.request).toHaveBeenCalledWith(expect.objectContaining({ cmd: "position", id: "v1", position_ms: 7_000, playing: false })));
  });

  it("says so and retries when the server cannot start the video", async () => {
    h.play.mockRejectedValueOnce(new Error("boom")).mockResolvedValueOnce(info());
    render(() => <GalleryVideo id="v1" active />);
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(video().getAttribute("src")).toBe("/v1/media/s/tok/v1/file"));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("refuses a playlist nothing in this browser can play", async () => {
    h.play.mockResolvedValue(info({ mode: "transcode", hls_url: null, url: "" }));
    render(() => <GalleryVideo id="v1" active />);
    expect(await screen.findByRole("alert")).toHaveTextContent("This browser can't play this video.");
  });
});
