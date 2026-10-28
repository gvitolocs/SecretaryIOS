import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
import { setImageCoreForTests } from "../../lib/imageCore";
import { details, galleryItem } from "../../test/galleryFixtures";
import { itemRenditions, withLatest } from "../../test/galleryParityFixtures";
import { GalleryViewer } from "./GalleryViewer";
import { resetPhotoInfoCaches } from "./PhotoInfo";
import { SelectionBar } from "./SelectionBar";

// Share and Compare inside the viewer: the Share sheet from the viewer's Share button, "Server copy / Device copy" in
// the info panel, and Esc closing the compare view only.

vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  getDetails: vi.fn(),
  listAlbums: vi.fn(),
  listSources: vi.fn(),
  getRenditions: vi.fn(),
  createSelectionShare: vi.fn(),
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../LucinaMap", () => ({ LucinaMap: () => <div data-testid="mini-map" /> }));

const { getDetails, listAlbums, listSources, getRenditions, createSelectionShare } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

function open(items: GalleryItem[] = [galleryItem("a"), galleryItem("b")], trash = false) {
  const onClose = vi.fn();
  render(() => {
    const [index, setIndex] = createSignal(0);
    return <GalleryViewer items={items} index={index()} onIndex={setIndex} onClose={onClose} onChanged={() => {}} onRemoved={() => {}} trash={trash} />;
  });
  return { onClose };
}

beforeEach(() => {
  resetPhotoInfoCaches();
  vi.clearAllMocks();
  getDetails.mockImplementation(async (id) => details(galleryItem(id)));
  listAlbums.mockResolvedValue([]);
  listSources.mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
  getRenditions.mockResolvedValue(withLatest(itemRenditions()));
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});

describe("Share in the viewer", () => {
  it("opens the Share sheet for this picture from the Share button", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Share" }));
    const dialog = await screen.findByRole("dialog", { name: "Share this item" });
    createSelectionShare.mockResolvedValue({
      token: "T1", kind: "selection", albumId: null, title: "Shared photos", count: 1, coverItem: "a", coverThumb: null, createdAt: "", expiresAt: null,
      revokedAt: null, allowOriginal: false, views: 0, lastViewAt: null, live: true, publicPath: "/a/T1",
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create link" }));
    await waitFor(() => expect(createSelectionShare).toHaveBeenCalledWith(["a"], expect.objectContaining({ expiresInDays: 7 })));
  });

  it("closes the sheet on Esc and leaves the viewer open", async () => {
    const { onClose } = open();
    fireEvent.click(screen.getByRole("button", { name: "Share" }));
    const dialog = await screen.findByRole("dialog", { name: "Share this item" });
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Share this item" })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
  });

  it("has no Share button in the Trash", () => {
    open([galleryItem("a", { trashedAt: "2026-10-27T00:00:00Z" })], true);
    expect(screen.queryByRole("button", { name: "Share" })).toBeNull();
  });
});

describe("Server copy / Device copy in the info panel", () => {
  it("shows the copies under the file details and opens the compare view", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Info" }));
    const panel = screen.getByRole("region", { name: "Info" });
    const copies = await within(panel).findByRole("region", { name: "Copies" });
    expect(getRenditions).toHaveBeenCalledWith("a");
    expect(within(copies).getByRole("tab", { name: "Server copy" })).toBeInTheDocument();
    fireEvent.click(within(copies).getByRole("button", { name: "Compare" }));
    expect(await screen.findByRole("dialog", { name: "Compare copies of a.jpg" })).toBeInTheDocument();
  });

  it("closes the compare view on Esc without closing the viewer, and the viewer's keys do nothing under it", async () => {
    const { onClose } = open();
    fireEvent.click(screen.getByRole("button", { name: "Info" }));
    const copies = await screen.findByRole("region", { name: "Copies" });
    fireEvent.click(within(copies).getByRole("button", { name: "Compare" }));
    await screen.findByRole("dialog", { name: "Compare copies of a.jpg" });
    // "R" would rotate and Delete would open the delete dialog if the viewer saw them.
    fireEvent.keyDown(document.body, { key: "r" });
    fireEvent.keyDown(document.body, { key: "Delete" });
    expect(screen.queryByRole("dialog", { name: /Move to Trash/ })).toBeNull();
    fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Compare copies of a.jpg" })).toBeNull());
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
    // The second Esc closes the viewer, as before.
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("follows the picture: the next item asks for its own copies", async () => {
    open();
    fireEvent.click(screen.getByRole("button", { name: "Info" }));
    await screen.findByRole("region", { name: "Copies" });
    fireEvent.keyDown(document.body, { key: "ArrowRight" });
    await waitFor(() => expect(getRenditions).toHaveBeenCalledWith("b"));
  });
});

describe("the selection bar", () => {
  const props = {
    count: 2, all: false, allFavorite: false, busy: false,
    onSelectAll: () => {}, onClear: () => {}, onFavorite: () => {}, onAlbum: () => {}, onDownload: () => {}, onDelete: () => {}, onRestore: () => {},
  };

  it("has a Share button only when it is given the handler", () => {
    const onShare = vi.fn();
    const { unmount } = render(() => <SelectionBar {...props} />);
    expect(screen.queryByRole("button", { name: "Share" })).toBeNull();
    unmount();
    render(() => <SelectionBar {...props} onShare={onShare} />);
    fireEvent.click(screen.getByRole("button", { name: "Share" }));
    expect(onShare).toHaveBeenCalledTimes(1);
  });

  it("has none in the Trash", () => {
    render(() => <SelectionBar {...props} trash onShare={() => {}} />);
    expect(screen.queryByRole("button", { name: "Share" })).toBeNull();
  });
});
