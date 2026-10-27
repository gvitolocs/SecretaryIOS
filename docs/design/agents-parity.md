# Lucina Agents: parity matrix against Paseo 0.13.2

Source of the Paseo column: the read-only inventory of the installed 0.13.2 (2026-10-27, 129 CLI commands, 236 inbound
and 213 outbound protocol messages, ~30 MCP tools). The inventory itself is in the session notes
(`paseo-0.13.2-features.md`); this file keeps what matters for parity.

**Status values:** `todo`, `wip`, `built` (Lucina has it, tests pass), `green` (the differential test or the manual check
passes against Paseo), `n/a` (Paseo-only plumbing with no user-visible effect; the reason is given).

**Run them:** `PASEO_DIFF=1 cargo test -p lucina-agentd --test paseo_diff -- --nocapture` (about 40 s).

**Differential tests** live in `crates/lucina-agentd/tests/paseo_diff/` and run only with `PASEO_DIFF=1`. They never
touch the production Paseo daemon. They start a second one in development mode, on its own home and port:
`PASEO_NODE_ENV=development paseo daemon run --home ~/scratch/paseo-diff/home` with `listen` `127.0.0.1:6777`.
Only development mode registers the `mock` provider. The same scenario runs through `paseo --host 127.0.0.1:6777 … --json`
and through `lagent … --json`. Ids, times and epochs are normalized, then the tests compare:
- the lifecycle state sequence;
- the timeline item types and their order;
- the tool-call statuses;
- the worktree path pattern and the branch;
- the exit status.

Paseo's mock models and prompt triggers (for example `e2e-fast-stream`, "emit synthetic turn failure", "emit synthetic
plan approval", "emit synthetic question") are reproduced by Lucina's `mock` provider, so every scenario stays
deterministic on both sides.

| # | Area | Feature (Paseo 0.13.2) | Lucina | Phase | Check | Status |
|---|---|---|---|---|---|---|
| A1 | Agents | `run` (prompt, provider/model, mode, thinking, title, labels, env, images, `--no-wait`, wait timeout) | `lagent run`, `POST /v1/agents`, MCP `agent_run` | 1 | diff: mock e2e-fast-stream | green (diff a, d) |
| A2 | Agents | `ls` (archived, background, cwd, since, statuses, limit), state buckets needs_input/failed/running/attention/done | `lagent ls`, `GET /v1/agents` | 1 | diff | green (diff a, e) |
| A3 | Agents | `logs` / activity curator ("Showing N of M activities", `[Thought]`, tool summaries 400/200 chars) | `lagent logs` | 1 | diff: same text after normalization | green (diff a-d; curator fixtures 72/72) |
| A4 | Agents | `attach` (live stream) | `lagent attach`, `/v1/agents/stream` | 1 | diff: item order | built (lagent attach, no diff yet) |
| A5 | Agents | `send` with steer / interrupt / out-of-band dispositions | `lagent send --steer|--interrupt` | 1 | diff | green for interrupt (diff d); steer built |
| A6 | Agents | `stop` (interrupt, auto-deny pending permissions "Interrupted") | `lagent stop` | 1 | diff | green (diff c) |
| A7 | Agents | `wait` (idle, timeout) | `lagent wait` | 1 | diff | green (diff d) |
| A8 | Agents | `archive` / unarchive (cascade to children), `delete` | `lagent archive/unarchive/delete` | 1 | diff | green for archive/delete (diff e); unarchive built |
| A9 | Agents | `inspect` snapshot (capabilities, pending permissions, lastUsage, activeTurn) | `lagent inspect` | 1 | diff: field set | green (diff a, c) |
| A10 | Agents | lifecycle initializing→idle⇄running→error/closed; restart repairs stale "running" | kernel `agent_lifecycle` (Verus) | 1 | Verus + diff: restart scenario | green (Verus L1-L7; restart test in daemon.rs) |
| A11 | Agents | timeline item types (user_message, assistant_message, reasoning, tool_call + 11 detail variants, todo, error, notification, compaction, plugin), seq/epoch cursors | `lucina-agents` types; durable JSONL (Paseo keeps it in memory only) | 1 | diff: item kinds | built (wire types round-trip; projection port) |
| A12 | Agents | titles (60 chars, from first prompt line), generated title + branch from a cheap model | same, cheap model from chains.toml | 4 | diff: mock answers the title prompt | todo |
| A13 | Agents | labels, `paseo.*` reserved labels, detach | `lucina.*` labels, `lagent detach` | 6 | diff | todo |
| A14 | Agents | `import` provider sessions (list importable, hide imported) | `lagent import` | 2 | manual: one Claude + one Codex session | todo |
| A15 | Agents | timeline search, prompt index | FTS5 search, `GET /v1/agents/search` | 6 | test | todo |
| A16 | Agents | rewind (conversation/files/both), fork with chat-history context (100k chars) | `lagent rewind`, `lagent fork` | 6 | diff: mock supports all rewind modes | todo |
| A17 | Agents | mode / model / thinking / feature changes, reload | `lagent agent set-*`, `reload` | 2 | diff | todo |
| A18 | Agents | idempotent creation (`creations/*.json` + claim) | idempotency key on `POST /v1/agents` | 1 | test: double submit | built (daemon.rs idempotent_create) |
| P1 | Providers | claude (Agent SDK) | Claude Code CLI stream-json | 2 | manual run + permission | todo |
| P2 | Providers | codex (`codex app-server`) | same protocol | 2 | manual run + permission | todo |
| P3 | Providers | opencode (`opencode serve`, HTTP+SSE) | same | 2 | manual run | todo |
| P4 | Providers | copilot (ACP), custom ACP providers (cursor, kimi, kiro, trae) | generic ACP adapter | 2 | manual: cursor via ACP or stream-json | todo |
| P5 | Providers | pi, omp (`--mode rpc`) | pi adapter (omp if installed) | 2 | manual | todo |
| P6 | Providers | plugin providers antigravity, muse, hoplite | provider plugins (phase 10) | 10 | manual | todo |
| P7 | Providers | provider subagents (Claude Task, OpenCode children) with their own timelines | same | 6 | manual: a Claude Task run | todo |
| P8 | Providers | `provider ls`, models, modes, profiles, inspect | `lagent provider …`, from chains.toml + adapters | 2 | diff: field set | todo |
| P9 | Providers | (Lucina only) chain fall-through on quota | lane detectors | 2 | test with a fake quota lane | todo |
| P10 | Providers | mock, mock-slow | `mock`, `mock-slow` | 1 | diff | wip: mock triggers for failure, plan, questions, settled/streaming replies, pre-echo, delays; mock-slow built |
| R1 | Permissions | request kinds tool/plan/question/mode/other, actions, updatedInput | same types | 3 | diff: "emit synthetic plan approval", "emit synthetic question" | todo |
| R2 | Permissions | `permit ls/allow/deny` (non-zero exit after 15 s without confirmation) | `lagent permit` | 3 | diff | todo |
| R3 | Permissions | answered from the phone and desktop | push actions + web/iOS/Android | 3/8 | manual: answer from the iPhone | todo |
| R4 | Permissions | tool policy preapproval (`{kind:"mcp",server,tool}`) | same | 3 | test | todo |
| R5 | Permissions | (Lucina only) MCP can't answer; expiry denies | kernel `permission_gate` (Verus) | 3 | Verus | todo |
| N1 | Attention | finished/error/permission attention, 3 min presence, push unless the client is focused | `/v1/notifications/presence` + `/v1/notify` | 3 | manual | todo |
| N2 | Attention | notification texts ("Agent finished", "Agent needs permission", 220-char preview) | same texts in it/en | 3 | test | todo |
| G1 | Gate | (Lucina only) diffgate + proof command + judge on every finished run | gate kernel `run_gate` (Verus) | 3 | test + Verus | todo |
| W1 | Workspaces | projects and workspaces registry, background workspaces | SQLite registry | 4 | diff: list after create | todo |
| W2 | Workspaces | worktree create: branch-off / checkout-branch / checkout-pr, slug, base | `wt add` → `.worktrees/<Repo>/<name>` | 4 | diff: branch result (path differs on purpose) | todo |
| W3 | Workspaces | `paseo.json` setup / teardown / terminals / servicePorts / scripts, setup env vars | reads `paseo.json` unchanged, plus `.lucina/agents.toml` (adds `verify`, `heavy`) | 4 | diff: setup log + exit codes | todo |
| W4 | Workspaces | archive workspace (archives agents, removes directory), auto-archive on PR merge | same, plus `git branch -d`; kernel `worktree_guard` | 4 | test | todo |
| W5 | Workspaces | untrusted source (fork PR): scripts blocked | same | 4 | test | todo |
| W6 | Workspaces | clone a GitHub repo and register it | `lagent clone` | 4 | manual | todo |
| W7 | Workspaces | labels, pins, unread, rename | same | 4 | diff | todo |
| D1 | Git | diff modes uncommitted / base / all (default), immune to the owner's git diff config | same with `--no-color --no-ext-diff` | 4 | diff: same files and hunks | todo |
| D2 | Git | checkout ops, commit, push, PR create/status on GitHub/GitLab/Gitea (forge) | gh + git; push only on explicit request | 4 | manual | todo |
| D3 | Git | diff comments → "Send feedback" to the agent | same | 8 | manual | todo |
| F1 | Files | explorer list/read/edit, one-shot download tokens | `/v1/agents/workspaces/{id}/files` | 4 | diff: listing | todo |
| F2 | Files | uploads (`uploads/<id>/`) | Lucina Drive | 4 | manual | todo |
| T1 | Terminals | PTY create/list/kill, capture (scrollback, strip ANSI), send keys (tmux tokens) | portable-pty, same tokens | 5 | diff: capture after keys | todo |
| T2 | Terminals | terminal agent hooks (claude/codex/opencode CLIs inside terminals report activity) | `lagent hooks` | 5 | manual | todo |
| T3 | Terminals | paste images / drop files into terminals | same | 5/8 | manual | todo |
| S1 | Scripts | scripts with service ports (portScript, range, any), health, `PASEO_SERVICE_*` env | same env names, plus `LUCINA_*` aliases | 5 | diff: env + port in range | todo |
| S2 | Scripts | service proxy `<script>--<branch>--<project>.localhost`, HTTP + WebSocket | path proxy through secretary-server, plus `.localhost` names on nezopt | 5 | test | todo |
| C1 | Schedules | schedules every/cron+timezone, maxRuns, expires, pause/resume/run-once, run history | agentd scheduler | 6 | diff: next-run times | todo |
| C2 | Schedules | heartbeats (schedule targeting the calling agent) | same | 6 | diff | todo |
| C3 | Schedules | new-agent schedules with worktree isolation, archive on finish | same | 6 | test | todo |
| O1 | Orchestration | child agents via MCP, notifyOnFinish envelopes, 2 s quiet delivery | same envelopes (`<lucina-system …>`) | 6 | diff: envelope text | todo |
| O2 | Orchestration | loops: worker/verifier iterations (maxIterations, maxTimeMs, verifyChecks) | `lagent loop` | 6 | diff | todo |
| O3 | Orchestration | chat rooms between agents (create/post/read/wait) | `lagent chat` | 6 | diff | todo |
| O4 | Orchestration | shipped skills paseo, advisor, committee, handoff, help, plugin, copied into `~/.claude/skills` etc. with a managed manifest | Lucina skills, same targets | 9 | manual | todo |
| M1 | MCP | ~30 agent tools (create/send/status/activity/list/cancel/archive/kill/update/mode, workspaces, scripts, terminals, schedules, heartbeats, providers, permissions, hosts) | secretary-mcp scope `agents` (Hermes too) | 9 | test per tool | todo |
| M2 | MCP | 22 `browser_*` tools (daemon asks a client browser host) | browser host in Secretary Code / web, or a headless browser on nezopt | 10 | manual | todo |
| M3 | MCP | `speak` | Lucina `/v1/tts` | 9 | manual | todo |
| H1 | Hosts | connected hosts over TCP / ssh / relay, fleet tools, cross-host notifyOnFinish | agentd ↔ agentd over SSH or Lucina Link | 10 | manual: Mac host | todo |
| X1 | Remote | pairing offer/QR, X25519 keypair, relay with Noise | QR login (Discord-style) + Lucina Link | 7 | kernel `qr_login` (Verus) + manual | todo |
| X2 | Remote | daemon password (bcrypt), `hello` handshake, 9 permission scopes | Secretary owner/device auth, MCP scopes | 7 | test | n/a once mapped |
| X3 | Remote | web UI | web `/agents` plugin | 8 | screenshots | todo |
| U1 | Usage | `paseo usage`: 9 quota sources (Claude, Codex, Copilot, Cursor, Grok, Kimi, MiniMax, OpenCode Go, Z.ai) | `/v1/agents/usage` | 2 | manual: compare numbers | todo |
| V1 | Speech | local STT (Parakeet), TTS (Kokoro), VAD; voice mode | Whisper (lucina-memo), `/v1/tts` | 8 | manual | todo |
| K1 | Plugins | plugin manifest, server API (rpc, settings, events, before-hooks, providers, usage sources, processes) | `lucina-plugin.json`, command hooks | 10 | test | todo |
| K2 | Plugins | client API (panels, file panels, screens, settings, sidebar, command center, slash commands, header buttons, composer pills, attachment sources, themes, timeline transformers/renderers, translations) | web plugin host (sandboxed iframes) | 10 | screenshots | todo |
| K3 | Hub | Hub enrollment, YAML triggers, executions | catalog + triggers in agentd | 10 | manual | todo |
| Q1 | Search | command center (rank by match then recency) | web command palette | 8 | screenshots | todo |
| SEC1 | Security | phone/relay scopes `hub.execute`, `daemon.manage`, `access.manage`, `tunnel.manage` (desktop RCE by design) | hardened: separate revocable `agents` + `read` token; MCP never answers permissions | 7/9 | test: scope kernel `mcp_scopes`, permission_gate | todo |
| SEC2 | Security | agent profiles attach MCP servers (`mcpServers`, `toolPolicy`) | hardened: agentd attaches the Lucina agent MCP with a per-agent scoped token; mutating tools need owner confirmation | 3/9 | test: a mutating tool call waits for Allow | todo |
| SEC3 | Security | agents can read the owner's `~/.claude.json` token and `~/.config/lucina-link/secrets.env` | hardened: deny-read for agent processes (sandbox / own uid); secrets through the Infisical broker; owner token rotated after S1 (owner decides) | 2/3 | test: an agent's `cat` of those paths fails | todo |
| SEC4 | Security | `acceptLegacyHello`, `shouldIgnorePostHelloPlaintext` (plaintext hello) | intentionally not ported: TLS-only device tokens, unix-socket daemon | 7 | review | n/a (not ported) |
| SEC5 | Security | `usesCleartextTraffic: true` | intentionally not ported: no cleartext in the web, iOS or Android apps | 8 | review: manifest/ATS checks | n/a (not ported) |
| SEC6 | Security | Android `SYSTEM_ALERT_WINDOW`, `RECEIVE_BOOT_COMPLETED`, launcher badge permissions | intentionally not ported: camera and microphone only, requested at first use | 8 | review: manifest diff | n/a (not ported) |
| SEC7 | Security | relay `relay.paseo.sh` / `rrorelay.paseo.sh`, ad-SDK traces, stray third-party URLs | intentionally not ported: no third-party relay or SDKs; Lucina Link only | 7 | review | n/a (not ported) |
| Z1 | Ops | supervisor/worker, pid lock, rotated logs, managed-process reaping | systemd user unit, journald, reaping on start | 1 | test: kill -9, restart, no orphans | wip: SIGTERM drain + exit 0 tested; process reaping comes with the real providers (phase 2) |
| Z2 | Ops | (Lucina only) no leftover worktrees, capped background CPU | `wt` lifecycle, `systemd-run --scope` | 4/5 | test | todo |
