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

## API (all under `/v1/media`)

- `GET items?kind=&from=&to=&q=&near=lat,lon,km&album=&favorite=&trashed=&cursor=&limit=` gives newest first
  (`taken_at`, else `added_at`).
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
  - The Gallery (a plugin: the Plugins tab's card opens `GalleryScreen` via `app.showGallery`; until that tab exists, a button on the chat) shows a timeline by month, search, albums, and a viewer that hands videos and audio to the media player.
  - Photo backup (`PhotoBackup.swift`) reads PhotoKit newest first.
    - It sends originals (edited renderings as new versions, both halves of Live Photos) and a JPEG preview, because the server cannot decode HEIC.
    - Transfers go through a background URLSession. A BGProcessingTask and the app-refresh task continue the work.
    - Rules: Wi-Fi only, while charging, include videos. It never deletes on the phone.
- **Lucina** (`lucina-tools` `media` tool, `lucina-server` `chat/media_turn.rs`):
  - Requests about photos, videos or albums run a short tool loop.
  - Results go to "Lucina's picks", and the reply gets a "Show in Gallery" route button (`/gallery/album/<id>`).
- **Imports:** `lucina-media import <dir|takeout.zip> --source takeout|nextcloud --album …` copies files and never moves them. Takeout JSON sidecars supply dates and places. `lucina-media index-folder <label> <dir>` indexes in place.

## Not done yet

- Web gallery tab.
- Android upload: the Rust module for the Android app.
- WebDAV upload endpoint.
- HEIC decoding on the server (needs a user-space libde265 + libheif build).
- Stateright/TLA+ model of the upload protocol.
- HLS cache eviction.
