// The self-hosted map server (`secretary-maps` behind `/v1/maps/*`): tile and
// asset locations, home, and the local geocoder. Plain `fetch` rather than
// `request`: a map that can't load (401 from forward_auth, 404 before the
// server exists, 503) is not a reason to sign the owner out or show an error,
// the caller just keeps its fallback.

import { validLatLng, isRfc3339, ROUTE_MODES, type RouteMode, type LatLng } from "../lib/mapKit";

export interface MapsConfig {
  /** Server path of the PMTiles archive; null while no tiles are installed. */
  tiles: string | null;
  /** Fonts and sprites root. */
  assets: string;
  home: (LatLng & { label: string | null }) | null;
  geocoder: boolean;
  attribution: string;
}

export interface GeocodeResult extends LatLng {
  name: string;
  address: string;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function path(v: unknown): string | null {
  return typeof v === "string" && v.startsWith("/") && !v.startsWith("//") && !v.includes("\\") ? v : null;
}

export function parseMapsConfig(raw: unknown): MapsConfig | null {
  if (!isRecord(raw)) return null;
  const h = raw.home;
  const home =
    isRecord(h) && validLatLng(h.lat, h.lng)
      ? { lat: h.lat as number, lng: h.lng as number, label: typeof h.label === "string" ? h.label : null }
      : null;
  return {
    tiles: path(raw.tiles),
    assets: path(raw.assets) ?? "/v1/maps/assets",
    home,
    geocoder: raw.geocoder === true,
    attribution: typeof raw.attribution === "string" && raw.attribution.trim() ? raw.attribution.trim() : "© OpenStreetMap contributors",
  };
}

async function getJson(url: string): Promise<unknown> {
  const res = await fetch(url, { headers: { Accept: "application/json" }, credentials: "same-origin" });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as unknown;
}

let config: Promise<MapsConfig | null> | null = null;

/** `/v1/maps/config`, once per page; null when the map server is unavailable. */
export function mapsConfig(): Promise<MapsConfig | null> {
  config ??= getJson("/v1/maps/config").then(parseMapsConfig, () => null);
  return config;
}

/** Tests: forget the cached config. */
export function resetMapsConfig(): void {
  config = null;
}

/** First results of the local geocoder near `near` (home by default on the server). */
export async function geocode(q: string, near?: LatLng | null, limit = 1): Promise<GeocodeResult[]> {
  const params = new URLSearchParams({ q, limit: String(limit) });
  if (near) {
    params.set("lat", String(near.lat));
    params.set("lng", String(near.lng));
  }
  const raw = await getJson(`/v1/maps/geocode?${params.toString()}`);
  if (!isRecord(raw) || !Array.isArray(raw.results)) return [];
  return raw.results
    .filter(isRecord)
    .filter((r) => validLatLng(r.lat, r.lng))
    .map((r) => ({
      name: typeof r.name === "string" ? r.name : "",
      address: typeof r.address === "string" ? r.address : "",
      lat: r.lat as number,
      lng: r.lng as number,
    }));
}

export interface TransitLeg {
  line: string;
  vehicle: string;
  from: string;
  to: string;
  departAt?: string;
  arriveAt?: string;
}
export interface MapRoute {
  mode: RouteMode;
  durationS: number;
  staticDurationS?: number;
  distanceM: number;
  /** Server coordinates are latitude first. */
  polyline: [number, number][];
  leaveAt?: string;
  transit: TransitLeg[];
}

export function parseRoutes(raw: unknown): MapRoute[] {
  if (!isRecord(raw) || !Array.isArray(raw.routes)) return [];
  const seen = new Set<string>();
  const seconds = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
  const date = (v: unknown) => typeof v === "string" && isRfc3339(v) ? v : undefined;
  return raw.routes.filter(isRecord).flatMap((r): MapRoute[] => {
    if (!ROUTE_MODES.includes(r.mode as RouteMode) || seen.has(String(r.mode)) || !seconds(r.durationS) || !seconds(r.distanceM)) return [];
    seen.add(String(r.mode));
    const polyline: [number, number][] = Array.isArray(r.polyline) && r.polyline.length <= 400 && r.polyline.every(p => Array.isArray(p) && p.length === 2 && validLatLng(p[0], p[1])) ? r.polyline as [number, number][] : [];
    const transit = Array.isArray(r.transit) ? r.transit.filter(isRecord).flatMap((leg): TransitLeg[] => {
      if (![leg.line, leg.vehicle, leg.from, leg.to].every(v => typeof v === "string")) return [];
      return [{ line: leg.line as string, vehicle: leg.vehicle as string, from: leg.from as string, to: leg.to as string, departAt: date(leg.departAt), arriveAt: date(leg.arriveAt) }];
    }) : [];
    return [{ mode: r.mode as RouteMode, durationS: r.durationS, staticDurationS: seconds(r.staticDurationS) ? r.staticDurationS : undefined, distanceM: r.distanceM, polyline, leaveAt: date(r.leaveAt), transit }];
  });
}

// Deduplicate requests for the same origin/place, including concurrent consumers.
const routeCache = new Map<string, { at: number; promise: Promise<MapRoute[]> }>();
export function mapRoutes(from: LatLng, to: LatLng, when?: string): Promise<MapRoute[]> {
  const params = new URLSearchParams({ from: `${from.lat.toFixed(4)},${from.lng.toFixed(4)}`, to: `${to.lat.toFixed(4)},${to.lng.toFixed(4)}` });
  if (when && isRfc3339(when) && Date.parse(when) > Date.now()) params.set("arrive", when);
  const key = params.toString();
  const cached = routeCache.get(key);
  if (cached && Date.now() - cached.at < 300_000) return cached.promise;
  const promise = getJson(`/v1/maps/routes?${key}`).then(parseRoutes, () => []);
  routeCache.set(key, { at: Date.now(), promise });
  if (routeCache.size > 100) routeCache.delete(routeCache.keys().next().value!);
  return promise;
}

export function resetRoutesCache(): void { routeCache.clear(); }
