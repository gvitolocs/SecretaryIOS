# Pixiv plugin

The owner's Pixiv browser, as a plugin (Plugins → Pixiv) on the web app and, through it, the Secretary Code
shell and the phones. The service uses the public web-ajax endpoints of the `pixiv-rust` core anonymously.
Connecting a Pixiv account uses OAuth PKCE and the App API, unlocking R-18 (when enabled on the account),
Premium popular order, and a popular preview for accounts without Premium.

- `crates/secretary-pixiv`: the service (standalone crate with its own `[workspace]` and `Cargo.lock`, because
  its path dependency `pixiv` exists only on nezopt: `/home/nez/Projects/ReverseEngineering/pixiv-rust`, library
  target `pixiv::api`). Port 8872.
- `deploy/secretary-pixiv/`: systemd unit, env template, p1 Caddy snippet.

## HTTP (`/v1/pixiv`, p1 forward_auth `/v1/auth/owner` + `X-Lucina-Gateway`)

Answers are `{ok:true, data}` or `{ok:false, error}`. Image fields are always signed `/v1/pixiv/s/...` URLs.

| Route | |
|---|---|
| `GET /status` | `{enabled, account:{session:"none"\|"ok"\|"expired", premium, r18:true\|false\|null}, cacheBytes, cacheMaxBytes, apiIntervalMs, connection, settings}` |
| `POST /connect/start` | `{loginUrl, state, expiresInSecs:600}`; 503 `broker_unconfigured`, 409 `busy` during save/removal approval |
| `POST /connect/finish {state, code}` | `connection`; both fields 1–512 URL-safe characters; 400 `bad_request`/`connect_expired`, 502 `pixiv_login_failed` |
| `POST /disconnect` | `connection`, token dropped immediately; secret deletion awaits approval |
| `GET /settings`, `PUT /settings {defaultContent?, defaultSort?}` | `settings`; PUT merges fields, 400 `bad_request` for invalid fields/values |
| `GET /ranking?mode=&page=&adult=` | modes `day week month day_male day_female original rookie`; `day_r18 week_r18 day_male_r18 day_female_r18` need `adult=1` and a session. Listing |
| `GET /search?q=&order=date_d\|date\|popular&mode=safe\|all\|r18&type=all\|illust\|manga\|ugoira&page=&adult=` | Listing. `mode` is the content filter (default `safe`); `r18` needs `adult=1`; without it `all` is asked as `safe` |
| `GET /illust/{id}?adult=` | `{id,title,author,authorId,created,kind,ugoira,r18,description,stats,pageCount,coverUrl,tags[{tag,en}],pages[{index,thumbUrl,url,originalUrl}],bookmarked}` |
| `GET /bookmarks?content=safe\|all\|r18&adult=`, `PUT/DELETE /bookmarks/{id}` | local (0600 file); `content` defaults to `all` with `adult=1`, else `safe` |
| `POST /save {id, pages?:[zero-based index], adult?}` | `{saved:[{page,itemId,duplicate}], failed:[{page,error}]}` |
| `GET /s/{exp}/{token}/{sig}` | image (signed, 6 h, GET/HEAD, no header needed) |

Listing: `{items, page, hasMore, total, numPages, notice, sort}`. Item: `{id,title,author,authorId,tags,thumbUrl,
pages,r18,popularTier,bookmarked}` (`addedAt` in bookmarks; bookmarks answer `{items}` only).

- `popularTier`: the largest N of the item's `<N>users入り` tags (pixiv's bookmark tiers, e.g. `VOCALOID1000users入り`
  → 1000), `null` without one. The listing retains this tag signal for both backends; App API responses also provide bookmark counts internally.
- `sort` (search only, else `null`): `{requested, used}`. `used` is `date_d`/`date` as asked, or for `order=popular`:
  - `pixiv_popular`: the connected account is Premium, App API `popular_desc` order;
  - `pixiv_popular_preview`: connected without Premium. Page 1 preserves the App API popular preview's order;
    page p ≥ 2 is plain `date_desc` page p−1 with the preview's ids removed. Page 1 has more when the plain
    search has any work. App search has no totals: `total` and `numPages` are null;
  - `users_tags`: anonymous. First the tagged works by tier: the words AND `(100000users入り OR ... OR 100users入り)`,
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

## Pixiv account (Settings → Plugins → Pixiv)

Choose Connect in the app. The service creates a PKCE verifier (32 random bytes, S256) and a login state,
kept in memory for ten minutes (at most five pending logins). The app opens Pixiv's own login page:
on iOS an ephemeral `ASWebAuthenticationSession` with callback scheme `pixiv`; on Mac the Secretary Code
`window.secretaryShell.pixivLogin(loginUrl)` bridge uses an in-memory browser window. Plain browsers
cannot catch the redirect: connect from the Secretary app on iPhone or Mac. The app sends the returned
code and state to `/connect/finish` immediately; no agent sees a password or signs in for the owner.

The service exchanges the code at Pixiv's OAuth endpoint and becomes connected immediately. The refresh
token is saved through infisical-brokerd after the owner approves with Face ID on his iPhone. Until then,
`store` is `waiting_approval`; approval changes it to `saved`. Denial, expiry, an unreachable approval
(code fallback), or a save error drops the in-memory tokens and disconnects. Disconnect drops the tokens
immediately, then requests approval to delete the stored secret. A denied deletion leaves the secret
stored but this run stays disconnected. A new connect is busy while save/removal awaits approval.

Broker caller **`svc-secretary-pixiv`**, class **services**, bind_unit **`secretary-pixiv.service`** needs
`exec`, `list`, and `mutate` grants on exactly **`secret://lucina/prod#PIXIV_REFRESH_TOKEN`**. No broader grants.
Configuration (also in the env example):

| Variable | Default | Purpose |
|---|---|---|
| `PIXIV_BROKER_SOCKET` | `/run/infisical-broker/broker.sock` | broker Unix socket |
| `PIXIV_BROKER_TOKEN_FILE` | unset | private caller token file, read when needed; unset disables Connect |
| `PIXIV_TOKEN_REF` | `secret://lucina/prod#PIXIV_REFRESH_TOKEN` | exact secret reference |

On startup the service reads the broker without blocking HTTP startup, refreshes a stored token, and
connects with `store: saved`. Missing secret means disconnected. Broker unavailability means unavailable
and is retried every 60 seconds. A transient Pixiv refresh failure keeps the stored refresh token in
memory and retries on first use. Access tokens refresh when less than 60 seconds remain; rejected refresh
tokens change the state to expired. Account changes take effect immediately in lists and `/status`.
Anonymous and account JSON cache keys remain separated by account fingerprint.

`/status` includes these objects (connect/finish and disconnect return the `connection` object directly):

```json
{
  "connection": {
    "state": "connected",
    "user": {"id": "123", "name": "Owner", "account": "handle"},
    "premium": false, "r18": true, "r18g": false,
    "store": "saved", "removal": null, "error": null,
    "connectedAt": "2026-10-29T15:40:00Z"
  },
  "settings": {"defaultContent": "safe", "defaultSort": "date_d"}
}
```

Connection states: `disconnected`, `connected`, `expired`, `unavailable`. `user`, `premium`, `r18`, and
`r18g` are null without an in-memory connected token; `r18` means `x_restrict >= 1`, `r18g` means >= 2.
`connectedAt` is an RFC3339 timestamp or null. `store` is `saved`, `waiting_approval`, or null;
`removal` is `waiting_approval` or null. `error` is null or one of `approval_denied`, `approval_expired`,
`approval_unavailable`, `save_failed`, `remove_denied`, `remove_failed`, `token_rejected`,
`broker_unavailable`, `broker_unconfigured`. A token rejection fails the upstream call with the normal 502;
subsequent R-18 lists show `r18_needs_session`. `/status.account` remains `{session, premium, r18}` with
session `none`, `ok` (connected), or `expired`.

Settings defaults are `safe` and `date_d`. `defaultContent` allows `safe`, `all`, `r18`; `defaultSort`
allows `date_d`, `popular`. PUT merges only supplied fields and atomically writes `settings.json` (0600)
next to the bookmarks. These are app preferences; the request-level adult gate still applies.

**R-18 needs a connected account with R-18 viewing enabled.** This viewing setting can only be changed
on [pixiv.net → Settings → Viewing](https://www.pixiv.net/settings/viewing), “Explicit content (R-18)” → Show.
The App API cannot change it. App searches for R-18 append the `R-18` tag; all lists still use the local
content filter. Images retain Pixiv's original i.pximg.net URLs internally and are signed by the service.

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
- **Tokens:** access/refresh tokens stay in memory (refresh tokens also stored through the broker), have redacted
  Debug/Display, and are zeroized on drop. Tokens never appear in logs, HTTP answers, or errors. Refresh tokens
  go only to Pixiv's OAuth endpoint and the broker; Bearer access tokens only to the App API. Image requests
  carry no token. Logs carry method, route template and status only, never connection request bodies or codes.
- Ugoira is shown as its cover.

## Deploy

```sh
cd crates/secretary-pixiv && cargo test && cargo build --release
install -m 755 target/release/secretary-pixiv ~/.local/bin/
mkdir -p ~/.config/secretary-pixiv && cp ../../deploy/secretary-pixiv/pixiv.env.example ~/.config/secretary-pixiv/pixiv.env
head -c 32 /dev/urandom | base64 > ~/.config/secretary-pixiv/gateway.secret && chmod 600 ~/.config/secretary-pixiv/*
cp ../../deploy/secretary-pixiv/secretary-pixiv.service ~/.config/systemd/user/ && systemctl --user daemon-reload && systemctl --user enable --now secretary-pixiv
# p1: put the same secret in /etc/caddy/pixiv-gateway.key, add deploy/secretary-pixiv/Caddyfile.snippet, caddy reload
```
