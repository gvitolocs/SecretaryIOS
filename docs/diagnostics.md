# Diagnostics: crash reports and logs

One Lucina-owned place for crashes, errors and log bundles from every client. When something breaks, a Claude
session or Lucina reads what failed instead of asking the owner for screenshots.

```
Secretary Code (Mac) ┐
iPhone / Android     ├─HTTPS─► p1 Caddy /v1/diagnostics* ──tailnet──► lucina-diagnostics (nezopt :8850)
web app              ┘         forward_auth to secretary-server        scrub ─► dedupe ─► store + SQLite index
Lucina servers ──────────────── Bearer service token (local) ─────────┘          │
                                                                                 ├─► new crash: app inbox + Telegram
secretary-mcp (p1) ──tailnet, service token──► diagnostics_recent / _get / _file │
Lucina (lucina-rs) ──127.0.0.1, service token──► tool `diagnostics` ◄────────────┘
```

## Pieces

| Piece | Where |
| --- | --- |
| Service, CLI | `crates/lucina-diagnostics` (binary `lucina-diagnostics`), user unit `lucina-diagnostics.service`, `deploy/lucina-diagnostics/` |
| Config | `~/.config/lucina-diagnostics/diagnostics.env` (template `deploy/lucina-diagnostics/diagnostics.env.example`) plus Lucina's `~/.config/lucina-rs/node.env` (secretary-server URL and service token, Telegram chat) for notifications |
| Gateway | p1 `/etc/caddy/Caddyfile`, block `@diagnostics` (`deploy/lucina-diagnostics/Caddyfile.diagnostics`, same gate as `/v1/memos`); key `/etc/caddy/diagnostics-gateway.key` = nezopt `~/.config/lucina-diagnostics/gateway.token` |
| MCP tools | `crates/secretary-mcp/src/diagnostics.rs`: `diagnostics_recent`, `diagnostics_get`, `diagnostics_file` (scope `read`) |
| Lucina's tool | `crates/lucina-tools/src/tools/diagnostics.rs` (`diagnostics`: recent, get, file) |
| Clients | Secretary Code: repo `gvitolocs/SecretaryCode` (main process, renderer, background service). iOS, Android, web: to do, same API |

## API

Base `https://getsecretary.tech/v1/diagnostics` for the apps (device token or web cookie, checked by Caddy's
`forward_auth`), `http://127.0.0.1:8850` / `http://100.107.164.33:8850` for local callers with
`Authorization: Bearer <service.token>`. Cookie-authenticated writes need `X-Secretary-CSRF: 1`.
`GET /health` is open. Errors are `{"error": {"code", "message"}}`.

### `POST /v1/diagnostics/reports`

Either `Content-Type: application/json` with the report, or `multipart/form-data` with a `report` part (the same
JSON) and file parts. The whole body may also be sent with `Content-Encoding: gzip`.

```json
{
  "app": "secretary-code",
  "app_version": "1.4.2",
  "os": "macOS 15.1 (24B83) arm64",
  "device": "nez-mbp",
  "kind": "crash",
  "message": "Background service timed out after 30012ms",
  "stack": "Error: Background service timed out after 30012ms\n    at startSidecar (…/out/main/index.js:1234:56)\n    …",
  "context": {"process": "main", "window": "main", "route": "/session/ses_…", "engine": "1.0.200", "electron": "38.1.0"},
  "occurred_at": "2026-10-26T18:44:00.000Z",
  "client_id": "3f0c1d1e-…"
}
```

| Field | | |
| --- | --- | --- |
| `app` | required | `secretary-code`, `ios`, `android`, `web`, `server` |
| `kind` | required | `crash` (process died), `fatal_error` (unrecoverable, app shows an error screen), `error` (handled but worth seeing), `user_report` (the owner describing a problem), `hang` |
| `message` | one of message/stack | ≤ 16 KiB kept (cut after scrubbing) |
| `stack` | one of message/stack | ≤ 128 KiB kept; JS, Swift/Apple crash report, Kotlin/Java, Rust and Python frames are understood |
| `app_version`, `os`, `device` | optional | one line each, ≤ 64 / 120 / 120 characters. `device` also keys the rate limit: send a stable name |
| `context` | optional | any JSON, ≤ 64 KiB serialized (bigger is replaced by `{"_truncated": true, "_bytes": n}`) |
| `occurred_at` | optional | RFC 3339; default: received time; more than a day in the future is refused |
| `client_id` | optional | 8–100 of `A-Za-z0-9-_.:`; a retry with the same id returns the first answer and changes nothing (use it from offline queues) |

camelCase (`appVersion`, `occurredAt`, `clientId`) is accepted too. Unknown fields are ignored.

File parts: any part with a `filename`, or named `file`, `files`, `log`, `logs`, `minidump`, `attachment(s)`. At most
10 files. A part is stored as:

- **gzip** (magic bytes `1f 8b`, any name) is expanded first and `.gz` dropped from the name;
- **minidump**: starts with `MDMP` (Crashpad/Breakpad), or binary data in a part named `minidump`; stored as sent;
- **log**: anything without NUL bytes in its first 8 KiB; read as UTF-8 (invalid bytes replaced) and **scrubbed**;
- **binary**: anything else, stored as sent.

Names are reduced to their base name and `[A-Za-z0-9._-]`; collisions get `-2`, `-3`.

Caps: the request body ≤ **20 MB** (`LUCINA_DIAGNOSTICS_MAX_UPLOAD_MB`, before gzip), attachments ≤ **64 MB** after
gzip (`LUCINA_DIAGNOSTICS_MAX_EXPANDED_MB`). Over a cap → `413 too_large`, nothing is stored.

Answers:

| Status | Body | Meaning |
| --- | --- | --- |
| 201 | `{"id": "dg_…", "duplicate": false, "count": 1, "fingerprint": "…"}` | a new report |
| 200 | `{"id": "dg_…", "duplicate": true, "count": 7}` | the same crash was already stored: its counter went up, this copy and its files were dropped |
| 200 | `{"id": "dg_…", "duplicate": true, "count": 7, "replay": true}` | this `client_id` was sent before |
| 400 | `bad_request` | invalid field (the message says which) |
| 401 / 403 | `unauthorized` / `csrf` | |
| 413 | `too_large` | |
| 415 | `unsupported_media_type` / `unsupported_encoding` | |
| 429 | `rate_limited`, `Retry-After: <s>` | keep the report queued and retry after that |

Rate limit: per `app` + `device`, a burst of 20 then 60 an hour; 600 an hour for everything together.
Clients should queue reports on disk, retry with backoff, and send the same `client_id` on retries.

**Dedupe.** The fingerprint is SHA-256 over app, `app_version`, kind and the top 5 stack frames, normalized: function
names and file base names stay; line and column numbers, directories (install paths, home folders), hex addresses,
bundle hashes (`index-3f9a8b2c.js`), query strings, UUIDs and numbers go. Without a stack, the first line of the
message normalized the same way (quoted strings and numbers removed). Repeats bump `count`, `last_seen_at` and keep
the last 20 occurrences (time, version, OS, device). `user_report`s are never merged.

### `GET /v1/diagnostics/reports`

Query: `app` and `kind` (comma lists), `since` / `until` (on `last_seen_at`: RFC 3339, `YYYY-MM-DD` = midnight in
the owner's zone, or `30m` / `24h` / `7d`), `device`, `fingerprint`, `limit` (default 50, max 200). Most recently seen
first.

```json
{"reports": [{"id": "dg_…", "app": "secretary-code", "app_version": "1.4.2", "os": "…", "device": "nez-mbp",
  "kind": "crash", "message": "<first line, ≤ 300 chars>", "fingerprint": "…", "occurred_at": "…",
  "received_at": "…", "last_seen_at": "…", "count": 3, "attachments": 2}],
 "timeZone": "Europe/Copenhagen"}
```

### `GET /v1/diagnostics/reports/{id}`

`{"report": <report.json>, "attachments": [{"name", "kind": "log|minidump|binary", "bytes", "gzip", "text", "sha256", "url"}], "timeZone"}`.
`report.json` has every field above plus `received_at`, `fingerprint`, `signature` (the normalized frames),
`count`, `last_seen_at`, `last_occurred_at`, `recent`, `attachments`, `redactions` (secrets replaced).

### `GET /v1/diagnostics/reports/{id}/files/{name}`

The file. `?tail=<bytes>` or `?head=<bytes>` returns that part (text from a whole line). Headers:
`Content-Type` `text/plain; charset=utf-8` (logs) or `application/octet-stream`, `X-Diagnostics-Size` (whole
file), `X-Diagnostics-Truncated`.

## Storage

```
~/data/lucina-diagnostics/                    LUCINA_DIAGNOSTICS_DIR (mode 700)
  index.sqlite                                the index (rebuild: lucina-diagnostics reindex)
  .staging/<id>/                              a report being written (cleared at start)
  <app>/<YYYY-MM-DD>/<id>/report.json         date = received (UTC)
  <app>/<YYYY-MM-DD>/<id>/<attachments>
```

A report is written whole in `.staging/`, moved into place with one rename and then indexed; the dedupe decision and
the insert are taken under the index lock, so copies of one crash arriving together still give one report. A crash
between the rename and the index leaves an unindexed directory, which the retention sweep removes.

**Retention**: reports not seen for 90 days (`LUCINA_DIAGNOSTICS_RETENTION_DAYS`) are deleted, with their files,
every 6 hours and at start; a crash that keeps repeating stays. `lucina-diagnostics purge` runs it now.

## Privacy: what is stored

Stored: the fields the client sent (app, version, OS string, device name, kind, message, stack, context, time),
the attachments, the received time and the counters. **Not** stored: the client's IP address, its device token or
cookie, request headers.

Before anything is written, the message, the stack, every string in `context` and every text attachment are scrubbed
(`crates/lucina-diagnostics/src/scrub.rs`); each match becomes `[redacted]`:

- `Authorization`, `Cookie`, `Set-Cookie` and `X-…-Gateway` / `-Token` / `-Secret` / `-Api-Key` header values;
- `Bearer <token>` and `Basic <base64>` anywhere;
- `key=value`, `key: value`, `"key": "value"` where the key ends in `password`, `passwd`, `secret`, `token`,
  `api_key`/`apikey`, `access_key`, `private_key`, `credential(s)` or is `sec_session`. This covers
  `OPENCODE_SERVER_PASSWORD=…` and Secretary Code's background-service credentials `{"url", "password"}` (the
  engine's local service password), and `?access_token=…` in URLs;
- the password in URLs with credentials (`http://opencode:<pw>@127.0.0.1:4096`);
- token shapes: `sk-…` (OpenAI, Anthropic, DeepSeek), GitHub (`ghp_…`, `github_pat_…`), GitLab, Slack (`xox?-…`),
  AWS (`AKIA…`), Google API keys (`AIza…`), Telegram bot tokens, JWTs, Secretary `sec_`/`svc_`/`mcp_` tokens;
- PEM private key blocks.

In `context`, the value of any key that names a secret (password, secret, token, api key, authorization, cookie,
private key, credential) is replaced whatever its type. Numbers, booleans and `null` are kept (`max_tokens: 4096`).

Not scrubbed: minidumps and other binary files (they hold process memory: clients should send them only for
`crash`), file names, personal data that is not a credential (paths with the user name, prompts quoted in a log).
Clients should not attach conversation contents. The data stays on nezopt (mode 700), behind the same device auth as
the owner's other data; reports leave it only through the API to the owner's devices, Lucina and the MCP tools.

## Lucina and Claude sessions

- **MCP** (`getsecretary.tech/mcp`, scope `read`): `diagnostics_recent {app?, kind?, since?, limit?}`,
  `diagnostics_get {id, log_lines?}` (header with the time in the owner's zone, message, top 40 stack lines, context,
  repeats, attachments, the last 80 lines of the main log), `diagnostics_file {id, name, from?: tail|head, max_chars?}`
  (text only, ≤ 60 000 characters). secretary-mcp (p1) needs `SECRETARY_MCP_DIAGNOSTICS_URL=http://100.107.164.33:8850`
  and `SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE` (a copy of nezopt's `service.token`, `secretary-mcp:secretary-mcp 0600`).
- **Lucina** (lucina-rs on nezopt): tool `diagnostics` (`recent`, `get`, `file`), on when
  `~/.config/lucina-diagnostics/service.token` (or `LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE`) is readable; URL
  `LUCINA_DIAGNOSTICS_URL` (default `http://127.0.0.1:8850`). She can answer "Secretary Code crashed at 19:44: the
  background service timed out; here is why" from the report and its log.
- **Notifications**: the first report of a new fingerprint of kind `crash` or `fatal_error` sends the owner a short
  message the way memos do (app inbox `POST /v1/notify`, category `system`, `dedupKey diagnostics:<fingerprint>`, and
  Telegram from Lucina's bot): "Secretary Code crashed at 19:44: Background service timed out after 30012ms /
  1.4.2 · macOS 15.1 · nez-mbp / at startSidecar (index.js) / … diagnostics_get dg_…". At most 4 an hour
  (`LUCINA_DIAGNOSTICS_NOTIFY_PER_HOUR`); the next one says how many were held back. Repeats never notify.

## Deploy

On nezopt:

```bash
deploy/lucina-diagnostics/install.sh       # build, ~/.local/bin, tokens + diagnostics.env on first run, unit, restart
curl -s http://127.0.0.1:8850/health
systemctl --user restart lucina-rs          # Lucina picks up the `diagnostics` tool (token file now exists)
```

On p1 (root):

1. `/etc/caddy/diagnostics-gateway.key` = nezopt's `~/.config/lucina-diagnostics/gateway.token` (mode 600, owner
   caddy).
2. Add `deploy/lucina-diagnostics/Caddyfile.diagnostics` to `/etc/caddy/Caddyfile` inside `getsecretary.tech`, before
   the catch-all `handle`, then `caddy validate --config /etc/caddy/Caddyfile && systemctl reload caddy`.
3. secretary-mcp: `/etc/secretary-mcp/diagnostics.token` = nezopt's `service.token` (secretary-mcp 0600), the two
   `SECRETARY_MCP_DIAGNOSTICS_*` lines in `/etc/secretary-mcp/secretary-mcp.env`, deploy the new binary, restart.

Check from a device: `curl -H "Authorization: Bearer <device token>" https://getsecretary.tech/v1/diagnostics/reports`.

## Operations

```bash
systemctl --user status lucina-diagnostics
lucina-diagnostics list [--app secretary-code] [--kind crash] [--since 24h]   # with diagnostics.env loaded
lucina-diagnostics purge                    # retention now
lucina-diagnostics reindex                  # rebuild index.sqlite from report.json files
```

## Verification

Ingest is a single request with no protocol state across requests (no resumable upload, no state machine), so it is
covered by unit and integration tests rather than a model (`docs/verification.md`):
`cargo test -p lucina-diagnostics` (scrubbing, fingerprints, rate limits, notification budget; API: JSON, multipart,
gzip bodies and parts, caps, scrubbing on disk, dedupe including concurrent copies, `client_id` replays, listing
filters, files, retention and reindex, notifications), `cargo test -p secretary-mcp --test diagnostics` and
`cargo test -p lucina-tools --test diagnostics` (the tools against the real service).
