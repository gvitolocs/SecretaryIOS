# Hermes Agent sync, v0.22.5 → v0.22.7 / main ed1afb082e (2026-11-04)

Why: Lucina's stage C turns from Juno's Chat failed on 2026-11-04 (22:05-22:10 UTC). The owner suspected a deeper
pipeline failure and asked for the Hermes Agent updates to be checked and ported. Rules: `docs/porting-guidelines.md`.
Licence checked: MIT at v0.22.7 and at main (`LICENSE`, "Copyright (c) 2025 Nous Research"); hermes-official-plugins
MIT at 83f0a896. Code translated closely carries an "adapted from NousResearch/hermes-agent, MIT" note.

## Root causes found in our pipeline (evidence: lucina-rs journal, timeline)

1. Device replies reached the model as `text` only (lucina-tools `tools/juno.rs::answer`). devd's `intents`
   text is just "N intents; X is in front"; `intent`'s `action` (undo id) and `undo`'s list were dropped. She
   re-listed intents 4-5 times per turn and used every round. Fixed in juno-turn-fix2 (structured fields).
2. The loop's last round ran with `tool_choice: none`: 10 rounds were 9 acting rounds, so a refusal in round 9
   could not be put right. Fixed here (Hermes budget + summary request).
3. Calls behind the generic `juno_call {tool, arguments}` bridge are never checked against their schema, so a
   missing `body` went to the laptop and came back as "body is needed". Fixed here (Hermes deferred-call validation).
4. Jev's first-tool candidates do not include the laptop's intents; "create the note" picked the owner's `notes`
   tool (0.41) and the turn went to the no-tool planner, which narrated an action. Fixed in juno-turn-fix2
   (JunoWith + NO_TOOL_RULE). Hermes has no counterpart (no routing step).

Not causes: tool-call parsing (deepseek-flash, native tool_calls, no markup warnings), memory duplication (memory is
only in the user block), compaction (none in the loop; prompts 4.7k-11.8k tokens), the 8000-char result clip.

## Verdicts

| id | Hermes behaviour (file, commits) | verdict | ours |
|---|---|---|---|
| iteration-budget-summary | every budgeted iteration keeps its tools; when the budget is used, one request without tools asks for the final answer (`turn_iteration_prep.py`, `chat_completion_helpers.handle_max_iterations`; unchanged since v0.22.5) | adapt | `agent::run`: `max_rounds` acting rounds, then one summary request (`SUMMARY_NOTE` or `last_round_note`) |
| iteration-budget-warning | opt-in one-shot notice appended to the newest tool result at `budget_warning_ratio` (`turn_iteration_prep._maybe_inject_iteration_budget_warning`) | adapt | `AgentConfig::budget_note_ratio`; wording asks for the doing calls over more listing |
| tool-name-repair | case/separator/`_tool`-suffix repair before rejecting an unknown name (`agent_runtime_helpers.repair_tool_call`) | adapt | `agent::repair_name`, without the fuzzy (difflib) step |
| unknown-tool-error | unknown name answered with the available names; an empty name gets a terse anti-priming error (`conversation_loop._invalid_tool_name_error_content`) | adopt | `agent::execute` |
| missing-args-named | invalid arguments name the missing required ones, say the tool was NOT invoked and carry the schema (`tools/tool_search_validation.validate_deferred_call_args`) | adapt | `agent::execute` for every tool; `juno_call` checks `intent` and driver-tool arguments against the laptop's listed schemas before sending |
| tool-search-manifest | deferred tools listed in the prompt within a token budget (079b38cd07, 95505c60e9) | reference | juno-turn-fix2's `known_lists` in the loop's system text is the same idea |
| tool-desc-lead-capability | descriptions lead with the key capability (95505c60e9) | reference | worth a later pass over lucina-tools descriptions |
| spill-stub-persist | spilled results stored as the model saw them (614fddb694) | reference | our clip is in-memory per turn |
| empty-reply-housekeeping | an empty final reply after a housekeeping round ends on the text beside it (5cba2718c0) | reference | our re-ask covers empty last replies |
| memory-error-names-field | memory refusals name `operations[]` (ff706b7787, 38968d7552) | reference | Honcho replaces Hermes memory |
| bgreview-grounded-memory | background memory review saves only supported facts (4a8cc6f705) | reference | for Honcho conclusions |
| send-message-unconfirmed | a lost delivery ack is "unconfirmed", not success (4ff757efc4) | reference | `notify_owner` follow-up |
| cron-per-job-tz | per-job timezone, grace, missed-run policy (bfa67878cd …) | reference | reminders phase 6 |
| browser-rewrite | built-in browser tools replaced by `browser_exec` (68500d9ff9 …) | reject | browser tools are on the skip list; computer_task uses no browser toolset |
| compaction-prepared / tail fixes | prepare-ahead compaction and tail-cut fixes (32a636bc8d …, f6a84fd6d9 …) | reject | no compaction in our tool loop |
| desktop/TUI/gateway/catalog | the rest of the 4264 commits | reject | out of scope |

Full survey with commit lists: nezopt `~/scratch/hermes-sync/survey.md`; plugins: `~/scratch/hermes-sync/plugins-report.md`.

## Install and plugins

- `~/.local/share/lucina-hermes-agent` (computer_task through lucina-cua): v0.22.5 bf0203a7 → v0.22.7 b200f5bc
  with the re-generated `lucina-cua/hermes-linux-resolver.patch` (owner GO, decision dc-d117d55414a3).
- hermes-official-plugins e9910ad → 83f0a896: of our lock, agent-importers, blender, bubblewrap (now needs Hermes
  ≥ 0.22.6), camofox, claude-subscription-directsdk-experimental and meshy changed; none is enabled for computer_task
  (only cua-spaces, from trycua, pin 82bb8d31 unchanged).
