import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { setImageCoreForTests } from "../lib/imageCore";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

// Videos in the shared viewer: the default <video src>, and `renderVideo` (the Gallery's own player) with swipes.
const W = 800;
const H = 600;
const restore: (() => void)[] = [];
beforeAll(() => {
  setImageCoreForTests(null);
  const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, width: W, height: H, right: W, bottom: H, toJSON: () => ({}),
  } as DOMRect);
  restore.push(() => rect.mockRestore());
});
afterAll(() => {
  for (const undo of restore) undo();
});

const items: ViewerItem[] = [
  { src: "/p/a.jpg", name: "a.jpg", id: "a" },
  { src: "/p/clip.jpg", name: "clip.mov", kind: "video", id: "clip", poster: "/p/clip.jpg" },
  { src: "/p/c.jpg", name: "c.jpg", id: "c" },
];

const stage = () => document.querySelector<HTMLElement>(".pv-stage")!;
const pointer = (target: Element, type: "pointerDown" | "pointerMove" | "pointerUp", x: number, y: number) =>
  fireEvent[type](target, { pointerId: 1, clientX: x, clientY: y });

function open(start: number, renderVideo?: Parameters<typeof PhotoViewer>[0]["renderVideo"]) {
  const [index, setIndex] = createSignal(start);
  render(() => <PhotoViewer items={items} index={index()} onIndex={setIndex} onClose={() => {}} renderVideo={renderVideo} />);
  return index;
}

describe("PhotoViewer videos", () => {
  it("plays a plain <video src> by default", () => {
    open(1);
    const video = document.querySelector<HTMLVideoElement>(".pv-slot video")!;
    expect(video.getAttribute("src")).toBe("/p/clip.jpg");
    expect(video.controls).toBe(true);
  });

  it("hands a video to renderVideo, marking only the current one", () => {
    const seen: { id?: string; index: number; current: boolean }[] = [];
    // Middle item is the video; its neighbours are pictures, so open on a picture next to it to see it as a neighbour.
    open(0, ({ item, index, current }) => {
      seen.push({ id: item.id, index, current });
      return <div data-testid="custom" data-current={String(current)} />;
    });
    expect(seen).toEqual([{ id: "clip", index: 1, current: false }]);
    expect(document.querySelector("[data-testid=custom]")?.getAttribute("data-current")).toBe("false");
    expect(document.querySelector(".pv-slot video")).toBeNull();
  });

  it("marks the video current when it is the open item", () => {
    const flags: boolean[] = [];
    open(1, ({ current }) => {
      flags.push(current);
      return <video class="pv-video" />;
    });
    expect(flags).toContain(true);
    expect(flags.filter(Boolean)).toHaveLength(1);
  });

  it("pages when a drag starts on the video itself, outside its control strip", async () => {
    const index = open(1, () => <video class="pv-video" />);
    const video = document.querySelector<HTMLVideoElement>(".pv-slot video")!;
    pointer(video, "pointerDown", 600, 300);
    pointer(video, "pointerMove", 450, 302);
    pointer(video, "pointerMove", 250, 305);
    pointer(video, "pointerUp", 250, 305);
    await waitFor(() => expect(index()).toBe(2));
  });

  it("leaves the control strip to the video", async () => {
    const index = open(1, () => <video class="pv-video" />);
    const video = document.querySelector<HTMLVideoElement>(".pv-slot video")!;
    pointer(video, "pointerDown", 600, 580);
    pointer(video, "pointerMove", 450, 580);
    pointer(video, "pointerMove", 250, 580);
    pointer(video, "pointerUp", 250, 580);
    await new Promise((r) => setTimeout(r, 300));
    expect(index()).toBe(1);
  });

  it("does not toggle the controls on a tap on a custom video (the video plays or pauses itself)", async () => {
    open(1, () => <video class="pv-video" />);
    const video = document.querySelector<HTMLVideoElement>(".pv-slot video")!;
    const dialog = document.querySelector(".pv")!;
    expect(dialog).toHaveClass("pv--chrome");
    pointer(video, "pointerDown", 400, 300);
    pointer(video, "pointerUp", 400, 300);
    await new Promise((r) => setTimeout(r, 400));
    expect(dialog).toHaveClass("pv--chrome");
  });

  it("keeps ignoring drags on the built-in video, as before", async () => {
    const index = open(1);
    const video = document.querySelector<HTMLVideoElement>(".pv-slot video")!;
    pointer(video, "pointerDown", 600, 300);
    pointer(video, "pointerMove", 250, 302);
    pointer(video, "pointerUp", 250, 302);
    await new Promise((r) => setTimeout(r, 300));
    expect(index()).toBe(1);
    // Dragging the stage around it still pages.
    pointer(stage(), "pointerDown", 600, 300);
    pointer(stage(), "pointerMove", 450, 302);
    pointer(stage(), "pointerMove", 250, 305);
    pointer(stage(), "pointerUp", 250, 305);
    await waitFor(() => expect(index()).toBe(2));
  });
});
