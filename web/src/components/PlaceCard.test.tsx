import { render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { resetMapsConfig } from "../api/maps";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import type { LucinaMapProps } from "./LucinaMap";

// jsdom has no WebGL: the mini-map is a stub that records its pins.
const miniMaps = vi.hoisted(() => [] as LucinaMapProps[]);
vi.mock("./LucinaMap", () => ({
  LucinaMap: (props: LucinaMapProps) => {
    miniMaps.push(props);
    return <div data-testid="mini-map" />;
  },
}));

import { PlaceCard } from "./PlaceCard";

const CONFIG = {
  tiles: "/v1/maps/tiles/region.pmtiles",
  assets: "/v1/maps/assets",
  home: { lat: 56.19633, lng: 10.2011, label: "Home" },
  geocoder: true,
  attribution: "© OpenStreetMap contributors · Protomaps",
};

let calls: Call[];

function paths() {
  return calls.map((c) => {
    const u = new URL(c.url, "http://test.local");
    return `${u.pathname}${u.search}`;
  });
}

beforeEach(() => {
  setLocale("en");
  resetMapsConfig();
  miniMaps.length = 0;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("PlaceCard", () => {
  it("shows a known place with time, mini-map and the distance from home, without geocoding", async () => {
    calls = stubFetch((call) => (call.url === "/v1/maps/config" ? { body: CONFIG } : { status: 404, body: {} }));
    render(() => (
      <PlaceCard
        place={{
          title: "Hack Night: Rust meets AI",
          address: "Folkehuset Møllestien, Aarhus",
          lat: 56.1611,
          lng: 10.2006,
          when: "2026-10-27T17:00:00.000Z",
          end: "2026-10-27T20:00:00.000Z",
          kind: "event",
        }}
      />
    ));
    const link = screen.getByRole("link", { name: /Hack Night: Rust meets AI/ });
    expect(link.getAttribute("href")).toMatch(/^\/map\/place\?lat=56\.1611&lng=10\.2006&title=Hack\+Night/);
    expect(link).toHaveTextContent("Folkehuset Møllestien, Aarhus");
    expect(link.querySelector(".place-card-time")?.textContent).toMatch(/27/);
    expect(await screen.findByText("3.9 km from home")).toBeInTheDocument();
    expect(screen.getByTestId("mini-map")).toBeInTheDocument();
    expect(miniMaps[0]!.interactive).toBe(false);
    expect(miniMaps[0]!.pins).toEqual([{ id: "place", lat: 56.1611, lng: 10.2006, title: "Hack Night: Rust meets AI", tint: "accent" }]);
    expect(paths()).toEqual(["/v1/maps/config"]);
  });

  it("shows Lucina's travel time from home", async () => {
    calls = stubFetch((call) => (call.url === "/v1/maps/config" ? { body: CONFIG } : { status: 404, body: {} }));
    render(() => (
      <PlaceCard
        place={{ title: "Hack Night", address: "Folkehuset Møllestien", lat: 56.1611, lng: 10.2006, kind: "event",
                 travelMin: 16, travelMode: "bike", travelApprox: true }}
      />
    ));
    expect(screen.getByRole("link", { name: /Hack Night/ }).querySelector(".place-card-travel")?.textContent).toBe("Bike · ≈ 16 min");
  });

  it("geocodes an address near home and links the place screen with the result", async () => {
    calls = stubFetch((call) => {
      if (call.url === "/v1/maps/config") return { body: CONFIG };
      if (call.url.startsWith("/v1/maps/geocode")) {
        return { body: { results: [{ name: "Folkehuset Møllestien", address: "Møllestien 37, Aarhus", lat: 56.1611, lng: 10.2006 }] } };
      }
      return { status: 404, body: {} };
    });
    render(() => <PlaceCard place={{ title: "Hack Night", address: "Folkehuset Møllestien", kind: "event" }} />);
    expect(screen.getByRole("status")).toHaveTextContent("Finding it on the map…");
    await waitFor(() => expect(screen.getByTestId("mini-map")).toBeInTheDocument());
    expect(paths()).toEqual([
      "/v1/maps/config",
      "/v1/maps/geocode?q=Folkehuset+M%C3%B8llestien&limit=1&lat=56.19633&lng=10.2011",
    ]);
    expect(screen.getByRole("link", { name: /Hack Night/ }).getAttribute("href")).toContain("lat=56.1611&lng=10.2006");
    expect(await screen.findByText("3.9 km from home")).toBeInTheDocument();
  });

  it("says not found and offers a Maps search when the geocoder has nothing", async () => {
    calls = stubFetch((call) => {
      if (call.url === "/v1/maps/config") return { body: CONFIG };
      if (call.url.startsWith("/v1/maps/geocode")) return { body: { results: [] } };
      return { status: 404, body: {} };
    });
    const open = vi.fn();
    vi.stubGlobal("open", open);
    render(() => <PlaceCard place={{ title: "Mystery bar", kind: "place" }} />);
    expect(await screen.findByText("Not found on the map")).toBeInTheDocument();
    const search = screen.getByRole("link", { name: "Search in Maps" });
    expect(search).toHaveAttribute("href", "https://www.google.com/maps/search/?api=1&query=Mystery%20bar");
    expect(search).toHaveAttribute("target", "_blank");
    // nothing went to a third party: only our own endpoints were called
    expect(paths().every((p) => p.startsWith("/v1/maps/"))).toBe(true);
    expect(paths()[1]).toBe("/v1/maps/geocode?q=Mystery+bar&limit=1&lat=56.19633&lng=10.2011");
    expect(open).not.toHaveBeenCalled();
    expect(screen.queryByTestId("mini-map")).toBeNull();
  });

  it("falls back to home and not-found when the map server is missing", async () => {
    calls = stubFetch(() => ({ status: 404, body: {} }));
    render(() => <PlaceCard place={{ title: "Somewhere", address: "Nowhere 1", kind: "place" }} />);
    expect(await screen.findByText("Not found on the map")).toBeInTheDocument();
    expect(paths()).toEqual(["/v1/maps/config", "/v1/maps/geocode?q=Nowhere+1&limit=1&lat=56.19633&lng=10.2011"]);
  });
});
