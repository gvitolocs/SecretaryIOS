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
| `GET /s/{id}/{kind}/{index}/{exp}/{sig}` | image proxy (signed, 6 h; no header needed) |

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
