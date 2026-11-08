import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { lucinaViewer } from "../lib/desktopViewer";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

// The Gallery's browse hooks: the Places and Archive entries, the Archive view, a Places window in the query, the
// memories strip, and Archive / Unarchive on a selection. The viewer is a stand-in (it has its own tests).
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; archive?: boolean }) => (
    <div data-testid="viewer" data-archive={String(!!p.archive)}>
      {p.items[p.index]?.id}
    </div>
  ),
}));
vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

function raw(id: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    kind: "photo",
    title: `${id}.jpg`,
    mime: "image/jpeg",
    size: 1000,
    taken_at: "2026-10-20T12:00:00Z",
    created_at: "2026-10-20T12:00:00Z",
    width: 4000,
    height: 3000,
    source: "ios-photos",
    device: "iphone-test",
    device_name: "Test iPhone",
    origin: { filename: null, folder: null, imported_from: null },
    favorite: false,
    rotation: 0,
    albums: [],
    thumbhash: null,
    urls: { thumb: `/v1/media/items/${id}/thumb?size=256` },
    ...extra,
  };
}

const SOURCES = {
  sources: [{ id: "device:iphone-test", label: "Test iPhone", kind: "iphone", photos: 6, videos: 0, latest_taken_at: "2026-10-20T12:00:00Z" }],
  all: { photos: 6, videos: 0 },
};

interface Backend {
  calls: Call[];
  lists: URLSearchParams[];
  months: URLSearchParams[];
  batches: { op: string; ids: string[] }[];
}

function backend(opts: { main?: number; archived?: number; years?: unknown[]; recent?: unknown[] } = {}): Backend {
  const b: Backend = { calls: [], lists: [], months: [], batches: [] };
  const main = Array.from({ length: opts.main ?? 6 }, (_, i) => raw(`m${i}`));
  const archived = Array.from({ length: opts.archived ?? 3 }, (_, i) => raw(`z${i}`));
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/sources") return { body: SOURCES };
    if (url.pathname === "/v1/media/items/months") {
      b.months.push(url.searchParams);
      return { body: { months: [{ month: "2026-10", count: 6 }, { month: "2026-09", count: 2 }] } };
    }
    if (url.pathname === "/v1/media/items/on-this-day") return { body: { years: opts.years ?? [] } };
    if (url.pathname === "/v1/media/items/recent") return { body: { items: opts.recent ?? [], total: (opts.recent ?? []).length } };
    if (url.pathname === "/v1/media/items/batch") {
      const body = call.body as { op: string; ids: string[] };
      b.batches.push(body);
      return { body: { done: body.ids, failed: [] } };
    }
    if (url.pathname === "/v1/media/items" && call.method === "GET") {
      b.lists.push(url.searchParams);
      return { body: { items: url.searchParams.get("archived") === "true" ? archived : main, next_cursor: null } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function mount(path: string) {
  return renderAt(path, () => (
    <>
      <Route path="/gallery" component={() => <GalleryView />} />
      <Route path="/gallery/archive" component={() => <GalleryView archive />} />
      <Route path="/gallery/trash" component={() => <GalleryView trash />} />
    </>
  ));
}

const tiles = () => document.querySelectorAll<HTMLElement>(".g-tile");
const last = (b: Backend) => b.lists[b.lists.length - 1]!;

function sizeTheBox() {
  vi.spyOn(Element.prototype, "clientWidth", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? 1000 : 0;
  });
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? 800 : 0;
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
}

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
  try {
    sessionStorage.clear();
    localStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => {
  lucinaViewer.setDevice(null);
  vi.restoreAllMocks();
});

describe("the Gallery's entries", () => {
  it("links to Places, the Archive and the Trash from the header", async () => {
    backend();
    mount("/gallery");
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));
    expect(screen.getByRole("link", { name: "Places" })).toHaveAttribute("href", "/gallery/places");
    expect(screen.getByRole("link", { name: "Archive" })).toHaveAttribute("href", "/gallery/archive");
    expect(screen.getByRole("link", { name: "Trash" })).toHaveAttribute("href", "/gallery/trash");
  });

  it("asks for the main grid without the archive", async () => {
    const b = backend();
    mount("/gallery");
    await waitFor(() => expect(b.lists.length).toBeGreaterThan(0));
    expect(last(b).has("archived")).toBe(false);
    expect(last(b).has("bbox")).toBe(false);
  });
});

describe("the Archive view", () => {
  it("lists the archived pictures of every source, with its own title, note and way back", async () => {
    const b = backend();
    mount("/gallery/archive");
    expect(await screen.findByRole("heading", { name: "Archive" })).toBeInTheDocument();
    await waitFor(() => expect(tiles().length).toBe(3));
    expect(Object.fromEntries(last(b))).toMatchObject({ scope: "gallery", origin: "all", archived: "true" });
    expect(b.months.some((m) => m.get("archived") === "true")).toBe(true);
    expect(screen.getByText("Archived photos are hidden from the Gallery but not deleted.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Gallery" })).toHaveAttribute("href", "/gallery");
    // No source switcher, no memories, no Places/Archive/Trash links on a shelf.
    expect(screen.queryByRole("navigation", { name: "Where the photos come from" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Memories" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Places" })).toBeNull();
  });

  it("says when nothing is archived", async () => {
    backend({ archived: 0 });
    mount("/gallery/archive");
    expect(await screen.findByText("Nothing is archived.")).toBeInTheDocument();
  });

  it("unarchives the selection and drops it from the list", async () => {
    const b = backend();
    mount("/gallery/archive");
    await waitFor(() => expect(tiles().length).toBe(3));
    fireEvent.click(tiles()[0]!.querySelector<HTMLElement>(".g-check")!);
    const bar = await screen.findByRole("toolbar", { name: "Actions for the selected photos" });
    fireEvent.click(within(bar).getByRole("button", { name: "Unarchive" }));
    await waitFor(() => expect(b.batches).toEqual([{ op: "unarchive", ids: [expect.any(String)] }]));
    await waitFor(() => expect(tiles().length).toBe(2));
    expect(await screen.findByText("1 item moved back to the Gallery.")).toBeInTheDocument();
  });

  it("opens the viewer in archive mode", async () => {
    backend();
    mount("/gallery/archive");
    await waitFor(() => expect(tiles().length).toBe(3));
    fireEvent.click(tiles()[1]!.querySelector<HTMLElement>(".g-open")!);
    expect(await screen.findByTestId("viewer")).toHaveAttribute("data-archive", "true");
  });
});

describe("archiving from the Gallery", () => {
  it("archives the selection and drops it from the grid", async () => {
    const b = backend();
    mount("/gallery");
    await waitFor(() => expect(tiles().length).toBe(6));
    fireEvent.click(tiles()[0]!.querySelector<HTMLElement>(".g-check")!);
    fireEvent.click(tiles()[1]!.querySelector<HTMLElement>(".g-check")!);
    const bar = await screen.findByRole("toolbar", { name: "Actions for the selected photos" });
    fireEvent.click(within(bar).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(b.batches.map((x) => x.op)).toEqual(["archive"]));
    expect(b.batches[0]!.ids).toHaveLength(2);
    await waitFor(() => expect(tiles().length).toBe(4));
    expect(await screen.findByText("2 items archived.")).toBeInTheDocument();
  });
});

describe("a Places window", () => {
  it("narrows the grid to the pictures inside it and can be cleared", async () => {
    const b = backend();
    mount("/gallery?bbox=12.4%2C55.6%2C12.7%2C55.8");
    await waitFor(() => expect(b.lists.length).toBeGreaterThan(0));
    expect(last(b).get("bbox")).toBe("12.4,55.6,12.7,55.8");
    expect(screen.getByText("Photos from one place on the map")).toBeInTheDocument();
    // The month counts are of the whole library: no scrubber for a window.
    expect(screen.queryByRole("slider", { name: "Jump to a month" })).toBeNull();
    // Memories are of the whole library too.
    expect(screen.queryByRole("region", { name: "Memories" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show everything" }));
    await waitFor(() => expect(last(b).has("bbox")).toBe(false));
    expect(screen.queryByText("Photos from one place on the map")).toBeNull();
  });
});

describe("the memories strip in the Gallery", () => {
  const years = [{ year: 2024, years_ago: 2, total: 2, items: [raw("old1"), raw("old2")] }];

  it("shows On this day and Recently added above the grid", async () => {
    backend({ years, recent: [raw("n1")] });
    mount("/gallery");
    const strip = await screen.findByRole("region", { name: "Memories" });
    expect(within(strip).getAllByRole("button", { name: /^Open/ })).toHaveLength(2);
    await waitFor(() => expect(tiles().length).toBe(6));
  });

  it("steps aside while a filter or a search is on, and for the Trash", async () => {
    backend({ years, recent: [raw("n1")] });
    mount("/gallery");
    await screen.findByRole("region", { name: "Memories" });
    fireEvent.click(screen.getByRole("button", { name: "Favorites" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Memories" })).toBeNull());
  });

  it("is not part of the Trash", async () => {
    backend({ years, recent: [raw("n1")] });
    mount("/gallery/trash");
    await screen.findByRole("heading", { name: "Trash" });
    await waitFor(() => expect(globalThis.fetch).toHaveBeenCalled());
    expect(screen.queryByRole("region", { name: "Memories" })).toBeNull();
  });
});
