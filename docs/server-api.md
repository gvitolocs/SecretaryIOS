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
| Lucina | `Authorization: Bearer svc_…` (the service token) | Only `GET /v1/insights/summary` (see [Insights](#insights-for-lucina)), `/v1/avatar*` (see [Avatar wardrobe](#avatar-wardrobe)) and `POST /v1/notify` (see [Notifications](#notifications)). |

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
| `message` | Required, 1–4000 characters, not blank, no control characters except newline/tab. |
| `sessionId` | Optional, `[A-Za-z0-9_-]{1,64}`. Lucina uses Honcho session `secretary_<sessionId>`. |
| `clientMessageId` | Optional, 1–128 characters of `[A-Za-z0-9_.:-]` (a UUID works). A retry with the same `sessionId` + `clientMessageId` within 10 minutes returns the first answer with `"duplicate": true`. |
| `channel` | Optional, `web` or `ios`: which client the message came from (it labels the message in the [timeline](#timeline)). |

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
| `replyTo` | Optional id of the item this answers. |
| `attachments` | Optional `[{kind: image\|voice\|file, name?}]`. |
| `meta` | Optional `{chatTitle?, guildName?, channelName?, isDm?, kind?}` (`kind: "reminder"` for a delivered reminder). |
| `backfilled` | Optional `true` for history imported after the fact. |

Other fields may appear and are passed through unchanged.

### `GET /v1/timeline?before=<id>&limit=50`

Newest first. `200`:

```json
{ "items": [ { "id": "tl_0193…", "…": "…" } ], "nextBefore": "tl_0150…" }
```

| Query | Rules |
|---|---|
| `before` | Optional item id (`[A-Za-z0-9_.:-]{1,128}`): only older items. Pass the previous page's `nextBefore`. |
| `limit` | Optional integer 1–200; Lucina's default is 50. |

`nextBefore` is `null` at the start of history. Anything else is `400
bad_request` before Lucina is called. Timeout 15 s.

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
  `audio/mpeg`, `.vmd` `application/octet-stream`. Anything else is `404`.
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
| 409 | `already_sent` | Lucina | Cancelling a delivered reminder. |
| 409 | `revision_conflict` | server (from Lucina 409) | Shopping ops conflict with a newer list revision; refetch and retry. |
| 409 | `conflict` | gym/body engine | Stale `expected_revision` (engine body `{ok:false, code, error}`). |
| 413 | `payload_too_large` | server or Lucina | Body over 256 KiB (server; 4.5 MiB for `POST /v1/device/events`) / 64 KiB (Lucina). |
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
