import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import type { LucinaMapProps } from "../components/LucinaMap";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import GalleryPlaces from "./GalleryPlaces";

// jsdom has no WebGL: the map is a stand-in that records its props, so the view's conversation with the server is
// tested through the pins it draws and the viewport it is told about.
const maps = vi.hoisted(() => [] as LucinaMapProps[]);
vi.mock("../components/LucinaMap", () => ({
  LucinaMap: (props: LucinaMapProps) => {
    maps.push(props);
    queueMicrotask(() => props.onAvailable?.(!available.off));
    return <div data-testid="map" />;
  },
}));
const available = vi.hoisted(() => ({ off: false }));
vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));

const raw = (id: string, place: Record<string, unknown> | null = { name: "Copenhagen", admin1: "Hovedstaden", lat: 55.7, lon: 12.5 }) => ({
  id,
  kind: "photo",
  title: id,
  mime: "image/jpeg",
  size: 1,
  taken_at: "2024-10-28T10:00:00Z",
  created_at: "2026-10-01T10:00:00Z",
  place,
  urls: { thumb: `/v1/media/items/${id}/thumb?size=256` },
});

const WORLD = {
  zoom: 3,
  total: 12,
  clusters: [
    { id: "7:3", count: 9, lat: 55.7, lon: 12.5, bbox: [12.4, 55.6, 12.6, 55.8], cover: raw("c1") },
    { id: "9:4", count: 3, lat: 41.9, lon: 12.5, bbox: [12.4, 41.8, 12.6, 42.0], cover: raw("c2", { name: "Rome", lat: 41.9, lon: 12.5 }) },
  ],
};

function backend(opts: { empty?: boolean; fail?: boolean } = {}): Call[] {
  return stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (url.pathname === "/v1/media/sources") {
      return { body: { sources: [{ id: "device:p1", label: "Phone", kind: "iphone", photos: 12, videos: 0, latest_taken_at: null }], all: { photos: 12, videos: 0 } } };
    }
    if (url.pathname === "/v1/media/items/places") {
      if (opts.fail) return { status: 500, body: { error: { code: "boom", message: "boom" } } };
      return { body: opts.empty ? { zoom: 3, total: 0, clusters: [] } : WORLD };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

const mount = () =>
  renderAt("/gallery/places", () => (
    <>
      <Route path="/gallery/places" component={GalleryPlaces} />
      <Route path="/gallery" component={() => <p>the grid</p>} />
    </>
  ));

const placeCalls = (calls: Call[]) => calls.filter((c) => c.url.includes("/items/places")).map((c) => new URL(c.url, "http://x").searchParams);
const lastMap = () => maps[maps.length - 1]!;

beforeEach(() => {
  setLocale("en");
  maps.length = 0;
  available.off = false;
  try {
    sessionStorage.clear();
  } catch {
    // ignore
  }
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("Places", () => {
  it("asks for the world first and draws a pin per cluster", async () => {
    const calls = backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    const first = placeCalls(calls)[0]!;
    expect(Object.fromEntries(first)).toEqual({ origin: "all", kind: "photo,video", zoom: "3", limit: "300" });
    const pin = lastMap().pins[0]!;
    expect(pin).toMatchObject({ id: "7:3", lat: 55.7, lng: 12.5, title: "Open 9 photos here", subtitle: "Copenhagen, Hovedstaden" });
    expect(lastMap().fit).toBe("first");
    expect(screen.getByRole("heading", { name: "Places" })).toBeInTheDocument();
  });

  it("draws each pin as its cover thumbnail with a count badge", async () => {
    backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    const el = lastMap().renderPin!(lastMap().pins[0]!);
    expect(el.tagName).toBe("BUTTON");
    expect(el.querySelector("img")).toHaveAttribute("src", "/v1/media/items/c1/thumb?size=256");
    expect(el.querySelector(".g-cluster__count")).toHaveTextContent("9");
  });

  it("opens the cluster's pictures in the Gallery grid when its pin is chosen", async () => {
    backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    lastMap().onSelect!("9:4");
    expect(await screen.findByText("the grid")).toBeInTheDocument();
  });

  it("asks again, after a pause, for the window and the zoom the map moved to", async () => {
    const calls = backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    vi.useFakeTimers({ shouldAdvanceTime: true });
    lastMap().onViewport!({ bbox: [10, 50, 14, 58], zoom: 7 });
    lastMap().onViewport!({ bbox: [10.5, 50, 14.5, 58], zoom: 7.2 });
    await vi.advanceTimersByTimeAsync(400);
    const asked = placeCalls(calls);
    // One call for the settled window (the two moves in a row count once), at MapLibre zoom + 1.
    expect(asked).toHaveLength(2);
    expect(asked[1]!.get("bbox")).toBe("10.5,50,14.5,58");
    expect(asked[1]!.get("zoom")).toBe("8.2");
  });

  it("filters by type and by source", async () => {
    const calls = backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    fireEvent.click(screen.getByRole("button", { name: "Videos" }));
    await waitFor(() => expect(placeCalls(calls).some((p) => p.get("kind") === "video")).toBe(true));
    fireEvent.click(await screen.findByRole("button", { name: /^Phone/ }));
    await waitFor(() => expect(placeCalls(calls).some((p) => p.get("origin") === "device:p1")).toBe(true));
  });

  it("lists the places when the server has no map tiles", async () => {
    available.off = true;
    backend();
    mount();
    expect(await screen.findByText("The map isn't available on this server. Showing the places as a list.")).toBeInTheDocument();
    const rows = await screen.findAllByRole("link", { name: /photos/ });
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByText("Copenhagen, Hovedstaden")).toBeInTheDocument();
    expect(rows[0]).toHaveAttribute("href", "/gallery?bbox=12.39998%2C55.59998%2C12.60002%2C55.80002");
  });

  it("says when nothing has a position, and when loading failed", async () => {
    backend({ empty: true });
    const { unmount } = mount();
    expect(await screen.findByText("No photos with a location yet.")).toBeInTheDocument();
    unmount();
    backend({ fail: true });
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the places.");
  });
});

describe("Places on the page system (desktop)", () => {
  it("uses the shared Gallery header with Places current and the map on a card", async () => {
    stubWide();
    backend();
    mount();
    await waitFor(() => expect(lastMap()?.pins).toHaveLength(2));
    expect(screen.getByRole("heading", { level: 1, name: "Gallery" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation", { name: "Gallery sections" });
    expect(within(nav).getByRole("link", { name: "Places" })).toHaveAttribute("aria-current", "page");
    expect(document.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
    expect(document.querySelector(".g-bar")).toBeNull();
    expect(screen.getByRole("region", { name: "Map of where your photos were taken" })).toHaveClass("pg-card");
    expect(screen.getByRole("group", { name: "Show" })).toBeInTheDocument();
  });

  it("lists the places on a list card without map tiles", async () => {
    stubWide();
    available.off = true;
    backend();
    mount();
    const rows = await screen.findAllByRole("link", { name: /Copenhagen|Rome/ });
    expect(rows).toHaveLength(2);
    expect(rows[0]!.closest(".pg-card--list")).not.toBeNull();
  });

  it("shows the empty state and the error state", async () => {
    stubWide();
    backend({ empty: true });
    const { unmount } = mount();
    await waitFor(() => expect(document.querySelector(".pg-empty")).toHaveTextContent("No photos with a location yet."));
    unmount();
    backend({ fail: true });
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the places.");
  });
});
