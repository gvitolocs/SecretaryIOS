# Instagram plugin (secretary-instagram)

The owner's anonymous Instagram viewer, as a plugin (Plugins -> Instagram) on the web app and in
Secretary Code. Owner only: p1 admits it through `forward_auth /v1/auth/owner`, so Lucina's
service token is refused. Code: `crates/secretary-instagram` (standalone crate with its own
`[workspace]` and `Cargo.lock`; it depends on `/home/nez/Projects/ReverseEngineering/instarust/core`
by absolute path, which exists only on nezopt, so it is not a member of the root workspace).
Deploy files: `deploy/secretary-instagram/`.

## Upstream

`storyview.yodams.com/api/v1/*`, the anonymous StoryView backend (no account, no token). The URL
builders and parsers are `instarust-core`'s. Observed 2026-10-28:

| Backend | Used for | Notes |
|---|---|---|
| `GET /users/{u}/bundle` | profile | profile + first 12 posts + highlights, one call |
| `GET /users/{u}/posts?cursor=` | posts | **ignores `cursor`**: the same 12 posts come back for any cursor (even `garbage`), while `next_cursor` changes. The service reports a cursor page that is the first page again as the end (`exhausted: true`, `nextCursor: null`). So Posts shows the newest 12; Reels paginate. |
| `GET /users/{u}/reels?cursor=` | reels | **distinct from posts**: video posts only, `taken_at` is null, cursor works (page 2 differs) |
| `GET /users/{u}/stories?cursor=` | stories | all active stories in one page, `next_cursor` null |
| `GET /users/{u}/highlights`, `GET /highlights/items?id=&cursor=` | highlights | `highlight:<id>` ids |

Observed 2026-10-29: the bundle's `highlights` is `[]` even for accounts that have highlights, while
`/users/{u}/highlights` still lists them (natgeo: 0 in the bundle, 1 in the list; `/highlights/items`
gives its 4 items), and the list's `media_count` is 0 for highlights with items. So `GET /profile` asks
`/users/{u}/highlights` when the bundle has none and the account is public (through the cache entry
`GET /highlights/{username}` uses, so the two cost one backend call); if that call fails the profile
comes back with `highlights: []` and a warning in the log. A `mediaCount` of 0 or less is sent as
`null` ("not known"), and the web then shows no count. The recorded answers are in
`crates/secretary-instagram/tests/fixtures/natgeo.*.json`.
| `GET /resolve?url=` | resolve | `MediaPage` wrapper with one item |
| `GET /users/{u}/tagged` | not used | exists (tagged posts, paginates); could become a tab |

Media URLs in every response point to `…workers.dev/cdn?url=<instagram cdn url>&exp=&sig=`. The
service unwraps `url=`, checks the host (`*.cdninstagram.com`, `*.fbcdn.net`, https only, no IPs,
no credentials, no port) and fetches it itself with no Referer.

## Local first (2026-11-01)

The owner: "you should cache and download all the content and just refetch new content". The backend answers in
5 s on average, 30-55 s at worst (timeouts included), one request at a time, so a page must never wait for it when there
is something to show.

- **Store** (`store.rs`, `<cache>/profiles/`, one 0600 JSON per profile or feed, hashed names, never evicted): the
  profile, the tray, and every item seen per feed (posts, reels, stories, each highlight), newest first. `GET /profile`,
  `/posts`, `/reels`, `/stories`, `/highlights`, `/highlights/items` answer from it in milliseconds, with every URL signed
  afresh, plus `refreshing` (the service is asking the backend behind this answer: ask again shortly; the apps poll
  every 3 s with nothing on screen) and `updatedAt`. Only an account never opened waits for the backend, and then only
  for what that route needs. A backend failure leaves the page as it was.
- **Refresh** behind the answer, per kind when past its TTL (bundle and reels 5 min, stories 2 min, tray 10 min, an empty
  tray 1 min), at the warmer's low priority, one per account: reels go page by page **until a known item comes up**
  (at most 5 pages). A refresh that failed waits 60 s. 404 after a known account: `gone: true` and what is kept stays.
- **Pages:** an answer holds 48 items; `nextCursor` is `d<offset>` (the next page of the store) and, past what is known,
  the backend's own cursor. Clients treat it as opaque.
- **Stories** stay after they expire upstream: the ones up now first, then expired ones (`expired: true`, up to 500)
  that have a local copy; one without a copy is left out.
- **Profile pictures are 100 px** (checked again 2026-11-01): `/users/{u}` and `/bundle` give only the `s100x100`
  rendition, its signed CDN URL refuses other sizes (2026-10-30), and Instagram's own `web_profile_info`, the one place
  with `profile_pic_url_hd`, answers an anonymous request 401 `require_login`. The HD picture needs a logged-in source,
  which this anonymous service does not use.
- **Download** (`run_mirror`): when an item is first seen on a **watched** account (owner, 2026-11-01: "at least for the watching creators"), its files (poster,
  media, carousel frames; profile picture and covers) go on a queue and are fetched one at a time, a second apart,
  images before videos, newest first. Other profiles render at once from the store and fetch media only as it is looked at, in the ordinary cache, evicted first. Unwatching stops what is left of an account on the queue. It takes at most one of the two CDN slots and waits while an
  owner request is waiting for a file. Budget: `INSTAGRAM_MIRROR_DAILY_MAX_BYTES` (20 GiB per UTC day). A link the CDN
  refuses is left alone for 6 h. An item keeps the URL whose file is on disk, because a fresh backend URL can name
  another rendition of the same picture (`efg` differed in 11 of 432 repeats, 2026-11-01).
- **Backfill** (`run_backfill`): for watched accounts only, the first fill of one with
  nothing in the store (four calls; a name that does not answer waits 6 h), older reels pages and highlights' items: one
  step every 20 s at low priority, 300 calls per UTC day over all accounts. A page with nothing new ends a feed however
  its cursor goes on. A 429 rests it with the warmer.
- **Where and how much:** `INSTAGRAM_CACHE_DIR=/home/nez/mnt/mybook/secretary-instagram-cache`, cap 200 GiB
  (nezopt's system disk is 96 % full; mybook has 12 TB free). The directory must exist: if it is missing (mybook not
  mounted) the service uses `$INSTAGRAM_DATA_DIR/cache` with the small 2 GiB cap, so nothing lands on the wrong disk.
- **Eviction:** oldest file first, so other profiles go first; never media of watched accounts or of saved items (pinned, recomputed every
  10 min and after each check) and the store. If what is pinned alone is over the cap, a warning says so and the download
  pauses.

**The 403s (2026-11-01):** 116 of about 1800 media requests in a day answered 403, in bursts at the first frame of a
visit (14 at 15:31, 38 at 08:55). `/s/` answers 403 only for a refused signature; a CDN refusal is 410, and there were
none. So these were expired links: the web opens a page on its copy of up to 7 days (`lib/pageCache`) and links have
lived 60-75 minutes since 2026-10-31, so every visit over an hour after the last showed broken tiles until the refresh
landed. Fixed without longer links: `POST /v1/instagram/resign {urls}` (behind the gateway; at most 500) gives new links
for ones the service signed, however old, and nothing else, and clients re-sign an expired link before showing it
(`web/src/lib/instagramImages.ts`, iOS `IgPicture`). A tile that fails is asked again (at once, 3 s, 15 s, then every
minute) with no broken image, "Can't load. Trying again…" from the third failure, and the page asks for newer links.
`POST /save` takes an expired link of ours too, being behind the gateway. Each refusal now logs
`instagram media refused` with its reason.

**Logs:** every request has `ms`. `instagram page` (kind, `source=disk|backend`, ms, refreshing), `instagram backend`
(kind, ms, attempts), `instagram refresh` (ms), `instagram cdn` (type, bytes, ms), `instagram mirror` (files, bytes, left,
today's bytes, every 100 files), `instagram backfill` (failures).

| Measured on the real backend, 2026-11-01 (a throwaway instance, by curl) | Before | After |
|---|---|---|
| First open: profile answer (bundle only now; the tray comes behind it) | 20.1 s (with the tray) | 1.1-30 s (the backend's bundle) |
| Repeat visit past the TTL: profile | 2 ms | 1 ms |
| Repeat visit past the TTL: stories | 848 ms, up to 55 s | 0-64 ms, never waits |
| Repeat visit, backend down: stories, reels past 2 days | error | from the store |
| Repeat visit: 12 tiles, 6 at a time | 153 ms | 65 ms |
| Tiles on a first open of a page copy over an hour old | 403s until the refresh | re-signed first |

## HTTP (`/v1/instagram`, behind p1's forward_auth + `X-Lucina-Gateway`)

Success: `{"ok":true,"data":…}`. Error: `{"ok":false,"error":"<code>"}`; 429 is
`{"ok":false,"error":"rate_limited","retryAfter":<s>}` with `Retry-After`.

| Route | `data` |
|---|---|
| `GET /status` | `{enabled, rateLimitPerSecond}` |
| `GET /profile/{username}` | `{profile, posts:{items,nextCursor}, highlights:[{id,title,coverUrl,mediaCount}]}` (also records the profile in `recent`; `mediaCount` is null when the backend says 0) |
| `GET /posts/{username}?cursor=` | `{items, nextCursor, exhausted?}` |
| `GET /reels/{username}?cursor=` | `{items, nextCursor}` |
| `GET /stories/{username}?cursor=` | `{items, nextCursor}` |
| `GET /highlights/{username}` | `{items:[highlight]}` (a user named `items` is not reachable here) |
| `GET /highlights/items?id=highlight:…&cursor=` | `{items, nextCursor}` |
| `GET /resolve?url=<instagram post/reel url>` | `{items, nextCursor}` |
| `GET /recent` | `{items:[{username, fullName, isVerified, isPrivate, picUrl, openedAt}]}`, newest first, max 30 (`isPrivate`: when last opened; absent on entries from before 2026-11-01) |
| `POST /save {items:[{url, kind, username, takenAt?, caption?}]}` | `{saved:[{index,itemId,duplicate}], failed:[{index,error}]}` |
| `POST /resign {urls:[signed url]}` | `{urls:{old:new}}`, only links this service signed, however old; at most 500 |
| `GET /s/{exp}/{sig}/{token}` | media bytes (signed, 60-75 min, GET/HEAD, **no gateway header**), Range supported |

Media item (camelCase): `{id, kind:"image"|"video"|"carousel", url, thumbUrl, takenAt?, caption?,
likeCount, commentCount, permalink?, children?:[{id, kind, url, thumbUrl}]}`. `url`/`thumbUrl` are our
signed URLs; a carousel's `url` is its first frame and `children` has every frame. No width or
height (the backend doesn't send them). Items whose media URL is not on the allowlist are dropped.

`POST /save`: `url` is the signed URL from a response (the service verifies it; it never takes a
raw Instagram URL), up to 50 items. For each: the media comes from the cache (or is fetched),
is copied into `~/.local/share/secretary-instagram/spool/`, and lucina-media's loopback
`POST /internal/v1/ingest` gets `{path, name, mime, source:"instagram", album:"Instagram",
caption:"@username <caption>", taken_at, move:true}`. Failures: `bad_url`, `forbidden`, `expired`,
`bad_request`, `download_failed`, `rate_limited`, `spool_failed`, `ingest_failed`,
`ingest_unavailable`. A refused ingest removes its spool file.

## Watching (auto-save, badges, change notices, Archive)

`PUT /watch/{user}` puts an account on the watch list. A background **warmer** then refreshes each
watched account about once a day: every 23 h ± 30 min per account by default (a random offset per
refresh, so accounts drift apart; stories last 24 h, hence the cap below 24 h), `INSTAGRAM_WATCH_INTERVAL_H`
(1-23), or the owner's choice in the app (6, 12 or 23 h) via `watch-settings`, which applies to all watched
accounts. A new account is first checked within seconds. Each account's next check is persisted
(`nextDue` in `watch.json`), so restarts don't add checks: when the service starts, and when the setting
changes, an account whose next check is outside `last check + interval ± jitter` is moved to
`last check + interval + a random offset` (one never checked keeps its first check; one pushed out by a 429
keeps its later time). The refreshes go through the same single queue, spacing, backoff and 429 block as the
owner's requests. Owner requests go first: the warmer waits while one is pending. When the backend says
429 (to the warmer or to the owner) the warmer stays quiet for at least `INSTAGRAM_WATCH_PAUSE_SECS`
(30 min) and the account is rescheduled after the pause. A refresh is the profile bundle plus, for a
public account, the stories: two backend calls, both read-only (nothing is ever sent that Instagram
could count as "seen"), then what else is out of date (reels down to the first known one, the tray). All of
it goes into the store and its media on the download queue, so the owner's next visit is instant.

**Auto-save.** Each refresh saves the stories that are up and not saved yet (images and videos, the
original file through our CDN fetcher) into the Gallery through lucina-media's `POST /internal/v1/ingest`:
source `instagram`, album `Instagram · @user`, caption `@user …`, `taken_at` from the story, `move: true`.
Dedupe is by the Instagram story id: the account keeps the ids it saved (the last 5000, in `watch.json`),
so nothing is saved twice even after a restart. lucina-media also dedupes by content hash (`duplicate:
true`). Its `PUT /internal/v1/external/{source}/{ext_id}` registers a file in place (it does not move
it) and the upload `asset_id` belongs to phone devices, so neither fits a moved ingest; the saved-ids
store is the dedupe key. A story whose save fails (lucina-media down, a refused ingest) stays unsaved
and is retried at the next refresh while it is still up (stories last 24 h). The per-account switch
(`autoSave`, default on) and a daily size cap (`INSTAGRAM_WATCH_DAILY_MAX_BYTES`, default 500 MiB per
account per UTC day; a story that does not fit is skipped) limit it. The first refresh saves the stories
that are up at that moment.

**Badges.** The first refresh is a baseline (what is there is not "new"). After it, a story or post id not
seen before is *unseen* until the owner opens the account: `POST /watch/{user}/opened` clears it. A story that
expired is no longer unseen. `GET /badges` -> `{total, accounts:{user:n}}` (n = unseen stories + unseen
posts; only accounts with n > 0). The web shows a dot on the account's chip and on the Plugins card and calls
`window.secretaryShell?.setBadge?.("instagram", total)` (the Mac rail).

**Change notices.** A refresh that sees the account turn private or public, the user id behind the name
change, the name answer 404 after it had answered, or come back, queues one notice per detected change; it
is pushed to the owner through secretary-server (`POST /v1/notify`, category `lucina_message`, route
`/instagram/<user>`, with Lucina's `svc_…` service token read from `INSTAGRAM_NOTIFY_TOKEN_FILE`, the way
lucina-meshy does it) and shows up as an iPhone push and in the app's inbox. A notice has its own
`dedupKey` (`instagram:<user>:<change>:<time>`): a delivery that failed is retried at the next pass, and
secretary-server drops a repeat, so one change notifies once. Not configured: the change is logged and
dropped. The backend has no lookup by user id, so a renamed account shows as "gone" (404), not as a rename;
a name taken over by another account shows as "now belongs to a different account" (the user id changed).

| Route | `data` |
|---|---|
| `GET /watch` | `[{username, autoSave, lastCheck, nextCheck, newStories, newPosts, private, fullName, picUrl, isVerified, gone, status, archived}]` (`lastCheck`, `nextCheck`: unix seconds) |
| `GET /watch-settings` | `{intervalHours, defaultHours, jitterMinutes, choices:[6,12,23]}` (`intervalHours`: the owner's choice, else `defaultHours`) |
| `PUT /watch-settings {intervalHours}` | the same; 1-23, else 400 `bad_request`. Saved in `watch.json`; every account's next check is moved to the new interval. (Not `/watch/settings`: that is the account `settings`.) |
| `PUT /watch/{user} {autoSave?}` | the entry (creates it; an empty body keeps the switch) |
| `DELETE /watch/{user}` | `{removed:true}` (the archive index stays) |
| `GET /watch/{user}/new` | `{username, since, stories:[item], posts:[item]}`: what is unseen, as media items |
| `POST /watch/{user}/opened` | the badges after clearing this account |
| `GET /badges` | `{total, accounts}` |
| `GET /archive/{user}?cursor=` | `{items:[{id, mediaId, kind, url, thumbUrl, takenAt, savedAt, caption, auto}], nextCursor}`, newest saved first, 30 a page, cursor = offset |
| `GET /a/{exp}/{sig}/{mediaId}` | a saved item's file (signed like `/s/*`, no gateway header, Range supported) |

The Archive lists what lucina-media took (auto-saves and the owner's own Save to Lucina taps for that
account). The file is read from where lucina-media keeps it (its loopback `GET /internal/v1/items/{id}/path`),
so the Archive shows the original even after the CDN link is gone; `thumbUrl` is the Gallery's own 256 px
thumbnail (`/v1/media/items/{id}/thumb?size=256`, the owner's session).

## Safety and limits

- Every route but `/s/*` needs `X-Lucina-Gateway` (constant-time compare). `/s/*` verifies an
  HMAC-SHA256 over `exp/token` (key in the data dir, 0600) and re-checks the host allowlist even
  for a valid signature. Never an open proxy; redirects are not followed.
- **Backend spacing:** one request at a time, at least `INSTAGRAM_API_INTERVAL_MS` (default and
  floor 1000) apart. 429 and 5xx are retried twice with exponential backoff and jitter
  (2 s, 4 s), honouring `Retry-After` up to 30 s. When a 429 survives the retries, or its
  `Retry-After` is longer, the backend is left alone for that long (default 60 s): every request
  in the meantime answers `429 rate_limited` at once.
- **CDN spacing:** the CDN is not the rate-limited API, and a profile page loads 15+ pictures, so
  media fetches run two at a time, 250 ms apart (`INSTAGRAM_MEDIA_INTERVAL_MS`). This differs from
  the "one request, 1 s" rule for the backend on purpose. Concurrent requests for one item (a
  browser's Range requests on a video) share one download.
- **Cache:** `INSTAGRAM_CACHE_DIR`, capped by `INSTAGRAM_CACHE_MAX_BYTES`
  (see "Local first"; JSON and media together), oldest file first (a hit refreshes a file's age).
  Media is keyed by CDN path plus the `stp`/`efg` rendition parameters, not by the volatile
  `oh`/`oe`, so a fresh backend URL for the same picture hits the cache and an expired link is
  still served from it. JSON bodies live 2-10 minutes. Images up to 32 MiB, videos 256 MiB.
- **Older copies (2026-10-30):** a cold bundle takes the backend 10 s or more (35 s with a retry),
  longer than the web waited (20 s), so the first open of an account showed an error. Now an answer
  past its TTL but at most 2 days old (more than the warmer's 23 h, so a watched or recently opened
  account always has one) is sent at once, and a fresh one is fetched behind it at the warmer's low
  priority; the next open has it. Stories never: yesterday's have expired. Empty highlight trays are
  still never kept. Every backend fetch runs in a task of its own, one per cache key: a request that
  comes while one is under way waits for it, and a client that gives up does not cancel it (its
  retry finds the answer). The web waits 60 s for `/v1/instagram` GETs.
- **Logs:** method, route template, status; and one `instagram backend` line per backend call
  with its kind (`users/bundle`, `highlights/items`, ...), `ms` (the queue included) and `attempts`.
  No usernames, cursors, ids or captions.
- **Shutdown:** SIGTERM finishes requests in flight and exits 0.

## Signed media URLs (2026-10-31)

`/s/*` and `/a/*` skip p1's forward_auth on purpose: `<img>` and `<video>` in the apps (the Mac app loads the web
from its bundle, so it is cross-origin) send no session. They are capabilities instead: whoever has one can fetch
that one item, and nothing else, until it expires.
- **Bound:** `<sig>` is HMAC-SHA256 (key `signing.key`, 0600, never leaves nezopt) over `<exp>/<token>`, where
  `<token>` is the exact Instagram CDN URL (`/a/`: `a/<exp>/<mediaId>`); another item, a later expiry or a media
  signature used as an archive one is refused. A valid signature on a foreign host is still refused.
- **Short:** 60-75 minutes (an hour, rounded up to a quarter hour so image caches see one URL); it was six
  hours. Responses are `private, max-age=3600`, `no-referrer`, `noindex`.
- **What it can reach:** only what the anonymous backend shows (public posts, stories and highlights, and every
  account's profile picture, which Instagram serves publicly); for `/a/`, that saved file from the Gallery.
  Private accounts' posts and stories never pass through the service.
- **Not logged:** the service logs the route template only; p1's Caddy access log replaces everything after
  `/v1/instagram/s/`, `/a/` (and `/v1/media/s/`, `/v1/nclient/s/`) with `REDACTED`.
- **Rotation:** deleting `signing.key` and restarting the service makes a new key and voids every URL handed out
  (done 2026-10-31, after a shared link opened without sign-in). Cached pages fetch fresh URLs when reopened; the
  web asks a cached highlight tray again once per visit for that reason.

## Empty highlights and upstream failures (2026-10-29)

The web Highlights tab uses the profile bundle's tray when nonempty. Otherwise it asks
`GET /v1/instagram/highlights/{username}` on entry. An empty tray remains unknown: leaving
and reopening the tab, or pressing Try again, requests it again. Loading and errors are shown
separately. Covers use the signed `coverUrl`; opening a highlight loads its items into the
existing tile grid before opening an individual item in the viewer.

The profile endpoint also falls back to StoryView's list when the bundle omits highlights.
Both service paths share the same tray cache. Only parsed, nonempty trays are reusable for
10 minutes; old cached empty trays are bypassed, and new empty responses are not cached.
Private profiles skip the fallback. The service's internal list fallback is logged as part of
`/profile/{username}`, so the absence of `/highlights/{username}` in access logs alone does
not mean the backend list was never requested. The previous web client had no list call.

Live curl checks against `100.107.164.33:8871` around 21:54–21:56 UTC (22:54–22:56 Copenhagen):

- `profile/natgeo` and `highlights/natgeo`: 200, one highlight with a signed cover. The cover
  returned 200 (7163 bytes) and `highlights/items` returned four items.
- `profile/_ter.restre` and `highlights/_ter.restre`: 200, empty highlights. Direct StoryView
  `/api/v1/users/_ter.restre/highlights` also returned 200 with `[]`. Retrying can recover when
  the provider returns the tray again; the app cannot reconstruct a tray absent upstream.
- `posts/natgeo`: 200, 12 items. `posts/_ter.restre` with the profile's next cursor: 502
  `upstream_unavailable`. Direct StoryView `/api/v1/users/_ter.restre/posts` returned 503 with
  `{"detail":"Instagram feed temporarily unavailable"}`. The service retries upstream 5xx
  twice before returning 502. This reproduces a provider failure; the earlier 21:52:05 request
  cannot be attributed to an account/cursor from the deliberately anonymous access log.

The gateway header was supplied to curl through stdin from the configured secret file;
no secret or signed URL is included here. No deployment was performed.

## Deploy (nezopt + p1)

```sh
cd crates/secretary-instagram && cargo test
cargo build --release && install -m 755 target/release/secretary-instagram ~/.local/bin/
mkdir -p ~/.config/secretary-instagram && cp ../../deploy/secretary-instagram/instagram.env.example ~/.config/secretary-instagram/instagram.env
head -c 32 /dev/urandom | base64 > ~/.config/secretary-instagram/gateway.secret && chmod 600 ~/.config/secretary-instagram/*
cp ../../deploy/secretary-instagram/secretary-instagram.service ~/.config/systemd/user/ && systemctl --user daemon-reload && systemctl --user enable --now secretary-instagram
# p1: deploy/secretary-instagram/Caddyfile.snippet (tailnet IP, /etc/caddy/instagram-gateway.key = the gateway secret), caddy reload
```
