import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryAlbumView from "./GalleryAlbum";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; onClose: () => void }) => (
    <div data-testid="viewer">
      <span>{p.items[p.index]?.id}</span>
      <button type="button" onClick={p.onClose}>
        close viewer
      </button>
    </div>
  ),
}));
vi.mock("../components/gallery/share/ShareSheet", () => ({
  ShareSheet: (p: { target: { kind: string; id?: string; title?: string }; onClose: () => void }) => (
    <div role="dialog" aria-label="share sheet" data-kind={p.target.kind} data-album={p.target.id} data-title={p.target.title}>
      <button type="button" onClick={p.onClose}>
        close share
      </button>
    </div>
  ),
}));

const IDS = ["p1", "p2", "p3", "p4"];
const album = (over: Record<string, unknown> = {}) => ({
  id: "alb_1",
  title: "Holiday",
  system_key: null,
  cover_item: null,
  count: 4,
  created_at: "2026-10-01T10:00:00Z",
  updated_at: "2026-10-02T10:00:00Z",
  ...over,
});

interface Backend {
  calls: Call[];
  lists: URLSearchParams[];
  batches: { op: string; ids: string[]; album?: string }[];
}

function backend(opts: { album?: unknown; missing?: boolean; empty?: boolean } = {}): Backend {
  const b: Backend = { calls: [], lists: [], batches: [] };
  let current = opts.album ?? album();
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/albums/alb_1") {
      if (opts.missing) return { status: 404, body: { error: { code: "not_found", message: "no" } } };
      if (call.method === "PATCH") current = { ...(current as object), title: (call.body as { title: string }).title };
      if (call.method === "DELETE") return { status: 204 };
      return { body: current };
    }
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 4, videos: 0 } } };
    if (url.pathname === "/v1/media/items/batch") {
      const body = call.body as Backend["batches"][number];
      b.batches.push(body);
      return { body: { done: body.ids, failed: [] } };
    }
    if (url.pathname === "/v1/media/items" && call.method === "GET") {
      b.lists.push(url.searchParams);
      const rows = url.searchParams.get("album") === "alb_1" && !opts.empty ? IDS : [];
      return { body: { items: rows.map((id, i) => rawItem(id, { albums: ["alb_1"] }, i)), next_cursor: null } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function mount(path = "/gallery/album/alb_1") {
  return renderAt(path, () => (
    <>
      <Route path="/gallery/album/:id" component={GalleryAlbumView} />
      <Route path="/gallery/albums" component={() => <p>albums page</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));
}

const tiles = () => document.querySelectorAll<HTMLElement>(".g-tile");
const open = (index: number) => document.querySelector<HTMLElement>(`.g-tile[data-index="${index}"] .g-open`)!;

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("an album's view (/gallery/album/:id)", () => {
  it("is the Gallery's grid filtered by the album, with the album's title and a way back to the albums", async () => {
    const b = backend();
    mount();
    expect(await screen.findByRole("heading", { name: "Holiday" })).toBeInTheDocument();
    await waitFor(() => expect(tiles()).toHaveLength(4));
    const list = b.lists[0]!;
    expect(list.get("album")).toBe("alb_1");
    expect(list.get("origin")).toBe("all");
    expect(screen.getByRole("link", { name: "Albums" })).toHaveAttribute("href", "/gallery/albums");
    // It asks for no month scrubber of the whole library and has no source bar or memories.
    expect(b.calls.some((c) => c.url.startsWith("/v1/media/items/months"))).toBe(false);
    expect(document.querySelector(".g-source")).toBeNull();
  });

  it("opens a photo in the viewer", async () => {
    backend();
    mount();
    await waitFor(() => expect(tiles()).toHaveLength(4));
    fireEvent.click(open(1));
    expect(screen.getByTestId("viewer")).toHaveTextContent("p2");
  });

  it("says when the album has no photos and when it is gone", async () => {
    backend({ album: album({ count: 0 }), empty: true });
    const { unmount } = mount();
    expect(await screen.findByText(/No photos in this album yet/)).toBeInTheDocument();
    unmount();
    backend({ missing: true, empty: true });
    mount();
    expect(await screen.findByText("This album doesn't exist any more.")).toBeInTheDocument();
  });

  it("renames the album", async () => {
    const b = backend();
    mount();
    await screen.findByRole("heading", { name: "Holiday" });
    fireEvent.click(screen.getByRole("button", { name: "Rename album" }));
    const dialog = await screen.findByRole("dialog");
    const input = within(dialog).getByLabelText("Name") as HTMLInputElement;
    expect(input.value).toBe("Holiday");
    fireEvent.input(input, { target: { value: "Summer 2026" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("heading", { name: "Summer 2026" })).toBeInTheDocument();
    expect(b.calls.find((c) => c.method === "PATCH")?.body).toEqual({ title: "Summer 2026" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("deletes the album after a confirmation that says the photos stay, then goes to the albums", async () => {
    const b = backend();
    mount();
    await screen.findByRole("heading", { name: "Holiday" });
    fireEvent.click(screen.getByRole("button", { name: "Delete album" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("The photos stay in your Gallery");
    expect(b.calls.some((c) => c.method === "DELETE")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete album" }));
    expect(await screen.findByText("albums page")).toBeInTheDocument();
    expect(b.calls.find((c) => c.method === "DELETE")?.url).toBe("/v1/media/albums/alb_1");
    // Only the album went: no batch trash or purge.
    expect(b.batches).toEqual([]);
  });

  it("keeps the album when the confirmation is cancelled", async () => {
    const b = backend();
    mount();
    await screen.findByRole("heading", { name: "Holiday" });
    fireEvent.click(screen.getByRole("button", { name: "Delete album" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(b.calls.some((c) => c.method === "DELETE")).toBe(false);
  });

  it("a built-in album can be shared but not renamed or deleted", async () => {
    backend({ album: album({ title: "Lucina", system_key: "lucina" }) });
    mount();
    await screen.findByRole("heading", { name: "Lucina" });
    expect(screen.getByRole("button", { name: "Share album" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Rename album" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete album" })).toBeNull();
  });

  it("shares the album with the existing share sheet", async () => {
    backend();
    mount();
    await screen.findByRole("heading", { name: "Holiday" });
    fireEvent.click(screen.getByRole("button", { name: "Share album" }));
    const sheet = await screen.findByRole("dialog", { name: "share sheet" });
    expect(sheet).toHaveAttribute("data-kind", "album");
    expect(sheet).toHaveAttribute("data-album", "alb_1");
    expect(sheet).toHaveAttribute("data-title", "Holiday");
  });

  it("removes the selected photos from the album and keeps them in the Gallery", async () => {
    const b = backend();
    mount();
    await waitFor(() => expect(tiles()).toHaveLength(4));
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(2), { ctrlKey: true });
    const bar = await screen.findByRole("toolbar", { name: "Actions for the selected photos" });
    fireEvent.click(within(bar).getByRole("button", { name: "Remove from album" }));
    await waitFor(() => expect(tiles()).toHaveLength(2));
    expect(b.batches).toEqual([{ op: "album_remove", ids: ["p1", "p3"], album: "alb_1" }]);
    expect(screen.getByText("2 photos removed from the album.")).toBeInTheDocument();
  });

  it("offers Remove from album only inside an album", async () => {
    backend();
    mount();
    await waitFor(() => expect(tiles()).toHaveLength(4));
    fireEvent.click(open(0), { ctrlKey: true });
    const bar = await screen.findByRole("toolbar", { name: "Actions for the selected photos" });
    expect(within(bar).getByRole("button", { name: "Add to album" })).toBeInTheDocument();
    expect(within(bar).getByRole("button", { name: "Remove from album" })).toBeInTheDocument();
  });
});
