# Lucina delegates: Claude, Grok, ChatGPT

Lucina hands work to other AI agents **on her own**. Until now it only went the other way: Claude Code,
Grok and ChatGPT called Lucina through Lucina MCP. The owner's policy (2026-10-22): *"make Lucina act and be
judgmental on herself"*. Lucina does not ask for confirmation before she delegates. She reviews herself,
acts on her own decision and shows what she did and why.

Code: crate `crates/lucina-delegate` (engine, policy, agents, sandbox, state, audit, cards), the tools in
`crates/lucina-tools/src/tools/delegate.rs`, and the server glue in `crates/lucina-server/src/delegation.rs`.
The policy file is `deploy/lucina-delegate/delegate.example.toml`.

## How a delegation runs

1. **Entry points.**
   - JSON planner path (the live one, `LUCINA_PLANNER=json`):
     - explicit asks parsed before the planner: "chiedi a Grok …", "ask Claude to …", "delega …",
       "fai una ricerca su …", "latest news on …";
     - the planner's own action `{"action":"delegate_task","to":"auto|grok|claude|chatgpt","kind":…,"task":…}`.
       The plan prompt offers it only when `LUCINA_DELEGATE=true`; with delegation off it stays the Node parity prompt.
   - Tools path (`LUCINA_PLANNER=tools`): `ask_claude`, `ask_grok`, `ask_chatgpt` and the router `delegate`.
     They use `SideEffect::Delegate`, so they never create a pending action.
   - Proactive engine: `POST /api/secretary/delegate` with `origin: "proactive"`. It is read-only research only
     and `dryRun` by default.
2. **Hard limits** ([`policy.rs`](../../crates/lucina-delegate/src/policy.rs)). See the table below.
3. **Agent choice.**
   - Research and news go to Grok, then ChatGPT, then Claude. Code goes to Claude only.
   - Analysis goes to Claude, then ChatGPT, then Grok. Questions go to ChatGPT, then Claude, then Grok.
   - Lucina skips any agent the owner switched off, an agent that is busy (one job per agent), or one with
     no working backend.
4. **Budget.** The daily caps work as a runaway guard (see Budget below).
5. **Self-review** ([`judge.rs`](../../crates/lucina-delegate/src/judge.rs)).
   - It is one JSON-mode DeepSeek call that returns `{goal, risk: low|med|high, reversible, why, decision: act|skip|revise, revised_task}`.
   - `revise` sends a minimal, self-contained task instead, for example without private details. The hard limits are
     checked again on the revised task.
   - High risk with an irreversible effect is allowed when she judges it right. The card then says so plainly.
   - If DeepSeek is down she still acts: the work is read-only or sandboxed, and the card says the review was
     unavailable.
6. **Sandbox and prompt.**
   - Every job gets `~/scratch/lucina-delegate/<job>/`. The prompt is built with the redactor and has a header:
     `[Lucina delegation · job … · depth 1/1]`, who is asking (Lucina, on Giuseppe's behalf), and the rules.
   - The prompt is Italian or English, following the task.
7. **Background job.**
   - A chat turn waits up to `inline_wait_secs` (20) for a quick answer. After that the answer arrives as a
     timeline card plus one app notification.
   - Answers longer than 1200 characters, and every code report, are saved as a note (`/v1/notes`, tags
     `lucina, delegation, <agent>`). The chat gets a 2-3 sentence summary.
8. **Card** (Lucina message format v1, `meta.kind = "delegation"`).
   - It shows "Ho chiesto a Grok: «…» ✅ risposta pronta · 42 s · xai-api", the answer, and "Perché: … (rischio basso,
     reversibile)".
   - It has a button for the note and, for code, the undo command plus an "Annulla" button that prefills
     "annulla delega <id>".

## Policy table

| | What | Who decides |
|---|---|---|
| **Autonomous** | Questions, research, analysis to any agent; code in a fresh sandbox; code on a repo as a sandbox clone on branch `lucina/<job>`; anything her self-review says `act` to, including posts or sends an agent would do, which are marked high-risk on the card | Lucina (self-review), no confirmation |
| **Paused, owner told once** | A daily cap reached (all delegation, or that agent) | Budget guard; resumes at midnight |
| **Switched off by the owner** | "non usare Grok", "don't use Claude", "stop delegating" (persisted); back on with "puoi usare di nuovo Grok", "riprendi a delegare" | The owner |
| **Never (hard limits)** | Secrets or `secret://` refs in an outgoing prompt (the redactor runs on every prompt, twice); purchases or money movement; a delegation from a Lucina MCP turn or from a delegated agent (depth max 1, prompt marker); sending the task back to the agent that asked; code outside the bubblewrap sandbox; pushing, touching `main`, deploying | Code: `policy::hard_limits`, `sandbox`. The self-review cannot lift them. |

## The agents and their paths

| Agent | Path (`backend`) | Notes |
|---|---|---|
| Claude | `claude-cli`: `~/.local/bin/claude -p --model claude-opus-5-5 --effort medium --output-format json --max-turns N --strict-mcp-config` | The owner's model and effort, never Fast. It runs inside bubblewrap with `--dangerously-skip-permissions`; the fence is bwrap. It has no MCP servers, so it cannot reach Lucina MCP. `--resume <session>` handles follow-ups (`Request.resume_job`). |
| Grok | `gbot`: the Grok Bot desktop session on nezopt (`gbot send` / `gbot thread`), or `xai-api`: the xAI Responses API with `web_search` and `x_search` | `auto` prefers gbot when its gateway descriptor exists (the app is signed in), else xAI if a key is configured. |
| ChatGPT | `openai-api`: the OpenAI Responses API when a key is configured, else `codex-cli`: `codex [--search] exec -m gpt-6.1-sol -c model_reasoning_effort=medium -c mcp_servers={} -s read-only` | Codex is signed in with the owner's ChatGPT account. It does no code (that goes to Claude). |

ChatGPT Desktop over CDP, through the grok-bot MCP, is not used. It is reachable only on the Mac's loopback, and Lucina
runs on nezopt.

## Sandbox (bubblewrap)

- The system is mounted read-only. Only the job directory is writable, plus the agent's own state (`~/.claude`,
  `~/.claude.json`, `~/.codex`).
- Their instruction and settings files are read-only on top: `CLAUDE.md`, `settings*.json`, agents, commands, skills,
  plugins, hooks; `config.toml`, `AGENTS.md`.
- Hidden with tmpfs: `~/Projects` (other repos), `~/scratch` (other jobs), `~/.ssh`, `~/.config`, `~/secrets`,
  keyrings, `~/.infisical`, `~/.docker`, `~/mnt`, `/run/user` (no systemd user bus, no D-Bus), and the docker socket.
  Also hidden: `~/.git-credentials`, `.netrc`, `.npmrc`, `.pypirc`.
- So there is no SSH to p1 or p2, no `systemctl`, no docker, no push and no deploy. The network stays on, because the
  agents need their APIs.
- Code on a repo:
  - The repo is cloned with `--no-hardlinks` into `<job>/repo` on `lucina/<job8>`, and `origin` is removed.
  - Afterwards the branch (only if it has commits) is fetched back into the real repo as `lucina/<job8>`.
  - To undo: `git -C <repo> branch -D lucina/<job8> && rm -rf <sandbox>`, or say "annulla delega <job8>".

## Budget (runaway guard)

The defaults are generous:

| Cap | Default |
|---|---|
| All delegations | 200 per day |
| Calls per agent | 80 per day |
| Claude wall time | 8 h per day |
| Grok and ChatGPT wall time | 4 h per day each |
| Claude turns | 1500 per day (`--max-turns` is the per-job cap of 40, lowered to what is left today) |

A call that fits is never asked about. The call that hits a cap pauses delegation (all of it, or that agent) until
midnight, writes one timeline message and sends one notification. A skipped self-review gives its call back. State lives
in `<data>/lucina-rs/delegate-state.json`.

## Transparency, audit, logs

- **Timeline:**
  - One card per delegation. A second "⏳ in corso" card appears first when the job outlives the inline wait.
  - The turn's own reply relays the answer in Lucina's words (`generate_skill_reply`) and carries `meta.kind = "delegation"`.
- **Audit:** `<data>/lucina-rs/delegate-audit.jsonl`.
  - It records time, job, agent, backend, kind, origin, outcome (or `refused:<code>`), decision, risk, reversible,
    judge source, duration, turns, whether a note exists, the branch, and the budget used.
  - It never records the task or the answer. Lines go through the redactor and to the log (`lucina_delegate::audit`).
- **Jobs:** `<data>/lucina-rs/delegate-jobs.json` keeps the last 100.
  - The task is stored redacted and clipped to 160 characters, the result redacted and clipped to 8000.
  - A job left running by a restart is marked interrupted.
- **API** (bearer token):
  - `GET /api/secretary/delegations`: switches, budget, agent backends, recent jobs.
  - `GET /api/secretary/delegations/<id or 8+ prefix>`.
  - `POST /api/secretary/delegate`: proactive only.

## Proactive engine

`lucina-proactive` (branch `lucina-proactive`, dry run) uses `lucina_delegate::proactive::ProactiveClient` against
`POST /api/secretary/delegate`:

- `{origin: "proactive", kind: "research", dryRun: true}` returns the agent Lucina would pick and her judgement.
  Nothing is sent to an agent.
- With `dryRun: false` the request goes through the same limits, budget, cards and audit. Code and workspaces are refused.

The engine stays in dry run until the owner turns it live.

## Configuration

| Variable | Meaning |
|---|---|
| `LUCINA_DELEGATE=true` | Turns delegation on (default off). |
| `LUCINA_DELEGATE_CONFIG=~/.config/lucina-rs/delegate.toml` | The policy file. Without it, the defaults apply. |
| xAI and OpenAI keys | A secrets-skill setting (`delegate.xai_api_key`, `delegate.openai_api_key` in `secrets.toml [refs]`) or a key file (`xai_key_file`, `openai_key_file`, 0600). Never in the TOML. |

## Live state on nezopt (2026-10-22)

- **Claude:** works (CLI 2.1.295, logged in).
- **ChatGPT:** works through Codex (signed in with ChatGPT, `gpt-6.1-sol`). There is no OpenAI API key: Node's `OPENAI_*`
  points at a dead local proxy on 127.0.0.1:18080.
- **Grok:** no working path yet.
  - The Grok Bot desktop app on nezopt is signed out (`desktop-status.json: signedIn false`, no `gateway-descriptor.json`).
  - No xAI key exists in the environment or the visible Infisical folders.
  - Sign in to the Grok Bot app on nezopt's desktop (`/opt/Grok Bot/grok-bot`), or add an xAI key. Until then Lucina
    routes research to ChatGPT (Codex with live web search).
