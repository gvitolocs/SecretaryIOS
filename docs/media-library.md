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
- No job starts at or above `LUCINA_MEDIA_PREPARE_MAX_TEMP` (k10temp Tctl, default 90 °C, the owner's limit) or
  `LUCINA_MEDIA_PREPARE_MAX_LOAD` (one-minute load, default 16); the worker re-checks every minute and logs a
  pause once. A running job is sampled every 10 s and stopped after three samples in a row at or above 90 °C
  (load alone never stops it: builds are not capped), its output removed, and it returns to the queue as paused,
  not failed. The scope is bound to lucina-media's unit (`BindsTo=`; `LUCINA_MEDIA_PREPARE_BIND`), so a
  restart of the service stops its ffmpeg too. A failed VAAPI job is retried
  once on libx264 in the night window. Viewers pressing play are not throttled by any of this.

## The Drive (2026-10-26)

Nextcloud is gone; its files live in Drive folders on mybook (`Lezioni`: university lectures, `Tracer`: a code and
data project, …), listed in `LUCINA_MEDIA_FOLDERS` like the film folders. Every folder that is not a film folder is
the Drive, and **every regular file** in it is indexed in place (`drive.rs`): documents, code, archives and anything
else, not only media. Nothing is copied: the catalog keeps type, size, mtime, the path within the folder and the
blake3 of the bytes (the `blobs` row references the file; a phone copy of the same bytes becomes the owned original,
kernel DK3).

- **Scans** every 30 minutes (and `lucina-media index-folder <label> <dir>`). Skipped: hidden files and
  `LUCINA_MEDIA_DRIVE_IGNORE` (`;`-separated names, `*` at either end; default `.git`, `node_modules`, `target`,
  `__pycache__`, `.DS_Store`, `.venv`, `*.pyc`, Office lock files, …), and symlinks (a link in a project must not pull
  the rest of the disk in). `LUCINA_MEDIA_DRIVE_HIDDEN=1` indexes hidden files too. An unchanged file (same size and
  mtime) costs one lookup; a new or changed one is hashed. A file gone from the folder is trashed in the catalog,
  never on disk, and only after a clean walk of the folder on the filesystem it was indexed from (DK4: an unmounted
  mybook never empties the Drive).
- **Types** (`doctype.rs`): pdf, document, spreadsheet, presentation, text, code, archive, image, video, audio,
  other. Each has a preview: `pdf`, `office`, `text`, `code`, `archive`, the media viewers, or `none`.
- **Background work** (`drive_worker`, one job at a time, newest first): the text of PDFs (poppler `pdftotext`, else
  pure-Rust `pdf-extract`), Office files (docx/xlsx/pptx/odt/ods/odp read from their XML in Rust; RTF), text and
  code goes into a full-text index (`drive_fts`); a PDF's first page (`pdftoppm`) becomes its thumbnail; Office files
  become PDFs with LibreOffice (`soffice --headless`) when installed. External tools and the extractor run in
  `systemd-run --user --scope -p CPUQuota=50% -p MemoryMax=2G` under `nice -n 19`
  (`LUCINA_MEDIA_DRIVE_CPU_QUOTA`), with a timeout; the worker and the hashing wait while Tctl ≥ 90 °C
  (`LUCINA_MEDIA_DRIVE_MAX_TEMP`) or the load is over `LUCINA_MEDIA_PREPARE_MAX_LOAD`. Tools are found on `PATH`;
  `LUCINA_MEDIA_PDFTOTEXT`, `LUCINA_MEDIA_PDFTOPPM`, `LUCINA_MEDIA_SOFFICE` take a path or `0`.
- **Scopes stay apart** (DK2): the Gallery is the phones' backups only, Films the film folders, the Drive everything
  else. A phone backup is in the Gallery even if filed in a film album.

## API (all under `/v1/media`)

- `GET items?scope=&kind=&from=&to=&q=&near=lat,lon,km&album=&favorite=&trashed=&cursor=&limit=` gives newest first
  (`taken_at`, else `added_at`).
  - `scope` splits the library into the owner's three places (2026-10-26): `gallery` is what the devices backed
    up plus the pictures from chat and shares and Lucina's own (see "The Gallery" below); `films` is the film folders (`LUCINA_MEDIA_FILM_FOLDERS`, default
    `Film;Dansk film`: the Films plugin); `drive` is everything else (old Nextcloud files, computer photo backups,
    audio, other folders, shared documents, voice memos: the Drive). Without it, everything. `GET albums?scope=` lists
    only the albums with items there, counted there.
  - `q` searches title, caption, place, country (also in Danish/Italian), month words ("July 2025"), camera and album titles.
  - `album` takes an id or a title.
- `GET|PATCH|DELETE items/{id}`, `POST items/{id}/restore`.
  - DELETE is a soft delete (trash). The original stays, and phones are never touched.
- `GET items/{id}/duplicates` → `{item_id, merged_from?, duplicates: [{id, kind, score, decided_by, merged_at, size, mime, width,
  height, source, device, device_name, hash, urls}]}`: the copies merged under a photo (see "Identity"); the id of a
  hidden copy works too. `GET items/{id}` of a hidden copy answers with the survivor and `merged_from`.
- Duplicate review and stacks (`dedup_review.rs`, `stacks.rs`; "Duplicate review and stacks" below):
  - `GET duplicates?status=open&limit&offset` → `{hashes: {photos, hashed, pending}, total, applicable, min_score,
    reclaimable_bytes, stack_suggestions, groups: [{id, class, score, applicable, keep, reason, members: [item JSON +
    review {metadata, keep}]}]}`; `POST duplicates/resolve {keep, drop}`, `POST duplicates/dismiss {ids, undo?}`,
    `POST duplicates/undo {ids}`, `POST duplicates/resolve-all {min_score?}`;
  - `POST stacks {ids, primary?, kind?}`, `GET|PUT|DELETE stacks/{id}`, `GET stacks/suggestions`,
    `POST stacks/suggestions/dismiss {ids}`; every item JSON has `stack: null | {id, count, primary}`.
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
- The Drive:
  - `GET drive`: the Drive folders (files, bytes, last change), the other Drive collections (voice memos, imports,
    shares: open them with `items?album=`), and which converters exist.
  - `GET drive/browse?folder=&path=&sort=name|modified|size|type&order=asc|desc&type=&q=&recursive=1&offset=&limit=`:
    breadcrumbs, the subfolders (with recursive counts), then the files. `q` searches the subtree (names, folder
    names, content). An unsafe `path` (`..`, `.`, empty segments, `/` at either end, `\`, NUL) is 400 (DK1).
  - Items carry `drive` (`folder`, `path`, `dir`, `name`, `ext`, `type`, `preview`, `language`, `modified`, `text`)
    and, on searches that matched the content, `snippet`. `items?type=` filters by Drive type; `q` also matches content.
  - `GET items/{id}/text?highlight=1&max_bytes=`: a text or code file (with syntax tokens: syntect, classes `kw`,
    `str`, `com`, …), or the extracted text of a PDF or Office file (read on demand when the worker has not been
    there). `GET items/{id}/pdf`: a PDF, or an Office file converted (503 without LibreOffice).
    `GET items/{id}/archive`: zip, tar, tar.gz and 7z listings (nothing is extracted). `GET items/{id}/original?download=1`:
    `Content-Disposition: attachment` with the real name.
  - `GET|POST items/{id}/sheet`, `sheet/versions`, `sheet/restore`: `.xlsx` workbooks as sheets, edited in place with a
    version backup ([`xlsx.md`](xlsx.md)). This is the only route that writes to a Drive file.
- Devices (2026-10-27: the owner wants one gallery per device, everything in the Lucina cloud):
  `PUT devices/{id} {name, kind}` registers or renames a device (`kind`: iphone, ipad, android, mac, pc, backup,
  camera, other; ids `[A-Za-z0-9._-]{1,64}`, e.g. `ios-<uuid>`, `mac-<uuid>`); an upload whose `meta.device` is unknown
  adds it. `GET devices` lists every device with `photos`, `videos`, `latest_taken_at`. `GET items?device=<id>` is one
  device's photos; the Gallery (`scope=gallery`) is everything that came from a device: `source` phone-… or `device`
  set (`db::FROM_DEVICE`, the Drive kernel's DK2 `phone` input). The old nespc backups are the device
  `nespc-backup`.
- `GET devices/{device}/assets`, `GET summary`, `GET events` (SSE: `item.added|updated|trashed|purged`).
- The Gallery (2026-10-28, `gallery.rs`, `batch.rs`, `trash.rs`, `deletions.rs`). Everything the web Gallery needs:
  - **Item fields.** Every item (list and single) also carries:
    - `device` (id or null) and `device_name` (from the registry, null if unknown);
    - `rotation` (0, 90, 180 or 270);
    - `trash_expires_at` (`trashed_at` plus the retention, RFC 3339, only while trashed and only if the Trash is
      not kept forever);
    - `origin: {filename, folder, imported_from}`:
      - `filename`: the original file name (uploads: the upload's `filename`; imports and in-place folders: the
        file's name; a Drive file: its Drive name; older items: the title when it reads as a file name);
      - `folder`: the folder under the import root, `DCIM/2019` (Takeout zips: the folder inside the zip; Drive
        files and in-place folders: under the folder's root). `null` at the top;
      - `imported_from`: the import root's own name (a folder or zip name), a Drive or film folder's label, else the
        import `source` (`takeout`, `nextcloud`, `import`) for older imports.
      - An absolute host path never appears: imports and folders store the path relative to their root
        (`gallery::import_origin`, `relative_to_roots`), and what a client sends in `meta.origin_folder` /
        `meta.origin_root` is refused when it is absolute or has `..` (`gallery::clean_folder`). These are stored in
        `items.origin_name|origin_folder|origin_root` when the item arrives; items stored before only have what can
        be derived.
  - `GET items/{id}/details` answers `{item, exif, people}`. `item` is the item as above. `exif` is read on demand
    from the original with nom-exif: `{make, model, lens, iso, exposure_s, f_number, focal_mm, flash, orientation,
    software, taken_at, offset, duration_ms}`. `exposure_s` is seconds (0.008 = 1/125), `flash` is whether it fired,
    `taken_at` is UTC, `offset` is `+02:00` when the file states one, and `duration_ms` is only set for videos and
    audio (an addition to the contract's list). A field the file lacks is `null`. `exif` is `null` when there is
    nothing to read (a PNG, or the original is not reachable). `people` is `[]` until faces are recognised.
  - `PATCH items/{id}` also takes `rotation` (0, 90, 180, 270, else 400). **The client draws the rotation (CSS);
    the renditions are not re-rendered and the original is untouched.**
  - **Sources.** `GET sources` is the switcher: `{sources: [...], all: {photos, videos}}`. Each source is `{id, label,
    kind, photos, videos, latest_taken_at}`:
    - one `device:<id>` per registered device, most recently seen first (kept even with nothing in it), then
      `chat` ("Chat & shares") and `lucina` ("Lucina"), which are left out while they hold nothing;
    - `chat` = photos and videos whose `source` starts with `ios-chat`, `ios-share`, `web-chat`, `web-share`, `chat`,
      `telegram` or `discord`; `lucina` = `source` starts with `lucina` (`gallery::source_group`, a test keeps the SQL
      and the Rust rule the same). A document shared from a phone is not a Gallery picture: it stays a Drive file;
    - `all` counts the Gallery: devices, chat and Lucina, not the Drive. Hidden Live Photo parts and the Trash are
      never counted.
  - **Filters on `GET items`** (and `items/months`): `origin=all|device:<id>|chat|lucina` (other values are 400; blank
    means none) and `kind=photo,video` as a comma list. `origin=all` and `scope=gallery` are the same set
    (`db::FROM_DEVICE`): from a device, or a photo or video from chat or shares, or Lucina's own. The Drive scope is
    the rest, so each item is in exactly one of the three scopes (the kernel's DK2 partition keeps holding). Paging
    is unchanged: `cursor`, `limit` up to 500, `next_cursor`, newest `taken_at`/`added_at` first.
  - `GET items/months?origin=&kind=` (any listing filter, also `trashed=true`) answers
    `{months: [{month: "2026-10", count}]}`, newest month first. A month is the UTC month of the sort date, which is
    what the listing pages by.
  - **Batch.** `POST items/batch {op, ids, album?, also_from_device?}` answers `{done: [ids], failed: [{id, error}],
    queued}`. At most 500 ids (duplicates count once); a bad request is 400 and a missing album 404 before anything
    runs. All ids of one call run in one transaction; an id that cannot be done is `failed` and the others go on.
    - `trash`, `restore`, `favorite`, `unfavorite`, `album_add`, `album_remove` (the last two need `album`: an id).
    - `purge` (below).
    - `also_from_device: true` goes only with `trash`. `queued` counts the device deletions it queued.
    - Errors per id: `not_found`, `invalid_id`, `already_trashed`, `not_trashed`.
    - The single routes (`DELETE items/{id}` = trash, `POST items/{id}/restore`) work as before.
  - **Purge** is the only permanent delete, and it only acts on items already in the trash (`not_trashed` otherwise).
    It deletes the item row and its versions, album links, externals, Drive row, search text and upload record, and
    the thumbnails. An original's file is removed only when no item and no version row still references its hash,
    and only when the library owns the file (store or spool); a file in place (a Drive file, a voice memo) is never
    deleted. A Live Photo's hidden motion part is not touched. The same function runs the retention.
  - **Retention.** Items trashed longer ago than `LUCINA_MEDIA_TRASH_DAYS` (30; 0 never purges) are purged by a task
    once an hour (`trash::retention_worker`); it logs how many, never names.
  - **Copy semantics (the owner's rule).** Deleting in Lucina never deletes the original on a device. Only
    `also_from_device` queues a request, for items that have a device and an asset id, and the device app asks the
    system to delete (PhotoKit shows its own confirmation):
    - table `device_deletions(device, asset_id, item_id, requested_at, done_at)`;
    - `GET devices/{id}/deletions` → `{pending: [{asset_id, item_id, requested_at}]}`;
    - `POST devices/{id}/deletions/ack {asset_ids}` → `{acked: n}` (only pending ones count).
    - A request stays queued if its item is purged, and is queued again if the item is trashed again after the ack.
    - The iOS side that consumes this is a follow-up.
- Browsing the Gallery (2026-10-28, `browse.rs`): Places, memories and the archive. All of it uses the listing's own
  filters (`db::list_filters`), so a map, a memory and the grid always agree on what is visible (not trashed, not
  archived, the chosen `origin` and `kind`; both default to the whole Gallery, photos and videos).
  - **Places.** `GET items/places?origin=&kind=&bbox=&zoom=&limit=` groups the pictures that have a position into
    map clusters: `{zoom, total, clusters: [{id, count, lat, lon, bbox, cover}]}`, biggest first, at most `limit`
    (300 by default, 500 at most). `bbox` is the visible window `west,south,east,north` in degrees (`west > east`
    crosses the date line); `zoom` is the map zoom on a 256 px-tile map (0 to 24, 3 by default; MapLibre counts 512 px
    tiles, so the web sends its zoom plus one). The grid is square on screen (Web Mercator), 72 px cells; `lat` and
    `lon` are the mean of the members, `bbox` the box around them. `cover` is the item JSON of the picture that
    stands for the cluster (a favorite first, then the newest); `total` counts every picture with a position in
    the window. Bad `bbox`, `zoom` or `origin` are 400.
  - **`bbox` on `GET items`** (and `items/months`) lists exactly those pictures: a cluster opens in the grid with
    `items?bbox=<the cluster's bbox>`. Pictures without a position never match.
  - **On this day.** `GET items/on-this-day?date=MM-DD&year=&per_year=&origin=&kind=` answers
    `{date, year, years: [{year, years_ago, total, items}]}`, newest year first: the pictures taken on that calendar
    day in years before `year` (today's date and year in the library's time zone by default). `total` is all of the
    year's pictures that day, `items` at most `per_year` (6; up to 20) in time order: favorites first, then the most
    pixels, and a picture within 30 s of one already chosen is skipped (a burst counts once). Only pictures with a
    real `taken_at` count, never ones dated by when they were added. `date` must be a real day (`02-29` is one).
  - **Recently added.** `GET items/recent?origin=&kind=&days=&limit=` answers `{total, items}`: the newest arrivals by
    `added_at` (not taken date), `total` counting those within `days` (1 to 36500; all by default), `limit` 30 (up to 200).
  - **Archive.** `items.archived` (idempotent column migration, partial index) hides a photo or video from the Gallery
    without deleting it. `POST items/batch` takes `archive` and `unarchive`; per-id errors `not_media` (a Drive file),
    `trashed`, `already_archived`, `not_archived`. `GET items?archived=true` lists the archive (default: the archived
    are left out, of the listing, `items/months`, the sources' and devices' counts, Places and the memories). The Trash
    shows trashed items whether archived or not, and a restored item is still archived. An archived item is still
    found by id, in albums and by a share link (archiving is about browsing, not about access).

- Share links (2026-10-27, `share.rs`, `share_http.rs`):
  - owner: `POST albums/{id}/shares {expires_in_days?, allow_original?}` → `{token, url_path, …}`;
    `GET albums/{id}/shares`, `GET shares` (with views and `live`); `DELETE shares/{token}` revokes (the row stays).
  - public, no key (`upstream_guard` lets `/v1/media/a/` through; p1 maps `/a/*` to it): `GET a/{token}` (a
    self-contained page: grid, lightbox, videos through the play-token `/file` route), `GET a/{token}/items`
    (no place, camera, caption or file name), `GET a/{token}/i/{item}/{thumb|large|poster|original}`.
  - every public request: a well-formed 22-character token, 120 requests a minute per link, then the verified
    rule (`decide`, Verus AS1): unknown 404, revoked or expired 410, an item not in the album now (or trashed,
    hidden) 404, an original without permission 403. Renditions are served with their metadata stripped
    (`strip_jpeg_metadata`); anything that cannot be stripped is not served. Headers: `noindex`, no referrer,
    a CSP with a per-page nonce, no framing.
  - p1 (live since 2026-10-27, `/etc/caddy/Caddyfile`, just above `@media_play`; the repo's `deploy/Caddyfile.p1`
    does not carry the media routes):
    ```
    @album_share_short path /a/*
    handle @album_share_short {
        rewrite * /v1/media{uri}
        reverse_proxy 100.107.164.33:8830
    }
    @album_share path /v1/media/a/*
    handle @album_share {
        reverse_proxy 100.107.164.33:8830
    }
    ```
    A link is `https://getsecretary.tech/a/<token>`. Checked from outside: the page, the items and a thumbnail
    (no EXIF) answer 200, an original without permission 403, a revoked link 410, `/v1/media/items` still 401.
- Links over chosen photos and the owner's list (2026-10-28, `share_sel.rs`, `share_http.rs`):
  - `POST shares {item_ids, title?, expires_in_days?, allow_original?}` (1 to 500 ids; photos and videos that exist and
    are not in the Trash, the rest come back in `skipped`; none left: 400; `title` at most 200 characters, default
    "Shared photos") makes a link over a hidden system album `share:<token>` that holds the chosen items. The public
    side is the album-share machinery above, unchanged: `decide` (Verus AS1, `verification/verus/album_share.rs`) and
    the kernel file were not touched. Answer 201: the row as below plus `skipped`.
  - The hidden album has `system_key` `share:<token>`. It is not in `GET albums`, not counted, not in an item's
    `albums` or `album` title, not in search text, not found by `?album=<title>`, and an upload's `meta.album` with
    the same title makes its own album; `GET|PATCH|DELETE albums/{id}`, `albums/{id}/items`, `albums/{id}/shares` and
    batch `album_add|album_remove` answer 404 for it (`db::get_album`, `db::not_share_album`).
  - `GET shares` and `GET albums/{id}/shares` rows: `{token, url_path, public_path ("/a/<token>"), kind
    ("album"|"selection"), album_id (null for a selection), title, count (items served now), cover_item, cover_thumb,
    created_at, expires_at, revoked_at, allow_original, views, last_view_at, live}`. `DELETE shares/{token}` revokes
    either kind.
  - `expires_in_days` is clamped to 1..3650 (it used to be unbounded and could overflow) for both kinds of link.
- One item's stored forms, for the compare view (`renditions.rs`):
  - `GET items/{id}/renditions` → `{item_id, kind, original, versions, renditions, device}`. `original`: `{version,
    format, mime, bytes, blake3, width, height, stored ("store"|"spool"|"external"), reachable, url}`; `versions`: the
    older originals (`item_versions`) with `{version, format, mime, bytes, blake3, replaced_at, stored, reachable,
    url ("original?version=N")}`; `renditions`: those that exist of `grid` (JPEG XL 256 px), `preview` (JPEG XL
    1440 px), `thumb` (`s.jpg`), `large` (`l.jpg`), `poster`, `device_preview` (the JPEG the phone sent with the
    upload), each `{name, format, mime, bytes, width, height, url}` (JPEG XL sizes come from the codestream header).
    A host path is never in the answer. 404 for an unknown or malformed id; a trashed item still answers.
  - `device` is `null` unless the item has a device (`items.device`, or one named in an upload's meta). Otherwise
    `{id, name, kind, last_seen_at, asset_id, uploads, latest}`: one `uploads` entry per finished upload row of the
    item (an edit adds one), each `{filename, mime, uti, bytes (the size the app declared), blake3 (the hash the app
    sent, null when it sent none), width, height, taken_at, uploaded_at, matches_version (the stored version whose
    hash equals the reported one)}`. `uti` is a new optional upload `meta` field (the type identifier, `public.heic`).
    This is only what the upload carried; the file on the phone cannot be read from the server. An upload that was
    skipped as a duplicate before it started leaves no row.
  - `GET items/{id}/original?version=N` serves an older version (404 for a version that does not exist), with the
    type its blob's extension names; the current version's number gives the current original.
    `GET items/{id}/device-preview` serves the phone's JPEG (404 when none). Index `uploads_item` on `uploads(item_id)`.
- Internal: `PUT|DELETE /internal/v1/external/{source}/{id}` indexes a file in place (voice memos, the film folders), `POST /internal/v1/import {path, source, album}`.

The upload protocol's decisions (chunk offsets, finish once, dedupe or version) are pure functions in `protocol.rs`.

## AI Lab: "Edit with Lucina…" (2026-10-28)

A prompt, and optionally a reference photo, edit one picture with a small image model that runs **on nezopt, in the
background, one job at a time**. Nothing goes to a cloud. The result waits in the Lab (`/gallery/lab`); Accept puts it in the
library, Discard deletes it, and Lucina says "Your picture is ready" when a job finishes. Code: `src/lab.rs` (queue table,
accept, discard), `src/lab_run.rs` (worker, runner, notification), `src/lab_http.rs` (routes).

**Routes** (all under `/v1/media`, behind the upstream key like the rest; jobs are JSON with `id` = `lab_<32 hex>`):

| Route | |
|---|---|
| `POST items/{id}/lab` `{prompt, reference_item_id?, steps?, seed?}` | Queue an edit. 201 + the job. 400: empty or over 1000 characters, not a photo, in the Trash, a reference that is the item itself or not a photo. 404: no such item. 429: 30 jobs already queued. |
| `GET lab[?status=a,b&item_id=…&limit=n]` | `{enabled, jobs}`: running first, then the queue (oldest first), then the newest. Without `status`, discarded jobs are left out. `enabled` is whether `LUCINA_MEDIA_LAB_CMD` is set. |
| `GET lab/{job}` | One job. |
| `GET lab/{job}/result` | The finished picture (PNG), while the job is `done`. |
| `GET lab/{job}/before` | What the model was given (the item as displayed, scaled), while the job keeps it. |
| `POST lab/{job}/accept` | `done` only: the result becomes a **new item**; 200 `{job, item, duplicate}`. |
| `POST lab/{job}/discard` | `done` or `failed`: deletes the result files; the row stays as `discarded`. |
| `POST lab/{job}/retry` | `failed` only: back in the queue with the same request. |
| `DELETE lab/{job}` | 204 for a queued, failed or discarded job; 409 for a running one (it finishes first) and for `done` / `accepted` ones. |

A job: `{id, item_id, reference_item_id, prompt, status, phase, progress, error, seed, steps, model, attempts,
queue_position, created_at, started_at, finished_at, accepted_item_id, result: {hash, bytes, width, height}|null,
original: {id, title, width, height, rotation, thumbhash, trashed}|null, urls: {before, after, reference, result}}`.
`status` is `queued | running | done | failed | accepted | discarded`; while running, `phase` is `waiting` (for the GPU) and then
`generating`, and `progress` is 0 to 100. `urls.before` is `…/lab/{job}/before` while the input is kept, else the original's
1024 px thumbnail; `urls.after` is the result while `done` and the new item's thumbnail once `accepted`.

**Accept makes a new item, never a new version.** The original keeps its bytes, hash, version and device asset (a phone upload of
the original must still find it, and a "version" would replace what the Gallery shows). The edit is a new item with source
`lucina-lab` (so it is in the Gallery's All and Lucina groups, with no device: a device deletion can never target it), the
original's `taken_at` and place, the prompt as its caption, and `links: {derived_from, lab_job, prompt}`. The original's `links`
gets `lab_edits: [new ids]`; that is the only column of the original that changes. The result's bytes are copied before ingest, so
a failure leaves the job `done`. If the bytes already are an item, nothing new is made (`duplicate: true`). The tables have no
foreign keys, so purging an original from the Trash is never blocked by an old job.

**Worker.** One background task claims the oldest queued job, writes `input.png` (the item as displayed: EXIF orientation and the
saved rotation applied, long side at most `LUCINA_MEDIA_LAB_MAX_SIDE`, 1024) and `reference.png` into
`<data_dir>/lab/<job>/`, and runs `LUCINA_MEDIA_LAB_CMD --input IN --prompt TEXT --output OUT [--reference REF] [--steps N]
[--seed N]` without a shell, in its own process group. The command owns the GPU lease (`gpu-lease run`) and exits when done, so
the model never stays in VRAM. On stdout it may print `MODEL <name>`, `STARTED` (lease granted) and `PROGRESS <0-100>`; its
stderr tail is the error of a failed job. Two clocks: `LUCINA_MEDIA_LAB_WAIT_TIMEOUT` (7200 s) before `STARTED`,
`LUCINA_MEDIA_LAB_TIMEOUT` (1800 s) after it; at the limit the group gets SIGTERM, then SIGKILL after 5 s. Jobs left `running`
by a restart are queued again. Events on `/v1/media/events`: `lab.queued`, `lab.running`, `lab.done`, `lab.failed`,
`lab.accepted`, `lab.cancelled` (`item_id` carries the job id, except `lab.accepted`: the new item).

**Telling the owner.** When a job ends the job is marked announced (once), then: the app inbox through secretary-server
`POST /v1/notify` (service token, category `lucina_message`, title "Lucina Lab", body "Your picture is ready" or "Your picture
could not be made", route `/gallery/lab`, `dedupKey lab:<job>`), the same path the voice memos use; and Lucina's Telegram bot
(app push needs APNs, which is off, so Telegram is what buzzes the phone). A failed send never fails the job. Environment:
`LUCINA_SECRETARY_SERVER_URL`, `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (as lucina-memo), `TELEGRAM_BOT_TOKEN_LUCINA[_FILE]`,
`LUCINA_MEDIA_LAB_TELEGRAM_CHAT_ID` (else `TELEGRAM_OWNER_CHAT_ID`, else the first `TELEGRAM_ALLOWED_CHAT_IDS`),
`LUCINA_MEDIA_LAB_NOTIFY_APP` / `LUCINA_MEDIA_LAB_NOTIFY_TELEGRAM` (`off` to silence a channel), `LUCINA_MEDIA_LAB_LANG`
(`en`, `it`, `da`), `LUCINA_MEDIA_LAB_LINK_BASE` (default `https://getsecretary.tech`).

**The model** is `~/models/lab/lab-edit` on nezopt (chosen and measured in `~/scratch/gallery/lab/MODEL.md`): stable-diffusion.cpp
(Vulkan) with FLUX.2 klein 4B, Apache 2.0. Set `LUCINA_MEDIA_LAB_CMD=/home/nez/models/lab/lab-edit`.

## Clients

- **iPhone** (`ios/Secretary/Features/Gallery`):
  - The Gallery (a plugin card, `GalleryScreen` via `app.showGallery`, `scope=gallery`) shows the phones' photos and
    videos: a timeline by month, search, albums, and a viewer that zooms out of its tile and back. Phone photos come
    from PhotoKit; server renditions are cached on disk and a cut-off preview resumes with `Range`.
  - The Drive (`DriveScreen`, `app.showDrive`) browses the Drive folders with breadcrumbs, sort, type chips and search
    (`GET drive`, `drive/browse`); PDFs and Office files open in Quick Look, text and code in a highlighted viewer,
    archives as a listing, media in the viewers; every preview has Share and Download. Links: `/drive`,
    `/drive/folder/<folder>/<path>`, `/drive/file/<id>` (branch `cloud/drive-ios`). Films have their own plugin
    (the player session's, `scope=films`).
  - Photo backup (`PhotoBackup.swift`) reads PhotoKit newest first.
    - It sends originals (edited renderings as new versions, both halves of Live Photos) and a JPEG preview, because the server cannot decode HEIC.
    - Transfers go through a background URLSession. A BGProcessingTask and the app-refresh task continue the work.
    - Rules: Wi-Fi only, while charging, include videos. It never deletes on the phone.
    - While Secretary is closed (iOS 27), iOS uploads new photos itself: see "Background uploads" below.
- **Lucina** (`lucina-tools` `media` tool, `lucina-server` `chat/media_turn.rs`):
  - Requests about photos, videos or albums run a short tool loop.
  - Results go to "Lucina's picks", and the reply gets a "Show in Gallery" route button (`/gallery/album/<id>`).
- **Imports:** `lucina-media import <dir|takeout.zip> --source takeout|nextcloud --album …` copies files and never moves them. Takeout JSON sidecars supply dates and places. `lucina-media index-folder <label> <dir>` indexes in place.

## Identity: one photo, one item (2026-10-28)

The owner's rule: never duplicate. Each picture has its own id, so a HEIC on the iPhone and a JPEG XL on the server
are the same item.

- **The item is the photo.** `items.id` (`itm_…`) names the picture, not a file. Everything that is a way of
  holding that picture hangs off it:
  - the **original** (`items.hash`, the current bytes) and its older **versions** (`item_versions`, an edited phone
    asset);
  - the **renditions** made from it: ThumbHash, grid tile, preview, `s.jpg`/`l.jpg` (see "Renditions"); the planned
    archive JPEG XL is one more rendition of the same item, never a second item;
  - the **alternates**: other encodings of the same photo that reached the library through another path, kept as
    hidden items under it (below). `GET items/{id}/duplicates` lists them.
- **Two keys resolve to an item**, and both must keep resolving after a merge:
  - the **content hash** (blake3 of the stored bytes): the same bytes are the same item (`ingest::known_for`,
    `insert_new_or_duplicate`);
  - the **device asset** `(device, asset_id)`: the same phone asset is the same item; new bytes under it are a new
    version of that item. A web upload with no asset id uses the SHA-256 of its content as asset id
    (`web/src/api/mediaUpload.ts`).
- **How a device's HEIC and the server's JXL map to one item.** The phone sends its original (HEIC) once; the item is
  `(iphone, A1)` with `hash = blake3(HEIC)`. The archive JPEG XL is stored as a rendition of that item and the
  device asset never changes. If the same picture arrives some other way (an old backup's JPEG, a chat share, a
  resized copy), it is first its own item; the duplicate detector (below) links it to the phone's item. A
  re-upload of `(iphone, A1)` or of any of those bytes then lands on the survivor, not on a new item.
- **Near-duplicate detection** (`dedup.rs`, `phash.rs`): exact duplicates are already folded at ingest. The rest
  are found by a perceptual hash of the 360 px thumbnail (a 64-bit DCT pHash and a 64-bit dHash, computed lazily into
  `item_hashes`), among pairs that agree on the moment (taken within 2 s), on file name and size, or look the same.
  `dedup::classify` is a pure function with four answers:
  - `exact`: the same bytes (should not exist);
  - `same_photo_reencoded`: the same picture, same size, another encoding;
  - `resized_copy`: the same picture at another size;
  - `burst_neighbor`: alike but different frames. **Not a duplicate.** Two assets of one device, or two differently
    named files of one collection, are never duplicates of each other however alike they look.
- **Merging** (`dedup_merge.rs`) hides, never deletes. `lucina-media dedup-apply --report <file>
  --i-have-a-backup` merges what a `dedup-report` found; `dedup-undo <item>` reverses one merge.
  - The duplicate gets `items.hidden = 2` (every listing, count, share and search already skips hidden items; 1 is a
    Live Photo's motion part) and a row in `item_links(item_id, duplicate_of, kind, score, decided_by, created_at,
    undone_at, undo)`. Its bytes and row stay: they are the survivor's alternate rendition, still served by
    `items/{dup}/original`.
  - Survivor rule: the device's original. A picture with 5% more pixels wins; otherwise the one that is a device's
    own asset (not a chat copy); otherwise the earlier arrival. A survivor outside the Gallery (an old backup) takes
    the duplicate's `device`, so the photo stays in the Gallery.
  - Album memberships (system albums, Favourites too), the favourite flag, album covers and a Live Photo's motion
    part move to the survivor. `undo` puts them back.
  - The duplicate's `(device, asset_id)` becomes a row of `item_aliases(device, asset_id, item_id = survivor,
    from_item, hash)`; `asset_id` is cleared on the hidden row. Ingest looks aliases up first: the same bytes are a
    duplicate of the survivor, other bytes under that asset are a new version of the survivor, and a content hash
    that matches a hidden duplicate resolves to the survivor (`dedup_merge::asset_lookup`, `resolve`).
    `GET devices/{id}/assets` still lists the asset, with the survivor's id.
  - `GET items/{id}` of a hidden duplicate answers with the survivor and adds `merged_from: <id asked>`.
  - Purging a survivor (it must be trashed) purges the copies hidden under it, and drops the links, aliases and
    hashes of everything purged. Trashing and restoring a survivor leave the hidden copies alone.
  - `item_links` rows stay after an undo (`undone_at`), so a pair the owner split is not proposed again
    (`dedup-apply --redo-undone` overrides).
  - Drive files are never candidates or survivors: a file of a Drive folder stays where the owner put it, copies
    included (the live Drive holds 634 byte-identical photo copies on purpose).
- **Commands.**
  - `lucina-media dedup-report [--report path] [--hashes path] [--data-dir dir]` opens the catalog read-only,
    keeps its perceptual hashes in a scratch database (never in the catalog; it refuses a path inside the data
    directory), pauses while the CPU is at 90 °C or more, and writes the full report (counts per class and per
    pair of sources, the bytes a merge would hide, 20 sample pairs, the calibration histograms, every pair and the
    merge plan; opaque ids only). Run it under `systemd-run --user --scope -p CPUQuota=300% nice -n 19`.
  - `lucina-media dedup-hash [--limit n]` computes the hashes into the library's own `item_hashes` table.
  - `lucina-media dedup-apply --report <file> [--min-score 0.7] [--classes …] [--limit n] [--dry-run]
    --i-have-a-backup`: hides what the report found. A dry run reads only the report.
  - `lucina-media dedup-undo <item>`.
- **Thresholds** (`dedup::Thresholds`, distance = the mean of the two 64-bit Hamming distances):
  - a duplicate that agrees on the moment (within 2 s, or a time zone read differently) or on file name and
    size: distance up to 10 (12 when the sizes differ);
  - a duplicate with no such evidence (a chat copy without EXIF): distance up to 4 (6 scaled), and a score below
    the 0.7 that `dedup-apply` asks for, so it is proposed, not applied;
  - similar but not duplicate (burst neighbour): up to 22.
  - Chosen on the live library (docs and `~/scratch/gallery/dedup/REPORT.md`): known copies (the library's own
    thumbnails re-encoded at JPEG quality 30/60/90, or scaled to 50% and 25%) all fall under 8; unrelated
    pictures of the same size taken more than a day apart reach 4 only 4 times among 4.1 million pairs; different
    assets of one iPhone taken together span 0 to 22, which is why they are excluded by identity, not by distance.

## Duplicate review and stacks (2026-10-28)

- **Groups** (`dedup_review.rs`): `GET duplicates` runs the detector over the stored hashes (`find_pairs`, then
  `plan_merges`, grouped by surviving photo) minus the pairs the owner dismissed or split. A request never hashes
  pictures: photos without a row in `item_hashes` are reported as `hashes.pending` (run `dedup-hash`; nothing hashes
  new uploads yet). Stacked photos are left out of the candidates. The pair search is a few tens of milliseconds
  per thousand photos squared; it runs outside the catalog lock.
- **Keeper suggestion**: clearly more pixels (5 %), else the larger file, else more metadata (capture time, place,
  camera, caption, size, favourite, a user album, original name), else the device's own asset, else the earlier
  arrival. `reason` names the rule that beat the runner-up (`highest_resolution`, `largest_file`, `most_metadata`,
  `device_original`, `first_added`). A group's `score` and `class` are those of its weakest merge; `applicable` means
  the score reaches the `dedup-apply` threshold (0.7).
- **Resolving** hides the `drop` photos under `keep[0]` with the undoable merge (the link records the detector's class
  and score, or `manual`). Further `keep` ids stay, and each pair of them is recorded in `dedup_dismissed`. Besides
  what the merge carries (albums, favourite, cover, device asset, Live Photo part), the keeper gets: the copies'
  captions joined (each text once), the location of the copies when it has none and they all agree within 100 m,
  and a capture time when it has none and the copies agree within 2 s. `dedup_meta_undo` records this per copy;
  `dedup_merge::undo` (the CLI's `dedup-undo` and `POST duplicates/undo`) reverts each field unless the owner has
  changed it since. Skips: `missing`, `same_item`, `not_photo`, `trashed`, `hidden`, `stacked`, `undone_before`,
  `drive_file`, and `keep_*` when the keeper itself cannot be used.
- **Stacks**: tables `stacks` and `stack_items` (not `item_links`: a link makes `GET items/{copy}` answer with the
  survivor, and a stack member must stay readable). The primary stays visible; the other members have
  `items.hidden = 3`, so every listing, count, month, source total and search skips them. A trigger on
  `items.trashed_at` keeps stacks whole for every trashing code path: the next member becomes primary, the trashed
  photo leaves the stack (and shows in the Trash), a stack of one dissolves. A restored photo does not rejoin.
  Batch ops on a stack tile act on the primary only. A photo is in at most one stack; videos and Drive files are not
  stackable.
- **Suggestions** (never applied by themselves): bursts are shots of one device, each within 1.5 s of the one before,
  and, when both have a hash, within the "similar" distance (22) and the same shape (`evidence: time_and_look`;
  `time` when a hash is missing). RAW+JPEG pairs share device, source and file name, differ in extension (a RAW
  such as `.dng` and a picture) and were taken together (`evidence: name`); the picture is the suggested primary.
  `stack_dismissed` keeps the sets the owner turned down. **No burst identifier is available**: the iPhone upload
  sends `taken_at` (milliseconds) but not PhotoKit's `burstIdentifier`, `meta.rs` does not read Apple's BurstUUID, and
  the scan does not set `includeAllBurstAssets`, so PhotoKit hands over only each burst's key photo.

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

## Photo edits (2026-10-28)

The Gallery's editor (web `components/gallery/editor`) edits non-destructively: an edit list per item plus a rendered derivative. The original file is never touched.

- **Edit list** (`secretary_image::edit::EditList`, JSON, `"v": 1`; the format and the render order are documented at the top of `crates/secretary-image/src/edit.rs`): quarter turns, straighten (±45°, cut to the largest inscribed rectangle), a crop box, nine adjustments (exposure in EV, contrast, highlights, shadows, saturation, warmth, tint, vignette, fade), a named filter with a strength, and markup (pen, highlighter, arrow, text; frame coordinates).
- **One renderer.** The web editor previews through `image-core.wasm` (`image_edit`, on a copy of at most 1600 px); the server renders the same list from the original at full resolution. Single-threaded, one job at a time (`Library::jobs`), on the blocking pool.
- **Routes** (`edits.rs`):
  - `GET items/{id}/edits` → `{ edited, edits, updated_at, base_url, item }`.
  - `PUT items/{id}/edits` (body: the list) validates (400 with the reason), renders, writes `thumbs/<id>/edited.jpg` (JPEG q92), stores the list in `item_edits`, remakes `s.jpg`, `l.jpg`, `g.jxl`, `p.jxl` and the ThumbHash from the edited picture, sets the item's width and height to the derivative's, and answers like GET. An identity list is a revert. 400 for non-photos, 409 in the Trash, 422 when the server can't decode the picture, 503 when the original is unreachable.
  - `DELETE items/{id}/edits` reverts: list and derivative go, renditions are made again from the original, width and height come back. Idempotent.
  - `GET items/{id}/edits/base` is the unedited picture at 1600 px (the editor's working copy; `l.jpg` while not edited, `base.jpg` after). `GET items/{id}/edits/download` is the derivative as `<name>-edited.jpg`.
- **Item JSON:** `edited: bool`; `urls.edited` (`r/edited`, the full-size derivative) and `urls.download_edited` while edited. `urls.original` and `urls.download` stay the untouched file. An edited item's rendition URLs carry `v=<version>e<stamp>` (the derivative's mtime), so no cache mixes the two looks; a revert returns to the old URLs.
- **HEIC and other formats the server can't decode** are edited from `preview.jpg` (the phone's rendering), the same source the renditions use. `thumbs::generate` makes renditions from `edited.jpg` while it exists, so a regeneration keeps the edit.
- A new original for the same asset (the phone re-sends an edited photo) forgets the edit list. Purging an item removes its list and derivative with the rest of its thumbs.
- Not done: JPEG XL for the derivative (it is JPEG so the renditions can be made from it with the `image` crate); edits for videos; a batch "edited" filter in the list API; text outlines and fonts beyond the embedded Latin subset.
## Archive format: JPEG XL (2026-10-28)

The backup photos can be stored as JPEG XL without losing a bit of picture. Modules `archive_jxl.rs` (storage, apply, restore),
`archive_png.rs` (PNG pixels, digest, rebuild), `archive_manifest.rs` (manifest, export) and `archive_jxl_measure.rs` (dry run);
command `lucina-media archive-jxl`. Numbers and the decision per type are in the build report
(`~/scratch/gallery/format/REPORT.md`); nothing is applied by default.

| Original | Archive action | Result |
|---|---|---|
| JPEG | `jpeg_recompress`: libjxl JPEG transcoding with the reconstruction data stored (effort 7) | the JXL rebuilds the original file **bit for bit** (blake3 and SHA-256 checked) |
| PNG | `png_lossless`: decoded pixels, lossless, effort 7, sRGB or Display P3 | **pixel-exact**, not byte-exact: after the original is deleted a PNG with the same pixels, layout and ICC profile is rebuilt |
| TIFF, BMP | `lossless_jxl` measured only | never applied |
| HEIC, RAW, GIF, WebP, AVIF, video, other | `keep_original` (HEIC/RAW also get a measured display derivative) | never touched by `--apply` |

- **One identity.** `items.hash` stays the original's hash (device comparison, dedup, downloads' identity do not change).
  Table `archive_blobs(hash, kind, jxl_hash, jxl_bytes, reconstructs_to, original_bytes, original_ext, verified_at,
  original_deleted_at, original_sha256, pixel_digest, png_color, icc)` records the JXL, kept at
  `<store>/archive-jxl/ab/cd/<hash>.jxl`. `kind` is `jpeg_recompress` or `png_lossless`; the migration adds the columns and
  widens the `kind` check (rows kept). `reconstructs_to` is the blake3 of the JPEG, or the pixel digest of the PNG.
- **JPEG verification.** The JPEG rebuilt from the JXL must have the original's blake3 *and* SHA-256 (`original_sha256`);
  it is checked at conversion, again from the JXL on disk, before any deletion and at every rebuild.
- **PNG verification.** The pixel digest is a SHA-256 over width, height, colour layout (`l8`, `la8`, `rgb8`, `rgba8` and the
  16-bit ones), every sample (16-bit little-endian) and the ICC profile bytes. It is taken from the original file; the original
  may be deleted only after the JXL, read back from disk, decodes to the same digest (and the PNG built from it decodes to it
  too). Refused (original kept, listed as a failure): palette and 1/2/4-bit PNGs, animated PNGs, a `tRNS` chunk, a profile
  other than sRGB or Display P3 (judged by colorants and tone curve), float data. Text, EXIF, resolution and gamma chunks are
  not carried over. The ICC bytes are stored in the row (the JXL only tags sRGB or Display P3).
- **Reading.** `ingest::original_path` rebuilds an original whose file is gone (`archive_jxl::materialize`, also used for an
  older version's file): JPEG bytes must hash to the recorded blake3 and SHA-256, a PNG must decode to the recorded digest,
  or nothing is served (503). The rebuilt file is cached under `<data>/archive-cache/<hash>.jpg|png` (1 GiB, oldest out), so
  `GET items/{id}/original` (with `Range`, `If-None-Match`, `?download=1`), thumbnails and `details` all read it.
- **PNG serving.** A rebuilt PNG is deterministic for a given library version (fixed compression and filter, `iCCP`
  embedded, no timestamp) but is not the original file, so `items/{id}/original` answers it with a **weak ETag** `W/"<item hash>"`
  and `Content-Type: image/png`. `If-None-Match` matches the weak or the old strong tag (the pixels are the same, 304). `If-Range`
  never validates a weak tag, so a request with any `If-Range` gets the whole file (200) instead of a range: a client resuming
  with the pre-conversion strong tag cannot be handed bytes of the rebuilt file. `items.hash` and `items.bytes` stay the
  original's (the served length differs). A JPEG is still served byte for byte with its strong ETag.
- **Applying.** `archive-jxl --apply --i-have-a-backup [--kinds jpeg,png] [--delete-originals] [--limit N] [--manifest P]
  [--max-temp 90]` refuses without the backup flag, on quiet hours and when the store is not mounted. One run is one batch
  (`--limit` counts converted or deleted files, in hash order; run it again for the next batch). Per file: the stored object is
  re-hashed against the catalog, converted, the JXL written durably, read back from disk and verified once more, then the row
  is recorded. An original is deleted only with `--delete-originals`, only after that second verification, and only from the
  owned store (never `external` files or the spool). The CPU temperature is checked before every file (pause while Tctl is at or
  above `--max-temp`, default 90 °C); the CPU cap is the caller's `systemd-run -p CPUQuota=…` (apply is single-threaded).
  Output is counts only (`converted_by_kind`, `failed` by reason, `temp_pauses`).
- **Manifest.** `--manifest P` appends one JSON line per processed file and item, fsynced every 50 lines and at the end of the
  run: `{item_id, device, asset_id, kind, original_rel_path, original_bytes, original_blake3, original_sha256, jxl_rel_path,
  jxl_bytes, jxl_blake3, verified, verified_at, original_deleted_at, error?}` (PNG lines add `pixel_sha256`; paths relative to
  the store). A file that failed has `verified:false` and `error`, keeps its original and is also appended to
  `P.failures.jsonl`. `archive-jxl --export-manifest P` writes the complete manifest from the catalog (read-only), and is the
  authority after an interrupted run.
- **Restore.** `archive-jxl --restore-originals` rebuilds every deleted *JPEG* original into the store, byte for byte. A deleted
  PNG original cannot be put back byte for byte: those rows stay served from the JXL and are counted (`png_not_restorable`);
  restore PNGs from the backup of `objects/`.
- **Purge.** A purged blob takes its JXL, row and cache file with it.
- **Dry run.** `archive-jxl --dry-run [--sample N] [--report path]` opens the catalog read-only, converts and verifies a
  stratified sample, writes nothing to the library, and reports sizes, ratios, timings and failures by reason (opaque ids only).

## Backup and the device rate limit

Every `/v1/media/*` request outside `/s/` passes p1's forward_auth, which allows 60 requests a minute for each device. The backup therefore:
- waits 4 s between assets (create, PUT, finish; the JPEG preview only for HEIC), about 36 requests a minute;
- stops on 429 and comes back after `Retry-After`;
- pauses the whole pass on network, timeout and 502–504 errors (retries after 120 s), and skips just the asset on anything else.

An upload whose bytes are gone answers 410 on finish, and the phone starts that asset again. Ingest and the "finished" mark run in one blocking step, because a client hang-up drops the handler.

## Background uploads (2026-11-01)

Before, a photo reached the library only when Secretary ran: under a minute with the app open, 6 to 45 minutes when
iOS woke it for a BGProcessing or app-refresh task. On iOS 27 the PhotoKit background upload extension
(`ios/Extensions/SecretaryPhotoUpload`, `com.apple.photos.background-upload`) closes that gap: iOS calls it when there
is upload work and performs the uploads itself, also while the app is closed.

- **Phone.** The extension makes one upload job per new photo (and Live Photo motion half), with the same asset ids,
  metadata and edited-rendering rules as `PhotoBackup.swift` (`BackgroundUploadShared.swift`). It only takes photos
  added after it was turned on, plus edits of photos it sent; the app's backup keeps the backlog. There is no App
  Group, so the app passes the server, key and device id through the shared keychain group, and the extension
  passes back each photo it sent (the app's next pass moves them into its ledger). The backup turns it on with
  `enableUploadJobExtension` (Wi-Fi only = `preventsExpensiveNetworkAccess`), and turns it off while "Only while
  charging" is on. Photo backup settings show its state. Logs: `log stream --predicate 'category == "PhotoUpload"'`.
- **Info.plist.** `BackgroundUploadURLBase` must be inside `EXAppExtensionAttributes`: with it only at the top level,
  as Apple's tutorial shows, assetsd refuses the extension (`com.apple.photos.error` 50207, "missing valid a
  Info.plist BaseURL key"). `ios/project.yml` sets both.
- **Server** (`bgupload.rs`, `resumable.rs`; own listener `LUCINA_MEDIA_BG_LISTEN`, 100.107.164.33:8831). iOS speaks
  the IETF resumable upload draft (interop version 6 seen):
  - `OPTIONS /v1/media/bgupload`: 200 with `Upload-Limit` (20 GB, p1's cap);
  - `POST /v1/media/bgupload`: the file, with the metadata in `X-Lucina-Meta` (base64url `ClientMeta` JSON). The server
    sends `104 Upload Resumption Supported` with the upload's URL, then 201 with `Upload-Offset`, `Upload-Complete`
    (or the older `Upload-Incomplete`, answered in the client's dialect) and `X-Lucina-Item`;
  - `HEAD`, `PATCH` (`Upload-Offset`, exact or 409) and `DELETE` on `/v1/media/bgupload/<id>.<token>`.
  - hyper can't send a 1xx, so this listener hands each request its socket and the handler writes the 104 itself.
  - The upload URL is a capability (a random token, stored as its blake3), because iOS may resume without the
    original headers. Unfinished uploads are dropped after a week (`sweep_uploads`).
  - A completed upload is ingested like the app's own: dedupe by device and asset id, then by hash.
- **p1 Caddy** (`/etc/caddy/Caddyfile`): the POST needs a `Bearer` device key and passes forward_auth; the preflight and
  the capability URLs skip device auth, but Caddy still adds the upstream key.
- **HEIC without the phone's preview.** The extension sends only the original. ffmpeg 8 decodes an iPhone HEIC's tiles
  but does not join them, so `heif.rs` decodes each tile stream and stitches them (cropped, EXIF-oriented, 2048 px)
  into `preview.jpg`.

## Internal API for other services on nezopt

- `POST /internal/v1/ingest {path, mime?, name?, source?, album?, taken_at?, caption?, move?}`: a local file into the library (shares to Lucina). Answers `{item_id, duplicate, kind, mime, thumb_state, urls}`.
- `PUT|DELETE /internal/v1/external/{source}/{id}`: index a file in place (voice memos, film folders).
- `GET /internal/v1/items/{id}/path`: the original's path on disk.

## Not done yet

- Duplicates: a lazy hash worker in `serve` (today `dedup-hash`), and queuing the hidden copies' device assets for
  deletion with the survivor.
- Bursts: the iPhone upload sending `burstIdentifier` (and fetching every burst frame), to replace the time-and-look guess.
- Web gallery tab.
- The iOS consumer of the device deletion queue; face recognition (`people`).
- Android upload: the Rust module for the Android app.
- WebDAV upload endpoint.
- HEIC decoding on the server (needs a user-space libde265 + libheif build); the phone's preview covers iPhone uploads.
- Android and web bindings for `secretary-image` (JNI; wasm in the player's `ptr<<32|len` style).
- Zoom/pan on the device is unit-tested only: XCUITest can't run on iOS 27 with Xcode 26.
- Stateright/TLA+ model of the upload protocol.
- HLS cache eviction.
- Archive format: TIFF/BMP stay as originals; video preview renditions are recommended in the build report, not built.
