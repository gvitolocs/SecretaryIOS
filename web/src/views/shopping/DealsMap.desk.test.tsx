import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { onMount } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../../i18n";
import { Route } from "@solidjs/router";
import { resetShopping } from "../../state/shopping";
import { renderAt } from "../../test/router";
import { stubWide } from "../../test/wide";
import { stubFetch } from "../../test/fetch";
import type { Deal, MapStore } from "../../lib/dealsMap";
import type { LucinaMapProps } from "../../components/LucinaMap";

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

const noMaps = { webKey: null, iosKey: null, mapId: null };

async function renderMap(stores: MapStore[], failures: string[] = []) {
  stubFetch((call) => {
    const url = new URL(call.url, "http://test.local");
    if (url.pathname === "/v1/grocery/deals/map") {
      return { body: { generatedAt: "2026-10-24T12:00:00.000Z", home: { lat: 56.19633, lng: 10.2011, label: "Risskov" }, radiusM: 3000, failures, maps: noMaps, stores } };
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
  stubWide();
});
afterEach(() => vi.unstubAllGlobals());

describe("Deals map desktop layout", () => {
  it("has the page header with a link back to the list, store chips, a stores card and the map card", async () => {
    await renderMap([foetex, netto, kvickly]);
    expect(await screen.findByRole("heading", { level: 1, name: "Offerte vicino a te" })).toBeInTheDocument();
    expect(document.querySelector(".deals-head")).toBeNull();
    expect(document.querySelector(".bring-back-pill")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(screen.getByRole("link", { name: /La lista/ })).toHaveAttribute("href", "/shopping");
    const chips = screen.getByRole("group", { name: "Offerte vicino a te" });
    expect(within(chips).getByRole("button", { name: "Dalla tua lista" })).toHaveClass("pg-btn");
    // Stores as rows, nearest first, with their offer counts.
    const list = await screen.findByRole("list", { name: "Offerte vicino a te" });
    expect(list).toHaveClass("pg-rows");
    expect([...list.querySelectorAll("button.pg-row-name")].map((n) => n.textContent)).toEqual(["Netto Risskov", "føtex Skejby", "Kvickly Skejby"]);
    expect(list.textContent).toContain("240 m");
    expect(list.textContent).toContain("Skelagervej 1-9");
    expect(await screen.findByTestId("lucina-map")).toBeInTheDocument();
    expect(document.querySelector(".deals-map-card")).not.toBeNull();
  });

  it("filters with the chips, opens a store from its row into a detail card, and closes it", async () => {
    await renderMap([foetex, netto, kvickly]);
    await screen.findByText("Netto Risskov");
    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));
    expect(screen.getByRole("button", { name: "føtex Skejby" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Netto Risskov" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Dalla tua lista" }));

    fireEvent.click(screen.getByRole("button", { name: "føtex Skejby" }));
    await waitFor(() => expect(lucinaMap.props!.focus).toEqual({ id: foetex.id, tick: 1 }));
    expect(screen.getByRole("button", { name: "føtex Skejby" })).toHaveAttribute("aria-current", "true");
    const panel = await screen.findByRole("complementary", { name: "føtex Skejby" });
    expect(within(panel).getByRole("heading", { level: 2, name: "føtex Skejby" })).toBeInTheDocument();
    expect(within(panel).getByRole("heading", { name: "Dalla tua lista" })).toBeInTheDocument();
    expect(within(panel).getByRole("link", { name: /Indicazioni/ }).getAttribute("href")).toMatch(/^\/map\/place\?navigate=1&lat=56\.193657/);
    fireEvent.click(within(panel).getByRole("button", { name: "Chiudi føtex Skejby" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "føtex Skejby" })).toBeNull());
  });

  it("shows the missing tiles as an empty state inside the map card", async () => {
    lucinaMap.available = false;
    await renderMap([netto]);
    const text = await screen.findByText("Le mappe non sono ancora installate.");
    expect(text.closest(".pg-empty")).not.toBeNull();
    expect(text.closest(".deals-map-card")).not.toBeNull();
    expect(screen.queryByTestId("lucina-map")).toBeNull();
  });

  it("shows an empty stores list and the foot note as a notice", async () => {
    await renderMap([], ["flyer"]);
    expect(await screen.findByText("Nessun supermercato nel raggio.")).toBeInTheDocument();
    expect(document.querySelector(".pg-empty")).not.toBeNull();
    expect(screen.getByRole("status")).toHaveClass("pg-notice");
  });

  it("keeps the header and shows an error card with a retry", async () => {
    stubFetch(() => ({ status: 503, body: { error: { code: "grocery_unavailable", message: "Lucina says no" } } }));
    renderAt("/shopping/map", () => <Route path="/shopping/map" component={DealsMapView} />);
    expect(await screen.findByRole("heading", { level: 1, name: "Offerte vicino a te" })).toBeInTheDocument();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveClass("pg-error");
    expect(alert).toHaveTextContent("Lucina says no");
    expect(within(alert).getByRole("button", { name: "Riprova" })).toBeInTheDocument();
  });
});
