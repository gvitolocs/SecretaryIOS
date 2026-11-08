// The map server (`secretary-maps`, maps contract section 5) for the dev mock.
//   GET /v1/maps/config  -> tiles null, unless MOCK_MAPS_TILES=<path to a .pmtiles file>
//   GET /v1/maps/tiles/region.pmtiles -> that file, with HTTP Range (206)
//   GET /v1/maps/assets/<path> -> files from MOCK_MAPS_ASSETS (fonts/, sprites/v4/), if set
//   GET /v1/maps/geocode?q= -> a tiny fixed table (case-insensitive substring match)

import { createReadStream, statSync } from "node:fs";
import { extname, resolve, sep } from "node:path";

const TILES = process.env.MOCK_MAPS_TILES ? resolve(process.env.MOCK_MAPS_TILES) : null;
const ASSETS = process.env.MOCK_MAPS_ASSETS ? resolve(process.env.MOCK_MAPS_ASSETS) : null;
const HOME = { lat: 56.19633, lng: 10.2011, label: "Home" };

const PLACES = [
  { name: "Folkehuset Møllestien", address: "Møllestien 37, 8000 Aarhus C", lat: 56.1611, lng: 10.2006 },
  { name: "Skejbygårdsvej 3", address: "Skejbygårdsvej 3, 8240 Risskov", lat: 56.19633, lng: 10.2011 },
  { name: "Universitetsbyen 76", address: "Universitetsbyen 76, 8000 Aarhus C", lat: 56.1704, lng: 10.1988 },
];

const TYPES = { ".pbf": "application/x-protobuf", ".json": "application/json", ".png": "image/png" };

function json(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" });
  res.end(JSON.stringify(body));
}

function stat(file) {
  try {
    const st = statSync(file);
    return st.isFile() ? st : null;
  } catch {
    return null;
  }
}

/** A file with single-range support (what pmtiles asks for). */
function serveFile(req, res, file, type) {
  const st = stat(file);
  if (!st) return json(res, 404, { error: { code: "not_found", message: "No such file." } });
  const headers = {
    "Content-Type": type,
    "Accept-Ranges": "bytes",
    ETag: `"${st.size.toString(16)}-${Math.floor(st.mtimeMs).toString(16)}"`,
    "Cache-Control": "private, max-age=86400",
  };
  const range = /^bytes=(\d*)-(\d*)$/.exec(String(req.headers.range ?? ""));
  if (!range || (range[1] === "" && range[2] === "")) {
    res.writeHead(200, { ...headers, "Content-Length": String(st.size) });
    return createReadStream(file).pipe(res);
  }
  let start = range[1] === "" ? Math.max(0, st.size - Number(range[2])) : Number(range[1]);
  let end = range[1] === "" || range[2] === "" ? st.size - 1 : Math.min(Number(range[2]), st.size - 1);
  if (start >= st.size || start > end) {
    res.writeHead(416, { ...headers, "Content-Range": `bytes */${st.size}` });
    return res.end();
  }
  res.writeHead(206, { ...headers, "Content-Length": String(end - start + 1), "Content-Range": `bytes ${start}-${end}/${st.size}` });
  createReadStream(file, { start, end }).pipe(res);
}

const dealsDay = (days) => new Date(Date.now() + days * 86_400_000).toISOString();
const mockDeal = (key, name, priceDkk, normalPriceDkk, source = "food_waste") => ({
  key, name, priceDkk, normalPriceDkk, discountPct: Math.round((1 - priceDkk / normalPriceDkk) * 100),
  source, validFrom: dealsDay(-1), validTo: dealsDay(2), stock: 4, unitPrice: null,
});

/** `GET /v1/grocery/deals/map`: three stores around home (the deals map preview). */
function dealsMap() {
  const prosciutto = mockDeal("f1", "Prosciutto crudo 80 g", 22, 30, "flyer");
  return {
    generatedAt: new Date().toISOString(),
    home: HOME,
    radiusM: 3000,
    failures: [],
    maps: { webKey: null, iosKey: null, mapId: null },
    stores: [
      { id: "tjek:4412", chain: "Netto", chainKey: "netto", name: "Netto Risskov", street: "Vestre Strandallé 1",
        lat: 56.1992, lng: 10.2116, distanceM: 240, listMatches: [], clearances: [], flyerOffers: [] },
      { id: "salling:foetex-skejby", chain: "føtex", chainKey: "foetex", name: "føtex Skejby", street: "Skelagervej 1-9",
        lat: 56.193657, lng: 10.193406, distanceM: 560,
        listMatches: [{ item: "Prosciutto crudo", ruleId: "prosciutto_crudo", deal: prosciutto }],
        clearances: [mockDeal("c1", "Parmesan 250 g", 34, 50), mockDeal("c2", "Smør", 12, 16)], flyerOffers: [prosciutto] },
      { id: "salling:kvickly-skejby", chain: "Kvickly", chainKey: "kvickly", name: "Kvickly Skejby", street: "Skejby Centret",
        lat: 56.1984, lng: 10.1868, distanceM: 1200, listMatches: [],
        clearances: [mockDeal("c3", "Rugbrød", 18, 22)], flyerOffers: [] },
    ],
  };
}

/** Handles `/v1/maps/*` (and the deals map's data); false for any other path. */
export function handleMaps(req, res, url, path) {
  if (path === "/v1/grocery/deals/map") {
    json(res, 200, dealsMap());
    return true;
  }
  if (!path.startsWith("/v1/maps/")) return false;
  if (req.method !== "GET" && req.method !== "HEAD") {
    json(res, 405, { error: { code: "method_not_allowed", message: "Method not allowed." } });
    return true;
  }
  if (path === "/v1/maps/config") {
    json(res, 200, {
      tiles: TILES ? "/v1/maps/tiles/region.pmtiles" : null,
      assets: "/v1/maps/assets",
      home: HOME,
      geocoder: true,
      attribution: "© OpenStreetMap contributors · Protomaps",
    });
    return true;
  }
  if (path === "/v1/maps/tiles/region.pmtiles" && TILES) {
    serveFile(req, res, TILES, "application/octet-stream");
    return true;
  }
  if (path.startsWith("/v1/maps/assets/") && ASSETS) {
    const file = resolve(ASSETS, `.${sep}${decodeURIComponent(path.slice("/v1/maps/assets/".length))}`);
    const type = TYPES[extname(file).toLowerCase()];
    if (file.startsWith(ASSETS + sep) && type) serveFile(req, res, file, type);
    else json(res, 404, { error: { code: "not_found", message: "No such asset." } });
    return true;
  }
  if (path === "/v1/maps/routes") {
    const from = (url.searchParams.get("from") ?? "56.19633,10.2011").split(",").map(Number);
    const to = (url.searchParams.get("to") ?? "56.1611,10.2006").split(",").map(Number);
    const arrive = Date.parse(url.searchParams.get("arrive") ?? "");
    const routes = [["transit", 1100], ["walk", 3300], ["bike", 1020], ["drive", 540]].map(([mode, durationS]) => ({
      mode, durationS, staticDurationS: mode === "drive" ? 480 : durationS, distanceM: 4600,
      polyline: [from, [(from[0] + to[0]) / 2, from[1] + 0.003], to],
      ...(Number.isFinite(arrive) ? { leaveAt: new Date(arrive - (durationS + 300) * 1000).toISOString() } : {}),
      ...(mode === "transit" ? { transit: [{ line: "2A", vehicle: "BUS", from: "Skejby", to: "Mølleparken", departAt: new Date((Number.isFinite(arrive) ? arrive - durationS * 1000 : Date.now()) + 120000).toISOString(), arriveAt: new Date(Number.isFinite(arrive) ? arrive : Date.now() + durationS * 1000).toISOString() }] } : {}),
    }));
    json(res, 200, { provider: "google", routes, failed: [] });
    return true;
  }
  if (path === "/v1/maps/geocode") {
    const q = (url.searchParams.get("q") ?? "").trim().toLowerCase();
    const limit = Math.min(5, Math.max(1, Number(url.searchParams.get("limit")) || 1));
    const results = q ? PLACES.filter((p) => p.name.toLowerCase().includes(q) || q.includes(p.name.toLowerCase())) : [];
    json(res, 200, { results: results.slice(0, limit) });
    return true;
  }
  json(res, 404, { error: { code: "not_found", message: "Not found." } });
  return true;
}
