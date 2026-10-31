# Lucina Gallery vs Immich: gap analysis (2026-10-28)

Immich v3.3.5 is a self-hosted photo and video backup (NestJS, Postgres with VectorChord, a Python ML service, a
Flutter app). The owner saw it on social media and asked which of its features are worth bringing into the Lucina
Gallery.

- **Reference clone:** `~/Projects/ReverseEngineering/immich` on nezopt (tag `v3.3.5`, shallow).
- **AGPL-3.0:** no Immich code is copied into Lucina. This page records features, defaults and design ideas only. Every
  implementation in Lucina is our own Rust/Solid code.
- **Lucina baseline:** lucina-media `51952b5` and web-live `189ac3b`. The live library holds about 11,900 items: 5,164
  in the Gallery (phone backups, old nespc backup, chat) and the rest in the Drive. There is one owner and one iPhone.

## Already at parity or ahead

These need no work. Lucina already has them, in several cases done better than Immich.

| Area | Lucina | Immich |
|---|---|---|
| Timeline at scale | Virtual grid, day/month sections, month scrubber with years, ThumbHash placeholders, JXL tiles, band selection | Bucket counts up front, a columnar month payload, justified wasm layout |
| Storage | Content-addressed blake3 store, quiet-hours spool, lossless JPEG XL archive (bit-exact JPEG) | Files per a storage template, no recompression |
| Exact dedup | blake3 plus `(device, asset_id)` at ingest; a duplicate finish makes one item | sha1 bulk check before upload |
| Perceptual dedup | pHash plus dHash with evidence gating (capture time, name/size), undoable merge | CLIP cosine distance ≤ 0.01 |
| Editor | Non-destructive: crop, straighten, 9 adjustments, filters, markup; the same Rust core runs as wasm in the preview | Crop, rotate and mirror only |
| AI editing | AI Lab, local FLUX.2 klein under gpu-lease | none |
| Map | Offline GeoNames reverse geocoding, MapLibre on self-hosted tiles, server clusters, bbox filter | The same idea, but Immich's hosted tiles by default |
| Trash | 30 days, an hourly purge, and "also delete from device" | 30 days, nightly purge |
| Video | Range, HLS on demand, resume positions, VAAPI film preparation | Transcode policy, and real-time HLS (off by default) |
| Share links | Album or selection, expiry, original download on/off, metadata stripped, Verus-verified access rule, 410 on revoke | Adds password, slug, allow-upload, show-EXIF |
| Compare | Server vs device copy and versions side by side or with a slider | none |
| Drive | Every file indexed in place, full-text search | External libraries for photos only |

## Gaps, ranked

**Value** is to the owner: a single user with one iPhone, about 5k gallery photos (growing) and a chat assistant that
already answers "show me photos of …" through the media tool. **Effort** is S (≤1 day), M (2–4 days) or L (a week or
more) for one builder, including tests.

| # | Gap | Value | Effort |
|---|---|---|---|
| 1 | Smart search (CLIP-style embeddings), "similar photos" | Very high | M |
| 2 | Faces and people | Very high | L |
| 3 | Live Photos playback | High | S |
| 4 | Duplicate review screen, plus stacks for bursts and RAW+JPEG | High | M |
| 5 | Albums page and album view on the web | High | S |
| 6 | OCR: text in photos, searchable | Medium-high | M |
| 7 | Share links: password, show-metadata, allow upload | Medium | S |
| 8 | Memories beyond on-this-day (birthdays, saved), a Memories viewer | Medium | S–M |
| 9 | Rule-based smart albums (Immich "workflows") | Medium | M |
| 10 | Locked folder (PIN, timed unlock) | Medium | M |
| 11 | iOS: selective albums, and the gallery features that are web-only today | Medium | M–L |
| 12 | Ratings, tags, XMP sidecars | Low-medium | M |
| 13 | Integrity checks and a jobs/status page | Low-medium | S–M |
| 14 | Partner sharing, shared albums with roles, comments and likes | Low now | L |
| 15 | Widgets and casting | Low | M |
| 16 | Watch folders, storage templates, multi-user quotas | Low | — |

### 1. Smart search and "similar photos"

- **How Immich does it:**
  - Each preview (1440 px JPEG) goes through a CLIP image encoder. The default is `ViT-B-32__openai`, 512 dimensions,
    and SigLIP2 and multilingual nllb/XLM models can be chosen instead.
  - Vectors are stored in Postgres with a VectorChord index. A query text goes through the text encoder, and the
    search orders by cosine distance with the metadata filters in the same SQL.
  - "More like this" reuses the stored vector of an asset.
  - Query embeddings are cached (an LRU of 100).
  - Changing the model rebuilds the column and re-runs the job.
- **What Lucina has:**
  - FTS5 over title, caption, place, month words, camera and album names.
  - Nothing looks at the picture itself. The media session designed SigLIP2-base via `ort` reading the 256 px grid
    renditions (routes `items/search`, `items/{id}/similar`, `embeddings/status`), but has not built it.
- **Value:**
  - Searches like "beach at sunset", "receipt", "my car" or "whiteboard" work in the web Gallery and in chat.
  - SigLIP2 is multilingual, so Italian and Danish queries work too.
  - "Similar photos" also finds near-duplicates that pHash misses.
- **Lucina plan:**
  - A new crate `lucina-media-ml` with SigLIP2-base (ONNX image and text towers through `ort`, loaded dynamically,
    the same way `lucina-memo` loads it).
  - Embeddings are 768 f16 values (about 18 MB for 12k items), stored as BLOBs in the catalog. Search is brute-force
    cosine over an in-memory matrix: at this size a scan takes about 1 ms, so no vector database is needed.
  - Search combines with the existing filters (source, kind, dates, bbox).
  - A backfill CLI runs on CPU in a capped scope, pauses at Tctl ≥ 90 °C, and leaves the 7900 XTX to Qwen. New items
    are embedded as they arrive.
- **Effort:** M.

### 2. Faces and people

- **How Immich does it:**
  - Detection uses InsightFace `buffalo_l`: the SCRFD detector (minimum score 0.7) and a 512-dimensional ArcFace
    embedding of the aligned crop.
  - Clustering is incremental and DBSCAN-like, with `maxDistance 0.5` (cosine) and `minFaces 3`.
    - A face with at least `minFaces` neighbours is a core point, and it creates or joins a person.
    - A weak face is deferred and retried after the batch, so later faces can claim it.
    - A nightly pass picks up new faces, and only runs when new faces exist.
    - Manual faces are never reassigned.
  - People have a name, a hidden flag, a feature face, a birth date (age at photo, birthday memories) and a favourite
    flag. Two people can be merged, faces can be reassigned and tagged by hand, and an "unassigned faces" view exists.
  - A "minimum faces to show a person" filter is applied at view time.
  - Re-detection keeps face ids by IoU > 0.5.
- **What Lucina has:**
  - `details.people` is an empty placeholder.
  - The plan in `docs/media-library-next.md` §3 is SCRFD with ArcFace, or YuNet with SFace as the Apache-2.0 option.
- **Value:** very high. People is the most used way to browse a family library, and it also lets chat answer "photos
  with Mum from last summer".
- **Licence choice:**
  - The InsightFace pretrained packs are for non-commercial research only.
  - YuNet (detector) and SFace (recognizer) from OpenCV Zoo are Apache-2.0, at somewhat lower accuracy.
  - Lucina is owner-only for now, but the safe default is YuNet + SFace, with the model behind a trait so ArcFace can
    replace it later.
- **Lucina plan:**
  - The same crate gets the `faces` and `people` tables, Immich-style incremental clustering (core points, deferral,
    manual faces pinned), and routes `people`, `people/{id}`, `people/{id}/items`, merge, rename/hide, and
    `items?person=`.
  - The web gets a People view and faces in the info panel.
- **Effort:** L.

### 3. Live Photos playback

- **How Immich does it:**
  - The motion `.mov` is a hidden asset linked to the still by `livePhotoVideoId`. The link is made from EXIF
    `ContentIdentifier`, so it works in either upload order.
  - The timeline payload carries the link, so a "LIVE" badge needs no extra request.
  - The viewer plays the motion on hover or long-press.
  - Android motion photos are split out of the JPEG's XMP container.
- **What Lucina has:** the iPhone already uploads both halves, and the catalog links them (`live_pair`; the motion half
  is `hidden=1`). Nothing in the web or the app plays them.
- **Value:** high, for little work. Every recent iPhone photo is a Live Photo.
- **Lucina plan:**
  - Expose `live` (the motion item id) in the item JSON.
  - Show a LIVE badge on tiles.
  - In the viewer, play the motion once on open and on hover or a long-press, muted, through the existing play plan.
- **Effort:** S.

### 4. Duplicate review screen and stacks

- **How Immich does it:**
  - A `/utilities/duplicates` page lists groups.
  - The suggested keep is the largest file, then the one with the most EXIF fields.
  - Keeping one merges metadata from the others:
    - albums: union;
    - favourite: any;
    - rating: highest;
    - descriptions: joined;
    - location: copied only if all copies agree;
    - tags: union.
  - "Deduplicate all" applies the suggestion to every group.
  - Stacks (`stack.primaryAssetId`) collapse a set into one tile with a count. They are made manually or from the
    duplicate screen. `autoStackId` (BurstUUID) is read but nothing consumes it automatically.
- **What Lucina has:**
  - The detection and merge engine exists (`dedup*.rs`, `item_links`, undo) but has only been used from the CLI.
  - The docs list "dedup compare view" as not done.
  - There are no stacks.
- **Value:** high. The owner asked for "no duplicates", and the compare view already exists to reuse.
- **Lucina plan:**
  - A review screen built on `dedup-report` and the compare view, with keep-suggestion and a metadata merge on the
    existing undoable merge.
  - Stacks on `item_links`, kind `stack`: automatic for iPhone bursts (same BurstUUID) and for RAW+JPEG pairs (same
    capture time and base name), plus manual stack and unstack.
- **Effort:** M.

### 5. Albums page and album view

- **How Immich does it:**
  - An Albums page with cover, count and date range.
  - An album view with its own sort order and description.
  - Shared albums with editor/viewer roles and comments and likes (that part is gap 14).
- **What Lucina has:** 29 albums in the catalog and an album picker, but no Albums list or album page on the web.
  Lucina's "Show in Gallery" links point at `/gallery/album/<id>`, which the web does not route.
- **Value:** high. It is a visible hole, and chat links to it already.
- **Effort:** S.

### 6. OCR

- **How Immich does it:**
  - PP-OCRv5 mobile (detection plus CTC recognition), with detection score ≥ 0.5 and recognition score ≥ 0.8, on the
    preview downscaled to 736 px.
  - It stores quad boxes and text, with a trigram index for search.
  - The viewer can draw the text boxes.
- **What Lucina has:** `extract-text` covers Drive documents (PDF, Office). Photos have none.
- **Value:** medium-high. Screenshots, receipts, signs and slides become findable through the existing FTS.
- **Lucina plan:** after gap 1, in the same crate: PP-OCRv5 ONNX through `ort`, text into `items_fts`.
- **Effort:** M.

### 7. Share link options

- **Immich adds:** password, a vanity slug, allow upload (guests add photos), show-EXIF on/off, and a description.
- **Lucina has:** expiry, originals on/off, metadata always stripped.
- **Lucina plan:**
  - An optional password (argon2 hash, a short-lived cookie after unlock), extending the Verus kernel's decision with
    a `password_ok` input.
  - "Show place and date" on/off.
  - Allow upload later, if guests are wanted.
- **Effort:** S.

### 8. Memories

- **Immich:**
  - A nightly job builds "on this day" memories for today ±3 days (one per past year) and birthday memories (named
    people with a birth date, shown 3 days before, up to 5 photos per year and 25 in total).
  - Memories can be saved and marked seen.
  - A full-screen story viewer with timed slides.
  - Memory widgets on iOS and Android.
- **Lucina:** an on-this-day strip and recently added.
- **Lucina plan:** a story viewer for the strip, with saved memories. Birthday memories come after people. Lucina can
  send the memory as a morning message, which Immich cannot.
- **Effort:** S–M.

### 9. Rule-based smart albums

- **Immich v3:** WASM plugin "workflows" with triggers (on create, after metadata) and filters (file name, location,
  date, type, EXIF, tag) that act (add to album, archive, lock, tag, favourite). Shipped templates are "archive
  screenshots" and "location album".
- **Lucina plan:** saved searches as albums: a stored filter, including smart search and person, shown as an album.
  Plus an "archive screenshots" rule. No plugin runtime is needed.
- **Effort:** M.

### 10. Locked folder

- **Immich:**
  - Visibility `locked`, a bcrypt PIN, and a session elevated for 15 minutes.
  - Locked items are hidden from timeline, search, faces and memories unless the session is elevated.
- **Lucina:** archive (hidden from the timeline) only.
- **Lucina plan:** `hidden=3` "locked", unlocked with the owner's QR sign-in or a PIN, and kept out of every listing
  including chat answers.
- **Effort:** M.

### 11. iOS app

- **Immich:**
  - Background backup with selective albums (include and exclude).
  - Hashes cached per asset.
  - Duplicates skipped before upload.
  - The motion half uploaded first.
  - "Free up space" (delete local copies already backed up).
  - Offline browsing from a synced local DB.
  - Widgets and Google Cast.
- **Lucina:**
  - Background backup of the whole library: BGProcessing, background URLSession, Wi-Fi/charging rules, HEIC plus a
    JPEG preview, edits as versions, favourites.
  - No album selection.
  - The app has the timeline, albums, favourites and search. Shares, Places, the editor, the Lab, trash management and
    the device-deletion consumer are on the web only.
- **Lucina plan:**
  - Selective albums (include/exclude).
  - The deletion-queue consumer, which also gives "free up space".
  - Then share links and Places in the app.
- **Effort:** M–L.

### 12. Ratings, tags, XMP

- **Immich:**
  - Ratings −1..5 (−1 = reject).
  - Hierarchical tags with a closure table.
  - Both read from and written to XMP sidecars (`digiKam:TagsList`, `xmp:Rating`).
  - `lockedProperties` keeps hand edits from being overwritten by re-extraction.
- **Lucina:** favourites and captions only.
- **Value:** low-medium for a phone-first library. It is useful if Lightroom/darktable folders are added to the Drive.
- **Effort:** M.

### 13. Integrity and jobs

- **Immich:**
  - Nightly integrity checks (missing files, untracked files, checksum mismatches), time-boxed to 1 hour.
  - A jobs page per queue (pause, run missing).
  - `.immich` marker files to detect an unmounted disk.
- **Lucina:** blake3 everywhere, but no periodic verify.
- **Lucina plan:**
  - A nightly verify of a slice of the store (blake3), and a mount-marker check on MyBook.
  - Results go into the existing server-health report.
  - The ML backfill shows its progress through `embeddings/status`.
- **Effort:** S–M.

### 14. Partner sharing and shared albums

- **Immich:**
  - Partners see your whole timeline, optionally merged into theirs.
  - Shared albums have owner/editor/viewer roles, and comments and likes.
  - Cluster groups share face identities between users.
- **Lucina:** single-user. Share links cover sending photos to people.
- **Value:** low now, higher if a partner or family gets accounts. Revisit when Lucina has a second user.
- **Effort:** L.

### 15. Widgets and casting

- **Immich:** iOS and Android memory and random widgets, and Google Cast from the app and the web.
- **Lucina plan:** an iOS widget for "on this day" (WidgetKit). AirPlay already works from Safari's video player.
- **Effort:** M.

### 16. Not worth it now

- **Watch folders:** the Drive already indexes folders in place, and new phone photos arrive by upload.
- **Storage templates:** content addressing is better for dedup and JXL.
- **Multi-user quotas, OAuth, maintenance mode, Prometheus:** single owner, and server-health covers monitoring.
- **Immich's columnar timeline payload:** Lucina's cursor pages with ThumbHash already scroll smoothly at this size.
  Revisit past about 100k items.

## Design ideas worth copying

- Keep faces with too few photos hidden at view time ("minimum faces"), so changing the setting never needs a recluster.
- Defer weak faces instead of making one-photo people, and cluster nightly only when new faces arrived.
- A duplicate merge never loses metadata: union of albums and tags, any favourite, and location only if all copies agree.
- Record which fields the owner edited by hand, so re-extraction never overwrites them.
- Hidden or locked state is copied onto face rows, so a people query cannot leak locked photos.
- A model change rebuilds the embeddings in the background, and search keeps using the old vectors until the new set
  is complete.

## Status (2026-10-28, evening)

**Live** (lucina-media `99f535d`, web-live `ad75e7a3`):
- Smart search: `lucina-media-ml` and `smart.rs`.
- The albums page and album view.
- Live Photos playback.
- The smart-search UI, with "Similar photos".
- **Smart search is off** (`LUCINA_MEDIA_ML` unset) until the owner OKs the backfill, about 70 CPU-minutes.

**Built, waiting for review and deploy:**

| Feature | Server branch | Web branch | Notes |
|---|---|---|---|
| Faces and people (gap 2) | `gallery-faces` | `gallery-faces-web` | YuNet + SFace, own clustering. Opt-in from the People page. |
| Duplicate review and stacks (gap 4) | `gallery-dedup` | `gallery-dedup-web` | Each branch also contains the faces branch, so it merges cleanly after it. |

**Follow-ups found while building:**
- **Live Photos on iOS.** The catalog has no Live pairs. The iPhone app sends the motion half only when "include videos" is on (`PhotoBackup.swift`, scan).
- **Bursts on iOS.** The app never sends `burstIdentifier` and doesn't fetch `includeAllBurstAssets`, so only each burst's key photo is backed up. Server-side bursts are guessed from 1.5 s gaps and pHash.
- **Duplicate hashes.** Nothing hashes new uploads (`item_hashes` is empty on the live catalog). The duplicate screen shows "hashes pending" until `dedup-hash` runs. A background hash step in the single embed thread is the natural fix.
- **Purge hooks.** `purge_in` calls `lucina_media_ml::forget_item`, which covers vectors and faces. Stacks keep themselves consistent with a trigger on `items.trashed_at`.
- **Phone header.** The Gallery header now has many icons and overflows at phone width. It needs a "More" menu.
- **Unrelated test failure.** `web/src/views/Shell.approvals.test.tsx` ("shows inside Secretary Code…") fails on web-live here, independent of the Gallery work.
- **Next gaps by rank:** OCR (6), share-link password (7), the memories viewer (8), smart albums (9), locked folder (10).

## Who builds what (agreed with the media session, 2026-10-28)

- **Media session:** owns `lucina-media` and `/gallery`, and is applying the JPEG XL archive. It reviews, merges into
  main-next and web-live, and deploys.
  - Hands off while it works: `archive_jxl*.rs`, `ingest.rs`, `trash.rs`, the `archive-jxl` command and
    `archive_blobs`.
- **Gallery-vs-Immich session (branch `gallery-ml` from main-next):**
  - the new crate `lucina-media-ml` for smart search, then faces and OCR;
  - thin glue in lucina-media (one module each, one router line, one migrate line);
  - web pieces under `components/gallery/<feature>/`.
- The live catalog and store stay read-only for builders. Tests use synthetic data. Nothing is applied to the library
  without the owner's OK.
