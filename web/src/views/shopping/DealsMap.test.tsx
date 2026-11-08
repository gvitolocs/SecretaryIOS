import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { onMount } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../i18n";
import { Route } from "@solidjs/router";
import { resetShopping } from "../../state/shopping";
import { renderAt } from "../../test/router";
import { stubFetch, type Call } from "../../test/fetch";
import type { Deal, DealsMapData, MapStore } from "../../lib/dealsMap";
import type { LucinaMapProps } from "../../components/LucinaMap";
import type { MapPin } from "../../lib/mapKit";

// jsdom has no WebGL: LucinaMap is the boundary. The mock records its props,
// renders the pins through `renderPin` and reports whether tiles exist.
const lucinaMap = vi.hoisted(() => ({
  available: true,
  props: null as LucinaMapProps | null,
  renders: 0,
}));

vi.mock("../../components/LucinaMap", () => ({
  LucinaMap: (props: LucinaMapProps) => {
    lucinaMap.props = props;
    lucinaMap.renders++;
    onMount(() => props.onAvailable?.(lucinaMap.available));
    return lucinaMap.available ? <div data-testid="lucina-map" aria-label={props.label} /> : props.fallback;
  },
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

// The server still sends the old key fields; nothing reads them.
const noMaps = { webKey: null, iosKey: null, mapId: null };
const withKey = { webKey: "web-key-1", iosKey: null, mapId: "map-9" };

function pins(): MapPin[] {
  return lucinaMap.props?.pins ?? [];
}

function noGoogleScript() {
  expect(document.querySelector('script[src*="maps.googleapis.com"], script[src*="google"]')).toBeNull();
}

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
  lucinaMap.available = true;
  lucinaMap.props = null;
  lucinaMap.renders = 0;
});

describe("Deals map view", () => {
  it("lists the nearby stores sorted by distance and logs one GET; without tiles, a note instead of the map", async () => {
    lucinaMap.available = false;
    await renderMap(withKey, [foetex, netto, kvickly]);
    const list = await screen.findByRole("list", { name: "Offerte vicino a te" });
    const names = [...list.querySelectorAll(".deals-store-name")].map((n) => n.textContent);
    // Lucina sorts by distance; 240 m < 560 m < 1200 m
    expect(names).toEqual(["Netto Risskov", "føtex Skejby", "Kvickly Skejby"]);
    expect(list.textContent).toContain("240 m");
    expect(list.textContent).toContain("1.2 km");
    expect(list.textContent).toContain("Skelagervej 1-9");
    // the map panel is just the note, even with the (unused) Google key; no Google script
    expect(await screen.findByText("Le mappe non sono ancora installate.")).toBeInTheDocument();
    expect(screen.queryByTestId("lucina-map")).toBeNull();
    noGoogleScript();
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

  it("passes home and store pins to LucinaMap; a pin tap opens the store panel", async () => {
    await renderMap(noMaps, [foetex, netto, kvickly]);
    await screen.findByText("Netto Risskov");
    expect(await screen.findByTestId("lucina-map")).toHaveAttribute("aria-label", "Offerte vicino a te");
    expect(screen.queryByText("Le mappe non sono ancora installate.")).toBeNull();
    // one home pin plus one per store, at their coordinates and tinted by what they offer
    expect(pins().map((p) => [p.id, p.tint])).toEqual([
      ["home", "home"],
      ["tjek:4412", "neutral"],
      ["salling:0847307a-1111-4222-8333-9c0d1e2f3a4b", "clearance"],
      ["salling:kvik-1", "clearance"],
    ]);
    const foetexPin = pins().find((p) => p.title === "føtex Skejby")!;
    expect([foetexPin.lat, foetexPin.lng]).toEqual([56.193657, 10.193406]);
    expect(foetexPin.subtitle).toBe("Skelagervej 1-9");
    // the chip look: chain initial + best clearance badge; home stays the house
    const chip = lucinaMap.props!.renderPin!(foetexPin);
    expect(chip.querySelector(".deals-chip-letter")?.textContent).toBe("F");
    expect(chip.querySelector(".deals-chip-badge")?.textContent).toBe("−32%");
    expect(lucinaMap.props!.renderPin!(pins()[0]!).textContent).toBe("🏠");
    noGoogleScript();

    // the home pin selects nothing; a store pin opens its panel and marks the pin
    lucinaMap.props!.onSelect!("home");
    expect(screen.queryByRole("complementary")).toBeNull();
    lucinaMap.props!.onSelect!(foetexPin.id);
    expect(await screen.findByRole("complementary", { name: "føtex Skejby" })).toBeInTheDocument();
    await waitFor(() => expect(pins().find((p) => p.id === foetexPin.id)?.selected).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Chiudi føtex Skejby" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "føtex Skejby" })).toBeNull());

    // filters narrow the pins too (home always stays)
    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));
    await waitFor(() => expect(pins().map((p) => p.id)).toEqual(["home", foetexPin.id]));
    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));

    // a list tap opens the panel and asks the map to fly to the store
    fireEvent.click(screen.getByRole("button", { name: /^føtex Skejby/ }));
    await waitFor(() => expect(lucinaMap.props!.focus).toEqual({ id: foetexPin.id, tick: 1 }));
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
    // the store's directions are Lucina's own navigation, never another maps app
    const nav = within(panel).getByRole("link", { name: /Indicazioni/ });
    expect(nav.getAttribute("href")).toMatch(/^\/map\/place\?navigate=1&lat=56\.193657&lng=10\.193406&title=f%C3%B8tex\+Skejby/);
    expect(nav).not.toHaveAttribute("target");
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
