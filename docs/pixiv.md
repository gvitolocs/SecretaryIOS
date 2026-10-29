# Pixiv plugin

The owner's Pixiv browser, as a plugin (Plugins → Pixiv) on the web app and, through it, the Secretary Code
shell and the phones. The service uses the public web-ajax endpoints of the `pixiv-rust` core, anonymously by
default; an optional pixiv.net session (below) unlocks R-18 and, with Premium, pixiv's popular order.

- `crates/secretary-pixiv`: the service (standalone crate with its own `[workspace]` and `Cargo.lock`, because
  its path dependency `pixiv` exists only on nezopt: `/home/nez/Projects/ReverseEngineering/pixiv-rust`, library
  target `pixiv::api`). Port 8872.
- `deploy/secretary-pixiv/`: systemd unit, env template, p1 Caddy snippet.

## HTTP (`/v1/pixiv`, p1 forward_auth `/v1/auth/owner` + `X-Lucina-Gateway`)

Answers are `{ok:true, data}` or `{ok:false, error}`. Image fields are always signed `/v1/pixiv/s/...` URLs.

| Route | |
|---|---|
| `GET /status` | `{enabled, account:{session:"none"\|"ok"\|"expired", premium, r18:true\|false\|null}, cacheBytes, cacheMaxBytes, apiIntervalMs}` |
| `GET /ranking?mode=&page=&adult=` | modes `day week month day_male day_female original rookie`; `day_r18 week_r18 day_male_r18 day_female_r18` need `adult=1` and a session. Listing |
| `GET /search?q=&order=date_d\|date\|popular&mode=safe\|all\|r18&type=all\|illust\|manga\|ugoira&page=&adult=` | Listing. `mode` is the content filter (default `safe`); `r18` needs `adult=1`; without it `all` is asked as `safe` |
| `GET /illust/{id}?adult=` | `{id,title,author,authorId,created,kind,ugoira,r18,description,stats,pageCount,coverUrl,tags[{tag,en}],pages[{index,thumbUrl,url,originalUrl}],bookmarked}` |
| `GET /bookmarks?content=safe\|all\|r18&adult=`, `PUT/DELETE /bookmarks/{id}` | local (0600 file); `content` defaults to `all` with `adult=1`, else `safe` |
| `POST /save {id, pages?:[zero-based index], adult?}` | `{saved:[{page,itemId,duplicate}], failed:[{page,error}]}` |
| `GET /s/{exp}/{token}/{sig}` | image (signed, 6 h, GET/HEAD, no header needed) |

Listing: `{items, page, hasMore, total, numPages, notice, sort}`. Item: `{id,title,author,authorId,tags,thumbUrl,
pages,r18,popularTier,bookmarked}` (`addedAt` in bookmarks; bookmarks answer `{items}` only).

- `popularTier`: the largest N of the item's `<N>users入り` tags (pixiv's bookmark tiers, e.g. `VOCALOID1000users入り`
  → 1000), `null` without one. Search items carry no bookmark count; this is the only popularity signal.
- `sort` (search only, else `null`): `{requested, used}`. `used` is `date_d`/`date` as asked, or for `order=popular`:
  - `pixiv_popular`: the session is Premium, pixiv's `popular_d` order;
  - `users_tags`: otherwise. First the tagged works by tier: the words AND `(100000users入り OR ... OR 100users入り)`,
    pixiv's `date_d` order, upstream pages 1..3 merged, sorted by `popularTier` then id (both descending), in local
    pages 1..k of 60 (k = ceil(tier works / 60)). Then the rest newest first: page p > k is the plain `date_d` search's
    page p − k without the works already shown, and its `hasMore` is that page's. The last tier page has
    `hasMore: true` when the plain query has any works. `total` is the plain query's total (the tier query's until the
    plain one has been read), `numPages` is k + the plain query's pages. The tagged part is "popular among the
    newest", not pixiv's all-time popularity;
  - `date_d_fallback`: no work carried a tier tag, the plain `date_d` search (pages numbered from 1) instead.
  Empty answers with an R-18 `notice` have `sort: null`.
- `notice` (`null` normally):
  - `r18_needs_session`: R-18 asked (search `mode=r18`, an `_r18` ranking) with no session or an expired one. Empty
    listing (`hasMore false, total 0, numPages 0`), no upstream request.
  - `r18_disabled_in_account`: same, the session's account has R-18 display off (or, when pixiv did not say, R-18 was
    asked and the upstream page had only all-ages works).
  - `all_ages_only`: search `mode=all` without a usable session: the list is real, pixiv just leaves R-18 out.

Errors: 401 `unauthorized` (gateway header), 403 `adult_off` (R-18 mode/work without `adult=1`), 403 `forbidden`
(signature, expiry or host), 400 `bad_request`, 404 `not_found`, 429 `rate_limited` + `retryAfter` + `Retry-After`,
502 `upstream_unavailable`.

## Session (optional)

`PIXIV_SESSION_FILE`: a 0600 file holding only the `PHPSESSID` cookie value of a logged-in pixiv.net browser
(DevTools → Application → Cookies → `https://www.pixiv.net` → `PHPSESSID`). Missing variable, file or empty file:
anonymous. The core sends it as `Cookie: PHPSESSID=…` to `https://www.pixiv.net` only (ajax, ranking.php, touch),
never to i.pximg.net; it is never logged (the start log says `session=configured|none`). The account
(`/touch/ajax/user/self/status`: `is_logged_in`, `is_premium`, `x_restrict`) is checked through the same gate and kept
30 min (a failed check is not kept and reads as `{session ok, premium false, r18 null}`). JSON cache keys carry the
account (`anon`, `user-p<0|1>-r<0|1|u>`), so anonymous answers are never served to a session or the other way round.

**R-18 needs a session.** pixiv shows logged-out visitors only all-ages works: search ignores `mode` (`r18`, `all` and
`safe` all answer the same all-ages works), `ranking.php` answers 403 for the `_r18` modes, and `order=popular_d` is
ignored (Premium only). The account also has to show R-18: pixiv.net → Settings → Viewing restrictions / Display,
"Explicit content (R-18)" = Show (<https://www.pixiv.net/settings/viewing>). A session expires when pixiv logs the
browser out: `/status.account.session` turns `expired`; put a fresh `PHPSESSID` in the file and restart.

## Rules

- **Adult gate** is in the service: R-18 ranking/search modes, R-18 works (detail, bookmark, save) answer 403
  `adult_off`; R-18 items never appear in lists without `adult=1`. One content filter (`safe` drops R-18, `r18` keeps
  only R-18, `all` keeps both) runs on search, ranking and bookmarks; without `adult=1`, `all` is `safe`. The web sends `adult=1` only while
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
