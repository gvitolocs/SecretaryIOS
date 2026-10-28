import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
import type { PlayInfo } from "../../api/player";
import { setLocale } from "../../i18n";
import { setImageCoreForTests } from "../../lib/imageCore";
import { details, galleryItem } from "../../test/galleryFixtures";
import { GalleryViewer } from "./GalleryViewer";
import { resetPhotoInfoCaches } from "./PhotoInfo";

const h = vi.hoisted(() => ({ play: vi.fn() }));
vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  getDetails: vi.fn(),
  patchItem: vi.fn(),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
}));
vi.mock("../../api/player", async (original) => ({
  ...(await original<typeof import("../../api/player")>()),
  mediaApi: { play: h.play },
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const { getDetails } = vi.mocked(api);
const playSpy = vi.fn();
const pauseSpy = vi.fn();

const plan = (): PlayInfo => ({
  id: "x",
  kind: "video",
  title: "clip",
  mode: "direct",
  url: "/v1/media/s/tok/x/file",
  file_url: "/v1/media/s/tok/x/file",
  hls_url: null,
  poster_url: null,
  duration_ms: 1500,
  subtitles: [],
  bitmap_subtitles: 0,
  expires_at: 0,
  progress: null,
  resume_ms: 0,
});

beforeAll(() => setImageCoreForTests(null));

function open(items: GalleryItem[], extra: { onSimilar?: (i: GalleryItem) => void } = {}) {
  const [index, setIndex] = createSignal(0);
  render(() => <GalleryViewer items={items} index={index()} onIndex={setIndex} onClose={() => {}} onChanged={() => {}} onRemoved={() => {}} onSimilar={extra.onSimilar} />);
  return { index, setIndex };
}

const live = (id: string, over: Partial<GalleryItem> = {}) => galleryItem(id, { livePair: `${id}_mov`, ...over });
const toolbar = () => screen.getByRole("toolbar", { name: "Photo actions" });
const liveButton = () => within(toolbar()).queryByRole("button", { name: "Play Live Photo" });
const motion = () => document.querySelector<HTMLVideoElement>(".pv-slot video.gl-video");
const key = (type: "keyDown" | "keyUp", k: string, init: KeyboardEventInit = {}) => fireEvent[type](window, { key: k, ...init });

function pointer(type: "pointerenter" | "pointerleave", el: Element, pointerType: string) {
  const e = new Event(type, { bubbles: false });
  Object.assign(e, { pointerType });
  el.dispatchEvent(e);
}

function reduceMotion(on: boolean) {
  vi.stubGlobal("matchMedia", (q: string) => ({ matches: on && q.includes("prefers-reduced-motion"), media: q, addEventListener() {}, removeEventListener() {} }));
}

beforeEach(() => {
  setLocale("en");
  resetPhotoInfoCaches();
  getDetails.mockReset();
  getDetails.mockImplementation(async (id) => details(galleryItem(id)));
  vi.mocked(api.listAlbums).mockResolvedValue([]);
  vi.mocked(api.listSources).mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
  h.play.mockReset();
  h.play.mockResolvedValue(plan());
  playSpy.mockReset();
  playSpy.mockResolvedValue(undefined);
  pauseSpy.mockReset();
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(playSpy);
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(pauseSpy);
  vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => {});
  reduceMotion(false);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("Live Photos in the viewer", () => {
  it("shows the LIVE button for a Live Photo only, and does not play on open", async () => {
    open([live("a"), galleryItem("b"), galleryItem("v", { kind: "video", livePair: "v_mov" })]);
    expect(liveButton()).toBeInTheDocument();
    expect(liveButton()).toHaveAttribute("title", expect.stringContaining("hold L"));
    expect(motion()).not.toBeNull();
    await new Promise((r) => setTimeout(r, 30));
    expect(h.play).not.toHaveBeenCalled();
    expect(playSpy).not.toHaveBeenCalled();
  });

  it("has no LIVE button on an ordinary photo, and no motion layer", () => {
    open([galleryItem("b"), live("a")]);
    expect(liveButton()).toBeNull();
    expect(motion()).toBeNull();
  });

  it("plays the motion half (not the still's id) when LIVE is pressed, muted, over the still, and fades back", async () => {
    open([live("a")]);
    fireEvent.click(liveButton()!);
    await waitFor(() => expect(liveButton()).toHaveAttribute("aria-pressed", "true"));
    expect(h.play).toHaveBeenCalledWith("a_mov");
    const layer = motion()!;
    expect(layer.muted).toBe(true);
    expect(layer).toHaveClass("gl-video--on");
    // It sits right after the still in the same page.
    expect(layer.previousElementSibling).toBe(document.querySelector(".pv-slot img.pv-img"));
    layer.dispatchEvent(new Event("ended"));
    expect(layer).not.toHaveClass("gl-video--on");
    await waitFor(() => expect(liveButton()).toHaveAttribute("aria-pressed", "false"));
  });

  it("plays while L is held and stops when it is let go; a key repeat doesn't restart it", async () => {
    open([live("a")]);
    key("keyDown", "l");
    await waitFor(() => expect(motion()).toHaveClass("gl-video--on"));
    key("keyDown", "l", { repeat: true });
    expect(playSpy).toHaveBeenCalledTimes(1);
    key("keyUp", "l");
    await waitFor(() => expect(motion()).not.toHaveClass("gl-video--on"));
  });

  it("L does nothing on an ordinary photo", async () => {
    open([galleryItem("b")]);
    key("keyDown", "l");
    await new Promise((r) => setTimeout(r, 30));
    expect(h.play).not.toHaveBeenCalled();
  });

  it("plays while the mouse is over the button, and stops when it leaves", async () => {
    open([live("a")]);
    pointer("pointerenter", liveButton()!, "mouse");
    await waitFor(() => expect(motion()).toHaveClass("gl-video--on"));
    pointer("pointerleave", liveButton()!, "mouse");
    await waitFor(() => expect(motion()).not.toHaveClass("gl-video--on"));
  });

  it("a touch passing over the button is not a hover", async () => {
    open([live("a")]);
    pointer("pointerenter", liveButton()!, "touch");
    await new Promise((r) => setTimeout(r, 30));
    expect(h.play).not.toHaveBeenCalled();
  });

  it("with reduced motion a hover plays nothing, but pressing the button does", async () => {
    reduceMotion(true);
    open([live("a")]);
    pointer("pointerenter", liveButton()!, "mouse");
    await new Promise((r) => setTimeout(r, 30));
    expect(h.play).not.toHaveBeenCalled();
    fireEvent.click(liveButton()!);
    await waitFor(() => expect(motion()).toHaveClass("gl-video--on"));
  });

  it("goes back to the still when another picture is shown", async () => {
    const { setIndex } = open([live("a"), live("b")]);
    fireEvent.click(liveButton()!);
    await waitFor(() => expect(motion()).toHaveClass("gl-video--on"));
    setIndex(1);
    await waitFor(() => expect(liveButton()).toHaveAttribute("aria-pressed", "false"));
    expect(motion()).not.toHaveClass("gl-video--on");
    expect(document.querySelectorAll(".pv-slot video.gl-video")).toHaveLength(1);
  });
});

describe("Similar photos button", () => {
  it("is in the toolbar only when the Gallery can search by meaning, and hands over the photo", () => {
    const onSimilar = vi.fn();
    open([galleryItem("a")], { onSimilar });
    fireEvent.click(within(toolbar()).getByRole("button", { name: "Similar photos" }));
    expect(onSimilar).toHaveBeenCalledWith(expect.objectContaining({ id: "a" }));
    cleanup();
    open([galleryItem("a")]);
    expect(within(toolbar()).queryByRole("button", { name: "Similar photos" })).toBeNull();
  });

  it("is not offered for a video", () => {
    open([galleryItem("v", { kind: "video" })], { onSimilar: vi.fn() });
    expect(within(toolbar()).queryByRole("button", { name: "Similar photos" })).toBeNull();
  });
});
