import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../i18n";
import { Route } from "@solidjs/router";
import { resetShopping } from "../../state/shopping";
import { renderAt } from "../../test/router";
import { stubFetch, type Call } from "../../test/fetch";
import type { Deal, DealsMapData, MapStore } from "../../lib/dealsMap";
import { mockMaps } from "./fakeMaps";

// The Google Maps JS API never loads in tests: loadMaps is the boundary.
vi.mock("./mapLoader", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./mapLoader")>()),
  loadMaps: mockMaps.loadMaps,
}));

import DealsMapView from "./DealsMap";

const deal = (over: Partial<Deal>): Deal => ({
  key: "k1",
  source: "food_waste",
  name: "Parmesan 250 g",
  priceDkk: 29,
  normalPriceDkk: 39,
  discountPct: 26,
  unitPrice: null as string | null,
  stock: 3,
  stockUnit: "stk",
  validFrom: "2026-10-24T10:00:00.000Z",
  validTo: "2026-10-26T00:00:00.000Z",
  status: "active",
  category: null,
  ...over,
});

const prosciutto = deal({
  key: "f1",
  source: "flyer",
  name: "Prosciutto crudo",
  priceDkk: 55,
  normalPriceDkk: 79,
  discountPct: 30,
});

function store(over: Partial<MapStore> & Pick<MapStore, "id" | "name" | "distanceM">): MapStore {
  return {
    chain: "Netto",
    chainKey: "netto",
    street: null as string | null,
    lat: 56.2,
    lng: 10.2,
    listMatches: [],
    clearances: [],
    flyerOffers: [],
    ...over,
  };
}

const foetex = store({
  id: "salling:0847307a-1111-4222-8333-9c0d1e2f3a4b",
  chain: "føtex",
  chainKey: "foetex",
  name: "føtex Skejby",
  street: "Skelagervej 1-9",
  lat: 56.193657,
  lng: 10.193406,
  distanceM: 560,
  listMatches: [{ item: "Prosciutto crudo", ruleId: "prosciutto_crudo", deal: prosciutto }],
  clearances: [deal({ key: "c1", name: "Parmesan 250 g", discountPct: 32 }), deal({ key: "c2", name: "Smør", priceDkk: 12, discountPct: 26 })],
  flyerOffers: [prosciutto],
});
const netto = store({ id: "tjek:4412", name: "Netto Risskov", distanceM: 240, lat: 56.1992, lng: 10.2116 });
const kvickly = store({
  id: "salling:kvik-1",
  chain: "Kvickly",
  chainKey: "kvickly",
  name: "Kvickly Skejby",
  distanceM: 1200,
  clearances: [deal({ key: "c3", name: "Rugbrød", discountPct: 18 })],
});

const noMaps = { webKey: null, iosKey: null, mapId: null };
const withMaps = { webKey: "web-key-1", iosKey: null, mapId: "map-9" };

let calls: Call[];

async function renderMap(maps: DealsMapData["maps"], stores: MapStore[], failures: string[] = []) {
  calls = stubFetch((call) => {
    const url = new URL(call.url, "http://test.local");
    if (url.pathname === "/v1/grocery/deals/map") {
      return { body: { generatedAt: "2026-10-24T12:00:00.000Z", home: { lat: 56.19633, lng: 10.2011, label: "Risskov" }, radiusM: 3000, failures, maps, stores } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "No such route." } } };
  });
  renderAt("/shopping/map", () => <Route path="/shopping/map" component={DealsMapView} />);
}

beforeEach(() => {
  setLocale("it");
  resetShopping();
  mockMaps.reset();
});

describe("Deals map view", () => {
  it("lists the nearby stores sorted by distance and logs one GET; no web key, no loader", async () => {
    await renderMap(noMaps, [foetex, netto, kvickly]);
    const list = await screen.findByRole("list", { name: "Offerte vicino a te" });
    const names = [...list.querySelectorAll(".deals-store-name")].map((n) => n.textContent);
    // Lucina sorts by distance; 240 m < 560 m < 1200 m
    expect(names).toEqual(["Netto Risskov", "føtex Skejby", "Kvickly Skejby"]);
    expect(list.textContent).toContain("240 m");
    expect(list.textContent).toContain("1.2 km");
    expect(list.textContent).toContain("Skelagervej 1-9");
    // the map panel is just the note; nothing Maps-like is loaded
    expect(screen.getByText("Chiave mappa non configurata sul server.")).toBeInTheDocument();
    expect(mockMaps.loadMaps).not.toHaveBeenCalled();
    expect(calls.map((c) => `${c.method} ${new URL(c.url, "http://test.local").pathname}`)).toEqual(["GET /v1/grocery/deals/map"]);
  });

  it("filters by list matches, food-waste deals and chain", async () => {
    await renderMap(noMaps, [foetex, netto, kvickly]);
    await screen.findByText("Netto Risskov");

    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));
    expect(screen.getByRole("button", { name: /^føtex Skejby/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Netto Risskov/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Kvickly Skejby/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));
    expect(screen.getByRole("button", { name: /^Kvickly Skejby/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Anti-spreco" }));
    expect(screen.getByRole("button", { name: /^føtex Skejby/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Kvickly Skejby/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Netto Risskov/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Anti-spreco" }));

    fireEvent.click(screen.getByRole("button", { name: "Netto" }));
    expect(screen.getByRole("button", { name: /^Netto Risskov/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /føtex Skejby/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Kvickly Skejby/ })).toBeNull();
    // second tap on the same chain clears the filter again
    fireEvent.click(screen.getByRole("button", { name: "Netto" }));
    expect(screen.getByRole("button", { name: /^føtex Skejby/ })).toBeInTheDocument();
  });

  it("builds a map with chip markers via the mocked loader and opens the store panel", async () => {
    await renderMap(withMaps, [foetex, netto, kvickly]);
    await screen.findByText("Netto Risskov");
    await waitFor(() => expect(mockMaps.loadMaps).toHaveBeenCalledTimes(1));
    expect(mockMaps.loadMaps).toHaveBeenCalledWith({ key: "web-key-1", mapId: "map-9" });
    await waitFor(() => expect(mockMaps.made.maps).toHaveLength(1));
    expect((mockMaps.made.maps[0]!.opts as Record<string, unknown>).mapId).toBe("map-9");
    // one home pin (created once with the map) plus one chip marker per store
    await waitFor(() => expect(mockMaps.made.advanced).toHaveLength(4));
    expect((mockMaps.made.advanced[0]!.content as HTMLElement).textContent).toBe("🏠");
    const foetexChip = mockMaps.made.advanced.find((m) => m.title === "føtex Skejby")!;
    expect(foetexChip.position).toEqual({ lat: 56.193657, lng: 10.193406 });
    expect((foetexChip.content as HTMLElement).querySelector(".deals-chip-badge")?.textContent).toBe("−32%");
    // bounds fit home + every store
    expect(mockMaps.made.fit).toBeTruthy();
    expect(mockMaps.made.fitPoints).toHaveLength(4);

    fireEvent.click(screen.getByRole("button", { name: /^føtex Skejby/ }));
    const panel = await screen.findByRole("complementary", { name: "føtex Skejby" });
    expect(within(panel).getByRole("heading", { name: "føtex Skejby" })).toBeInTheDocument();
    expect(within(panel).getByRole("heading", { name: "Dalla tua lista" })).toBeInTheDocument();
    // the item is listed once as a list match and once as its deal row
    expect(within(panel).getAllByText("Prosciutto crudo")).toHaveLength(2);
    const txt = panel.textContent ?? "";
    expect(txt).toContain("55,00 kr");
    expect(txt).toContain("era 79,00 kr");
    expect(txt).toContain("−32%");
    expect(txt).toContain("Scorta 3 stk");
    expect(txt).toContain("valido fino al 26 ott");
    expect(within(panel).getByRole("link", { name: /Indicazioni/ })).toHaveAttribute(
      "href",
      "https://www.google.com/maps/dir/?api=1&destination=56.193657,10.193406",
    );
    fireEvent.click(within(panel).getByRole("button", { name: "Chiudi føtex Skejby" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "føtex Skejby" })).toBeNull());
  });

  it("maps failure sources to their labels in the foot note", async () => {
    await renderMap(noMaps, [netto], ["food_waste", "flyer"]);
    await screen.findByText("Netto Risskov");
    const note = document.querySelector(".deals-note--foot");
    expect(note?.textContent).toContain("Qualche fonte non risponde");
    expect(note?.textContent).toContain("Anti-spreco");
    expect(note?.textContent).toContain("Volantino");
  });

  it("shows a server error with a retry that recovers", async () => {
    let fail = true;
    calls = stubFetch((call) => {
      const url = new URL(call.url, "http://test.local");
      if (url.pathname === "/v1/grocery/deals/map") {
        if (fail) {
          fail = false;
          return { status: 503, body: { error: { code: "grocery_unavailable", message: "Lucina says no" } } };
        }
        return { body: { generatedAt: "2026-10-24T12:00:00.000Z", home: { lat: 56.19633, lng: 10.2011, label: "Risskov" }, radiusM: 3000, failures: [], maps: noMaps, stores: [netto] } };
      }
      return { status: 404, body: { error: { code: "not_found", message: "No such route." } } };
    });
    renderAt("/shopping/map", () => <Route path="/shopping/map" component={DealsMapView} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Lucina says no");
    fireEvent.click(screen.getByRole("button", { name: "Riprova" }));
    expect(await screen.findByRole("button", { name: /^Netto Risskov/ })).toBeInTheDocument();
    expect(calls.map((c) => `${c.method} ${new URL(c.url, "http://test.local").pathname}`)).toEqual([
      "GET /v1/grocery/deals/map",
      "GET /v1/grocery/deals/map",
    ]);
  });
});
