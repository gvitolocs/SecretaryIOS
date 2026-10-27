# secretary-server API (`/v1`)

The contract between the Secretary clients (the iOS app, the web app at
`https://secratary.app`) and `secretary-server`. The server sits in front of
Lucina's `/api/secretary/*` (see Lucina's `docs/secretary-api.md`): it
authenticates the client, validates the input, forwards it with its own
Lucina token, and returns Lucina's `data`. Clients never see the Lucina token
or the `{ok, data}` envelope.

## Conventions

- **JSON** in and out. Request bodies need `Content-Type: application/json`
  and are capped at 256 KiB (Lucina itself caps at 64 KiB). Exception:
  `POST /v1/device/events` accepts 4.5 MiB.
- **Success:** the HTTP status is 2xx and the body is the resource itself
  (no envelope). `201` means Lucina created something new.
- **Failure:** `{"error": {"code": "<code>", "message": "<text>"}}` with a
  matching status. Branch on `code`; `message` is for humans and may change.
- **Caching:** every `/v1` response carries `Cache-Control: no-store`.
- **Unknown fields:** ignore unknown response fields; new optional fields may
  appear (they are passed through from Lucina unchanged).
- **Times:** ISO 8601 UTC in responses (`2026-10-22T07:30:00.000Z`). Request
  `dueAt` must carry an offset (`Z` or `±HH:MM`). Dates are `YYYY-MM-DD` in
  the owner's time zone (`timeZone`, default `Europe/Rome`).

## Authentication

Every `/v1` route except `GET /v1/health`, `/v1/session` and
`POST /v1/auth/device` needs one of:

| Client | How | Notes |
|---|---|---|
| iOS app, scripts | `Authorization: Bearer sec_…` (a device token) | If an `Authorization` header is present, it alone decides; the cookie is ignored. |
| Browser | the `sec_session` cookie from `POST /v1/session` | State-changing requests (`POST`, `PUT`, `PATCH`, `DELETE`) must also send `X-Secretary-CSRF: 1`, else `403 csrf`. |
| Lucina | `Authorization: Bearer svc_…` (the service token) | `GET /v1/insights/summary` (see [Insights](#insights-for-lucina)), `/v1/avatar*` (see [Avatar wardrobe](#avatar-wardrobe)), `POST /v1/notify` and `GET /v1/notify/settings` (see [Notifications](#notifications)), `GET /v1/proactive/feedback` (see [Proactive outreach](#proactive-outreach)), the `/v1/calendar/*` routes (see [Calendar](#calendar)) and `/v1/notes*` (see [Notes](#notes)) and `/v1/library/*` (see [Library](#library)). |

There is exactly **one user**, the owner. Ways in:

- **Email + password** (the owner account): the web app logs in with
  `POST /v1/session {email, password}`; native apps get a device token from
  `POST /v1/auth/device {email, password, deviceName}`. The account is
  created on the server with `secretary-server user set --email <e>
  --password-stdin` (see `deploy/README.md`).
- **Device tokens** are also issued on the server with
  `secretary-server token issue --name <name>` and shown once. Format: `sec_`
  + 43 base64url characters. Revoking a device (`token revoke <id>`) ends its
  Bearer access and all of its browser sessions on the next request.
  `POST /v1/session {token}` still exchanges one for a browser cookie.

Password rules:

- Passwords are stored as Argon2id hashes (m = 19 MiB, t = 2, p = 1) in
  `owner.json`. The email is case-insensitive.
- Every failed password attempt answers the same `401 invalid_credentials`
  (wrong email, wrong password, no account). An unknown email costs the
  same Argon2 work as a known one.
- **Lockout:** the 10th consecutive failed password attempt (whatever the
  email) locks password logins for 15 minutes; each further failure after a
  lock doubles it (30 min, 1 h, … at most 24 h). A success resets it. While
  locked, `POST /v1/session {email,…}`, `POST /v1/auth/device` and the
  password change answer `429 account_locked` with `Retry-After: <seconds>`.
  Device-token login is never locked.
- The per-IP login limit (5/minute, `429 rate_limited`) covers both password
  routes and token login.
- **CSRF:** the server sends no CORS headers, so a cross-site page cannot add
  `X-Secretary-CSRF` (a custom header forces a preflight that fails). The
  cookie is also `SameSite=Strict`. Bearer requests are exempt.
- **Rate limits:** 60 requests/minute per device (token bucket, burst 60),
  `POST /v1/device/events` in its own bucket of 120/minute per device
  (`SECRETARY_DEVICE_EVENTS_PER_MINUTE`), and 5 `POST /v1/session`
  attempts/minute per client IP. Both answer `429
  rate_limited` with `Retry-After: <seconds>`.

Auth errors: `401 unauthorized` (with `WWW-Authenticate: Bearer`). The
`message` says why: `missing or malformed device token`, `unknown device
token`, `device token revoked`, `session expired or invalid`.

### Web app recipe

```js
// login (once): email + password (or {token}) for the cookie
await fetch("/v1/session", {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ email, password }),
});

// every later call: same origin, cookie sent automatically
const api = (path, { method = "GET", body } = {}) =>
  fetch(path, {
    method,
    headers: {
      ...(body !== undefined && { "Content-Type": "application/json" }),
      ...(method !== "GET" && { "X-Secretary-CSRF": "1" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
```

On `401` from any route, show the login screen. `GET /v1/session` tells the
app on startup whether the cookie is still good.

## Sessions

### `POST /v1/session`

Request, either:

- `{"email": "owner@example.com", "password": "…"}` (the owner account), or
- `{"token": "sec_…"}` (a device token).

`200`:

```json
{ "authenticated": true, "deviceName": "web" }
```

(`deviceName` is `"web"` for a password login, else the token's device
name.) Password logins hang off one server-side device named `web`;
revoking it (`token revoke`) ends every password-login browser session.

with

```
Set-Cookie: sec_session=<43 base64url chars>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000
```

The session lasts 30 days (not sliding). Only its SHA-256 is stored on the
server. Errors: `400 bad_request` (neither `token` nor `email`+`password`)
/ `400 invalid_json` / `415 unsupported_media_type` (body), `401
invalid_credentials` (password login), `401 unauthorized` (unknown or
revoked token), `429 rate_limited` (per IP), `429 account_locked`
(password lockout, with `Retry-After`). No CSRF header is needed here; the
JSON content type already forces a CORS preflight for cross-site callers.

```json
{ "error": { "code": "invalid_credentials", "message": "wrong email or password" } }
```

### `POST /v1/auth/device` (no auth)

For native apps: trade the owner's password for a new device token.

```json
{ "email": "owner@example.com", "password": "…", "deviceName": "Giuseppe's iPhone" }
```

`201`:

```json
{ "token": "sec_…43 chars…", "deviceId": "dev_Xk3…", "deviceName": "Giuseppe's iPhone" }
```

The token is shown only here (the server keeps its SHA-256); store it in
the Keychain and send it as `Authorization: Bearer`. `deviceName`: 1–64
printable characters. Errors as for the password login (`401
invalid_credentials`, `429 rate_limited`, `429 account_locked`), plus `400
bad_request` for a bad `deviceName`. The device appears in `token list`
and can be revoked there.

### `POST /v1/session/password` (authenticated)

```json
{ "currentPassword": "…", "newPassword": "at least 8 characters" }
```

Cookie clients need `X-Secretary-CSRF: 1`. `200`:

```json
{ "changed": true, "sessionsRevoked": 2 }
```

Every browser session except the caller's own cookie session ends (with a
Bearer token: every browser session). Device tokens stay valid. Errors:
`403 invalid_credentials` (wrong current password; counts toward the
lockout; 403 so the app does not log out), `400 bad_request` (new password
shorter than 8 characters or over 1024 bytes), `429 account_locked`, `409
no_owner_account`.

### `GET /v1/session`

Never 401. With a valid cookie or Bearer token:
`{"authenticated": true, "deviceName": "…"}`; otherwise
`{"authenticated": false, "deviceName": null}`.

### `DELETE /v1/session`

Logs out: deletes the session and clears the cookie
(`Set-Cookie: sec_session=; …; Max-Age=0`). With a cookie it needs
`X-Secretary-CSRF: 1` (else `403 csrf`). `200`
`{"authenticated": false, "deviceName": null}`.

## Health

### `GET /v1/health` (no auth)

```json
{
  "ok": true,
  "version": "0.1.0",
  "lucina": { "reachable": true, "latencyMs": 12, "apiVersion": 1 }
}
```

`ok` is the server's own liveness (always `true` when it answers). The Lucina
probe has a 3 s timeout and is cached for 10 s. When Lucina is down:
`"lucina": {"reachable": false, "latencyMs": null, "error": "lucina_unreachable"}`
(`error` is one of `lucina_unreachable`, `lucina_timeout`, `lucina_error`,
`not_configured`, or a Lucina error code; `latencyMs` is set when Lucina
answered with an error).

## Chat

### `POST /v1/chat`

```json
{ "message": "Ricordami alle 18:30 di chiamare la mamma", "sessionId": "iphone", "clientMessageId": "B1E4C0DE-2F0A-4B8C-9D11-7A6E5F4C3B2A" }
```

| Field | Rules |
|---|---|
| `message` | Required, 1–12000 characters (12100 with a Lucina MCP prefix), not blank, no control characters except newline/tab. For longer documents, save a [note](#notes). |
| `sessionId` | Optional, `[A-Za-z0-9_-]{1,64}`. Lucina uses Honcho session `secretary_<sessionId>`. |
| `clientMessageId` | Optional, 1–128 characters of `[A-Za-z0-9_.:-]` (a UUID works). A retry with the same `sessionId` + `clientMessageId` within 10 minutes returns the first answer with `"duplicate": true`. |
| `channel` | Optional, `web` or `ios`: which client the message came from (it labels the message in the [timeline](#timeline)). |
| `replyTo` | Optional, `[A-Za-z0-9_-]{1,64}`: the timeline id of the earlier message this one replies to (swipe to reply, see [Replies](#replies)). Empty = absent. |

The channel comes from the `X-Secretary-Client: web|ios` request header,
else from the body's `channel`, else defaults to `web`; the header wins when
both are sent. Any other value is `400 bad_request`. The server always
forwards a `channel` to Lucina.

Server timeout 120 s (an LLM turn); use a client timeout of at least 125 s.

`200`:

```json
{
  "reply": "Reminder set for Tuesday 21 October 2026 at 18:30 (Europe/Rome): chiamare la mamma",
  "action": "schedule_reminder",
  "job": null,
  "sessionId": "secretary_iphone",
  "clientMessageId": "B1E4C0DE-2F0A-4B8C-9D11-7A6E5F4C3B2A",
  "reminder": { "id": "…", "text": "chiamare la mamma", "dueAt": "2026-10-21T16:30:00.000Z", "…": "…" },
  "userItemId": "tl_0192…",
  "lucinaItemId": "tl_0193…"
}
```

`userItemId` / `lucinaItemId` are the timeline ids of the stored message
and of Lucina's reply (present when Lucina records the timeline). The same
items also arrive on [`/v1/timeline/stream`](#get-v1timelinestream);
de-duplicate by `id` (or by `clientMessageId` for the optimistic copy of
your own message).

#### Replies

Swiping a timeline message (the owner's or Lucina's) replies to it: send its
`id` as `replyTo`.

```json
{ "message": "Cosa intendevi?", "sessionId": "iphone", "replyTo": "01K83ZK4Q6X1V9W2D3E4F5G6H7" }
```

- Lucina gets the quoted message in the turn, marked as the message being
  replied to: its text, who wrote it and when, its attachments (names and
  kinds, plus what Lucina read from them), and a voice note's transcript
  (the attachment's `transcript`, else the one on Lucina's reply to it).
  The block is clipped to 4000 characters.
- The stored user item carries `"replyTo": "<that id>"`, so every client can
  draw the quote by looking the item up; Lucina's answer keeps
  `replyTo` = the owner's item, as for every turn.
- An id that names no stored item is ignored (normal turn, no `replyTo` on
  the item). A malformed one is `400 bad_request`. The response shape is
  unchanged.
- A shared file or voice note can be a reply too: `replyTo` form field on
  `POST /v1/share` (docs/share.md, "Replies").

`action` is `reply`, `grocery`, `list_reminders`, `schedule_reminder`
(with `reminder` when one was created or already existed), or an async
action (`cursor_task`, `oracle_monitor_task`, `spreadsheet_update_task`,
`graph_task`, `generate_image`, `social_post_task`). An async action with a
`job` (`{"id", "status": "queued"}`) delivers its result through
`GET /v1/jobs/{id}`; with `job: null` it could not start and `reply` says why.
Treat unknown `action` values as `reply`.

### `GET /v1/jobs/{id}`

`200` Job:

```json
{
  "id": "0c64051c-…", "action": "generate_image", "status": "done",
  "result": "Here it is.", "error": null, "imageUrl": "https://…",
  "sessionId": "secretary_app", "clientMessageId": null,
  "createdAt": "…", "startedAt": "…", "finishedAt": "…", "updatedAt": "…"
}
```

`status`: `queued` → `running` → `done` | `error`. Poll every 2–5 s until
`done` or `error`. `error: "interrupted"` means Lucina restarted mid-job.
`404 job_not_found`.

## Timeline

The unified conversation: every message Lucina sees or sends, on every
channel (Telegram, Discord, the web app, the iOS app, Alexa), as one list.
Both routes need auth (Bearer or the session cookie); they are `GET`, so
cookie callers need no CSRF header.

Timeline item:

```json
{
  "id": "tl_0193…",
  "ts": 1761048003000,
  "channel": "telegram",
  "role": "lucina",
  "text": "Fatto, te lo ricordo alle 18:30.",
  "clientMessageId": "B1E4C0DE-…",
  "replyTo": "tl_0192…",
  "attachments": [{ "kind": "image", "name": "scontrino.jpg" }],
  "meta": { "chatTitle": "Casa", "isDm": true },
  "backfilled": true
}
```

| Field | |
|---|---|
| `id` | Opaque id (string; treat a number as its decimal text). Also the pagination cursor and the SSE event id. |
| `ts` | Epoch milliseconds. Parse an ISO 8601 string too: the server passes `ts` through as Lucina sent it. |
| `channel` | `telegram`, `discord`, `web`, `ios`, `alexa` or `other`. Show unknown values as `other`. |
| `role` | `user` or `lucina`. Skip items with any other role. |
| `text` | May be empty (for example an attachment-only message). |
| `clientMessageId` | Optional; set on messages sent through `POST /v1/chat` with one. |
| `replyTo` | Optional id of the item this answers: on Lucina's items the owner's message; on the owner's items the message he replied to (swipe to reply). |
| `attachments` | Optional `[{kind: image\|voice\|file, name?}]`. |
| `meta` | Optional `{chatTitle?, guildName?, channelName?, isDm?, kind?}` (`kind: "reminder"` for a delivered reminder). |
| `backfilled` | Optional `true` for history imported after the fact. |

Other fields may appear and are passed through unchanged.

### `GET /v1/timeline?before=<id>&limit=50`

Also served at `GET /api/secretary/timeline`, with the same bearer authentication.
Items are raw stored JSON, newest id first (including incremental responses).
The success envelope is:

```json
{"ok":true,"data":{"items":[],"nextBefore":null,"cursor":null,"hasMore":false}}
```

| Query | Rules |
|---|---|
| `before` | Optional timeline id (`[A-Za-z0-9_.:-]{1,128}`): only older ids. Pass the previous page's `nextBefore`. |
| `limit` | Optional integer 1–200, default 50. |
| `after` | Optional timeline id: scan items that arrived after this item, in file arrival order, returning at most `limit` matches. Mutually exclusive with `before`. Unknown or rotated-away cursor: HTTP 410, `{"ok":false,"error":"cursor_expired","message":"timeline cursor expired"}`. |
| `conversation` | Optional key matching `^[a-z]+(:[A-Za-z0-9_-]{1,64})?$`. Only that conversation. `selftest` retains its separate diagnostic store. Mutually exclusive with `scope`. |
| `scope` | Only `mine` is accepted; requires `device` (valid id) and `channel` (`web` or `ios`). Includes `device:<device>`, `legacy:<channel>`, and `lucina`. |

Invalid queries return HTTP 400 `bad_request`. All responses have
`Cache-Control: no-store`. `nextBefore` is the oldest returned id if more matching
older items remain, otherwise `null`. `cursor` is the newest arrival id at read
time for ordinary pages. For `after` pages, it is the last scanned id when more
items remain to scan, otherwise the newest arrival id. It is `null` only for an
empty store. Save `cursor` as the next `after`, and continue while `hasMore` is
true. A scan may cross nonmatching items; always advance using `cursor`, not a
returned item's id. A late writer can append an older id, so incremental sync
uses arrival order while responses display items newest id first. On 410, rebuild
the local snapshot using ordinary pages. The SSE stream is unchanged.

Chat bodies accept optional `device` (`[A-Za-z0-9_-]{1,64}`; invalid values are
dropped) and `deviceName` (controls removed, trimmed, clipped to 64 UTF-16 code
units). Both are stored on the user and reply meta, also on MCP turns and later
job results when the turn context is available. Existing lines remain unchanged.

Conversation keys are `agent:<caller>` for MCP turns (`via=mcp`, lowercased caller
with characters outside `[a-z0-9_-]` removed, empty becomes `mcp`), otherwise
`device:<device>` for web/iOS with a device, `legacy:web` or `legacy:ios` without
one, `telegram`, `discord`, `alexa`, or `lucina` for other channels.

### `GET /v1/timeline/conversations`

Also `GET /api/secretary/timeline/conversations`, same authentication,
`Cache-Control: no-store`. Returns `{"ok":true,"data":{"items":[...]}}`, excluding
the selftest store. Summaries are newest `lastTs` first and contain `key`, `kind`,
`title`, `channel`, optional `device`, `deviceName`, `caller`, `count`, `lastId`,
`lastTs`, `lastRole`, and `lastText`. Counts cover retained generations only.
The last fields describe the newest item by id; `lastText` is plain text with
whitespace collapsed and clipped to 160 UTF-16 code units. Device titles use the
latest deviceName seen, otherwise the device id; legacy titles are “iPhone
(before devices)” and “Web (before devices)”. Agent titles are ChatGPT, Claude,
Claude Code, Grok Bot (grok or grok-bot), Cursor, Codex, otherwise the stored
caller. Other titles are Telegram, Discord, Alexa, and Lucina.

### `GET /v1/timeline/stream`

[Server-Sent Events](https://html.spec.whatwg.org/multipage/server-sent-events.html):
new items as they happen, relayed unchanged from Lucina's
`/api/secretary/timeline/stream` without buffering. Response headers:
`Content-Type: text/event-stream; charset=utf-8`, `Cache-Control: no-store`,
`X-Accel-Buffering: no`.

```
: ping

event: item
id: tl_0193…
data: {"id":"tl_0193…","ts":1761048003000,"channel":"web","role":"user","text":"Ciao"}

```

- `event: item` carries one timeline item as JSON in `data` (the raw
  Lucina item, same shape as above). Ignore other event names.
- `: ping` is a comment line Lucina sends every 20 s to keep proxies from
  closing the idle connection. EventSource ignores it; hand-written parsers
  must skip lines starting with `:`.
- **Resuming:** send the last `id` you saw as the `Last-Event-ID` header and
  Lucina replays everything after it. EventSource does this by itself when
  it reconnects. A *new* EventSource cannot set headers, so pass
  `?lastEventId=<id>` instead (the header wins if both are present). An id
  outside the id charset is `400 bad_request`.
- **Ending:** when Lucina's stream ends, fails, or sends nothing (not even a
  ping) for 60 s, the server ends the response cleanly; it also ends every
  stream when it shuts down. EventSource then reconnects on its own (after
  its retry delay, sending `Last-Event-ID`). Native clients: reconnect with
  backoff (1 s, 2 s, 5 s, 10 s, 30 s) and send `Last-Event-ID`.
- **Errors before the stream starts** are ordinary JSON errors with a
  status: `401`, `429 too_many_streams`, `502 lucina_error` /
  `lucina_unreachable`, `504 lucina_timeout` (Lucina did not connect within
  10 s or send headers within 60 s), `503 lucina_not_configured`.
  EventSource gives up for good on a non-200 answer (`readyState` becomes
  `CLOSED`): open a new one with backoff and reload the latest page with
  `GET /v1/timeline` to fill the gap.
- **Limits:** at most 4 open streams per device and 20 in total; beyond
  that `429 too_many_streams` with `Retry-After: 5`. A stream counts once
  against the per-device rate limit (when it opens), not per event. One
  stream per tab or app is enough; close it when hidden for long.
- There is no total duration limit on the stream.

**Behind Caddy** nothing special is needed. Caddy's
[`reverse_proxy`](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy#streaming)
flushes immediately when the upstream answers `Content-Type:
text/event-stream` (it ignores `flush_interval` then), and it has no read,
response-header or stream timeout by default. The `encode zstd gzip` in
`deploy/Caddyfile.snippet` still compresses the stream, but
[`encode`](https://caddyserver.com/docs/caddyfile/directives/encode) does not
hold back `text/event-stream` responses to reach `minimum_length`: headers go
out at once and the body is streamed as written. (Checked against Caddy's
docs on 2026-10-21.) `X-Accel-Buffering: no` is for nginx, should one ever
sit in front.

## Reminders

Reminder object:

```json
{
  "id": "6f1c2a8e-…", "text": "Call the dentist", "dueAt": "2026-10-22T07:30:00.000Z",
  "timeZone": "Europe/Rome", "status": "pending", "source": "secretary",
  "createdAt": "…", "sentAt": null, "cancelledAt": null
}
```

`status`: `pending`, `sent` (delivered to the owner's Telegram), `cancelled`.

### `GET /v1/reminders?scope=upcoming|today|all&includeSent=0|1`

Both parameters optional (Lucina defaults: `upcoming`, `0`); `includeSent`
also accepts `true|false`. `200` (Lucina's object, not unwrapped):

```json
{ "scope": "upcoming", "includeSent": false, "timeZone": "Europe/Rome", "reminders": [ <Reminder>, … ] }
```

### `POST /v1/reminders`

Either `{"text": "Call the dentist", "dueAt": "2026-10-22T09:30:00+02:00"}`
(text 1–500 characters, `dueAt` RFC 3339 with offset, in the future) or
`{"naturalText": "ricordami domani alle 9:30 di chiamare il dentista"}`
(1–500 characters; Italian or English).

`201` when created, `200` when an identical pending reminder already existed:

```json
{ "created": true, "reminder": <Reminder> }
```

Errors: `400 bad_request`, `400 due_in_past`, `400 unparseable_reminder`,
`503 reminders_disabled`, `503 owner_chat_unconfigured`.

### `DELETE /v1/reminders/{id}`

`200` `{"cancelled": true, "reminder": <Reminder, status "cancelled">}`
(`"cancelled": false` if it was already cancelled). `404 reminder_not_found`,
`409 already_sent`. Cookie clients: needs `X-Secretary-CSRF: 1`.

## Agenda

### `GET /v1/agenda?date=YYYY-MM-DD&refresh=0|1`

Both optional (`date` defaults to today in `timeZone`). `200`:

```json
{
  "date": "2026-10-22", "timeZone": "Europe/Rome",
  "reminders": [ <Reminder>, … ],
  "briefing": "Tomorrow starts at 9:30 with the dentist call. …",
  "briefingGeneratedAt": "…", "briefingCached": false
}
```

If the briefing could not be generated, `briefing` is null and
`briefingError` is `"briefing_unavailable"` (still `200`). `refresh=1` can
take as long as an LLM turn (server timeout 15 s → `504 lucina_timeout`).

## Shopping

Item object:

```json
{ "id": "3d0b8a52-…", "product": "milk", "rawName": "latte", "category": "dairy",
  "status": "needed", "createdAt": "…", "updatedAt": "…" }
```

### `GET /v1/shopping`

`200`: a JSON **array** of items (Lucina's `{items}` is unwrapped), `needed`
first, then `purchased`.

### `POST /v1/shopping`

`{"rawName": "latte", "product": "milk"}`: `rawName` required (1–200
characters), `product` optional (1–200). `201`: the new item (Lucina's
`{item}` unwrapped), status `needed`.

### `PATCH /v1/shopping/{id}`

`{"status": "purchased"}` (or `"needed"`). `200`: the item, unwrapped. Lucina
returns only `{id, product, status}` here, with `product` = the raw name.
Errors: `400 invalid_status`, `404 item_not_found`.

All shopping routes may answer `503 grocery_unavailable`.

The three routes above are the **legacy** surface (chat and old clients);
they keep working against the default list. New clients use v2 below.

### Shopping v2

Multiple lists, catalog tiles and an idempotent op log, per
[`shopping-v2.md`](shopping-v2.md). Lucina's grocery store is the source of
truth; the server validates and forwards (`/api/secretary/shopping/lists…`).
All routes need auth; cookie writes need `X-Secretary-CSRF: 1`.

#### `GET /v1/shopping/catalog?lang=it|en|da`

The static catalog (`{version, lang, sections, items}`, see
`shopping-v2.md`), served byte for byte from
`$SECRETARY_CATALOG_DIR/catalog.{lang}.json` (default dir
`/var/lib/secretary/catalog`). No Lucina call. `lang` defaults to `it`;
anything else is `400 bad_request`. A missing file is `503
catalog_unavailable`.

Headers: a strong `ETag` (`"<base64url SHA-256 of the file>"`), `Cache-Control:
public, max-age=3600`, `Vary: Authorization, Cookie`. Send the tag back in
`If-None-Match` and an unchanged file answers `304` with no body (the same
`ETag` and `Cache-Control`). The tag changes whenever the file does.

#### Lists

| Route | Body | `2xx` |
|---|---|---|
| `GET /v1/shopping/lists` | | Array of `{id, name, theme, needed, recent, updatedAt}` (Lucina's `{lists}` is unwrapped if present). |
| `POST /v1/shopping/lists` | `{name, theme?}` | `201` the list (Lucina's `{list}` unwrapped if present). |
| `GET /v1/shopping/lists/{id}` | | `{list, revision, items}` (below). |
| `PATCH /v1/shopping/lists/{id}` | `{name?, theme?}`, at least one | The list. |
| `DELETE /v1/shopping/lists/{id}` | | Lucina's answer as is (the default list can't be deleted). |

`name`: 1–80 characters, no control characters. `theme`: `home`,
`grocery`, `work`, `holiday`, `garden`, `hardware`, `baby`, `other`. List ids
use the id charset `[A-Za-z0-9_.:-]{1,128}` (`default` is the default list).

List state:

```json
{ "list": {"id": "default", "name": "Casa", "theme": "home"}, "revision": 42,
  "items": [ { "id": "…", "catalogId": "banana", "name": "Banane", "spec": "6, mature",
               "quantity": null, "unit": null, "section": "produce", "icon": "🍌",
               "status": "needed", "position": 3, "updatedAt": "…", "updatedBy": "telegram" } ] }
```

`status`: `needed` (tiles), `recent` (quick re-add), `removed` (never show;
treat unknown statuses like `removed`). Unknown fields pass through.

#### `POST /v1/shopping/lists/{id}/ops`

```json
{ "expectedRevision": 42,
  "ops": [ { "opId": "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b", "type": "add", "catalogId": "banana", "name": "Banane", "spec": "6" },
           { "opId": "7f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b", "type": "purchase", "itemId": "…" } ] }
```

`200`: `{revision, applied: [opId], duplicates: [opId], items: [...full state...]}`.

Validation (all before Lucina; any failure is `400 bad_request` naming
`ops: [i].<field>`):

- 1–200 ops per batch; every `opId` a canonical UUID (`8-4-4-4-12`), distinct
  within the batch. `expectedRevision` (optional) a non-negative integer.
- Op types and their fields (other fields are dropped):

| `type` | Fields |
|---|---|
| `add` | `catalogId?` (id), `name?` (1–200), at least one of the two; `spec?` (≤200), `quantity?` (number > 0, ≤ 100000), `unit?` (≤32), `section?` (id) |
| `need`, `purchase`, `remove` | `itemId` |
| `set_spec` | `itemId`, `spec` (≤200; `null` or `""` clears) |
| `set_quantity` | `itemId`, `quantity` (as above; `null` clears), `unit?` |
| `set_section` | `itemId`, `section` |
| `rename` | `itemId`, `name` (1–200) |
| `move` | `itemId`, `toListId` |
| `clear_recent` | none |
| `reorder` | `itemId`, `position` (integer 0–100000) |

Semantics are Lucina's: an already-applied `opId` is listed in `duplicates`
and changes nothing, so an outbox can replay a batch safely. A stale
`expectedRevision` only fails when an op conflicts: Lucina's `409` becomes
**`409 revision_conflict`**; refetch the list and retry the unapplied ops with
new `opId`s. Lucina's `404` (`list_not_found`, `item_not_found`) and `422`
codes pass through unchanged.

## Local modules: gym and body

The gym and body-weight modules are Rust engines (`crates/secretary-gym`,
`crates/secretary-body`) that the server hosts with persistent files in
`$SECRETARY_DATA_DIR` (`gym.json`, `body.json`, atomic writes, mode 0600).
The iOS app links the same engines through `secretary-ffi`
(`SecretaryCore.h`) and speaks the same JSON. Lucina is not involved. The
clients are thin: they send commands and render the returned view; every
rule, unit conversion, statistic and label is computed in Rust.

### The command protocol (both modules)

Every write is one JSON request:

```json
{
  "op_id": "3f0c6f9e-7c1d-4a51-9d8e-2b6a1f0e9c11",
  "expected_revision": 12,
  "timezone": "Europe/Rome",
  "command": { "type": "log_entry", "weight_grams": 80500 }
}
```

| Field | Rules |
|---|---|
| `op_id` | Required for writes. 1–128 bytes, not blank. Generate one per user action (a UUID) and keep it with the queued command. |
| `expected_revision` | Required for writes: the `revision` of the state the user saw. |
| `timezone` | Optional IANA name, default `UTC`. Decides calendar days, weeks and date labels. Unknown names → `invalid_command`. |
| `command` | `{"type": "<snake_case>", …fields}`. Unknown fields are ignored; unknown types are `invalid_command`. |

Query commands (`snapshot`, `parse_weight`, and for the gym also `clock`,
`search_exercises`, `export_json`, `export_csv`) need neither `op_id` nor
`expected_revision` and never change state.

Write semantics, in this order:

1. `op_id` already used for the **same** command → success with
   `"replayed": true`; nothing changes; the response carries the current
   view (even if the revision moved on since). Resending a queued command is
   always safe.
2. `op_id` already used for a **different** command → `op_id_reused`.
3. `expected_revision` ≠ current revision → `conflict`.
4. The command is applied to a copy, validated, written to disk atomically,
   and only then becomes the live state (revision + 1). Any rejection leaves
   the state byte-for-byte unchanged.

Response:

```json
{ "ok": true, "replayed": false, "view": { … } }
{ "ok": false, "replayed": false, "code": "conflict", "error": "State changed. Refresh and retry the edit." }
```

`code`: `invalid_command` (validation, bad JSON, unknown timezone),
`conflict`, `op_id_reused`, `storage_error` (the server could not write its
file). `error` is a human message (English) that can be shown as is.

**Offline queue recipe (phone or browser):** for each user action create
`{op_id: uuid(), expected_revision: <revision shown>, timezone, command}`,
persist it, send queued requests in order, and resend them verbatim after a
crash or timeout. `replayed: true` counts as success. On `conflict`
(recommended, and what the web app does): fetch the state, rebuild the
command against the new revision **with a new op_id** and retry it once
automatically; if that also conflicts or the command no longer applies
(`invalid_command`), drop it from the queue and tell the user. Never reuse
an op_id for a different command (`op_id_reused`, 422).

### HTTP routes

| Route | Body | Notes |
|---|---|---|
| `GET /v1/gym/state?timezone=Europe/Rome` | — | The `snapshot` view. |
| `GET /v1/gym/clock?timezone=…` | — | The `clock` query (cheap; poll it for the rest timer). |
| `POST /v1/gym/command` | a request as above | Any gym command, queries included (`clock` works here without `op_id`). Body up to 16 MiB (backups for `restore_json`). |
| `GET /v1/body/state?timezone=…` | — | The body `snapshot` view. |
| `POST /v1/body/command` | a request as above | Body up to 256 KiB. |

Auth is the same as for every `/v1` route (device Bearer, or cookie plus
`X-Secretary-CSRF: 1` on POST). POST bodies need `Content-Type:
application/json`.

The response body is always the engine response above (`{ok, replayed,
code?, error?, view?, …}`), also on errors, with this status:

| Engine result | HTTP |
|---|---|
| `ok: true` (replays included) | 200 |
| `conflict` | 409 |
| `invalid_command`, `op_id_reused` | 422 |
| `storage_error` | 500 |

Errors raised before the engine runs (401, 403 `csrf`, 413, 415, 429) use
the server's usual `{"error": {"code", "message"}}` shape.

### Weights on the wire

Integer grams are the canonical unit everywhere (`weight_grams`,
`goal_grams`, `volume_grams`, …); stored state never contains floats. The
user's display unit (`kg` or `lb`) is a setting of each module. For text
input, the engines own the parsing:

- `{"type": "parse_weight", "value": "176,5", "unit": "lb"}` (query, both
  modules; `unit` defaults to the module's display unit) →
  `{"ok": true, "replayed": false, "weight_grams": 80059}`. Up to three
  decimals, `,` or `.` as separator; lb → g uses 1 lb = 453.59237 g, rounded
  to the nearest gram.
- Commands that take a weight accept either `weight_grams` (integer) or
  `weight` (a decimal string in the display unit, parsed as above), never
  both.

Every view carries preformatted labels in the display unit; clients should
show those instead of formatting numbers themselves.

### Body module

Body weight lives here only. (The gym's `record_measurement` keeps
`body_fat` and `waist`; `kind: "body_weight"` is rejected there since this
module exists. Old gym `body_weight` measurements stay readable in the gym
state.)

Commands:

| `type` | Fields | |
|---|---|---|
| `snapshot` | — | query |
| `parse_weight` | `value: string`, `unit?: "kg"\|"lb"` | query |
| `log_entry` | `weight_grams?: int` or `weight?: string` (exactly one), `measured_ms?: int` (default now), `note?: string` | |
| `edit_entry` | `entry_id: string`, then any of `weight_grams`/`weight` (at most one), `measured_ms`, `note` (at least one field) | omitted fields keep their value |
| `delete_entry` | `entry_id: string` | |
| `update_settings` | `unit: "kg"\|"lb"`, `goal_grams?: int \| null` or `goal?: string` | replaces the settings; no goal clears it |

Limits: weight and goal 1 000–650 000 g; `measured_ms` ≤ now + 24 h; note ≤
1 000 bytes; ≤ 50 000 entries.

Real response to a `log_entry` (`{"weight": "80,5", "note": "after run"}`)
after four earlier entries 9, 6, 3 and 1 days before, with a 78 kg goal;
`entries`, `series` and `labels` are trimmed to two items:

```json
{
  "ok": true,
  "replayed": false,
  "view": {
    "revision": 6,
    "settings": { "unit": "kg", "goal_grams": 78000 },
    "entries": [
      { "id": "773a210b-8db8-41e0-b3b5-d38b9771412e", "weight_grams": 80500, "measured_ms": 1792602906883, "note": "after run", "created_ms": 1792602906883, "updated_ms": 1792602906883 },
      { "id": "1e69e2f3-3fe8-40b4-9614-559ed4eef206", "weight_grams": 80400, "measured_ms": 1792516506708, "note": "", "created_ms": 1792602906855, "updated_ms": 1792602906855 }
    ],
    "stats": {
      "count": 5,
      "current_grams": 80500, "current_measured_ms": 1792602906883,
      "avg7_grams": 80625, "weekly_change_grams": -575,
      "min_grams": 80400, "max_grams": 81200,
      "trend30_grams": -575, "trend90_grams": -575,
      "goal_grams": 78000, "goal_distance_grams": 2625, "goal_reached": false
    },
    "series": [
      { "entry_id": "1a45cf06-8446-4965-a1dd-edaeada33fce", "measured_ms": 1791825306708, "weight_grams": 81200, "avg7_grams": 81200 },
      { "entry_id": "773a210b-8db8-41e0-b3b5-d38b9771412e", "measured_ms": 1792602906883, "weight_grams": 80500, "avg7_grams": 80625 }
    ],
    "labels": { "773a210b-8db8-41e0-b3b5-d38b9771412e": "80.5 kg", "1e69e2f3-3fe8-40b4-9614-559ed4eef206": "80.4 kg" },
    "stat_labels": {
      "avg7": "80.6 kg", "current": "80.5 kg", "goal": "78.0 kg", "goal_distance": "+2.6 kg",
      "max": "81.2 kg", "min": "80.4 kg", "trend30": "-0.6 kg", "trend90": "-0.6 kg", "weekly_change": "-0.6 kg"
    }
  }
}
```

- `entries`: newest first (by `measured_ms`). `series`: the same entries
  oldest first, each with the 7-day average at that point (for the line).
- `avg7(t)` = mean of the entries measured in `(t − 7 days, t]`, rounded
  half up to the gram. `avg7_grams` = `avg7` at the latest entry.
- `current_grams` = the latest entry's weight.
- `weekly_change_grams` = `avg7(latest) − avg7(latest − 7 days)`; null when
  no entry falls in `(latest − 14 d, latest − 7 d]`.
- `trend30_grams` / `trend90_grams`: over the entries measured in the last 30
  / 90 days (ending now), `avg7(last of them) − avg7(first of them)`; null
  with fewer than 2 such entries.
- `min_grams` / `max_grams`: over all entries.
- `goal_distance_grams` = `avg7_grams − goal_grams` (positive: still above
  the goal). `goal_reached`: for a goal below the first-ever entry (losing)
  `avg7 ≤ goal`, otherwise `avg7 ≥ goal`.
- Every stat is null when there is not enough data (`count` 0 → all null).
- `stat_labels` keys (present when the stat is non-null): `current`, `avg7`,
  `weekly_change`, `min`, `max`, `trend30`, `trend90`, `goal`,
  `goal_distance`. Signed values carry a sign (`-0.4 kg`, `+1.2 kg`).
  Labels use one decimal.

### Gym module

Commands (fields are required unless marked `?`; weights as above):

| `type` | Fields | Query? |
|---|---|---|
| `snapshot` | — | yes |
| `clock` | — | yes; returns `clock` instead of `view` |
| `parse_weight` | `value`, `unit?` | yes; returns `weight_grams` |
| `search_exercises` | `query: string` | yes; returns `exercises` (and `view`) |
| `export_json` | — | yes; `text` = pretty JSON backup (no receipts) |
| `export_csv` | — | yes; `text` = CSV of completed sets |
| `start_workout` | `title: string` (1–120 bytes), `routine_id?: string` | |
| `repeat_workout` | `workout_id: string` (a finished workout) | starts a new active workout with the same title, exercises and set values (sets unchecked) |
| `rename_workout` | `title` | |
| `set_notes` | `notes: string` (≤ 8000 bytes), `exercise_id?: string` (a workout-exercise id; omitted = workout notes) | |
| `add_exercise` | `exercise_id` (catalog id) | |
| `remove_exercise` | `exercise_id` (workout-exercise id) | |
| `reorder_exercises` | `ids: [string]` (a permutation of the workout-exercise ids) | |
| `add_set` | `exercise_id` (workout-exercise id) | copies the last set |
| `edit_set` | `set_id`, `weight_grams` or `weight` (exactly one), `reps: int` (≤ 1000), `seconds: int` (≤ 86400), `distance_meters: int` (≤ 1 000 000), `kind: "normal"\|"warmup"\|"drop"\|"failure"` | the set must be unchecked; fields that do not apply to the exercise kind are zeroed |
| `complete_set` | `set_id`, `completed: bool` | checking a set starts the rest timer |
| `remove_set` | `set_id` | |
| `finish_workout` | — | needs ≥ 1 completed set |
| `cancel_workout` | — | |
| `save_routine` | `name`, `exercise_ids?: [catalog id]`, `from_active?: bool` | |
| `delete_routine` | `routine_id` | |
| `delete_workout` | `workout_id` | |
| `create_exercise` | `name`, `muscle`, `equipment` (1–120 bytes each), `kind: "weight_reps"\|"reps_only"\|"timed"\|"distance_time"` | |
| `start_rest` | `seconds: int` (1–3600) | needs an active workout |
| `stop_rest` | — | |
| `update_settings` | `weight_unit: "kg"\|"lb"`, `rest_seconds: int` (1–3600), `weekly_goal: int` (1–14) | |
| `record_measurement` | `kind: "body_fat"\|"waist"`, `value: string` (decimal; % or cm) | |
| `delete_measurement` | `measurement_id` | |
| `restore_json` | `backup: string` (an `export_json` text) | keeps revision and receipts |

View (`view` in every successful response except `clock` and
`parse_weight`):

| Field | |
|---|---|
| `state` | `schema_version`, `revision`, `settings {weight_unit, rest_seconds, weekly_goal}`, `exercises[]` (catalog, sorted by name), `routines[]`, `active` (workout or null), `history[]` (newest first), `measurements[]` (newest first), `rest {workout_id, deadline_ms}` or null. Workout: `{id, title, notes, started_ms, finished_ms, exercises: [{id, exercise_id, notes, sets: [{id, kind, weight_grams, reps, seconds, distance_meters, completed}]}]}`. |
| `summary` | `workouts`, `completed_sets`, `weekly_workouts`, `weekly_goal`, `volume` (label), `active_volume` (label), `active_duration` (label), `active_elapsed_seconds` (int), `active_completed_sets` |
| `records[]` | per catalog exercise with history: `exercise_id`, `name`, `weight` (label), `reps`, `volume` (label), `seconds`, `distance_meters`, `estimated_1rm` (label with one decimal, e.g. `"116.7 kg"`, or null), `estimated_1rm_grams` (int or null; Epley `w·(1 + reps/30)`, `w` for single reps) |
| `chart[]` | last 14 finished workouts, oldest first: `{id, label: "21 Oct", volume: number (display unit), volume_grams: int}` |
| `exercise_charts` | `{catalog exercise id: ChartPoint[]}`: the same per exercise (its volume in each of its last 14 workouts) |
| `rest_remaining` | seconds |
| `weight_labels` | `{set id: "60"}` (number in the display unit, no unit) |
| `previous_labels` | `{workout-exercise id: "60 kg × 8"}` (last time's set) |
| `measurement_labels` | `{measurement id: "18.5%"}` |
| `workout_labels` | `{workout id: {sets, volume, duration}}` |

Real response to `complete_set` in a second workout (the first one: bench
press 80 kg × 5); `state.exercises` (the 30-exercise catalog),
`state.routines`, `state.history` and `state.measurements` are elided:

```json
{
  "ok": true,
  "replayed": false,
  "view": {
    "state": {
      "schema_version": 1, "revision": 9,
      "settings": { "weight_unit": "kg", "rest_seconds": 90, "weekly_goal": 3 },
      "exercises": "…", "routines": "…", "history": "…", "measurements": [],
      "active": {
        "id": "04e54b17-2a5b-45c6-ab75-e03bce9aee0e", "title": "Push", "notes": "",
        "started_ms": 1792602906497, "finished_ms": null,
        "exercises": [
          { "id": "dc24d2a5-edaf-4651-9195-fd9bff73f83e", "exercise_id": "bench-press", "notes": "",
            "sets": [ { "id": "8ea818e6-5ff6-42c8-8764-9dc104d670b6", "kind": "normal", "weight_grams": 82500, "reps": 5, "seconds": 0, "distance_meters": 0, "completed": true } ] }
        ]
      },
      "rest": { "workout_id": "04e54b17-2a5b-45c6-ab75-e03bce9aee0e", "deadline_ms": 1792602996616 },
      "receipts": {}
    },
    "summary": {
      "workouts": 1, "completed_sets": 1, "weekly_workouts": 1, "weekly_goal": 3,
      "volume": "400 kg", "active_volume": "412.5 kg", "active_duration": "0m 00s",
      "active_elapsed_seconds": 0, "active_completed_sets": 1
    },
    "records": [
      { "exercise_id": "bench-press", "name": "Bench Press", "weight": "80 kg", "reps": 5, "volume": "400 kg",
        "seconds": 0, "distance_meters": 0, "estimated_1rm": "93.3 kg", "estimated_1rm_grams": 93333 }
    ],
    "chart": [ { "id": "e0926a06-33c1-44cc-9bda-6f82ed157b2e", "label": "21 Oct", "volume": 400.0, "volume_grams": 400000 } ],
    "exercise_charts": {
      "bench-press": [ { "id": "e0926a06-33c1-44cc-9bda-6f82ed157b2e", "label": "21 Oct", "volume": 400.0, "volume_grams": 400000 } ]
    },
    "rest_remaining": 90,
    "weight_labels": { "8ea818e6-5ff6-42c8-8764-9dc104d670b6": "82.5", "ef3b17ab-6f98-4758-beb7-fbf698d9435f": "80" },
    "previous_labels": { "dc24d2a5-edaf-4651-9195-fd9bff73f83e": "80 kg × 5" },
    "measurement_labels": {},
    "workout_labels": {
      "04e54b17-2a5b-45c6-ab75-e03bce9aee0e": { "sets": 1, "volume": "412.5 kg", "duration": "0m 00s" },
      "e0926a06-33c1-44cc-9bda-6f82ed157b2e": { "sets": 1, "volume": "400 kg", "duration": "0m 02s" }
    }
  }
}
```

`state.receipts` is always `{}` in views. Records count finished workouts
only. `weight_labels` covers every set in history and the active workout.

Errors (real):

```
409 {"ok":false,"replayed":false,"error":"State changed. Refresh and retry the edit.","code":"conflict"}
422 {"ok":false,"replayed":false,"error":"Finish or discard your current workout first","code":"invalid_command"}
```

`clock` (real): `{"ok":true,"replayed":false,"clock":{"rest_remaining":90,"active_duration":"0m 00s","active_elapsed_seconds":0}}`.
Tick `active_elapsed_seconds` locally between refreshes.

## Device bridge

The owner's phone and Mac tell Lucina what happens on them, and run the
commands Lucina sends back (see [`device-bridge.md`](device-bridge.md) for
what iOS allows). All three routes need auth; cookie callers need
`X-Secretary-CSRF: 1`. Every body forwarded to Lucina gets
`"device": {"id", "name"}` of the authenticated device; a `device` the
client sends is ignored.

### `POST /v1/device/events`

```json
{ "events": [
  { "id": "6F1C2A3B-…", "ts": 1761048000000, "type": "web_visit", "source": "safari-ext",
    "data": { "domain": "example.com", "title": "Example" } }
] }
```

| Field | Rules |
|---|---|
| `events` | 1–500 events. The whole batch is rejected if one event is invalid. |
| `id` | `[A-Za-z0-9_.:-]{1,128}`, unique per device (a UUID works). The dedup key. |
| `ts` | Epoch milliseconds (positive integer) or an RFC 3339 timestamp with offset. |
| `type` | `web_visit`, `location_visit`, `health_summary`, `workout`, `focus_change`, `screen_threshold`, `calendar_change`, `reminder_change`, `alarm_fired`, `app_open`. |
| `source` | `ios`, `mac`, `safari-ext`, `chrome-ext`. |
| `data` | Optional JSON object (default `{}`), at most 8 KiB serialized. Opaque to the server. |

Other fields are dropped. `200`:

```json
{ "accepted": 12, "duplicates": ["6F1C2A3B-…"], "results": [ { "…": "Lucina's data" } ] }
```

- **Dedup:** the server remembers the last 8192 event ids it forwarded (in
  memory, per device; lost on restart, so Lucina should dedupe by id too).
  Ids seen before, and repeats inside the batch, are dropped and listed in
  `duplicates`. A batch of only duplicates answers `accepted: 0` without
  calling Lucina.
- **Forwarding:** the remaining events go to Lucina's
  `POST /api/secretary/device/events` in order, in chunks of at most 48 KiB
  (Lucina caps bodies at 64 KiB); `results` has Lucina's `data` per chunk.
  An id is remembered only once its chunk was accepted.
- **Offline queue:** keep a batch until it gets a `200`. On any error (for
  example `502` in the middle of a large batch) resend the *same* batch: the
  chunks Lucina already stored come back as `duplicates`.
- Limits: 4.5 MiB body (`413 payload_too_large`), 120 requests/minute per
  device in their own bucket (`429 rate_limited`, honour `Retry-After`).

### `POST /v1/device/commands/{id}/result`

Ack a command Lucina sent as a timeline item (`meta.kind: "device_command"`,
`meta.command: {id, type, args}`):

```json
{ "ok": true, "deviceObjectId": "B7E1…" }
{ "ok": false, "error": "AlarmKit is not authorized" }
```

`ok` required; `error` and `deviceObjectId` optional, 1–500 characters, no
control characters. `{id}` uses the id charset. Forwarded to
`POST /api/secretary/device/commands/{id}/result`; Lucina's answer and 4xx
codes (for example `404` for an unknown or expired command) pass through.

### `POST /v1/device/state`

The device's current alarms, timers, upcoming reminders and calendar
events, and the Focus mode, so Lucina can resolve "delete my 7:00 alarm":

```json
{ "alarms": [ { "id": "…", "time": "07:00", "label": "Gym", "repeat": ["mon", "wed"] } ],
  "timers": [], "reminders": [ { "id": "…", "title": "Latte", "due": "…" } ],
  "events": [ { "id": "…", "title": "Dentista", "start": "…", "end": "…" } ],
  "focus": { "mode": "work" }, "ts": 1761048000000 }
```

Each list is optional (default `[]`), at most 500 JSON objects of at most
8 KiB each (opaque to the server). `focus` is an object, a string or `null`;
`ts` is optional (as for events). Other fields are dropped. Forwarded to
`POST /api/secretary/device/state`; Lucina's answer passes through.

## Insights for Lucina

### `GET /v1/insights/summary?timezone=Europe/Rome&workouts=5`

Read-only, compact JSON for Lucina to reason over. It accepts **only** a
service token: `Authorization: Bearer svc_…`, the token stored in
`SECRETARY_SERVICE_TOKEN_FILE` (create one with `secretary-server
service-token generate <path>`; the file is written 0600 and the token is
never printed). Device tokens and cookies get `401` here, and a service
token gets `401` on every other route except `/v1/avatar*`. Without `SECRETARY_SERVICE_TOKEN_FILE`
the route answers `503 service_token_not_configured`. `workouts` is 1–20
(default 5; values outside are clamped); `timezone` defaults to `UTC`.

Real response (two workouts today, the second a bench PR; the body data
from the example above; `weekly_volume` trimmed to the last two of its 8
weeks):

```json
{
  "generated_ms": 1792602906912,
  "timezone": "Europe/Rome",
  "gym": {
    "unit": "kg", "weekly_goal": 3, "workouts_this_week": 2,
    "recent_workouts": [
      { "id": "04e54b17-2a5b-45c6-ab75-e03bce9aee0e", "title": "Push", "finished_ms": 1792602906695,
        "date": "2026-10-21", "duration_min": 0, "completed_sets": 1,
        "volume_grams": 412500, "volume_label": "412.5 kg", "exercises": ["Bench Press"],
        "prs": [ { "exercise_id": "bench-press", "name": "Bench Press", "weight_grams": 82500, "reps": 5, "label": "82.5 kg × 5" } ] },
      { "id": "e0926a06-33c1-44cc-9bda-6f82ed157b2e", "title": "Push", "finished_ms": 1792602906471,
        "date": "2026-10-21", "duration_min": 0, "completed_sets": 1,
        "volume_grams": 400000, "volume_label": "400 kg", "exercises": ["Bench Press"], "prs": [] }
    ],
    "weekly_volume": [
      { "week_start": "2026-10-12", "workouts": 0, "volume_grams": 0, "volume_label": "0 kg" },
      { "week_start": "2026-10-19", "workouts": 2, "volume_grams": 812500, "volume_label": "812.5 kg" }
    ]
  },
  "body": {
    "unit": "kg",
    "stats": { "count": 5, "current_grams": 80500, "current_measured_ms": 1792602906883, "avg7_grams": 80625,
               "weekly_change_grams": -575, "min_grams": 80400, "max_grams": 81200, "trend30_grams": -575,
               "trend90_grams": -575, "goal_grams": 78000, "goal_distance_grams": 2625, "goal_reached": false },
    "stat_labels": { "avg7": "80.6 kg", "current": "80.5 kg", "goal": "78.0 kg", "goal_distance": "+2.6 kg",
                     "max": "81.2 kg", "min": "80.4 kg", "trend30": "-0.6 kg", "trend90": "-0.6 kg", "weekly_change": "-0.6 kg" }
  }
}
```

Errors: `401 unauthorized` (no or wrong service token, device token,
cookie), `400 bad_request` (unknown `timezone`), `503
service_token_not_configured`, `429 rate_limited` (shared bucket for the
service token).

- `recent_workouts`: newest first. A PR is a completed non-warmup set of a
  weight × reps exercise heavier than every such set of that exercise in all
  earlier workouts (a first-ever performance is not a PR); the heaviest one
  per exercise is listed.
- `weekly_volume`: the last 8 weeks (Monday starts, in `timezone`),
  oldest first, zero-filled.

## Avatar media

### `GET /v1/assets/{name}` (authenticated)

Private files for the 3D Lucina avatar (model, motions, textures, sounds),
served from `SECRETARY_PRIVATE_ASSETS_DIR` (default
`/var/lib/secretary/private-assets`) **only** to authenticated callers
(device Bearer or the session cookie; no CSRF header needed for GET). The
model's licence forbids redistribution, so there is no public URL for
these files, and they are never written to the web build.

- `name`: `[a-z0-9][a-z0-9._-]*`, at most 128 bytes, no `..`, no slashes.
- Extensions and content types: `.glb` `model/gltf-binary`, `.json`
  `application/json`, `.png` `image/png`, `.webp` `image/webp`, `.mp3`
  `audio/mpeg`, `.vmd` `application/octet-stream`, `.usdz` `model/vnd.usdz+zip`
  (the iOS AR model, `lucina.usdz` with its clip table `lucina-ar.json`, built by
  `tools/lucina-usdz/run.sh`). Anything else is `404`.
- `200` with `Cache-Control: private, max-age=86400`, `ETag`,
  `Last-Modified`. `If-None-Match` → `304`. `Range: bytes=…` → `206`.
- `404 not_found` for a missing or refused name; `401 unauthorized` without
  auth.

Browsers can use the URL directly (`fetch("/v1/assets/lucina.glb")`, same
origin, cookie sent automatically). The iOS app sends its Bearer token.

### `POST /v1/tts` (authenticated)

```json
{ "text": "Ciao Giuseppe, come stai oggi?", "voice": "if_sara", "lang": "it", "speed": 1.0 }
```

| Field | Rules |
|---|---|
| `text` | Required, 1–1200 characters (trimmed). |
| `voice` | Optional; one of `SECRETARY_TTS_VOICES` (default `if_sara`, `af_heart`, `af_bella`, `bf_emma`). Wins over `lang`. |
| `lang` | Optional, `it` or `en` (`it-IT` style accepted): `it` → `if_sara`, `en` → `af_heart`. |
| `speed` | Optional, 0.5–2.0, default 1.0. |

Without `voice` and `lang`, the server scores the text: Italian stopwords
(`che, non, per, sono, ciao, il, la, è, un, una, di, ti, mi, come, oggi, …`)
plus accented letters against English stopwords (`the, and, you, is, …`). A
tie or no signal is Italian (`if_sara`); English wins only with a higher
score (`af_heart`). The chosen voice is always sent to Kokoro.

Texts over 300 characters are split on sentence boundaries (then commas,
then spaces) and synthesized chunk by chunk into the same MP3 stream, so
audio starts after the first sentence. The status comes from the first
chunk; if a later chunk fails, the stream ends early.

`200`: the MP3 bytes, `Content-Type: audio/mpeg`, streamed as Kokoro
produces them, plus `X-TTS-Voice: <voice used>`. The server proxies to
Kokoro's OpenAI-compatible API (`POST {SECRETARY_TTS_URL}/v1/audio/speech`
with `{model: "kokoro", input, voice, response_format: "mp3", speed}`;
default URL `http://100.107.164.33:8880`, over Tailscale).

Errors: `400 bad_request` (field rules above), `401`, `403 csrf` (cookie
without `X-Secretary-CSRF: 1`), `429 rate_limited` (30 requests/minute per
device, separate from the general limit; `Retry-After`), `502
tts_unavailable` (Kokoro unreachable or failed), `504 tts_unavailable`
(no answer within 30 s).

```js
const r = await api("/v1/tts", { method: "POST", body: { text } });
const audio = new Audio(URL.createObjectURL(await r.blob()));
await audio.play();
```

## Avatar wardrobe

Lucina's 3D avatar has outfits, hairstyles and hair colours. The owner picks
them in the app (the Wardrobe), lets the look follow the app section, or asks
Lucina in chat ("mettiti il vestito da cameriera", "fatti la coda"). The
choice lives on the server, so every client and Lucina see the same one.

Auth: a device (Bearer, or the session cookie plus `X-Secretary-CSRF: 1` for
the `PUT`) **or** Lucina's service token (`Authorization: Bearer svc_…`).
`X-Secretary-Client: web|ios` on a device write is recorded as `updatedBy`.

### `GET /v1/avatar`

```json
{
  "outfit": null,
  "hair": null,
  "hairColor": null,
  "auto": true,
  "rules": {
    "gym": { "outfit": "gym", "hair": "ponytail" },
    "shopping": { "outfit": "chef", "hair": "natural" },
    "default": { "outfit": "uniform", "hair": "natural" }
  },
  "updatedMs": 1792602906912,
  "updatedBy": "web"
}
```

- `outfit` / `hair`: **pinned** (worn everywhere until unpinned); `null` =
  follow the rules. The hairstyle `natural` is her own hair and is always
  valid.
- `hairColor`: a colour id from the wardrobe; `null` = natural colour. It
  applies to whatever hair shows.
- `auto`: with nothing pinned, the look follows the app section: `rules.gym`
  on the Gym tab, `rules.shopping` on Shopping, `rules.default` anywhere
  else. With `auto: false` she wears `rules.default`.
- `updatedMs` / `updatedBy` (`web` | `ios` | `device` | `lucina`): the last
  change; `0` / `""` until the first one. The values above are the defaults.

Clients resolve the look themselves: `rule = auto ? rules[section] :
rules.default`, then `outfit ?? rule.outfit` and `hair ?? rule.hair`.

### `PUT /v1/avatar`

A patch: any of `outfit`, `hair`, `hairColor` (an id, or `null`), `auto`
(bool), `rules` (any of `gym`, `shopping`, `default`, each `{outfit?,
hair?}`; a bare string is an outfit, the older form). Absent fields keep
their value. Answers the full new settings (`200`).

```json
{ "outfit": "maid" }
{ "hair": "bun", "hairColor": "pink" }
{ "outfit": null, "hair": null, "auto": true }
{ "rules": { "shopping": { "outfit": "maid" }, "gym": { "hair": "bun" } } }
```

- Ids: `[a-z0-9][a-z0-9_-]*`, at most 40 characters. Unknown fields and
  wrong types are refused (`400 bad_request`).
- When the wardrobe lists outfits / hairstyles / colours, the ids must be
  among them: `422 unknown_outfit` / `unknown_hairstyle` /
  `unknown_hair_color` (the message lists the known ids). A kind the
  wardrobe does not list accepts any well-formed id.
- Stored in `$SECRETARY_DATA_DIR/avatar.json` (atomic write, mode 0600). A
  refused write changes nothing. A file from before hairstyles (rules as
  outfit strings) still loads.

### `GET /v1/avatar/wardrobe`

From `{SECRETARY_PRIVATE_ASSETS_DIR}/wardrobe.json`, which is
`{outfits, hairstyles, colors?}` (a bare array is read as the outfits). Only
valid entries are served (file order, first of each id, at most 200 per
list); every list is empty when the file is missing.

```json
{
  "outfits": [
    { "id": "uniform", "name": { "it": "Divisa", "en": "Uniform", "da": "Uniform" },
      "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["uniform"] }
  ],
  "hairstyles": [
    { "id": "natural", "name": { "it": "Naturali", "en": "Natural", "da": "Naturligt" } },
    { "id": "ponytail", "name": { "it": "Coda", "en": "Ponytail", "da": "Hestehale" },
      "glb": "lucina-hair-ponytail.glb", "thumbnail": "hair-ponytail.webp", "hideTiara": true }
  ],
  "colors": [
    { "id": "pink", "name": { "it": "Rosa", "en": "Pink", "da": "Lyserød" }, "hex": "#ff8ac8" }
  ]
}
```

- Outfits: `id`; `glb` (asset name ending in `.glb`, loaded from
  `/v1/assets/{glb}`); `thumbnail` (optional `.png`/`.webp`); `name`
  (`{it?, en?, da?}` or one string, at most 80 characters each); `tags`
  (optional, at most 16). The Wardrobe groups outfits by the first known
  tag: `uniform`, `casual`, `sport`, `work`, `formal`, `fantasy`.
- Hairstyles: the same fields (`natural` needs no `glb` or `name`), plus
  `hideTiara` (bool: hide the tiara while it is on) and `hideMaterials`
  (base-model materials to hide instead of the hair). A part is a separate
  GLB attached to the `head` bone; the base hair is hidden while it is on.
- Colours: `id`, `hex` (`#rrggbb`), optional `name`.
- Other fields pass through unchanged.

### Live changes

When Lucina changes her look she also writes a timeline item with
`meta: {kind: "avatar_outfit", outfit?, hair?}` (`outfit` / `hair` name what
she put on; neither when she went back to automatic). Clients that see one
on the timeline stream refetch `GET /v1/avatar` and apply it with the change
effect. Clients also refetch when they come back to the foreground.

## Notifications

Architecture, iPhone options and privacy: [`notifications.md`](notifications.md).
Device routes take Bearer or the cookie (+ `X-Secretary-CSRF: 1` for writes).

Notification:

```json
{ "id": "n_1792654200000a1b2c3d4", "ts": 1792654200000, "category": "reminder",
  "title": "optional custom title", "body": "Call the dentist", "route": "/",
  "channel": "telegram", "timelineId": "01JA…", "dedupKey": "reminder:42",
  "data": {}, "read": false, "readAt": null }
```

`category`: `lucina_message`, `reminder`, `calendar`, `device_confirm`,
`shopping`, `presence`, `system`. Without `title`, clients title it by category.
`route` is an app path (`/`, `/shopping`, `/gym`, `/weight`).

| Route | |
|---|---|
| `GET /v1/notifications?limit=1..200&before=<id>&unread=0\|1` | `{items, unread, nextBefore}`, newest first (500 kept). |
| `POST /v1/notifications/read {ids: […]}` or `{all: true}` | `{updated, unread}`. |
| `GET /v1/notifications/settings` | `{settings, categories, subscriptions, currentDevice, webPush: {enabled, publicKey}, apns: {enabled, environment}, present}`. |
| `PUT /v1/notifications/settings` | The whole `settings` object (below); `400 bad_request` names the bad field. Answers like `GET`. |
| `POST /v1/notifications/presence {active}` | The app is (not) in front; counts 90 s. Lucina's messages are not notified while any device is present. |
| `GET /v1/push/key` | `{publicKey}`: the VAPID `applicationServerKey` (base64url). |
| `POST /v1/push/subscribe` | `PushSubscription.toJSON()` + `label`, `lang` → `201` subscription view (no secrets). Upserts by endpoint; the endpoint must be HTTPS on a known push service. |
| `DELETE /v1/push/subscribe {endpoint}` | `{removed}`. |
| `DELETE /v1/push/subscriptions/{id}` | `{removed}`; `404 subscription_not_found`. |
| `POST /v1/push/test {id?}` | A test push to that subscription (default: the caller's); waits: `{results: [{id, ok, delivery: {result: sent\|gone\|failed, status?, error?}}]}`. |
| `POST /v1/push/apns {token, lang?}` | Native app device token → `201`; `503 apns_disabled` until APNs is configured. |

Settings:

```json
{ "categories": {"lucina_message": true, "reminder": true, "calendar": true, "device_confirm": true,
                 "shopping": true, "presence": false, "system": false},
  "quietHours": {"enabled": false, "start": "22:30", "end": "07:30"},
  "timeZone": "Europe/Rome", "dndUntil": null, "calendarLeadMinutes": 10,
  "remindersBypassQuiet": true, "digest": true, "previews": true, "language": "en",
  "devices": {"ps_…": {"enabled": true, "categories": {"calendar": false}}} }
```

### `POST /v1/notify` (service token)

Lucina posts a notification:
`{category, body (1–2000), title? (≤120), route?, dedupKey? (≤200), timelineId?, channel?, data? (≤4 KiB)}`
(unknown fields → `400`). `201 {status: "created", notification, pushed, deferred}`,
or `200 {status}` with `duplicate` (same `dedupKey` or `timelineId` seen),
`disabled` (category off) or `present` (a Lucina message while the owner is in
the app). Device tokens and cookies get `401`.

### `GET /v1/notify/settings` (service token)

`{settings, present}`: the settings object above plus whether the app is in
front, so Lucina's proactive engine plans around quiet hours and Do Not
Disturb. No subscriptions or device list.

## Proactive outreach

Lucina's proactive engine (`crates/lucina-proactive`) reaches out with a
timeline item `{role: "lucina", channel: "other", meta: {kind: "proactive",
topic, priority: low|normal|high, outreachId: "po_…", why, actions: [{id,
label, route?, feedback?}]}}` plus a `lucina_message` push whose route is
`/?outreach=<id>`. Telegram copies carry the deep link
`https://getsecretary.tech/?outreach=<id>`. The app renders the item as a
"Lucina reached out" card: content buttons navigate to their `route`, the
feedback buttons (`useful`, `not_now`, `stop`: "Utile / Non ora / Basta così")
post here.

### `POST /v1/proactive/outreach/{id}/feedback` (device)

`{action: "useful" | "not_now" | "stop" | "opened"}` → `201 {feedback: {id,
outreachId, action, at, device, source}}`; the same (outreach, action) again
returns the stored tap with `200`. `400` for another action or an id that is
not `po_` + 26 base-32 characters. Cookie requests need `X-Secretary-CSRF: 1`.

### `GET /v1/proactive/feedback?after=<id>&limit=500` (service token)

The taps after the cursor, oldest first: `{items: [tap], cursor}` (`cursor`
is the last id returned, or `after` when there is nothing new). The engine
pulls this every tick: "Non ora" holds that kind for a few hours, "Basta
così" mutes it, everything is stored for learning.

## Links

The connections hub behind the Links tab (web and iOS). The server computes
every status; clients render cards and send the connect/disconnect requests.
State: `$SECRETARY_DATA_DIR/links.json` (atomic, 0600). Secrets in it (the
Google refresh token, ICS addresses, remote MCP addresses) are sealed with
ChaCha20-Poly1305 under `links.key` (32 random bytes, 0600, created on first
use) and are **never returned**: clients only see a host. Device auth (Bearer
or cookie + CSRF); the service token gets `401` here.

### `GET /v1/links`

```json
{ "timeZone": "Europe/Rome",
  "google": { "configured": true, "status": "connected", "account": "owner@gmail.com", "services": ["calendar"] },
  "groups": [
    { "id": "calendars", "cards": [
      { "id": "google-calendar", "kind": "google", "service": "calendar", "name": "Google Calendar",
        "status": "connected", "account": "owner@gmail.com", "scopes": ["calendar.events", "calendar.readonly"],
        "lastSyncAt": "2026-10-22T09:00:00.000Z", "count": 57, "detail": "57 items synced", "actions": ["sync", "disconnect"] },
      { "id": "iphone-calendar", "kind": "iphone", "status": "connected", "account": "Giuseppe's iPhone", "count": 24, "…": "…" },
      { "id": "ics:ics_Ab3…", "kind": "ics", "name": "Serie A", "status": "connected", "account": "calendar.google.com",
        "count": 38, "scopes": ["read-only"], "actions": ["sync", "remove"] } ] },
    { "id": "channels", "cards": [ "telegram, discord: connected; alexa: planned; lastSyncAt = latest timeline activity" ] },
    { "id": "ai", "cards": [ "ai:claude-code, ai:chatgpt, ai:grok (+ other MCP callers), from secretary-mcp" ] },
    { "id": "google", "cards": [ "google-gmail, google-drive: available | connected" ] },
    { "id": "custom", "cards": [ { "id": "custom:lnk_…", "kind": "bookmark", "url": "https://…", "icon": "🏦", "tags": ["money"] },
                                 { "id": "custom:lnk_…", "kind": "mcp", "status": "saved", "account": "mcp.example.com" } ] } ],
  "proposals": [ <Proposal> ] }
```

Card `status`: `connected`, `disconnected`, `reconnect` (Google refused the
refresh token: show **Reconnect**), `error` (an ICS feed failed; `error` says
why), `planned`, `available`, `unavailable` (Google not configured on the
server), `unknown` (secretary-mcp status not reachable), `saved` (MCP URL).
`actions` ⊂ `connect`, `reconnect`, `sync`, `disconnect`, `remove`, `edit`.
AI client status comes from secretary-mcp's `GET /internal/status`
(`SECRETARY_MCP_STATUS_URL`, bearer `SECRETARY_MCP_STATUS_TOKEN_FILE`).

### Google: `POST /v1/links/google/connect`, `GET /v1/links/google/callback`, `DELETE /v1/links/google`

- `POST /v1/links/google/connect {"services": ["calendar"|"gmail"|"drive"], "client": "web"|"ios"}`
  → `{"authorizeUrl": "https://accounts.google.com/o/oauth2/v2/auth?…"}`. The
  app navigates there (iOS: `ASWebAuthenticationSession`, callback scheme
  `secretary`). Authorization code + PKCE S256, `access_type=offline`,
  `prompt=consent`, `include_granted_scopes=true`. Scopes: calendar →
  `calendar.events` + `calendar.readonly`; gmail → `gmail.readonly`; drive →
  `drive.readonly` (already granted services are kept). `state` and the PKCE
  verifier live in memory only, single use, 10 minutes. `503
  google_not_configured` without the client files.
- `GET /v1/links/google/callback?code&state` (no auth: the `SameSite=Strict`
  cookie is not sent on Google's cross-site redirect; the unguessable state
  is the binding) exchanges the code, reads the primary calendar id (the
  account email), stores the sealed refresh token, starts a sync and answers
  `303` to `/links?google=connected` (iOS: `secretary://links?google=connected`)
  or `?google=error&reason=<code>`.
- `DELETE /v1/links/google` revokes the grant at Google (best effort), drops
  the token and the Google cache: `{"disconnected": true, "revokedAtGoogle": true}`.

### ICS subscriptions: `POST /v1/links/ics`, `DELETE /v1/links/ics/{id}`

`{"label": "Serie A", "url": "webcal://…"}` (any provider's secret iCal
address; `webcal(s)://` becomes `https://`; plain `http`, `localhost` and
private/Tailscale IP literals are refused). The feed is fetched once (`422
ics_unavailable` when it cannot be read or is not iCalendar, at most 10 MB),
then on every sync with `If-None-Match` / `If-Modified-Since`. `201 {id,
label, host, eventCount, feedName}`. Read-only.

### Custom links: `POST /v1/links/custom`, `PATCH|DELETE /v1/links/custom/{id}`

`{"kind": "bookmark"|"mcp", "label": "Bank", "url": "https://…", "icon": "🏦", "tags": ["money"]}`.
Label 1–80, URL http(s) (MCP: https only) ≤ 2048, icon ≤ 16 characters, ≤ 10
tags of ≤ 24 (lowercased, deduplicated), ≤ 200 links. MCP server URLs are
sealed and never called (stored for Lucina's later use); bookmarks return
their `url`.

## Calendar

All sources merged: Google Calendar (synced into `calendar.json` every
`SECRETARY_CALENDAR_SYNC_SECS`, default 300, with sync tokens; a `410 Gone`
triggers a full resync from 365 days back; recurring events are expanded by
`secretary-calendar`, DST-correct, with EXDATEs and per-instance
exceptions), ICS subscriptions, and the iPhone's EventKit events from the
latest `POST /v1/device/state` snapshot (`events: [{id, title, start, end,
allDay?, location?, calendar?, notes?}]`, start/end RFC 3339, epoch ms or
`YYYY-MM-DD`). Auth: device Bearer, cookie (+CSRF on writes), or **Lucina's
service token** (`svc_…`).

Event:

```json
{ "id": "google:abc_20261022T070000Z", "source": "google", "sourceId": "abc", "calendar": "Google",
  "title": "Gym", "start": "2026-10-22T07:00:00.000Z", "end": "2026-10-22T08:00:00.000Z", "allDay": false,
  "startMs": 1792652400000, "endMs": 1792656000000, "location": "…", "url": "https://calendar.google.com/…",
  "recurring": true, "editable": true }
```

All-day events have `start`/`end` as `YYYY-MM-DD` (end exclusive) and
`allDay: true`. Ids: `google:<event or instance id>`, `ics:<sub>:<hash>`,
`iphone:<EventKit id>`.

| Route | |
|---|---|
| `GET /v1/calendar/events?from&to&days&sources` | `from`/`to`: `YYYY-MM-DD` (owner's zone, `to` inclusive) or RFC 3339; default today + 7 days; `days` 1–400; `sources=google,ics,iphone`. `{from, to, timeZone, events, sources}` (`sources` has each source's status). Range ≤ 400 days. |
| `GET /v1/calendar/free?from&to&minutes=60&dayStart=9&dayEnd=19&limit=5` | Gaps between timed events inside working hours: `{slots: [{start, end}]}`. All-day events do not block. |
| `POST /v1/calendar/sync` | Sync now: `{google: {ok, changed?, error?}, ics: [{id, ok, changed?, error?}]}`. `error: "reconnect_needed"` when Google revoked access. |
| `POST /v1/calendar/events` | `{title, start, end?, allDay?, location?, description?, timeZone?}` → Google primary calendar. `start` RFC 3339 with offset (default end +1 h) or `YYYY-MM-DD` (all-day, default +1 day). `201 {created, event}`. |
| `PATCH /v1/calendar/events/{id}` | **`confirm: true` required** (`428 confirmation_required` otherwise) + any of the create fields. Moving `start` without `end` keeps the duration. Google events only (`422 read_only_source` for ICS/iPhone). |
| `DELETE /v1/calendar/events/{id}?confirm=true` | Same rules. Deleting a recurring instance id cancels that instance only. |
| `GET /v1/calendar/proposals` · `POST /v1/calendar/proposals` | Proposals from clients that may not write (the Lucina MCP): body as for create plus `from`; validated, kept 7 days, ≤ 50. |
| `POST /v1/calendar/proposals/{id}/accept` `{confirm: true}` · `DELETE /v1/calendar/proposals/{id}` | The owner accepts (creates the event) or dismisses it **in the app**; the service token gets `403 owner_only`. |

Errors: `409 google_not_connected`, `409 google_reconnect_needed` (show
Reconnect in Links), `404 event_not_found`, `400 google_rejected`, `502
google_unreachable` / `google_error`, `400 bad_request`.

`GET /v1/agenda` also carries `events`: the merged events of that date.


## Tasks

The owner's tasks, synced both ways with Todoist (official API v1: `/api/v1/sync`
with sync tokens, `/api/v1/tasks/completed/by_completion_date`). The engine is
`crates/secretary-tasks` (`$SECRETARY_DATA_DIR/tasks.json`, atomic 0600, an
exclusive lock) on the gym/body command protocol; the server runs the Todoist
sync worker. Callers: the owner's devices (Bearer, or cookie + CSRF for writes)
or Lucina's service token.

### Connecting Todoist (Links)

- `GET /v1/links` has a `tasks` group with one card: `{id: "todoist", kind:
  "todoist", status: connected|reconnect|error|disconnected, account,
  lastSyncAt, count, method: token|oauth, oauth: bool, actions}`. Actions:
  `token` (paste the personal API token), `connect`/`reconnect` (Todoist's
  OAuth, only when `oauth` is true), `sync`, `disconnect`.
- `POST /v1/links/todoist/token {token}` → `{connected, account}`. The token is
  checked with `GET /api/v1/user` first: 422 `todoist_token_rejected` when
  Todoist refuses it, 502 `todoist_unavailable` when Todoist is down.
- `POST /v1/links/todoist/connect {client?: web|ios}` → `{authorizeUrl}` (503
  `todoist_oauth_not_configured` without `SECRETARY_TODOIST_CLIENT_ID_FILE` and
  `_SECRET_FILE`); Todoist returns to `GET /v1/links/todoist/callback` (public;
  single-use `state`), which redirects to `/links?todoist=connected|error`
  (`secretary://links?…` for iOS).
- `POST /v1/links/todoist/sync`, `DELETE /v1/links/todoist` (OAuth tokens are
  revoked at Todoist; the synced data is dropped; local-only tasks stay).
- Tokens are sealed with `links.key` (purposes `todoist-token`,
  `todoist-refresh`) and never returned. Owner devices only (not the service
  token).

### `GET /v1/tasks/state?view=&id=&query=&days=&timezone=`

`view`: `today` (default; an `overdue` group then today's date), `upcoming`
(`overdue` + one `date` group per day, `days` default 7), `inbox`, `project`
(`id` = project id; groups per section, sub-tasks nested in `children`),
`label` (`id` = label name), `filter` (`id` = saved filter, or `query` = a
Todoist filter: `today`, `overdue`, `no date`, `recurring`, `p1`…`p4`,
`7 days`, `@label`, `#Project`, `##Project`, `/Section`, `search: x`,
`due before: <date>`, `&`, `|`, `!`, `( )`, `,`; other words are reported in
`list.warning`), `search` (`query`), `completed` (`days`, default 7).
`timezone` defaults to the owner's (`SECRETARY_TIMEZONE`).

Answer: `{ok, replayed, view}` with `view = {revision, today, timezone,
counts: {inbox, today, overdue, upcoming}, goal: {daily_goal, completed_today,
reached}, inbox_id, projects: [{id, name, color, parent_id?, inbox, favorite,
count, sections: [{id, name}]}], labels: [{id, name, color, favorite,
count}], filters: [{id, name, query, color, favorite, count}], sync: {status:
disconnected|ok|syncing|error|rate_limited|reconnect, connected, account?,
last_sync_ms?, pending, error?, retry_after_ms?}, conflicts: [{id, task_id?,
content, kind, message, at_ms}], undo: [task ids inside the delete undo
window], list: {kind, id?, title, query?, count, warning?, groups: [{key,
kind: overdue|date|section|none|completed, title, date?, rel_days?, tasks}]}}`.

A task: `{id, content, description, project_id, project_name, project_color,
section_id?, section_name?, parent_id?, parent_content?, priority: 1 (urgent)
… 4, due?: {date, time?, string, is_recurring, rel_days, overdue,
provisional?}, deadline?, labels, checked, subtasks, subtasks_done, pending,
duration_minutes?, children?, completed_ms?}`. `id` is Todoist's, or `tmp-…`
until Todoist confirms a new task. `due.string` is Todoist's recurrence text,
kept verbatim; `provisional` marks this engine's guess of the next date after
completing a recurring task (Todoist's answer replaces it at the next sync).

### `POST /v1/tasks/command`

`{op_id, expected_revision?, timezone?, view?, command}`. `expected_revision`
is optional here (when given it must match: 409 `conflict`); each command
targets one task, so offline queues and Lucina send none. `view` (`{kind, id?,
query?, days?}`) picks the list in the answer. Same receipts as gym/body
(`replayed`, 422 `op_id_reused`), 422 `invalid_command` with `error`.

Commands (`type`): `quick_add {text, lang?, project_id?, section_id?,
default_due?}` (Todoist quick add: dates and recurrence in it/en/da such as
"tomorrow 9am", "ogni lunedì", "hver dag", "every 2 weeks"; `#Project`,
`/Section`, `@label`, `p1`…`p4`, `{deadline}`, ` // description`; answers
`task_id` and `parsed`), `add {content, description?, project_id?, section_id?,
parent_id?, due_string? | due_date?, lang?, priority?, labels?}`, `complete
{task_id}` (a recurring task moves to its next date on Todoist), `uncomplete
{task_id}`, `update {task_id, content?, description?, priority?, labels?,
due_string? | due_date? | clear_due?}`, `reschedule {task_id, date}` (YYYY-MM-DD
or a phrase; keeps the time and the recurrence), `reschedule_overdue {date?}`
(default today), `delete {task_id}` (soft: hidden at once, sent to Todoist after
20 s), `undo_delete {task_id}` (also after it reached Todoist: re-created),
`move {task_id, project_id | section_id | parent_id}`, `set_daily_goal {goal}`,
`dismiss_conflict {conflict_id}`. Queries: `snapshot {view?}`,
`parse_quick_add {text, lang?}` (answers `parsed`, no view; no op_id needed).

### Sync

Every edit is applied at once and queued as a Todoist sync command (its `uuid`
makes Todoist idempotent too). The worker runs every
`SECRETARY_TODOIST_SYNC_SECS` (90 s), 1.5 s after an edit and when a delete's
undo window ends: pull (incremental sync token; full the first time or after a
rejected command) → push the queue (≤ 100 commands) → completed history (every
10 min). Conflict policy: an edit carries the task's Todoist `updated_at`; if
Todoist's copy changed since, Todoist wins, the edit is dropped and a
`changed_remotely` conflict is listed (also `deleted_remotely`,
`completed_remotely`, `rejected`, `recurrence_lost`). 401/403 → the card shows
`reconnect` and edits keep queueing; 429 → `rate_limited` until Retry-After;
other failures back off up to 15 minutes. `POST /v1/tasks/sync` runs a cycle
now (409 `todoist_not_connected` / `todoist_reconnect`, 429 with Retry-After).

### `GET /v1/tasks/signals?timezone=`

For Lucina and the proactive engine (also `tasks` in `/v1/insights/summary`):
`{today, timezone, today_open, overdue_open, completed_today, daily_goal,
goal_reached, load_minutes, unestimated, today_tasks: [TaskRef], overdue:
[TaskRef] (longest overdue first), routines: [{id, content, recurrence,
due_date, days_overdue, streak_days, done_last_7, last_completed_ms}],
sync_status, pending_edits, conflicts}` with `TaskRef = {id, content, project,
priority, due_date, time?, recurring, days_overdue, minutes?}`. Minutes come
from Todoist durations or the title ("15 min shadowing").

## Notes

Briefings, notes and documents the owner reads in the app (Notes view on
web and iOS) and that Lucina recalls by name. ChatGPT, Grok and Claude Code
save them through Lucina MCP (`note_save`, `note_append`), Lucina through
her service token, the owner in the app.

Auth: a device (Bearer, or the session cookie plus `X-Secretary-CSRF: 1` for
writes) **or** Lucina's service token. Every write is logged (id, slug,
size, source, caller; never the content). Bodies on these routes may be up
to 768 KiB (a note holds at most 256 KiB of text; JSON escaping can double
it).

A note:

```json
{
  "id": "n_3f9c0a7b12de45a8c901",
  "slug": "craftit-meeting",
  "title": "CRAFT-IT4SD — meeting with Marianne & Jonas",
  "content": "# Briefing\n\n…markdown…",
  "tags": ["work", "aarhus"],
  "source": "chatgpt",
  "pinned": true,
  "createdAt": "2026-10-22T11:30:00.123Z",
  "updatedAt": "2026-10-22T11:30:00.123Z"
}
```

- `slug`: unique among live notes, `[a-z0-9]` then `[a-z0-9-]` (no `--`, no
  trailing `-`), at most 64; not `trash`, `new` or `search`. Wherever a route
  takes `{key}`, it is the id or the slug (case-insensitive), so
  `/v1/notes/craftit-meeting` works.
- `content`: markdown, at most 256 KiB (UTF-8), no control characters other
  than newline and tab; `\r\n` becomes `\n`. Clients render it without raw
  HTML and only follow `http(s)`/`mailto` links.
- `tags`: at most 16, each at most 32 letters, digits, spaces, `-` or `_`;
  stored lower-case, a leading `#` dropped, duplicates removed.
- `source`: who wrote it, `[a-z0-9][a-z0-9-]*` (`owner`, `lucina`, `chatgpt`,
  `grok`, `claude-code`, …). Default: `lucina` for the service token,
  `owner` otherwise.
- `updatedAt` has millisecond precision and doubles as the version for
  `baseUpdatedAt`.

### `GET /v1/notes?q=&tag=&limit=200`

`{"notes": [NoteSummary]}`: pinned first, then the most recently updated. A
summary is the note without `content`, plus `size` (bytes) and `excerpt`
(the start of the content, or the text around the first match of `q`).
`q` (at most 200 characters) looks in the title, slug, tags and content,
case- and accent-insensitive; `tag` keeps notes with that tag. `limit`:
1-500.

### `POST /v1/notes`

`{title, content?, slug?, tags?, source?, pinned?}` → `201` and the note.
Without `slug`, one is made from the title (`-2`, `-3`, … when taken). An
explicit slug that is taken answers `409 slug_taken` (update or append to
that note instead). At most 2000 live notes (`409 notes_full`).

### `GET /v1/notes/{key}`

The note, or `404 note_not_found`.

### `PATCH /v1/notes/{key}`

Any of `{title, content, slug, tags, pinned}`; absent fields keep their
value. Optional `baseUpdatedAt`: the `updatedAt` the client edited; when
the note changed since, `409 conflict` and nothing is saved. Optional
`source` names who edited (logged only). Unknown fields are refused.

### `POST /v1/notes/{key}/append`

`{content, separator?}`: adds `content` at the end exactly as given (for
documents sent in chunks); `separator` (at most 8 characters, e.g.
`"\n\n"`) goes between the old text and the new one when the note is not
empty. Over 256 KiB in total: `413 note_too_large`.

### `DELETE /v1/notes/{key}`

Moves the note to the trash: `{deleted: true, id, slug, deletedAt,
purgeAfter}`. The slug is free again at once.

### `GET /v1/notes/trash`

`{"notes": [NoteSummary + deletedAt, purgeAfter], "days": 30}`, newest
deletion first. Notes deleted more than 30 days ago are purged here and on
every delete.

### `POST /v1/notes/trash/{id}/restore`

Brings the note back (by id) and answers it. If a live note took its slug
meanwhile, the restored one gets `-2` (or the next free number).

### Seed notes

`secretary-server notes import <file|dir>… [--replace]` (as the service
user, with `SECRETARY_DATA_DIR`) imports `.md` files with a front matter
block (`slug`, `title`, `tags: a, b`, `source`, `pinned`) or `.json` files
(`{slug, title, content, tags?, source?, pinned?}`). A slug that exists is
kept unless `--replace`. `secretary-server notes list` prints id, slug,
size, source and title. The running server picks imported notes up on its
next request.

## Library

Books and clippings imported from a read-only Kindle USB scan. Ownership,
device presence, mentions and reading status are independent: importing a book
never sets it to started or completed. DRM-protected files contribute only
unencrypted header metadata and a fingerprint; text is never decrypted.
**KFX CONT/ION entity decoding is deferred**: no container/symbol-table reader
is implemented. KFX stays `drm: unknown`, using filename metadata only; no DRM
wrappers are decoded. Undocumented sidecars are not parsed. Sidecar extensions
appear in scan JSON. Topaz (`azw1`, `tpz`) likewise uses filename metadata and
unknown DRM. MOBI names and EXTH strings honor header encoding 1252 or 65001;
unsupported encodings produce a scan warning and filename fallback.

Auth: a device (Bearer, or session cookie plus `X-Secretary-CSRF: 1` for writes)
**or** Lucina's service token. Writes log the caller, never book/highlight content.
Library request bodies may be up to **32 MiB**. Invalid JSON or metadata returns
`400 bad_request` (syntax errors: `invalid_json`); oversized bodies return 413.

Storage under `$SECRETARY_DATA_DIR/library/`: `books/<id>.json`,
`highlights/<bookId>.json` (array), `activity.json` (explicit owner changes and
sessions), `notes/<bookId>.json` (Obsidian note references, only for books that
have some), `notes-index.json` (last index per vault), and `imports.jsonl`
(time, caller, source, counts only). JSON writes
are atomic with mode 0600; a temporary `pending.json` snapshot allows recovery
of interrupted multi-file writes. Records load lazily under a mutex.

A book has `id` (`bk_` + 16 lowercase hex digits), `title`, `authors`, nullable
`isbn`/`doi`/`asin`, `fingerprints` (SHA-256), `formats`, `sources`, `drm`
(`none|protected|unknown`), `owned`, nullable `onKindle`
(`{path,sizeBytes,lastSeenAt}`), nullable `removedFromKindleAt`, `mentionedOnly`,
`kind` (`book|dictionary|document`, default `book` for older records),
`status` (`unknown|not_started|reading|completed|abandoned`), `statusSource`,
nullable `progress` (`{percent,location,page,source,at}`), `lastOpenedAt`, `tags`,
`briefs`, `relatedNotes`, `memoryRefs`, `createdAt` and `updatedAt`.

Unknown values stay null/unknown. A clipping without a matching file creates
an unowned book with no Kindle presence. Dates from clippings lack a timezone:
parsed `addedAt` is an ISO local datetime, and `addedAtRaw` always preserves the
original. Unparseable dates remain null. No serial numbers are collected;
`device` is an optional user-supplied friendly label.

### `GET /v1/library/books?q=&status=&onKindle=&kind=book,document&limit=200`

`{"books":[BookSummary]}`, newest updated first, ties by id. `q` searches
case/diacritic-insensitive terms in titles, authors, tags, highlight text and
notes. `status` takes one of the values above; `onKindle=true|false` filters
presence. `kind` is a comma-separated list of `book`, `document`, `dictionary`;
omitting it defaults to `book,document`. Invalid kinds return 400. Dictionaries
remain addressable by id and with `kind=dictionary`.
`limit` is clamped to 1–500; queries have at most 200 characters.
A summary contains `id,kind,title,authors,status,progress,onKindle,owned,formats,drm,
highlightCount,lastHighlightAt,updatedAt`.

### `GET /v1/library/books/{id}`

`{"book":Book,"highlights":[Highlight],"sessions":[ReadingSession]}`.
Unknown ids return `404 book_not_found`. Highlights contain `id`, `bookId`,
`kind` (`highlight|note|bookmark`), `text`, nullable attached `note`, `location`
(`{start,end}`), `page`, `addedAt`, `addedAtRaw`, `source`, `provenance`
(`{file,entryIndex,device}`), and `dedupKey`. Notes remain separate records even
when attached to a highlight ending at the same location.

### `PATCH /v1/library/books/{id}`

Any of `{status,tags,progress,baseUpdatedAt}`. Absent fields keep their value.
Status edits set `statusSource:"owner"`; progress edits set `source:"owner"`
and a server timestamp. Percent must be 0–100. Tags use the Notes limits
(16 tags, 32 characters each). A stale `baseUpdatedAt` returns `409 conflict`.
Imports preserve owner status, progress and tags. Explicit status/progress
edits are retained for recent-reading reports.

### `GET /v1/library/books/{id}/highlights?q=&limit=200`

`{"highlights":[Highlight]}`, filtered by terms in text and attached notes.
Same query and limit bounds as the book list.

### `GET /v1/library/search?q=&limit=200`

`{"results":[{bookId,title,authors,highlightId,snippet,location,page,source}]}`.
Title/author/tag matches rank before highlight matches. Nullable reference
fields are null on book hits. Optional `kind` filters results with the same
comma-separated values as the list; search includes dictionaries when omitted.
Sources are explicit (`library`,
`kindle-clippings`, …); page and location are never invented.

### `GET /v1/library/recent?days=7&limit=200`

`{"highlights":[Highlight],"changes":[{bookId,status,progress,at,source}],
"sessions":[],"readingSessions":"unavailable_on_stock_kindle",
"unknownDateHighlightsExcluded":true,"kindleDatesHaveNoTimezone":true}`.
`days` is 1–3650. Highlights use `addedAt` when known, newest first; unknown
dates are excluded. Local Kindle dates are compared against the window's UTC
wall-clock boundary without claiming a timezone. Changes are explicit owner
edits, newest first. Stock Kindle USB storage provides no session producer:
reading time and last-opened time remain unknown.

### `GET /v1/library/stats`

`{books,dictionaries,byStatus,byFormat,byDrm,highlights,booksOnKindle,lastImport}`;
`books`, `booksOnKindle` and the status/format/DRM maps exclude dictionaries
(documents are included). `dictionaries` reports their separate count. Highlights
count all records. Count maps use canonical enum values. `lastImport` is the
latest import log record or null.

### `POST /v1/library/import`

Body: `{source,device,scannedAt,books:[ScannedBook],clippings:[ClippingEntry]}`.
`lucina-ink scan <mount> --json` produces this JSON. A scanned book contains
`path,sizeBytes,fingerprint,format,kind,metadata,sidecarPresent,sidecarExtensions`;
metadata contains `title,authors,publisher,isbn,asin,cdetype,language,drm,dictionary`.
The scanner sets `dictionary` when EXTH 531/532 is present and classifies the
scanned book as `kind:dictionary`; files under `documents/dictionaries/` also
have dictionary kind. PDOC personal documents and PDF/TXT/MD files are documents;
other files are books. Older batches without `kind` default to book. A clipping entry contains
`title,authors,highlight` (the Highlight shape above; ids and dedup keys are
recomputed). Titles are bounded to 1000 bytes, highlight/note text to 256 KiB;
paths must be relative without `..`, fingerprints 64 lowercase hex digits,
sources `[a-z0-9][a-z0-9-]*` up to 32 bytes. Capacity: 50k books, 200k highlights.

Returns `{booksAdded,booksUpdated,booksUnchanged,highlightsAdded,
highlightsSkipped,removedFromKindle,conflicts,warnings}`. Identity priority:
fingerprint, ISBN, ASIN. A file can also promote an unowned clipping-only record
using the matching rules below. Distinct files are **never** merged merely because
their titles match; collisions are reported in `conflicts`. Existing ids never
change; conflicting identifiers keep the existing value and are reported.

Clipping matches use a dedicated title key, without altering display titles:
strip balanced trailing parenthesized edition/series groups, cut at the first
`:` / `_ ` / ` - ` separator, then lowercase, remove diacritics and punctuation,
collapse spaces and remove a leading article (including `L'`/`L’`). A period is
not a subtitle separator. Author keys lowercase, remove diacritics/punctuation
and collapse spaces, using the first author. A unique title-key + author-key
candidate wins; otherwise a title-key candidate wins only when exactly one
exists. Ambiguity creates an unowned clipping-only record.

Canonical `dedupKey` (UTF-8, no Rust Debug output):
`<titleKey>|<authorKey>|<kind lowercase>|<start>-<end or start>|<normalised text>`.
Unknown author/location segments are empty; a known single location repeats
start as end. Quoted text retains case, accents and punctuation and collapses
whitespace only. Highlight id = `hl_` + the first 16 lowercase SHA-256 hex digits
of this exact key. For example,
`great gatsby|fitzgerald f scott|highlight|170-172|A thought.` has the pinned id
`hl_8c6f78e9e5e5c21e`. This replaces the initial experimental Debug-based keys before
real-data storage; no migration of old highlight ids is provided.
Replaying a saved batch leaves records and `updatedAt` unchanged.
A complete `kindle-usb` inventory clears missing Kindle presence and sets
`removedFromKindleAt`; books and highlights are never deleted. This slice models
one Kindle inventory, not independent per-device inventories.

### `GET /v1/library/sync`

`{"imports":[…],"books":N,"highlights":N,"serverClock":N,"devices":{device:N},
"pendingTombstones":[Tombstone],"recentConflicts":[Conflict]}`. Last 20 import log
records, oldest first; logs contain counts and caller/source, never content.
`serverClock` counts the device ops recorded; `devices` maps each device id to its
largest contiguous applied counter; `recentConflicts` holds the 20 newest
conflicts, newest first.

### `POST /v1/library/sync`

Local-first sync for reader devices (crate `ink-sync`; the future native reader,
Mode B). A device appends every op durably to its own log (`ink_sync::outbox`,
JSONL + fsync) before any network I/O, then sends unacked ops oldest first, in
batches, retrying with exponential backoff and jitter. It never drops an unacked
op and never re-sends an acked one after a restart.

Body: `{"device":"kobo-1","ops":[Op]}`, at most 500 ops, every `opId` of that
device (else `400`). Device ids are 1–64 characters `[A-Za-z0-9_.-]`.
An op is `{"opId":"<device>:<counter>","at":RFC3339,"type":…,…}`; counters start at 1
and only grow (the client never reuses one). Types:

| `type` | fields | rule |
|---|---|---|
| `setProgress` | `bookId, anchor?, percent?, location?` | last writer wins by `(at, device, counter)`; an owner `PATCH` of progress sets a floor: device ops stamped at or before it lose |
| `setStatus` | `bookId, status` (not `unknown`) | last writer wins by `(status = completed, at, device, counter)`: a device never moves a book back from `completed` (`protected`); an owner `PATCH` sets a floor like progress, and an owner `completed` freezes the status against every device op. Only the owner moves a book back from completed |
| `addHighlight` | `bookId, kind? (highlight), text, anchor, note?` | grow-only set keyed by `device|bookId|kind|anchor|text` (whitespace collapsed); id `hl_` + 16 hex of SHA-256 of that key; the earliest op names `addedAt` |
| `editNote` | `bookId, highlightId, note` (`null` clears) | last-writer-wins note per highlight id, device or Kindle highlights; a note also survives Kindle re-imports |
| `deleteHighlight` | `bookId, highlightId` | recorded as an unconfirmed tombstone; **nothing is removed** until the owner confirms |
| `startSession` / `endSession` | `bookId, sessionId, location?` | session start is the earliest, end the latest; sessions appear in `GET …/books/{id}` with `source:"device:<id>"` |

Response: `{"acked":[opId],"conflicts":[Conflict],"serverClock":N}`. Every op the
server recorded is acked, including duplicates, ops that lost, protected ops and
invalid ops (rejected once, reported, never retried). An op far ahead of the
device's applied counters (more than 1024 out of order) is not acked; the client
retries it later. Apply is idempotent by op id (a replay changes nothing) and
commutative, so two servers that apply the same ops in different orders hold the
same content. A `Conflict` is `{opId,bookId,opType,kind,winner,detail}` with `kind`
`superseded | protected | rejected | deleted`; conflicts are reported per request
and kept in a bounded log (1000). Projected values carry `source:"device:<id>"`
(progress, status) or `source:"device-sync"` (highlights). Ops on a book not in
the library are kept and appear once the book is imported.

Storage: `library/sync.json` (0600, atomic) holds the per-device watermarks (the
largest contiguous counter plus a sparse set of out-of-order counters), the
registers, tombstones and the conflict log. It is written before the book
records it projects onto; loading re-projects it, so a crash between the two
writes heals itself. Times are device clocks: a device with a fast clock wins
last-writer-wins ties against others.

### `POST /v1/library/sync/tombstones/{opId}/confirm`

The owner confirms a device's `deleteHighlight`: the highlight is hidden from the
library and stays hidden (Kindle re-imports and later `addHighlight` ops do not
bring it back; such ops are reported as `deleted` conflicts). Device auth only:
Lucina's service token gets `403 owner_required`. Unknown op ids return
`404 tombstone_not_found`. Returns `{"tombstone":Tombstone}` with
`{opId,bookId,highlightId,at,confirmed}`.

### `POST /v1/library/books/{id}/brief`

Creates and returns `{id,kind:"extractive",text,generatedAt,model:null,basedOn}`.
Uses the first ten highlights by position (unknown positions last), with
highlight ids as references. Repeating against the same highlight set returns
the stored brief. Briefs are always generated, never the owner's words.
**Model briefs are not implemented.** Memory refs have no automatic producer
yet. `relatedNotes` is filled only by the Obsidian notes index below; empty
means none indexed, not "no relationship".

### `POST /v1/library/notes-index`

Body: `{vault,generatedAt,refs:[BookRef]}`, produced by
`lucina-ink obsidian-index <vault> --library <file> --json` (or posted directly
with `--server … --token-file …`). A BookRef is
`{bookId,notePath,noteTitle,how,excerpt,line}`: `how` is one of
`frontmatter|wikilink|mention|isbn|asin`, `excerpt` at most 240 characters of
the matching line, `line` 1-based. `vault` is 1–64 ASCII letters, digits,
spaces, `.`, `_` or `-` (first character alphanumeric); `notePath` a relative
`.md` path without `..`; titles at most 1000 bytes; at most 200k refs.
Invalid input returns 400.

The index **replaces** every reference of that vault (other vaults are kept),
so posting the same refs again is idempotent: books and their `updatedAt` do
not change, and `relatedNotes[].linkedAt` keeps the first time a note was
linked. References to unknown book ids are counted and dropped. Each book's
`relatedNotes` holds one `{vault,path,title,linkedAt,how}` per note, with the
strongest kind (frontmatter > isbn > asin > wikilink > mention). Returns
`{vault,refsStored,booksLinked,booksUpdated,unknownBooks,changed}`; the import
log records caller, `source:"obsidian"` and the counts, never paths or text.

A reference is a relation found in the owner's notes. A `mention` (a title in
plain text) is never evidence that a book was read and never changes status
or progress.

### `GET /v1/library/books/{id}/notes?limit=200`

`{"bookId","total","notes":[NoteLink],"indexedVaults":[{vault,generatedAt,indexedAt,refs,books}]}`.
A NoteLink is `{vault,bookId,notePath,noteTitle,how,excerpt,line}`, sorted by
vault, path, kind and line. Unknown ids return 404. Empty `indexedVaults` means
no vault has been indexed yet, so relationships are unknown.

The `lucina-ink` CLI also supports `import <mount> --server <url> --token-file
<path>` and `import-json <batch.json> --server <url> --token-file <path>`.
`--dry-run` merges against an empty local state and prints the outcome without
sending or reading a token. Tokens are read from files, never passed as argv
values. Scans never write to the device, follow symlinks, or parse sidecars;
they cap depth at 8, total entries at 50k, and clippings at 64 MiB/200k entries.
Filename fallback removes trailing `_B0…` ASINs or ISBN-10-style ids (stored as
ASIN when header metadata lacks one), removes 32-character base32 personal ids
without making them ASINs, and reverses `_ ` to `: `. Every clipping title drops
invisible formatting characters, including repeated per-entry BOMs.

The human scan summary reports marker names and booleans `jailbreak`, `kual`,
`mrpi`, `otaBlocker`, `koreader`. Detection checks names only, including
`winterbreak.log`, `jb.sh`, `patchedUks.sqsh`, `mkk/`, `developer.keystore`,
`documents/KUAL.kual`, `extensions/KUAL.cfg`, `extensions/MRInstaller/`,
`extensions/renameotabin/`, `koreader/`, `libkh/`. macOS litter is ignored
throughout: `._*`, `.DS_Store`, `.Spotlight-V100/`, `.fseventsd/`, `.Trashes/`.

`lucina-ink obsidian-index <vault> --library <file>` indexes an Obsidian vault
**read-only**. `--library` takes a saved ImportBatch (ids as an empty server
would assign them), a library state snapshot, or a `{"books":[…]}` listing such
as `GET /v1/library/books`. Default output: counts, then one
`<bookId> <how> <path>#L<line>` per reference (warnings on stderr); `--summary`
prints counts only; `--json` prints the notes-index body with excerpts.
`--vault-name` sets the citation name (default: the folder name),
`--include`/`--exclude` take patterns, `--no-default-excludes` drops the
privacy defaults, and `--state <file>` (outside the vault) keeps a manifest of
`path → (mtime, size, sha256)` so only changed notes are read again.

The walk never follows symlinks, rejects paths whose canonical form leaves the
vault, caps 20k files, 2 MiB per note and depth 32, and skips non-UTF-8 notes
with a warning. Excluded by default: `.obsidian/`, `.trash/`, `.backups/`,
`.git/`, `Templates/`, `_tmp*` folders and files, `._*`, `.DS_Store`, and any
folder named `Private`, `private`, `Journal`, `journal`, `Diary` or `diary`.
Patterns: `name/` is a folder at any depth, `a/b/` a folder path from the root,
`name` a file name, `a/*.md` a path; `*` stays within a segment, `**` crosses.

Parsing: frontmatter `key: value`, flow `[a, b]` and block `- item` lists (nested
maps and block scalars are ignored); title from frontmatter `title`, the first
`# ` heading, or the file name; tags from frontmatter and inline `#tag`; links
`[[T]]`, `[[T|alias]]`, `[[T#Heading]]`, embeds `![[…]]`, and Markdown links to
`.md` files, outside code. Links resolve like Obsidian: exact path, then a
unique path-suffix or basename match, else unresolved/ambiguous. Book
references: frontmatter `lucina_book_id`, `isbn`, `asin`, `book`/`books`, and
`title` + `author`/`authors` (a note about the book; links to it count as
wikilinks to the book); wikilink targets or aliases naming a title; plain-text
title mentions; checksum-valid ISBN-10 (after an `ISBN` label) or ISBN-13, and
`B0…` ASINs, matched against library identifiers. Titles use the clipping
title and author keys above. A title needs a key with at least two significant
words (3+ letters, not a function word), or must equal the main or full title
for frontmatter/wikilinks; plain-text mentions of single-word titles are never
matched, and weak multi-word titles must appear exactly as written. Ambiguous
titles match nothing. A mention is dropped when the same note refers to the
book more strongly.

`ink_obsidian::render` can render an opt-in generated book note
(`Books/<Title>.md`, frontmatter `lucina_book_id,title,authors,status,source,
progress,generated_by: lucina-ink`, highlights with Kindle locations) and plan
a write: create when absent; otherwise replace only the region between
`<!-- lucina-ink:begin -->` and `<!-- lucina-ink:end -->` of a note we generated;
anything else needs the owner's approval. **No code path writes to a vault
yet.**

Lucina MCP exposes `library_list_books`, `library_search`, `library_get_book`,
`library_get_progress`, `library_get_highlights`, `library_get_recent_reading`,
`library_get_brief`, `library_find_related_notes`, `library_sync_status` (read
scope), and `library_generate_brief` (write scope). Results cite book/page/
location/source and say when progress, sessions or relationships are unknown.
`library_find_related_notes` reads `/v1/library/books/{id}/notes` and cites each
note as `[obsidian:<vault>/<path>#L<line>]`, stating that mentions are not
reading evidence and saying plainly when no vault has been indexed.

## Secrets ("Add secret")

The owner stores secrets and creates projects in Infisical from the apps. **Device login only**
(Bearer device token, or the browser session with `X-Secretary-CSRF: 1`); Lucina's service token
gets 401. Every call signs one single-use **owner grant** (Ed25519, key
`$SECRETARY_DATA_DIR/secret-grant.key`, 0600, made on first use; lifetime 90 s) naming exactly the
action, and forwards it to Lucina (`POST /api/secretary/secrets/grant`), which relays it to
infisical-brokerd. The broker trusts only this server's public key (`[owner_grants]` in
`broker.toml`). Values are never logged, stored or put in a grant (it carries the sha256).

| Route | Body / query | Answer |
|---|---|---|
| `GET /v1/secrets/status` | | `{grantKey, audience}`: the public key for broker.toml (also `secretary-server secret-grant-key`) |
| `GET /v1/secrets/catalog` | | `{projects: [{id, name, slug, alias, environments: [{slug, name}]}]}`: every secret-manager project the broker's owner identity sees, names only |
| `GET /v1/secrets/names` | `project, environment, path` | `{folders: [name], secrets: [{key, version, updated_at}]}` at that folder, no values |
| `POST /v1/secrets` | `{name, value \| held, project, environment, path?, overwrite?, createFolder?, confirmed: true}` | `{key, project: {id, name}, environment, path, action: created\|updated, version, fingerprint, receipt}` |
| `POST /v1/secrets/projects` | `{name, slug?, confirmed: true}` | `{project: {id, name, slug, environments}, access, receipt}`. No access for the agents or services identities. |

`name`: `[A-Za-z0-9_.@-]`, 1-256. `path`: folders of the same characters. `value`: 1-65536 bytes.
`held`: a value Lucina holds after the owner typed `/secret` or pasted a token in chat (410
`held_expired` after 10 minutes). Errors: 400 `bad_request` (never echoes the value), 403
`grant_*`, 404 `unknown_project`/`unknown_environment`, 409 `exists` (send `overwrite: true` after
the owner saw the name), 502 `upstream_unauthorized` (the owner identity's org role cannot create
projects), 503 `secrets_unconfigured` / `broker_unavailable`.

Lucina writes the receipt to the timeline (`meta.kind: secret_receipt`): "Saved `NAME` in project /
env / path (fingerprint xxxxxxxx)". In chat, `/secret …` never reaches her model, timeline, Honcho
or replay cache, and token-shaped text is masked before the turn and offered as a held value.

## Models

The owner's private 3D gallery: models Lucina made with Meshy AI (crate
`lucina-meshy` on the Rust Lucina), each with its prompt, Meshy task ids,
clips, credits and files. Lucina adds models with her service token; the
apps list, view and delete them. Files are never served without auth.

| Route | Who | |
|---|---|---|
| `GET /v1/models?limit=50&q=` | device, Lucina | `{models: [Model]}`, newest first; `q` searches title, prompt, animations |
| `GET /v1/models/{id}` | device, Lucina | `{model}` |
| `DELETE /v1/models/{id}` | device (cookie + `X-Secretary-CSRF: 1`), Lucina | `{ok, id}`; the model and its files are gone |
| `GET /v1/models/{id}/files/{name}` | device, Lucina | the file; `Cache-Control: private, max-age=3600`, `ETag`, `Range` |
| `POST /v1/models` | Lucina only (403 for devices) | 201 `{model}` |
| `PUT /v1/models/{id}/files/{name}` | Lucina only | raw body → `{model}` |

`Model`: `{id: "mdl_…", title, prompt, source: "meshy"|"upload", jobId,
credits, animations: [name], clips: [{actionId, name, inPlace}], formats,
parent, meshy: {taskId, taskKind, rigTaskId, finalTaskId, aiModel, steps},
files: [{name, bytes, contentType, updatedAt}], createdAt, updatedAt,
createdBy}`. `meshy.taskId`/`taskKind`/`rigTaskId` let Lucina post-process a
model later (remesh, retexture, rig, animate, export).

File names: `model.glb`, `thumb.png|jpg|webp`, `preview.mp4` (the CPU turntable
video), `model.fbx|usdz|obj|stl|3mf`. Uploads are checked against the format's
magic bytes (`glTF`, PNG, JPEG, WebP, `ftyp`, zip for USDZ/3MF); at most 200 MB
(20 MB for thumbnails and the preview). Storage:
`$SECRETARY_DATA_DIR/models/<id>/` (0700) with `model.json` and the files
(0600); at most 500 models.

The chat shows a model as the v1 `model3d` block (`meta.blocks`): `{type:
"model3d", id, title, prompt?, animations?, credits?, formats?, poster?,
video?, rigged?}`. Clients build the file URLs from the id on their own origin.

## Error codes

| HTTP | `code` | Source | When |
|---|---|---|---|
| 400 | `bad_request` | server or Lucina | Validation failed (`message` names the field). |
| 400 | `invalid_json` | server or Lucina | Body is not valid JSON. |
| 400 | `due_in_past`, `unparseable_reminder`, `invalid_status` | Lucina | See the routes above. |
| 401 | `unauthorized` | server | No/invalid/revoked device token or session. |
| 403 | `csrf` | server | Cookie request that changes state without `X-Secretary-CSRF: 1`. |
| 404 | `not_found` | server | Unknown `/v1` route. |
| 404 | `job_not_found`, `reminder_not_found`, `item_not_found` | Lucina | |
| 404 | `note_not_found` | server | No note with that id or slug. |
| 409 | `already_sent` | Lucina | Cancelling a delivered reminder. |
| 409 | `revision_conflict` | server (from Lucina 409) | Shopping ops conflict with a newer list revision; refetch and retry. |
| 409 | `conflict` | gym/body engine | Stale `expected_revision` (engine body `{ok:false, code, error}`). |
| 409 | `conflict` | server | `PATCH /v1/notes/{key}` with a stale `baseUpdatedAt`. |
| 409 | `slug_taken`, `notes_full` | server | Notes: the slug is used by another note; 2000 live notes. |
| 413 | `payload_too_large` | server or Lucina | Body over 256 KiB (server; 768 KiB on `/v1/notes*`, 4.5 MiB for `POST /v1/device/events`) / 64 KiB (Lucina). |
| 413 | `note_too_large` | server | A note would hold more than 256 KiB of text. |
| 415 | `unsupported_media_type` | server | Body without `Content-Type: application/json`. |
| 422 | `invalid_command`, `op_id_reused` | gym/body engine | Engine rejected the command (engine body). |
| 422 | `unknown_outfit`, `unknown_hairstyle`, `unknown_hair_color` | server | `PUT /v1/avatar` names an outfit, hairstyle or colour the wardrobe does not have. |
| 429 | `rate_limited` | server | See rate limits; honour `Retry-After`. |
| 429 | `too_many_streams` | server | Over 4 open timeline streams for this device or 20 in total; honour `Retry-After`. |
| 500 | `internal` | server | Unexpected server failure. |
| 500 | `storage_error` | gym/body engine | The server could not write the module file (engine body). |
| 502 | `lucina_error` | server | Lucina failed (5xx), rejected the server's credentials, has the secretary API disabled, or answered off-contract. |
| 502 | `lucina_unreachable` | server | Could not connect to Lucina. |
| 502 | `tts_unavailable` | server | Kokoro unreachable or failed (`POST /v1/tts`). |
| 503 | `lucina_not_configured` | server | The server has no Lucina token configured. |
| 503 | `catalog_unavailable` | server | `catalog.{lang}.json` missing from `SECRETARY_CATALOG_DIR`. |
| 503 | `service_token_not_configured` | server | `/v1/insights/summary` or `/v1/notify` without `SECRETARY_SERVICE_TOKEN_FILE`. |
| 503 | `apns_disabled` | server | `POST /v1/push/apns` without APNs configured. |
| 404 | `subscription_not_found` | server | `DELETE /v1/push/subscriptions/{id}`, `POST /v1/push/test`. |
| 503 | `reminders_disabled`, `owner_chat_unconfigured`, `grocery_unavailable` | Lucina | Feature off on Lucina's host; show it, don't retry in a loop. |
| 504 | `lucina_timeout` | server | Lucina did not answer in time (chat 120 s, else 15 s). |
| 504 | `tts_unavailable` | server | Kokoro did not answer within 30 s. |

Any other Lucina 4xx code is passed through with Lucina's status.

## Upstream shopping routes (`/v1/upstream/*`, optional)

Mounted only when `SECRETARY_UPSTREAM_LEGACY` and/or
`SECRETARY_UPSTREAM_SERVICES` is set. They are the old `lucina_*` routes of
`lucina-proxy` (see `api-surface.md`) without the prefix, e.g.
`/v1/upstream/lists/{user}`, `/v1/upstream/items_put/{list}`,
`/v1/upstream/any/{family}/{*path}`. They need device auth like every `/v1`
route; the caller's upstream credential goes in `X-Upstream-Authorization`
and is relayed upstream as `Authorization`. Bodies are relayed as bytes.
The web app does not use them.

## Web app hosting

With `SECRETARY_WEB_DIR` set, the server also serves the built web app at
`/`:

- files from the directory; `/` serves `index.html`;
- SPA fallback: a `GET`/`HEAD` for a missing path outside `/v1/` and
  `/assets/` whose last segment has no `.` (e.g. `/agenda`) gets
  `index.html`; missing files stay `404`;
- `Cache-Control`: `/assets/*` → `public, max-age=31536000, immutable`
  (emit content-hashed file names there); everything else, `index.html`
  included → `no-cache`.

Every response carries `X-Content-Type-Options: nosniff` and
`Referrer-Policy: no-referrer`, and one of two Content Security Policies.

`/v1` responses (and JSON errors, such as a missing static file) get the
strict policy:

```
Content-Security-Policy: default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'
```

Files served from the web directory, the SPA `index.html` included, get the
web app policy, which the 3D Lucina stage in Chat needs:

```
Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; img-src 'self' data: blob:; connect-src 'self' blob: data:; media-src 'self' blob:; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'
```

| Directive | Why |
|---|---|
| `script-src 'wasm-unsafe-eval'` | The meshopt GLB decoder is WebAssembly. It does not allow `eval`. |
| `img-src blob:` | GLTFLoader turns textures embedded in the GLB into `blob:` URLs. |
| `connect-src blob: data:` | The loader fetches those `blob:`/`data:` URLs. |
| `media-src blob:` | TTS audio played from a `blob:` URL. |
| `worker-src blob:` | Decoder workers started from `blob:` URLs. |
| `base-uri 'none'` | No `<base>` tag can redirect relative URLs. |

It matches the standalone `/lucina/` page that Caddy serves
(`deploy/Caddyfile.p1`). Neither policy allows inline `<script>` or remote
origins, so the web build must not use inline `<script>`, remote scripts or fonts, or
remote images (`imageUrl` from a `generate_image` job will not render in an
`<img>` under this CSP; open it as a link instead).
