# Samples of what the real `lane` prints

These files are real output of the installed `lane`, version LB-V001 (lane 8166366d04),
captured on 2026-11-17 with free text replaced by `<text removed>`:

- `ls.jsonl`: `lane ls --json`, JSON Lines (one object per line), one run per state.
- `status-<state>.json`: `lane status <id>` for a run in that state (one pretty-printed object).

The work-jobs code of `lucina-agents` (the pure rules) and of `lucina-agentd` (the daemon
tests) read them with `include_str!`.

If the output of `lane` changes, capture new samples from the real tool. Never edit these
files by hand: they are the only record of what `lane` really prints.

`status-queued_lost.json` (a queued run whose runner is gone) and `ls-with-ids.jsonl` (rows of
`lane ls --json` that carry `lead`, `run` and `change`) were captured on 2026-11-17 14:11Z from
lane LB-V005 (main d9d92ec193). No real `ended_empty` sample exists yet.

`ended_empty` (lane LB-V003, 2026-11-17 13:00Z) has no captured sample yet because no real
run had ended in that state when the samples were taken; the tests build its status from
the `reported_done` sample. Capture a real one when it exists.
