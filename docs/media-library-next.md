# Media library: the next update (plan, 2026-10-27)

The owner asked for five things at once (relayed on 2026-10-27): bring the old picture backups in, make the
library online-first like Mega or Dropbox, recognise faces on the server, link the Gallery and the chat like
WhatsApp media, and give albums shareable links. This is the plan. Each part ships on its own, with tests, and
with a formal model where it adds a protocol (`docs/verification.md` policy).

## 1. The old picture backups (started)

Found on nespc:

| Where | What | Size |
|---|---|---|
| partition `DANSKROMDS` (`/dev/nvme0n1p4`, not mounted), `foto-backup/DCIM (1)/` | an Android phone's DCIM: Camera, Screenshots, Facebook, CamScanner, PhotosEditor, Restored, Subito, Videocaptures | 11 GB, 4,250 photos, 20 videos |
| `~/Downloads/dirve/` (a Google Drive export) | `BackupIcloudPhotos`, `Foto pc`, `101_FUJI` (a Fujifilm camera), `Caccia` | 1.1 GB, 570 MB, 234 MB, 17 MB |

On nezopt's mybook, `Data/MyDocs/Cellulare` (115 photos) and `Fotocamera` (61) are small extras.

Copied (never moved) to `~/mnt/mybook/imports/nespc-2026-10-27/` on nezopt; the nespc partition was mounted
read-only for the copy and unmounted after. The import itself is `lucina-media import <dir> --source … --album …`:
it copies into the content-addressed store, takes dates and places from EXIF, and dedupes by hash, so anything the
iPhone already backed up is not stored twice.

Where they go (owner to confirm, see the end):

- old phone photos (`DCIM/Camera`, `Videocaptures`, `BackupIcloudPhotos`, `Cellulare`, `Fotocamera`, `101_FUJI`):
  the **Gallery** timeline, with `source` `phone-archive` (the Gallery is `source LIKE 'phone%'`), so they sit in
  the timeline by their EXIF date like Google Photos;
- everything else (`Screenshots`, `Facebook`, `CamScanner`, `PhotosEditor`, `Restored`, `Subito`, `Foto pc`,
  `Caccia`): the **Drive**, one folder each.

## 2. Online-first, "keep offline" like Mega and Dropbox

How they do it: Dropbox and Mega show every file at once and fetch it when opened; a file or folder marked
"Available offline" / "Make available offline" is downloaded and kept, and stays in sync; unmarking frees the space.
Nothing else stays on the device except a cache the system may empty.

What we have already: the Gallery streams (JPEG XL renditions, progressive), phone photos come from PhotoKit, and
server renditions sit in `Caches/gallery` (which iOS may purge).

To add:

- **Keep offline** on a photo, a selection or an album (and a Drive folder): the 1440 px preview (and, as an option,
  the original) goes to `Application Support/offline/` (never purged), with a badge on the tile; albums stay in
  sync (new items are fetched, removed ones dropped). "Remove from this iPhone" frees it. Settings shows the space
  used and a "remove all".
- **Save to Photos** (Mega's "save to device"): the original into the iPhone's photo library, once.
- The phone keeps nothing else: the listing is cached, renditions are a cache.

## 3. Faces on the server, Google Photos style

"Gemma embed": EmbeddingGemma is a *text* embedding model (308M parameters), so it cannot compare faces. Gemma 3's
vision encoder (SigLIP) gives image embeddings that describe what is in a picture (a beach, a dog), not *who*: two
photos of the same person at different ages or angles are not close in it. So:

- **Faces: SCRFD (detector) + ArcFace R50 (recognition, 512-d), the InsightFace `buffalo_l` pair**, run in Rust with
  ONNX Runtime (`ort`) on nezopt, niced, CPU (the GPU is Qwen's; no lease needed for about 40 ms per photo).
  ArcFace is the accuracy reference for face recognition. Licence: the pretrained InsightFace models are for
  non-commercial research use, which fits a private home library; if that ever changes, YuNet + SFace (OpenCV Zoo,
  Apache-2.0) is the drop-in with lower accuracy.
- **"Gemma embed" where it fits: search by meaning.** A Gemma/SigLIP image embedding per photo plus EmbeddingGemma
  for the query gives "photos of the sea at sunset" or "the receipt from IKEA" search. Phase 2, after faces.

How it works:

1. Every new item (upload, import, share) queues a face scan; the backlog is scanned in the background (about 6,000
   photos: under an hour).
2. Faces are clustered (cosine distance, agglomerative with a threshold, then incremental: a new face joins the
   nearest person within the threshold, or stays unassigned until there are enough to form one).
3. The Gallery gets **People**: a row of faces; naming one ("Mario") names the cluster; merge two, remove a wrong
   face, hide a person. Search and Lucina understand names ("le foto con Mario a Roma").
4. A new version of a photo (an edit) is scanned again; trashing removes its faces.

Formal side: a Stateright model + TLA+ twin of the scan queue and the naming (each version scanned once, a crash
mid-scan rescans, merging and renaming never lose a face or name one face twice), and a Verus kernel for the
assignment decision (threshold, ties).

## 4. Gallery and chat, like WhatsApp media

Chat photos are library items already (shares and chat attachments go through `/internal/v1/ingest`). To add:

- In the chat, tapping a photo opens the Gallery's viewer (zoom, swipe through the chat's photos, swipe down).
- In the viewer, a photo that came from the chat has **Show in chat**: it closes the viewer and scrolls the chat to
  that message.
- From the Gallery or the Drive, **Send to chat** puts the item in the composer as an attachment (a reference by id:
  nothing is uploaded again), so Lucina sees it.
- The chat gets **Media** (like WhatsApp's "Media, links and docs"): a grid of everything shared in the chat.

## 5. Albums with shareable links

- An album gets **Share link**: a random 128-bit token, `https://getsecretary.tech/a/<token>`. The link opens a
  small read-only web page with the album's photos and videos (renditions; originals only if the owner allows
  downloads for that link). Optional expiry; revoke any time from the app (the link then answers 410).
- p1's Caddy passes `/a/*` straight to lucina-media's share route (like the play tokens). That route serves only the
  album the token names, checked on every request, with GPS stripped from what it serves, no search, no listing,
  `noindex`, and a rate limit.
- Shared albums are listed in the app with their links, views and expiry.
- Formal side: a Verus kernel of the token check (only that album's items, revoked or expired → nothing) and a
  model case for revoke during a download.

## Order and size

1. Import (this week: copy done, import after the owner's answer) — small.
2. Share links — medium; it opens a public route, so it goes through review and the gate first.
3. Gallery ⇄ chat — medium (iOS and lucina-rs).
4. Keep offline — medium (iOS).
5. Faces — large (server worker, models, People UI, Lucina tool), then semantic search.

## Questions for the owner

1. Old phone photos (Android DCIM camera, the iCloud backup, the Fujifilm): in the Gallery timeline, as above, or in
   the Drive?
2. Share links: expire by default (e.g. 30 days) or never? Allow downloading originals by default?
