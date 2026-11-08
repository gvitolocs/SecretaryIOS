/// <reference types="node" />
import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { imageCoreFromBytes, setImageCoreForTests } from "../../../lib/imageCore";
import { itemRenditions } from "../../../test/galleryParityFixtures";
import { CompareView } from "./CompareView";

vi.mock("../../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

// Two 600 x 400 panes (jsdom has no layout) and pictures of different pixel sizes but the same 4:3 shape.
const W = 600;
const H = 400;
const SIZES: Record<string, [number, number]> = {
  original: [4000, 3000],
  large: [1600, 1200],
  thumb: [480, 360],
  device: [4032, 3024],
};

const restore: (() => void)[] = [];
beforeAll(async () => {
  setImageCoreForTests(await imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm"))));
  const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, width: W, height: H, right: W, bottom: H, toJSON: () => ({}),
  } as DOMRect);
  restore.push(() => rect.mockRestore());
  const natural = (axis: 0 | 1) =>
    function (this: HTMLImageElement) {
      const src = this.getAttribute("src") ?? "";
      const key = src.includes("original") ? "original" : src.includes("size=1024") ? "large" : src.includes("size=256") ? "thumb" : src.includes("device-preview") ? "device" : "original";
      return SIZES[key]![axis];
    };
  Object.defineProperty(HTMLImageElement.prototype, "naturalWidth", { configurable: true, get: natural(0) });
  Object.defineProperty(HTMLImageElement.prototype, "naturalHeight", { configurable: true, get: natural(1) });
  restore.push(() => {
    delete (HTMLImageElement.prototype as unknown as Record<string, unknown>).naturalWidth;
    delete (HTMLImageElement.prototype as unknown as Record<string, unknown>).naturalHeight;
  });
});
afterAll(() => {
  for (const undo of restore) undo();
  setImageCoreForTests(null);
});

beforeEach(() => vi.useRealTimers());

function open(data = itemRenditions()) {
  const onClose = vi.fn();
  render(() => <CompareView name="IMG_0001.JPG" data={data} onClose={onClose} />);
  return { onClose };
}

const images = () => [...document.querySelectorAll<HTMLImageElement>(".cv-img")];
const stage = () => document.querySelector<HTMLElement>(".cv-stage")!;

/** Both pictures loaded and placed by the core. */
async function placed(): Promise<HTMLImageElement[]> {
  await waitFor(() => expect(images()).toHaveLength(2));
  for (const img of images()) fireEvent.load(img);
  await waitFor(() => expect(images().every((i) => i.classList.contains("cv-img--placed"))).toBe(true));
  return images();
}

const box = (img: HTMLImageElement) => ["left", "top", "width", "height"].map((k) => parseFloat(img.style.getPropertyValue(k)));

describe("the two panes", () => {
  it("opens on the server's original against the best display copy, both fitted", async () => {
    open();
    const [a, b] = await placed();
    expect(a!.getAttribute("src")).toBe("/v1/media/items/a/original");
    expect(b!.getAttribute("src")).toBe("/v1/media/items/a/thumb?size=1024&v=1");
    expect(screen.getByLabelText("Left")).toHaveValue("original");
    expect(screen.getByLabelText("Right")).toHaveValue("large");
    // 4:3 in 600 x 400 fits its height: 533 x 400, centred. Both pictures, whatever their pixel size.
    for (const img of [a!, b!]) {
      const [left, top, width, height] = box(img);
      expect(width).toBeCloseTo(533.33, 1);
      expect(height).toBeCloseTo(400, 1);
      expect(left).toBeCloseTo(33.33, 1);
      expect(top).toBeCloseTo(0, 1);
    }
    expect(screen.getByRole("dialog", { name: "Compare copies of IMG_0001.JPG" })).toBeInTheDocument();
  });

  it("captions each pane with its format, size and pixels", async () => {
    open();
    await placed();
    const captions = [...document.querySelectorAll(".cv-caption")].map((c) => c.textContent);
    expect(captions).toEqual(["JPEG · 2.4 MB · 4000 × 3000", "JPEG · 135 KB · 1600 × 1200"]);
  });

  it("explains a picture the browser can't draw and starts from the phone's own rendering", async () => {
    const data = itemRenditions();
    data.original = { ...data.original, format: "heic", mime: "image/heic" };
    data.renditions.push({ name: "device_preview", format: "jpeg", mime: "image/jpeg", bytes: 900_000, blake3: null, width: 4032, height: 3024, url: "/v1/media/items/a/device-preview?v=1" });
    open(data);
    await waitFor(() => expect(images()).toHaveLength(2));
    expect(screen.getByLabelText("Left")).toHaveValue("device_preview");
    // Picking the HEIC original shows why there is nothing to see.
    fireEvent.change(screen.getByLabelText("Left"), { target: { value: "original" } });
    expect(await screen.findByText("HEIC can't be shown in this browser.")).toBeInTheDocument();
    expect(images()).toHaveLength(1);
  });

  it("lets either pane show an older version", async () => {
    const data = itemRenditions({
      versions: [{ version: 1, format: "jpeg", mime: "image/jpeg", bytes: 2_000_000, blake3: "c".repeat(64), width: null, height: null, url: "/v1/media/items/a/original?version=1", replacedAt: null, stored: "store", reachable: true }],
    });
    open(data);
    await placed();
    fireEvent.change(screen.getByLabelText("Right"), { target: { value: "version:1" } });
    await waitFor(() => expect(images().map((i) => i.getAttribute("src"))).toEqual(["/v1/media/items/a/original", "/v1/media/items/a/original?version=1"]));
  });
});

describe("zoom and pan", () => {
  it("zooms both panes together about the pointer", async () => {
    open();
    const [a, b] = await placed();
    fireEvent.wheel(stage(), { deltaY: -100, ctrlKey: true, clientX: 300, clientY: 200 });
    await waitFor(() => expect(box(a!)[2]).toBeGreaterThan(540));
    const ba = box(a!);
    const bb = box(b!);
    for (let i = 0; i < 4; i++) expect(bb[i]).toBeCloseTo(ba[i]!, 2);
    // The percentage follows (a ctrl-wheel of 100 is a factor of e).
    expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("272%");
  });

  it("pans both panes together and keeps them inside the picture", async () => {
    open();
    const [a, b] = await placed();
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("225%"));
    const before = box(a!);
    fireEvent.pointerDown(stage(), { pointerId: 1, clientX: 300, clientY: 200 });
    fireEvent.pointerMove(stage(), { pointerId: 1, clientX: 260, clientY: 180 });
    fireEvent.pointerUp(stage(), { pointerId: 1, clientX: 260, clientY: 180 });
    await waitFor(() => expect(box(a!)[0]).toBeCloseTo(before[0]! - 40, 1));
    expect(box(a!)[1]).toBeCloseTo(before[1]! - 20, 1);
    for (let i = 0; i < 4; i++) expect(box(b!)[i]).toBeCloseTo(box(a!)[i]!, 2);
    // A long drag stops at the picture's edge, in both.
    fireEvent.pointerDown(stage(), { pointerId: 1, clientX: 300, clientY: 200 });
    fireEvent.pointerMove(stage(), { pointerId: 1, clientX: -5000, clientY: -5000 });
    fireEvent.pointerUp(stage(), { pointerId: 1 });
    await waitFor(() => expect(box(a!)[0]! + box(a!)[2]!).toBeCloseTo(W, 1));
    expect(box(a!)[1]! + box(a!)[3]!).toBeCloseTo(H, 1);
    for (let i = 0; i < 4; i++) expect(box(b!)[i]).toBeCloseTo(box(a!)[i]!, 2);
  });

  it("zooms on a double click and fits again on 0", async () => {
    open();
    const [a] = await placed();
    fireEvent.dblClick(stage(), { clientX: 300, clientY: 200 });
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("250%"));
    fireEvent.keyDown(document.body, { key: "0" });
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("100%"));
    expect(box(a!)[2]).toBeCloseTo(533.33, 1);
    fireEvent.keyDown(document.body, { key: "+" });
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("150%"));
    fireEvent.keyDown(document.body, { key: "-" });
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("100%"));
  });

  it("keeps the zoom when the other source is changed to a picture of the same size", async () => {
    open();
    await placed();
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("150%"));
  });
});

describe("the slider", () => {
  it("lays one picture over the other and moves the divider with the keys", async () => {
    open();
    await placed();
    fireEvent.click(screen.getByRole("button", { name: "Slider" }));
    const handle = await screen.findByRole("slider", { name: "Where the two pictures meet" });
    expect(handle).toHaveAttribute("aria-valuenow", "50");
    const over = () => document.querySelector<HTMLElement>(".cv-pane--over")!;
    expect(over().style.clipPath).toBe("inset(0 0 0 50%)");
    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(handle).toHaveAttribute("aria-valuenow", "55");
    expect(over().style.clipPath).toBe("inset(0 0 0 55%)");
    fireEvent.keyDown(handle, { key: "Home" });
    expect(handle).toHaveAttribute("aria-valuenow", "2");
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(handle).toHaveAttribute("aria-valuenow", "2");
    fireEvent.keyDown(handle, { key: "End" });
    expect(handle).toHaveAttribute("aria-valuenow", "98");
  });

  it("drags the divider with the pointer", async () => {
    open();
    await placed();
    fireEvent.click(screen.getByRole("button", { name: "Slider" }));
    const handle = await screen.findByRole("slider");
    fireEvent.pointerDown(handle, { pointerId: 2, clientX: 150, clientY: 100 });
    expect(handle).toHaveAttribute("aria-valuenow", "25");
    fireEvent.pointerMove(handle, { pointerId: 2, clientX: 450, clientY: 100 });
    expect(handle).toHaveAttribute("aria-valuenow", "75");
    fireEvent.pointerUp(handle, { pointerId: 2 });
    fireEvent.pointerMove(handle, { pointerId: 2, clientX: 60, clientY: 100 });
    expect(handle).toHaveAttribute("aria-valuenow", "75");
  });

  it("keeps the zoom shared in the slider and toggles with S", async () => {
    open();
    await placed();
    fireEvent.keyDown(document.body, { key: "s" });
    await screen.findByRole("slider");
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Fit to window" }).textContent).toBe("150%"));
    const [a, b] = images();
    for (let i = 0; i < 4; i++) expect(box(b!)[i]).toBeCloseTo(box(a!)[i]!, 2);
    fireEvent.keyDown(document.body, { key: "S" });
    await waitFor(() => expect(screen.queryByRole("slider")).toBeNull());
    expect(screen.getByRole("button", { name: "Side by side" })).toHaveAttribute("aria-pressed", "true");
  });
});

describe("focus", () => {
  it("keeps Tab inside the dialog", async () => {
    open();
    await placed();
    const dialog = screen.getByRole("dialog");
    const buttons = [...dialog.querySelectorAll<HTMLElement>("button:not([disabled]), select")];
    const first = buttons[0]!;
    const last = buttons[buttons.length - 1]!;
    last.focus();
    const forward = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    document.activeElement!.dispatchEvent(forward);
    expect(forward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);
    const back = new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true, cancelable: true });
    first.dispatchEvent(back);
    expect(back.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);
    // From the middle the browser's own Tab order is left alone.
    buttons[2]!.focus();
    const middle = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    buttons[2]!.dispatchEvent(middle);
    expect(middle.defaultPrevented).toBe(false);
  });
});

describe("closing", () => {
  it("closes on Esc without the viewer under it seeing the key", async () => {
    const { onClose } = open();
    const viewer = vi.fn();
    window.addEventListener("keydown", viewer);
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(viewer).not.toHaveBeenCalled();
    window.removeEventListener("keydown", viewer);
  });

  it("keeps the viewer's own keys (favorite, rotate, delete, paging) from reaching it", async () => {
    open();
    await placed();
    const viewer = vi.fn();
    window.addEventListener("keydown", viewer);
    for (const key of ["f", "r", "Delete", "Backspace", "i"]) fireEvent.keyDown(document.body, { key });
    expect(viewer).not.toHaveBeenCalled();
    window.removeEventListener("keydown", viewer);
  });

  it("closes from the close button", () => {
    const { onClose } = open();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
