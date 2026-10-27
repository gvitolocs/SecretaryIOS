/// <reference types="node" />
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createSignal } from "solid-js";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { imageCoreFromBytes, setImageCoreForTests } from "../lib/imageCore";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

// The viewer with the real Rust core (image-core.wasm) in an 800×600 window.
const W = 800;
const H = 600;
const items: ViewerItem[] = ["a", "b", "c"].map((id) => ({ src: `/p/${id}.jpg`, name: `${id}.jpg` }));

const restore: (() => void)[] = [];
beforeAll(async () => {
  setImageCoreForTests(await imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm"))));
  const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, width: W, height: H, right: W, bottom: H, toJSON: () => ({}),
  } as DOMRect);
  restore.push(() => rect.mockRestore());
  for (const [key, value] of [["naturalWidth", 1600], ["naturalHeight", 1200]] as const) {
    Object.defineProperty(HTMLImageElement.prototype, key, { configurable: true, get: () => value });
    restore.push(() => delete (HTMLImageElement.prototype as unknown as Record<string, unknown>)[key]);
  }
});
afterAll(() => {
  for (const undo of restore) undo();
  setImageCoreForTests(null);
});

function open(props: { actions?: Parameters<typeof PhotoViewer>[0]["actions"]; start?: number } = {}) {
  const [index, setIndex] = createSignal(props.start ?? 0);
  const onClose = vi.fn();
  render(() => <PhotoViewer items={items} index={index()} onIndex={setIndex} onClose={onClose} actions={props.actions} />);
  return { index, onClose };
}

/** The current picture, placed by the core once it has loaded. */
async function placed(): Promise<HTMLImageElement> {
  const img = screen.getByRole("dialog").querySelector<HTMLImageElement>(".pv-img")!;
  fireEvent.load(img);
  await waitFor(() => expect(img.classList.contains("pv-img--placed")).toBe(true));
  return img;
}

const stage = () => document.querySelector<HTMLElement>(".pv-stage")!;
const pointer = (type: "pointerDown" | "pointerMove" | "pointerUp", x: number, y: number) =>
  fireEvent[type](stage(), { pointerId: 1, clientX: x, clientY: y });

describe("PhotoViewer", () => {
  beforeEach(() => vi.useRealTimers());

  it("fits the picture with the core and zooms on a double tap and a trackpad pinch", async () => {
    open();
    expect(screen.getByRole("dialog", { name: "Photo 1 of 3" })).toBeInTheDocument();
    const img = await placed();
    // 1600×1200 fitted into 800×600.
    expect([img.style.width, img.style.height, img.style.left, img.style.top]).toEqual(["800px", "600px", "0px", "0px"]);

    for (let i = 0; i < 2; i++) {
      pointer("pointerDown", 400, 300);
      pointer("pointerUp", 400, 300);
    }
    await waitFor(() => expect(img.style.width).toBe("2000px"));

    // Back to fitted, then a ctrl+wheel (how Chromium reports a trackpad pinch).
    fireEvent.keyDown(window, { key: "0" });
    await waitFor(() => expect(img.style.width).toBe("800px"));
    fireEvent.wheel(stage(), { ctrlKey: true, deltaY: -50, clientX: 400, clientY: 300 });
    expect(parseFloat(img.style.width)).toBeGreaterThan(800);
  });

  it("pages with a horizontal drag and the arrow keys, and stops at the folder's ends", async () => {
    const { index } = open();
    await placed();
    pointer("pointerDown", 600, 300);
    pointer("pointerMove", 500, 302);
    pointer("pointerMove", 300, 305);
    pointer("pointerUp", 300, 305);
    await waitFor(() => expect(index()).toBe(1));
    expect(screen.getByRole("dialog", { name: "Photo 2 of 3" }).querySelector("img")).toHaveAttribute("src", "/p/b.jpg");

    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(index()).toBe(2);
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(index()).toBe(2);
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
  });

  it("closes on a swipe down, and springs back from a short one", async () => {
    const { onClose } = open();
    await placed();
    pointer("pointerDown", 400, 200);
    pointer("pointerMove", 402, 230);
    pointer("pointerMove", 404, 260);
    pointer("pointerUp", 404, 262);
    expect(onClose).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByRole("dialog").classList.contains("pv--animating")).toBe(false));

    pointer("pointerDown", 400, 200);
    pointer("pointerMove", 402, 260);
    pointer("pointerMove", 404, 330);
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).not.toBe("1");
    pointer("pointerUp", 404, 340);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("saves to Lucina with progress and says when it's already there", async () => {
    let finish: (v: { duplicate: boolean }) => void = () => {};
    const saveToLucina = vi.fn((_item: ViewerItem, progress: (sent: number, total: number) => void) => {
      progress(5, 10);
      return new Promise<{ duplicate: boolean }>((resolve) => { finish = resolve; });
    });
    open({ actions: { saveToLucina } });
    fireEvent.click(screen.getByRole("button", { name: "Save to Lucina" }));
    expect(await screen.findByRole("button", { name: "Saving… 50%" })).toBeDisabled();
    expect(saveToLucina.mock.calls[0]![0].name).toBe("a.jpg");
    finish({ duplicate: true });
    expect(await screen.findByRole("button", { name: "Already in Lucina" })).toBeInTheDocument();
    // Another picture starts fresh.
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(screen.getByRole("button", { name: "Save to Lucina" })).toBeEnabled();
  });

  it("closes on Escape and hides the controls on a tap", async () => {
    const { onClose } = open();
    await placed();
    pointer("pointerDown", 100, 100);
    pointer("pointerUp", 100, 100);
    await waitFor(() => expect(screen.getByRole("dialog").classList.contains("pv--chrome")).toBe(false));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });
});
