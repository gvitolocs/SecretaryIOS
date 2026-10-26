# secretary-maps

Self-hosted maps for the Secretary apps (the web app, the Android WebView, the place cards): one
Protomaps PMTiles extract served with HTTP range, the map fonts and sprites, and geocoding through a
local [Photon](https://github.com/komoot/photon). No API keys, and no request leaves the box: the
geocoder is local and clients read tiles from this server. iOS uses MapKit and needs only
`/v1/maps/config` (home) and, optionally, `/v1/maps/geocode`.

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
| `MAPS_PHOTON_URL` | `http://127.0.0.1:2322` | |
| `MAPS_UPSTREAM_KEY_FILE` | unset | when set, every route except `/v1/maps/health` requires `X-Maps-Upstream-Key` equal to the file's trimmed content (at least 16 characters; constant-time compare), else 401 |
| `RUST_LOG` | `info` | |

## Routes

All under `/v1/maps`; anything else is `404 {"error":"not_found"}`.

| Route | |
|---|---|
| `GET /health` | `{"ok":true,"tiles":bool,"geocoder":bool}` (no upstream key needed) |
| `GET /config` | `{"tiles":"/v1/maps/tiles/region.pmtiles"\|null,"assets":"/v1/maps/assets","home":{"lat","lng","label"}\|null,"geocoder":bool,"attribution":"© OpenStreetMap contributors · Protomaps"}`; `tiles` is null while the file is missing, `geocoder` is a probe of Photon (`/api?q=aarhus&limit=1`) cached for 60 s |
| `GET /tiles/<name>.pmtiles` | the file with `Range` (206 + `Content-Range`, 416 for an unsatisfiable range), `ETag` (`If-None-Match` 304, `If-Match` 412, `If-Range`), `Last-Modified`, `Accept-Ranges: bytes`, `Cache-Control: private, max-age=86400` |
| `GET /assets/<path>` | files under `MAPS_DIR/assets` (`.pbf` as `application/x-protobuf`, `.json`, `.png`), `Cache-Control: private, max-age=604800`. Paths with `..`, `.` or empty segments, a leading `/`, `\`, `%`, NUL or other control bytes are 400; a path whose canonical form leaves the assets dir (a symlink out) is 404 |
| `GET /geocode?q=&lat=&lng=&limit=` | `{"results":[{"name","address","lat","lng"}]}`. `q` trimmed, 2..=200 chars, `limit` 1..=5 (default 3), else 400. Biased to `lat`/`lng` when valid, else home. Photon down or failing (3 s timeout) is `503 {"error":"geocoder_unavailable"}`. Results are cached in memory (512 entries, 24 h) by lowercased, whitespace-collapsed query + bias rounded to 0.01° + limit. The query is never logged (only its length and the result count) |

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
```

Tests: `cargo test -p secretary-maps` (kernels, the HTTP routes against a fake Photon, and
`tests/kernel_model.rs`, the differential test against the Verus specs).
