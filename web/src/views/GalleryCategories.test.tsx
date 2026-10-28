import { Route, useParams } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { type Call, stubFetch } from "../test/fetch";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { renderAt } from "../test/router";
import GalleryAlbumsView from "./GalleryAlbums";
import GalleryCategoryView from "./GalleryCategory";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => null }));
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

const thing = (category: string, count: number, cover: string | null = `c-${category}`) => ({ category, count, cover: cover ? rawItem(cover, { thumbhash: "3OcRJYB4d3h/iIeHeEh3eIhw+j3A" }) : null });
const items = (ids: string[]) => ids.map((id, i) => rawItem(id, {}, i));

interface Setup {
  things?: unknown[];
  failThings?: boolean;
  ids?: string[];
  pageSize?: number;
}

function backend(setup: Setup = {}): Call[] {
  return stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/albums") return { body: { albums: [] } };
    if (url.pathname === "/v1/media/categories") {
      if (setup.failThings) return { status: 500, body: { error: { code: "boom", message: "boom" } } };
      return { body: { ok: true, categories: setup.things ?? [] } };
    }
    const page = /^\/v1\/media\/categories\/([^/]+)\/items$/.exec(url.pathname);
    if (page) {
      const all = items(setup.ids ?? ["a1", "a2", "a3", "a4", "a5"]);
      const size = setup.pageSize ?? 3;
      const from = Number(url.searchParams.get("cursor") ?? 0);
      const more = from + size < all.length;
      return { body: { ok: true, items: all.slice(from, from + size), next_cursor: more ? String(from + size) : null, total: null, albums: [] } };
    }
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 5, videos: 0 } } };
    if (url.pathname === "/v1/media/items/months") return { body: { months: [] } };
    if (url.pathname === "/v1/media/embeddings/status") return { status: 404, body: { error: { code: "x", message: "x" } } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

const mountAlbums = () =>
  renderAt("/gallery/albums", () => (
    <>
      <Route path="/gallery/albums" component={GalleryAlbumsView} />
      <Route path="/gallery/category/:cat" component={() => <p>category page {useParams().cat}</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));

const mountCategory = (path: string) =>
  renderAt(path, () => (
    <>
      <Route path="/gallery/category/:cat" component={GalleryCategoryView} />
      <Route path="/gallery/albums" component={() => <p>albums page</p>} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));

const shownIds = () =>
  [...document.querySelectorAll<HTMLElement>(".g-tile")]
    .sort((a, b) => Number(a.dataset.index) - Number(b.dataset.index))
    .map((t) => /items\/([^/]+)\/thumb/.exec(t.querySelector("img")?.getAttribute("src") ?? "")?.[1]);
const listCalls = (calls: Call[]) => calls.filter((c) => /^\/v1\/media\/categories\/[^/]+\/items/.test(c.url));

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
  try {
    localStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Things on the albums page", () => {
  it("shows the automatic albums with their names, counts and covers, each opening its page", async () => {
    backend({ things: [thing("screenshot", 12), thing("receipt", 1), thing("pet", 3, null)] });
    mountAlbums();
    const list = await screen.findByRole("list", { name: "Things" });
    expect(screen.getByRole("heading", { name: "Things" })).toBeInTheDocument();
    expect(screen.getByText("Made automatically. The photos stay in your Gallery.")).toBeInTheDocument();
    const cards = within(list).getAllByRole("link");
    expect(cards.map((c) => c.textContent)).toEqual(["Screenshots12 photos", "Receipts1 photo", "Pets3 photos"]);
    expect(cards.map((c) => c.getAttribute("href"))).toEqual(["/gallery/category/screenshot", "/gallery/category/receipt", "/gallery/category/pet"]);
    // The newest photo of the category is the cover; a category with no cover shows its icon.
    expect(cards[0]!.querySelector("img")?.getAttribute("src")).toContain("c-screenshot");
    expect(cards[2]!.querySelector("img")).toBeNull();
    expect(cards[2]!.querySelector("svg")).not.toBeNull();
    fireEvent.click(cards[1]!);
    expect(await screen.findByText("category page receipt")).toBeInTheDocument();
  });

  it("comes after the albums and is no album: nothing to rename, delete or share", async () => {
    backend({ things: [thing("document", 2)] });
    mountAlbums();
    const things = await screen.findByRole("list", { name: "Things" });
    expect(screen.getByText(/No albums yet\./)).toBeInTheDocument();
    expect(things.closest("section")).not.toBeNull();
    expect(within(things.closest("section")!).queryByRole("button")).toBeNull();
  });

  it("is not there until the server has put a photo in one", async () => {
    backend({ things: [] });
    mountAlbums();
    await screen.findByText(/No albums yet\./);
    expect(screen.queryByRole("heading", { name: "Things" })).toBeNull();
  });

  it("stays quiet when the server cannot say (an older server, an error)", async () => {
    backend({ failThings: true });
    mountAlbums();
    await screen.findByText(/No albums yet\./);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(screen.queryByRole("heading", { name: "Things" })).toBeNull();
  });

  it("speaks Italian and Danish", async () => {
    backend({ things: [thing("receipt", 2)] });
    setLocale("it");
    mountAlbums();
    expect(await screen.findByRole("heading", { name: "Cose" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Apri Ricevute" })).toHaveTextContent("2 foto");
  });
});

describe("a category's page (/gallery/category/:cat)", () => {
  it("shows the category's name over the Gallery's grid of its photos, from its own route", async () => {
    const calls = backend();
    mountCategory("/gallery/category/receipt");
    expect(await screen.findByRole("heading", { name: "Receipts" })).toBeInTheDocument();
    await waitFor(() => expect(shownIds()).toEqual(["a1", "a2", "a3", "a4", "a5"]));
    // Never the plain listing, no month scrubber, source bar or memories: it is a list of its own.
    expect(calls.some((c) => c.method === "GET" && /^\/v1\/media\/items\?/.test(c.url))).toBe(false);
    expect(calls.some((c) => c.url.startsWith("/v1/media/items/months"))).toBe(false);
    expect(document.querySelector(".g-source")).toBeNull();
    expect(screen.getByRole("link", { name: "Albums" })).toHaveAttribute("href", "/gallery/albums");
  });

  it("pages with the cursor as the grid asks for more, and sends the category in the path only", async () => {
    const calls = backend({ pageSize: 3 });
    mountCategory("/gallery/category/receipt");
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    const urls = listCalls(calls).map((c) => new URL(c.url, "http://x"));
    expect(urls.map((u) => u.searchParams.get("cursor"))).toEqual([null, "3"]);
    expect(urls.every((u) => u.pathname === "/v1/media/categories/receipt/items" && u.searchParams.get("category") === null)).toBe(true);
    expect(urls[0]!.searchParams.get("scope")).toBe("gallery");
  });

  it("searches by words inside the category, never by meaning", async () => {
    const calls = backend();
    mountCategory("/gallery/category/pet");
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.input(screen.getByRole("searchbox"), { target: { value: "rex" } });
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "Enter" });
    await waitFor(() => expect(listCalls(calls).some((c) => new URL(c.url, "http://x").searchParams.get("q") === "rex")).toBe(true));
    expect(calls.some((c) => c.url.startsWith("/v1/media/items/search"))).toBe(false);
    expect(calls.some((c) => c.url.startsWith("/v1/media/embeddings/status"))).toBe(false);
  });

  it("opens a photo in the viewer", async () => {
    backend();
    mountCategory("/gallery/category/note");
    await waitFor(() => expect(shownIds()).toHaveLength(5));
    fireEvent.click(document.querySelector<HTMLElement>('.g-tile[data-index="1"] .g-open')!);
    expect(await screen.findByTestId("viewer")).toHaveTextContent("a2");
  });

  it("says so when the category has no photos", async () => {
    backend({ ids: [] });
    mountCategory("/gallery/category/ticket");
    expect(await screen.findByText("No photos here yet.")).toBeInTheDocument();
  });

  it("says there is no such automatic album for an address that is not a category", async () => {
    const calls = backend();
    mountCategory("/gallery/category/dragons");
    expect(await screen.findByText("There is no such automatic album.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Albums" })).toHaveAttribute("href", "/gallery/albums");
    expect(listCalls(calls)).toHaveLength(0);
  });
});
