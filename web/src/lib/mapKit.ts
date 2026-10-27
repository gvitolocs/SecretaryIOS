// Pure helpers for the in-app maps (docs: the shared maps contract): the pin
// model of LucinaMap, distances and the "≈" travel estimates, the external
// directions URLs, how a set of pins is framed, shared map links and the
// `/map/place` route. Nothing here touches MapLibre or the network.

export type PinTint = "accent" | "clearance" | "neutral" | "home";

export interface LatLng {
  lat: number;
  lng: number;
}

export interface MapPin extends LatLng {
  id: string;
  title: string;
  subtitle?: string;
  /** Short text shown under the pin (e.g. the place name). */
  label?: string;
  tint?: PinTint;
  selected?: boolean;
}

/** A place on the map: a `place` block, `meta.place` or a shared location. */
export interface PlaceInfo {
  title: string;
  address?: string;
  lat?: number;
  lng?: number;
  /** RFC 3339 start (appointments). */
  when?: string;
  end?: string;
  eventId?: string;
  kind: "event" | "place";
}

/** Home (Skejbygårdsvej 3, 8240 Risskov): the fallback origin. */
export const DEFAULT_HOME: LatLng & { label: string } = { lat: 56.19633, lng: 10.2011, label: "Home" };

/** Single pin (and the place screen): the zoom it opens at. */
export const PIN_ZOOM = 15;
/** Fit-to-pins never zooms in further than this. */
export const MAX_FIT_ZOOM = 16;
/** The place screen frames the user and the place together within this. */
export const NEAR_M = 50_000;

/** Finite, in range and not the (0,0) "no fix" placeholder. */
export function validLatLng(lat: unknown, lng: unknown): boolean {
  return (
    typeof lat === "number" &&
    typeof lng === "number" &&
    Number.isFinite(lat) &&
    Number.isFinite(lng) &&
    Math.abs(lat) <= 90 &&
    Math.abs(lng) <= 180 &&
    !(lat === 0 && lng === 0)
  );
}

export function hasPoint(p: { lat?: number; lng?: number }): p is { lat: number; lng: number } {
  return validLatLng(p.lat, p.lng);
}

const EARTH_M = 6_371_008.8;

/** Great-circle distance in metres. */
export function distanceM(a: LatLng, b: LatLng): number {
  const rad = Math.PI / 180;
  const dLat = (b.lat - a.lat) * rad;
  const dLng = (b.lng - a.lng) * rad;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.sin(dLng / 2) ** 2;
  return 2 * EARTH_M * Math.asin(Math.min(1, Math.sqrt(h)));
}

/** "240 m", "1.2 km", "37 km" in the given locale. */
export function formatDistance(m: number, locale: string): string {
  if (m < 1000) return `${Math.max(0, Math.round(m / 10) * 10)} m`;
  const km = m / 1000;
  const n = new Intl.NumberFormat(locale, { maximumFractionDigits: km < 10 ? 1 : 0 }).format(km);
  return `${n} km`;
}

export type TravelMode = "walk" | "bike" | "car";

/** Straight-line speeds (km/h) of the web estimate; no routing engine. */
export const TRAVEL_KMH: Record<TravelMode, number> = { walk: 4.8, bike: 15, car: 35 };
/** Roads are not straight lines. */
export const DETOUR = 1.3;

/** Estimated minutes per mode for a straight-line distance (at least 1). */
export function travelEstimates(m: number): Record<TravelMode, number> {
  const minutes = (kmh: number) => Math.max(1, Math.round(((m * DETOUR) / 1000 / kmh) * 60));
  return { walk: minutes(TRAVEL_KMH.walk), bike: minutes(TRAVEL_KMH.bike), car: minutes(TRAVEL_KMH.car) };
}

/** "12 min", "1 h 5 min", "3 h". */
export function formatMinutes(min: number): string {
  if (min < 60) return `${min} min`;
  const h = Math.floor(min / 60);
  const rest = min % 60;
  return rest ? `${h} h ${rest} min` : `${h} h`;
}

/** Apple Maps directions (opened only on the owner's tap). */
export function appleMapsUrl(p: LatLng & { title?: string }): string {
  const q = p.title ? `&q=${encodeURIComponent(p.title)}` : "";
  return `https://maps.apple.com/?daddr=${p.lat},${p.lng}${q}`;
}

/** Google Maps directions (opened only on the owner's tap). */
export function googleDirectionsUrl(p: LatLng): string {
  return `https://www.google.com/maps/dir/?api=1&destination=${p.lat},${p.lng}`;
}

/** Google Maps search, for places the local geocoder could not find. */
export function googleSearchUrl(query: string): string {
  return `https://www.google.com/maps/search/?api=1&query=${encodeURIComponent(query)}`;
}

/** Padding (px) around fitted pins: room for pin labels, capped on small maps. */
export function fitPadding(width: number, height: number): { top: number; right: number; bottom: number; left: number } {
  const side = Math.max(16, Math.min(64, Math.round(width * 0.12)));
  const vert = Math.max(16, Math.min(64, Math.round(height * 0.12)));
  // pins hang above their point and labels sit below: a little more at both ends
  return { top: vert + 24, right: side, bottom: vert + 16, left: side };
}

export type FitPlan =
  | { kind: "none" }
  | { kind: "center"; center: LatLng; zoom: number }
  | { kind: "bounds"; sw: LatLng; ne: LatLng };

/** How to frame a set of points: nothing, one point at PIN_ZOOM, or their bounds. */
export function fitPlan(points: readonly LatLng[]): FitPlan {
  const pts = points.filter((p) => validLatLng(p.lat, p.lng));
  if (!pts.length) return { kind: "none" };
  const lats = pts.map((p) => p.lat);
  const lngs = pts.map((p) => p.lng);
  const sw = { lat: Math.min(...lats), lng: Math.min(...lngs) };
  const ne = { lat: Math.max(...lats), lng: Math.max(...lngs) };
  if (pts.length === 1 || (sw.lat === ne.lat && sw.lng === ne.lng)) return { kind: "center", center: pts[0]!, zoom: PIN_ZOOM };
  return { kind: "bounds", sw, ne };
}

/** The place screen: the place with the user when they're within NEAR_M, else the place alone. */
export function placeFitPoints(place: LatLng, user: LatLng | null): LatLng[] {
  return user && distanceM(place, user) <= NEAR_M ? [place, user] : [place];
}

/** Ids of a pin set, order-insensitive: refit only when this changes. */
export function pinSetKey(pins: readonly { id: string }[]): string {
  return pins
    .map((p) => p.id)
    .sort()
    .join("\u0000");
}

/** Absolute URL of a server path (MapLibre resolves URLs in its worker). */
export function absoluteUrl(path: string, base = typeof location === "undefined" ? "http://localhost/" : location.href): string {
  return new URL(path, base).href;
}

function coordPair(s: string | null | undefined): LatLng | null {
  if (!s) return null;
  const m = /^\s*(-?\d{1,3}(?:\.\d+)?)\s*,\s*(-?\d{1,3}(?:\.\d+)?)/.exec(s);
  if (!m) return null;
  const lat = Number(m[1]);
  const lng = Number(m[2]);
  return validLatLng(lat, lng) ? { lat, lng } : null;
}

function plusDecoded(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, " ")).trim();
  } catch {
    return s.trim();
  }
}

/**
 * A place from a shared map link: Apple Maps (`ll`, `q`, `address`, `daddr`),
 * Google Maps (`/@lat,lng`, `/place/<name>`, `query`, `q`, `destination`) or a
 * `geo:` URI. Null for anything else (and for short links we can't resolve
 * without asking a third party).
 */
export function placeFromMapsLink(raw: string): Omit<PlaceInfo, "kind"> | null {
  const s = raw.trim();
  if (/^geo:/i.test(s)) {
    const body = s.slice(4);
    const [path = "", query = ""] = body.split("?");
    const params = new URLSearchParams(query);
    const q = params.get("q") ?? "";
    // geo:0,0?q=lat,lng(Label) or geo:0,0?q=Some+address
    const qPoint = coordPair(q);
    const label = /\(([^)]+)\)\s*$/.exec(q)?.[1]?.trim();
    const point = coordPair(path) ?? qPoint;
    const address = !qPoint && q ? q.trim() : undefined;
    const title = label || address || (point ? `${point.lat.toFixed(5)}, ${point.lng.toFixed(5)}` : "");
    if (!title) return null;
    return { title, ...(address ? { address } : {}), ...(point ?? {}) };
  }
  let url: URL;
  try {
    url = new URL(s);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  const host = url.hostname.toLowerCase();
  const p = url.searchParams;
  if (host === "maps.apple.com") {
    const point = coordPair(p.get("ll")) ?? coordPair(p.get("coordinate")) ?? coordPair(p.get("daddr")) ?? coordPair(p.get("q"));
    const name = p.get("q") && !coordPair(p.get("q")) ? p.get("q")!.trim() : p.get("name")?.trim();
    const address = p.get("address")?.trim() || (p.get("daddr") && !coordPair(p.get("daddr")) ? p.get("daddr")!.trim() : undefined);
    const title = name || address || (point ? `${point.lat.toFixed(5)}, ${point.lng.toFixed(5)}` : "");
    if (!title) return null;
    return { title, ...(address ? { address } : !point && name ? { address: name } : {}), ...(point ?? {}) };
  }
  const google = /(^|\.)google\.[a-z.]+$/.test(host) && url.pathname.startsWith("/maps");
  if (google || host === "maps.google.com") {
    const at = /@(-?\d{1,3}\.\d+),(-?\d{1,3}\.\d+)/.exec(url.pathname);
    const atPoint = at ? coordPair(`${at[1]},${at[2]}`) : null;
    const place = /\/maps\/place\/([^/@]+)/.exec(url.pathname)?.[1];
    const named = place ? plusDecoded(place) : undefined;
    const q = p.get("query") ?? p.get("q") ?? p.get("destination") ?? p.get("daddr");
    const qPoint = coordPair(q);
    const point = qPoint ?? coordPair(named) ?? atPoint;
    const text = (q && !qPoint ? q.trim() : undefined) || (named && !coordPair(named) ? named : undefined);
    const title = text || (point ? `${point.lat.toFixed(5)}, ${point.lng.toFixed(5)}` : "");
    if (!title) return null;
    return { title, ...(text && !point ? { address: text } : {}), ...(point ?? {}) };
  }
  return null;
}

/** In-app path of the place screen for a place. */
export function placeRoute(p: Omit<PlaceInfo, "kind"> & { kind?: PlaceInfo["kind"] }): string {
  const q = new URLSearchParams();
  if (hasPoint(p)) {
    q.set("lat", String(p.lat));
    q.set("lng", String(p.lng));
  }
  q.set("title", p.title);
  if (p.address) q.set("address", p.address);
  if (p.when) q.set("when", p.when);
  if (p.end) q.set("end", p.end);
  return `/map/place?${q.toString()}`;
}

/** The place screen's query back into a place (null without a title or address). */
export function placeFromQuery(search: string | URLSearchParams): PlaceInfo | null {
  const q = typeof search === "string" ? new URLSearchParams(search) : search;
  const clip = (v: string | null, max: number) => (v ?? "").replace(/\s+/g, " ").trim().slice(0, max);
  const address = clip(q.get("address"), 200);
  const title = clip(q.get("title"), 120) || address;
  if (!title) return null;
  const lat = Number(q.get("lat") ?? NaN);
  const lng = Number(q.get("lng") ?? NaN);
  const out: PlaceInfo = { title, kind: "place" };
  if (address) out.address = address;
  if (q.get("lat") !== null && q.get("lng") !== null && validLatLng(lat, lng)) {
    out.lat = lat;
    out.lng = lng;
  }
  const when = q.get("when");
  if (when && isRfc3339(when)) out.when = when;
  const end = q.get("end");
  if (end && isRfc3339(end)) out.end = end;
  return out;
}

const RFC3339 = /^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/;

export function isRfc3339(s: string): boolean {
  return RFC3339.test(s) && !Number.isNaN(Date.parse(s));
}

/** "Tue 27 Oct, 18:00–21:00" (same day) or "Tue 27 Oct, 18:00 – Wed 28 Oct, 09:00". */
export function placeTimeLabel(when: string, end: string | undefined, locale: string): string {
  const start = new Date(when);
  if (Number.isNaN(start.getTime())) return "";
  const dayFmt = new Intl.DateTimeFormat(locale, { weekday: "short", day: "numeric", month: "short" });
  const timeFmt = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" });
  const head = `${dayFmt.format(start)}, ${timeFmt.format(start)}`;
  const stop = end ? new Date(end) : null;
  if (!stop || Number.isNaN(stop.getTime()) || stop <= start) return head;
  if (dayFmt.format(stop) === dayFmt.format(start) && stop.getTime() - start.getTime() < 86_400_000) {
    return `${head}–${timeFmt.format(stop)}`;
  }
  return `${head} – ${dayFmt.format(stop)}, ${timeFmt.format(stop)}`;
}

export type RouteMode = "transit" | "walk" | "bike" | "drive";
export const ROUTE_MODES: RouteMode[] = ["transit", "walk", "bike", "drive"];

export function defaultRouteMode(meters: number, saved?: string | null): RouteMode {
  return ROUTE_MODES.includes(saved as RouteMode) ? saved as RouteMode : meters > 2000 ? "transit" : "walk";
}

export function localTime(value: string, language: string): string {
  return new Intl.DateTimeFormat(language, { hour: "2-digit", minute: "2-digit" }).format(new Date(value));
}
