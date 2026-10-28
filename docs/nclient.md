# Reader plugin (nclient)

The owner's private reader for nhentai, as a plugin (Plugins → Reader) on iOS and the web. Code:

- `crates/nclient-core`: the Rust port of the NClientV3 portable core (Apache-2.0). Pure: URL
  building and JSON parsing, no I/O. JVM golden parity tests (`tests/parity.rs`,
  `java-goldens/`) and Verus proofs of its invariants (`verus/verify_core.rs`).
- `crates/secretary-nclient`: the service. The HTTP the core leaves to the caller, the
  child-safety filter, the image proxy with its disk cache, favorites and reading progress.
- `deploy/secretary-nclient/`: systemd unit, env template, p1 Caddy snippet.

## Hard rule: no content that sexualises minors

`crates/secretary-nclient/src/safety.rs`. It can't be configured: there's no switch and no env
var. Every gallery goes through `safety::vet` on the server, after parsing and before
anything reaches the cache or a client:

- **Tags.** Any tag matching the hard list blocks the gallery: lolicon, shotacon,
  toddlercon, loli, shota (with their "low …" and spelling variants), the age-related
  "young", "child", "kid" and "minor" tags, and the school-age and age-regression tags.
- **Uniform and child context.** A school-uniform tag (schoolgirl or schoolboy uniform,
  school swimsuit and so on) together with a child-context tag (flat chest, petite and
  so on) blocks the gallery.
- **Titles.** Titles with these terms block the gallery, in English, Japanese and Chinese.
- **Missing tag data.** A gallery with no tags, or with a tag id that can't be resolved,
  is dropped. The v2 lists carry only `tag_ids`, so an unknown id is looked up through
  that gallery's detail (at most 10 per page, 1 per second). Anything still unknown is
  dropped.
- **Blocked searches.** A search for a blocked term answers empty without contacting the
  site. Every search also sends `-tag:"lolicon" -tag:"shotacon" …` upstream, as defence in
  depth.
- **Not found.** A blocked gallery answers 404, the same as a missing one. Only an id-only
  marker is kept for it. Its images are never fetched, and the proxy re-vets the gallery
  on every image request, cached or not.
- **Re-vetting.** Cached pages, favorites and related lists are vetted again on every
  read. If the blocklist grows, a newly blocked gallery is purged from the cache and
  from the owner's favorites and progress.

To change the lists, add entries and never remove them. Every entry is pinned by
`safety::tests`, and `tests/service.rs` checks that no blocked title or URL reaches the
cache.

## Privacy

- **Opt-in and locked.** The plugin is off until the owner turns it on in Settings, and it
  opens only after Face ID or the passcode.
- **Hidden from the system.** It's never in widgets, notifications, Spotlight, Siri
  suggestions or Handoff. The app-switcher snapshot is blurred.
- **Kept out of Lucina.** Lucina's proactive messages and memory never see it, because the
  service has no link to lucina-rs, lucina-proactive or the timeline.
- **Not shared.** Favorites and progress are kept for one owner (`NCLIENT_OWNER`), in a
  0600 file. They're never shared with other users.
- **Logs.** Logs carry only the method, route template and status. They never carry
  queries, ids, titles or tags.

## HTTP (`/v1/nclient`, behind p1's forward_auth + `X-Lucina-Gateway`)

| Route | |
|---|---|
| `GET /status` | `{enabled}` |
| `GET /search?q=&page=&sort=recent\|popular\|popular-week\|popular-today\|popular-month&tags=1,2&exclude=3` | `{items, page, numPages}` |
| `GET /random` | a gallery detail |
| `GET /galleries/{id}` | `{id, title, titles, tags, language, uploaded, favorites, coverUrl, thumbUrl, pages[{index,width,height,url,thumbUrl}], related, favorite, progress}` |
| `GET /galleries/{id}/related` | `{items}` |
| `GET /tags?type=tag&q=&limit=` | known tags, most used first (blocked ones never listed) |
| `GET /favorites`, `PUT/DELETE /favorites/{id}` | |
| `GET /progress`, `PUT /progress/{id} {page, pages}` | |
| `POST /galleries/{id}/save {target:"gallery"}` | Save to Lucina: `202 {job, state, total, done, saved, failed}` (see below). `target:"library"` answers `400 library_not_supported`. A blocked or missing gallery answers 404 and nothing is fetched or ingested. |
| `GET /saves/{job}` | progress: `{job, state: queued\|running\|done\|failed\|cancelled, total, done, saved:[{index,itemId,duplicate}], failed:[{index,error}], error?}` |
| `DELETE /saves/{job}` | stop after the current page |
| `GET /s/{id}/{kind}/{index}/{exp}/{sig}` | image proxy (signed, 6 h; no header needed) |

Errors answer `{ok:false, error}`. When the site rate-limits us the answer is `429 {error:"rate_limited", retryAfter}`
with a `Retry-After` header.

## Save to Lucina

`POST /galleries/{id}/save` re-vets the gallery (`safety::vet`; blocked → 404), then starts a background job
that fetches every page through the same spaced image fetcher and cache as the reader (one image per second at
most; a 200-page gallery takes a few minutes), writes each page to `$NCLIENT_SPOOL_DIR` (0600) and hands it to
lucina-media's loopback `POST /internal/v1/ingest` with `{path, name: "001.jpg", mime, source: "nclient",
album: <gallery title>, move: true}`. Duplicates are recognised by lucina-media (`duplicate: true`). The job
vets the gallery again before every page, so a gallery blocked meanwhile stops it. Jobs run one at a time (up to
8 queued); saving a gallery that is already being saved returns the running job. The job id is random and the
logs carry no ids, titles or page numbers. If lucina-media is unreachable for three pages in a row the job fails
with `media_unavailable`.

**Library: not supported.** The Library (`ink-library`) reads Kindle formats only (azw3, azw, mobi, kfx, prc,
pobi, pdf, epub, txt, md); it has no CBZ or comic format. Pages go to the Gallery only.

## Upstream limits and the cache

Requests to the site are spaced (`NCLIENT_API_INTERVAL_MS` / `NCLIENT_IMAGE_INTERVAL_MS`, at least 1000). A 429 or
5xx is retried twice with exponential backoff (2 s, 4 s, with jitter), and a `Retry-After` is honoured; one longer
than 20 s is not waited for and goes to the client as `rate_limited`. Everything behind a 429 waits for what the
site asked. The cache directory is capped by `NCLIENT_CACHE_MAX_BYTES` (default 2 GiB); the oldest files are
removed first, down to 90 % of the cap. The service stops cleanly on SIGTERM.

## Deploy (nezopt + p1)

```sh
cargo test -p nclient-core -p secretary-nclient          # parity + filter + service tests
crates/nclient-core/run_all.sh                           # goldens on the JVM, parity, Verus
cargo build --release -p secretary-nclient && install -m 755 target/release/secretary-nclient ~/.local/bin/
mkdir -p ~/.config/secretary-nclient && cp deploy/secretary-nclient/nclient.env.example ~/.config/secretary-nclient/nclient.env
head -c 32 /dev/urandom | base64 > ~/.config/secretary-nclient/gateway.secret && chmod 600 ~/.config/secretary-nclient/*
cp deploy/secretary-nclient/secretary-nclient.service ~/.config/systemd/user/ && systemctl --user daemon-reload && systemctl --user enable --now secretary-nclient
# p1: add deploy/secretary-nclient/Caddyfile.snippet (tailnet IP + NCLIENT_GATEWAY_SECRET in Caddy's env), caddy reload
```
