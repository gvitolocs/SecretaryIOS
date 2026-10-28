# JEV routing

Jev (crate `lucina_jev`) picks an intent for each turn. Two switches in
`~/.config/lucina-rs/mode.env` decide how far that answer goes:

| Switch | Meaning |
|---|---|
| `LUCINA_JEV_INTENT_MIN` | Below this (default 0.6) the intent is checked with DeepSeek. |
| `LUCINA_JEV_FIRST` | `1` / `true` / `on` / `yes`: Jev routes the turn before the tier-0 rules. |
| `LUCINA_JEV_FIRST_MIN` | JEV-first trusts Jev from this confidence up (default 0.75). |
| `LUCINA_JEV_FIRST_INTENT_MIN` | Per-intent overrides, `cursor_task=0.95,chat=0.7`. |

## JEV-first

When `LUCINA_JEV_FIRST` is on, a turn Jev answers at or above its threshold is routed as that
intent. A deterministic route still wins (`computer_task`, `grocery.*`, `lucina_log_inspect`,
`study_focus_reminders`): those parse their own arguments. `unknown` and a missing Jev answer
fall through. A side-effect intent that no rule agreed with does not run directly (an MCP turn
becomes chat; otherwise the planner).

Every such decision is one journal line on `lucina-rs.service` (the unit runs under the broker,
so filter `-u lucina-rs`, not by process name):

```
INFO lucina_jev::route: jev_first event="jev_first" used=true intent=chat confidence=0.91 tier0=Some("chat") guard=""
INFO lucina_jev::route: jev_first fallback event="jev_first" used=false reason="below_threshold" intent=Some("computer_task") confidence=Some(0.62) tier0=None
```

`used=false` names the reason: `below_threshold`, `deterministic_rule`, `jev_unknown`,
`jev_unavailable`.

## `POST /api/secretary/jev/route`

Reads only. Body `{"text", "recent", "platform", "mcp"}`. The response is tier 0, Jev's intent
and confidence, whether JEV-first is on, and the intent the live router would pick. No turn, no
timeline write, no memory write. The same bearer token as the rest of `/api/secretary`.

## Drift report

`lucina-rs jev drift-report [--days 7] [--labels-dir DIR] [--dry-run] [--recalibrate]` reads the
last N days of those log lines (turns, used vs fallback, reasons, intents, confidence buckets,
agreement with tier 0 where a rule fired) and, when the labelled set is present, replays it
through `/jev/route`.

The set is `cal-in.jsonl`, `labels.txt` (`<line> <intent>[|<intent>…]`; `skip` is left out) and
`extras.jsonl` (rows with their own `label`; an id already in `cal-in` is not replayed twice).
Default directory `/home/nez/Projects/Lucina/data/jev-cal`, or `LUCINA_JEV_CAL_DIR`. The files are
the owner's messages and stay 0600; the report never stores a turn's text. A missing directory
skips the replay and says so. The replay stops on the first error.

Scoring matches the calibration scorer: Jev's accuracy in `[0,0.5) [0.5,0.6) [0.6,0.75) [0.75,0.9) [0.9,1]`,
precision per intent at ≥ 0.75, and what JEV-first would get at 0.6 / 0.7 / 0.75 / 0.8 / 0.9
against the live router. A labelled turn with several intents counts as right on any of them.

Each run writes `<LUCINA_DATA_DIR>/jev-drift/<date>.json` (numbers and intent names only, mode 0600)
and one ops record: kind `status`, project `lucina`, dedup key `jev-drift-<date>`. Drift is an
accuracy drop of at least 5 points in a bucket that has at least 10 turns in both reports, or the
JEV-first used rate moving by at least 15 points. `--dry-run` prints the record and writes nothing.

`--recalibrate` adds a suggestion: for each intent Jev picked at least 10 times, the lowest
threshold on a 0.05 grid that keeps precision ≥ 0.9 (and still has at least 5 answers above it),
as `LUCINA_JEV_FIRST_INTENT_MIN`. It does not edit `mode.env`. That run's dedup key is
`jev-recal-<date>`, so it is not dropped as a duplicate of the weekly record. The timer for it is
one-shot.

Units (not installed here): `deploy/lucina-rs/lucina-jev-drift.timer` (Wednesday 08:00
Europe/Copenhagen) and `lucina-jev-recal.timer` (2026-11-04 09:00 Europe/Copenhagen).
