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

1. **Reconcile.** For each target an agent holds: a merged task records the commit and becomes `fixed`. An agent that
   ended without a merge is put away (stopped and archived) and its claim ends one of two ways. A *lane failure* (its error
   says the sign-in was refused, the quota or limit is out, or the provider is unavailable; or it never provably worked: no
   tool call on its timeline, no commit or change in its workspace) gives the claim back, counts no attempt and opens the
   lane's breaker. A *task failure* (the agent provably worked and did not fix it) is a counted attempt: the target waits
   5 minutes, doubling per failure up to 2 hours, and the third task failure inside 24 hours (`LUCINA_FLAWS_MAX_ATTEMPTS`)
   makes it `blocked` for a lead, and he is told once. After 45 minutes an agent is stopped (a task failure). A flaw that
   became a duplicate or was dismissed stops its agent. Every release carries the agent and claim it read, and touches
   nothing newer. A claim with no agent after 5 minutes asks agentd by labels first: an agent that exists is attached, none
   is a give-back, never a counted try.
2. **Triage.** A report with clients nobody looked at gets one agent, once, that reads the code and sets status and layer
   (`LUCINA_FLAWS_AUTO_TRIAGE`). This is what turns a plain "emoji are boxes" into a target that can start. The rows it
   leaves open stay open. It runs on the cheap end of the chain only, `LUCINA_FLAWS_TRIAGE_STEPS` steps of it, and waits
   when none of those has quota: it never reaches an expensive step.
3. **Start.** Every `affected` target with a layer, held by nobody and past its back-off: worst first, under caps on agents
   *alive* (the union, by agent id, of `fixing` rows and what agentd lists with the label `flaw`, triage agents included, from
   one `GET /agents` per tick; agentd that cannot be asked means no dispatch): `LUCINA_FLAWS_MAX_ALIVE` (3) in all,
   `LUCINA_FLAWS_MAX_PER_LANE` (1) per lane, `LUCINA_FLAWS_MAX_RUNNING` (2) per repo, and at most
   `LUCINA_FLAWS_MAX_NEW_PER_TICK` (1) new dispatch per tick, so nezopt doesn't overheat. An owner-approved gated flaw counts
   like any other. One dispatch lock serialises cap check, claim, create and attach for the loop and for `POST
   /flaws/{id}/dispatch`; the claim re-checks the counts in SQL (a second process on the file), and a refused claim is `409
   busy` with nothing started. The create carries the idempotency key `flaw:<id>:<platform>:<claim number>`; a create that
   errors looks agentd up by labels before it gives anything back, and an agent that exists is attached. Every dispatch
   writes one line, `flaws: dispatched flaw=.. platform=.. caller=loop|post|mcp attempt=.. lane=.. agent=..`, and a
   refusal one line with its reason.

   What the SQL re-check in the claim counts, and what it does not: it counts *claims* (`fixing` rows without a session, and
   triage agents the store tracks), not processes. An agent that agentd holds without a `fixing` row (a create whose answer was
   lost, a target let go while its agent ran) is invisible to it. Three things protect against that: the dispatch lock, under
   which the `alive` list and the claim are one step; the idempotency key, so a repeated create for one claim is the same agent
   and never a second; and the sweep, which before a start stops any untracked agent already working on that target (and any
   idle agent whose target no longer holds it), so a start never adds a second live agent to a target. A second process on the
   file is covered by the SQL re-check alone, and its agentd-only agents are not: one agentd, one dispatching server is the
   deployed shape.

   **How long the lock can be held (`MAX_HOLD`).** The lock is never waited on by a caller that has someone waiting on it.
   `POST /flaws/{id}/dispatch` and the MCP tool `flaw_dispatch` wait for it at most 5 s (`Limits::lock_wait`); then they start
   nothing and answer `409 dispatcher_busy` with `retry_after_s` (the worst-case time left of the hold; the MCP tool says
   "the dispatcher is busy, retry in N s. Nothing was started."). The minute loop never waits: it tries the lock and, if a
   request holds it, skips that tick and says `flaws: dispatcher busy, tick skipped` once per change of state, so ticks never
   queue. Every call made under the lock has its own deadline (`Limits`): `GET /agents` 5 s, each stop and each archive 5 s,
   the worktree create and the agent create 25 s each, the lookup by labels after a failed create 5 s, the ops record of an
   opened breaker 5 s, one lane probe 10 s with all the probes of a hold under one 15 s budget (a probe that does not answer, or
   that finds the budget spent, is not ready). Agents put away in one hold are bounded (`MAX_AWAY` = 3 for the sweep and the
   untracked agents of the target, 3 more extras after a failed create); an untracked agent that cannot be stopped in time
   stops the start on its target, which waits for the next tick. The owner's go for a gated flaw is asked before the lock is
   taken. The sum, `MAX_HOLD`, is 5 + 3 x 10 + 15 + 2 x 25 + (5 + 4 x 10) = **145 s** (the first
   draft had 240, the second 140 and left out the put-away of the agent a lookup found; a three-step chain whose every call hangs is the case it bounds). A hold is milliseconds when agentd and the
   lanes answer. A request that cannot wait that long is told busy after 5 s; one whose client gives up earlier (the MCP client
   waits 20 s) leaves its claim without an agent, which the stale-claim sweep looks up in agentd after 5 minutes. Requests
   build their own probe memo (`dispatch_one` cannot share the tick's, which lives for one tick); only the lane breaker, in the
   store, is shared. Triage starts under the same lock and the same deadlines, one start a tick. A create that
   refuses (503/429) or does not answer in time is the lane's failure, for a fix and for triage alike: the breaker opens, so a create
   that agentd finishes late cannot be started and stopped again every tick. A request also leaves a lane alone for a minute after it
   opened (its probes are its own), and the target the lane blocked is told as such, not as "after 0 tries". Moving the probes
   outside the lock is a follow-up. Try *n* starts on step *n* of the chain (capped at the last one) and walks **down** to the first step whose
   probe says it has quota; it never comes back up to a cheaper step it started at. When no step from there down is ready,
   nothing starts this round. A server or core fix is one task and one commit for every client waiting on it.
4. **Tell him.** Fixed, closed or blocked targets, once per change, in his chat and on his phone.

`POST /flaws/{id}/dispatch` (`{platform, caller?}`) starts one under the same gates and caps. A session already on a target sets `session` on it
and the dispatcher leaves it alone.

Agents run through agentd on the step of the chain the try landed on, at medium effort (never Codex), in a `lagent task`
worktree. Their brief carries his words as data, not instructions. Which provider and model ran a try is in that target's
history entry and in the line she writes in his chat when it starts.

**Agents can't deploy.** agentd refuses `/pipeline/deploy` from an agent, and refuses it from `service:lucina`. That guard
stays: this slice only adds `service:lucina` to starting a task. So an agent stops at merged (`fixed`), a session deploys,
and the target closes with evidence after that. Lucina says so when she tells him.

**The lane breaker (DISP-01, the 2026-11-19 storm: 29 agents failed at sign-in in ten minutes).** An agent ending on a limit
(Claude's weekly limit, 2026-11-02), a refused sign-in or an unavailable provider isn't a failed fix: the claim goes back, the
try doesn't count and he isn't told it failed. That lane's breaker, kept in the flaw store so it survives a restart, opens on
the first such failure, whatever was cached about the lane being ready, and one ops record says so once. While it is open
nothing is dispatched on the lane and the walk down passes it; every tick probes it, and a probe that cannot run, times out or
says anything but ok is not ready. A passing probe makes it half-open: exactly one agent may try the lane, and its first
turn succeeding closes the breaker (failing opens it again). `lucina-rs flaws lane clear <lane>` and `POST
/flaws/lanes/{lane}/clear` close it by hand; `lucina-rs flaws lanes` and `GET /flaws/lanes` list the breakers. A create
answered `503 PROVIDER_TRIPPED` or `429 AGENT_LIMIT` (agentd's own breaker and create caps) is the same failure. Only when
the walk finds no usable step at all does the target wait, so an outage leaves no trail of worktrees. Then it starts by itself.
After a deploy, `lucina-rs flaws restore-lane-failures --since <rfc3339> [--apply]` lists, and with `--apply` resets, the
blocked targets whose every failure since was a lane failure by the stored agent error text.

**Lane failures that repeat (LA-C035 review).** A lane failure costs a target no attempt, but the second one on a target
inside 24 hours backs it off 5 minutes and the third (`LANE_FAILURES_BEFORE_LEAD`) marks it `blocked`, "needs a lead", told once:
an agent that ends with no tool call and no commit on a lane whose probe says ok would otherwise go round the half-open slot
for ever. The breaker keeps its own back-off: a trial agent that fails shuts the lane again for 5 minutes, doubling per failed
trial (`lane_breakers.until`, `fails`), and a lane is never healed in the tick that opened it. Only the trial agent (the claim a
half-open lane let through, `lane_breakers.trial`) closes it, on a first turn that ran or a work-doing idle end. The ops
record's key is the lane and how many times it has opened, never a time. Triage is covered the same way: a triage agent that
ends on a sign-in failure, a limit or an unavailable provider opens its lane's breaker and leaves the flaw triage-due (given back,
three times at most, as for a limit); any other end is an answer, as before.

**Deploy notes.** (1) `restore-lane-failures --apply` sets `attempts` to 0, so a target that also failed for real reasons
before `--since` gets those tries back; the dry run says so. A lead confirms the lane really signs in (one real agent turn,
not `lane probe`, which said ok during the incident) before `--apply`. (2) The migration only adds columns and tables. A rollback
to the old binary on the migrated database works, but it runs without any of this protection (no caps, no breaker, no lock): that
is the reason to fix forward, not to roll back, if a problem shows after deploy.

**The lane probe's cache (changed in LA-C035).** `LANE_OUT_FOR`, how long a lane that answered not-ready is remembered before
it is probed again, went from 60 minutes to 5. It used to be the only memory of a lane being out, and an hour was cheap because
nothing else could bring the lane back. Now the breaker in the store holds "out" across a restart and a probe is asked on every
tick while it is open, so the cache only spares closed lanes a provider call per pick; a long "out" would keep a lane that
is back waiting for no reason. `LANE_READY_FOR` stays 30 minutes, and a ready answer is dropped the moment a failure is seen
on that lane.

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
