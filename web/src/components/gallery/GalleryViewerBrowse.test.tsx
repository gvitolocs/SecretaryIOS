import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
import { setLocale } from "../../i18n";
import { setImageCoreForTests } from "../../lib/imageCore";
import { galleryItem } from "../../test/galleryFixtures";
import { GalleryViewer } from "./GalleryViewer";
import { resetPhotoInfoCaches } from "./PhotoInfo";

// The browse parts of the viewer: Archive / Unarchive and videos played by GalleryVideo.
vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  getDetails: vi.fn(),
  patchItem: vi.fn(),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
  createAlbum: vi.fn(),
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("./browse/GalleryVideo", () => ({
  GalleryVideo: (p: { id: string; poster?: string | null; active: boolean; durationMs?: number | null }) => (
    <div data-testid="gallery-video" data-id={p.id} data-active={String(p.active)} data-poster={p.poster ?? ""} data-duration={String(p.durationMs ?? "")} />
  ),
}));

const { getDetails, batch, listAlbums, listSources } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

function open(opts: { items?: GalleryItem[]; start?: number; archive?: boolean; trash?: boolean } = {}) {
  const [list, setList] = createSignal(opts.items ?? [galleryItem("a"), galleryItem("b")]);
  const [index, setIndex] = createSignal(opts.start ?? 0);
  const onRemoved = vi.fn((id: string) => setList((l) => l.filter((i) => i.id !== id)));
  render(() => (
    <GalleryViewer items={list()} index={index()} onIndex={setIndex} onClose={() => {}} onChanged={() => {}} onRemoved={onRemoved} archive={opts.archive} trash={opts.trash} />
  ));
  return { onRemoved, index, list };
}

beforeEach(() => {
  setLocale("en");
  resetPhotoInfoCaches();
  for (const fn of [getDetails, batch, listAlbums, listSources]) fn.mockReset();
  getDetails.mockImplementation(async (id) => ({ item: galleryItem(id), exif: null, people: [] }));
  listAlbums.mockResolvedValue([]);
  listSources.mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
});

describe("archive in the viewer", () => {
  it("archives the current item from the toolbar and lets it leave the list", async () => {
    batch.mockResolvedValue({ done: ["a"], failed: [] });
    const { onRemoved } = open();
    fireEvent.click(screen.getByRole("button", { name: "More" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Archive" }));
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
    expect(batch).toHaveBeenCalledWith("archive", ["a"]);
  });

  it("archives with Shift+A, and a plain A does nothing", async () => {
    batch.mockResolvedValue({ done: ["a"], failed: [] });
    const { onRemoved } = open();
    fireEvent.keyDown(window, { key: "a" });
    expect(batch).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "A", shiftKey: true });
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
    expect(batch).toHaveBeenCalledWith("archive", ["a"]);
  });

  it("says Unarchive inside the Archive", async () => {
    batch.mockResolvedValue({ done: ["a"], failed: [] });
    const { onRemoved } = open({ archive: true });
    expect(screen.queryByRole("button", { name: "Archive" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "More" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Unarchive" }));
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
    expect(batch).toHaveBeenCalledWith("unarchive", ["a"]);
  });

  it("keeps the item and says so when the server refuses", async () => {
    batch.mockResolvedValue({ done: [], failed: [{ id: "a", error: "trashed" }] });
    const { onRemoved } = open();
    fireEvent.click(screen.getByRole("button", { name: "More" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Archive" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't change the archive. Try again.");
    expect(onRemoved).not.toHaveBeenCalled();
  });

  it("has no Archive in the Trash", () => {
    open({ trash: true });
    expect(screen.queryByRole("button", { name: "Archive" })).toBeNull();
    fireEvent.keyDown(window, { key: "A", shiftKey: true });
    expect(batch).not.toHaveBeenCalled();
  });
});

describe("videos in the viewer", () => {
  const clip = galleryItem("clip", { kind: "video", mime: "video/mp4", durationMs: 65_000, urls: { thumb: "/t/clip", large: "/l/clip", poster: "/p/clip" } });

  it("plays a video through GalleryVideo with its poster, active only while current", () => {
    open({ items: [clip, galleryItem("b")], start: 0 });
    const video = screen.getByTestId("gallery-video");
    expect(video).toHaveAttribute("data-id", "clip");
    expect(video).toHaveAttribute("data-active", "true");
    expect(video).toHaveAttribute("data-poster", "/p/clip");
    expect(video).toHaveAttribute("data-duration", "65000");
  });

  it("does not wake a neighbouring video", () => {
    open({ items: [galleryItem("a"), clip], start: 0 });
    expect(screen.getByTestId("gallery-video")).toHaveAttribute("data-active", "false");
  });

  it("leaves the arrow keys to a focused video (they seek there) but still pages from elsewhere", () => {
    const { index } = open({ items: [galleryItem("a"), clip, galleryItem("c")], start: 1 });
    const video = document.createElement("video");
    document.body.append(video);
    fireEvent.keyDown(video, { key: "ArrowRight" });
    expect(index()).toBe(1);
    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(index()).toBe(2);
    video.remove();
  });
});
