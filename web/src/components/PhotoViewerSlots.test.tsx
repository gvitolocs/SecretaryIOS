/// <reference types="node" />
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createSignal } from "solid-js";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { imageCoreFromBytes, setImageCoreForTests } from "../lib/imageCore";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

// The viewer's extension points (toolbar, panel, key hook, rotation) with the real Rust core in an 800×600 stage.
const W = 800;
const H = 600;
const restore: (() => void)[] = [];
beforeAll(async () => {
  setImageCoreForTests(await imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm"))));
  const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, width: W, height: H, right: W, bottom: H, toJSON: () => ({}),
  } as DOMRect);
  restore.push(() => rect.mockRestore());
  // A 1600×1200 picture.
  for (const [key, value] of [["naturalWidth", 1600], ["naturalHeight", 1200]] as const) {
    Object.defineProperty(HTMLImageElement.prototype, key, { configurable: true, get: () => value });
    restore.push(() => delete (HTMLImageElement.prototype as unknown as Record<string, unknown>)[key]);
  }
});
afterAll(() => {
  for (const undo of restore) undo();
  setImageCoreForTests(null);
});

const base: ViewerItem[] = ["a", "b"].map((id) => ({ src: `/p/${id}.jpg`, name: `${id}.jpg` }));

async function placed(): Promise<HTMLImageElement> {
  const img = document.querySelector<HTMLImageElement>(".pv-slot .pv-img")!;
  fireEvent.load(img);
  await waitFor(() => expect(img.classList.contains("pv-img--placed")).toBe(true));
  return img;
}

describe("PhotoViewer extension points", () => {
  it("renders a toolbar and a panel only when given, and marks the dialog for the layout", () => {
    const { unmount } = render(() => <PhotoViewer items={base} index={0} onIndex={() => {}} onClose={() => {}} />);
    expect(document.querySelector(".pv-toolbar")).toBeNull();
    expect(document.querySelector(".pv-panel")).toBeNull();
    expect(document.querySelector(".pv")).not.toHaveClass("pv--panel");
    unmount();
    render(() => (
      <PhotoViewer items={base} index={0} onIndex={() => {}} onClose={() => {}} toolbar={<button type="button">Extra</button>} panel={<p>Side</p>} />
    ));
    expect(screen.getByRole("toolbar")).toContainElement(screen.getByRole("button", { name: "Extra" }));
    expect(document.querySelector(".pv-panel")).toHaveTextContent("Side");
    expect(document.querySelector(".pv")).toHaveClass("pv--panel");
  });

  it("calls the key hook first; true keeps the viewer from acting on the key", () => {
    const [index, setIndex] = createSignal(0);
    const onClose = vi.fn();
    const hook = vi.fn((e: KeyboardEvent) => e.key === "ArrowRight" || e.key === "Escape");
    render(() => <PhotoViewer items={base} index={index()} onIndex={setIndex} onClose={onClose} onKeyDown={hook} />);
    fireEvent.keyDown(window, { key: "ArrowRight" });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(index()).toBe(0);
    expect(onClose).not.toHaveBeenCalled();
    expect(hook).toHaveBeenCalledTimes(2);
    // A key the hook declines is the viewer's own.
    hook.mockReturnValue(false);
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(index()).toBe(1);
  });

  it("draws a quarter-turned picture fitted to the turned size, and fits it again after a turn", async () => {
    const [rotation, setRotation] = createSignal<0 | 90 | 180 | 270>(0);
    render(() => (
      <PhotoViewer items={[{ ...base[0]!, rotation: rotation() }, base[1]!]} index={0} onIndex={() => {}} onClose={() => {}} />
    ));
    const img = await placed();
    expect([img.style.width, img.style.height, img.style.transform]).toEqual(["800px", "600px", ""]);

    // On its side the 1600×1200 picture is 1200 wide and 1600 tall: 450×600 in the 800×600 stage.
    setRotation(90);
    await waitFor(() => expect(img.style.transform).toBe("rotate(90deg)"));
    // The core fits the turned picture (450×600) into the stage; the unturned image, 600×450, is drawn into its centre.
    await waitFor(() => expect([img.style.width, img.style.height]).toEqual(["600px", "450px"]));
    expect([img.style.left, img.style.top]).toEqual(["100px", "75px"]);

    setRotation(180);
    await waitFor(() => expect(img.style.transform).toBe("rotate(180deg)"));
    await waitFor(() => expect([img.style.width, img.style.height]).toEqual(["800px", "600px"]));
  });

  it("shows a quarter-turned neighbour on its side before it is placed", () => {
    render(() => (
      <PhotoViewer items={[base[0]!, { ...base[1]!, rotation: 270 }]} index={0} onIndex={() => {}} onClose={() => {}} />
    ));
    const imgs = document.querySelectorAll<HTMLImageElement>(".pv-slot .pv-img");
    const neighbour = [...imgs].find((i) => i.getAttribute("src") === "/p/b.jpg")!;
    // Swapped sides, centred on the stage.
    expect([neighbour.style.width, neighbour.style.height, neighbour.style.transform]).toEqual(["600px", "800px", "rotate(270deg)"]);
  });
});
