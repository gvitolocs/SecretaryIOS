# Lucina Agents: the `/v1/agents` contract (phase 7 builds it, phase 9 and the apps use it)

Published 2026-10-28 from branch `agents-p4`; phase 7 (branch `agents-p7`) built it and recorded its decisions
below (marked **phase 7**). The wire types are in `crates/lucina-agents` and the daemon routes in
`crates/lucina-agentd/src/api.rs` and `api/workspaces.rs`. This file pins down what secretary-server exposes, so
phase 7 (server), phase 9 (secretary-mcp) and phase 8 (apps) can work in parallel. Change it only together with
those sessions.

## Transport

- secretary-server mounts everything below under `/v1/agents`. It forwards to lucina-agentd's unix socket
  (`$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`, or `LUCINA_AGENTD_SOCKET`) with the prefix stripped:
  `/v1/agents/x` → `/x`.
- **phase 7, the path:** secretary-server runs on p1 and the daemon on nezopt, so it goes the way every other Lucina
  route goes: secretary-server → `SECRETARY_LUCINA_URL` (`http://127.0.0.1:18788` on p1, the Lucina Link tunnel to
  nezopt) → lucina-server `/api/secretary/agentd/<daemon path>` (Secretary bearer as for every `/api/secretary`
  route) → lucina-server's `agentd` module, a raw forwarder to the socket (`crates/lucina-server/src/agentd.rs`).
  lucina-server requires an `X-Lucina-Origin` of the three forms below (never empty or `local`), passes only it,
  `Content-Type`, `Accept` and `Last-Event-ID`, and streams the answer. Its own errors (`agentd_unavailable` 503
  when the daemon is down) use the daemon's error shape.
- `/v1/agents` and `/v1/agents/{id}…` map to agentd's `/agents` and `/agents/{id}…`. So agentd's
  `/agents/{id}/send` is `/v1/agents/{id}/send`, its `/permissions` is `/v1/agents/permissions`, and its `/workspaces`
  is `/v1/agents/workspaces`.
- Bodies, query strings and status codes pass through unchanged. Errors are always
  `{"error": {"code", "message"}}`, with the codes agentd uses (`AGENT_NOT_FOUND`, `PERMISSION_FORBIDDEN`,
  `WORKSPACE_GUARD`, …).
- JSON is camelCase. The shapes are Paseo 0.13.2's (`AgentSnapshot`, `AgentListItem`, `TimelineRow`,
  `PermissionRequest`), plus Lucina's fields: `gate`, `workspaceId`, and the permission book's `PermissionView`.

## Who is calling

The server authenticates the caller first, then tells agentd who it is in `X-Lucina-Origin`. It always overwrites
any value the client sent.

| Caller | Auth at the server | `X-Lucina-Origin` | May |
|---|---|---|---|
| An owner device (web, iOS, Android, Secretary Code) | device token, or web session (cookie; writes need `X-Secretary-CSRF: 1`) | `device:<deviceId>` | everything, including answering permissions, push, PR, archive and `allowMainCheckout` |
| An MCP token with scope `agents` (Hermes) | MCP bearer | `mcp:<tokenId>` | read, `agent_run`/`send`/`stop`/`archive`, workspaces. Never: answer permissions, push, open PRs, allow untrusted setup, archive workspaces, delete projects, `allowMainCheckout` (agentd refuses or clears these for `mcp`) |
| A service token with scope `permissions.request` (the Infisical broker) | service token | `service:<name>` | only `POST/GET /v1/agents/permissions[/{id}]` (external requests) |

**phase 7, how secretary-server tells them apart** (it sees secretary-mcp's requests, not the MCP's OAuth tokens):

- secretary-mcp calls `/v1/agents/*` with its own agents service token, never its device token:
  `SECRETARY_MCP_AGENTS_TOKEN_FILE` on the server (a `svc_…` token, the same content in secretary-mcp's config). It
  sends `X-Lucina-Mcp-Caller: <tokenId>:<name>` (`[A-Za-z0-9_:-]`, up to 80) and `X-Lucina-Delegation-Depth: 0|1`
  (agreed with phase 9). The origin is `mcp:<tokenId>`, or `mcp:unknown` when the header is missing or invalid,
  never `device:`. The depth is passed on to the daemon (which ignores it for now; the MCP enforces depth ≤ 1 and
  labels what it creates). The server builds the request to Lucina itself, so a client's own `X-Lucina-*` headers
  never reach the daemon.
- A device token is also `mcp:` when its device is listed in `SECRETARY_MCP_DEVICE_IDS` (secretary-mcp's backend
  device), or when the request carries `X-Secretary-Caller: mcp:<name>`. The header can only lower a caller's
  rights, never raise them.
- The broker: `SECRETARY_AGENTS_BROKER_TOKEN_FILE` (`svc_…`), origin `service:infisical-broker`. Any other path is
  403 at the server.
- Lucina's own service token (`SECRETARY_SERVICE_TOKEN_FILE`) is 403 here.
- MCP answers to permissions are refused at the server (403) and again by the daemon's gate.

- agentd treats `service:*` like `local` for creating external requests only (`peer::requester`); everywhere else it
  is `Unknown` (read only), so it never answers. The server routes service tokens only to those two paths.
- The server reaches agentd's socket as the same user. agentd's own peer check still refuses processes that
  agents started.

## Routes

The agent routes are agentd's routes, prefixed. The table below adds what phase 7 builds itself.

| Method | Path | Notes |
|---|---|---|
| GET/POST | `/v1/agents` | list (`?archived=true`) / create (`CreateRequest`: provider, cwd or workspaceId, prompt, mode, chain, verify, autoFix, toolPolicy, …) |
| GET | `/v1/agents/items` | list items with state buckets (the apps' lists) |
| GET/DELETE | `/v1/agents/{id}` | snapshot / delete |
| POST | `/v1/agents/{id}/{send,stop,archive,unarchive,wait}` | as agentd. Steer is `send` with `activeTurnBehavior: "steer"` |
| GET | `/v1/agents/{id}/timeline?afterSeq=&limit=` | `{epoch, rows, nextSeq}` |
| POST | `/v1/agents/{id}/permissions/{requestId}` | answer (owner devices only) |
| GET/POST | `/v1/agents/permissions[/{id}]` | the permission book; external requests |
| … | `/v1/agents/projects`, `/v1/agents/workspaces/…`, `/v1/agents/clone` | phase 4 routes (`api/workspaces.rs`) |
| GET | `/v1/agents/providers` | |
| GET | `/v1/agents/stream` | SSE, see below |
| PATCH | `/v1/agents/{id}` | **phase 7:** `{title}` (one line, up to 200): rename; the Mac mirror uses it |
| POST | `/v1/agents/{id}/events` | **phase 7:** `{events: [StreamEvent]}` (up to 200; not `attention_required`): an external agent's owner device posts its session's events → `{agentId, applied}`; anyone else 403 `NOT_OWNER` |
| POST | `/v1/agents/{id}/commands/{commandId}/ack` | **phase 7:** the owner device confirms an `agent_command` → `{agentId, commandId, acked}`; again is fine; another device 403, unknown 404 `COMMAND_NOT_FOUND`, expired 410 `COMMAND_EXPIRED` |
| GET | `/v1/agents/commands` | **phase 7:** `{commands: [AgentCommand]}` still waiting for the calling device (after a `stream.reset`) |

**External agents:** `POST /v1/agents` with `provider: "secretary-code"`, `external: true`, `host: "mac"` registers
one. Its `ownerDeviceId` is the registering device. Sends, steers, stops and permission answers for it become
`agent_command` events, queued for that device until it acknowledges them. Unacknowledged commands expire after
24 h, and an expired permission answer counts as denied.

**phase 7 decided:** they live in agentd, as its built-in provider `secretary-code` (`crates/lucina-agentd/src/external.rs`,
wire types and rules in `lucina-agents::external`). So they keep a record, timeline, state, attention, pushes and
the permission book like any agent, survive a daemon restart, and the queue survives with them (SQLite). Rules:

- Registering needs a device origin (`ownerDeviceId` comes from it, never from the body); `cwd` is the session's
  folder on the Mac; no prompt, chain, workspace or gate. `idempotencyKey` answers a repeat with the same agent.
- The owner device's posted events drive the lifecycle exactly as a provider's would (the device posts the user's
  messages too: the daemon does not add them).
- `send` → `{"disposition": "queued"}` and a `send` command (running: `steer` for `activeTurnBehavior: "steer"`, else
  `stop` then `send`); `stop` → a `stop` command when it is running; a permission answer goes through the gate
  (owner only, once, before expiry), then becomes a `permission_response` command, and the request stays pending
  until the device posts `permission_resolved`. Nothing changes state until the device reports it.
- Commands: `{type: "agent_command", commandId, agentId, command: {kind: send|steer|stop|permission_response, …},
  createdAt, expiresAt}`, the shape Secretary Code's mirror reads (`agents/wire.ts`). It is `agent_command`, not
  `agent.command`, like the other event types.

## Stream

`GET /v1/agents/stream` is SSE. Each event has a monotonic `id:` and `data:` set to one bus event:

- `{"type":"agent_state","agent":{…snapshot}}`
- `{"type":"agent_stream","agentId","event":{…StreamEvent},"seq","epoch","timestamp"}`
- `{"type":"agent_deleted","agentId"}`
- `{"type":"permission","permission":{…PermissionView}}`
- `{"type":"workspace","workspace":{…WorkspaceRecord}}`
- phase 7: `{"type":"agent_command","agentId","commandId","command":{…},"createdAt","expiresAt"}`, only to the owning device
- `{"type":"stream.reset"}`, sent as `event: stream.reset` with that data

Replay: a client reconnecting with `Last-Event-ID` gets what it missed. Events are kept for 24 h or 10,000 entries;
past that the client gets `stream.reset` and refetches.

**phase 7 decided:** the ids and the replay buffer are in agentd (`crates/lucina-agentd/src/journal.rs`, rules in
`lucina-agents::stream`), so a server restart loses nothing and every client sees the same ids.

- Ids are integers. Each daemon run starts at its start time in ms × 1000, so they increase across restarts and an
  id from an earlier run (or one never issued, or garbage) gets `stream.reset`, never a replay with a gap. The reset
  frame carries an id to resume from.
- `Last-Event-ID` header, or `?lastEventId=` for a new EventSource.
- A device that connects without `Last-Event-ID` first gets its waiting commands (frames without an id).
- A client that lags behind the bus catches up from the buffer, or gets a reset.
- secretary-server: at most 4 open streams per caller and 40 in all (429 `too_many_streams`); a stream ends on
  shutdown, when Lucina is quiet for 90 s (the daemon sends a keep-alive comment every 15 s), or when it ends.

## Pushes

agentd sends `POST /v1/notify` with category `agent` and `data: {kind, agentId | permissionId, reason, requestId?,
actions?}` (phase 3). The apps' notification actions (Allow/Deny) call
`POST /v1/agents/{id}/permissions/{requestId}` or `POST /v1/agents/permissions/{id}`.

## MCP tools (phase 9)

The tools are thin wrappers over these routes, with the caller's `mcp:<tokenId>` origin:

- agents: `agents_list`, `agent_run`, `agent_send`, `agent_logs`, `agent_wait`, `agent_stop`, `agent_archive`,
  `agent_inspect`, `agents_search` (phase 6);
- workspaces: `workspaces_list`, `workspace_create`, `workspace_diff`, `files_list`, `file_read`;
- terminals: `terminal_*` (phase 5);
- schedules: `schedule_*` (phase 6);
- `permissions_list` (read only), `agents_usage`.

Scope: `agents`, plus `read` for reads. Hermes gets a token with `read` + `agents`, stored in Infisical as `agents/prod`
`LUCINA_MCP_AGENTS_TOKEN`.

## Deploying phase 7 (for the coordinator)

- nezopt: install and start `lucina-agentd.service` (the unit from phase 1, not installed yet), so its socket exists
  at `$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`; lucina-rs (built from this branch's `lucina-server`) runs as the
  same user and finds it there. Add both to `~/.config/pokoin/expected-active.txt`.
- p1: secretary-server from this branch. Optional env: `SECRETARY_MCP_AGENTS_TOKEN_FILE` (phase 9 creates the token),
  `SECRETARY_AGENTS_BROKER_TOKEN_FILE` (the broker's), `SECRETARY_MCP_DEVICE_IDS` (secretary-mcp's backend device id).
  Without them, only owner devices reach `/v1/agents`.
- Caddy: nothing new; `/v1/*` already goes to secretary-server, and responses set `X-Accel-Buffering: no` and
  `Cache-Control: no-store`.
