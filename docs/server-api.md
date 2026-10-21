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

Every `/v1` route except `GET /v1/health`, `/v1/session` and
`POST /v1/auth/device` needs one of:

| Client | How | Notes |
|---|---|---|
| iOS app, scripts | `Authorization: Bearer sec_…` (a device token) | If an `Authorization` header is present, it alone decides; the cookie is ignored. |
| Browser | the `sec_session` cookie from `POST /v1/session` | State-changing requests (`POST`, `PUT`, `PATCH`, `DELETE`) must also send `X-Secretary-CSRF: 1`, else `403 csrf`. |
| Lucina | `Authorization: Bearer svc_…` (the service token) | Only `GET /v1/insights/summary`; see [Insights](#insights-for-lucina). |

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
  and 5 `POST /v1/session` attempts/minute per client IP. Both answer `429
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
{ "currentPassword": "…", "newPassword": "at least 12 characters" }
```

Cookie clients need `X-Secretary-CSRF: 1`. `200`:

```json
{ "changed": true, "sessionsRevoked": 2 }
```

Every browser session except the caller's own cookie session ends (with a
Bearer token: every browser session). Device tokens stay valid. Errors:
`403 invalid_credentials` (wrong current password; counts toward the
lockout; 403 so the app does not log out), `400 bad_request` (new password
shorter than 12 characters or over 1024 bytes), `429 account_locked`, `409
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

View:

```json
{
  "revision": 3,
  "settings": { "unit": "kg", "goal_grams": 78000 },
  "entries": [ { "id": "…", "weight_grams": 80500, "measured_ms": 1792584000000, "note": "", "created_ms": …, "updated_ms": … } ],
  "stats": {
    "count": 3,
    "current_grams": 80500, "current_measured_ms": 1792584000000,
    "avg7_grams": 80800, "weekly_change_grams": -400,
    "min_grams": 80500, "max_grams": 81200,
    "trend30_grams": -700, "trend90_grams": null,
    "goal_grams": 78000, "goal_distance_grams": 2800, "goal_reached": false
  },
  "series": [ { "entry_id": "…", "measured_ms": 1792411200000, "weight_grams": 81200, "avg7_grams": 81200 } ],
  "labels": { "<entry_id>": "80.5 kg" },
  "stat_labels": { "current": "80.5 kg", "avg7": "80.8 kg", "weekly_change": "-0.4 kg", "…": "…" }
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

`clock`: `{"rest_remaining": 42, "active_duration": "12m 05s", "active_elapsed_seconds": 725}`.
Tick `active_elapsed_seconds` locally between refreshes.

## Insights for Lucina

### `GET /v1/insights/summary?timezone=Europe/Rome&workouts=5`

Read-only, compact JSON for Lucina to reason over. It accepts **only** a
service token: `Authorization: Bearer svc_…`, the token stored in
`SECRETARY_SERVICE_TOKEN_FILE` (create one with `secretary-server
service-token generate <path>`; the file is written 0600 and the token is
never printed). Device tokens and cookies get `401` here, and a service
token gets `401` on every other route. Without `SECRETARY_SERVICE_TOKEN_FILE`
the route answers `503 service_token_not_configured`. `workouts` is 1–20
(default 5).

```json
{
  "generated_ms": 1792600000000,
  "timezone": "Europe/Rome",
  "gym": {
    "unit": "kg", "weekly_goal": 3, "workouts_this_week": 2,
    "recent_workouts": [
      { "id": "…", "title": "Push", "finished_ms": …, "date": "2026-10-20", "duration_min": 58,
        "completed_sets": 15, "volume_grams": 8240000, "volume_label": "8240 kg",
        "exercises": ["Bench Press", "Overhead Press"],
        "prs": [ { "exercise_id": "bench-press", "name": "Bench Press", "weight_grams": 82500, "reps": 5, "label": "82.5 kg × 5" } ] }
    ],
    "weekly_volume": [ { "week_start": "2026-10-19", "workouts": 2, "volume_grams": 15100000, "volume_label": "15100 kg" } ]
  },
  "body": { "unit": "kg", "stats": { …the body stats… }, "stat_labels": { … } }
}
```

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

Without `voice` and `lang`, the server guesses: Italian (`if_sara`) when
the text has at least two Italian stopwords (`che, non, per, sono, ciao, il,
la, è, un, una, di, ti, mi, come, oggi`), or one in a text of at most three
words; otherwise English (`af_heart`).

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
| 409 | `conflict` | gym/body engine | Stale `expected_revision` (engine body `{ok:false, code, error}`). |
| 413 | `payload_too_large` | server or Lucina | Body over 256 KiB (server) / 64 KiB (Lucina). |
| 415 | `unsupported_media_type` | server | Body without `Content-Type: application/json`. |
| 422 | `invalid_command`, `op_id_reused` | gym/body engine | Engine rejected the command (engine body). |
| 429 | `rate_limited` | server | See rate limits; honour `Retry-After`. |
| 500 | `internal` | server | Unexpected server failure. |
| 500 | `storage_error` | gym/body engine | The server could not write the module file (engine body). |
| 502 | `lucina_error` | server | Lucina failed (5xx), rejected the server's credentials, has the secretary API disabled, or answered off-contract. |
| 502 | `lucina_unreachable` | server | Could not connect to Lucina. |
| 502 | `tts_unavailable` | server | Kokoro unreachable or failed (`POST /v1/tts`). |
| 503 | `lucina_not_configured` | server | The server has no Lucina token configured. |
| 503 | `service_token_not_configured` | server | `/v1/insights/summary` without `SECRETARY_SERVICE_TOKEN_FILE`. |
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

Every response (API and static) carries:

```
Content-Security-Policy: default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
```

So the web build must not use inline `<script>`, remote scripts/fonts or
remote images (`imageUrl` from a `generate_image` job will not render in an
`<img>` under this CSP; open it as a link instead).
