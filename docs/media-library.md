# Media library (lucina-media)

The owner's photos, videos, audio and other files, on their own server, like Google Photos. The owner chose
this over Nextcloud on 2026-10-24: Nextcloud on nezopt held almost no photos, its auto-upload had never
run, and it is stopped every night.

## Where things are

| What | Where |
|---|---|
| Server | `crates/lucina-media`, binary `~/.local/bin/lucina-media` on nezopt, user unit `deploy/lucina-media.service` |
| Config | `~/.config/lucina-media/env` (listeners, dirs, folders); keys in `~/.config/secretary-media/` (upstream key, play-token key) |
| Public API | `100.107.164.33:8830`. It is reached only through p1 Caddy (`/v1/media/*`), which runs Secretary auth, then adds `X-Media-Upstream-Key`. ufw does not filter tailnet traffic, so the key is the gate. |
| Internal API | `127.0.0.1:8830` `/internal/*`: voice-memo registration, imports, item paths |
| Originals | mybook `~/mnt/mybook/lucina-media/objects/ab/cd/<blake3>.<ext>`. They are written only while `.lucina-media-store` exists, so an unmounted mybook is never filled by mistake. |
| Catalog, thumbnails, spool | NVMe `~/data/lucina-media` (`catalog.db` SQLite WAL + FTS5, `thumbs/`, `spool/`, `uploads/`), mode 700 |
| Streaming | `crates/secretary-media-stream`, mounted on the catalog (`source.rs`): `/play`, `/v1/media/s/{token}/…` (Range, HLS, subtitles), `/v1/media/progress` |

Uploads that arrive in the quiet hours (22:00–09:00 Copenhagen, like Nextcloud's "disk quiet" timer) stay in the
NVMe spool. A mover takes them to mybook in the day window: copy, fsync, rename, flip the catalog row, then
remove the spool copy, so it is safe to interrupt.

## Background film preparation

Only the film folders (`scope=films`) get HLS outputs ahead of time; phone videos are left alone. One job at a
time runs beside (but never ahead of) a viewer's job:

- It runs in `systemd-run --user --scope -p CPUQuota=100%` (`LUCINA_MEDIA_PREPARE_SCOPE=0` turns the scope off,
  `LUCINA_MEDIA_PREPARE_CPU_QUOTA` changes the quota), so preparation cannot take more than one core.
- Re-encodes use VAAPI when `/dev/dri/renderD128` and ffmpeg's `h264_vaapi` exist, under
  `~/bin/gpu-lease run --name media-prepare --mb 512`; tone-mapping and the fallback use libx264 with two
  decode/encode threads. `LUCINA_MEDIA_VAAPI=0|<device>` and `LUCINA_MEDIA_GPU_LEASE=<path>|0` override.
- libx264 and tone-mapping jobs wait for the night window (`LUCINA_MEDIA_PREPARE_WINDOW`, default `1-7`,
  `[start, end)` local, wraps past midnight). VAAPI and remux jobs may start any hour.
- No job starts at or above `LUCINA_MEDIA_PREPARE_MAX_TEMP` (k10temp Tctl, default 80 °C) or
  `LUCINA_MEDIA_PREPARE_MAX_LOAD` (one-minute load, default 16); the worker re-checks every minute and logs a
  pause once. A running job is sampled every 10 s and stopped after three over-limit samples (85 °C or the max
  load), its output removed, and it returns to the queue as paused, not failed. A failed VAAPI job is retried
  once on libx264 in the night window. Viewers pressing play are not throttled by any of this.

## API (all under `/v1/media`)

- `GET items?scope=&kind=&from=&to=&q=&near=lat,lon,km&album=&favorite=&trashed=&cursor=&limit=` gives newest first
  (`taken_at`, else `added_at`).
  - `scope` splits the library into the owner's three places (2026-10-26): `gallery` is only what the phones backed
    up (`source` phone-…: the Gallery); `films` is the film folders (`LUCINA_MEDIA_FILM_FOLDERS`, default
    `Film;Dansk film`: the Films plugin); `drive` is everything else (old Nextcloud files, computer photo backups,
    audio, other folders, shared files, voice memos: the Drive). Without it, everything. `GET albums?scope=` lists
    only the albums with items there, counted there.
  - `q` searches title, caption, place, country (also in Danish/Italian), month words ("July 2025"), camera and album titles.
  - `album` takes an id or a title.
- `GET|PATCH|DELETE items/{id}`, `POST items/{id}/restore`.
  - DELETE is a soft delete (trash). The original stays, and phones are never touched.
- `GET items/{id}/original` serves HTTP Range with ETag = blake3. `GET items/{id}/thumb?size=256|1024` and `GET items/{id}/poster` also exist.
- Albums:
  - `GET|POST albums`, `GET|PATCH|DELETE albums/{id}` (system albums can't be deleted);
  - `POST|PUT|DELETE albums/{id}/items` (`PUT` replaces the items; Lucina uses it for `sys-lucina`, "Lucina's picks").
- Upload:
  1. `POST uploads {filename, size, hash?, meta}` answers `{duplicate, item_id}` when the bytes (or the same unchanged phone asset) are already in.
  2. Then `PUT uploads/{id}?offset=N` with the body; resume from `GET uploads/{id}`.
  3. Optionally `PUT uploads/{id}/preview` (JPEG).
  4. Then `POST uploads/{id}/finish`, which is idempotent.
  - The same device + asset id with new bytes makes a new version of the same item.
- `GET devices/{device}/assets`, `GET summary`, `GET events` (SSE: `item.added|updated|trashed`).
- Internal: `PUT|DELETE /internal/v1/external/{source}/{id}` indexes a file in place (voice memos, the film folders), `POST /internal/v1/import {path, source, album}`.

The upload protocol's decisions (chunk offsets, finish once, dedupe or version) are pure functions in `protocol.rs`.

## Clients

- **iPhone** (`ios/Secretary/Features/Gallery`):
  - The Gallery (a plugin card, `GalleryScreen` via `app.showGallery`, `scope=gallery`) shows the phones' photos and
    videos: a timeline by month, search, albums, and a viewer that zooms out of its tile and back. Phone photos come
    from PhotoKit; server renditions are cached on disk and a cut-off preview resumes with `Range`.
  - The Drive (`DriveScreen`, `app.showDrive`, `scope=drive`) lists the archive as folders and files; photos and
    videos open in the same viewer, audio in the player, other files in Quick Look. Films have their own plugin
    (the player session's, `scope=films`).
  - Photo backup (`PhotoBackup.swift`) reads PhotoKit newest first.
    - It sends originals (edited renderings as new versions, both halves of Live Photos) and a JPEG preview, because the server cannot decode HEIC.
    - Transfers go through a background URLSession. A BGProcessingTask and the app-refresh task continue the work.
    - Rules: Wi-Fi only, while charging, include videos. It never deletes on the phone.
- **Lucina** (`lucina-tools` `media` tool, `lucina-server` `chat/media_turn.rs`):
  - Requests about photos, videos or albums run a short tool loop.
  - Results go to "Lucina's picks", and the reply gets a "Show in Gallery" route button (`/gallery/album/<id>`).
- **Imports:** `lucina-media import <dir|takeout.zip> --source takeout|nextcloud --album …` copies files and never moves them. Takeout JSON sidecars supply dates and places. `lucina-media index-folder <label> <dir>` indexes in place.

## Renditions and the Rust viewer (2026-10-25)

| What | Where | Size (owner's photos) |
|---|---|---|
| ThumbHash | `items.thumbhash`, base64 in the item JSON (`thumbhash`) | about 18 bytes |
| Grid tile | `g.jxl`, 256 px square crop, JPEG XL d2.5 | 8.8 KB (JPEG `s.jpg` was 38.7 KB) |
| Preview | `p.jxl`, 1440 px long side, progressive JPEG XL d2.5 | 57 KB (JPEG `l.jpg` was 135 KB) |
| Original | blake3 store, `urls.original` | unchanged, only on explicit open |

- `urls.grid` / `urls.preview` are play-token URLs (`/v1/media/s/{token}/{id}/r/{grid|preview}`). p1 passes `/v1/media/s/*` without the login check, so tiles don't spend the device's 60 requests a minute. They expire with the token (12 h); clients cache by item id plus ThumbHash, not by URL.
- `crates/secretary-image` decodes everything the apps show: the ThumbHash placeholder, progressive JPEG XL fed as bytes arrive (jxl-oxide; the first picture after about 13% of the bytes), and JPEG/PNG. It also holds zoom/pan as pure functions.
- The C ABI is `secretary_image_*` in `secretary-ffi` (header block at the end of SecretaryCore.h).
- iOS uses it in `Features/Gallery/ImageCore.swift` (`RustTile`, `RustPhoto`). Debug options:
  - `-SecretaryImageThrottleKBps N` (slow feed, to watch the refinement);
  - `-SecretaryGalleryOpenFirst YES` and `-SecretaryGalleryOpen <id>` (open the viewer at launch).
- Formats compared on real photos: `tools/image-bench` (results in the side-session notes, gallery-image-formats.md). JPEG XL won as the only format with a pure-Rust progressive decoder.

## Backup and the device rate limit

Every `/v1/media/*` request outside `/s/` passes p1's forward_auth, which allows 60 requests a minute for each device. The backup therefore:
- waits 4 s between assets (create, PUT, finish; the JPEG preview only for HEIC), about 36 requests a minute;
- stops on 429 and comes back after `Retry-After`;
- pauses the whole pass on network, timeout and 502–504 errors (retries after 120 s), and skips just the asset on anything else.

An upload whose bytes are gone answers 410 on finish, and the phone starts that asset again. Ingest and the "finished" mark run in one blocking step, because a client hang-up drops the handler.

## Internal API for other services on nezopt

- `POST /internal/v1/ingest {path, mime?, name?, source?, album?, taken_at?, caption?, move?}`: a local file into the library (shares to Lucina). Answers `{item_id, duplicate, kind, mime, thumb_state, urls}`.
- `PUT|DELETE /internal/v1/external/{source}/{id}`: index a file in place (voice memos, film folders).
- `GET /internal/v1/items/{id}/path`: the original's path on disk.

## Not done yet

- Web gallery tab.
- Android upload: the Rust module for the Android app.
- WebDAV upload endpoint.
- HEIC decoding on the server (needs a user-space libde265 + libheif build); the phone's preview covers iPhone uploads.
- Android and web bindings for `secretary-image` (JNI; wasm in the player's `ptr<<32|len` style).
- Zoom/pan on the device is unit-tested only: XCUITest can't run on iOS 27 with Xcode 26.
- Stateright/TLA+ model of the upload protocol.
- HLS cache eviction.
