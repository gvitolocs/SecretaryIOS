import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem, Stack } from "../../api/gallery";
import { setLocale } from "../../i18n";
import { setImageCoreForTests } from "../../lib/imageCore";
import { details, galleryItem } from "../../test/galleryFixtures";
import { GalleryViewer } from "./GalleryViewer";
import { resetPhotoInfoCaches } from "./PhotoInfo";

vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  getDetails: vi.fn(),
  patchItem: vi.fn(),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
  getStack: vi.fn(),
  setStackPrimary: vi.fn(),
  deleteStack: vi.fn(),
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const { getStack, setStackPrimary, deleteStack } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

const inStack = (primary: boolean, count = 3): Partial<GalleryItem> => ({ stack: { id: "stk_1", count, primary } });

const members = (): GalleryItem[] => [galleryItem("p", inStack(true)), galleryItem("m1", inStack(false)), galleryItem("m2", inStack(false))];

const stack = (): Stack => ({ id: "stk_1", kind: "burst", primary: "p", count: 3, createdAt: "2026-10-28T10:00:00Z", items: members() });

function open(items: GalleryItem[], extra: { onStackChanged?: (c: "primary" | "unstack" | "member") => void } = {}) {
  const [index, setIndex] = createSignal(0);
  render(() => (
    <GalleryViewer items={items} index={index()} onIndex={setIndex} onClose={() => {}} onChanged={() => {}} onRemoved={() => {}} onStackChanged={extra.onStackChanged} />
  ));
  return { index, setIndex };
}

const strip = () => screen.findByRole("group", { name: "Photos in this stack" });
const thumbs = (el: HTMLElement) => within(el).getAllByRole("button", { name: /^Photo \d of 3$/ });

beforeEach(() => {
  setLocale("en");
  resetPhotoInfoCaches();
  vi.mocked(api.getDetails).mockImplementation(async (id) => details(galleryItem(id)));
  vi.mocked(api.listAlbums).mockResolvedValue([]);
  vi.mocked(api.listSources).mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
  getStack.mockReset();
  getStack.mockResolvedValue(stack());
  setStackPrimary.mockReset();
  setStackPrimary.mockResolvedValue(stack());
  deleteStack.mockReset();
  deleteStack.mockResolvedValue(["p", "m1", "m2"]);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("a stack in the viewer", () => {
  it("shows the strip of the stack's photos with the primary marked, only for a stacked photo", async () => {
    open([galleryItem("single")]);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole("group", { name: "Photos in this stack" })).toBeNull();
    cleanup();

    open([galleryItem("p", inStack(true))]);
    const el = await strip();
    expect(getStack).toHaveBeenCalledWith("stk_1");
    expect(await within(el).findByText("3 photos in this stack")).toBeInTheDocument();
    expect(thumbs(el)).toHaveLength(3);
    expect(thumbs(el)[0]).toHaveAttribute("aria-current", "true");
    expect(within(el).getByText("Primary")).toBeInTheDocument();
  });

  it("opens another photo of the stack even though the grid lists only the primary", async () => {
    open([galleryItem("p", inStack(true))]);
    const el = await strip();
    await waitFor(() => expect(thumbs(el)).toHaveLength(3));
    fireEvent.click(thumbs(el)[2]!);
    await waitFor(() => expect(thumbs(el)[2]).toHaveAttribute("aria-current", "true"));
    expect(thumbs(el)[0]).not.toHaveAttribute("aria-current");
    // The viewer now pages through the stack: the first photo is one back.
    fireEvent.click(thumbs(el)[0]!);
    await waitFor(() => expect(thumbs(el)[0]).toHaveAttribute("aria-current", "true"));
  });

  it("has no Make primary on the primary, and makes the shown photo primary otherwise", async () => {
    const changed = vi.fn();
    open([galleryItem("p", inStack(true))], { onStackChanged: changed });
    const el = await strip();
    await waitFor(() => expect(thumbs(el)).toHaveLength(3));
    expect(within(el).queryByRole("button", { name: "Make primary" })).toBeNull();
    fireEvent.click(thumbs(el)[1]!);
    await waitFor(() => expect(thumbs(el)[1]).toHaveAttribute("aria-current", "true"));
    fireEvent.click(within(el).getByRole("button", { name: "Make primary" }));
    await waitFor(() => expect(setStackPrimary).toHaveBeenCalledWith("stk_1", "m1"));
    await waitFor(() => expect(changed).toHaveBeenCalledWith("primary"));
  });

  it("unstacks, and tells the list to reload", async () => {
    const changed = vi.fn();
    open([galleryItem("p", inStack(true))], { onStackChanged: changed });
    const el = await strip();
    await waitFor(() => expect(thumbs(el)).toHaveLength(3));
    fireEvent.click(within(el).getByRole("button", { name: "Unstack" }));
    await waitFor(() => expect(deleteStack).toHaveBeenCalledWith("stk_1"));
    await waitFor(() => expect(changed).toHaveBeenCalledWith("unstack"));
  });

  it("says so when the server refuses, and changes nothing", async () => {
    deleteStack.mockRejectedValue(new Error("boom"));
    const changed = vi.fn();
    open([galleryItem("p", inStack(true))], { onStackChanged: changed });
    const el = await strip();
    await waitFor(() => expect(thumbs(el)).toHaveLength(3));
    fireEvent.click(within(el).getByRole("button", { name: "Unstack" }));
    expect(await within(el).findByRole("alert")).toHaveTextContent("That didn't work");
    expect(changed).not.toHaveBeenCalled();
  });

  it("says when the stack can't be loaded", async () => {
    getStack.mockRejectedValue(new Error("gone"));
    open([galleryItem("p", inStack(true))]);
    expect(await screen.findByText("Couldn't load the stack.")).toBeInTheDocument();
  });
});
