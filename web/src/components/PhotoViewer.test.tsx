/// <reference types="node" />
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createSignal } from "solid-js";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { imageCoreFromBytes, setImageCoreForTests, type ImageCore } from "../lib/imageCore";
import { PhotoViewer, type ViewerItem } from "./PhotoViewer";

// The viewer with the real Rust core (image-core.wasm) in an 800×600 window.
const W = 800;
const H = 600;
const items: ViewerItem[] = ["a", "b", "c"].map((id) => ({ src: `/p/${id}.jpg`, name: `${id}.jpg` }));

const restore: (() => void)[] = [];
let realCore: ImageCore;
beforeAll(async () => {
  realCore = await imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm")));
  setImageCoreForTests(realCore);
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
  beforeEach(() => {
    vi.useRealTimers();
    setImageCoreForTests(realCore);
  });

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
    let now = 0;
    const clock = vi.spyOn(performance, "now").mockImplementation(() => now);
    pointer("pointerDown", 400, 200);
    now = 500;
    pointer("pointerMove", 402, 230);
    now = 1000;
    pointer("pointerMove", 404, 260);
    now = 1100;
    pointer("pointerUp", 404, 262);
    expect(onClose).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByRole("dialog").classList.contains("pv--animating")).toBe(false));

    pointer("pointerDown", 400, 200);
    pointer("pointerMove", 402, 260);
    pointer("pointerMove", 404, 330);
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).not.toBe("1");
    pointer("pointerUp", 404, 340);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    clock.mockRestore();
  });

  // The swipe down follows the finger at fit size: the picture moves and shrinks and the black fades with the
  // distance. Zoomed, the same drag pans and never closes.
  it("moves, shrinks and fades with a drag down at fit size, and closes past the threshold", async () => {
    const { onClose } = open();
    const img = await placed();
    const slot = () => img.closest<HTMLElement>(".pv-slot")!;
    const backdrop = () => Number(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity);
    pointer("pointerDown", 400, 200);
    pointer("pointerMove", 400, 260);
    const near = backdrop();
    expect(slot().style.transform).toMatch(/translate3d\(0px, 60px, 0(px)?\) scale\(0\.\d+\)/);
    pointer("pointerMove", 400, 350);
    expect(slot().style.transform).toMatch(/translate3d\(0px, 150px, 0(px)?\)/);
    expect(near).toBeLessThan(1);
    expect(backdrop()).toBeLessThan(near);
    pointer("pointerUp", 400, 350);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("pans a zoomed picture with a vertical drag instead of closing", async () => {
    const { onClose } = open();
    const img = await placed();
    fireEvent.dblClick(stage(), { clientX: 400, clientY: 300 });
    await waitFor(() => expect(screen.getByRole("dialog").classList.contains("pv--zoomed")).toBe(true));
    const top = img.style.top;
    pointer("pointerDown", 400, 200);
    pointer("pointerMove", 400, 260);
    pointer("pointerMove", 400, 350);
    expect(img.closest<HTMLElement>(".pv-slot")!.style.transform).not.toContain("scale");
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).toBe("1");
    expect(img.style.top).not.toBe(top);
    pointer("pointerUp", 400, 350);
    await new Promise((r) => setTimeout(r, 250));
    expect(onClose).not.toHaveBeenCalled();
  });

  it.each([true, false])("closes a diagonal drag with the picture loaded=%s", async (loaded) => {
    const { onClose } = open();
    if (loaded) await placed();
    else await Promise.resolve();
    pointer("pointerDown", 300, 150);
    pointer("pointerMove", 405, 300);
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).not.toBe("1");
    pointer("pointerUp", 405, 300);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("closes a fast short flick", async () => {
    const { onClose } = open();
    await placed();
    let now = 0;
    const clock = vi.spyOn(performance, "now").mockImplementation(() => now);
    pointer("pointerDown", 400, 200);
    now = 25;
    pointer("pointerMove", 400, 224);
    pointer("pointerUp", 400, 224);
    clock.mockRestore();
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("keeps a flick's speed when the release comes late, but not after a rest", async () => {
    const { onClose } = open();
    await placed();
    let now = 0;
    const clock = vi.spyOn(performance, "now").mockImplementation(() => now);
    // 30 points in 30 ms; the up event arrives 45 ms later without moving (a busy page). Counting that wait,
    // the speed would be 400 points per second, under the flick threshold.
    pointer("pointerDown", 400, 200);
    now = 15;
    pointer("pointerMove", 400, 215);
    now = 30;
    pointer("pointerMove", 400, 230);
    now = 75;
    pointer("pointerUp", 400, 230);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    clock.mockRestore();
  });

  it("springs back a short drag that rested before the release", async () => {
    const { onClose } = open();
    await placed();
    let now = 0;
    const clock = vi.spyOn(performance, "now").mockImplementation(() => now);
    pointer("pointerDown", 400, 200);
    now = 15;
    pointer("pointerMove", 400, 215);
    now = 30;
    pointer("pointerMove", 400, 230);
    now = 230;
    pointer("pointerUp", 400, 230);
    expect(screen.getByRole("dialog").classList.contains("pv--closing")).toBe(false);
    await new Promise((r) => setTimeout(r, 300));
    expect(onClose).not.toHaveBeenCalled();
    clock.mockRestore();
  });

  it("finishes a page slide when the next drag starts", async () => {
    const { index, onClose } = open();
    await placed();
    pointer("pointerDown", 600, 200);
    pointer("pointerMove", 300, 200);
    pointer("pointerUp", 300, 200);
    expect(index()).toBe(0);
    pointer("pointerDown", 400, 200);
    expect(index()).toBe(1);
    pointer("pointerMove", 400, 400);
    pointer("pointerUp", 400, 400);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("renders extra controls in the top bar and leaves the bottom free", () => {
    render(() => <PhotoViewer items={items} index={0} onIndex={() => {}} onClose={() => {}} toolbar={<button>Extra</button>} />);
    expect(screen.getByRole("button", { name: "Extra" }).closest(".pv-top")).not.toBeNull();
    expect(document.querySelector(".pv-toolbar")).toBeNull();
  });

  it("follows and dismisses a drag while the core is unavailable", async () => {
    setImageCoreForTests(null);
    const { onClose } = open();
    pointer("pointerDown", 300, 150);
    pointer("pointerMove", 405, 300);
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).toBe("0.5");
    pointer("pointerUp", 405, 300);
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });

  it("does not start a picture gesture from the top bar", async () => {
    const { onClose } = open();
    await placed();
    fireEvent.pointerDown(document.querySelector(".pv-title")!, { pointerId: 1, clientX: 300, clientY: 20 });
    pointer("pointerMove", 300, 300);
    pointer("pointerUp", 300, 300);
    expect(document.querySelector<HTMLElement>(".pv-backdrop")!.style.opacity).toBe("1");
    expect(onClose).not.toHaveBeenCalled();
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

  it("zooms on ⌘ + a mouse wheel and toggles fit and 100 % on a slow mouse double click", async () => {
    open();
    const img = await placed();
    fireEvent.wheel(stage(), { metaKey: true, deltaY: -50, clientX: 400, clientY: 300 });
    expect(parseFloat(img.style.width)).toBeGreaterThan(800);

    fireEvent.keyDown(window, { key: "0" });
    await waitFor(() => expect(img.style.width).toBe("800px"));
    fireEvent.dblClick(stage(), { clientX: 400, clientY: 300 });
    await waitFor(() => expect(parseFloat(img.style.width)).toBeGreaterThan(800));
    fireEvent.dblClick(stage(), { clientX: 400, clientY: 300 });
    await waitFor(() => expect(img.style.width).toBe("800px"));
  });

  describe("full screen", () => {
    const shell = (window as { secretaryShell?: unknown });
    afterEach(() => {
      delete shell.secretaryShell;
    });

    it("is Secretary Code's focus mode in the Mac app, toggled by the button and F, and left on close", async () => {
      const setFocusMode = vi.fn();
      let tell: ((on: boolean) => void) | undefined;
      shell.secretaryShell = { setFocusMode, onFocusModeChange: (l: (on: boolean) => void) => { tell = l; return () => undefined; } };
      const [index, setIndex] = createSignal(0);
      const view = render(() => <PhotoViewer items={items} index={index()} onIndex={setIndex} onClose={() => undefined} />);

      fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
      expect(setFocusMode).toHaveBeenLastCalledWith(true);
      expect(screen.getByRole("button", { name: "Exit full screen" })).toHaveAttribute("aria-pressed", "true");
      fireEvent.keyDown(window, { key: "f" });
      expect(setFocusMode).toHaveBeenLastCalledWith(false);
      fireEvent.keyDown(window, { key: "F" });
      expect(setFocusMode).toHaveBeenLastCalledWith(true);

      // The shell leaving focus mode by itself (its Esc) is followed, not echoed back.
      tell?.(false);
      await waitFor(() => expect(screen.getByRole("button", { name: "Full screen" })).toBeInTheDocument());
      const calls = setFocusMode.mock.calls.length;
      view.unmount();
      expect(setFocusMode.mock.calls.length).toBe(calls);
    });

    it("leaves focus mode when the viewer closes while in it", () => {
      const setFocusMode = vi.fn();
      shell.secretaryShell = { setFocusMode };
      const [index, setIndex] = createSignal(0);
      const view = render(() => <PhotoViewer items={items} index={index()} onIndex={setIndex} onClose={() => undefined} />);
      fireEvent.click(screen.getByRole("button", { name: "Full screen" }));
      view.unmount();
      expect(setFocusMode).toHaveBeenLastCalledWith(false);
    });

    it("has no button without the shell or the Fullscreen API", () => {
      open();
      expect(screen.queryByRole("button", { name: "Full screen" })).toBeNull();
    });
  });

  it("story mode: a tap on the sides pages at once, the middle toggles the overlay, which hides by itself", async () => {
    const [index, setIndex] = createSignal(1);
    render(() => (
      <PhotoViewer items={items} index={index()} onIndex={setIndex} onClose={() => {}} tapZones chromeAutoHideMs={3000} overlay={<p class="story-info">who</p>} />
    ));
    await placed();
    const dialog = screen.getByRole("dialog");
    expect(dialog.querySelector(".pv-overlay")).toHaveTextContent("who");
    // Right third: next, without waiting for a double tap.
    pointer("pointerDown", W * 0.9, H / 2);
    pointer("pointerUp", W * 0.9, H / 2);
    await waitFor(() => expect(index()).toBe(2));
    // Left third: back.
    pointer("pointerDown", W * 0.1, H / 2);
    pointer("pointerUp", W * 0.1, H / 2);
    await waitFor(() => expect(index()).toBe(1));
    // The middle toggles the controls and the overlay.
    expect(dialog).toHaveClass("pv--chrome");
    pointer("pointerDown", W / 2, H / 2);
    pointer("pointerUp", W / 2, H / 2);
    await waitFor(() => expect(dialog).not.toHaveClass("pv--chrome"));
    pointer("pointerDown", W / 2, H / 2);
    pointer("pointerUp", W / 2, H / 2);
    await waitFor(() => expect(dialog).toHaveClass("pv--chrome"));
    await waitFor(() => expect(dialog).not.toHaveClass("pv--chrome"), { timeout: 4000 });
  }, 10_000);

  it("keeps its usual taps without story mode: no paging from the sides, no auto-hide", async () => {
    const { index } = open({ start: 1 });
    await placed();
    pointer("pointerDown", W * 0.9, H / 2);
    pointer("pointerUp", W * 0.9, H / 2);
    await new Promise((r) => setTimeout(r, 400));
    expect(index()).toBe(1);
    expect(screen.getByRole("dialog").querySelector(".pv-overlay")).toBeNull();
  });
});
