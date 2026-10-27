// Deals map (Lucina's deal engine, `/v1/grocery/deals/map`). Parsing is as
// forgiving as shoppingOps: bad shared fields only cost themselves, a store
// just needs id, name and coordinates.

import { ApiError } from "../api/client";

export type DealSource = "food_waste" | "flyer";
export type DealStatus = "active" | "upcoming";

export interface Deal {
  key: string;
  source: DealSource;
  name: string;
  priceDkk: number;
  normalPriceDkk: number | null;
  discountPct: number | null;
  unitPrice: string | null;
  stock: number | null;
  stockUnit: string | null;
  validFrom: string;
  validTo: string;
  status: DealStatus;
  category: string | null;
}

export interface ListMatch {
  item: string;
  ruleId: string;
  deal: Deal;
}

export interface MapStore {
  id: string;
  chain: string | null;
  chainKey: string | null;
  name: string;
  street: string | null;
  lat: number;
  lng: number;
  distanceM: number | null;
  /** List items this store covers, plus this chain's matching flyer offers. */
  listMatches: ListMatch[];
  /** Food-waste deals at this store, best discount first. */
  clearances: Deal[];
  flyerOffers: Deal[];
}

export interface MapsKeys {
  webKey: string | null;
  iosKey: string | null;
  mapId: string | null;
}

export interface DealsMapData {
  generatedAt: string;
  home: { lat: number; lng: number; label: string | null };
  radiusM: number;
  failures: string[];
  maps: MapsKeys;
  stores: MapStore[];
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v.trim() : null;
}

function num(v: unknown): number | null {
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function arr(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

function parseDeal(raw: unknown): Deal | null {
  if (!isRecord(raw)) return null;
  const name = str(raw.name);
  const price = num(raw.priceDkk);
  const key = str(raw.key);
  if (!name || price === null) return null;
  const pct = num(raw.discountPct);
  const stock = num(raw.stock);
  return {
    key: key ?? `${str(raw.source) ?? "x"}:${name}:${price}`,
    source: raw.source === "flyer" ? "flyer" : "food_waste",
    name,
    priceDkk: price,
    normalPriceDkk: num(raw.normalPriceDkk),
    discountPct: pct !== null ? Math.round(Math.abs(pct)) : null,
    unitPrice: typeof raw.unitPrice === "string" || typeof raw.unitPrice === "number" ? String(raw.unitPrice) : null,
    stock: stock !== null ? Math.abs(stock) : null,
    stockUnit: str(raw.stockUnit),
    validFrom: str(raw.validFrom) ?? "",
    validTo: str(raw.validTo) ?? "",
    status: raw.status === "upcoming" ? "upcoming" : "active",
    category: str(raw.category),
  };
}

function parseListMatch(raw: unknown): ListMatch | null {
  if (!isRecord(raw)) return null;
  const item = str(raw.item);
  const deal = parseDeal(raw.deal);
  if (!item || !deal) return null;
  return { item, ruleId: str(raw.ruleId) ?? "", deal };
}

function parseStore(raw: unknown, i = 0): MapStore | null {
  if (!isRecord(raw)) return null;
  const id = str(raw.id) ?? `store-${i}`;
  const name = str(raw.name);
  const lat = num(raw.lat);
  const lng = num(raw.lng);
  if (!name || lat === null || lng === null) return null;
  const deals = (v: unknown) => arr(v).map(parseDeal).filter((d): d is Deal => d !== null);
  const clearances = deals(raw.clearances).sort(
    (a, b) => (b.discountPct ?? 0) - (a.discountPct ?? 0) || a.priceDkk - b.priceDkk,
  );
  return {
    id,
    chain: str(raw.chain),
    chainKey: str(raw.chainKey),
    name,
    street: str(raw.street),
    lat,
    lng,
    distanceM: num(raw.distanceM),
    listMatches: arr(raw.listMatches).map(parseListMatch).filter((m): m is ListMatch => m !== null),
    clearances: clearances.slice(0, 40),
    flyerOffers: deals(raw.flyerOffers),
  };
}

/** Defensive parse of `/v1/grocery/deals/map`; throws `bad_response` when unusable. */
export function parseDealsMap(raw: unknown): DealsMapData {
  const r = isRecord(raw) ? raw : {};
  const homeRaw = isRecord(r.home) ? r.home : {};
  const homeLat = num(homeRaw.lat);
  const homeLng = num(homeRaw.lng);
  if (homeLat === null || homeLng === null) {
    throw new ApiError(500, "bad_response", "The server sent an unexpected map.");
  }
  const home = { lat: homeLat, lng: homeLng, label: str(homeRaw.label) };
  const stores = arr(r.stores)
    .map(parseStore)
    .filter((s): s is MapStore => s !== null)
    .sort((a, b) => (a.distanceM ?? Infinity) - (b.distanceM ?? Infinity));
  const keys = isRecord(r.maps) ? r.maps : {};
  return {
    generatedAt: str(r.generatedAt) ?? "",
    home,
    radiusM: num(r.radiusM) ?? 3000,
    failures: arr(r.failures).map(String),
    maps: {
      webKey: str(keys.webKey),
      iosKey: str(keys.iosKey),
      mapId: str(keys.mapId),
    },
    stores,
  };
}

/** Chain initial for a marker chip ("føtex" -> "F", driver-upplied fallback "S"). */
export function chainInitial(store: MapStore): string {
  return ((store.chain ?? store.name).trim()[0] ?? "S").toUpperCase();
}

/** Stable hue (0..359) per chain for the marker chips. */
export function chainHue(store: MapStore): number {
  const key = store.chainKey ?? store.chain ?? store.name;
  let h = 0;
  for (let i = 0; i < key.length; i += 1) h = (h * 31 + key.charCodeAt(i)) % 360;
  return h;
}

/** "560 m" below a kilometre, "1.2 km" above. */
export function distanceLabel(m: number | null): string {
  if (m === null) return "";
  return m < 1000 ? `${Math.round(m)} m` : `${(m / 1000).toFixed(1)} km`;
}

/** Marker chip caption: "−NN%" of the best clearance, else the match count. */
export function chipCaption(store: MapStore): string | null {
  const best = store.clearances[0];
  if (best?.discountPct) return `−${best.discountPct}%`;
  const matches = store.listMatches.length;
  return matches > 0 ? String(matches) : null;
}
