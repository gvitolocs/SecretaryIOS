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

## Yes/no questions (`crate::jev_yesno`)

The owner's closed questions ("Ho palestra oggi?", "Is milk on my shopping list?", "Har jeg en
forelæsning i morgen?") are answered by Jev on the JSON planner path, after decisions, YouTube,
shares, her computer, the ops record and AU Find, before every other shortcut. His own turns only
(app, Telegram, Discord, Alexa); MCP turns and other senders keep the normal path.

1. **Screen** (no call): a `?`, or an English/Danish yes/no lead word without one. Open questions
   (what, cosa, hvad, …) and Italian without a `?` are skipped.
2. **Gate** (`purpose=yesno_gate`): P(closed yes/no question) ≥ `LUCINA_JEV_YESNO_GATE_MIN` (0.75),
   the kind (personal / general / opinion / live / about_lucina) and medical, legal or financial
   (≥ `LUCINA_JEV_YESNO_SENSITIVE_MIN`, 0.5).
3. **State:** the calendar for the next 7 days, the shopping list, pending reminders, the gym week
   and body weight as numbered facts (sources in parallel, 2.5 s each, a failed source says so),
   then her memory context and the recent turns as background. General knowledge gets no personal data.
4. **Answer** (`purpose=yesno`): P(yes), `grounded` (the facts settle it) and `evidence` (which fact).
   P(yes) ≥ `LUCINA_JEV_YESNO_MIN` (0.75) → yes, ≤ 0.25 → no; a personal or live question also needs
   grounded ≥ `LUCINA_JEV_YESNO_GROUNDED_MIN` (0.6).
5. **Reply:** "Sì / Ja / Yes" or "No / Nej" with the percentage and a confidence word, then one
   sentence of reason written by her text model (thinking off, 90 tokens) from the evidence fact and
   its source only; if the model fails, the fact itself. For general knowledge the model may answer
   DISAGREE, which makes the answer unsure. The bubble's `meta.jev` carries the badge
   (`feature`, `answer`, `p`, `ms`, `totalMs`, `model`); the web shows "Jev · 93% · 412 ms".
6. **Otherwise:** a question about Lucina herself or the conversation ("sei sicura?", "are you
   online?", "can you access Gmail?") goes to her normal path alone; between the bars she says she is
   not sure, then her normal answer; a medical, legal
   or financial question gets a hedge line, then the normal answer; a live question her data does not
   settle, Jev failing (quota, breaker, timeout) or `LUCINA_JEV_YESNO=off` → the normal path alone.

It only answers: the action is `yesno`, with no job, tool or decision. Each turn logs one
`event="jev_yesno"` line; the drift report counts them (answered / unsure / hedged, kinds, why the
normal path answered alone, gate buckets, mean Jev latency) and flags a move of ≥ 15 points in the
share Jev answered itself. `POST /api/secretary/jev/yesno {"text", "recent"?, "lang"?, "gateOnly"?}`
replays a text (no turn), also while `LUCINA_JEV_YESNO=off`; `gateOnly` stops after the gate (for
old questions about "today").
