# secretary-maps

Maps for the Secretary apps: self-hosted Protomaps PMTiles, fonts and sprites, with Google
Places API (New) Text Search and Google Routes API called by this server. The Google key stays
on the server and travels only in `X-Goog-Api-Key`; clients never receive it. iOS keeps MapKit
for rendering; web and Android use MapLibre. Provider failures let clients fall back to their
platform search/directions or approximate travel estimates.

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
| `MAPS_GOOGLE_KEY_FILE` | `~/.config/secretary-maps/google.key` | trimmed at startup; missing/empty key disables geocoding and routes; restart after rotation |
| `MAPS_GOOGLE_PLACES_URL` | `https://places.googleapis.com/v1/places:searchText` | base origin or complete endpoint override for local tests |
| `MAPS_GOOGLE_ROUTES_URL` | `https://routes.googleapis.com/directions/v2:computeRoutes` | base origin or complete endpoint override for local tests |
| `XDG_STATE_HOME` | `~/.local/state` | Places snapshot at `secretary-maps/cache.json`, private directory/file permissions, atomic temp-file + rename |
| `MAPS_UPSTREAM_KEY_FILE` | unset | when set, every route except `/v1/maps/health` requires `X-Maps-Upstream-Key` equal to the file's trimmed content (at least 16 characters; constant-time compare), else 401 |
| `RUST_LOG` | `info` | |

## Routes

All under `/v1/maps`; anything else is `404 {"error":"not_found"}`.

| Route | |
|---|---|
| `GET /health` | `{"ok":true,"tiles":bool,"geocoder":bool}` (no upstream key needed) |
| `GET /config` | `{"tiles":"/v1/maps/tiles/region.pmtiles"\|null,"assets":"/v1/maps/assets","home":{"lat","lng","label"}\|null,"geocoder":bool,"routes":bool,"attribution":"© OpenStreetMap contributors · Protomaps"}`; `tiles` is null while the file is missing, `geocoder` and `routes` are true when a Google key is loaded (no network probe) |
| `GET /tiles/<name>.pmtiles` | the file with `Range` (206 + `Content-Range`, 416 for an unsatisfiable range), `ETag` (`If-None-Match` 304, `If-Match` 412, `If-Range`), `Last-Modified`, `Accept-Ranges: bytes`, `Cache-Control: private, max-age=86400` |
| `GET /assets/<path>` | files under `MAPS_DIR/assets` (`.pbf` as `application/x-protobuf`, `.json`, `.png`), `Cache-Control: private, max-age=604800`. Paths with `..`, `.` or empty segments, a leading `/`, `\`, `%`, NUL or other control bytes are 400; a path whose canonical form leaves the assets dir (a symlink out) is 404 |
| `GET /geocode?q=&lat=&lng=&limit=` | `{"results":[{"name","address","lat","lng"}]}`. `q` trimmed, 2..=200 chars, `limit` 1..=5 (default 3), else 400. Places Text Search, Danish language, 50 km circular bias around valid query coordinates or home. Three-second deadline. Persistent + memory LRU/TTL cache, 5000 entries, 30 days, keyed by normalized query, bias rounded to 0.01°, and limit. Logs only query length and result count. |
| `GET /routes?from=lat,lng&to=lat,lng&modes=drive,walk,bike,transit&arrive=RFC3339` | Coordinates required and validated via `parse_coord`; modes optional subset (default all four), arrival optional. Invalid input is 400. Concurrent Google Routes calls, four-second deadline per mode. Returns `{"provider":"google","routes":[...],"failed":[...]}`; each route has mode, durationS, staticDurationS, distanceM, decoded precision-5 polyline simplified to ≤400 points, leaveAt (with arrival: arrival − duration − 5 min), and transit step summaries for transit. |

Both provider endpoints return `503 {"error":"provider_unavailable"}` without a key or on provider failure
(routes: when every requested mode fails). Partial routing failures appear in `failed`. Drive uses
`TRAFFIC_AWARE` and departure now; for a future arrival it first obtains static duration and then routes at
arrival minus that duration, within the same deadline. Transit uses arrivalTime when supplied,
otherwise departureTime now. Walk/bike omit time options. Routing logs contain only mode, status and latency.
Route results are cached in memory by coordinates rounded to four decimals, mode and arrival rounded to five
minutes (absent arrival is a distinct bucket): drive/transit five minutes, walk/bike 24 h, at most 5000 entries.
Cached leaveAt is recalculated for the exact requested arrival. Only Places entries persist.

`deploy/secretary-maps/sync-google-key.sh` reads `GOOGLE_MAPS_SERVER_KEY` from the dev Infisical project and
atomically writes the private key file with `umask 077`; it never prints the value. Run it at deployment,
then restart the service. It exits 3 if the secret is missing. This script is a deployment step, not a service
startup dependency (the service can serve tiles without Google).

The web client opens the tiles with `pmtiles://` + the absolute URL of `config.tiles`, and the
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
MAPS_UPSTREAM_KEY_FILE=/home/nez/.config/secretary-maps/upstream.key
MAPS_GOOGLE_KEY_FILE=/home/nez/.config/secretary-maps/google.key
```

Tests: `cargo test -p secretary-maps` (kernels, the HTTP routes against a local Google stub, and
`tests/kernel_model.rs`, the differential test against the Verus specs).
