# Hermes Agent parity: Hermes vs Lucina's Rust brain (2026-11-05)

Hermes Agent (NousResearch, Python, MIT) is a **reference** for behaviour, never run as Lucina's brain. Owner rules
(2026-11-04/05): implement in our Rust only what we are missing; where both have a feature and do it differently, keep
ours and record here what Hermes does better and a proposal; change ours only where it was a proven cause of a failure.
Prefer code we already have and have proved (LucinaCode, Lucina Agents, OpenBot) over a new port. Rules for every port:
`docs/porting-guidelines.md`. Earlier decision record: `docs/lucina-rust/hermes-sync-2026-11-04.md`.

- Reference copies on nezopt `~/scratch/hermes-ref/`: `agent-v0.22.7` (b200f5bc, latest release), `agent-main`
  (ed1afb08, 2026-11-04), `plugins-main` (83f0a89). Our snapshot compared: SecretaryIOS main f0a2f607.
- Licences: Hermes MIT ("Copyright (c) 2025 Nous Research"), plugins MIT, OpenCode MIT (LucinaCode is our port).
- Raw area reports (Mistral Vibe lanes V1-V4, Qwen for OpenBot), with file:line cites: `~/scratch/hermes-ref/reports/`.
  Rows below were checked against the code; two report claims were wrong and are corrected here (marked ✱).

Status: **same**, **differs** (both have it, keep ours unless a proven cause), **missing** (we lack it), **ours-only**.
Failure classes from the 2026-11-04 Juno Chat turns: (a) re-listing because results hid the payload, (b) stopping
short / last round toolless, (c) bridge call missing a required argument with a refusal that did not name it,
(d) routing miss to a no-tool planner that narrated an action, (e) weak continuation after "go ahead".

## What failed, and what fixed it (the proven causes)

| cause (turn) | evidence | fix | source | where |
|---|---|---|---|---|
| Device replies reached the model as `text` only: intents list, undo id, undo list dropped (1, 2) | each `intents` result grew the prompt ~30 tokens; 4-5 re-lists per turn | structured fields forwarded under the MAX_TEXT clip | ours (juno-lucina-tools) | main 12f2860f, f0a2f607 |
| same, device side | devd `text` held only "N intents" | intents/intent/undo `text` carries the lists, undo id | ours (juno-intents) | LucinaOS 8dbb01f4f5 (next generation) |
| Last round sent with tool_choice none: 10 rounds = 9 acting rounds; refusal in round 9 unfixable (2) | journal: intent refused in round 9, round 10 toolless | every budgeted round keeps tools + one toolless summary request | Hermes `handle_max_iterations` / LucinaCode `MAX_STEPS_PROMPT` (same behaviour) | main 9bf8c1b4 |
| Bridge args never checked against the target's schema; refusal named nothing (2) | "body is needed" | juno_call checks `intent` and driver-tool args against the laptop's listed schemas, sends nothing when a needed one is missing; missing args named for every tool | Hermes `validate_deferred_call_args` | main 9bf8c1b4 (all tools), main 1a16a19b (bridge) |
| Jev's top tool `notes` (0.41) → no loop → planner narrated "Sending all three now" (3) | journal: purpose=plan, no calls | Juno sessions run the loop with Jev's top tool added; no-tool rule on the planner path | ours (juno-lucina-tools); Hermes has no routing step | main 12f2860f |
| Each turn rediscovered tools/intents (2) | go-ahead turn re-ran juno_tools, intents, list_apps | laptop's lists cached 10 min and put in the loop's system text | ours (juno-lucina-tools) ≈ Hermes tool-search manifest | main 12f2860f |

## Agent loop and tool calling

| feature | Hermes | ours | status | Hermes better at | proposal | a-e |
|---|---|---|---|---|---|---|
| Iteration budget and the cap | `iteration_budget.py`, `turn_iteration_prep.py`: every iteration has tools; `handle_max_iterations` asks once without tools | `agent.rs::run`: same since 9bf8c1b4 (was: last round toolless) | same | — | done (proven cause) | b |
| Budget warning | `_maybe_inject_iteration_budget_warning`, opt-in ratio | `budget_note_ratio`, opt-in | same | — | done (was missing) | a |
| Unknown tool names | `repair_tool_call` (incl. fuzzy), error lists valid names, 3-strike exit | `repair_name` (no fuzzy), lists names, no strike cap | differs | strike cap | keep ours: the round budget bounds it | — |
| Malformed / cut-off JSON arguments | retry the request up to 3×, then error results; args not ending in `}`/`]` refused as truncated | one "not valid JSON" error result | missing (cut-off detection) | names the cut-off case | port: LucinaCode INPUT_CUT_BY_LENGTH / INPUT_NOT_JSON texts + Hermes' `}`/`]` rule (this commit, loop-guards) | b, c |
| Output-limit continuation | finish_reason length handled in `turn_truncation.py` | none: a cut reply was taken as the answer | missing | — | port LucinaCode `oc-agent` (bench-proven 2026-11-04): continue nudge, last call of a cut reply not run, 3 in a row stop (this commit, loop-guards) | b |
| Schema validation of arguments | `validate_deferred_call_args` (bridge), registry coercion | `Registry::validate` + named missing args (9bf8c1b4) | same | arg coercion (`arg_coercion.py`) | keep ours | c |
| Repeat / cycle guard | `tool_guardrails.py`: identical-call streak notice at 3, 2-4-call cycle notice, duplicate-result stub ≥512 chars, hard stop at 5 on unattended platforms | none | missing | stops replay loops | port (this commit, loop-guards); OpenCode's `doom_loop` (3× identical → ask) is the same idea, ask-the-user variant rejected for chat | a |
| Parallel tool calls | thread pool, 8 workers | sequential in a round | differs | latency | keep ours (deadline accounting simpler); revisit if latency is a measured problem | — |
| Deferred tools / tool search | `tool_search` / `tool_describe` / `tool_call` bridge, manifest within a token budget | full registry each round; juno_* lists in system text (12f2860f) | differs | scales to hundreds of tools | keep ours while the registry is ~45 tools | a |
| Tool-list selection per turn | toolsets, postures | Jev route hint (never a filter); skill turns pick registries | differs | — | keep ours (Jev is the owner's rule) | d |
| Result size and spill | 100k/result, 200k/turn, spill to file with preview | 8000-char head clip as `{"truncated","partial"}` | differs | keeps the tail; nothing lost | proposal: head+tail clip with omitted count (LucinaCode `oc-tools` format); not a cause in these turns | a |
| Error / refusal shape | tool-role results, typed error classes | `{"error": …}` tool results | same | error classification | keep ours | c |
| Empty / reasoning-only replies | recovery ladder (nudges, prefill, fallback provider) | one plain re-ask | differs | depth | keep ours; no failure traced to it | b |
| Provider tool-call parsing | sanitizers, id uniquify | DSML recovery, native calls | same | — | keep ours (no parse failure in the journal) | — |
| Reasoning replay within a turn ✱ | carried | `reasoning_content` kept on assistant messages and serialized back for DeepSeek (lucina-llm) | same | — | keep (V4 said missing; it is not) | — |
| Confirmation gates | approval policies, prompts | `call_gate` + PendingStore, MCP refused, money denied | same (UX differs) | — | keep ours | — |
| Interrupt / deadline | /stop interrupts tools | total deadline + per-tool timeout | differs | mid-tool cancel | keep ours | — |

## Context, memory, planning, continuation

| feature | Hermes | ours | status | Hermes better at | proposal | a-e |
|---|---|---|---|---|---|---|
| Cross-turn history ✱ | full transcript replay incl. tool calls and results | recent turns as text + tool trace lines; a tool loop starts from `[system, user]` | differs | the next turn sees earlier results | partly fixed for Juno by the cached lists (12f2860f). Proposal: carry the previous loop's tool messages (clipped) into the next loop of the same session within 15 min; proven in LucinaCode (session store) and Lucina Agents `fork_context.rs` | e |
| Continuation / "go ahead" | transcript + todo rehydrated | PendingStore for gated actions; Jev facts of the previous turn | differs | resumes a plan | same proposal as above | e |
| Compaction | `context_compressor.py`, prepared compaction | none in the loop (turns are short) | missing | long sessions | not needed for chat turns; LucinaCode `compaction.rs` is the source when needed | — |
| Todo / plan tool | `todo_tool.py`, folded into compaction | tasks + notes tools (store-backed); no per-turn plan | differs | in-turn checklist | keep ours; PORT_PLAN P2 | — |
| Memory tools | `memory_tool.py` + providers + background review | Honcho context per turn, no model-facing memory writes | differs (by plan) | grounded write rules | keep Honcho; reference Hermes' grounding rule (4a8cc6f705) for Honcho conclusions | — |
| System prompt assembly | built once per session, stable prefix | rebuilt per turn | differs | cache hits | keep ours (DeepSeek cache hit 7.7k of 9.8k tokens already) | — |
| Skills index | skills catalog in the prompt, skill_view loads | skill turns in lucina-server, no index | differs | discoverability | keep ours (skills hub skipped by plan) | — |
| Per-turn context budgets | token preflight | char budgets per block | differs | precision | keep ours | — |

## Tools, skills, plugins

Every Hermes toolset was mapped in `reports/v3-tools.md`. Summary:
- **Skipped by PORT_PLAN section 7 on purpose** (stay skipped): terminal, file tools, execute_code, browser_*, kanban,
  skills hub, desktop/TUI tools, x_search, video/image generation, platform bundles other than Telegram/Discord.
- **Planned, still missing**: web_search/web_extract (P1), clarify (P1), TTS through Kokoro (P2), recurring cron (phase 6).
  No failure traced to them; they stay on the plan.
- **Both, different, keep ours**: vision/media, computer use (lucina-computer-use + Cua), reminders vs cronjob, tasks/notes
  vs todo, delegation (ask_claude/grok/chatgpt), Discord admin (12 ops, no deletes), memory (Honcho).
- **Ours only** (kept, owner rule): Jev routing, the juno_* device bridge, the secret keeper and secret redaction,
  the secretary surface (calendar, shopping, places, AU campus, callers, memos, media, diagnostics, reach/message_*),
  ops/flaws tools, meshy.
- **Plugins** (hermes-official-plugins e9910ad → 83f0a89): none is enabled for Lucina; computer_task's Hermes install
  enables only `cua-spaces` (trycua). Changes: agent-importers, blender, bubblewrap (needs Hermes ≥ 0.22.6), camofox,
  directsdk, meshy (27 commits). Nothing to port.

## In-house sources reused

| problem | proven source we already have | used for |
|---|---|---|
| cap behaviour | LucinaCode `oc-agent` MAX_STEPS (toolless last step, tools kept for cache) = Hermes summary | 9bf8c1b4 |
| output-limit continuation | LucinaCode `oc-agent` (bench 2026-11-04, `bench/results/2026-11-04-reasoning-replay.json`) | loop-guards |
| cut-off / non-JSON arguments | LucinaCode INPUT_CUT_BY_LENGTH / INPUT_NOT_JSON | loop-guards |
| repeat calls | OpenCode `doom_loop` (3×, ask) and Hermes `tool_guardrails` (warn/stub/stop) | loop-guards (Hermes variant: no human to ask in chat) |
| continuation with tool history | LucinaCode session store; Lucina Agents `fork_context.rs` | proposal |
| OpenBot | see `reports/q-openbot.md` | — |

## Open proposals (not done; each needs its own go-ahead)
1. Carry the previous tool loop's messages into the next turn of the same session (e).
2. Head+tail clip with an omitted-bytes count for long tool results (a).
3. Arg coercion before validation (Hermes `arg_coercion.py`) for numbers sent as strings (c).
4. Port the planned missing tools (web_search/web_extract, clarify) per PORT_PLAN.
