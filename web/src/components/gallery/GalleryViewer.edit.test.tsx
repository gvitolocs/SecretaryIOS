import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
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
  createAlbum: vi.fn(),
}));
vi.mock("../../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../LucinaMap", () => ({ LucinaMap: () => <div /> }));

// The editor is tested on its own (editor/PhotoEditor.test.tsx); here it is a stand-in with a save and a close.
const editorProps = vi.hoisted(() => [] as { item: GalleryItem }[]);
vi.mock("./editor/PhotoEditor", () => ({
  PhotoEditor: (props: { item: GalleryItem; onClose: () => void; onSaved: (item: GalleryItem) => void }) => {
    editorProps.push({ item: props.item });
    return (
      <div role="dialog" aria-label="Edit photo">
        <button type="button" onClick={props.onClose}>
          close editor
        </button>
        <button
          type="button"
          onClick={() =>
            props.onSaved({
              ...props.item,
              edited: true,
              urls: { ...props.item.urls, large: `${props.item.id}-edited-large`, edited: `${props.item.id}-edited-full`, downloadEdited: `${props.item.id}-edited-download` },
            })
          }
        >
          save editor
        </button>
      </div>
    );
  },
}));

const { getDetails, patchItem, listAlbums, listSources } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

beforeEach(() => {
  setLocale("en");
  editorProps.length = 0;
  resetPhotoInfoCaches();
  for (const fn of [getDetails, patchItem, listAlbums, listSources]) fn.mockReset();
  getDetails.mockImplementation(async (id) => details(galleryItem(id)));
  listAlbums.mockResolvedValue([]);
  listSources.mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
});

function open(items: GalleryItem[], trash = false) {
  const [list, setList] = createSignal(items);
  const spies = { onClose: vi.fn(), onChanged: vi.fn(), onRemoved: vi.fn() };
  render(() => (
    <GalleryViewer
      items={list()}
      index={0}
      onIndex={() => {}}
      onClose={spies.onClose}
      trash={trash}
      onChanged={(item) => {
        spies.onChanged(item);
        setList((l) => l.map((i) => (i.id === item.id ? item : i)));
      }}
      onRemoved={spies.onRemoved}
    />
  ));
  return spies;
}

const currentImage = () => document.querySelector<HTMLImageElement>(".pv-slot .pv-img")!;

describe("the Edit action in the viewer", () => {
  it("opens the editor from the toolbar and from the E key", async () => {
    open([galleryItem("a")]);
    expect(screen.queryByRole("dialog", { name: "Edit photo" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    expect(await screen.findByRole("dialog", { name: "Edit photo" })).toBeInTheDocument();
    expect(editorProps[0]!.item.id).toBe("a");
    fireEvent.click(screen.getByRole("button", { name: "close editor" }));
    expect(screen.queryByRole("dialog", { name: "Edit photo" })).toBeNull();
    fireEvent.keyDown(window, { key: "e" });
    expect(await screen.findByRole("dialog", { name: "Edit photo" })).toBeInTheDocument();
  });

  it("is only offered for photos outside the Trash", () => {
    open([galleryItem("v", { kind: "video", mime: "video/mp4" })]);
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    fireEvent.keyDown(window, { key: "e" });
    expect(screen.queryByRole("dialog", { name: "Edit photo" })).toBeNull();
  });

  it("leaves photos in the Trash alone", () => {
    open([galleryItem("a", { trashedAt: "2026-10-01T00:00:00Z" })], true);
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    fireEvent.keyDown(window, { key: "e" });
    expect(screen.queryByRole("dialog", { name: "Edit photo" })).toBeNull();
  });

  it("keeps the viewer's shortcuts away from the editor", async () => {
    const { onClose } = open([galleryItem("a")]);
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    await screen.findByRole("dialog", { name: "Edit photo" });
    for (const key of ["f", "r", "Delete", "Escape", "i"]) fireEvent.keyDown(window, { key });
    expect(patchItem).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog", { name: /Move to Trash/ })).toBeNull();
  });

  it("shows the saved edit at once: new picture, the flag, and the second download", async () => {
    const { onChanged } = open([galleryItem("a")]);
    const before = currentImage().getAttribute("src");
    expect(screen.queryByRole("link", { name: "Download edited" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.click(await screen.findByRole("button", { name: "save editor" }));
    expect(screen.queryByRole("dialog", { name: "Edit photo" })).toBeNull();
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(onChanged.mock.calls[0]![0]).toMatchObject({ id: "a", edited: true });
    await waitFor(() => expect(currentImage().getAttribute("src")).toBe("a-edited-large"));
    expect(currentImage().getAttribute("src")).not.toBe(before);
    // The original is still what "Download" gives; the edit has its own link.
    expect(screen.getByRole("link", { name: "Download" })).toHaveAttribute("href", "/v1/media/items/a/download");
    expect(screen.getByRole("link", { name: "Download edited" })).toHaveAttribute("href", "a-edited-download");
    expect(screen.getByRole("button", { name: "Edit (already edited)" })).toBeInTheDocument();
  });
});
