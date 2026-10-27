# Lucina Agents: the `/v1/agents` contract (phase 7 builds it, phase 9 and the apps use it)

Published 2026-10-28 from branch `agents-p4`. The wire types are in `crates/lucina-agents` and the daemon routes in
`crates/lucina-agentd/src/api.rs` and `api/workspaces.rs`. This file pins down what secretary-server exposes, so
phase 7 (server), phase 9 (secretary-mcp) and phase 8 (apps) can work in parallel. Change it only together with
those sessions.

## Transport

- secretary-server mounts everything below under `/v1/agents`. It forwards to lucina-agentd's unix socket
  (`$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`, or `LUCINA_AGENTD_SOCKET`) with the prefix stripped:
  `/v1/agents/x` → `/x`.
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
| An owner device (web, iOS, Android, Secretary Code) | device token | `device:<deviceId>` | everything, including answering permissions, push, PR, archive and `allowMainCheckout` |
| An MCP token with scope `agents` (Hermes) | MCP bearer | `mcp:<tokenId>` | read, `agent_run`/`send`/`stop`/`archive`, workspaces. Never: answer permissions, push, open PRs, allow untrusted setup, archive workspaces, delete projects, `allowMainCheckout` (agentd refuses or clears these for `mcp`) |
| A service token with scope `permissions.request` (the Infisical broker) | service token | `service:<name>` | only `POST/GET /v1/agents/permissions[/{id}]` (external requests) |

- agentd treats `service:*` like `local` for creating external requests only. Phase 7 adds that origin (today an
  unknown value counts as `Unknown`), and the server routes service tokens only to those two paths.
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
| POST | `/v1/agents/{id}/events` | **phase 7, server-side:** an external agent (Secretary Code session) posts its timeline items; only its owner device |
| POST | `/v1/agents/{id}/commands/{commandId}/ack` | **phase 7, server-side:** the owner device confirms an `agent.command` |

**External agents:** `POST /v1/agents` with `provider: "secretary-code"`, `external: true`, `host: "mac"` registers
one. Its `ownerDeviceId` is the registering device. Sends, steers, stops and permission answers for it become
`agent.command` events, queued for that device until it acknowledges them. Unacknowledged commands expire after
24 h, and an expired permission answer counts as denied. This lives in the server, or in agentd as a provider
`secretary-code`: phase 7 decides and records it here.

## Stream

`GET /v1/agents/stream` is SSE. Each event has a monotonic `id:` and `data:` set to one bus event:

- `{"type":"agent_state","agent":{…snapshot}}`
- `{"type":"agent_stream","agentId","event":{…StreamEvent},"seq","epoch","timestamp"}`
- `{"type":"agent_deleted","agentId"}`
- `{"type":"permission","permission":{…PermissionView}}`
- `{"type":"workspace","workspace":{…WorkspaceRecord}}`
- phase 7: `{"type":"agent.command","agentId","commandId","command":{…}}`, only to the owning device
- `{"type":"stream.reset"}`

Replay: a client reconnecting with `Last-Event-ID` gets what it missed. Events are kept for 24 h or 10,000 entries;
past that the client gets `stream.reset` and refetches. agentd's own `/stream` has no ids today. Phase 7 adds the ids
and the replay buffer in the server (or in agentd, and records which here).

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
