# Lucina Agents: Paseo rebuilt in Rust (design, 2026-10-27)

Paseo 0.13.2 (Node, `paseo.service` on 127.0.0.1:6767) drives the owner's coding agents on nezopt. This document
describes its replacement inside Lucina: a Rust daemon, an API through secretary-server, UIs in Secretary Code, the
web app, iOS and Android, and tools in the Lucina MCP. Paseo keeps running until the parity matrix
(`docs/design/agents-parity.md`) is green and the owner has used the Lucina version for real work.

## Scope (confirmed by the owner, 2026-10-27)

The owner chose to port everything and drop nothing: where Paseo has a feature that Lucina already covers, Lucina's
version replaces it and must still cover what Paseo's did.

- **Ported:** agents (run, send, attach, logs, stop, wait, archive, delete, import, inspect), the timeline, search,
  subagents and notify-on-finish, heartbeats. Providers: Claude, Codex, OpenCode, Cursor, Pi, ZCode, and the ACP agents
  (Copilot, Kimi, Kiro, Trae, generic) through one ACP adapter. Permission requests, answered from the phone and the
  desktop. Workspaces and worktrees with diffs. The file explorer. Terminals. Workspace scripts with ports and a
  service proxy. Schedules. Connected hosts (0.13). Usage and quotas. Plugins and a plugin catalog (our Hub).
  The MCP tools.
- **Also ported (found in the 0.13.2 inventory):**
  - Orchestration: worker/verifier loops, chat rooms between agents, and the shipped skills (advisor, committee,
    handoff, help, plugin).
  - Agent history: rewind (conversation, files, or both) and fork with the chat history attached.
  - Repos and browser: the 22 browser tools, PR checkout on GitHub, GitLab and Gitea, and clone.
  - Workspace config: `paseo.json` (setup, teardown, terminals, service ports, scripts) is read unchanged, so
    existing repos keep working.
  - Every feature has a row in `docs/design/agents-parity.md`.
- **Lucina's version instead of Paseo's:** Secretary pairing and owner auth replace the daemon keypair and password.
  Lucina Link replaces the relay. The Secretary web app replaces Paseo's web UI. Push goes through `/v1/notify`.
  Dictation goes through Whisper (lucina-memo).
- **New:** a QR login like Discord's for every Secretary client (below). Hermes (Flareon) can use the agents through
  the Lucina MCP.
- **References:**
  - The daemon-side inventory is the session notes' `paseo-0.13.2-features.md`.
  - The app side is nezopt `~/Projects/ReverseEngineering/paseo`: the decompiled Android 0.13.2 app (the
    `hermes-bundle.js` keeps its function names), `parity/constants.json` (129 session message types, scopes, timing),
    and a Rust port of the relay/E2EE core.
  - The relay/E2EE port is not used here (Lucina Link replaces the relay). The decompiled UI is the reference for
    phase 8.
- **Baseline:** Paseo was upgraded from 0.11.1 to 0.13.2 (the latest release, 2026-10-26) before porting, so parity
  is measured against 0.13.2. The state backup from before the upgrade is in nezopt
  `~/scratch/paseo-backup-20261027-1328/`.

## Pieces

```
 Secretary Code (desktop)   web /agents plugin   iOS Agents   Android Agents   Lucina MCP (Hermes, Claude, Codex…)
            \                     |                 |              /                   |
             +------- secretary-server /v1/agents/*  (owner + device auth, SSE/WebSocket, push) ----+
                                          |  unix socket, JSON
                                lucina-agentd (user unit on nezopt)
             providers | timeline store | permissions | workspaces (wt) | PTYs | scripts | schedules | gate
                 |                                       |                            |
   claude / codex app-server / opencode / cursor-agent   git + wt + .worktrees      lane chains.toml, diffgate,
   / ACP agents / pi / zcode / mock providers                                       verify-gate
```

### Crates (SecretaryIOS, branch line `lucina-rs`)

- `crates/lucina-agents`: types and pure kernels, no I/O. Agent, AgentState, TimelineItem, PermissionRequest,
  Workspace, Schedule, the wire types of the API, and the kernels that get Verus proofs (below). Used by the
  daemon, secretary-server, secretary-mcp and secretary-ffi (for iOS).
- `crates/lucina-agentd`: the daemon and its CLI `lagent`. Tokio, axum over a unix socket
  (`$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`, mode 0600) and optionally 127.0.0.1:6768 for local tools. State in
  `~/.local/share/lucina-agentd/`: SQLite (rusqlite 0.32 bundled, as in the rest of the workspace) for agents,
  workspaces, permissions, schedules and an FTS5 index for search; one append-only JSONL timeline per agent.
- `lagent` mirrors Paseo's CLI verbs and flags where they make sense (`lagent run --provider codex --new-workspace
  worktree …`, `lagent ls --json`, `lagent permit allow <id>`), so the Codex rules and the differential tests can
  switch with few changes.

### Providers

One `Provider` trait: `start(spec) -> Session`, `send`, `steer` (a message into a running turn, as Paseo 0.12),
`interrupt`, `resume(session_id)`, `import(session_id)`, and a stream of normalized events (text, reasoning, tool
call, tool result, permission request, usage, turn end, error). Adapters:

| Provider | Launch |
|---|---|
| claude | Claude Code CLI, `--input-format stream-json --output-format stream-json`; permissions through `--permission-prompt-tool` pointed at an MCP tool that agentd serves |
| codex | `codex app-server` (JSON-RPC over stdio) |
| opencode | `opencode serve` (HTTP + SSE) |
| cursor | `cursor-agent --output-format stream-json` |
| acp | any Agent Client Protocol agent over stdio: Copilot, Kimi, Kiro, Trae, generic |
| pi, zcode | their headless CLIs (`zcode.cjs --json`, Pi's RPC mode) |
| mock, mock-slow | test providers matching Paseo's: the same models (`e2e-fast-stream`, `ten-second-stream`, …) and prompt triggers ("emit synthetic turn failure", "emit synthetic plan approval", …), for the differential tests |

Model and effort defaults come from lane's `~/.config/lane/chains.toml`, which stays the single source of truth;
agentd never changes them and never turns on Fast. A run may name a chain (`--chain deep`): agentd then falls through
lanes in chain order only when lane's `[[detectors]]` classify the failure as quota or unavailable, and stops on a
real failure or on a quota stop that left changes (the same rules as `lane`). A contract test parses the real
chains.toml so a schema change in lane breaks the build, not a run.

### Agent lifecycle

States: `initializing → idle ⇄ running → (idle | error)`, plus `closed` (provider process gone, resumable) and
`archived` (soft delete). A permission request moves a running agent to `waiting_permission` until answered or
expired. Every transition is a timeline item, so the apps, `lagent logs` and the parity harness all read one source.
Paseo keeps timelines only in memory and rebuilds them from each provider's history after a restart, which gives
them a new epoch. agentd writes them to disk, so cursors survive a restart. The wire format (`{epoch, seq}` cursors,
the item types) stays Paseo's.
On restart agentd marks agents that were running as `interrupted` instead of `running` (Paseo 0.13 fixed the same
bug) and delivers pending notify-on-finish messages to parents.

### Finished-run gate (owner: "smartest and consistent")

Every run ends with the same three checks, and the run card shows all three. This follows current practice for
agent output (tests are a proxy; run the proof yourself; detect test tampering; add a trajectory judge):

1. **diffgate**, always: stubs, TODO markers, skipped or removed tests and assertions, deleted checks.
2. **Proof command**, run by agentd itself, never taken from the agent's report:
   - The workspace declares it in `.lucina/agents.toml` (`verify = "cargo test -p foo"`), or the run passes it.
   - With `heavy = true` it goes through `verify-gate`, one at a time.
   - With no proof command the run is `unverified`, never `verified`.
3. **Judge**: a cheap model reads the task, the diff and a summary of the trajectory, and flags runs that look
   like reward hacking (hard-coded answers, edited tests, claims the diff does not support). It is advisory: it can
   mark a run `suspicious`, never `verified`.

Result: `verified` (1 and 2 pass, 3 clean), `refused` (1 or 2 failed; refusals go back to the same session as a
follow-up when the run asked for auto-fix), `suspicious`, or `unverified`.

### Permissions

A provider's permission request becomes a `PermissionRequest {id, agent, tool, summary, args, expires_at}`.
secretary-server pushes it (`kind = agent.permission`, with Allow and Deny actions) to the owner's devices.
It can be answered from the web app, Secretary Code, iOS, Android, or `lagent permit` on nezopt.
It cannot be answered through MCP: an outside agent never approves for the owner (the same rule as Lucina's
confirmation gate, C2). An answer counts once, and only before the request expires. An unanswered request is denied
when it expires.

### Workspaces and worktrees

- A workspace is a repo checkout. A worktree workspace is created with the owner's `wt add <name> <base>`, so it
  lives in `~/Projects/.worktrees/<Repo>/<name>`, never as a sibling in `~/Projects` and never in `~/.paseo`.
- Branch names come from the task: `agent/<slug>` by default. Codex runs in SecretaryIOS use `codex/<topic>` from
  `lucina-rs` and never the main checkout. A guard kernel refuses any write workspace that is a main checkout.
- Every worktree has an owner agent and a lifecycle. When its branch is merged, agentd removes it
  (`git worktree remove`, then `git branch -d`), as the owner's rule says. An archived agent's clean, fully merged
  worktree goes too, and `wt prune` still runs daily for the rest. This fixes Paseo's 12 GB of leftover worktrees.
- Agents share one `CARGO_TARGET_DIR` per repo under `~/.cache/cargo-target/`.
- Diffs: "All changes" since the branch base (Paseo 0.13's default), uncommitted, and per commit, computed with
  `--no-color --no-ext-diff` so the owner's git settings can't break them. Diff comments go to the agent as a
  follow-up ("Send feedback").
- The file explorer can list, read and download files. Uploads go through Lucina Drive, which already handles
  media and sizes.

### Terminals, scripts and services

- PTYs (`portable-pty`) live in agentd and survive client disconnects. They stream over WebSocket to xterm.js on the
  web and desktop, SwiftTerm on iOS, and a terminal view on Android. Pasting images and dropping files into a
  terminal works, as in Paseo 0.13.
- Scripts are declared in `.lucina/agents.toml` (`[scripts.dev] cmd = "…", port = "auto", health = "/health"`).
  Ports are allocated from 4100-4999 without collisions. They run in capped scopes (`systemd-run --user --scope -p
  CPUQuota=…`), as the owner requires for background CPU work. Their services are proxied by path under
  `/v1/agents/workspaces/{id}/services/{name}/`.
- When a workspace is archived its scripts and terminals stop. Forgotten processes are what the owner minds.

### Schedules, heartbeats, subagents, hosts

- Schedules are cron-like records in agentd's database (`lagent schedule add "0 7 * * 1-5" --provider … prompt`),
  listed and edited from the apps. They are not systemd timers.
- Heartbeats are an agent's periodic self-check-ins, as in Paseo.
- Subagents: an agent may start agents through the agentd MCP tools. Parents get one message with the child's
  report and finish notice (0.13).
- Connected hosts: one agentd talks to another over SSH or Lucina Link. The Mac can run an agentd for Xcode work and
  show up as a host.

### Plugins and catalog

`lucina-plugin.json` manifests with Paseo's extension points: panels (workspace, agent, file), timeline
transformers, turn hooks, composer pills and translations. The web and Secretary Code load plugin UI as ES modules
in a sandboxed iframe, and server-side hooks run as commands in a capped scope. The catalog (our Hub) is a list of
plugin sources on the owner's own git server. This is the last phase.

## API through secretary-server

secretary-server mounts `/v1/agents/*` and forwards to agentd's socket after its usual owner/device auth, rate
limits and audit. Push uses the existing notify module. Remote access comes for free through Lucina Link.

- REST: `GET/POST /v1/agents`, `GET/PATCH/DELETE /v1/agents/{id}`, `POST /v1/agents/{id}/{send|steer|stop|archive}`,
  `GET /v1/agents/{id}/timeline?after=`, `GET /v1/agents/search?q=`, `GET/POST /v1/agents/permissions[/{id}]`,
  `/v1/agents/workspaces[/{id}/{diff|files|scripts|services}]`, `/v1/agents/schedules`, `/v1/agents/hosts`,
  `/v1/agents/usage`, `/v1/agents/providers`.
- Streams: `GET /v1/agents/stream` (SSE: agent states, timeline items, permissions) and
  `GET /v1/agents/terminals/{id}` (WebSocket).
- The wire types live in `lucina-agents`, so the server, the MCP and the apps can't drift.

## QR login (Discord-style), for every Secretary client

Paseo's QR only pairs one daemon. Lucina's QR signs a new client in to the owner's account, as Discord does:

1. A client that isn't signed in (Secretary Code, the web app, a new phone, a remote agentd host) generates an
   ephemeral X25519 key pair. It calls `POST /v1/auth/qr` with its public key and device name, and gets back
   `{challenge_id, expires_in: 120}`. It shows a QR with `secretary://login/<challenge_id>#<pubkey fingerprint>`
   and waits on `GET /v1/auth/qr/{id}` (SSE).
2. The owner scans it with a signed-in Secretary app. The app shows the new device's name, platform, IP and
   approximate place, plus a fingerprint check, and asks for Face ID or the device passcode.
3. On approval the server issues a new device token. It encrypts the token to the client's public key, so neither
   the QR nor the waiting stream can be replayed by someone who only saw them. The new device appears in the
   device list and can be revoked like any other.
4. A challenge can be used once and dies after 120 s. It can be approved only by an authenticated device of the owner,
   never by an MCP token. Password login stays as the fallback.

## Lucina MCP tools (secretary-mcp)

A new scope `agents`, granted per token, separate from `write`. Hermes gets a token with `read` + `agents`.

- Agents: `agents_list`, `agent_run`, `agent_send`, `agent_logs`, `agent_wait`, `agent_stop`, `agent_archive`,
  `agent_inspect`, `agents_search`.
- Workspaces: `workspaces_list`, `workspace_create` (worktree via `wt`), `workspace_diff`, `files_list`, `file_read`.
- Terminals and scripts: `terminal_list`, `terminal_read`, `terminal_send` (keys into a terminal the agent owns),
  `scripts_list`, `script_start`, `script_stop`.
- Other: `schedules_list`, `schedule_add`, `schedule_remove`, `permissions_list` (read only), `agents_usage`.

MCP-origin runs can't answer permissions and can't change schedules they didn't create. Delegation depth is
limited, as in lucina-delegate.

## One protocol and one UI for agents and coding sessions

Secretary Code is becoming a full Lucina client: it embeds the SecretaryIOS web app, and its coding sessions show up
on the phone. It doesn't get a second protocol for that.

- **Protocol:** `/v1/agents/*` with the `lucina-agents` wire types is the only API for agents and coding sessions,
  on every client.
- **Secretary Code sessions** are agents with provider `secretary-code`, `external: true` and `host: "mac"`:
  - Secretary Code registers each session with `POST /v1/agents`.
  - It posts the session's timeline items to `POST /v1/agents/{id}/events`.
  - It receives `agent.command` events (send, steer, stop, permission answers) for the sessions it owns on the
    stream.
- **External agents, auth:**
  - A `secretary-code` agent is registered with the desktop's existing Secretary device token. The agent records
    that device as its `ownerDeviceId`.
  - Only that device can post its events and receive its `agent.command`s.
  - Any of the owner's signed-in devices (phone, web) can send, steer, stop and answer permissions. Those become
    commands queued for the owning device.
  - Other devices, and MCP tokens, can't post events for it.
- **Resumable delivery:**
  - Every stream event has a monotonic `id:`, and a client reconnecting with `Last-Event-ID` gets what it missed.
    Events are kept for 24 h or 10,000 entries; a client that fell further behind gets `stream.reset` and refetches.
  - `agent.command`s are also stored until the owning device acknowledges them
    (`POST /v1/agents/{id}/commands/{commandId}/ack`), so a Mac that sleeps misses none.
  - A command not acknowledged in 24 h expires. An expired permission answer counts as denied.
- **UI:** the web `/agents` plugin is the agents UI everywhere. Secretary Code's `agents` slot loads it from the
  embedded web app. iOS and Android add native screens only for notification actions and the QR scanner.
- **Ownership:** this design builds agentd, the API, the MCP tools and the web plugin. The Secretary Code session
  builds the desktop embedding, the `secretary-code` provider client, and the Mac build.

## Security requirements (from the Paseo reverse-engineering)

Source: nezopt `~/Projects/ReverseEngineering/paseo/HANDOFF-lucina-integration.md` §5-§6, from a full decompilation of
the Paseo 0.13.2 Android app. These are design requirements. Each one has a row in the parity matrix (SEC rows).

1. **Trust boundary first.** Whatever can drive agents can run commands on nezopt and the desktop. Paseo's phone-side
   scopes `hub.execute`, `daemon.manage`, `access.manage` and `tunnel.manage` amount to remote code execution by design.
   - The agent-facing surface gets its own separate, revocable, tool-scoped token: secretary-mcp scope `agents`, plus
     `read`. It is never the owner's token.
   - Reads are allowed. Mutating Lucina tools (`reminder_create`, `task_add`, `shopping_add`, `calendar_create` and
     the like) need the owner's confirmation when an agent calls them, because agents are remotely driven. The
     confirmation goes through the same permission flow as agent permissions (Allow/Deny push).
   - A stolen agent token is not the owner's identity: it can't answer permissions, change scopes or rotate tokens.
2. **Lucina as an MCP server attached to agents.** This is Paseo's `mcpServers` pattern.
   - agentd attaches the Lucina agent MCP to each agent through the provider's MCP config, with that agent's scoped
     token and a deliberate `toolPolicy`: reads preapproved, mutations gated. It sets `supportsMcpServers` per
     provider.
   - The token is handed to the agent process at launch. It is never written to a file the agent can read.
3. **Token hygiene.** Today the owner's Lucina MCP bearer token sits in plaintext in `~/.claude.json` (Mac and
   nezopt), and the lucina-link tunnel credentials in `~/.config/lucina-link/secrets.env`. Any agent with file access
   can read both.
   - Agent runs can't read those paths: agentd launches providers with deny-read rules for them (bubblewrap, as in
     lucina-delegate's sandbox, or a separate uid; see below).
   - Secrets for agents come from the Infisical broker (`infisical-brokerd`, built by another session), not from
     dotfiles.
     - They live in their own Infisical project `agents` (env `prod`, root `/`). Its broker identity `broker-agents`
       can read only that project, not Lucina/prod. Custom roles are Enterprise-only, so folder scoping isn't
       possible.
     - agentd is a `services`-class broker caller and uses the `exec` op:
       `infisical-broker exec --env VAR=secret://agents/prod#KEY -- <provider>`, or the same op over the socket.
     - Its caller entry is bound to the unit (`bind_unit = "lucina-agentd.service"`), so only processes in that
       unit's cgroup can use its token.
     - Keys: `LUCINA_MCP_AGENTS_TOKEN` (the `read`+`agents` token for the Lucina agent MCP). Provider API keys go
       under their own names when a provider needs one.
   - Once the scoped token exists, rotate the owner token. The owner decides when.
4. **No weak spots copied from Paseo.**
   - No plaintext or legacy hello: Paseo has `acceptLegacyHello`, `shouldIgnorePostHelloPlaintext` and
     `usesCleartextTraffic`.
   - No cleartext transport: pairing and device auth go only through secretary-server's device tokens, over TLS (Lucina
     Link), and agentd itself listens only on a unix socket (or loopback).
   - The QR login (above) is the only pairing path.
5. **Phone apps ask only for what a feature needs.** No screen overlay (`SYSTEM_ALERT_WINDOW`), no boot auto-start,
   no extra launcher-badge permissions. Camera only for the QR scanner, microphone only for dictation, each requested
   when the feature is first used.
6. **Same-uid limit.** Until agent processes run under their own uid, anything running as `nez` can read the owner's
   files. The hardening path is agentd launching providers under a dedicated agents uid that can't read `~nez`.
   - That needs root once, so the owner approves it first. It is a separate step from the broker's `secretsd`
     (uid 995, already approved).
   - P0 found by the broker session (2026-10-27): `~/secrets/nezopt-sudo.honcho` and its AES key
     `~/secrets/honcho-aes256.key` are both readable by `nez`, so any agent running as `nez` can sudo.
   - The agents uid fixes this for agents agentd launches. Interactive Claude and Codex sessions running as `nez`
     stay exposed until the owner moves that blob.

## UIs

- **Web** (Solid, `web/`): an Agents plugin at `/agents` in `PLUGIN_PATHS` (`web/src/views/Shell.tsx`) and the grid in
  `web/src/views/Plugins.tsx`. Pages: agents list with status rings, agent chat (timeline, composer with Whisper
  dictation and images, steer and stop), permissions inbox, workspace (All changes diff with comments, files,
  terminals, scripts), schedules, usage.
- **Secretary Code** (desktop, gvitolocs/SecretaryCode): the panel reuses the existing docked `agents` slot instead of
  adding a third panel.
  - Where it is: `packages/desktop/src/main/secretary/panel.ts` places the slot. `lane-window.ts`
    `codingLanesView()` serves `src/renderer/secretary-lanes/` over `secretary-coding://`.
  - The local lane launcher (`lanes.ts`, `lane-process.ts`, and `lane-ipc.ts` on channel `secretary:lanes`) moves
    to `/v1/agents/*`. The preload bridge `window.electron.codingLanes` (`CodingLanes` in `preload/types.ts`) keeps
    its shape, or gets a version bump.
  - Agent failures can also be reported through `sc-diagnostics` (`kind: "error"`, `context.lane`).
  - The Secretary Code session owns the integration, build and install on the Mac. At phase 8 it gets a spec with
    the endpoints, auth, and the event and stream shapes. engine-rs has no multi-agent orchestration to reuse.
- **iOS** (SwiftUI, `ios/Secretary/Features/Agents`) and **Android** (`android/secretary`): agents list, chat,
  permission actions from the notification, diff viewer, terminal, QR scanner for login approval.

## Verification

Pure kernels in `lucina-agents` with Verus proofs in `verification/verus/` and differential tests against the
production functions, following the pattern of `confirmation_gate.rs`. They run through `scripts/verify.sh` under
`~/.local/bin/verify-gate`.

- `agent_lifecycle`: only allowed transitions; archived is terminal except unarchive; restart never leaves `running`.
- `permission_gate`: one answer per request, only before expiry, only from an owner device, never from MCP;
  expiry denies.
- `worktree_guard`: a write workspace is always under `~/Projects/.worktrees/<Repo>/`, never a main checkout.
  Codex branches are `codex/*` from `lucina-rs`.
- `qr_login`: a challenge is single use, approved by an owner device before expiry, and its token is bound to the
  challenge key.
- `port_alloc`: allocated ports are unique and in range.
- `run_gate`: `verified` implies diffgate pass and proof pass and no judge flag.

**Parity**: `docs/design/agents-parity.md` keeps the feature matrix. For each feature, a differential test in
`crates/lucina-agentd/tests/paseo_diff/` runs the same scenario through `paseo` (mock providers) and through `lagent`.
It normalizes ids and times, then compares the lifecycle states, the timeline item kinds and order, the worktree and
branch result, and the exit status. The tests run only when `PASEO_DIFF=1` and Paseo is reachable, so they stop
once Paseo is retired.

## Phases

Each phase gets its own `wt` worktree and branch from `lucina-rs` (or SecretaryCode `main`), its own tests, and a
parity check before the next phase starts. Implementation goes to the Claude CLI lane (`lane run --category deep`).
Every result is reviewed with `git diff`, diffgate, build and tests, and screenshots for UI.

| # | Phase | Done when |
|---|---|---|
| 0 | Design, parity matrix, Paseo upgraded to 0.13.2 | this document |
| 1 | `lucina-agents` types and lifecycle kernel, agentd skeleton, storage, mock providers, `lagent` run/ls/logs/wait/stop/send/archive/delete/inspect, differential harness | mock scenarios match Paseo |
| 2 | Providers claude, codex, opencode, cursor; then acp, pi, zcode; chains and quota fall-through; import; usage | one real run per provider |
| 3 | Permissions, the finished-run gate, notifications | permission answered from the phone |
| 4 | Workspaces, worktrees via `wt`, diffs, files, branch names, cleanup on merge, setup scripts | matrix rows green |
| 5 | Terminals, scripts, ports, service proxy | |
| 6 | Schedules, heartbeats, subagents, notify-on-finish, search | |
| 7 | secretary-server `/v1/agents`, streams, QR login | |
| 8 | UIs: web plugin, Secretary Code panel, iOS, Android | screenshots on each |
| 9 | MCP tools, Hermes token | Hermes runs an agent |
| 10 | Connected hosts, plugins and catalog | |
| 11 | Soak: the owner uses it for real work; then Paseo is retired (unit off, `expected-active.txt`, `~/.paseo` checked for unpushed work and archived to the MyBook, `@getpaseo/cli` uninstalled, Codex rules changed) | owner sign-off |
