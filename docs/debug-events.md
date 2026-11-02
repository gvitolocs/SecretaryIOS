# Debug events: what happened on the phone

The owner's apps keep a step-by-step log of what happened on the device: which element he tapped, which intent ran,
what a notification or Live Activity button did, what Face ID answered, how an approval ended and which requests
failed. The log goes to secretary-server, so Claude, Lucina and the MCP clients can read a phone test afterwards,
without USB and without asking him what he saw.

```
iPhone app / widgets / share / photo upload ──record()──► DebugEvents/<process>.jsonl (ring buffer, ≤ 2 MB / 5000)
        │ flush (foreground, every 60 s in front, background, after an intent / notification action)
        ▼
POST /v1/debug/events (device token) ──► secretary-server: sanitize again ──► <data dir>/debug-events.jsonl (0600)
GET  /v1/debug/events (device or service token) ◄── secretary-mcp `debug_events`, `secretary-mcp debug-events`
```

| Piece | Where |
| --- | --- |
| Recorder, sanitizer, uploader (shared by the app and extensions) | `ios/Secretary/Core/DebugEvents.swift` |
| App uploads and lifecycle | `ios/Secretary/App/DebugEventsUploader.swift` |
| Approval fields | `ios/Secretary/Features/Approvals/ApprovalsDebug.swift` |
| Build commit | `SecretaryBuildCommit` in the app's Info.plist, stamped by the app target's "Stamp build commit" phase (`ios/project.yml`) |
| Server | `crates/secretary-server/src/debug_events.rs`, tests `crates/secretary-server/tests/debug_events.rs` |
| MCP tool and CLI | `crates/secretary-mcp/src/debug_events.rs` (`debug_events`, scope `read`), `secretary-mcp debug-events` |

## Event shape

One JSON object per event:

```json
{"ts": "2026-11-01T09:00:00.250Z", "seq": 41, "device": "dev_3fa2b1c9", "device_name": "Giuseppe's iPhone",
 "build": "adfd5fe", "process": "app", "type": "notification.action",
 "fields": {"kind": "approval", "id": "ap_8f3a19c2d4", "action": "APPROVAL_APPROVE", "category": "APPROVAL_SENSITIVE"},
 "received_at": "2026-11-01T09:00:02.104Z"}
```

| Field | |
| --- | --- |
| `ts` | when it happened on the device, RFC 3339 with milliseconds (stored in UTC) |
| `seq` | per device and process, increasing; the client's upload mark |
| `device`, `device_name` | the authenticated device (set by the server; whatever the client sends is ignored) |
| `build` | the git commit the app was built from (`git rev-parse --short HEAD`, `-dirty` with uncommitted changes, `unknown` without git); `SECRETARY_BUILD_COMMIT` overrides it at build time |
| `process` | `app`, `widgets`, `share`, `photoupload`, … (from the bundle id) |
| `type` | dotted lowercase, below |
| `fields` | a flat map of strings (≤ 200 characters), numbers and booleans; at most 40 |
| `received_at` | when the server stored it |

Reused, not invented: the device-bridge conventions (`ts`, a dotted `type`, `device` = the device id, as
`secretary-core`'s `DeviceEvent`), the JSONL storage and rotation of LucinaVoice telemetry
(`crates/secretary-server/src/voice_telemetry.rs`), and the secret key names of lucina-diagnostics' scrubber
(`docs/diagnostics.md`). LucinaVoice's own events (`{t, k, …}` with a fixed schema per kind) and the diagnostics
reports (crashes with stacks and attachments) did not fit a free-form step log, so neither shape is used as is.

### Types

| Type | Fields | Where |
| --- | --- | --- |
| `app.launch` | `state` (foreground/background), `reason` (remote_notification, location, url), `os`, `version` | AppDelegate |
| `app.foreground`, `app.background`, `app.terminate` | | `DebugEventsUploader` |
| `bg.task` | `task` (refresh, photobackup, photo_upload_ext), `phase` (start, end, expired, terminated), `outcome`, `jobs`, `error` | BackgroundRefresh, PhotoBackup, PhotoUploadExtension |
| `net.request` | `method`, `path` (ids as `{id}`), `status` or `error`, `duration_ms` | `APIClient.send` (every attempt) |
| `place.route` | `mode` (bike, drive, walk, transit), `via` (mapKit, walkingPath, straightLine, none), `seconds`, `meters`, `steps`, `navigating`, `from_home` | `PlaceMapView` |
| `place.navigate` | `mode`, `via` | `PlaceMapView`, Navigate tapped or opened navigating |
| `place.open_external` | `maps` (google, apple), `mode`, `app` (Google Maps app installed) | `ExternalMaps` |
| `notification.delivered` | `kind` (approval, decision, route, other), `id`, `sensitive`, `via` | AppDelegate (`content-available` push) |
| `notification.presented` | `kind`, `id`, `sensitive`, `category` | `willPresent` |
| `notification.action` | `kind`, `id`, `sensitive`, `category`, `action` (the action identifier; `com.apple.UNNotificationDefaultActionIdentifier` is a tap) | `didReceive` |
| `notification.replaced` | `id`, `by` (activity) | ApprovalActivities |
| `activity.start`, `activity.update`, `activity.end` | `id`, `state` / `status`, `ok`, `error`, `count` | ApprovalActivities |
| `activity.push_token` | `kind` (start, update), `id`, `registered`, `error` | ApprovalActivities (never the token) |
| `card.intent` | `intent` (deny_approval_activity, deny_approval, check_shopping_item, complete_task, open_secretary, start_memo_recording, ask_lucina, …), `phase` (start, end), `duration_ms`, ids | every widget, Live Activity and Shortcuts intent `perform()` |
| `card.tap` | `action` (deny, approve), `id`, `sensitive` | the Live Activity's Deny and Approve handlers |
| `ui.tap` | `element`, `screen` (approval_sheet, settings_approvals, settings_about), `id`, `on`, `trigger`, `pad`, `ignored` | the approval card (Approve, Deny, PIN pad, Use PIN / Face ID, Close), Settings switches |
| `calendar.detail_open`, `calendar.detail_device` (`found`, `writable`), `calendar.detail_map`, `calendar.setting_change`, `calendar.travel_estimate` | `id`, `source`, `editable`, `phones`, `has_location`, `alarms` (counts); `found`; `setting` (alarms, show_as, travel, leave_alert, calendar), `value`, `ok`, `error`; `via`, `minutes`, `mode` | `CalendarDebug` (the event sheet; its taps are `ui.tap` with `screen` calendar_detail) |
| `auth.prompt`, `auth.result` | `signer` (face_id, unlock), `policy`, `stage` (sign), `id` and `trigger` (unlock-key signatures), `ok`, `cancelled`, `error` (`com.apple.LocalAuthentication:-2` is a cancel) | ApprovalSigner |
| `approval.decide` | `id`, `decision`, `sensitive`, `via` (face_id, approval_pin, plain, unlock_key), `outcome`, `error` | ApprovalsStore (card and one-tap) |
| `approval.one_tap` | `id`, `step` (refused_no_trigger, not_pending, expired, changed_since_shown, not_ready, fetch_failed:…) | the notification's one-tap Approve |
| `approval.card` | `id`, `step` | the card refused an answer |
| `approval.activity_approve` | `id`, `outcome` (decided_unlock_key, decided_plain, opened_card, refused), `reason` (approved, expired, not_pending, changed_since_shown, already_in_flight, unlock_key_approve_failed, one_tap_not_ready, signed_out, an outcome name, …) | the Live Activity's Approve (`approveFromActivity`) |
| `face_id_key.enroll`, `unlock_key.enroll`, `approval_pin.change` | `step` (requested, start, face_id_ok, face_id_cancelled, registered, retry_after_cancel:<request>:<n>, enrolled, cancelled, failed, skipped_…), `error` | ApprovalsStore |
| `share.open`, `share.send`, `share.sent`, `share.close` | counts, `action`, `background`, `ok`, `error` | share extension |
| `apns.register` | `ok`, `error` | AppDelegate |

Errors are codes, never messages: `NSURLErrorDomain:-1009` (offline), `NSURLErrorDomain:-1001` (timeout),
`APIError:409:already_decided`, `com.apple.LocalAuthentication:-2` (user cancel).

## Never recorded

- secrets: tokens, keys, signatures, proofs, passwords, PINs, OTPs, cookies, `Authorization` headers;
- content: message and chat text, note text and titles, command text, plan details and summaries, transcripts,
  prompts, e-mail addresses;
- request and response bodies, URL query strings and fragments, URL credentials.

Approvals record only the id, the action's decision, `sensitive`, how it was proven, the outcome and error codes.

Enforced twice, on the device and again on the server, with the same rules:

- a field whose name contains a forbidden word (split at `_`, `.`, `-` and camelCase: `authToken`, `unlock_key`,
  `message_text`, `requestURL`, …) is dropped on the device (`dropped_fields` counts them) and **refuses the whole
  event** on the server (`refused: [{seq, reason: "forbidden_field:<name>"}]`). The list is `FORBIDDEN_WORDS` in
  `debug_events.rs` and `DebugEvents.forbiddenWords`; keep them equal;
- a string that looks like a credential (`Bearer …`, `Basic …`, `sec_…`, `svc_…`, `mcp_…`, a JWT, any unbroken
  base64 / hex run of 40 characters or more) becomes `[redacted]`;
- a URL loses its query, fragment and `user:password@`;
- in `path` / `route` (and `*_path` / `*_route`), ids become `{id}`: numbers, segments of 8+ characters with a digit,
  anything with `@` or `%`, longer than 40, or after `notes` / `files` (note names are content);
- control characters are removed; strings are cut at 200 characters; nested values, lists and `null` are dropped.

Tests: `debug_events::tests::forbidden_field_names_refuse_the_event`, `values_are_cleaned_and_secrets_redacted`
(server) and `DebugEventsTests` (iOS).

## On the device

`DebugEvents.record(type, fields)` cleans the fields on the caller's thread and appends one line on a utility
queue; it never blocks. Each process writes `DebugEvents/<process>.jsonl` and `<process>.state.json` (the next `seq`
and the last uploaded one) in the App Group container, or in its own Application Support while the App Group is not
provisioned. Over 5000 events or 2 MB the oldest are dropped (4000 kept).

Each process uploads its own buffer (`DebugEvents.flush`): batches of 200 to `POST /v1/debug/events` with the device
token from the shared keychain (`BridgeCredentials`), each endpoint in turn. 2xx moves the mark; 429, 5xx or no answer
keep the events for the next flush; any other 4xx drops the batch. The app flushes on foreground, every 60 s in front,
on background (inside a background-task window), after a notification action and after a background refresh;
extensions after each intent, share and photo-upload run.

Settings → About shows the build commit, the **Send debug events** switch (default on; off stops uploads in every
process that shares the keychain, recording continues locally) and when the events were last sent.

## Server

### `POST /v1/debug/events` (device token)

```json
{"events": [{"ts": "2026-11-01T10:00:00.250+01:00", "seq": 41, "build": "adfd5fe", "process": "app",
             "type": "ui.tap", "fields": {"element": "approve", "screen": "approval_sheet"}}]}
```

At most 200 events and 256 KiB (`413` above). Each event needs a valid `ts`, `seq` (≥ 0), `type` and `process`; a bad
one is refused alone. Answer `200 {"accepted": 1, "refused": [], "max_seq": 41}` (`refused` lists at most 20). `400`
for a body without `events` or over 200 events, `403 devices_only` for the service token, `429` past 30 batches a
minute per device (on top of the general per-device limit).

Stored in `<SECRETARY_DATA_DIR>/debug-events.jsonl` (0600), one event per line with `received_at`. At 50 MiB it moves
to `.1` (`.1` → `.2`, `.2` → `.3`, the old `.3` is deleted): about 200 MiB at most.

### `GET /v1/debug/events?device=&since=&until=&type=&limit=` (any device token or the service token)

- `device`: a device id, or part of a device name, case-insensitive (`iphone`);
- `since`, `until`: inclusive; RFC 3339, `YYYY-MM-DD` (start of that day in `SECRETARY_TIMEZONE`) or `30m` / `2h` /
  `1d` back from now;
- `type`: a prefix (`net`, `auth.`, `notification.action`);
- `limit`: 1–2000, default 200.

Answer `{"count": n, "more": bool, "events": [...]}`: the newest `limit` matching events, oldest first (newest last).
`more` says older matches exist. Every device token reads every device (they are all the owner's), like the MCP's own
device token. Unknown parameters and malformed values get `400`.

## MCP and CLI

`debug_events {device?, since?, until?, type?, limit?}` (secretary-mcp, scope `read`, through secretary-server with
the MCP's device token; limit 1–500, default 100). One line per event, oldest first, in the owner's time zone:

```
2 debug event(s) (device iphone, since 1h), oldest first, times in Europe/Rome:
2026-11-01 10:00:00.250 [Giuseppe's iPhone/app] notification.action action=APPROVAL_APPROVE category=APPROVAL id=ap_1 kind=approval  (seq 41, build adfd5fe)
10:00:01.000 [Giuseppe's iPhone/app] net.request duration_ms=210 method=POST path=/v1/approvals/{id}/decide status=409  (seq 42, build adfd5fe)
```

On the MCP host: `secretary-mcp debug-events --device iphone --since 30m [--type net] [--until …] [--limit 100]`
(reads `SECRETARY_MCP_BACKEND_URL` and `SECRETARY_MCP_BACKEND_TOKEN_FILE`, like the service). Without the MCP:
`curl -H "Authorization: Bearer $SVC" "http://127.0.0.1:8800/v1/debug/events?device=iphone&since=30m"`.

## Reading a phone test

1. Ask the owner nothing; note roughly when he tested. `debug_events device=iphone since=30m` (or `since` the test's
   start) shows the whole sequence.
2. Check `build` first: it says which commit was on the phone. `unknown` means a build without the stamp; `-dirty` a
   local build with uncommitted changes.
3. Follow one approval by its id: `notification.delivered` / `presented` (did it arrive, was a banner shown),
   `activity.start` and `notification.replaced` (the card took over), `notification.action` or `card.intent` /
   `ui.tap` (what he pressed), `auth.prompt` → `auth.result` (Face ID; `cancelled=true` is his cancel,
   `com.apple.LocalAuthentication:-1` a failed match), `net.request` on `/v1/approvals/{id}/decide` (status) and
   `approval.decide` (the outcome), then `activity.end`.
4. Failures: `type=net` and look for `status` ≥ 400 or an `error`; `approval.one_tap` steps say why a one-tap Approve
   fell back to "Open Secretary to approve".
5. Gaps: events are uploaded within a minute in front, at background, or after an intent; an extension's events
   arrive with its next run. `seq` is per process, so a jump inside one process means the ring buffer dropped events
   (more than 5000 since the last upload) or uploads were off.
