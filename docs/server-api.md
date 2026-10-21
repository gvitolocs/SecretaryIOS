# secretary-server API (`/v1`)

The contract between the Secretary clients (the iOS app, the web app at
`https://secratary.app`) and `secretary-server`. The server sits in front of
Lucina's `/api/secretary/*` (see Lucina's `docs/secretary-api.md`): it
authenticates the client, validates the input, forwards it with its own
Lucina token, and returns Lucina's `data`. Clients never see the Lucina token
or the `{ok, data}` envelope.

## Conventions

- **JSON** in and out. Request bodies need `Content-Type: application/json`
  and are capped at 256 KiB (Lucina itself caps at 64 KiB).
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

Every `/v1` route except `GET /v1/health` and `/v1/session` needs one of:

| Client | How | Notes |
|---|---|---|
| iOS app, scripts | `Authorization: Bearer sec_…` (a device token) | If an `Authorization` header is present, it alone decides; the cookie is ignored. |
| Browser | the `sec_session` cookie from `POST /v1/session` | State-changing requests (`POST`, `PUT`, `PATCH`, `DELETE`) must also send `X-Secretary-CSRF: 1`, else `403 csrf`. |

- **Device tokens** are issued on the server with
  `secretary-server token issue --name <name>` and shown once. Format: `sec_`
  + 43 base64url characters. Revoking a device (`token revoke <id>`) ends its
  Bearer access and all of its browser sessions on the next request.
- **CSRF:** the server sends no CORS headers, so a cross-site page cannot add
  `X-Secretary-CSRF` (a custom header forces a preflight that fails). The
  cookie is also `SameSite=Strict`. Bearer requests are exempt.
- **Rate limits:** 60 requests/minute per device (token bucket, burst 60),
  and 5 `POST /v1/session` attempts/minute per client IP. Both answer `429
  rate_limited` with `Retry-After: <seconds>`.

Auth errors: `401 unauthorized` (with `WWW-Authenticate: Bearer`). The
`message` says why: `missing or malformed device token`, `unknown device
token`, `device token revoked`, `session expired or invalid`.

### Web app recipe

```js
// login (once): exchange a device token for the cookie
await fetch("/v1/session", {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ token }),
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

Request: `{"token": "sec_…"}` (a device token).

`200`:

```json
{ "authenticated": true, "deviceName": "Giuseppe's iPhone" }
```

with

```
Set-Cookie: sec_session=<43 base64url chars>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000
```

The session lasts 30 days (not sliding). Only its SHA-256 is stored on the
server. Errors: `400 bad_request` / `400 invalid_json` / `415
unsupported_media_type` (body), `401 unauthorized` (unknown or revoked
token), `429 rate_limited`. No CSRF header is needed here; the JSON
content type already forces a CORS preflight for cross-site callers.

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

Server timeout 120 s (an LLM turn); use a client timeout of at least 125 s.

`200`:

```json
{
  "reply": "Reminder set for Tuesday 21 October 2026 at 18:30 (Europe/Rome): chiamare la mamma",
  "action": "schedule_reminder",
  "job": null,
  "sessionId": "secretary_iphone",
  "clientMessageId": "B1E4C0DE-2F0A-4B8C-9D11-7A6E5F4C3B2A",
  "reminder": { "id": "…", "text": "chiamare la mamma", "dueAt": "2026-10-21T16:30:00.000Z", "…": "…" }
}
```

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
| 413 | `payload_too_large` | server or Lucina | Body over 256 KiB (server) / 64 KiB (Lucina). |
| 415 | `unsupported_media_type` | server | Body without `Content-Type: application/json`. |
| 429 | `rate_limited` | server | See rate limits; honour `Retry-After`. |
| 500 | `internal` | server | Unexpected server failure. |
| 502 | `lucina_error` | server | Lucina failed (5xx), rejected the server's credentials, has the secretary API disabled, or answered off-contract. |
| 502 | `lucina_unreachable` | server | Could not connect to Lucina. |
| 503 | `lucina_not_configured` | server | The server has no Lucina token configured. |
| 503 | `reminders_disabled`, `owner_chat_unconfigured`, `grocery_unavailable` | Lucina | Feature off on Lucina's host; show it, don't retry in a loop. |
| 504 | `lucina_timeout` | server | Lucina did not answer in time (chat 120 s, else 15 s). |

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

Every response (API and static) carries:

```
Content-Security-Policy: default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
```

So the web build must not use inline `<script>`, remote scripts/fonts or
remote images (`imageUrl` from a `generate_image` job will not render in an
`<img>` under this CSP; open it as a link instead).
