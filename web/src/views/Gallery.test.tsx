/// <reference types="node" />
import { stubWide } from "../test/wide";
import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { lucinaViewer } from "../lib/desktopViewer";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

// The viewer and the two dialogs belong to the viewer builder (their own tests cover them); here they are stand-ins
// that show how the grid calls them.
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; trash?: boolean; onClose: () => void; onChanged: (i: unknown) => void; onRemoved: (id: string) => void }) => (
    <div data-testid="viewer" data-index={p.index} data-trash={String(!!p.trash)}>
      <span>{p.items[p.index]?.id}</span>
      <button type="button" onClick={p.onClose}>
        close viewer
      </button>
      <button type="button" onClick={() => p.onRemoved(p.items[p.index]!.id)}>
        remove in viewer
      </button>
    </div>
  ),
}));
vi.mock("../components/gallery/DeleteDialog", () => ({
  DeleteDialog: (p: { items: { id: string }[]; purge?: boolean; onDone: (r: unknown) => void; onCancel: () => void }) => (
    <div role="dialog" aria-label="delete dialog" data-purge={String(!!p.purge)}>
      <span>{p.items.map((i) => i.id).join(",")}</span>
      <button type="button" onClick={() => p.onDone(p.purge ? { trashed: [], purged: p.items.map((i) => i.id), failed: [] } : { trashed: p.items.map((i) => i.id), purged: [], failed: [] })}>
        confirm delete
      </button>
      <button type="button" onClick={p.onCancel}>
        cancel delete
      </button>
    </div>
  ),
}));
vi.mock("../components/gallery/AlbumPicker", () => ({
  AlbumPicker: (p: { itemIds: string[]; onDone: (a: string) => void; onCancel: () => void }) => (
    <div role="dialog" aria-label="album picker">
      <span>{p.itemIds.join(",")}</span>
      <button type="button" onClick={() => p.onDone("alb_1")}>
        pick album
      </button>
    </div>
  ),
}));

const WIDTH = 1000;
const HEIGHT = 800;

/** jsdom has no layout: the scroll box is 1000 x 800. */
function sizeTheBox() {
  vi.spyOn(Element.prototype, "clientWidth", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? WIDTH : 0;
  });
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? HEIGHT : 0;
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
}

interface Fx {
  id: string;
  device: string | null;
  kind?: "photo" | "video";
  favorite?: boolean;
  trashed?: boolean;
  source?: string;
}

/** `count` items, newest first, three days apart every 6 items, so there are several sections. */
function library(count: number, over: (i: number) => Partial<Fx> = () => ({})): (Fx & { raw: Record<string, unknown> })[] {
  return Array.from({ length: count }, (_, i) => {
    const o = over(i);
    const id = o.id ?? `itm${String(i).padStart(5, "0")}`;
    const day = new Date(2026, 9, 28 - Math.floor(i / 6), 12 - (i % 6));
    const device = o.device === undefined ? "iphone-test" : o.device;
    const fx: Fx = { id, device, kind: o.kind ?? "photo", favorite: o.favorite ?? false, trashed: o.trashed ?? false, source: o.source ?? (device ? "ios-photos" : "ios-chat") };
    return {
      ...fx,
      raw: {
        id,
        kind: fx.kind,
        title: `${id}.jpg`,
        mime: "image/jpeg",
        size: 1000,
        taken_at: day.toISOString(),
        created_at: day.toISOString(),
        width: i % 2 ? 3000 : 4000,
        height: i % 2 ? 4000 : 3000,
        duration_ms: fx.kind === "video" ? 65_000 : null,
        source: fx.source,
        device: fx.device,
        device_name: fx.device ? "Test iPhone" : null,
        origin: { filename: null, folder: null, imported_from: null },
        favorite: fx.favorite,
        rotation: 0,
        albums: [],
        trashed_at: fx.trashed ? "2026-10-20T10:00:00Z" : null,
        trash_expires_at: fx.trashed ? new Date(Date.now() + 12.2 * 86_400_000).toISOString() : null,
        thumbhash: null,
        urls: { thumb: `/v1/media/items/${id}/thumb?size=256`, download: `/v1/media/items/${id}/original?download=1` },
      },
    };
  });
}

const SOURCES = {
  sources: [
    { id: "device:iphone-test", label: "Test iPhone", kind: "iphone", photos: 70, videos: 5, latest_taken_at: "2026-10-28T10:00:00Z" },
    { id: "device:backup-old", label: "Old backup", kind: "backup", photos: 20, videos: 0, latest_taken_at: "2019-01-01T10:00:00Z" },
    { id: "chat", label: "Chat & shares", kind: "chat", photos: 4, videos: 0, latest_taken_at: "2026-10-01T10:00:00Z" },
  ],
  all: { photos: 94, videos: 5 },
};

const MONTHS = { months: [{ month: "2026-10", count: 60 }, { month: "2026-09", count: 30 }] };

interface Backend {
  calls: Call[];
  items: ReturnType<typeof library>;
  lists: URLSearchParams[];
  batches: { op: string; ids: string[] }[];
}

/** A tiny lucina-media: the sources, months, a cursor-paged item list that honours the filters, and batch ops. */
function backend(all: ReturnType<typeof library>, opts: { sources?: unknown; months?: unknown; failList?: () => boolean } = {}): Backend {
  const b: Backend = { calls: [], items: all, lists: [], batches: [] };
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/sources") return { body: opts.sources ?? SOURCES };
    if (url.pathname === "/v1/media/items/months") return { body: opts.months ?? MONTHS };
    if (url.pathname === "/v1/media/items/batch") {
      const body = call.body as { op: string; ids: string[] };
      b.batches.push(body);
      return { body: { done: body.ids, failed: [] } };
    }
    if (url.pathname === "/v1/media/items" && call.method === "GET") {
      b.lists.push(url.searchParams);
      if (opts.failList?.()) return { status: 500, body: { error: { code: "boom", message: "boom" } } };
      const p = url.searchParams;
      const origin = p.get("origin") ?? "all";
      const kinds = (p.get("kind") ?? "photo,video").split(",");
      const rows = b.items.filter(
        (i) =>
          kinds.includes(i.kind!) &&
          (p.get("trashed") === "true") === i.trashed &&
          (p.get("favorite") !== "true" || i.favorite) &&
          (origin === "all" || (origin === "chat" ? i.device === null : origin === `device:${i.device}`)),
      );
      const start = Number(p.get("cursor") ?? 0);
      const limit = Number(p.get("limit"));
      const page = rows.slice(start, start + limit);
      return { body: { items: page.map((i) => i.raw), next_cursor: start + limit < rows.length ? String(start + limit) : null } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function mount(path = "/gallery") {
  return renderAt(path, () => (
    <>
      <Route path="/gallery" component={() => <GalleryView />} />
      <Route path="/gallery/trash" component={() => <GalleryView trash />} />
    </>
  ));
}

const tiles = () => document.querySelectorAll<HTMLElement>(".g-tile");
const tile = (index: number) => document.querySelector<HTMLElement>(`.g-tile[data-index="${index}"]`)!;
const open = (index: number) => tile(index).querySelector<HTMLElement>(".g-open")!;
const scroller = () => document.querySelector<HTMLElement>(".g-scroll")!;
const last = (b: Backend) => b.lists[b.lists.length - 1]!;

function scrollTo(top: number) {
  const el = scroller();
  el.scrollTop = top;
  fireEvent.scroll(el);
}

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
});

afterEach(() => {
  // Only the desktop stub (stubWide): the fetch stub stays, since gallery requests may still settle after a test.
  delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  lucinaViewer.setDevice(null);
  vi.restoreAllMocks();
});

describe("the Gallery's scroll box", () => {
  it("is one box with its own overflow, inside a flex chain that can shrink", () => {
    const css = readFileSync(join(process.cwd(), "src/styles/gallery.css"), "utf8");
    const rule = (selector: string) => new RegExp(`(^|\\n)${selector.replace(/[.]/g, "\\.")}\\s*\\{([^}]*)\\}`).exec(css)?.[2] ?? "";
    expect(rule(".gallery")).toMatch(/flex:\s*1/);
    expect(rule(".gallery")).toMatch(/min-height:\s*0/);
    expect(rule(".g-main")).toMatch(/flex:\s*1/);
    expect(rule(".g-main")).toMatch(/min-height:\s*0/);
    expect(rule(".g-scroll")).toMatch(/overflow-y:\s*auto/);
    expect(rule(".g-scroll")).toMatch(/position:\s*absolute/);
    expect(rule(".g-scroll")).toMatch(/inset:\s*0/);
  });

  it("renders the grid in .g-main inside .gallery", async () => {
    backend(library(30));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));
    expect(scroller().closest(".g-main")?.parentElement).toHaveClass("gallery");
  });
});

describe("sources", () => {
  it("shows All and every source with its count, All selected, and asks for the whole Gallery", async () => {
    const b = backend(library(30));
    mount();
    const nav = await screen.findByRole("navigation", { name: "Where the photos come from" });
    const chips = within(nav).getAllByRole("button");
    expect(chips.map((c) => c.textContent)).toEqual(["All99", "Test iPhone75", "Old backup20", "Chat & shares4"].map((x) => x));
    expect(within(nav).getByRole("button", { name: /^All/ })).toHaveAttribute("aria-current", "true");
    await waitFor(() => expect(b.lists.length).toBe(1));
    expect(Object.fromEntries(last(b))).toEqual({ scope: "gallery", origin: "all", kind: "photo,video", limit: "200" });
  });

  it("opens on this Mac's own pictures in the Mac app, and falls back to All when the Mac has none", async () => {
    lucinaViewer.setDevice({ id: "mac-test", name: "Test Mac", kind: "mac" });
    const b = backend(library(30));
    mount();
    await waitFor(() => expect(b.lists.length).toBeGreaterThan(0));
    expect(b.lists[0]!.get("origin")).toBe("device:mac-test");
    await waitFor(() => expect(last(b).get("origin")).toBe("all"));
  });

  it("opens on the Mac when it is in the library", async () => {
    lucinaViewer.setDevice({ id: "iphone-test", name: "Test Mac", kind: "mac" });
    const b = backend(library(30));
    mount();
    const nav = await screen.findByRole("navigation", { name: "Where the photos come from" });
    expect(within(nav).getByRole("button", { name: /Test iPhone/ })).toHaveAttribute("aria-current", "true");
    expect(b.lists.every((l) => l.get("origin") === "device:iphone-test")).toBe(true);
  });

  it("switches source: a new query, a fresh list, and no device badge outside All", async () => {
    const b = backend(library(30, (i) => ({ device: i < 12 ? "iphone-test" : null })));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));
    expect(document.querySelector(".g-badge--source")).not.toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: /^Test iPhone/ }));
    await waitFor(() => expect(last(b).get("origin")).toBe("device:iphone-test"));
    await waitFor(() => expect(tiles().length).toBe(12));
    expect(document.querySelector(".g-badge--source")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^Chat & shares/ }));
    await waitFor(() => expect(last(b).get("origin")).toBe("chat"));
    await waitFor(() => expect(tiles().length).toBe(18));
    expect(screen.getByRole("button", { name: /^Chat & shares/ })).toHaveAttribute("aria-current", "true");
  });
});

describe("filters", () => {
  it("builds the query from type, favorites, dates and search", async () => {
    const b = backend(library(30, (i) => ({ kind: i % 5 === 0 ? "video" : "photo", favorite: i % 7 === 0 })));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));

    fireEvent.click(screen.getByRole("button", { name: "Videos" }));
    await waitFor(() => expect(last(b).get("kind")).toBe("video"));
    await waitFor(() => expect(tiles().length).toBe(6));

    fireEvent.click(screen.getByRole("button", { name: "All" , pressed: false }));
    fireEvent.click(screen.getByRole("button", { name: "Favorites" }));
    await waitFor(() => expect(last(b).get("favorite")).toBe("true"));
    expect(last(b).get("kind")).toBe("photo,video");

    fireEvent.click(screen.getByRole("button", { name: "Dates" }));
    fireEvent.input(screen.getByLabelText("From"), { target: { value: "2026-09-01" } });
    fireEvent.input(screen.getByLabelText("To"), { target: { value: "2026-10-15" } });
    await waitFor(() => expect(last(b).get("to")).toBe("2026-10-15"));
    expect(last(b).get("from")).toBe("2026-09-01");

    fireEvent.input(screen.getByRole("searchbox", { name: "Search photos" }), { target: { value: "  beach  " } });
    await waitFor(() => expect(last(b).get("q")).toBe("beach"));
    expect(last(b).get("favorite")).toBe("true");
    expect(last(b).get("origin")).toBe("all");
  });

  it("clears all filters at once", async () => {
    const b = backend(library(30));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));
    fireEvent.click(screen.getByRole("button", { name: "Favorites" }));
    fireEvent.click(screen.getByRole("button", { name: "Photos" }));
    await waitFor(() => expect(last(b).get("kind")).toBe("photo"));
    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => expect(last(b).get("favorite")).toBeNull());
    expect(last(b).get("kind")).toBe("photo,video");
    expect(screen.queryByRole("button", { name: "Clear filters" })).toBeNull();
  });

  it("says when nothing matches and offers to clear", async () => {
    backend(library(30));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(0));
    fireEvent.click(screen.getByRole("button", { name: "Favorites" }));
    expect(await screen.findByText("Nothing matches these filters.")).toBeInTheDocument();
    expect(tiles().length).toBe(0);
  });

  it("says when the library is empty", async () => {
    backend([]);
    mount();
    expect(await screen.findByText("No photos here yet.")).toBeInTheDocument();
  });

  it("groups by month on request and remembers it", async () => {
    backend(library(30));
    mount();
    await waitFor(() => expect(document.querySelectorAll(".g-section").length).toBeGreaterThan(1));
    const daySections = document.querySelectorAll(".g-section").length;
    fireEvent.click(screen.getByRole("button", { name: "Months" }));
    await waitFor(() => expect(document.querySelectorAll(".g-section").length).toBeLessThan(daySections));
    expect(localStorage.getItem("secretary.gallery.group")).toBe("month");
  });
});

describe("the virtual grid", () => {
  it("keeps only the pictures near the viewport in the DOM and loads the next page near the end", async () => {
    const b = backend(library(1200));
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(10));
    await screen.findByRole("slider", { name: "Jump to a month" }); // the scrubber narrows the grid: wait for the final layout
    await waitFor(() => expect(scroller().scrollTop).toBe(0));
    // 200 loaded, a few screens of them drawn.
    expect(tiles().length).toBeLessThan(120);
    expect(tiles().length).toBeLessThan(200);
    await waitFor(() => expect(b.lists.length).toBeGreaterThanOrEqual(1));
    const before = b.lists.length;
    expect(last(b).get("cursor")).toBeNull();

    const height = parseFloat(document.querySelector<HTMLElement>(".g-content")!.style.height);
    scrollTo(height - HEIGHT * 2);
    await waitFor(() => expect(b.lists.length).toBeGreaterThan(before));
    expect(last(b).get("cursor")).toBe("200");
    expect(last(b).get("limit")).toBe("200");
    await waitFor(() => expect(parseFloat(document.querySelector<HTMLElement>(".g-content")!.style.height)).toBeGreaterThan(height));
    expect(tiles().length).toBeLessThan(150);
    // The pictures drawn are the ones at the scroll position.
    const indexes = [...tiles()].map((t) => Number(t.dataset.index));
    expect(Math.max(...indexes)).toBeGreaterThan(150);
    expect(Math.min(...indexes)).toBeGreaterThan(20);
  });

  it("sticks one header per section and names the days", async () => {
    backend(library(30));
    mount();
    await waitFor(() => expect(document.querySelectorAll(".g-head").length).toBeGreaterThan(2));
    const heads = [...document.querySelectorAll(".g-head__label")].map((h) => h.textContent);
    expect(heads[0]).toContain("Wednesday");
    expect(heads[0]).toContain("2026");
    expect(document.querySelectorAll(".g-section h2").length).toBe(document.querySelectorAll(".g-section").length);
  });

  it("shows a video's length and a device badge in All", async () => {
    backend(library(12, (i) => ({ kind: i === 0 ? "video" : "photo" })));
    mount();
    await waitFor(() => expect(tiles().length).toBe(12));
    expect(tile(0).querySelector(".g-badge--video")).toHaveTextContent("1:05");
    expect(tile(1).querySelector(".g-badge--video")).toBeNull();
    expect(tile(1).querySelector(".g-badge--source")).not.toBeNull();
    expect(open(0)).toHaveAccessibleName(/^Video, 1:05, .*Test iPhone/);
  });

  it("offers a retry when a page fails", async () => {
    let fail = false;
    const b = backend(library(600), { failList: () => fail });
    mount();
    await waitFor(() => expect(tiles().length).toBeGreaterThan(10));
    await screen.findByRole("slider", { name: "Jump to a month" });
    fail = true;
    scrollTo(parseFloat(document.querySelector<HTMLElement>(".g-content")!.style.height));
    expect(await screen.findByText("Couldn't load more.")).toBeInTheDocument();
    const calls = b.lists.length;
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(b.lists.length).toBeGreaterThan(calls));
    await waitFor(() => expect(screen.queryByText("Couldn't load more.")).toBeNull());
  });

  it("shows an error with a retry when the first page fails", async () => {
    let fail = true;
    backend(library(30), { failList: () => fail });
    mount();
    expect(await screen.findByText("Couldn't load the gallery.")).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(tiles().length).toBe(30));
  });
});

describe("the month scrubber", () => {
  const months = { months: [{ month: "2026-10", count: 60 }, { month: "2026-09", count: 30 }, { month: "2026-05", count: 30 }] };

  it("jumps to a month, loading pages down to it first (500 at a time)", async () => {
    const b = backend(library(1200), { months });
    mount();
    const slider = await screen.findByRole("slider", { name: "Jump to a month" });
    await waitFor(() => expect(tiles().length).toBeGreaterThan(10));
    expect(slider).toHaveAttribute("aria-valuetext", expect.stringContaining("October"));
    const before = b.lists.length;
    fireEvent.keyDown(slider, { key: "End" });
    await waitFor(() => expect(scroller().scrollTop).toBeGreaterThan(5000));
    expect(b.lists.length).toBeGreaterThan(before);
    expect(b.lists.slice(before).every((l) => l.get("limit") === "500")).toBe(true);
    const sections = [...document.querySelectorAll<HTMLElement>(".g-section")].map((s) => s.dataset.section!);
    expect(sections.some((id) => id.startsWith("2026-05"))).toBe(true);
    await waitFor(() => expect(slider).toHaveAttribute("aria-valuetext", expect.stringContaining("May")));
    fireEvent.keyDown(slider, { key: "Home" });
    await waitFor(() => expect(scroller().scrollTop).toBe(0));
  });

  it("is hidden under a search or a date range, where the month counts no longer match", async () => {
    backend(library(60), { months });
    mount();
    await screen.findByRole("slider", { name: "Jump to a month" });
    fireEvent.click(screen.getByRole("button", { name: "Dates" }));
    fireEvent.input(screen.getByLabelText("From"), { target: { value: "2026-09-01" } });
    await waitFor(() => expect(screen.queryByRole("slider")).toBeNull());
  });
});

describe("selection", () => {
  async function ready(n = 30, over?: (i: number) => Partial<Fx>) {
    const b = backend(library(n, over));
    mount();
    await waitFor(() => expect(tiles().length).toBe(n));
    return b;
  }
  const selected = () => [...document.querySelectorAll<HTMLElement>(".g-tile--selected")].map((t) => Number(t.dataset.index)).sort((a, b) => a - b);

  it("opens the viewer on a plain click at that picture", async () => {
    await ready();
    fireEvent.click(open(4));
    const viewer = await screen.findByTestId("viewer");
    expect(viewer).toHaveAttribute("data-index", "4");
    expect(viewer).toHaveAttribute("data-trash", "false");
    expect(screen.queryByRole("toolbar")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "close viewer" }));
    expect(screen.queryByTestId("viewer")).toBeNull();
  });

  it("toggles with ctrl or cmd click, then plain clicks keep toggling", async () => {
    await ready();
    fireEvent.click(open(2), { ctrlKey: true });
    expect(selected()).toEqual([2]);
    expect(screen.getByRole("toolbar", { name: "Actions for the selected photos" })).toHaveTextContent("1 selected");
    fireEvent.click(open(5), { metaKey: true });
    expect(selected()).toEqual([2, 5]);
    fireEvent.click(open(7)); // selecting: a plain click selects, it doesn't open
    expect(selected()).toEqual([2, 5, 7]);
    expect(screen.queryByTestId("viewer")).toBeNull();
    fireEvent.click(open(5));
    expect(selected()).toEqual([2, 7]);
  });

  it("selects a range with shift-click from the last one picked, either way", async () => {
    await ready();
    fireEvent.click(open(3), { ctrlKey: true });
    fireEvent.click(open(8), { shiftKey: true });
    expect(selected()).toEqual([3, 4, 5, 6, 7, 8]);
    fireEvent.click(open(1), { shiftKey: true });
    expect(selected()).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
  });

  it("opens the actions with a long press on touch (Select among them), and nothing with a short tap", async () => {
    await ready();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.pointerDown(open(6), { pointerType: "touch", clientX: 10, clientY: 10 });
    fireEvent.pointerUp(open(6), { pointerType: "touch" });
    vi.advanceTimersByTime(600);
    expect(selected()).toEqual([]);
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.pointerDown(open(6), { pointerType: "touch", clientX: 10, clientY: 10 });
    vi.advanceTimersByTime(600);
    // The click that ends the press is swallowed (it would otherwise open the photo).
    fireEvent.click(open(6));
    const sheet = screen.getByRole("menu", { name: "Photo actions" });
    expect(within(sheet).getByRole("button", { name: "Cancel" })).toBeInTheDocument();
    fireEvent.click(within(sheet).getByRole("menuitem", { name: "Select" }));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(selected()).toEqual([6]);
    // Selecting already: a long press adds the photo, with no sheet.
    fireEvent.pointerDown(open(7), { pointerType: "touch", clientX: 10, clientY: 10 });
    vi.advanceTimersByTime(600);
    fireEvent.click(open(7));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(selected()).toEqual([6, 7]);
    fireEvent.click(within(tile(7)).getByRole("checkbox", { name: "Select" }));
    // Moving the finger first (a scroll) cancels the press.
    fireEvent.pointerDown(open(9), { pointerType: "touch", clientX: 10, clientY: 10 });
    fireEvent.pointerMove(open(9), { pointerType: "touch", clientX: 10, clientY: 60 });
    vi.advanceTimersByTime(600);
    expect(selected()).toEqual([6]);
    vi.useRealTimers();
  });

  it("selects through the tile's check button and a section's check button", async () => {
    await ready();
    fireEvent.click(within(tile(0)).getByRole("checkbox", { name: "Select" }));
    expect(selected()).toEqual([0]);
    expect(within(tile(0)).getByRole("checkbox")).toBeChecked();
    const section = document.querySelectorAll(".g-section")[1]!;
    fireEvent.click(within(section as HTMLElement).getByRole("checkbox", { name: /Select all of/ }));
    expect(selected()).toEqual([0, 6, 7, 8, 9, 10, 11]);
    fireEvent.click(within(section as HTMLElement).getByRole("checkbox", { name: /Select all of/ }));
    expect(selected()).toEqual([0]);
  });

  it("selects everything loaded with Select all and Ctrl+A, and clears with Escape or the close button", async () => {
    await ready();
    fireEvent.click(open(1), { ctrlKey: true });
    fireEvent.click(screen.getByRole("button", { name: "Select all" }));
    expect(selected()).toHaveLength(30);
    expect(screen.getByRole("toolbar")).toHaveTextContent("30 selected");
    expect(screen.queryByRole("button", { name: "Select all" })).toBeNull();
    fireEvent.keyDown(open(1), { key: "Escape" });
    expect(selected()).toEqual([]);
    expect(screen.queryByRole("toolbar")).toBeNull();
    fireEvent.keyDown(open(1), { key: "a", ctrlKey: true });
    expect(selected()).toHaveLength(30);
    fireEvent.click(screen.getByRole("button", { name: "Clear selection" }));
    expect(selected()).toEqual([]);
  });

  it("drops the selection when the source changes", async () => {
    await ready();
    fireEvent.click(open(1), { ctrlKey: true });
    fireEvent.click(await screen.findByRole("button", { name: /^Chat & shares/ }));
    await waitFor(() => expect(screen.queryByRole("toolbar")).toBeNull());
  });

  it("moves the focus with the arrow keys and toggles with Space", async () => {
    await ready();
    const first = open(0);
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect(document.activeElement).toBe(open(1));
    fireEvent.keyDown(open(1), { key: "ArrowDown" });
    expect(Number((document.activeElement as HTMLElement).closest(".g-tile")!.getAttribute("data-index"))).toBeGreaterThan(1);
    fireEvent.keyDown(open(1), { key: "ArrowLeft" });
    expect(document.activeElement).toBe(open(0));
    fireEvent.keyDown(open(0), { key: " " });
    expect(selected()).toEqual([0]);
    // One tab stop for the whole grid.
    expect(document.querySelectorAll('.g-open[tabindex="0"]')).toHaveLength(1);
  });
});

// Owner, 2026-10-30: "clicking with the right click should open a popup menu with actions and also copy".
describe("context menu", () => {
  const frame = () => new Promise((r) => setTimeout(r, 20));
  async function ready() {
    const b = backend(library(30));
    mount();
    await waitFor(() => expect(tiles().length).toBe(30));
    return b;
  }
  const selected = () => [...document.querySelectorAll<HTMLElement>(".g-tile--selected")].map((t) => Number(t.dataset.index)).sort((a, b) => a - b);

  it("opens on a right-click at the pointer and acts on that photo alone", async () => {
    const b = await ready();
    vi.stubGlobal("innerWidth", 1200);
    fireEvent.contextMenu(open(3), { clientX: 200, clientY: 150 });
    await frame();
    const menu = screen.getByRole("menu", { name: "Photo actions" });
    expect(within(menu).getAllByRole("menuitem").map((m) => m.textContent)).toEqual([
      "Open", "Copy image", "Copy link", "Download original", "Share", "Favorite", "Show in its day", "Ask Lucina about this photo", "Delete",
    ]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Favorite" }));
    await waitFor(() => expect(b.batches).toEqual([{ op: "favorite", ids: ["itm00003"] }]));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(selected()).toEqual([]);
  });

  it("acts on the whole selection when the right-clicked photo is part of it", async () => {
    const b = await ready();
    vi.stubGlobal("innerWidth", 1200);
    fireEvent.click(open(1), { ctrlKey: true });
    fireEvent.click(open(2), { ctrlKey: true });
    fireEvent.contextMenu(open(2), { clientX: 10, clientY: 10 });
    await frame();
    const menu = screen.getByRole("menu");
    expect(within(menu).queryByRole("menuitem", { name: "Copy image" })).toBeNull();
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Delete" }));
    expect(within(screen.getByRole("dialog", { name: "delete dialog" })).getByText("itm00001,itm00002")).toBeInTheDocument();
    // A right-click outside the selection is about that photo only.
    fireEvent.click(screen.getByRole("button", { name: "cancel delete" }));
    fireEvent.contextMenu(open(7), { clientX: 10, clientY: 10 });
    await frame();
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Favorite" }));
    await waitFor(() => expect(b.batches.at(-1)).toEqual({ op: "favorite", ids: ["itm00007"] }));
    expect(selected()).toEqual([1, 2]);
  });

  it("copies the photo's link", async () => {
    await ready();
    vi.stubGlobal("innerWidth", 1200);
    const writeText = vi.fn(async (_text: string) => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    fireEvent.contextMenu(open(0), { clientX: 10, clientY: 10 });
    await frame();
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Copy link" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledOnce());
    expect(writeText.mock.calls[0]![0]).toMatch(/^http:\/\/localhost(:\d+)?\/v1\/media\/items\/itm00000\//);
    expect(await screen.findByText("Link copied.")).toBeInTheDocument();
  });

  it("opens from the keyboard (the context-menu key on a focused tile) over the tile", async () => {
    await ready();
    vi.stubGlobal("innerWidth", 1200);
    open(5).focus();
    fireEvent.contextMenu(open(5));
    await frame();
    expect(screen.getByRole("menu", { name: "Photo actions" })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(open(5));
  });
});

describe("batch actions", () => {
  async function selectThree(over?: (i: number) => Partial<Fx>) {
    const b = backend(library(30, over));
    mount();
    await waitFor(() => expect(tiles().length).toBe(30));
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(2), { shiftKey: true });
    return b;
  }

  it("favorites the selection in one batch, and marks the tiles", async () => {
    const b = await selectThree();
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Favorite" }));
    await waitFor(() => expect(b.batches).toEqual([{ op: "favorite", ids: ["itm00000", "itm00001", "itm00002"] }]));
    await waitFor(() => expect(tile(0).querySelector(".g-badge--fav")).not.toBeNull());
    expect(tile(2).querySelector(".g-badge--fav")).not.toBeNull();
    expect(tile(3).querySelector(".g-badge--fav")).toBeNull();
    expect(await screen.findByText("3 items added to favorites.")).toBeInTheDocument();
    // Now all favorites: the button takes them out again.
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Unfavorite" }));
    await waitFor(() => expect(b.batches.at(-1)).toEqual({ op: "unfavorite", ids: ["itm00000", "itm00001", "itm00002"] }));
  });

  it("removes un-favorited items from a favorites-only list", async () => {
    const b = backend(library(30, (i) => ({ favorite: i < 3 })));
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Favorites" }));
    await waitFor(() => expect(tiles().length).toBe(3));
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(2), { shiftKey: true });
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Unfavorite" }));
    await waitFor(() => expect(tiles().length).toBe(0));
    expect(b.batches.at(-1)!.op).toBe("unfavorite");
  });

  it("opens the album picker with the selected ids and says when they are added", async () => {
    await selectThree();
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Add to album" }));
    const picker = screen.getByRole("dialog", { name: "album picker" });
    expect(picker).toHaveTextContent("itm00000,itm00001,itm00002");
    fireEvent.click(within(picker).getByRole("button", { name: "pick album" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(await screen.findByText("3 items added to the album.")).toBeInTheDocument();
    expect(screen.getByRole("toolbar")).toBeInTheDocument(); // the selection stays for the next action
  });

  it("deletes through the delete dialog and drops the items from the grid and the counts", async () => {
    const b = await selectThree();
    const sourcesBefore = b.calls.filter((c) => c.url.includes("/v1/media/sources")).length;
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Delete" }));
    const dialog = screen.getByRole("dialog", { name: "delete dialog" });
    expect(dialog).toHaveAttribute("data-purge", "false");
    expect(dialog).toHaveTextContent("itm00000,itm00001,itm00002");
    fireEvent.click(within(dialog).getByRole("button", { name: "confirm delete" }));
    await waitFor(() => expect(tiles().length).toBe(27));
    expect(document.querySelector('[data-id="itm00000"]')).toBeNull();
    expect(screen.queryByRole("toolbar")).toBeNull();
    expect(await screen.findByText("3 items moved to the Trash.")).toBeInTheDocument();
    await waitFor(() => expect(b.calls.filter((c) => c.url.includes("/v1/media/sources")).length).toBeGreaterThan(sourcesBefore));
  });

  it("keeps the items when the delete dialog is cancelled", async () => {
    await selectThree();
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Delete" }));
    fireEvent.click(screen.getByRole("button", { name: "cancel delete" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(tiles().length).toBe(30);
    expect(screen.getByRole("toolbar")).toHaveTextContent("3 selected");
  });

  it("deletes with the Delete key on a selection", async () => {
    await selectThree();
    fireEvent.keyDown(open(1), { key: "Delete" });
    expect(screen.getByRole("dialog", { name: "delete dialog" })).toBeInTheDocument();
  });

  it("downloads each selected item", async () => {
    backend(library(30));
    mount();
    await waitFor(() => expect(tiles().length).toBe(30));
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(1), { ctrlKey: true });
    const clicked: string[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      clicked.push(this.getAttribute("href")!);
    });
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Download" }));
    vi.advanceTimersByTime(2000);
    expect(clicked).toEqual(["/v1/media/items/itm00000/original?download=1", "/v1/media/items/itm00001/original?download=1"]);
    vi.useRealTimers();
  });

  it("drops an item the viewer removed, and closes the viewer on the last one", async () => {
    backend(library(2));
    mount();
    await waitFor(() => expect(tiles().length).toBe(2));
    fireEvent.click(open(0));
    fireEvent.click(await screen.findByRole("button", { name: "remove in viewer" }));
    await waitFor(() => expect(tiles().length).toBe(1));
    expect(screen.getByTestId("viewer")).toHaveAttribute("data-index", "0");
    fireEvent.click(screen.getByRole("button", { name: "remove in viewer" }));
    await waitFor(() => expect(screen.queryByTestId("viewer")).toBeNull());
  });
});

describe("the Trash", () => {
  async function trash() {
    const items = library(14, (i) => ({ trashed: i < 8, device: i % 2 ? "iphone-test" : null }));
    const b = backend(items);
    mount("/gallery/trash");
    await waitFor(() => expect(tiles().length).toBe(8));
    return b;
  }

  it("lists the trash of every source, with the days left on each tile and no source bar", async () => {
    const b = await trash();
    expect(last(b).get("trashed")).toBe("true");
    expect(last(b).get("origin")).toBe("all");
    expect(screen.getByRole("heading", { name: "Trash" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Where the photos come from" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Favorites" })).toBeNull();
    expect(tile(0).querySelector(".g-badge--days")).toHaveTextContent("13 days");
    expect(open(0)).toHaveAccessibleName(/13 days left/);
    expect(screen.getByText("Items are deleted for good after 30 days.")).toBeInTheDocument();
    expect(document.querySelector(".gallery")).toHaveClass("gallery--trash");
  });

  it("opens the viewer in trash mode", async () => {
    await trash();
    fireEvent.click(open(1));
    expect(await screen.findByTestId("viewer")).toHaveAttribute("data-trash", "true");
  });

  it("restores the selection in one batch and drops it from the list", async () => {
    const b = await trash();
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(1), { ctrlKey: true });
    const bar = screen.getByRole("toolbar");
    expect(within(bar).queryByRole("button", { name: "Favorite" })).toBeNull();
    expect(within(bar).queryByRole("button", { name: "Add to album" })).toBeNull();
    fireEvent.click(within(bar).getByRole("button", { name: "Restore" }));
    await waitFor(() => expect(tiles().length).toBe(6));
    expect(b.batches).toEqual([{ op: "restore", ids: ["itm00000", "itm00001"] }]);
    expect(await screen.findByText("2 items restored.")).toBeInTheDocument();
  });

  it("deletes forever through the dialog in purge mode", async () => {
    await trash();
    fireEvent.click(open(2), { ctrlKey: true });
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Delete forever" }));
    const dialog = screen.getByRole("dialog", { name: "delete dialog" });
    expect(dialog).toHaveAttribute("data-purge", "true");
    fireEvent.click(within(dialog).getByRole("button", { name: "confirm delete" }));
    await waitFor(() => expect(tiles().length).toBe(7));
    expect(await screen.findByText("1 item deleted.")).toBeInTheDocument();
  });

  it("says when the trash is empty", async () => {
    backend(library(5));
    mount("/gallery/trash");
    expect(await screen.findByText("The Trash is empty.")).toBeInTheDocument();
  });

  it("links to the trash from the gallery and back", async () => {
    backend(library(5));
    mount();
    const link = await screen.findByRole("link", { name: "Trash" });
    expect(link).toHaveAttribute("href", "/gallery/trash");
    fireEvent.click(link);
    expect(await screen.findByRole("link", { name: "Gallery" })).toHaveAttribute("href", "/gallery");
  });
});

describe("languages", () => {
  it("speaks Italian and Danish", async () => {
    setLocale("it");
    backend(library(10));
    mount();
    expect(await screen.findByRole("button", { name: "Preferiti" })).toBeInTheDocument();
    expect(await screen.findByRole("navigation", { name: "Da dove arrivano le foto" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Tutte/ })).toBeInTheDocument();
  });

  it("names the date headings in the language", async () => {
    setLocale("da");
    backend(library(10));
    mount();
    await waitFor(() => expect(document.querySelector(".g-head__label")).not.toBeNull());
    expect(document.querySelector(".g-head__label")!.textContent!.toLowerCase()).toContain("onsdag");
  });
});

it("keeps the desktop Gallery header above its filters and empty photo surface", async () => {
  stubWide(); backend([]); mount();
  expect(await screen.findByRole("heading", { level: 1, name: "Gallery" })).toBeInTheDocument();
  expect(within(screen.getByRole("navigation", { name: "Gallery sections" })).getByRole("link", { name: "Photos" })).toHaveAttribute("aria-current", "page");
  expect(document.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  expect(document.querySelector(".g-title")).toBeNull();
  expect(document.querySelector(".pg-grid--full")).toBeInTheDocument();
  expect(screen.getByRole("group", { name: "Filters" })).toBeInTheDocument();
  await waitFor(() => expect(document.querySelector(".pg-empty")).toHaveTextContent("No photos here yet."));
});

it("renders the toolbar and the photo grid as full-width children of the grid page, under the header", async () => {
  stubWide(); backend(library(30)); mount();
  await screen.findByRole("heading", { level: 1, name: "Gallery" });
  await waitFor(() => expect(document.querySelector(".g-body .g-scroll")).not.toBeNull());
  const header = document.querySelector(".pg-head")!;
  const main = document.querySelector(".pg-grid--full > .pg-main")!;
  expect(header.compareDocumentPosition(main) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  // One toolbar row for the search and icon buttons, then the filters, then the grid, all inside the page's main column.
  expect(main.querySelector(":scope > .g-top .g-desktop-tools .g-search")).not.toBeNull();
  expect(main.querySelectorAll(":scope > .g-top .g-desktop-tools .pg-icon-btn").length).toBeGreaterThan(0);
  expect(main.querySelector(":scope > .g-top .g-filters")).not.toBeNull();
  expect(main.querySelector(":scope > .g-main .g-body .g-scroll")).not.toBeNull();
});

function mountShelf(path: string) {
  // The routes of src/index.tsx: the shelves ask for the desktop layout.
  return renderAt(path, () => (
    <>
      <Route path="/gallery" component={() => <GalleryView />} />
      <Route path="/gallery/trash" component={() => <GalleryView trash desktop />} />
      <Route path="/gallery/archive" component={() => <GalleryView archive desktop />} />
    </>
  ));
}

describe("Trash and Archive on the page system (desktop)", () => {
  it.each([
    ["/gallery/trash", "Trash"],
    ["/gallery/archive", "Archive"],
  ])("%s keeps the Gallery header (Photos current) over a back link and the %s heading", async (path, title) => {
    stubWide(); backend(library(5)); mountShelf(path);
    expect(await screen.findByRole("heading", { level: 1, name: "Gallery" })).toBeInTheDocument();
    expect(within(screen.getByRole("navigation", { name: "Gallery sections" })).getByRole("link", { name: "Photos" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("heading", { level: 2, name: title })).toHaveClass("g-detail-title");
    expect(document.querySelector("a.pg-back")).toHaveAttribute("href", "/gallery");
    expect(document.querySelector(".g-bar")).toBeNull();
    expect(document.querySelector(".g-title")).toBeNull();
    expect(document.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
    await waitFor(() => expect(document.querySelector(".pg-grid--full > .pg-main > .g-main .g-scroll")).not.toBeNull());
    // Shelves have no source bar, memories strip or shelf icons.
    expect(document.querySelector(".pg-icon-btn")).toBeNull();
  });

  it("shows the friendly empty state in the Trash", async () => {
    stubWide(); backend([]); mountShelf("/gallery/trash");
    await waitFor(() => expect(document.querySelector(".pg-empty")).not.toBeNull());
  });
});
