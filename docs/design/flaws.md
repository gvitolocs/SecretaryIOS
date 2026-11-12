# Flaw tracker (first slice, 2026-11-02)

The owner notices small flaws across his clients and wants Lucina to manage them in one place. He reports in his own
words; she records them, starts the fixes without asking, and tells him how it went. A flaw closes on a client only
when someone shows it working on an installed build.

Crate `crates/lucina-flaws`, SQLite at `<LUCINA_DATA_DIR>/flaws/flaws.db`, in the lucina-rs runtime.

## What is stored

- `flaws`: the owner's exact words, title, severity, state, `duplicate_of`, `discovered_from`, thread, reporter, gate, and
  which step of the chain its triage agent ran on.
- `flaw_targets`: one row per flaw and platform (web, mac, android, ios, juno; Minerva and Diana later): layer (server,
  core, client), status, evidence, workspace, agent or session holding it, commit, deploy tag, build, attempts.
- `flaw_events`: the history. Append-only, enforced by triggers.

A flaw's state (`open`, `fixing`, `resolved`) follows from its targets. Only `duplicate` and `dismissed` are set by hand.

## The evidence rule

In `check_transition`, called by the store's only write path, so no route, tool or CLI can skip it:

- `not_affected` and `verified` are refused without evidence: a harness run naming the installed build, a debug event,
  or a screenshot.
- `code_reading` and `owner_report` open a target and never close one.
- `verified` comes only from `fixed`, and `fixed` needs a commit. A merge is `fixed`, not closed.
- A closed target can't be rewritten with weaker evidence. A regression reopens it (`affected`).

## Dispatch

The owner's rule (2026-11-02, replacing the first plan of low-severity only): Lucina starts fixes for every flaw without
asking, whatever the severity, and tells him the outcome. `lucina_decide` only for what can't be undone or needs his
credentials or a payment (a flaw's `gate`). His eyes only for visual checks on the iPhone.

All of it is `lucina_flaws::Policy`, from `LUCINA_FLAWS_*` with these defaults: one place, not a rule per caller.

**The chain.** `LUCINA_FLAWS_CHAIN` is the list of agents a try can run on, cheapest first, as `provider:model@probe-lane`
steps separated by commas. `:model` and `@probe-lane` may be left out; a step without a probe lane is always ready. The
default is `opencode:opencode/deepseek-v4.1-flash-free@zen-deepseek-flash, claude:claude-sonnet-5-5@claude-sonnet,
claude:claude-opus-5-5@claude-cli`: free agents first, a Claude model last. Codex and anything OpenAI are refused, and so
is a DeepSeek model that isn't a `-free` one. A chain the environment asks for that can't be run stops the server with the
reason; it never quietly dispatches on another agent. With no chain set, `LUCINA_FLAWS_PROVIDER`, `_MODEL` and
`_PROBE_LANE` build exactly one step, each with its own default, the way they did before.

**Triage's cheap end.** `LUCINA_FLAWS_TRIAGE_STEPS` (1) is how many steps from the top of the chain may look at a report.
Triage is cheap work, so it never reaches a step below that count, however ready one is; of the steps it may use it takes
the first whose probe says ready, walking down, and waits when none of them does. One free lane being out of quota for
hours therefore no longer stops every new report: with 2 the second cheap step takes the triage. A count above the chain is
the whole chain; 0 or something that isn't a number stops the server with the reason. Which step a triage agent ran on is
stored with the flaw (`flaws.triage_step`, nothing before the chain is a step), so a triage that ends on a limit marks out
that step's lane, not always the first one.

`crate::flaws::tick`, every minute in lucina-rs:

1. **Reconcile.** For each target an agent holds: a merged task records the commit and becomes `fixed`; an agent that ended
   without a merge gives the claim back and retries on a new branch, then `blocked` after 2 tries; after 45 minutes it is
   stopped; a flaw that became a duplicate or was dismissed stops its agent.
2. **Triage.** A report with clients nobody looked at gets one agent, once, that reads the code and sets status and layer
   (`LUCINA_FLAWS_AUTO_TRIAGE`). This is what turns a plain "emoji are boxes" into a target that can start. The rows it
   leaves open stay open. It runs on the cheap end of the chain only, `LUCINA_FLAWS_TRIAGE_STEPS` steps of it, and waits
   when none of those has quota: it never reaches an expensive step.
3. **Start.** Every `affected` target with a layer, held by nobody: worst first, at most `LUCINA_FLAWS_MAX_RUNNING` (2) per
   repo, triage agents included, so nezopt doesn't overheat. A claim is atomic, so nothing starts twice, and a failed start
   gives it back. Try *n* starts on step *n* of the chain (capped at the last one) and walks **down** to the first step whose
   probe says it has quota; it never comes back up to a cheaper step it started at. When no step from there down is ready,
   nothing starts this round. A server or core fix is one task and one commit for every client waiting on it.
4. **Tell him.** Fixed, closed or blocked targets, once per change, in his chat and on his phone.

`POST /flaws/{id}/dispatch` starts one under the same gates and cap. A session already on a target sets `session` on it
and the dispatcher leaves it alone.

Agents run through agentd on the step of the chain the try landed on, at medium effort (never Codex), in a `lagent task`
worktree. Their brief carries his words as data, not instructions. Which provider and model ran a try is in that target's
history entry and in the line she writes in his chat when it starts.

**Agents can't deploy.** agentd refuses `/pipeline/deploy` from an agent, and refuses it from `service:lucina`. That guard
stays: this slice only adds `service:lucina` to starting a task. So an agent stops at merged (`fixed`), a session deploys,
and the target closes with evidence after that. Lucina says so when she tells him.

**Quota.** An agent ending on a limit (Claude's weekly limit, 2026-11-02) isn't a failed fix: the claim goes back, the try
doesn't count and he isn't told it failed — and that step's lane is marked out, so the next round's walk down takes the step
below it. Each lane answers for itself: `lane probe` runs per step, its answer cached half an hour while the lane is ready
and an hour while it is out. Only when the walk finds no ready step at all does the target wait, so an outage leaves no
trail of worktrees. Then it starts by itself.

## Getting a report in

- A chat turn Jev routes to `flaw_report` (at least 0.6), or an explicit `flaw: …`, which needs no model and works when Jev
  is out. Owner's turns only: a report can start an agent, so guests, guarded turns and Lucina MCP callers can't. The Jev
  state is the usual template, facts only. With Jev out, a plain sentence stays a chat (the DeepSeek classifier prompt is
  pinned by parity goldens and doesn't list the intent).
- `message_claude` with `issue` records a flaw first, then sends.
- The `flaw` tool (owner's turns), the MCP's `flaw_*` tools, `/v1/flaws/*`, `lucina-rs flaws report`.

## Recording the pipeline

`lagent task start --flaw <id> --target <platform>` stores the link on the task. `lagent merge` then runs
`lucina-rs flaws merged`, and `lagent deploy` runs `lucina-rs flaws deployed` for each flaw fix the deployed commit holds
(kept only if that runtime carries the target's fix). agentd doesn't link the crate; the database belongs to Lucina.

## Regression vectors

`tests/conformance/<area>/*.json`: `input` payload, `expect` view model. `rich/` runs in
`crates/lucina-store/tests/conformance_rich.rs`, `web/src/lib/richConformance.test.ts` and `RichMessageTests.swift` (the folder is a
resource of the test bundle). The vectors found one difference on the first run: the
server's speech read list dashes aloud where the web drops them; the server now drops them too. LucinaOS can pin these later.

## Sources (docs/porting-guidelines.md)

Read in clones for this survey, behaviours only, no code taken:

| Source | Verdict | What |
|---|---|---|
| Symphony (OpenAI) | adapt | Claims; re-reading state after each turn and stopping work whose issue changed; proof of work before done |
| Beads | adapt | `discovered-from` links between issues |
| WPT / wpt.fyi | adapt | Results per product, bug links per result: our `flaw_targets` |
| `lucina-parity` (ours) | adopt | Passing needs evidence |
| Gas Town, Multica, Vibe Kanban, Backlog.md | reject | Whole platforms; the owner's decision is to extend what exists |

## Not in this slice

Screenshots through Playwright, an app page, a matrix merged with LucinaOS's `features.toml`, and a verifying agent after a
deploy (a `fixed` target waits for someone to show it). `lucina-store` has no SQLite despite the first plan's note: the
pattern followed is `lucina-notebooks`' store.
