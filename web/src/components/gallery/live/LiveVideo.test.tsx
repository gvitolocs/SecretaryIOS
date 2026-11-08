import { cleanup, render } from "@solidjs/testing-library";
import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PlayInfo } from "../../../api/player";
import { FADE_MS } from "../../../lib/galleryLive";
import { createLive, type LiveController } from "./liveController";
import { LiveVideo } from "./LiveVideo";

const h = vi.hoisted(() => ({ play: vi.fn() }));
vi.mock("../../../api/player", async (original) => ({
  ...(await original<typeof import("../../../api/player")>()),
  mediaApi: { play: h.play },
}));

const plan = (over: Partial<PlayInfo> = {}): PlayInfo => ({
  id: "m1",
  kind: "video",
  title: "clip",
  mode: "direct",
  url: "/v1/media/s/tok/m1/file",
  file_url: "/v1/media/s/tok/m1/file",
  hls_url: null,
  poster_url: null,
  duration_ms: 1500,
  subtitles: [],
  bitmap_subtitles: 0,
  expires_at: 0,
  progress: null,
  resume_ms: 0,
  ...over,
});

let live: LiveController;
let dispose: () => void;
const playSpy = vi.fn();
const pauseSpy = vi.fn();
const flush = () => new Promise((r) => setTimeout(r, 0));
const video = () => document.querySelector<HTMLVideoElement>("video.gl-video")!;

function mount(style: () => Record<string, string> | undefined = () => undefined, placed = () => false) {
  createRoot((d) => {
    dispose = d;
    live = createLive(() => false);
  });
  return render(() => <LiveVideo pairId="m1" live={live} style={style} placed={placed} />);
}

beforeEach(() => {
  h.play.mockReset();
  h.play.mockResolvedValue(plan());
  playSpy.mockReset();
  playSpy.mockResolvedValue(undefined);
  pauseSpy.mockReset();
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(playSpy);
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(pauseSpy);
  vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => {});
});
afterEach(() => {
  cleanup();
  dispose?.();
  vi.restoreAllMocks();
});

describe("LiveVideo", () => {
  it("is muted, takes no pointer, hidden, and asks for nothing until it is pressed", async () => {
    mount();
    await flush();
    expect(video().muted).toBe(true);
    expect(video()).toHaveClass("pv-img");
    expect(video()).not.toHaveClass("gl-video--on");
    expect(h.play).not.toHaveBeenCalled();
    expect(playSpy).not.toHaveBeenCalled();
  });

  it("lies in the still's box: it gets the picture's style and placed class", () => {
    mount(() => ({ left: "10px", top: "20px", width: "300px", height: "200px" }), () => true);
    expect(video().style.left).toBe("10px");
    expect(video().style.width).toBe("300px");
    expect(video()).toHaveClass("pv-img--placed");
  });

  it("plays the pair's motion once from the start when pressed, fades in, and fades back out after it ends", async () => {
    mount();
    live.press("button");
    await vi.waitFor(() => expect(live.state().phase).toBe("playing"));
    expect(h.play).toHaveBeenCalledWith("m1");
    expect(video().getAttribute("src")).toBe("/v1/media/s/tok/m1/file");
    expect(playSpy).toHaveBeenCalledTimes(1);
    expect(video().currentTime).toBe(0);
    expect(video()).toHaveClass("gl-video--on");

    video().dispatchEvent(new Event("ended"));
    expect(live.state().phase).toBe("fading");
    expect(video()).not.toHaveClass("gl-video--on");
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"), { timeout: FADE_MS + 500 });
    expect(pauseSpy).toHaveBeenCalled();
  });

  it("plays again without asking for the plan a second time", async () => {
    mount();
    live.press("button");
    await vi.waitFor(() => expect(live.state().phase).toBe("playing"));
    live.dispatch({ type: "ended" });
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"), { timeout: FADE_MS + 500 });
    live.press("button");
    await vi.waitFor(() => expect(live.state().phase).toBe("playing"));
    expect(h.play).toHaveBeenCalledTimes(1);
    expect(playSpy).toHaveBeenCalledTimes(2);
  });

  it("stops at once when a hold is let go before the clip is ready", async () => {
    let finish!: (p: PlayInfo) => void;
    h.play.mockReturnValue(new Promise<PlayInfo>((r) => (finish = r)));
    mount();
    live.press("hold");
    live.release("hold");
    expect(live.state().phase).toBe("idle");
    finish(plan());
    await flush();
    expect(playSpy).not.toHaveBeenCalled();
    expect(live.state().phase).toBe("idle");
  });

  it("drops back to the still when the plan can't be had, or the clip can't be played", async () => {
    h.play.mockRejectedValueOnce(new Error("offline"));
    mount();
    live.press("button");
    await vi.waitFor(() => expect(h.play).toHaveBeenCalled());
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"));
    expect(video()).not.toHaveClass("gl-video--on");

    live.press("button");
    await vi.waitFor(() => expect(live.state().phase).toBe("playing"));
    live.dispatch({ type: "ended" });
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"), { timeout: FADE_MS + 500 });
    playSpy.mockRejectedValueOnce(new Error("blocked"));
    live.press("button");
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"));
  });

  it("drops back to the still for a plan it can't play", async () => {
    h.play.mockResolvedValue(plan({ mode: "transcode", url: undefined, hls_url: null }));
    mount();
    live.press("button");
    await vi.waitFor(() => expect(h.play).toHaveBeenCalled());
    await vi.waitFor(() => expect(live.state().phase).toBe("idle"));
    expect(playSpy).not.toHaveBeenCalled();
  });
});
