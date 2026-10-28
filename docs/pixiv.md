# Pixiv plugin

The owner's Pixiv browser, as a plugin (Plugins → Pixiv) on the web app and, through it, the Secretary Code
shell and the phones. No Pixiv account: the service uses the public web-ajax endpoints of the `pixiv-rust` core.

- `crates/secretary-pixiv`: the service (standalone crate with its own `[workspace]` and `Cargo.lock`, because
  its path dependency `pixiv` exists only on nezopt: `/home/nez/Projects/ReverseEngineering/pixiv-rust`, library
  target `pixiv::api`). Port 8872.
- `deploy/secretary-pixiv/`: systemd unit, env template, p1 Caddy snippet.

## HTTP (`/v1/pixiv`, p1 forward_auth `/v1/auth/owner` + `X-Lucina-Gateway`)

Answers are `{ok:true, data}` or `{ok:false, error}`. Image fields are always signed `/v1/pixiv/s/...` URLs.

| Route | |
|---|---|
| `GET /status` | `{enabled, cacheBytes, cacheMaxBytes, apiIntervalMs}` |
| `GET /ranking?mode=&page=&adult=` | modes `day week month day_male day_female original rookie`; `day_r18 week_r18 day_male_r18 day_female_r18` need `adult=1`. `{items, page, hasMore}` |
| `GET /search?q=&order=date_d\|date&mode=safe\|all\|r18&type=all\|illust\|manga\|ugoira&page=&adult=` | `{items, page, hasMore, total, numPages}`. `r18` needs `adult=1`; without it `all` is asked as `safe` |
| `GET /illust/{id}?adult=` | `{id,title,author,authorId,created,kind,ugoira,r18,description,stats,pageCount,coverUrl,tags[{tag,en}],pages[{index,thumbUrl,url,originalUrl}],bookmarked}` |
| `GET /bookmarks`, `PUT/DELETE /bookmarks/{id}` | local (0600 file); R-18 entries hidden without `adult=1` |
| `POST /save {id, pages?:[zero-based index], adult?}` | `{saved:[{page,itemId,duplicate}], failed:[{page,error}]}` |
| `GET /s/{exp}/{token}/{sig}` | image (signed, 6 h, GET/HEAD, no header needed) |

Item shape: `{id,title,author,authorId,tags,thumbUrl,pages,r18,bookmarked}` (`addedAt` in bookmarks).

Errors: 401 `unauthorized` (gateway header), 403 `adult_off` (R-18 mode/work without `adult=1`), 403 `forbidden`
(signature, expiry or host), 400 `bad_request`, 404 `not_found`, 429 `rate_limited` + `retryAfter` + `Retry-After`,
502 `upstream_unavailable`.

## Rules

- **Adult gate** is in the service: R-18 ranking/search modes, R-18 works (detail, bookmark, save) answer 403
  `adult_off`; R-18 items never appear in lists without `adult=1`. The web sends `adult=1` only while
  "Show adult plugins" is on. Lucina's tools never send it.
- **Never an open proxy:** a signature vouches only for `https://i.pximg.net/...` (no port, no userinfo); the service
  adds `Referer: https://www.pixiv.net/`. Thumbnails of other hosts get no URL.
- **Politeness:** pixiv.net is called one request at a time, ≥ 1.5 s apart (`PIXIV_API_INTERVAL_MS`, never below
  1 s), with exponential backoff and jitter on 429/5xx, and a 60 s cool-down after a 429 that outlasted the
  retries. Image requests (i.pximg.net CDN) start ≥ 250 ms apart, 3 at a time, same backoff, `Retry-After` honoured.
- **Disk cache** `PIXIV_CACHE_DIR`, capped by `PIXIV_CACHE_MAX_BYTES` (2 GiB), least recently used out first.
  JSON answers are kept 5 min (works 30 min); they hold original URLs, never signatures.
- **Save:** originals (`img-original`) are downloaded into the spool (0700) and handed to lucina-media's
  `POST /internal/v1/ingest` (`source pixiv`, `album Pixiv`, caption `title — author (pixiv #id)`, `move true`).
- Ugoira is shown as its cover. Logs carry method, route template and status only.

## Deploy

```sh
cd crates/secretary-pixiv && cargo test && cargo build --release
install -m 755 target/release/secretary-pixiv ~/.local/bin/
mkdir -p ~/.config/secretary-pixiv && cp ../../deploy/secretary-pixiv/pixiv.env.example ~/.config/secretary-pixiv/pixiv.env
head -c 32 /dev/urandom | base64 > ~/.config/secretary-pixiv/gateway.secret && chmod 600 ~/.config/secretary-pixiv/*
cp ../../deploy/secretary-pixiv/secretary-pixiv.service ~/.config/systemd/user/ && systemctl --user daemon-reload && systemctl --user enable --now secretary-pixiv
# p1: put the same secret in /etc/caddy/pixiv-gateway.key, add deploy/secretary-pixiv/Caddyfile.snippet, caddy reload
```
