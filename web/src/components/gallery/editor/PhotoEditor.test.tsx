/// <reference types="node" />
import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../../api/gallery";
import { ApiError } from "../../../api/client";
import { setLocale } from "../../../i18n";
import { emptyEdit, type EditList } from "../../../lib/editList";
import { imageCoreFromBytes, setImageCoreForTests } from "../../../lib/imageCore";
import { galleryItem } from "../../../test/galleryFixtures";
import { PhotoEditor } from "./PhotoEditor";

vi.mock("../../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../../api/gallery")>()),
  getEdits: vi.fn(),
  putEdits: vi.fn(),
  deleteEdits: vi.fn(),
  patchItem: vi.fn(),
}));

// The picture the editor "downloads": 64x48 mid-grey, plus the 16x12 copy for the filter tiles.
const raster = (w: number, h: number, v = 80) => ({ width: w, height: h, rgba: new Uint8ClampedArray(Array.from({ length: w * h }, () => [v, v, v, 255]).flat()) });
const sourceBox = vi.hoisted(() => ({ dark: false, fail: false, big: false }));
vi.mock("./loadSource", () => ({
  loadSource: vi.fn(async () => {
    if (sourceBox.fail) throw new Error("no picture");
    const v = sourceBox.dark ? 10 : 80;
    const full = sourceBox.big ? raster(1200, 900, v) : raster(64, 48, v);
    return { full, draft: sourceBox.big ? raster(960, 720, v) : full, tile: raster(16, 12, v) };
  }),
}));

const { getEdits, putEdits, deleteEdits, patchItem } = vi.mocked(api);

/** Every putImageData the preview canvas got: the sizes and the first pixel. */
const drawn: { width: number; height: number; first: number }[] = [];

beforeAll(async () => {
  setImageCoreForTests(await imageCoreFromBytes(readFileSync(join(process.cwd(), "src/lib/image-core.wasm"))));
});

beforeEach(() => {
  setLocale("en");
  drawn.length = 0;
  sourceBox.dark = false;
  sourceBox.fail = false;
  sourceBox.big = false;
  for (const fn of [getEdits, putEdits, deleteEdits, patchItem]) fn.mockReset();
  vi.stubGlobal(
    "ImageData",
    class {
      constructor(
        public data: Uint8ClampedArray,
        public width: number,
        public height: number,
      ) {}
    },
  );
  // The stage is 400x300 CSS pixels.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private cb: ResizeObserverCallback) {}
      observe() {
        this.cb([{ contentRect: { width: 400, height: 300 } } as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      disconnect() {}
    },
  );
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
    () => ({ putImageData: (d: { data: Uint8ClampedArray; width: number; height: number }) => drawn.push({ width: d.width, height: d.height, first: d.data[0]! }) }) as unknown as CanvasRenderingContext2D,
  );
  getEdits.mockResolvedValue({ edited: false, edits: null, updatedAt: null, baseUrl: "/v1/media/items/a/edits/base", item: galleryItem("a") });
  putEdits.mockImplementation(async (id) => ({ edited: true, edits: null, updatedAt: null, baseUrl: null, item: galleryItem(id, { edited: true }) }));
  deleteEdits.mockImplementation(async (id) => ({ edited: false, edits: null, updatedAt: null, baseUrl: null, item: galleryItem(id, { edited: false }) }));
  patchItem.mockImplementation(async (id) => galleryItem(id, { rotation: 0 }));
});

function open(item = galleryItem("a")) {
  const spies = { onClose: vi.fn(), onSaved: vi.fn() };
  render(() => <PhotoEditor item={item} onClose={spies.onClose} onSaved={spies.onSaved} />);
  return spies;
}

async function ready(item = galleryItem("a")) {
  const spies = open(item);
  await screen.findByRole("tablist");
  await waitFor(() => expect(drawn.length).toBeGreaterThan(0));
  return spies;
}

const slider = (name: string) => screen.getByLabelText(name) as HTMLInputElement;
const move = (name: string, value: number) => {
  fireEvent.input(slider(name), { target: { value: String(value) } });
  fireEvent.change(slider(name), { target: { value: String(value) } });
};
const saved = (): EditList => putEdits.mock.calls[0]![1];
const last = () => drawn[drawn.length - 1]!;

describe("the photo editor", () => {
  it("opens the working copy and renders it", async () => {
    await ready();
    expect(getEdits).toHaveBeenCalledWith("a");
    expect(drawn[0]).toMatchObject({ width: 64, height: 48, first: 80 });
    expect(screen.getByRole("tab", { name: "Adjust" })).toHaveAttribute("aria-selected", "true");
    for (const name of ["Exposure", "Contrast", "Highlights", "Shadows", "Saturation", "Warmth", "Tint", "Vignette"]) expect(slider(name)).toBeInTheDocument();
    // Nothing changed yet: Save waits, Undo waits.
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Undo" })).toBeDisabled();
  });

  it("shows a loading state, then says when the picture can't be opened", async () => {
    sourceBox.fail = true;
    const { onClose } = open();
    expect(screen.getByRole("status")).toHaveTextContent("Opening the picture");
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't open the picture for editing");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("follows a slider live, records one step on release, and undo and redo walk it", async () => {
    await ready();
    fireEvent.input(slider("Exposure"), { target: { value: "1" } });
    await waitFor(() => expect(last().first).toBeGreaterThan(80 + 20));
    // Nothing is recorded while it moves.
    expect(screen.getByRole("button", { name: "Save" })).not.toBeDisabled();
    fireEvent.change(slider("Exposure"), { target: { value: "1" } });
    expect(slider("Exposure").value).toBe("1");

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(slider("Exposure").value).toBe("0"));
    await waitFor(() => expect(last().first).toBe(80));
    expect(screen.getByRole("button", { name: "Undo" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Redo" }));
    await waitFor(() => expect(slider("Exposure").value).toBe("1"));
    // Ctrl+Z works from the keyboard too.
    fireEvent.keyDown(window, { key: "z", ctrlKey: true });
    await waitFor(() => expect(slider("Exposure").value).toBe("0"));
    fireEvent.keyDown(window, { key: "z", ctrlKey: true, shiftKey: true });
    await waitFor(() => expect(slider("Exposure").value).toBe("1"));
  });

  it("resets one slider on its own, and all of them", async () => {
    await ready();
    move("Contrast", 40);
    move("Warmth", -20);
    fireEvent.click(screen.getByRole("button", { name: "Reset Contrast" }));
    expect(slider("Contrast").value).toBe("0");
    expect(slider("Warmth").value).toBe("-20");
    fireEvent.click(screen.getByRole("button", { name: "Reset all" }));
    expect(slider("Warmth").value).toBe("0");
  });

  it("saves the edit list it built, and hands the saved item to the parent", async () => {
    const { onSaved } = await ready();
    move("Exposure", 0.5);
    move("Saturation", -100);
    fireEvent.click(screen.getByRole("tab", { name: "Filters" }));
    fireEvent.click(screen.getByRole("radio", { name: /Vivid/ }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(putEdits).toHaveBeenCalledTimes(1);
    expect(putEdits.mock.calls[0]![0]).toBe("a");
    expect(saved()).toMatchObject({ v: 1, rotate: 0, adjust: { exposure: 0.5, saturation: -100 }, filter: { name: "vivid", strength: 1 }, crop: null, markup: [] });
    expect(onSaved.mock.calls[0]![0]).toMatchObject({ id: "a", edited: true });
    expect(patchItem).not.toHaveBeenCalled();
  });

  it("turns the picture on the Crop tab: the stage follows and the list says so", async () => {
    await ready();
    fireEvent.click(screen.getByRole("tab", { name: "Crop & rotate" }));
    fireEvent.click(screen.getByRole("button", { name: "Rotate right" }));
    await waitFor(() => expect(last()).toMatchObject({ width: 48, height: 64 }));
    fireEvent.click(screen.getByRole("button", { name: "Rotate left" }));
    fireEvent.click(screen.getByRole("button", { name: "Rotate left" }));
    await waitFor(() => expect(last()).toMatchObject({ width: 48, height: 64 }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(putEdits).toHaveBeenCalled());
    expect(saved().rotate).toBe(3);
  });

  it("straightens, and the shape presets crop the frame", async () => {
    await ready();
    fireEvent.click(screen.getByRole("tab", { name: "Crop & rotate" }));
    move("Straighten", 10);
    await waitFor(() => expect(last().width).toBeLessThan(64));
    fireEvent.click(screen.getByRole("radio", { name: "Square" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(putEdits).toHaveBeenCalled());
    const list = saved();
    expect(list.straighten).toBe(10);
    expect(list.crop).not.toBeNull();
    // Square in pixels: the frame is a bit under 64x48 (aspect 4:3), so the box is 0.75 as wide as it is tall.
    expect(list.crop!.w / list.crop!.h).toBeCloseTo(0.75, 1);
    expect(list.crop!.h).toBeCloseTo(1, 2);
  });

  it("renders the smaller copy while a control is dragged and the full one when it is let go", async () => {
    sourceBox.big = true;
    await ready();
    expect(last()).toMatchObject({ width: 1200, height: 900 });
    fireEvent.input(slider("Exposure"), { target: { value: "1" } });
    await waitFor(() => expect(last()).toMatchObject({ width: 960, height: 720 }));
    fireEvent.change(slider("Exposure"), { target: { value: "1" } });
    await waitFor(() => expect(last()).toMatchObject({ width: 1200, height: 900 }));
    // The saved list does not depend on which copy the preview used.
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(putEdits).toHaveBeenCalled());
    expect(saved().adjust.exposure).toBe(1);
  });

  it("sets Auto from the picture", async () => {
    sourceBox.dark = true;
    await ready();
    expect(slider("Exposure").value).toBe("0");
    fireEvent.click(screen.getByRole("button", { name: "Auto" }));
    await waitFor(() => expect(Number(slider("Exposure").value)).toBeGreaterThan(0.5));
    await waitFor(() => expect(last().first).toBeGreaterThan(15));
  });

  it("holds the original while Compare is pressed", async () => {
    await ready();
    move("Exposure", 1);
    await waitFor(() => expect(last().first).toBeGreaterThan(100));
    const compare = screen.getByRole("button", { name: /compare/i });
    fireEvent.pointerDown(compare);
    await waitFor(() => expect(last().first).toBe(80));
    expect(screen.getByText("Original")).toBeInTheDocument();
    fireEvent.pointerUp(compare);
    await waitFor(() => expect(last().first).toBeGreaterThan(100));
  });

  it("draws a pen stroke on the Markup tab into the list", async () => {
    await ready();
    fireEvent.click(screen.getByRole("tab", { name: "Markup" }));
    const layer = document.querySelector<HTMLElement>(".ed-markup")!;
    vi.spyOn(layer, "getBoundingClientRect").mockReturnValue({ left: 0, top: 0, width: 200, height: 150, right: 200, bottom: 150, x: 0, y: 0, toJSON: () => ({}) });
    layer.setPointerCapture = vi.fn();
    layer.hasPointerCapture = () => false;
    fireEvent.click(screen.getByRole("radio", { name: "Colour #0a84ff" }));
    fireEvent.click(screen.getByRole("radio", { name: "Thick" }));
    fireEvent.pointerDown(layer, { clientX: 20, clientY: 30, pointerId: 1 });
    fireEvent.pointerMove(layer, { clientX: 100, clientY: 60, pointerId: 1 });
    fireEvent.pointerMove(layer, { clientX: 150, clientY: 120, pointerId: 1 });
    fireEvent.pointerUp(layer, { clientX: 150, clientY: 120, pointerId: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(putEdits).toHaveBeenCalled());
    const [mark] = saved().markup;
    expect(mark).toMatchObject({ t: "pen", color: "#0a84ff", width: 0.016 });
    const points = (mark as { points: [number, number][] }).points;
    expect(points.length).toBeGreaterThanOrEqual(3);
    expect(points[0]).toEqual([0.1, 0.2]);
    expect(points[points.length - 1]![0]).toBeCloseTo(0.75);
  });

  it("adds text where you click", async () => {
    await ready();
    fireEvent.click(screen.getByRole("tab", { name: "Markup" }));
    fireEvent.click(screen.getByRole("radio", { name: /Text/ }));
    const layer = document.querySelector<HTMLElement>(".ed-markup")!;
    vi.spyOn(layer, "getBoundingClientRect").mockReturnValue({ left: 0, top: 0, width: 200, height: 100, right: 200, bottom: 100, x: 0, y: 0, toJSON: () => ({}) });
    fireEvent.pointerDown(layer, { clientX: 50, clientY: 25, pointerId: 1 });
    fireEvent.pointerUp(layer, { clientX: 50, clientY: 25, pointerId: 1 });
    const input = await screen.findByLabelText("Text to add");
    fireEvent.input(input, { target: { value: "Ciao" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(putEdits).toHaveBeenCalled());
    expect(saved().markup[0]).toMatchObject({ t: "text", text: "Ciao", x: 0.25, y: 0.25 });
  });

  it("asks before throwing changes away, and only then", async () => {
    const { onClose } = await ready();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledTimes(1);

    // Not for a clean editor, but for a changed one.
    move("Tint", 20);
    fireEvent.keyDown(window, { key: "Escape" });
    const dialog = await screen.findByRole("dialog", { name: "Discard your changes?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Keep editing" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Discard your changes?" })).toBeNull());
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(within(await screen.findByRole("dialog", { name: "Discard your changes?" })).getByRole("button", { name: "Discard" }));
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(putEdits).not.toHaveBeenCalled();
  });

  it("reverts an edited photo to the original after a confirmation", async () => {
    const item = galleryItem("a", { edited: true });
    getEdits.mockResolvedValue({ edited: true, edits: { ...emptyEdit(), adjust: { ...emptyEdit().adjust, exposure: 0.5 } }, updatedAt: null, baseUrl: "/v1/media/items/a/edits/base", item });
    const { onSaved } = await ready(item);
    // The stored edit is what the sliders start from.
    expect(slider("Exposure").value).toBe("0.5");
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Revert to original" }));
    const dialog = await screen.findByRole("dialog", { name: "Revert to the original?" });
    expect(dialog).toHaveTextContent("original file was never changed");
    fireEvent.click(within(dialog).getByRole("button", { name: "Revert" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(deleteEdits).toHaveBeenCalledWith("a");
    expect(onSaved.mock.calls[0]![0]).toMatchObject({ id: "a", edited: false });
    expect(putEdits).not.toHaveBeenCalled();
  });

  it("offers no revert for a photo that was never edited", async () => {
    await ready();
    expect(screen.queryByRole("button", { name: "Revert to original" })).toBeNull();
  });

  it("keeps the editor open and says so when the save fails", async () => {
    putEdits.mockRejectedValueOnce(new ApiError(500, "internal", "boom"));
    const { onSaved } = await ready();
    move("Exposure", 1);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't save the edit");
    expect(onSaved).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Save" })).not.toBeDisabled();
    // The server refusing the picture has its own words.
    putEdits.mockRejectedValueOnce(new ApiError(422, "unprocessable", "x"));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("can't edit this picture"));
  });

  it("bakes the viewer's rotation into the edit and resets it", async () => {
    const item = galleryItem("a", { rotation: 90 });
    const { onSaved } = await ready(item);
    // Opened already turned: there is something to save.
    expect(screen.getByRole("button", { name: "Save" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(saved().rotate).toBe(1);
    expect(patchItem).toHaveBeenCalledWith("a", { rotation: 0 });
    expect(onSaved.mock.calls[0]![0].rotation).toBe(0);
  });
});
