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
- Internal: `PUT|DELETE /internal/v1/external/{source}/{id}` indexes a file in place (voice memos, the film folders), `POST /internal/v1/import {path, source, album}`.

The upload protocol's decisions (chunk offsets, finish once, dedupe or version) are pure functions in `protocol.rs`.

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

- Duplicates: the compare view over `items/{id}/duplicates`, a lazy hash worker in `serve` (today `dedup-hash`), and
  queuing the hidden copies' device assets for deletion with the survivor.
- Web gallery tab.
- The iOS consumer of the device deletion queue; face recognition (`people`).
- Android upload: the Rust module for the Android app.
- WebDAV upload endpoint.
- HEIC decoding on the server (needs a user-space libde265 + libheif build); the phone's preview covers iPhone uploads.
- Android and web bindings for `secretary-image` (JNI; wasm in the player's `ptr<<32|len` style).
- Zoom/pan on the device is unit-tested only: XCUITest can't run on iOS 27 with Xcode 26.
- Stateright/TLA+ model of the upload protocol.
- HLS cache eviction.
