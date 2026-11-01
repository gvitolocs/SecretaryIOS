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
| GET | `/v1/agents/{id}/timeline?afterSeq=&limit=` | `{epoch, rows, nextSeq}`. `&subagent=<id>` reads a provider subagent's timeline instead (below) |
| GET | `/v1/agents/{id}/provider-subagents` | **P7:** `{agentId, subagents: [descriptor]}`, oldest first |
| GET | `/v1/agents/{id}/provider-subagents/{subId}` | **P7:** one descriptor (id or unique prefix) |
| GET | `/v1/agents/{id}/provider-subagents/{subId}/timeline?afterSeq=&limit=&direction=after\|tail` | **P7:** a page of the child's timeline (below) |
| POST | `/v1/agents/{id}/permissions/{requestId}` | answer (owner devices only) |
| GET/POST | `/v1/agents/permissions[/{id}]` | the permission book; external requests |
| … | `/v1/agents/projects`, `/v1/agents/workspaces/…`, `/v1/agents/clone` | phase 4 routes (`api/workspaces.rs`) |
| GET | `/v1/agents/providers` | |
| GET | `/v1/agents/stream` | SSE, see below |
| PATCH | `/v1/agents/{id}` | **phase 7:** `{title}` (one line, up to 200): rename; the Mac mirror uses it |
| POST | `/v1/agents/{id}/events` | **phase 7:** `{events: [StreamEvent]}` (up to 200; not `attention_required`): an external agent's owner device posts its session's events → `{agentId, applied}`; anyone else 403 `NOT_OWNER` |
| POST | `/v1/agents/{id}/commands/{commandId}/ack` | **phase 7:** the owner device confirms an `agent_command` → `{agentId, commandId, acked}`; again is fine; another device 403, unknown 404 `COMMAND_NOT_FOUND`, expired 410 `COMMAND_EXPIRED` |
| GET | `/v1/agents/commands` | **phase 7:** `{commands: [AgentCommand]}` still waiting for the calling device (after a `stream.reset`) |
| GET/POST | `/v1/agents/schedules` | **phase 6:** `{schedules: [Schedule]}` / create → Schedule (201). Body: Paseo's (`prompt, name?, cadence, target, maxRuns?, expiresAt?, runOnCreate?`) or the short form (`cron, timezone?` or `everyMs`; `provider, model?, modeId?, cwd \| workspaceId, isolation?, archiveOnFinish?` for a new agent, `agentId` for a heartbeat; `title` = name; `labels`, `expiresInMs`) |
| GET/PATCH/DELETE | `/v1/agents/schedules/{id}` | **phase 6:** one schedule with its runs / `{name, prompt, cadence \| cron \| timezone \| everyMs, maxRuns, expiresAt, newAgentConfig}` (null clears) / `{scheduleId, deleted}` |
| GET | `/v1/agents/schedules/{id}/logs` | **phase 6:** `{runs}`, oldest first |
| POST | `/v1/agents/schedules/{id}/{pause,resume,run-once}` | **phase 6:** → Schedule; 409 `SCHEDULE_CONFLICT` when completed or already running |
| GET | `/v1/agents/search?q=&limit=&archived=` | **phase 6:** `{query, results: [{agentId, title, provider, status, cwd, updatedAt, archivedAt?, score, matches: [{field: title\|workspace\|branch\|project\|timeline, seq?, kind?, snippet}]}]}` |
| GET | `/v1/agents/{id}/prompts` | **phase 6:** `{epoch, prompts: [{seq, timestamp, preview, messageId}]}` (Paseo's prompt index; `messageId` is Lucina's: the id to rewind to) |
| POST | `/v1/agents/{id}/detach` | **phase 6:** drop the parent labels → snapshot |
| GET | `/v1/agents/usage` | **phase 6:** `{providers: [{provider, agents, running, inputTokens, cachedInputTokens, outputTokens, totalCostUsd}], schedules: {active, paused, completed}, since}` |
| POST | `/v1/agents/{id}/rewind` | **A16:** `{messageId, mode: conversation\|files\|both, allowMainCheckout?}` → `{agentId, messageId, mode, ok, epoch, removedRows}`; see "Rewind and fork" |
| POST | `/v1/agents/{id}/fork-context` | **A16:** `{boundaryCursor?: {epoch, seq}, boundaryMessageId?}` → Paseo's `agent.fork_context.response` payload without the request id: `{agentId, attachment, itemCount, boundaryCursor, boundaryMessageId, error: null}` |
| POST | `/v1/agents/{id}/fork` | **A16:** `{boundaryCursor?, boundaryMessageId?, prompt?, title?, provider?, model?, modeId?, thinkingOptionId?, cwd?, workspaceId?, labels?, idempotencyKey?, allowMainCheckout?}` → `{agent: snapshot, forkedFrom, itemCount, boundaryCursor, boundaryMessageId}` |

**External agents:** `POST /v1/agents` with `provider: "secretary-code"`, `external: true`, `host: "mac"` registers
one. Its `ownerDeviceId` is the registering device. Sends, steers, stops and permission answers for it become
`agent_command` events, queued for that device until it acknowledges them. Unacknowledged commands expire after
24 h, and an expired permission answer counts as denied.

Claude sessions (provider `claude-session`) are external agents without an owner device: a hook on their host reports
for them through `lagent session beat`, and only `send` reaches them (`docs/design/agents-pipeline.md`, section 6).

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

## Terminals, scripts and services (phase 5)

Daemon routes in `crates/lucina-agentd/src/api/terminals.rs`, PTYs and scripts in `src/terminals/`. The p1→nezopt hop is plain TCP over Tailscale (`SECRETARY_LUCINA_URL`), which would carry an upgrade. The two
forwarders do not: secretary-server proxies with reqwest, and lucina-server's agentd forwarder refuses `101`. So
terminal output is SSE and input is a POST. A WebSocket would need upgrade support in both forwarders and in agentd
(a follow-up if the apps need it).

| Method | Path | Notes |
|---|---|---|
| GET/POST | `/v1/agents/terminals` | list (`?workspaceId=`, `?cwd=`) / create `{workspaceId, cwd?, name?, command?, args?, rows?, cols?, agentId?}` |
| GET/PATCH/DELETE | `/v1/agents/terminals/{id}` | id, unique prefix or name / `{title}` / kill (`{terminalId, success}`) |
| GET | `/v1/agents/terminals/{id}/capture` | `?start=&end=&scrollback=true&stripAnsi=false` → `{terminalId, lines, totalLines}` |
| POST | `/v1/agents/terminals/{id}/input` | `{data}` raw, or `{keys: "C-c" \| ["echo hi","Enter"], literal?}` (tmux-style tokens) → `{terminalId, keysSent}` |
| POST | `/v1/agents/terminals/{id}/resize` | `{rows, cols}` |
| POST | `/v1/agents/terminals/{id}/paste` | `{name, dataBase64}`: saved on nezopt, and its quoted path is typed in (≤ 700 KB through p1) |
| GET | `/v1/agents/terminals/{id}/stream` | SSE (below) |
| GET | `/v1/agents/workspaces/{id}/scripts` | `{workspaceId, scripts: [Script]}`: Paseo's `script_status_update` payloads plus `command`, `source`, `servicePath` |
| POST | `/v1/agents/workspaces/{id}/scripts/{name}/{start,stop}` | → `Script` |
| GET | `/v1/agents/workspaces/{id}/scripts/{name}/logs` | `?lines=` → the last lines of the script's terminal |
| GET | `/v1/agents/workspaces/{id}/services` | running script services and **detected** ports, named `port-<n>` |
| any | `/v1/agents/workspaces/{id}/services/{name}/{path}` | HTTP proxy to `127.0.0.1:<port>`; **owner devices only** |

### Terminal stream

- The first event is `event: snapshot`, with `{rows, cols, data, offset}`.
- Then `event: output`, with `id: <byte offset>` and `{b64}`.
- Last, `event: exit`, with `{exitCode}`.
- To resume, send `Last-Event-ID` or `?offset=`. The daemon keeps the last 1 MiB; an older offset gets a new snapshot.

### Scripts

- **Where they come from:** `paseo.json` `scripts`, read unchanged, plus `.lucina/agents.toml` `[scripts.<name>]`
  (`cmd`, `port = "auto" | n`, `health = "/path"`).
- **Ports:** the script's own; else `worktree.servicePorts` (`range` or `portScript`); else 4100-4999. A port that
  another running script holds is never handed out.
- **How they run:** as `/bin/sh -c` in a terminal of their own, inside
  `systemd-run --user --scope -p CPUQuota=200%`. `LUCINA_AGENTD_SCRIPT_CPU_QUOTA` changes the quota, and
  `LUCINA_AGENTD_SCRIPT_SCOPE=off` turns the scope off.
- **Env:**
  - Paseo's: `PASEO_PORT`, `HOST`, `PASEO_URL`, `PASEO_SERVICE_<NAME>_{PORT,URL}`, …, with `LUCINA_*` aliases;
  - `PORT`;
  - `LUCINA_SERVICE_PATH`: the base path to give the dev server.
- **Health:** every 5 s, a GET of the `health` path, else a TCP connect.
- **`proxyUrl`:**
  - the Secretary URL (`$LUCINA_AGENTD_PUBLIC_URL`) plus the service path, when that variable is set;
  - else `<label>.localhost:<port>`, when agentd runs with `--listen`;
  - else the bare path.

### The proxy

- Requests to the service lose cookies, credentials, `X-Lucina-*` and `X-Secretary-*`.
- Responses lose `Set-Cookie` and get
  `Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads`. The dev app
  runs with an opaque origin, so it can't use the owner's session.
- An absolute `Location` is moved under the service path.
- **Limit:** cookie-authenticated writes still need `X-Secretary-CSRF: 1`. A dev app's own POSTs from the browser are
  refused; GET and HEAD work.
- **Limit:** no WebSocket or HMR through p1.
- On agentd's loopback `--listen` port, `Host: <label>.localhost` reaches the same service (Paseo's hostname
  routing, HTTP only).

### Who may do what

- **Owner devices and `lagent`:** everything.
- **An agent** (a process under its provider):
  - the terminals it serves or opened;
  - new terminals in its own workspace, inside the workspace directory (symlinks resolved);
  - that workspace's scripts.
- **An MCP token:**
  - lists and reads the terminals of the agents it started (label `lucina.mcp-token`) and of the scripts it ran;
  - types only into the terminals that it or its agents opened, never one the owner opened for its agent;
  - uses the scripts of workspaces it made or runs agents in. agentd labels the workspaces it makes `mcp:<hash>`,
    and only the owner can set that label by hand;
  - never creates or kills a terminal, and never opens a service.
- **A process in a terminal that no agent owns:** only that terminal.
- Every terminal process counts as an agent process for permission answers: `lagent permit` inside a terminal is
  refused.

### Cleanup

- An agent's terminals are killed when it is stopped, archived or deleted.
- Archiving a workspace stops its scripts and terminals before teardown.
- agentd's shutdown kills every terminal.
- Terminal pids are in the process registry, so the next start reaps the ones a crashed daemon left behind.

## Schedules, subagents, search (phase 6)

- **Schedules** live in agentd's SQLite (`schedules`), wire type `lucina-agents::schedule::Schedule` (Paseo's
  `StoredSchedule` plus `labels` and `createdBy`). The scheduler ticks every second; next runs follow Paseo's
  `computeNextRunAt` (whole minutes in the cadence's zone; checked against Paseo on 143 cases). A short-form `cron`
  without a zone runs in the owner's zone (`LUCINA_AGENTD_TIMEZONE`, else `LUCINA_TIMEZONE`, else Europe/Rome).
  A restart fails the run it cut off and skips missed fires.
- **Ownership:** `createdBy` is the caller's origin (`local`, `device:<id>`, `mcp:<tokenId>`, `agent:<id>`). The
  owner's devices and `lagent` change every schedule; an MCP token or an agent only its own (403
  `SCHEDULE_FORBIDDEN`). The MCP checks too (`labels["lucina.mcp-token"]`). Schedules from an MCP token or an agent
  never run a provider without permission prompts or a mode that skips them (the daemon picks a safe mode).
- **Heartbeats** are schedules whose target is an agent: only that agent (`agentId: "self"`, from inside it) or the
  owner sets them. A fire is a `<lucina-system>` notice sent with steer, hidden on the timeline, and its turn doesn't
  push "finished". Archiving the agent completes them.
- **Subagents:** a process an agent started acts as that agent (agentd reads `LUCINA_AGENT_ID` from its provider's
  launch environment). Its `POST /agents` makes a child labelled `lucina.parent-agent-id` and
  `lucina.delegation-depth`; a child (depth 1) can't start agents (403 `DELEGATION_DEPTH`), as in lucina-delegate.
  An agent may stop, archive, rename, delete or detach only its children (403 `NOT_YOUR_AGENT`), and message only
  its children and parent: `send` from an agent returns `{disposition: "queued", messageId}` and arrives after 2 quiet
  seconds as a `<lucina-system kind="agent-message">` envelope (an `agent_message` tool call on the timeline).
- **Notify on finish:** the parent hears once when the child finishes, errors, is closed or needs permission: one
  `<lucina-system kind="agent-notification" event="…">` message with the child's last answer (Paseo's text, 4,000
  characters). Kept in SQLite (`finish_subs`), so a restart doesn't lose it. A child its parent hears about doesn't
  push "finished" to the phone; its permission pushes always go. Text from the owner that looks like an envelope is
  quoted as the owner's (`kind="user-message"`).
- **Agents' MCP:** agentd attaches `lagent mcp` (server `lucina-agents`, tools `create_agent`, `send_agent_prompt`,
  `list_agents`, `get_agent_status`, `get_agent_activity`, `wait_for_agent`, `cancel_agent`, `archive_agent`,
  `detach_agent`, `create_heartbeat`, `create_schedule`, `list_schedules`, `delete_schedule`, `delete_heartbeat`,
  `search_agents`) to Claude (`--mcp-config`) and ACP agents (`mcpServers`), and preapproves them: the daemon
  enforces the rules above. Codex isn't attached yet. `LUCINA_AGENTD_MCP=0` turns it off.
- **Search:** Paseo's typo-tolerant ranking over title, workspace, branch and project, plus SQLite FTS5 over prompts,
  answers and agent messages (indexed when an agent is idle).

## Provider subagents (P7)

A provider can run children inside an agent: a Claude `Task` call, an OpenCode child session. Paseo gives each its own
timeline, apart from the parent's (`ProviderSubagentStore`, `agent.provider_subagents.*` messages); so does agentd. They
are **not** `lucina.parent-agent-id` children (those are separate agents that an agent started through the MCP, phase 6).

- **Descriptor** (Paseo's `ProviderSubagentDescriptorPayloadSchema`): `{id, parentAgentId, parentSubagentId, provider, title,
  description, status, createdAt, updatedAt, toolCallId, cwd, subtitle}`. `id` is the child's own id (the `Task` call id for
  Claude, the child session id for OpenCode); `title` is its name or type (a `Task` input `name` beats `subagent_type`),
  `description` the task, `status` `running | completed | failed | canceled`, `toolCallId` the parent timeline's tool call
  that started it, `parentSubagentId` the child that started this one, `subtitle` provider-formatted context
  (`general-purpose · claude-opus-5-5 · 5.1k tokens`; clients must not parse it). Fields an update omits keep their value.
- **Timeline:** `{agentId, subagentId, provider, direction, epoch, rows, nextSeq, window: {minSeq, maxSeq, nextSeq},
  hasOlder, hasNewer, subagent}`. Rows are `TimelineRow`s as the agent's own, numbered from 1 per child, and use the agent's
  `epoch`. `direction=after` (default) returns the rows after `afterSeq` (`limit` 0 = all); `tail` the last `limit`.
  Unlike Paseo they are not projected: a client collapses them like the agent's own rows (`projection`). A child's items:
  its prompt (`user_message`), `assistant_message`, `reasoning`, tool calls and `todo`. A child started by another child has
  that child's `Task` card in the parent child's timeline.
- **In the parent's timeline** there is only the provider's card for the call: a `tool_call` with a `sub_agent` detail
  (`subAgentType`, `description`, `log`). `log` lists what the child did (`[Bash] ls`, at most 200 lines) and, once it has
  finished, its report. This is all `lagent logs <agent>` and the MCP's activity text show of a child, as in Paseo.
- **Errors:** 404 `PROVIDER_SUBAGENT_NOT_FOUND`; an ambiguous prefix is 400 `INVALID_REQUEST`; an unknown `direction` is 400.
- **Stream:** `{"type":"provider_subagent","event":{...}}` with `kind` `upsert` (`subagent`: the descriptor), `timeline`
  (`parentAgentId`, `subagentId`, `provider`, `item`, `timestamp`, `seq`, `epoch`) or `remove` (`parentAgentId`, `subagentId`),
  Paseo's `agent.provider_subagents.update` payload. A child's items never come as `agent_stream` events of the parent.
- **Lifecycle:** a running child is `canceled` when its agent is archived or the daemon restarts (its provider process is
  gone); `delete` removes the agent's children (a `remove` event each). Stored in SQLite (`provider_subagents`,
  `provider_subagent_rows`); at most 5,000 rows per child.
- **`lagent`:** `logs <agent> --subagent <id|prefix>` prints the child's activity (not with `--follow`); `inspect` adds a
  `Subagents` list when there are any (Lucina only); `lagent mcp`'s `get_agent_activity` takes `subagentId`.
- **Claude** (stream-json): frames with `parent_tool_use_id` go to the child; identity and status come from Claude Code's task
  protocol (`task_started`, `task_updated`, `task_notification`, `task_progress`), as in Paseo; an older CLI without it
  declares the child on its first frame and the `Task` result ends it. A foreground child ends with its call's result, a
  background one outlives the turn. **OpenCode** (ACP): a `session/update` from another session id is a child, linked to the
  running `task` call; it is not checked against a live OpenCode (it may not forward those updates). Codex, Cursor, Copilot, Pi
  and ZCode report no children.
- **Differences from Paseo:** no timeline projection; an explicit `null` can't clear a descriptor field; a Claude child's text is
  taken from complete `assistant` messages (Paseo's tracker does the same); no `workflow` output files and no effort in
  the subtitle (Paseo reads it from Claude hooks).

## Rewind and fork (A16)

Paseo's CLI has neither; they are app operations there (`agent.rewind.request`, `agent.fork_context.request`). The routes
above are their equivalents, and `lagent rewind <id> --to <message> [--mode conversation|files|both]` and
`lagent fork <id> [prompt] [--at <boundary>]` call them. Code: `crates/lucina-agentd/src/manager/rewind.rs`,
`api/rewind.rs`, `cli/rewind.rs`; the pure parts (modes, target lookup, the attachment) in `lucina-agents`
(`rewind.rs`, `fork_context.rs`).

**Rewind** takes an agent back to just before one of its user messages. The target is a user message's `messageId` (or the
`clientMessageId` it came with); every user message has one (the manager gives the ones it records an id, a provider that
echoes the message brings its own), and the prompt index lists them. A rewind removes the target and everything after it, so
the owner can send the message again, edited.

| Mode | The provider reverts | The timeline |
|---|---|---|
| `conversation` | its conversation state | rows from the target on are removed, under a new epoch (`timeline_replacement` on the stream); the stored timeline is rewritten; the search index forgets the agent |
| `files` | the files the agent changed since the target | unchanged |
| `both` | files first, then the conversation, as one operation | as `conversation` |

Order of checks, none of which touches the agent until the last: the caller owns the agent (below); the provider has the
capability flag for the mode (Paseo's `supportsRewindConversation|Files|Both`, read from the provider, not from a closed
agent's stored flags); the target is a user message of this agent; the agent isn't in a lane chain; for a mode that changes
files, the file rules (below). Then the agent is held (`AGENT_BUSY` for a new prompt or a second rewind), a finished-run
check in progress is waited for (30 s at most), a running turn is cancelled, a closed agent is loaded, and the provider is
asked. A provider error (`PROVIDER_ERROR`, 502) leaves the timeline as it was.

**Providers.** A mode needs the provider's flag *and* its session's `rewind` implementation. Paseo 0.13.2 supports:

| Provider | conversation | files | both |
|---|---|---|---|
| claude | yes | yes | yes |
| codex | yes | no | no |
| opencode | no | no | yes |
| pi, omp | yes | no | no |
| copilot, cursor, other ACP | no | no | no |
| mock | yes | yes | yes |

Lucina implements **`mock` only** (all three modes, with real file checkpoints); every real provider reports all three flags
off and is refused with `422 REWIND_UNSUPPORTED`, "Provider does not support rewinding \<mode\>" (Paseo's text). External
agents (`secretary-code`) are refused too: the session is rewound on the Mac. Enabling a real provider means implementing
`Session::rewind` for it and setting its flags, each with its own test against the real CLI.

**Who may rewind what.**

| Caller | Rewind | Files |
|---|---|---|
| owner (`lagent`, owner devices) | any agent | the worktree guard still applies; `allowMainCheckout` overrides it |
| MCP token | agents with its `lucina.mcp-token` label | only if the agent's directory is inside its workspace's directory (`REWIND_FORBIDDEN` otherwise); no override |
| agent | its subagents only, not itself | as an MCP token |
| anyone else | nothing (`NOT_YOUR_AGENT`) | |

The worktree guard is the one that gates starting an agent: not in a main checkout under the projects root, not in a stray
worktree, the Codex rule (`WORKSPACE_GUARD`, 403). A conversation rewind changes no files, so it has no guard.

**Fork** starts a new agent from another's chat history.

- **`fork-context`** is Paseo's `buildAgentForkContextAttachment`, byte for byte (checked against Paseo's own code on 35
  cases, `crates/lucina-agents/tests/paseo_fork_context.rs`, and live in `paseo_diff` scenario i): `{type: "text", mimeType:
  "text/plain", contextKind: "chat_history", title: "Chat history", text}` where `text` is `<chat-history-summary>`, the header
  `Chat history from a previous Paseo agent.` (kept as is), `Source agent:` and `Source directory:` lines, then the projected
  timeline as `[User] …`, `[Assistant] …` and tool-call lines (reasoning, notifications and plugin items stay out). The whole
  text is at most 100,000 UTF-16 code units: the newest entries that fit, after `[Earlier chat history omitted]`; one entry
  bigger than all keeps its head and tail around `[Message truncated]`.
- **Boundary**, as Paseo: `boundaryCursor` `{epoch, seq}` (the projected entry that ends at `seq`; a cursor from another epoch
  is stale) or `boundaryMessageId` (the last assistant message with that id; a cursor wins when both are given); neither means
  the whole history. A boundary that isn't there, or that falls inside an entry that kept growing (a tool call that updated
  after it), is `409 FORK_BOUNDARY_UNAVAILABLE` with Paseo's message. Fork from a completed response.
- **`fork`** creates the agent. It takes the source's provider, model, mode, thinking option, feature values, directory and
  workspace unless the request names others (settings carry over only to the same provider; a caller that isn't the owner never
  inherits the mode: it gets a supervised one). It is labelled `lucina.forked-from=<source id>`, and an MCP token's fork carries
  its `lucina.mcp-token`. The first prompt it gets is the attachment text, a blank line, then the prompt. With a `prompt` that
  turn starts at once; without one the agent waits idle with the history kept on its record (`seedContext`), and the first prompt
  sent to it (also after a restart) carries it, once. The timeline shows the prompt as typed, except for a provider that echoes
  the message itself (the mock, Codex): it shows what it was sent. An external agent can't be forked into its own kind: pass a
  `provider`.
- **Who may fork:** the owner any agent; an MCP token the agents it started; an agent itself and its subagents (its fork is
  its subagent, under the depth limit: a subagent forks nothing, `DELEGATION_DEPTH`). The same rules apply to `fork-context`.
  Non-owners are supervised as for any agent they start (no provider or mode without permission prompts).

**Differences from Paseo** (the mock; `paseo_diff` scenario i): Paseo's mock keeps only the first user message on any
conversation rewind and fails an unknown target inside the provider; Lucina's mock rewinds to the target and an unknown
target is `404 REWIND_TARGET_NOT_FOUND` before anything runs. Paseo cancels the running turn before it finds out the rewind is
unsupported; Lucina checks first. Paseo reads the history back from the provider after a rewind; Lucina rewrites its own
stored timeline.

## Stream

`GET /v1/agents/stream` is SSE. Each event has a monotonic `id:` and `data:` set to one bus event:

- `{"type":"agent_state","agent":{…snapshot}}`
- `{"type":"agent_stream","agentId","event":{…StreamEvent},"seq","epoch","timestamp"}`
- `{"type":"agent_deleted","agentId"}`
- `{"type":"provider_subagent","event":{"kind":"upsert"|"timeline"|"remove",…}}` (P7, above)

- A16: `{"type":"timeline_replacement","agentId","epoch"}` (Paseo's `agent.timeline.replacement`): a conversation rewind replaced the
  timeline; drop what you hold for the agent and fetch `/timeline` again under the new epoch
- `{"type":"permission","permission":{…PermissionView}}`
- `{"type":"workspace","workspace":{…WorkspaceRecord}}`
- phase 7: `{"type":"agent_command","agentId","commandId","command":{…},"createdAt","expiresAt"}`, only to the owning device
- phase 5: `{"type":"terminal","terminal":{…}}`, `{"type":"terminal_removed","terminalId","workspaceId"}`,
  `{"type":"script_status","workspaceId","scripts":[…]}`, `{"type":"terminal_attention","terminalId","workspaceId","reason","title","body"}`
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
- secretary-server: at most 4 open streams per caller and 40 in all, terminal streams included (429 `too_many_streams`); a stream ends on
  shutdown, when Lucina is quiet for 90 s (the daemon sends a keep-alive comment every 15 s), or when it ends.
- secretary-server starts every stream with the comment `: connected`. Caddy's `encode` holds the response headers until the
  first write when the client accepts compression, as browsers do, so without it a quiet stream didn't open until the first
  keep-alive (found 2026-11-01: 15.1 s through Caddy, 0.05 s at the server). It also keeps a large first frame from turning on
  compression for the whole stream. Clients ignore comments.
- The web page (`web/src/state/agents.ts`) says nothing about a drop of up to 5 s. After that it reads the REST routes: if they
  fail it shows that the backend is down, with the server's message; if they work it says live updates are paused and refreshes
  every 15 s. Either way it keeps retrying the stream and clears the message itself.

## Pushes

agentd sends `POST /v1/notify` with category `agent` and `data: {kind, agentId | permissionId, reason, requestId?,
actions?}` (phase 3), plus `scheduleId` and `parentAgentId` when they apply (phase 6). Route `/agents/<agentId>`. The apps' notification actions (Allow/Deny) call
`POST /v1/agents/{id}/permissions/{requestId}` or `POST /v1/agents/permissions/{id}`.

### Permissions in the owner approvals (phase 8)

When a permission request becomes pending (an agent's or an external one), agentd's approvals bridge files it as an
owner approval (`POST /v1/approvals`, action `agent.permission`, `payloadHash` = sha256 of the canonical JSON
`{agentId|null, requestId, kind, name, input}`) with the same service token as `/v1/notify`. The phone, the web pop-up
and Secretary Code then show the usual Yes/No prompt, with Face ID or the PIN for sensitive ones (external requests,
tool input that mentions sudo, root or secrets). `PermissionView.approvalId` (absent until filed) is that approval's id,
also in the stream's `permission` events; the web links it to `/approvals/<approvalId>`. The bridge long-polls the
approval; an approved or denied one is answered through the normal gate as `device:<decidedBy>`, only after its grant is
verified (pinned key, `payloadHash`, `action`, single use). Nothing else can answer through it: MCP callers still
cannot. A permission answered elsewhere (web `/agents`, `lagent permit`, the apps' actions) or expired makes the bridge
call `POST /v1/approvals/{id}/withdraw`: the approval becomes `expired` (no new status value), leaves every pending list,
and deciding it afterwards gives 410 `approval_expired`. Only the requester that filed it can withdraw (anyone else gets
404), and doing it twice is fine. The bridge is off (logged once) when the URL, token or key is missing. It reads `LUCINA_AGENTD_NOTIFY_URL` and
`LUCINA_AGENTD_NOTIFY_TOKEN` (the same service token as the pushes), `LUCINA_AGENTD_APPROVAL_PUBKEY_FILE` (default
`~/.config/lucina-rs/approval-grant.pub`), `LUCINA_AGENTD_APPROVAL_USED_FILE` (single-use ids, default
`~/.local/state/lucina-agentd/approvals-used.jsonl`) and `LUCINA_AGENTD_APPROVALS=off`.

## MCP tools (phase 9)

The tools are thin wrappers over these routes, with the caller's `mcp:<tokenId>` origin:

- agents: `agents_list`, `agent_run`, `agent_send`, `agent_logs`, `agent_wait`, `agent_stop`, `agent_archive`,
  `agent_inspect`, `agents_search` (phase 6: agentd's `/search`, names-only against an older daemon);
- workspaces: `workspaces_list`, `workspace_create`, `workspace_diff`, `files_list`, `file_read`;
- terminals and scripts (phase 5): `terminal_list`, `terminal_read`, `terminal_send` (text or one key token),
  `scripts_list`, `script_start`, `script_stop`. They can't create or kill a terminal or open a service;
- schedules: `schedules_list`, `schedule_add` (a new agent per run, in a safe mode), `schedule_remove` (own only)
  (phase 6);
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

## Deploying phase 5 (for the coordinator)

- **nezopt:** lucina-rs and lucina-agentd from the `agents-p5` line. Its forwarder passes the service paths and
  headers.
  - Optional in the agentd unit: `LUCINA_AGENTD_PUBLIC_URL=https://getsecretary.tech` (`proxyUrl` and `PASEO_URL`
    become the public service paths) and `LUCINA_AGENTD_SCRIPT_CPU_QUOTA` (default `200%`).
  - systemd-run must reach the user manager, which it does from a user unit.
- **p1:** secretary-server and secretary-mcp from `agents-p5-server`.
- **Caddy:** nothing new; `/v1/*` already goes to secretary-server.
