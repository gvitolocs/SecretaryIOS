# NClient on the web

NClient (the former "Reader") is the owner's private reader for nhentai, served by `secretary-nclient`
(see `docs/nclient.md`). Code: `web/src/views/NClient.tsx`, `components/NClientReader.tsx`,
`components/NClientSave.tsx`, `api/nclient.ts`, `styles/nclient.css`, `i18n/dict/nclient.ts` (en/it/da),
and the mock `web/mock/nclient.mjs`.

## Where it is

- Route `/nclient` (lazy). `/reader` redirects to it.
- Plugins page: an `adult: true` card (`id: "nclient"`). It is hidden while "Show adult plugins" is off on this
  device (`lib/adult.ts`). A direct visit with the switch off shows a notice with a button to turn it on, and
  fetches nothing. The Mac shell hides its rail entry through `secretaryShell.setAdultPlugins`.
- `?shell=1` (Secretary Code): the page scrolls inside the shell's content area and fits 390 px with no
  horizontal scroll; the "Plugins" link and big title are dropped like the other plugins (`styles/embedded.css`).

## What it does

- **Search**: query, five sorts, tag filters (type to find tags, + include / − exclude, chips to remove), result
  paging. Cards show a favourite badge and a reading-progress bar.
- **Detail**: cover, title, tags, Read / Resume, favourite, "Save to Lucina", page thumbnails (24 at a time, a
  thumbnail opens the reader on that page), related galleries. Random and Favorites tabs.
- **Reader**: swipe left/right (touch, only while not zoomed), arrow keys / Page Up / Page Down / Home / End,
  zoom by pinch (touch), Ctrl + wheel (trackpad pinch), the `+` `-` keys or the zoom controls (50 to 400 %), fit
  height / width / original, a page box, Escape to close. After the current page has loaded, the next three
  pages are fetched ahead (never earlier, so they cannot queue in front of the page being read on the service's
  one-image-per-second line). Favourite and Save are in the reader's bar.
- **Progress**: zero-based on the wire; every page change is written (serialized per gallery, with retry); the
  detail gives the resume position.
- **Save to Lucina**: `POST /v1/nclient/galleries/{id}/save {target:"gallery"}` starts a background job on the
  service (every page into Lucina's Gallery, source `nclient`, album = the gallery title). The page shows
  "Saving n of N" with a progress bar and a Stop button (`GET`/`DELETE /v1/nclient/saves/{job}`, polled every
  1.2 s while the page is open; the job keeps running on the server if the page is closed). The result says how
  many pages were saved, how many were already in the Gallery and how many failed. A gallery the safety filter
  blocks answers 404 ("not available"). There is no Library target: the Library reads Kindle formats only.

## Privacy and filtering

- The document title is always `NClient` while the plugin is mounted.
- Searches, filters, gallery ids and page numbers remain in memory. The URL stays `/nclient`; nothing is pushed
  to history.
- Every image, including preloads, uses `referrerpolicy="no-referrer"` and accepts only signed
  `/v1/nclient/s/...` paths returned by the service (`nclientImageUrl`). No upstream links, fallback hosts or
  client-built URLs.
- Nothing is written to localStorage, sessionStorage, IndexedDB or CacheStorage (the shared service worker
  already excludes `/v1/`). The only key near it is the app-wide "Show adult plugins" switch.
- The service owns the child-safety filter. The web app has no bypass or override.

## Local validation

```sh
npm ci --prefix packages/lucina-stage && npm run build --prefix packages/lucina-stage   # once
npm ci --prefix web
npm run typecheck --prefix web
npm test --prefix web
npm run build --prefix web
# Look at it with neutral placeholder data (generated SVG pictures, no real content):
cd web && MOCK_PORT=18861 node mock/server.mjs &     # sign in with the token "dev"
MOCK_PORT=18861 npx vite --port 18862 --host 127.0.0.1   # open /nclient
```

The mock (`POST /v1/_mock/nclient {enabled, media:"down", slow}`) can switch the plugin off, make a save fail
as if lucina-media were down, or slow it down.

Before release, check through p1 with a real owner session: signed image loading and expiry, cross-device
progress and favourites, touch gestures and pinch on a physical phone, and one real "Save to Lucina".
