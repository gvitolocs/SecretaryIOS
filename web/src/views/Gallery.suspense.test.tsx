import { Route } from "@solidjs/router";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import { createResource, Suspense } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import GalleryView from "./Gallery";

// A viewer and a strip that are still loading, as on a slow connection: they read a resource that never answers.
const Pending = (p: { id: string }) => {
  const [never] = createResource(() => new Promise<string>(() => {}));
  return <div data-testid={p.id}>{never()}</div>;
};
vi.mock("../components/gallery/GalleryViewer", () => ({ GalleryViewer: () => <Pending id="viewer" /> }));
vi.mock("../components/gallery/browse/MemoriesStrip", () => ({ MemoriesStrip: () => <Pending id="memories" /> }));

const items = Array.from({ length: 300 }, (_, i) => {
  const day = new Date(2026, 9, 28 - Math.floor(i / 6), 12 - (i % 6)).toISOString();
  const id = `itm${i}`;
  return {
    id,
    kind: "photo",
    title: `${id}.jpg`,
    mime: "image/jpeg",
    size: 1000,
    taken_at: day,
    created_at: day,
    width: 4000,
    height: 3000,
    duration_ms: null,
    source: "ios-photos",
    device: "iphone-test",
    device_name: "T",
    origin: { filename: null, folder: null, imported_from: null },
    favorite: false,
    rotation: 0,
    albums: [],
    trashed_at: null,
    trash_expires_at: null,
    thumbhash: null,
    urls: { thumb: `/v1/media/items/${id}/thumb?size=256`, download: `/v1/media/items/${id}/original?download=1` },
  };
});

let observers: ResizeObserverCallback[] = [];
let boxHeight = 800;

beforeEach(() => {
  setLocale("en");
  boxHeight = 800;
  observers = [];
  vi.spyOn(Element.prototype, "clientWidth", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") && boxHeight ? 1000 : 0;
  });
  vi.spyOn(Element.prototype, "clientHeight", "get").mockImplementation(function (this: Element) {
    return this.classList.contains("g-scroll") ? boxHeight : 0;
  });
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(cb: ResizeObserverCallback) {
        observers.push(cb);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/sources") return { body: { sources: [], all: { photos: 300, videos: 0 } } };
    if (url.pathname === "/v1/media/items/months") return { body: { months: [{ month: "2026-10", count: 300 }] } };
    if (url.pathname === "/v1/media/items") return { body: { items, next_cursor: null } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function mount() {
  return renderAt("/gallery", () => (
    <Route
      path="/gallery"
      component={() => (
        <Suspense fallback={<div data-testid="route-fallback" />}>
          <GalleryView />
        </Suspense>
      )}
    />
  ));
}

const scroller = () => document.querySelector<HTMLElement>(".g-scroll");

describe("the Gallery under the route's Suspense", () => {
  it("shows the grid while the memories are still loading", async () => {
    mount();
    await waitFor(() => expect(document.querySelectorAll(".g-tile").length).toBeGreaterThan(0), { timeout: 5000 });
    expect(scroller()!.isConnected).toBe(true);
    expect(document.querySelector('[data-testid="route-fallback"]')).toBeNull();
  });

  it("keeps the grid on the page, and so its scroll position, while a photo's viewer loads", async () => {
    mount();
    await waitFor(() => expect(document.querySelectorAll(".g-tile .g-open").length).toBeGreaterThan(0));
    const grid = scroller()!;
    grid.scrollTop = 3000;
    fireEvent.click(document.querySelector<HTMLElement>(".g-tile .g-open")!);
    await new Promise((r) => setTimeout(r, 20));
    expect(document.querySelector('[data-testid="route-fallback"]')).toBeNull();
    expect(grid.isConnected).toBe(true);
    expect(scroller()).toBe(grid);
    expect(grid.scrollTop).toBe(3000);
  });

  it("puts the scroll position back when something else took the grid off the page", async () => {
    mount();
    await waitFor(() => expect(document.querySelectorAll(".g-tile").length).toBeGreaterThan(0));
    const grid = scroller()!;
    const report = () => observers.forEach((cb) => cb([{ contentRect: { width: boxHeight ? 1000 : 0 } } as ResizeObserverEntry], {} as ResizeObserver));
    grid.scrollTop = 3000;
    fireEvent.scroll(grid);
    await new Promise((r) => requestAnimationFrame(r));
    await new Promise((r) => setTimeout(r, 20));
    boxHeight = 0;
    report();
    grid.scrollTop = 0; // what the browser does to a detached element
    boxHeight = 800;
    report();
    expect(grid.scrollTop).toBe(3000);
  });
});
