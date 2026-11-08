import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string; title: string }[]; index: number; onClose: () => void; onSimilar?: (item: { id: string; title: string }) => void }) => (
    <div data-testid="viewer">
      <span>{p.items[p.index]?.id}</span>
      {p.onSimilar ? (
        <button type="button" onClick={() => p.onSimilar!(p.items[p.index]!)}>
          similar photos
        </button>
      ) : null}
    </div>
  ),
}));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => null }));

const PLAIN = ["p0", "p1", "p2", "p3", "p4", "p5"];
/** Smart search answers in this order (not by date), two to a page. */
const RANKED = ["p3", "p1", "p4", "p0"];
const READY = { enabled: true, model: "clip", indexed: 120, pending: 30, running: true, paused_hot: false };

interface Opts {
  status?: unknown;
  statusFails?: boolean;
  searchFails?: number;
  searchStatus?: unknown;
  similarFails?: number;
}

interface Backend {
  calls: Call[];
  plain: URLSearchParams[];
  search: URLSearchParams[];
  similar: { id: string; params: URLSearchParams }[];
}

const item = (id: string) => rawItem(id, { title: `${id}.jpg` }, PLAIN.indexOf(id));
const fail = (status: number) => ({ status, body: { error: { code: "x", message: "x" } } });

function backend(opts: Opts = {}): Backend {
  const b: Backend = { calls: [], plain: [], search: [], similar: [] };
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    const p = url.searchParams;
    if (url.pathname === "/v1/media/embeddings/status") return opts.statusFails ? fail(404) : { body: opts.status ?? READY };
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 6, videos: 0 } } };
    if (url.pathname === "/v1/media/items/months") return { body: { months: [] } };
    if (url.pathname === "/v1/media/items/search") {
      b.search.push(p);
      if (opts.searchFails) return fail(opts.searchFails);
      const start = Number(p.get("cursor") ?? 0);
      const page = RANKED.slice(start, start + 2);
      return { body: { items: page.map(item), scores: page.map((_, i) => 0.9 - i / 10), next_cursor: start + 2 < RANKED.length ? String(start + 2) : null, status: opts.searchStatus ?? READY } };
    }
    const similar = /^\/v1\/media\/items\/([^/]+)\/similar$/.exec(url.pathname);
    if (similar) {
      b.similar.push({ id: similar[1]!, params: p });
      if (opts.similarFails) return fail(opts.similarFails);
      return { body: { items: ["p5", "p2"].map(item), scores: [0.8, 0.7], next_cursor: null, status: READY } };
    }
    if (url.pathname === "/v1/media/items" && call.method === "GET") {
      b.plain.push(p);
      const q = p.get("q");
      const rows = q ? PLAIN.filter((id) => id !== "p0") : PLAIN;
      return { body: { items: rows.map(item), next_cursor: null } };
    }
    return fail(404);
  });
  return b;
}

const mount = () => renderAt("/gallery", () => <Route path="/gallery" component={() => <GalleryView />} />);
const searchBox = () => screen.getByRole("searchbox", { name: "Search photos" });
const shownIds = () =>
  [...document.querySelectorAll<HTMLElement>(".g-tile")]
    .sort((a, b) => Number(a.dataset.index) - Number(b.dataset.index))
    .map((t) => /items\/([^/]+)\/thumb/.exec(t.querySelector("img")?.getAttribute("src") ?? "")?.[1]);
const statusRequested = (b: Backend) => b.calls.some((c) => c.url.includes("embeddings/status"));

/** The grid has loaded and the index status is known. */
async function ready(b: Backend) {
  await waitFor(() => expect(shownIds()).toHaveLength(6));
  await waitFor(() => expect(statusRequested(b)).toBe(true));
  await new Promise((r) => setTimeout(r, 20));
}

async function ask(words: string) {
  fireEvent.input(searchBox(), { target: { value: words } });
  fireEvent.keyDown(searchBox(), { key: "Enter" });
}

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

describe("smart search in the Gallery's box", () => {
  it("answers a query by meaning: relevance order, one flat list, a results header", async () => {
    const b = backend();
    mount();
    await ready(b);
    expect(shownIds()).toEqual(PLAIN);
    await ask("beach");
    await waitFor(() => expect(shownIds()).toEqual(RANKED));
    expect(b.search[0]!.get("q")).toBe("beach");
    expect(b.search[0]!.get("origin")).toBe("all");
    // The words listing was not asked for the query.
    expect(b.plain.some((p) => p.get("q") === "beach")).toBe(false);
    expect(screen.getByText("Results for “beach”")).toBeInTheDocument();
    // No day headers, no Day / Month switch: the results are not in date order.
    expect(document.querySelector(".g-head")).toBeNull();
    expect(screen.queryByRole("button", { name: "Months" })).toBeNull();
    expect(screen.getByRole("button", { name: "Smart" })).toHaveAttribute("aria-pressed", "true");
  });

  it("pages with the cursor as the results are scrolled to", async () => {
    const b = backend();
    mount();
    await ready(b);
    await ask("beach");
    await waitFor(() => expect(b.search.length).toBeGreaterThanOrEqual(2));
    expect(b.search.map((p) => p.get("cursor"))).toEqual([null, "2"]);
    await waitFor(() => expect(shownIds()).toEqual(RANKED));
  });

  it("says how many photos it has looked at while the index is catching up, and nothing once it has caught up", async () => {
    const b = backend();
    mount();
    await ready(b);
    await ask("beach");
    expect(await screen.findByText("Smart search has looked at 120 photos so far.")).toBeInTheDocument();
  });

  it("shows no hint when every photo has been looked at", async () => {
    const done = { ...READY, pending: 0 };
    const b = backend({ status: done, searchStatus: done });
    mount();
    await ready(b);
    await ask("beach");
    await waitFor(() => expect(shownIds()).toEqual(RANKED));
    expect(screen.queryByText(/has looked at/)).toBeNull();
  });

  it("switches to the words search and back", async () => {
    const b = backend();
    mount();
    await ready(b);
    await ask("beach");
    await waitFor(() => expect(shownIds()).toEqual(RANKED));
    fireEvent.click(screen.getByRole("button", { name: "Words" }));
    await waitFor(() => expect(shownIds()).toEqual(["p1", "p2", "p3", "p4", "p5"]));
    expect(b.plain.some((p) => p.get("q") === "beach")).toBe(true);
    expect(screen.getByRole("button", { name: "Words" })).toHaveAttribute("aria-pressed", "true");
    // The grouping by day comes back with the words search.
    expect(screen.getByRole("button", { name: "Months" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Smart" }));
    await waitFor(() => expect(shownIds()).toEqual(RANKED));
  });

  it("falls back to the words search when the server answers 503, without an error", async () => {
    const b = backend({ searchFails: 503 });
    mount();
    await ready(b);
    await ask("beach");
    await waitFor(() => expect(shownIds()).toEqual(["p1", "p2", "p3", "p4", "p5"]));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/Results for/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Smart" })).toBeNull();
    // From then on queries go straight to the words search.
    const asked = b.search.length;
    await ask("dog");
    await waitFor(() => expect(b.plain.some((p) => p.get("q") === "dog")).toBe(true));
    expect(b.search.length).toBe(asked);
  });

  it("uses the words search from the start when smart search is off or the server has none", async () => {
    for (const opts of [{ status: { ...READY, enabled: false } }, { statusFails: true }, { status: { ...READY, indexed: 0, pending: 10 } }]) {
      const b = backend(opts);
      const { unmount } = mount();
      await ready(b);
      await ask("beach");
      await waitFor(() => expect(b.plain.some((p) => p.get("q") === "beach")).toBe(true));
      expect(b.search).toEqual([]);
      expect(screen.queryByText(/Results for/)).toBeNull();
      unmount();
      vi.unstubAllGlobals();
    }
  });
});

describe("Similar photos", () => {
  async function openViewer(b: Backend) {
    await ready(b);
    const first = document.querySelector<HTMLElement>('.g-tile[data-index="0"] .g-open')!;
    fireEvent.click(first);
    return screen.findByTestId("viewer");
  }

  it("opens the same grid with the pictures closest to the one in the viewer", async () => {
    const b = backend();
    mount();
    await openViewer(b);
    fireEvent.click(screen.getByRole("button", { name: "similar photos" }));
    await waitFor(() => expect(shownIds()).toEqual(["p5", "p2"]));
    expect(screen.queryByTestId("viewer")).toBeNull();
    expect(b.similar[0]!.id).toBe("p0");
    expect(b.similar[0]!.params.has("q")).toBe(false);
    expect(screen.getByText("Photos like p0.jpg")).toBeInTheDocument();
    expect(document.querySelector(".g-head")).toBeNull();
    // Back to everything.
    fireEvent.click(screen.getByRole("button", { name: "Show everything" }));
    await waitFor(() => expect(shownIds()).toEqual(PLAIN));
  });

  it("is not offered when smart search is off", async () => {
    const b = backend({ status: { ...READY, enabled: false } });
    mount();
    await openViewer(b);
    expect(screen.queryByRole("button", { name: "similar photos" })).toBeNull();
  });

  it("leaves the view when the server refuses (503), back to the plain Gallery", async () => {
    const b = backend({ similarFails: 503 });
    mount();
    await openViewer(b);
    fireEvent.click(screen.getByRole("button", { name: "similar photos" }));
    await waitFor(() => expect(shownIds()).toEqual(PLAIN));
    expect(screen.queryByText(/Photos like/)).toBeNull();
  });
});
