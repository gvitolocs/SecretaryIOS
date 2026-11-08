import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../../api/gallery";
import { setLocale } from "../../i18n";
import { AlbumPicker } from "./AlbumPicker";

vi.mock("../../api/gallery", async (original) => ({
  ...(await original<typeof import("../../api/gallery")>()),
  batch: vi.fn(),
  listAlbums: vi.fn(),
  createAlbum: vi.fn(),
}));
const { batch, listAlbums, createAlbum } = vi.mocked(api);

const ALBUMS = [
  { id: "fav", title: "Favorites", count: 4, system: true },
  { id: "trip", title: "Trip", count: 12, system: false },
  { id: "pets", title: "Pets", count: 1, system: false },
];

function open(itemIds = ["a"]) {
  const onDone = vi.fn();
  const onCancel = vi.fn();
  render(() => <AlbumPicker itemIds={itemIds} onDone={onDone} onCancel={onCancel} />);
  return { onDone, onCancel };
}

beforeEach(() => {
  setLocale("en");
  for (const fn of [batch, listAlbums, createAlbum]) fn.mockReset();
  listAlbums.mockResolvedValue(ALBUMS);
});

describe("AlbumPicker", () => {
  it("lists the owner's albums (not the built-in ones) with their counts", async () => {
    open(["a", "b", "c"]);
    expect(screen.getByRole("dialog", { name: "Add to album" })).toBeInTheDocument();
    expect(screen.getByText("3 items")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /Trip/ })).toHaveTextContent("12 items");
    expect(screen.getByRole("button", { name: /Pets/ })).toHaveTextContent("1 item");
    expect(screen.queryByRole("button", { name: /Favorites/ })).toBeNull();
  });

  it("adds every item to the picked album in one batch", async () => {
    batch.mockResolvedValue({ done: ["a", "b"], failed: [] });
    const { onDone } = open(["a", "b"]);
    fireEvent.click(await screen.findByRole("button", { name: /Trip/ }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith("trip"));
    expect(batch).toHaveBeenCalledWith("album_add", ["a", "b"], { album: "trip" });
  });

  it("creates an album and adds the items to it", async () => {
    createAlbum.mockResolvedValue({ id: "new", title: "Summer", count: 0, system: false });
    batch.mockResolvedValue({ done: ["a"], failed: [] });
    const { onDone } = open();
    await screen.findByRole("button", { name: /Trip/ });
    const create = screen.getByRole("button", { name: "Create and add" });
    expect(create).toBeDisabled();
    fireEvent.input(screen.getByLabelText("New album"), { target: { value: "  Summer " } });
    fireEvent.click(create);
    await waitFor(() => expect(onDone).toHaveBeenCalledWith("new"));
    expect(createAlbum).toHaveBeenCalledWith("Summer");
    expect(batch).toHaveBeenCalledWith("album_add", ["a"], { album: "new" });
  });

  it("shows an empty state, and an error when the albums cannot load", async () => {
    listAlbums.mockResolvedValueOnce([ALBUMS[0]!]);
    const { unmount } = render(() => <AlbumPicker itemIds={["a"]} onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(await screen.findByText("No albums yet. Create the first one below.")).toBeInTheDocument();
    unmount();
    listAlbums.mockRejectedValueOnce(new Error("offline"));
    render(() => <AlbumPicker itemIds={["a"]} onDone={vi.fn()} onCancel={vi.fn()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the albums.");
  });

  it("keeps the dialog open with an error when adding fails", async () => {
    batch.mockRejectedValueOnce(new Error("offline"));
    const { onDone } = open();
    fireEvent.click(await screen.findByRole("button", { name: /Trip/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't add to the album.");
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /Trip/ })).toBeEnabled();
  });

  it("does not add when the server refuses every item", async () => {
    batch.mockResolvedValueOnce({ done: [], failed: [{ id: "a", error: "not_found" }] });
    const { onDone } = open();
    fireEvent.click(await screen.findByRole("button", { name: /Pets/ }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled();
  });

  it("reuses a created album when adding to it failed, and closes on Esc", async () => {
    createAlbum.mockResolvedValue({ id: "new", title: "Summer", count: 0, system: false });
    batch.mockRejectedValueOnce(new Error("offline"));
    const { onCancel } = open();
    await screen.findByRole("button", { name: /Trip/ });
    listAlbums.mockResolvedValue([...ALBUMS, { id: "new", title: "Summer", count: 0, system: false }]);
    fireEvent.input(screen.getByLabelText("New album"), { target: { value: "Summer" } });
    fireEvent.click(screen.getByRole("button", { name: "Create and add" }));
    expect(await screen.findByRole("button", { name: /Summer/ })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });
});
