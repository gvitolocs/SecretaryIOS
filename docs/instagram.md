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
| `GET /resolve?url=` | resolve | `MediaPage` wrapper with one item |
| `GET /users/{u}/tagged` | not used | exists (tagged posts, paginates); could become a tab |

Media URLs in every response point to `…workers.dev/cdn?url=<instagram cdn url>&exp=&sig=`. The
service unwraps `url=`, checks the host (`*.cdninstagram.com`, `*.fbcdn.net`, https only, no IPs,
no credentials, no port) and fetches it itself with no Referer.

## HTTP (`/v1/instagram`, behind p1's forward_auth + `X-Lucina-Gateway`)

Success: `{"ok":true,"data":…}`. Error: `{"ok":false,"error":"<code>"}`; 429 is
`{"ok":false,"error":"rate_limited","retryAfter":<s>}` with `Retry-After`.

| Route | `data` |
|---|---|
| `GET /status` | `{enabled, rateLimitPerSecond}` |
| `GET /profile/{username}` | `{profile, posts:{items,nextCursor}, highlights:[{id,title,coverUrl,mediaCount}]}` (also records the profile in `recent`) |
| `GET /posts/{username}?cursor=` | `{items, nextCursor, exhausted?}` |
| `GET /reels/{username}?cursor=` | `{items, nextCursor}` |
| `GET /stories/{username}?cursor=` | `{items, nextCursor}` |
| `GET /highlights/{username}` | `{items:[highlight]}` (a user named `items` is not reachable here) |
| `GET /highlights/items?id=highlight:…&cursor=` | `{items, nextCursor}` |
| `GET /resolve?url=<instagram post/reel url>` | `{items, nextCursor}` |
| `GET /recent` | `{items:[{username, fullName, isVerified, picUrl, openedAt}]}`, newest first, max 30 |
| `POST /save {items:[{url, kind, username, takenAt?, caption?}]}` | `{saved:[{index,itemId,duplicate}], failed:[{index,error}]}` |
| `GET /s/{exp}/{sig}/{token}` | media bytes (signed, ~6 h, GET/HEAD, **no gateway header**), Range supported |

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
watched account every 20-40 minutes (a random time per refresh, so accounts drift apart; a new account
is first checked within seconds), through the same single queue, spacing, backoff and 429 block as the
owner's requests. Owner requests go first: the warmer waits while one is pending. When the backend says
429 (to the warmer or to the owner) the warmer stays quiet for at least `INSTAGRAM_WATCH_PAUSE_SECS`
(30 min) and the account is rescheduled after the pause. A refresh is the profile bundle plus, for a
public account, the stories: two backend calls, both read-only (nothing is ever sent that Instagram
could count as "seen"). Both answers are written to the cache, so the owner's next visit is instant.

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
| `GET /watch` | `[{username, autoSave, lastCheck, newStories, newPosts, private, fullName, picUrl, isVerified, gone, status, archived}]` |
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
- **Cache:** `~/.local/share/secretary-instagram/cache`, capped by `INSTAGRAM_CACHE_MAX_BYTES`
  (default 2 GiB, JSON and media together), oldest file first (a hit refreshes a file's age).
  Media is keyed by CDN path plus the `stp`/`efg` rendition parameters, not by the volatile
  `oh`/`oe`, so a fresh backend URL for the same picture hits the cache and an expired link is
  still served from it. JSON bodies live 2-10 minutes. Images up to 32 MiB, videos 256 MiB.
- **Logs:** method, route template, status. No usernames, cursors, ids or captions.
- **Shutdown:** SIGTERM finishes requests in flight and exits 0.

## Deploy (nezopt + p1)

```sh
cd crates/secretary-instagram && cargo test
cargo build --release && install -m 755 target/release/secretary-instagram ~/.local/bin/
mkdir -p ~/.config/secretary-instagram && cp ../../deploy/secretary-instagram/instagram.env.example ~/.config/secretary-instagram/instagram.env
head -c 32 /dev/urandom | base64 > ~/.config/secretary-instagram/gateway.secret && chmod 600 ~/.config/secretary-instagram/*
cp ../../deploy/secretary-instagram/secretary-instagram.service ~/.config/systemd/user/ && systemctl --user daemon-reload && systemctl --user enable --now secretary-instagram
# p1: deploy/secretary-instagram/Caddyfile.snippet (tailnet IP, /etc/caddy/instagram-gateway.key = the gateway secret), caddy reload
```
