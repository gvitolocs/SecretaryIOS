import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { type Call, stubFetch } from "../test/fetch";
import { rawItem, sizeTheBox } from "../test/galleryGrid";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

// The grid with stacks: the count on a tile, the Stack action of the selection bar, and the grid reloading when the
// viewer changes a stack. The viewer is a stand-in that shows how the Gallery calls it.
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; onClose: () => void; onStackChanged?: (c: "primary" | "unstack" | "member") => void }) => (
    <div data-testid="viewer">
      <span>{p.items[p.index]?.id}</span>
      <button type="button" onClick={() => p.onStackChanged?.("unstack")}>
        unstack in viewer
      </button>
      <button type="button" onClick={() => p.onStackChanged?.("member")}>
        delete member in viewer
      </button>
    </div>
  ),
}));

const stackRef = (count: number) => ({ id: "stk_1", count, primary: true });

interface Backend {
  calls: Call[];
  lists: number;
  stacks: { ids: string[] }[];
}

function backend(opts: { refuse?: boolean; stacked?: boolean } = {}): Backend {
  const b: Backend = { calls: [], lists: 0, stacks: [] };
  const ids = ["a", "b", "c", "d"];
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 4, videos: 0 } } };
    if (url.pathname === "/v1/media/items/months") return { body: { months: [{ month: "2026-10", count: 4 }] } };
    if (url.pathname === "/v1/media/embeddings/status") return { status: 503, body: { error: { code: "off", message: "off" } } };
    if (url.pathname === "/v1/media/stacks" && call.method === "POST") {
      const body = call.body as { ids: string[] };
      b.stacks.push(body);
      if (opts.refuse) return { status: 409, body: { ok: false, error: "b: already_stacked", failed: [{ id: "b", reason: "already_stacked" }] } };
      const primary = body.ids[0]!;
      return {
        status: 201,
        body: { ok: true, stack: { id: "stk_new", kind: "manual", primary, count: body.ids.length, created_at: "2026-10-28T10:00:00Z", items: body.ids.map((id) => rawItem(id, { stack: { id: "stk_new", count: body.ids.length, primary: id === primary } })) } },
      };
    }
    if (url.pathname === "/v1/media/items" && call.method === "GET") {
      b.lists++;
      return {
        body: {
          items: ids.map((id, n) => rawItem(id, id === "a" && opts.stacked !== false ? { stack: stackRef(4) } : {}, n)),
          next_cursor: null,
        },
      };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function mount() {
  return renderAt("/gallery", () => <Route path="/gallery" component={() => <GalleryView />} />);
}

const tiles = () => document.querySelectorAll<HTMLElement>(".g-tile");
const open = (index: number) => document.querySelector<HTMLElement>(`.g-tile[data-index="${index}"] .g-open`)!;

beforeEach(() => {
  setLocale("en");
  sizeTheBox();
});
afterEach(() => vi.restoreAllMocks());

describe("stacks in the grid", () => {
  it("shows the count on a stack's tile and names it for screen readers", async () => {
    backend();
    mount();
    await waitFor(() => expect(tiles().length).toBe(4));
    const badge = document.querySelector(".g-tile[data-index='0'] .g-badge--stack");
    expect(badge).toHaveTextContent("4");
    expect(open(0).getAttribute("aria-label")).toContain("Stack of 4 photos");
    expect(document.querySelector(".g-tile[data-index='1'] .g-badge--stack")).toBeNull();
  });

  it("links to the duplicate review from the header", async () => {
    backend();
    mount();
    expect(await screen.findByRole("link", { name: "Duplicates" })).toHaveAttribute("href", "/gallery/duplicates");
  });

  it("offers Stack only with two or more photos selected, and stacks them into one tile", async () => {
    const b = backend({ stacked: false });
    mount();
    await waitFor(() => expect(tiles().length).toBe(4));
    fireEvent.click(open(0), { ctrlKey: true });
    expect(within(screen.getByRole("toolbar")).queryByRole("button", { name: "Stack" })).toBeNull();
    fireEvent.click(open(1), { ctrlKey: true });
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Stack" }));
    await waitFor(() => expect(b.stacks).toEqual([{ ids: ["a", "b"] }]));
    // One tile left for the two, with the count; the selection is over.
    await waitFor(() => expect(tiles().length).toBe(3));
    expect(document.querySelector(".g-tile[data-index='0'] .g-badge--stack")).toHaveTextContent("2");
    expect(screen.queryByRole("toolbar")).toBeNull();
    expect(await screen.findByText("Stacked 2 photos.")).toBeInTheDocument();
  });

  it("keeps the tiles and says so when the server refuses the stack", async () => {
    backend({ refuse: true, stacked: false });
    mount();
    await waitFor(() => expect(tiles().length).toBe(4));
    fireEvent.click(open(0), { ctrlKey: true });
    fireEvent.click(open(1), { ctrlKey: true });
    fireEvent.click(within(screen.getByRole("toolbar")).getByRole("button", { name: "Stack" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't stack those photos");
    expect(tiles().length).toBe(4);
    expect(screen.getByRole("toolbar")).toBeInTheDocument();
  });

  it("closes the viewer and reloads the grid when the viewer changed a stack", async () => {
    const b = backend();
    mount();
    await waitFor(() => expect(tiles().length).toBe(4));
    const before = b.lists;
    fireEvent.click(open(0));
    fireEvent.click(await screen.findByRole("button", { name: "unstack in viewer" }));
    expect(screen.queryByTestId("viewer")).toBeNull();
    await waitFor(() => expect(b.lists).toBeGreaterThan(before));
    expect(await screen.findByText("Stack removed. The photos are back in the grid.")).toBeInTheDocument();
  });
});
