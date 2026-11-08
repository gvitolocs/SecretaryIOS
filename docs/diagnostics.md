# Diagnostics: crash reports and log bundles

Every Lucina client sends its crashes, fatal errors, hangs and "report a problem" bundles to one place on nezopt.
Lucina, Claude Code and the other MCP clients can then read them and explain what happened. The owner gets a short
note when something new crashes.

```
Secretary Code / iOS / Android / web ──HTTPS──► p1 Caddy /v1/diagnostics* ──tailnet──► lucina-diagnostics (nezopt :8794)
  report JSON + logs + minidumps          forward_auth to secretary-server        scrub ─► fingerprint ─► store / count repeat
                                          (/v1/avatar) + X-Diagnostics-Gateway    ~/data/lucina-diagnostics + index.sqlite
servers on nezopt ──Bearer service token──► 127.0.0.1:8794                        new crash ─► /v1/notify + Telegram
secretary-mcp (p1) ──tailnet, service token──► diagnostics_recent / _get / _file
lucina-rs ──127.0.0.1, service token──► tool `diagnostics`
```

## Pieces

| Piece | Where |
| --- | --- |
| Service, CLI | `crates/lucina-diagnostics` (binary `lucina-diagnostics`), user unit `lucina-diagnostics.service`, `deploy/lucina-diagnostics/` |
| Config | `~/.config/lucina-diagnostics/diagnostics.env` (template `deploy/lucina-diagnostics/diagnostics.env.example`) plus Lucina's `~/.config/lucina-rs/node.env` (secretary-server URL and service token, Telegram chat) |
| Gateway | p1 `/etc/caddy/Caddyfile`, block `@diagnostics` (`deploy/lucina-diagnostics/Caddyfile.p1`); key `/etc/caddy/diagnostics-gateway.key` = nezopt `~/.config/lucina-diagnostics/gateway.token` |
| MCP tools | `crates/secretary-mcp/src/diagnostics.rs`: `diagnostics_recent`, `diagnostics_get`, `diagnostics_file` (scope `read`) |
| Lucina's tool | `crates/lucina-tools/src/tools/diagnostics.rs` (`diagnostics`: recent, get, file) |
| Tests | `crates/lucina-diagnostics/tests/api.rs`, `crates/secretary-mcp/tests/diagnostics.rs` |

## API contract

All routes need auth, like `/v1/memos`. Clients go through p1, where Caddy checks the device token or session cookie
with secretary-server and adds `X-Diagnostics-Gateway`. Local callers send `Authorization: Bearer <service.token>`.
A cookie-authenticated POST also needs `X-Secretary-CSRF: 1`. `GET /health` needs no auth.

Errors are `{"error": {"code", "message"}}` with codes `bad_request` (400), `unauthorized` (401), `csrf` (403),
`not_found` (404), `too_large` (413), `unsupported` (415), `rate_limited` (429, with `Retry-After` and `retry_after`)
and `internal` (500).

### `POST /v1/diagnostics/reports`

The body is either:

- `Content-Type: application/json`: the report alone; or
- `Content-Type: multipart/form-data`: a part named `report` (the report JSON, without a filename) and one part per
  attachment (any field name, e.g. `file`, **with** a filename). Any other part without a filename gets 400.

Either form may be sent with `Content-Encoding: gzip`. Each attachment may also be gzip-compressed on its own. The
service recognises that by a `.gz` filename, a gzip content type or the gzip magic bytes, stores the file
decompressed, and strips `.gz` from the name.

The report (snake_case; the camelCase aliases `appVersion` and `occurredAt` are also accepted; unknown fields get 400):

| Field | Type | |
| --- | --- | --- |
| `app` | string, required | `secretary-code`, `ios`, `android`, `web` or `server` |
| `app_version` | string, required | 1–64 characters, e.g. `0.9.3` or `2026.10.26+abc123` |
| `os` | string | ≤ 128, e.g. `macOS 26.1 (arm64)`, `iOS 26.0.1`, `Android 16`, a user agent family |
| `device` | string, required | ≤ 128, a stable per-install id (not a person's name). Used for the per-device rate limit. |
| `kind` | string, required | `crash`, `fatal_error`, `error`, `user_report` or `hang` |
| `message` | string, required | the error message; longer than 8 KiB is cut |
| `stack` | string | the stack trace as text (JS, Swift, Kotlin/Java, Rust, native); longer than 128 KiB is cut |
| `context` | object | free JSON, e.g. `{"window": "main", "route": "/session/…", "engine_version": "1.4.2", "uptime_s": 812}`. A non-object is wrapped as `{"value": …}`. Over 64 KiB serialized, it is replaced by `{"truncated": true, "preview": "…"}`. |
| `occurred_at` | RFC 3339 | when it happened; default: when received. A time in the future is clamped to the receive time. |

Attachments: log files, JSON dumps, iOS `.ips` files and Crashpad minidumps (`.dmp`). Up to **16 files and 20 MiB
together after decompression**. A body over 21 MiB, compressed or not, gets 413. A gzip that inflates past the cap
stops at the cap and gets 413. Nothing of a refused report is stored. Names are reduced to `[A-Za-z0-9._-]`, at most 100
characters, without directories. Duplicates become `name-2.ext`, and `report.json` becomes
`attachment-report.json`.

Answers:

- `201 {"id": "dg_0123456789abcdef", "duplicate": false, "count": 1}`: a new report.
- `200 {"id": "<the stored report>", "duplicate": true, "count": n}`: a repeat. Only the counter, the last-seen time
  and the last device change; the repeat's own attachments are dropped.

The client should keep a report in its queue until it gets 200 or 201. On 429 it should wait for `Retry-After`. On
400, 413 or 415 it should drop the report, or retry without its attachments.

### `GET /v1/diagnostics/reports?app=&kind=&device=&since=&before=&limit=`

`since`: RFC 3339, `YYYY-MM-DD` (owner's zone), or `30m` / `24h` / `7d` back from now; it filters on the last time a
report was seen. `limit` 1–200 (default 50). `before` is the `next` cursor of the previous page. Empty parameters are
ignored, and an unknown `app` or `kind` gets 400.

```json
{"reports": [{"id": "dg_…", "app": "secretary-code", "app_version": "0.9.3", "os": "macOS 26.1", "device": "mac-1",
  "kind": "crash", "message": "first 500 characters", "top_frame": "waitReady (index.js)", "fingerprint": "32 hex",
  "occurred_at": "…", "received_at": "…", "count": 3, "last_seen_at": "…", "last_device": "mac-2",
  "attachments": 2, "bytes": 81234}],
 "next": "1793040240000_17"}
```

Reports come newest `last_seen_at` first; `next` is `null` on the last page.

### `GET /v1/diagnostics/reports/{id}`

```json
{"report": {"id", "app", "app_version", "os", "device", "kind", "message", "stack", "context", "occurred_at",
            "received_at", "fingerprint", "top_frames": ["…"], "attachments": [ … ], "redactions": 2,
            "truncated": ["stack"]},
 "count": 3, "last_seen_at": "…", "last_device": "…",
 "attachments": [{"name": "main.log", "size": 1234, "content_type": "text/plain; charset=utf-8", "text": true,
                  "gzip": true, "sha256": "…", "url": "/v1/diagnostics/reports/dg_…/files/main.log"}]}
```

### `GET /v1/diagnostics/reports/{id}/files/{name}[?tail=N | ?head=N]`

Returns the attachment as `Content-Disposition: attachment`, with `X-Content-Type-Options: nosniff` and
`Cache-Control: private, no-store`. Text files are served as `text/plain; charset=utf-8` and binary files as
`application/octet-stream`. On a text file, `tail` or `head` returns only the last or first N bytes, cut to whole
characters, with an `X-Slice` header. `X-Full-Size` always gives the file's full size.

## Clients

- **Secretary Code** (Electron): start `crashReporter` with `uploadToServer: false`. On the next launch, send each
  new dump in `app.getPath('crashDumps')` as kind `crash`, attaching the minidump and the tail of the main and engine
  logs (gzip). Send uncaught main-process exceptions as `fatal_error`, renderer `error` events as `error`, a sidecar
  that never becomes ready as `hang`, and "Report a problem" as `user_report`. Put the window, route and engine version
  in `context`. Do the upload in the main process with the device token; the renderer never sees the token or the
  sidecar password.
- **iOS**: MetricKit `MXCrashDiagnostic` / `MXHangDiagnostic` payloads (JSON attachment) as `crash` / `hang`;
  `.ips` files when available.
- **Android**: `ApplicationExitInfo` (`REASON_CRASH`, `REASON_ANR` → `hang`) with its trace as an attachment.
- **Web**: `error` / `unhandledrejection` as `error`, at most a few per page load.
- **Servers** on nezopt: POST to `127.0.0.1:8794` with the service token, `app: server`, `device`: the service name.

The `device` should be stable per install and anonymous, e.g. a UUID kept in app storage.

## Dedupe

Fingerprint = SHA-256 of `app`, `app_version`, the error type (the first stack line up to `:`, e.g. `TypeError`) and the
**top 5 normalized frames**. Normalizing removes frame indices, hex addresses, `+ offset`s, line and column numbers,
directories (POSIX, Windows, `file://`, `app.asar`), query strings and content hashes in bundle names
(`index-3f9a1c2b.js` → `index.js`). The same fault on another Mac, install path or build of the same version therefore
merges. Without frame lines the first stack lines are used; without a stack, the message's first line with numbers
replaced by `#`.

A report whose fingerprint and kind match a stored one is a repeat: `count + 1`, new `last_seen_at` and
`last_device`, nothing else stored. `user_report`s are never merged. A new version is a new fingerprint.

## Rate limits

- **Per device** (`app:device`): a token bucket with a burst of 20, then 60 an hour (`LUCINA_DIAGNOSTICS_RATE_BURST`,
  `…_RATE_PER_HOUR`). Repeats count too, so a crash loop cannot flood the service; the client retries later.
- **p1**: the `forward_auth` subrequest counts every request against the device's 60 a minute.
- **Notifications**: at most one per app every 10 minutes (`LUCINA_DIAGNOSTICS_NOTIFY_MIN_INTERVAL_SEC`); the ones held
  back are mentioned in the next ("+2 other new problem(s) since the last notice").

## Notifications

A **new fingerprint** of kind `crash` or `fatal_error` sends the owner a note through the same path memos use:
secretary-server `POST /v1/notify` (category `lucina_message`, channel `diagnostics`, `dedupKey`
`diagnostics:<fingerprint>`, `data.reportId`) and Telegram from Lucina's bot:

```
Secretary Code crashed at 19:44: the background service timed out after 30000 ms
at waitReady (index.js)
v0.9.3 · macOS 26.1 (arm64) · 2 attachment(s)
Ask Lucina about it, or diagnostics_get dg_….
```

Repeats, `error`, `hang` and `user_report` never notify. `LUCINA_DIAGNOSTICS_NOTIFY_APP=off` and
`…_NOTIFY_TELEGRAM=off` turn off one channel each.

## Agents

- **Lucina** (lucina-rs tool `diagnostics`): `recent` (app, kind, since), `get` (report plus the last 6 000 characters
  of its main log) and `file`. Asked "why did Secretary Code crash?", she reads the latest crash and answers like
  "Secretary Code crashed at 19:44: the background service timed out; the log shows the sidecar never answered
  /health". It is on when `LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE` (or `~/.config/lucina-diagnostics/service.token`) is
  readable; `LUCINA_DIAGNOSTICS_URL` defaults to `http://127.0.0.1:8794`.
- **MCP** (secretary-mcp, scope `read`): `diagnostics_recent {app?, kind?, since?, limit?, before?}`,
  `diagnostics_get {id, log_tail?}`, `diagnostics_file {id, name, from_end?, max_chars?}` (text only, ≤ 60 000
  characters). They are listed only when `SECRETARY_MCP_DIAGNOSTICS_URL` is set.

## Privacy: what is stored

Stored on nezopt only, under `~/data/lucina-diagnostics` (mode 700), for **90 days after a report was last seen**
(`LUCINA_DIAGNOSTICS_RETENTION_DAYS`; an hourly sweep deletes the directory and the index row). Nothing is sent
anywhere else, except the one-line crash notification to the owner's own inbox and Telegram.

Per report:

- the fields above (`app`, `app_version`, `os`, `device` id, `kind`, `message`, `stack`, `context`, times), after
  scrubbing;
- text attachments (logs, JSON, `.ips`), decompressed and scrubbed;
- binary attachments (minidumps, images), **byte for byte as sent**. A minidump holds memory of the crashed process
  and can contain anything that was in it, including secrets, so they are only downloadable through the authenticated
  API and never shown to Lucina or the MCP tools;
- in the index: the counters, last device and last-seen time of repeats, and a SHA-256 of each attachment.

Not stored: IP addresses, user agents, request headers, the device token, or the repeats' own payloads.

**Scrubbing** (before anything is written, on `message`, `stack`, every string in `context` and every text
attachment) replaces with `[redacted]`:

- `Bearer …`, `Basic …` (the engine's sidecar sends `Basic base64(opencode:<password>)`), `Token …`, `Digest …`;
- credentials in URLs: `scheme://user:password@host` → `scheme://[redacted]@host`;
- the value of any secret-looking key in `key=value`, `key: value` or JSON: names ending in `password`, `passwd`,
  `passphrase`, `secret`, `token`, `api_key` / `apikey`, `access_key`, `private_key`, `authorization`, `auth`, `cookie`,
  `credential(s)`. This covers the engine's local service password (`OPENCODE_SERVER_PASSWORD=…`,
  `"password": "…"`). `max_tokens` or `tokens: 12` are not secrets and are kept, as are `null` / `true` / `false`;
- `--password` / `--token` / `--api-key` / `--secret` flags, and the `?key=`, `sig=`, `signature=`, `code=`, `auth=`
  query parameters;
- API-key shapes: `sk-…` (OpenAI, Anthropic, DeepSeek), `xai-…`, GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`,
  `github_pat_`), Slack `xox?-`, AWS `AKIA…`, Google `AIza…`, Stripe `sk_live_`/`rk_`/`pk_`, GitLab `glpat-`,
  Hugging Face `hf_`, Telegram bot tokens, JWTs (`eyJ….….…`), and Secretary's own `sec_`, `svc_`, `mcp_`, `mcpa_`
  tokens;
- PEM private key blocks.

In `context`, a secret-named key's whole value is replaced. Each report records how many replacements were made
(`redactions`). The scrubber removes the obvious shapes only: clients must still not put personal content (chat text,
note bodies, file contents) in reports.

## Storage

```
~/data/lucina-diagnostics/
  index.sqlite                                  reports(id, app, …, fingerprint, count, last_seen_at, dir)
  .incoming/                                    reports being written (renamed into place when complete)
  <app>/<YYYY-MM-DD>/<id>/report.json           the first occurrence, scrubbed
  <app>/<YYYY-MM-DD>/<id>/<attachment>…
```

## Deploy

The steps below are manual. Neither this change nor `install.sh` touches p1.

1. **nezopt**: `deploy/lucina-diagnostics/install.sh`. It builds, installs `~/.local/bin/lucina-diagnostics`, creates
   `~/.config/lucina-diagnostics/{diagnostics.env,gateway.token,service.token}` and `~/data/lucina-diagnostics` on the
   first run, then installs, enables and starts `lucina-diagnostics.service`. Check it with
   `curl -fsS 127.0.0.1:8794/health`.
2. **p1 gateway key**: copy nezopt's `gateway.token` to `/etc/caddy/diagnostics-gateway.key` (owner `caddy`, mode 600).
3. **p1 Caddy**: paste `deploy/lucina-diagnostics/Caddyfile.p1` into the `secretary_app` snippet next to `@memos`,
   before the catch-all `handle`. Then run `caddy validate --config /etc/caddy/Caddyfile` and
   `systemctl reload caddy`. Check: `curl -s https://getsecretary.tech/v1/diagnostics/reports` answers 401 without a
   token.
4. **p1 secretary-mcp** (optional): copy nezopt's `service.token` to `/etc/secretary-mcp/diagnostics.token`
   (`secretary-mcp:secretary-mcp 0600`), then set `SECRETARY_MCP_DIAGNOSTICS_URL=http://100.107.164.33:8794` and
   `SECRETARY_MCP_DIAGNOSTICS_TOKEN_FILE=/etc/secretary-mcp/diagnostics.token` in `/etc/secretary-mcp/secretary-mcp.env`.
   Install the new binary and restart it.
5. **lucina-rs**: the `diagnostics` tool turns on at the next restart, once
   `~/.config/lucina-diagnostics/service.token` exists.

## Operations

```bash
systemctl --user status lucina-diagnostics
lucina-diagnostics list [--app secretary-code] [--kind crash] [--since 24h]   # with diagnostics.env loaded
lucina-diagnostics show dg_…
lucina-diagnostics prune                                                      # retention now
deploy/lucina-diagnostics/install.sh                                          # build, install, restart
```
