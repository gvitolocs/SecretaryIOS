# secretary-maps

Maps for the Secretary apps: self-hosted Protomaps PMTiles, fonts and sprites, with Google
Places API (New) Text Search and Google Routes API called by this server. The Google server key
stays on the server and travels only in `X-Goog-Api-Key`; clients never receive it. Web and iOS
render Google Maps (Maps JavaScript API, Maps SDK for iOS) with the client keys from `/config`
(the web key referrer-restricted to `https://getsecretary.tech/*`, the iOS key restricted to the
bundle `tech.getsecretary.app`); the self-hosted tiles stay for other clients.

Google is pluggable and never required: without a key (or when Google fails) geocoding falls
back to the gazetteer and a self-hosted Photon, routing to a self-hosted OSRM, and, for callers
that ask with `estimate=1` (Lucina), to straight-line estimates (the MK4 kernel). When nothing
answers, the route is 503 and the apps show their own "≈" estimates.

The kernels (coordinates, bounds, travel estimates, the `place` normalizer, the asset path rule)
are in `src/kernel.rs` and verified in `verification/verus/map_kernels.rs` (protocol 15 in
`docs/verification.md`).

## Environment

| Variable | Default | |
|---|---|---|
| `MAPS_BIND` | `127.0.0.1:8840` | listen address (nezopt: the tailnet address) |
| `MAPS_DIR` | `~/data/maps` | data root, layout below |
| `MAPS_TILES` | `region` | tiles file name without `.pmtiles`, `[a-z0-9-]{1,40}` |
| `MAPS_HOME_LAT`, `MAPS_HOME_LNG` | unset | home; `home` is `null` when either is unset or invalid (out of range, (0, 0), not a number) |
| `MAPS_HOME_LABEL` | `Home` | |
| `GOOGLE_MAPS_SERVER_KEY` | unset | the key itself (the Infisical secret's name); wins over the key file |
| `MAPS_GOOGLE_KEY_FILE` | `~/.config/secretary-maps/google.key` | trimmed at startup; a missing, empty or unreadable key turns Google off (never a startup error); restart after rotation |
| `GOOGLE_MAPS_WEB_KEY` | unset | the web client key (Maps JavaScript API, referrer-restricted); wins over its file; served in `/config` |
| `MAPS_GOOGLE_WEB_KEY_FILE` | `~/.config/secretary-maps/google-web.key` | as the server key file; a key that is not 20..=100 chars of `[A-Za-z0-9_-]` is logged (without the value) and served as `null` |
| `GOOGLE_MAPS_IOS_KEY` | unset | the iOS client key (Maps SDK for iOS, bundle-restricted); wins over its file; served in `/config` |
| `MAPS_GOOGLE_IOS_KEY_FILE` | `~/.config/secretary-maps/google-ios.key` | as the web key file |
| `MAPS_AUFIND_URL` | unset (off) | AU Find base URL, e.g. `http://127.0.0.1:18211`; university building lookup at `/api/buildings?q=` with a 1.5 s deadline |
| `MAPS_PHOTON_URL` | unset | self-hosted Photon base URL (`<url>/api`): geocoding without Google |
| `MAPS_OSRM_URL` | unset | self-hosted OSRM base URL (`<url>/route/v1/<driving\|walking\|cycling>/…`): drive/walk/bike routes without Google |
| `MAPS_GOOGLE_PLACES_URL` | `https://places.googleapis.com/v1/places:searchText` | base origin or complete endpoint override for local tests |
| `MAPS_GOOGLE_ROUTES_URL` | `https://routes.googleapis.com/directions/v2:computeRoutes` | base origin or complete endpoint override for local tests |
| `XDG_STATE_HOME` | `~/.local/state` | Places snapshot at `secretary-maps/cache.json`, private directory/file permissions, atomic temp-file + rename |
| `MAPS_UPSTREAM_KEY_FILE` | unset | when set, every route except `/v1/maps/health` requires `X-Maps-Upstream-Key` equal to the file's trimmed content (at least 16 characters; constant-time compare), else 401 |
| `RUST_LOG` | `info` | |

## Routes

All under `/v1/maps`; anything else is `404 {"error":"not_found"}`.

| Route | |
|---|---|
| `GET /health` | `{"ok":true,"tiles":bool,"geocoder":bool}` (no upstream key needed; `geocoder`: Google or Photon) |
| `GET /config` | `{"tiles":"/v1/maps/tiles/region.pmtiles"\|null,"assets":"/v1/maps/assets","home":{"lat","lng","label"}\|null,"geocoder":bool,"routes":bool,"attribution":"© OpenStreetMap contributors · Protomaps","google":{"webKey":string\|null,"iosKey":string\|null}}`; `tiles` is null while the file is missing, `geocoder` is true with a Google key or Photon, `routes` with a Google key or OSRM (no network probe); `google` is always present, a key is null when not configured. No key value is ever logged, and `/health` exposes none |
| `GET /tiles/<name>.pmtiles` | the file with `Range` (206 + `Content-Range`, 416 for an unsatisfiable range), `ETag` (`If-None-Match` 304, `If-Match` 412, `If-Range`), `Last-Modified`, `Accept-Ranges: bytes`, `Cache-Control: private, max-age=86400` |
| `GET /assets/<path>` | files under `MAPS_DIR/assets` (`.pbf` as `application/x-protobuf`, `.json`, `.png`), `Cache-Control: private, max-age=604800`. Paths with `..`, `.` or empty segments, a leading `/`, `\`, `%`, NUL or other control bytes are 400; a path whose canonical form leaves the assets dir (a symlink out) is 404 |
| `GET /geocode?q=&lat=&lng=&limit=` | `{"results":[{"name","address","lat","lng","label"?,"source"?}]}`. Optional fields are omitted when absent. `source` is `AU Find`, `OpenStreetMap`, `Google` or `Photon`; `label` describes university building, name, room and floor when known (floor 0 is `ground floor`). `q` trimmed, 2..=200 chars, `limit` 1..=5 (default 3), else 400. Places Text Search, Danish language, 50 km circular bias around valid query coordinates or home. Three-second deadline. Persistent + memory LRU/TTL cache, 5000 entries, 30 days, keyed by normalized query, bias rounded to 0.01°, and limit. Logs only query length and result count. |
| `GET /routes?from=lat,lng&to=lat,lng&modes=drive,walk,bike,transit&arrive=RFC3339&estimate=1` | Coordinates required and validated via `parse_coord`; modes optional subset (default all four), arrival optional. Invalid input is 400. Concurrent Google Routes calls, four-second deadline per mode; when there is no key or every mode fails, OSRM (`"provider":"osrm"`, no transit), then with `estimate=1` straight-line estimates (`"provider":"estimate"`, walk/bike/drive, `"estimate":true`, empty polyline). Returns `{"provider":"google"\|"osrm"\|"estimate","routes":[...],"failed":[...]}`; each route has mode, durationS, staticDurationS, distanceM, decoded precision-5 polyline simplified to ≤400 points, leaveAt (with arrival: arrival − duration − 5 min), `steps`, and transit step summaries for transit. `steps` is `[{"instruction":string,"maneuver":string\|null,"distanceM":int,"durationS":int,"polyline":[[lat,lng],...]}]`, at most 200, each polyline ≤100 points: Google's turn-by-turn steps for every mode (Danish instructions, Google's `Maneuver` names; a step without instruction keeps its maneuver, one with neither is dropped), OSRM's steps (the road name as instruction, its maneuver mapped to Google's names), `[]` for estimates. |

Both provider endpoints return `503 {"error":"provider_unavailable"}` when no provider answers
(routes: when every requested mode fails everywhere and `estimate=1` is absent). Partial routing failures appear in `failed`. Drive uses
`TRAFFIC_AWARE` and departure now; for a future arrival it first obtains static duration and then routes at
arrival minus that duration, within the same deadline. Transit uses arrivalTime when supplied,
otherwise departureTime now. Walk/bike omit time options. Routing logs contain only mode, status and latency.
Route results are cached in memory by coordinates rounded to four decimals, mode and arrival rounded to five
minutes (absent arrival is a distinct bucket): drive/transit five minutes, walk/bike 24 h, at most 5000 entries.
Cached leaveAt is recalculated for the exact requested arrival. Only Places entries persist; AU Find and Photon answers
(one day) and OSRM routes (one day) are cached in memory.

`deploy/secretary-maps/sync-google-key.sh` reads `GOOGLE_MAPS_SERVER_KEY`, `GOOGLE_MAPS_WEB_KEY` and
`GOOGLE_MAPS_IOS_KEY` from the dev Infisical project and atomically writes the private key files (`google.key`,
`google-web.key`, `google-ios.key`, or the `MAPS_GOOGLE_*_KEY_FILE` paths) with `umask 077`; it never prints a
value. Run it at deployment, then restart the service. It exits 3 if the server key is missing; a missing client
key is only a warning (the apps get `null` for it). This script is a deployment step, not a service
startup dependency (the service serves tiles, the gazetteer, Photon, OSRM and estimates without Google).

Clients that use the self-hosted tiles open them with `pmtiles://` + the absolute URL of `config.tiles`, and the
MapLibre style points `glyphs` at `<assets>/fonts/{fontstack}/{range}.pbf` and `sprite` at
`<assets>/sprites/v4/<flavor>`.

## Data layout (`MAPS_DIR`)

```text
~/data/maps/
  tiles/region.pmtiles                         the extract (MAPS_TILES=region)
  assets/fonts/<fontstack>/<start>-<end>.pbf    e.g. "Noto Sans Regular/0-255.pbf"
  assets/sprites/v4/<flavor>.json|.png          light, dark, ... and the @2x variants
```

Fonts and sprites come from the `protomaps/basemaps-assets` repository (`fonts/`, `sprites/v4/`);
copy them in as they are.

### (Re)building the region extract

Denmark with a margin (bbox west,south,east,north), from a daily Protomaps build (pick a
`YYYYMMDD` listed on https://maps.protomaps.com/builds/):

```sh
pmtiles extract https://build.protomaps.com/<YYYYMMDD>.pmtiles region.pmtiles --bbox=7.9,54.4,15.3,57.9
install -m 0644 region.pmtiles ~/data/maps/tiles/region.pmtiles.new
mv ~/data/maps/tiles/region.pmtiles.new ~/data/maps/tiles/region.pmtiles
```

The rename is atomic; the new file has a new size/mtime, so its ETag changes and clients holding the
old one get the full file again instead of mixing ranges (`If-Range`).

## p1 Caddy

nezopt listens on the tailnet only; p1's Caddy authenticates the owner with the same forward_auth as
the other `/v1/*` services (secretary-server on p1), then adds the upstream key. ufw does not filter
tailnet traffic on nezopt, so the key is the gate:

```caddyfile
@maps path /v1/maps/*
handle @maps {
	forward_auth 127.0.0.1:8800 {
		uri /v1/avatar
	}
	reverse_proxy 100.107.164.33:8840 {
		header_up X-Maps-Upstream-Key {$MAPS_UPSTREAM_KEY}
	}
}
```

Caddy passes `Range`, `If-Range` and the validators through unchanged. The key file on nezopt is
`~/.config/secretary-maps/upstream.key` (mode 0600, at least 16 characters, e.g.
`openssl rand -hex 32`); p1 gets the same value in its Caddy environment.

## Run

```sh
cargo build --release -p secretary-maps
install -m 0755 target/release/secretary-maps ~/.local/bin/
mkdir -p ~/.config/secretary-maps && $EDITOR ~/.config/secretary-maps/env   # the variables above
cp deploy/secretary-maps.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now secretary-maps
curl -s http://100.107.164.33:8840/v1/maps/health
```

Example `~/.config/secretary-maps/env`:

```sh
MAPS_BIND=100.107.164.33:8840
MAPS_HOME_LAT=56.19633
MAPS_HOME_LNG=10.20110
MAPS_HOME_LABEL=Home
MAPS_AUFIND_URL=http://127.0.0.1:18211
MAPS_UPSTREAM_KEY_FILE=/home/nez/.config/secretary-maps/upstream.key
MAPS_GOOGLE_KEY_FILE=/home/nez/.config/secretary-maps/google.key
MAPS_GOOGLE_WEB_KEY_FILE=/home/nez/.config/secretary-maps/google-web.key
MAPS_GOOGLE_IOS_KEY_FILE=/home/nez/.config/secretary-maps/google-ios.key
```

Tests: `cargo test -p secretary-maps` (kernels, the HTTP routes against a local Google stub, and
`tests/kernel_model.rs`, the differential test against the Verus specs).

## Gazetteer: university room codes

Calendar entries from the Aarhus University timetable give rooms, not addresses (`5125-140 Seminarrum`:
room 140 in building 5125). For university locations `/v1/maps/geocode` tries AU Find first when
`MAPS_AUFIND_URL` is set, then `MAPS_DIR/gazetteer/*.json` (arrays of
`{"ref", "name", "lat", "lng"}`), then the normal Google → Photon chain. These building lookups
work without a Google key. AU Find uses only an exact building `id` match and valid coordinates;
unavailable, empty or invalid responses fall through and are never cached. Successful AU Find
answers use the shared memory cache for one day, keyed by normalized query, rounded bias and limit
as for other geocode answers. AU Find logs contain only the building number, status and latency.

The parser accepts `NNNN-room` with surrounding text and case-insensitive `Building NNNN` /
`Bygning NNNN`, optionally followed by `- room`, `, room 524` or `, lokale 524`. Postcodes such as
`8000 Aarhus C`, dates and phone numbers do not match. Rooms starting with at least three digits
use their first digit as the floor. AU Find supplies the building address; gazetteer results keep
the original query as their address. Both supply the descriptive `label` and their `source`;
Google and Photon supply `source` without a label.

`au-buildings.json` comes from the building `ref` tags in OpenStreetMap around Aarhus (83 buildings on
2026-10-26; 5220 and 5522 were missing). Rebuild it with this Overpass query and convert each element to
`{ref, name, lat: center.lat, lng: center.lon}`:

```
[out:json][timeout:80];
nwr["building"]["ref"~"^[1-9][0-9]{3}$"](56.12,10.15,56.20,10.23);
out center tags;
```
