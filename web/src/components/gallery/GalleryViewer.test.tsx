import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import type { GalleryItem } from "../../api/gallery";
import { setLocale } from "../../i18n";
import { setImageCoreForTests } from "../../lib/imageCore";
import { details, exif, galleryItem } from "../../test/galleryFixtures";
import type { LucinaMapProps } from "../LucinaMap";
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

// jsdom has no WebGL: the small map is a stub that records its pins.
const maps = vi.hoisted(() => [] as LucinaMapProps[]);
vi.mock("../LucinaMap", () => ({
  LucinaMap: (props: LucinaMapProps) => {
    maps.push(props);
    return <div data-testid="mini-map" />;
  },
}));

const { getDetails, patchItem, batch, listAlbums, listSources } = vi.mocked(api);

beforeAll(() => setImageCoreForTests(null));

interface Opts {
  items?: GalleryItem[];
  start?: number;
  trash?: boolean;
  /** The parent applies onChanged / onRemoved to its list (the grid does). */
  apply?: boolean;
}

function open(opts: Opts = {}) {
  const [list, setList] = createSignal(opts.items ?? [galleryItem("a"), galleryItem("b")]);
  const [index, setIndex] = createSignal(opts.start ?? 0);
  const spies = { onClose: vi.fn(), onChanged: vi.fn(), onRemoved: vi.fn() };
  const apply = opts.apply !== false;
  render(() => (
    <GalleryViewer
      items={list()}
      index={index()}
      onIndex={setIndex}
      onClose={spies.onClose}
      trash={opts.trash}
      onChanged={(item) => {
        spies.onChanged(item);
        if (apply) setList((l) => l.map((i) => (i.id === item.id ? item : i)));
      }}
      onRemoved={(id) => {
        spies.onRemoved(id);
        if (apply) setList((l) => l.filter((i) => i.id !== id));
      }}
    />
  ));
  return { ...spies, index, list };
}

const key = (k: string, target: Element | Window = window) => fireEvent.keyDown(target, { key: k });
const button = (name: string | RegExp) => screen.getByRole("button", { name });
const currentImage = () => document.querySelector<HTMLImageElement>(".pv-slot .pv-img")!;

beforeEach(() => {
  setLocale("en");
  maps.length = 0;
  resetPhotoInfoCaches();
  for (const fn of [getDetails, patchItem, batch, listAlbums, listSources]) fn.mockReset();
  getDetails.mockImplementation(async (id) => details(galleryItem(id)));
  listAlbums.mockResolvedValue([]);
  listSources.mockResolvedValue({ sources: [], all: { photos: 0, videos: 0 } });
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});

describe("GalleryViewer", () => {
  it("shows the Gallery's picture in the shared viewer with its actions", () => {
    open();
    expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
    expect(currentImage().getAttribute("src")).toBe("/v1/media/items/a/r/large");
    const toolbar = screen.getByRole("toolbar", { name: "Photo actions" });
    for (const name of ["Info", "Add to favorites", "Rotate", "Rename", "Add to album", "Download", "Move to Trash"]) {
      expect(within(toolbar).getByRole(name === "Download" ? "link" : "button", { name })).toBeInTheDocument();
    }
    expect(within(toolbar).getByRole("link", { name: "Download" })).toHaveAttribute("href", "/v1/media/items/a/download");
    expect(screen.getByRole("button", { name: "Share" })).toBeInTheDocument();
  });

  describe("info panel", () => {
    it("opens with the button and the I key, and closes the same ways", async () => {
      open();
      expect(screen.queryByRole("region", { name: "Info" })).toBeNull();
      fireEvent.click(button("Info"));
      expect(screen.getByRole("region", { name: "Info" })).toBeInTheDocument();
      expect(document.querySelector(".pv")).toHaveClass("pv--panel");
      key("i");
      expect(screen.queryByRole("region", { name: "Info" })).toBeNull();
      key("I");
      fireEvent.click(await screen.findByRole("button", { name: "Close info" }));
      expect(screen.queryByRole("region", { name: "Info" })).toBeNull();
      expect(document.querySelector(".pv")).not.toHaveClass("pv--panel");
    });

    it("fills the fields from the item and its details", async () => {
      const item = galleryItem("a", {
        caption: "Harbour at dusk",
        albums: ["trip", "gone"],
        place: { name: "Aarhus", admin1: "Central Denmark", country: "Denmark", lat: 56.15, lon: 10.2 },
        origin: { filename: "IMG_0001.JPG", folder: "DCIM/2019/Summer", importedFrom: "DCIM/2019/Summer" },
      });
      getDetails.mockResolvedValue(details(item, { exif: exif({ offset: "+02:00" }), people: [{ id: "p_alice", name: "Alice" }, { id: "p_bob", name: "Bob" }] }));
      listAlbums.mockResolvedValue([{ id: "trip", title: "Summer trip", count: 3, system: false }]);
      listSources.mockResolvedValue({
        sources: [{ id: "device:dev-1", label: "Test iPhone", kind: "iphone", photos: 1, videos: 0, latestTakenAt: null }],
        all: { photos: 1, videos: 0 },
      });
      open({ items: [item] });
      key("i");
      const panel = screen.getByRole("region", { name: "Info" });

      // date taken with its offset
      expect(await within(panel).findByText("UTC+02:00")).toBeInTheDocument();
      expect(panel).toHaveTextContent("2026");
      // device name + kind
      expect(within(panel).getByText("Test iPhone")).toBeInTheDocument();
      expect(await within(panel).findByText("iPhone")).toBeInTheDocument();
      // camera / lens / settings
      expect(within(panel).getByText("Apple iPhone 15")).toBeInTheDocument();
      expect(within(panel).getByText("ISO 1600 · 1/250 s · f/1.8 · 7 mm")).toBeInTheDocument();
      expect(within(panel).getByText("Main camera 6.9mm")).toBeInTheDocument();
      // file
      expect(within(panel).getByText("2.4 MB")).toBeInTheDocument();
      expect(within(panel).getByText("4000 × 3000 · 12 MP")).toBeInTheDocument();
      expect(within(panel).getByText("IMG_0001.JPG")).toBeInTheDocument();
      expect(within(panel).getByText("DCIM/2019/Summer")).toBeInTheDocument();
      // place + map
      expect(within(panel).getByText("Aarhus, Central Denmark, Denmark")).toBeInTheDocument();
      expect(screen.getByTestId("mini-map")).toBeInTheDocument();
      expect(maps[0]?.pins[0]).toMatchObject({ lat: 56.15, lng: 10.2 });
      expect(within(panel).getByRole("link", { name: "Open in map" }).getAttribute("href")).toMatch(/^\/map\/place\?lat=56\.15&lng=10\.2&title=Aarhus/);
      // albums by title (an unknown album is left out), people
      expect(await within(panel).findByText("Summer trip")).toBeInTheDocument();
      expect(within(panel).queryByText("gone")).toBeNull();
      expect(within(panel).getByText("Alice")).toBeInTheDocument();
      expect(within(panel).getByText("Bob")).toBeInTheDocument();
      // caption
      expect(within(panel).getByLabelText("Caption")).toHaveValue("Harbour at dusk");
      expect(getDetails).toHaveBeenCalledWith("a");
    });

    it("still shows what the list knows when the details cannot be loaded, and has no map without a place", async () => {
      getDetails.mockRejectedValue(new Error("offline"));
      open();
      key("i");
      const panel = screen.getByRole("region", { name: "Info" });
      expect(await within(panel).findByText("Some details couldn't be loaded.")).toBeInTheDocument();
      expect(within(panel).getByText("IMG_a.JPG")).toBeInTheDocument();
      expect(within(panel).getByText("Not in any album")).toBeInTheDocument();
      expect(screen.queryByTestId("mini-map")).toBeNull();
    });

    it("follows the picture when you move to the next one", async () => {
      getDetails.mockImplementation(async (id) => details(galleryItem(id)));
      open();
      key("i");
      expect(await screen.findByText("IMG_a.JPG")).toBeInTheDocument();
      key("ArrowRight");
      expect(await screen.findByText("IMG_b.JPG")).toBeInTheDocument();
      expect(getDetails).toHaveBeenCalledWith("b");
    });

    it("says how long a trashed picture has left", async () => {
      const soon = new Date(Date.now() + 5 * 86_400_000 - 3_600_000).toISOString();
      open({ trash: true, items: [galleryItem("a", { trashedAt: "2026-10-01T00:00:00Z", trashExpiresAt: soon })] });
      key("i");
      expect(await screen.findByText("In the Trash. Removed for good in 5 days.")).toBeInTheDocument();
    });

    it("saves an edited caption with the caption button and keeps the draft when it fails", async () => {
      const saved = galleryItem("a", { caption: "New words" });
      patchItem.mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce(saved);
      const { onChanged } = open();
      key("i");
      const box = await screen.findByLabelText("Caption");
      expect(screen.queryByRole("button", { name: "Save caption" })).toBeNull();
      fireEvent.input(box, { target: { value: "  New words " } });
      fireEvent.click(button("Save caption"));
      expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't save the change");
      expect(box).toHaveValue("  New words ");
      expect(onChanged).not.toHaveBeenCalled();
      fireEvent.click(button("Save caption"));
      await waitFor(() => expect(onChanged).toHaveBeenCalledWith(saved));
      expect(patchItem).toHaveBeenLastCalledWith("a", { caption: "New words" });
      await waitFor(() => expect(screen.queryByRole("button", { name: "Save caption" })).toBeNull());
    });
  });

  describe("actions", () => {
    it("marks a favorite at once, saves it, and reports the server's item", async () => {
      const saved = galleryItem("a", { favorite: true });
      let release!: () => void;
      patchItem.mockReturnValue(new Promise((resolve) => (release = () => resolve(saved))));
      const { onChanged } = open();
      const heart = button("Add to favorites");
      expect(heart).toHaveAttribute("aria-pressed", "false");
      fireEvent.click(heart);
      // optimistic: pressed before the server answered
      expect(button("Remove from favorites")).toHaveAttribute("aria-pressed", "true");
      expect(patchItem).toHaveBeenCalledWith("a", { favorite: true });
      expect(onChanged).not.toHaveBeenCalled();
      release();
      await waitFor(() => expect(onChanged).toHaveBeenCalledWith(saved));
      expect(button("Remove from favorites")).toHaveAttribute("aria-pressed", "true");
    });

    it("keeps the change when the parent does not apply it (the viewer holds the saved state itself)", async () => {
      patchItem.mockResolvedValue(galleryItem("a", { favorite: true }));
      const { onChanged } = open({ apply: false });
      key("f");
      await waitFor(() => expect(onChanged).toHaveBeenCalled());
      expect(button("Remove from favorites")).toHaveAttribute("aria-pressed", "true");
      key("f");
      patchItem.mockResolvedValue(galleryItem("a", { favorite: false }));
      await waitFor(() => expect(button("Add to favorites")).toBeInTheDocument());
    });

    it("rolls a favorite back and shows an error when the server refuses", async () => {
      patchItem.mockRejectedValue(new Error("500"));
      const { onChanged } = open();
      key("F");
      expect(button("Remove from favorites")).toBeInTheDocument();
      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent("Couldn't save the change. Try again.");
      expect(button("Add to favorites")).toHaveAttribute("aria-pressed", "false");
      expect(onChanged).not.toHaveBeenCalled();
      fireEvent.click(within(alert).getByRole("button", { name: "Dismiss" }));
      expect(screen.queryByRole("alert")).toBeNull();
    });

    it("rotates a quarter turn per R, draws it and saves it, and rolls back on failure", async () => {
      patchItem.mockImplementation(async (id, edit) => galleryItem(id, { rotation: edit.rotation }));
      const { onChanged } = open();
      expect(currentImage().style.transform).toBe("");
      key("r");
      expect(currentImage().style.transform).toBe("rotate(90deg)");
      await waitFor(() => expect(onChanged).toHaveBeenCalledWith(expect.objectContaining({ id: "a", rotation: 90 })));
      expect(patchItem).toHaveBeenCalledWith("a", { rotation: 90 });
      fireEvent.click(button("Rotate"));
      expect(currentImage().style.transform).toBe("rotate(180deg)");
      await waitFor(() => expect(patchItem).toHaveBeenLastCalledWith("a", { rotation: 180 }));

      patchItem.mockRejectedValue(new Error("500"));
      await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(2));
      key("r");
      await screen.findByRole("alert");
      expect(currentImage().style.transform).toBe("rotate(180deg)");
    });

    it("answers quick rotations in order and ends on the server's last answer", async () => {
      patchItem.mockImplementation(async (id, edit) => galleryItem(id, { rotation: edit.rotation }));
      const { onChanged } = open({ apply: false });
      key("r");
      key("r");
      key("r");
      expect(currentImage().style.transform).toBe("rotate(270deg)");
      await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(3));
      expect(patchItem.mock.calls.map((c) => c[1])).toEqual([{ rotation: 90 }, { rotation: 180 }, { rotation: 270 }]);
      expect(currentImage().style.transform).toBe("rotate(270deg)");
    });

    it("does not rotate a video", () => {
      open({ items: [galleryItem("v", { kind: "video", mime: "video/mp4", urls: { play: "/v/play", original: "/v/original" } })] });
      expect(screen.queryByRole("button", { name: "Rotate" })).toBeNull();
      key("r");
      expect(patchItem).not.toHaveBeenCalled();
    });

    it("renames: the title and the caption go in one save", async () => {
      patchItem.mockResolvedValue(galleryItem("a", { title: "Harbour", caption: "At dusk" }));
      const { onChanged } = open();
      fireEvent.click(button("Rename"));
      const dialog = screen.getByRole("dialog", { name: "Rename" });
      const name = within(dialog).getByLabelText("Name");
      expect(name).toHaveValue("a.jpg");
      fireEvent.input(name, { target: { value: " Harbour " } });
      fireEvent.input(within(dialog).getByLabelText("Caption"), { target: { value: "At dusk" } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
      await waitFor(() => expect(onChanged).toHaveBeenCalled());
      expect(patchItem).toHaveBeenCalledWith("a", { title: "Harbour", caption: "At dusk" });
      await waitFor(() => expect(screen.queryByRole("dialog", { name: "Rename" })).toBeNull());
      expect(document.querySelector(".pv-name")).toHaveTextContent("Harbour");
    });

    it("keeps the rename dialog open with an error when saving fails", async () => {
      patchItem.mockRejectedValue(new Error("500"));
      open();
      fireEvent.click(button("Rename"));
      const dialog = screen.getByRole("dialog", { name: "Rename" });
      fireEvent.input(within(dialog).getByLabelText("Name"), { target: { value: "Harbour" } });
      fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
      await waitFor(() => expect(within(dialog).getAllByRole("alert").length).toBeGreaterThan(0));
      expect(screen.getByRole("dialog", { name: "Rename" })).toBeInTheDocument();
      expect(document.querySelector(".pv-name")).toHaveTextContent("a.jpg");
    });

    it("adds to an album and shows it in the item's albums", async () => {
      listAlbums.mockResolvedValue([{ id: "trip", title: "Summer trip", count: 3, system: false }]);
      batch.mockResolvedValue({ done: ["a"], failed: [] });
      const { onChanged } = open();
      fireEvent.click(button("Add to album"));
      fireEvent.click(await screen.findByRole("button", { name: /Summer trip/ }));
      await waitFor(() => expect(onChanged).toHaveBeenCalledWith(expect.objectContaining({ id: "a", albums: ["trip"] })));
      expect(batch).toHaveBeenCalledWith("album_add", ["a"], { album: "trip" });
      expect(screen.queryByRole("dialog", { name: "Add to album" })).toBeNull();
    });

    it("moves to the Trash through the dialog, then drops the picture and shows the next", async () => {
      batch.mockResolvedValue({ done: ["a"], failed: [] });
      const { onRemoved, index } = open();
      key("Delete");
      const dialog = screen.getByRole("dialog", { name: "Move to Trash?" });
      expect(dialog).toHaveTextContent("The originals stay on Test iPhone.");
      fireEvent.click(within(dialog).getByRole("button", { name: "Move to Trash" }));
      await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
      expect(batch).toHaveBeenCalledWith("trash", ["a"], { alsoFromDevice: false });
      expect(screen.queryByRole("dialog", { name: "Move to Trash?" })).toBeNull();
      expect(index()).toBe(0);
      expect(screen.getByRole("dialog", { name: "Photo 1 of 1" })).toBeInTheDocument();
      expect(document.querySelector(".pv-name")).toHaveTextContent("b.jpg");
    });

    it("steps back when the last picture is deleted, and closes when none is left", async () => {
      batch.mockResolvedValue({ done: ["b"], failed: [] });
      const { index, onClose } = open({ start: 1 });
      fireEvent.click(button("Move to Trash"));
      fireEvent.click(within(screen.getByRole("dialog", { name: "Move to Trash?" })).getByRole("button", { name: "Move to Trash" }));
      await waitFor(() => expect(index()).toBe(0));
      expect(onClose).not.toHaveBeenCalled();
      batch.mockResolvedValue({ done: ["a"], failed: [] });
      key("Backspace");
      fireEvent.click(within(screen.getByRole("dialog", { name: "Move to Trash?" })).getByRole("button", { name: "Move to Trash" }));
      await waitFor(() => expect(onClose).toHaveBeenCalled());
    });

    it("leaves the picture in place when the delete fails", async () => {
      batch.mockRejectedValue(new Error("offline"));
      const { onRemoved } = open();
      key("Delete");
      fireEvent.click(within(screen.getByRole("dialog", { name: "Move to Trash?" })).getByRole("button", { name: "Move to Trash" }));
      expect(await screen.findByText(/Couldn't finish/)).toBeInTheDocument();
      expect(onRemoved).not.toHaveBeenCalled();
      fireEvent.click(within(screen.getByRole("dialog", { name: "Move to Trash?" })).getByRole("button", { name: "Cancel" }));
      expect(screen.queryByRole("dialog", { name: "Move to Trash?" })).toBeNull();
      expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
    });
  });

  describe("keyboard", () => {
    it("leaves the shortcuts alone while typing in the caption, and Esc there only reverts it", async () => {
      const { onClose } = open();
      key("i");
      const box = await screen.findByLabelText("Caption");
      box.focus();
      fireEvent.input(box, { target: { value: "draft" } });
      key("f", box);
      key("r", box);
      key("Delete", box);
      key("ArrowRight", box);
      expect(patchItem).not.toHaveBeenCalled();
      expect(screen.queryByRole("dialog", { name: "Move to Trash?" })).toBeNull();
      expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
      key("Escape", box);
      expect(box).toHaveValue("");
      expect(onClose).not.toHaveBeenCalled();
    });

    it("lets a dialog own the keyboard: Esc closes the dialog, not the viewer", () => {
      const { onClose } = open();
      key("Delete");
      const dialog = screen.getByRole("dialog", { name: "Move to Trash?" });
      key("ArrowRight", dialog);
      key("f", dialog);
      expect(screen.getByRole("dialog", { name: "Photo 1 of 2" })).toBeInTheDocument();
      expect(patchItem).not.toHaveBeenCalled();
      key("Escape", dialog);
      expect(screen.queryByRole("dialog", { name: "Move to Trash?" })).toBeNull();
      expect(onClose).not.toHaveBeenCalled();
      // With no dialog Esc closes the viewer again.
      key("Escape");
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it("still pages with the arrows and ignores shortcuts with a modifier", () => {
      open();
      fireEvent.keyDown(window, { key: "f", metaKey: true });
      fireEvent.keyDown(window, { key: "r", ctrlKey: true });
      expect(patchItem).not.toHaveBeenCalled();
      key("ArrowRight");
      expect(screen.getByRole("dialog", { name: "Photo 2 of 2" })).toBeInTheDocument();
    });
  });

  describe("the Trash", () => {
    const trashed = (id: string) => galleryItem(id, { trashedAt: "2026-10-20T00:00:00Z", trashExpiresAt: "2026-11-19T00:00:00Z" });

    it("offers Restore and Delete forever instead of the edit actions", () => {
      open({ trash: true, items: [trashed("a")] });
      const toolbar = screen.getByRole("toolbar");
      expect(within(toolbar).getByRole("button", { name: "Restore" })).toBeInTheDocument();
      expect(within(toolbar).getByRole("button", { name: "Delete forever" })).toBeInTheDocument();
      for (const name of ["Add to favorites", "Rotate", "Rename", "Add to album", "Move to Trash"]) {
        expect(within(toolbar).queryByRole("button", { name })).toBeNull();
      }
      key("f");
      key("r");
      expect(patchItem).not.toHaveBeenCalled();
    });

    it("restores and drops the picture from the Trash list", async () => {
      batch.mockResolvedValue({ done: ["a"], failed: [] });
      const { onRemoved } = open({ trash: true, items: [trashed("a"), trashed("b")] });
      fireEvent.click(button("Restore"));
      await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
      expect(batch).toHaveBeenCalledWith("restore", ["a"]);
    });

    it("keeps it and says so when the restore fails", async () => {
      batch.mockResolvedValue({ done: [], failed: [{ id: "a", error: "gone" }] });
      const { onRemoved } = open({ trash: true, items: [trashed("a")] });
      fireEvent.click(button("Restore"));
      expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't restore");
      expect(onRemoved).not.toHaveBeenCalled();
    });

    it("deletes forever only after the stronger confirmation", async () => {
      batch.mockResolvedValue({ done: ["a"], failed: [] });
      const { onRemoved } = open({ trash: true, items: [trashed("a"), trashed("b")] });
      key("Delete");
      const dialog = screen.getByRole("dialog", { name: "Delete forever?" });
      expect(dialog).toHaveTextContent("It can't be undone");
      const confirm = within(dialog).getByRole("button", { name: "Delete forever" });
      expect(confirm).toBeDisabled();
      fireEvent.click(within(dialog).getByRole("checkbox"));
      fireEvent.click(confirm);
      await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("a"));
      expect(batch).toHaveBeenCalledWith("purge", ["a"]);
    });
  });
});
