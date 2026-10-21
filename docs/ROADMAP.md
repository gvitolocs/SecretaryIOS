# Secretary roadmap

Secretary grows one module at a time. Every module follows the same rule:
**all logic in Rust, thin shells.** A module is a Rust engine with the gym's
command protocol (`{op_id, expected_revision, timezone, command}` in,
`{ok, replayed, code?, error?, view}` out; op_id receipts, revision
conflicts, atomic 0600 files, integer units). The engine runs on the phone
through `secretary-ffi` (`secretary_<module>_open / _request / _close`) and
on `secretary-server` behind `GET /v1/<module>/state` and
`POST /v1/<module>/command`. The SolidJS web app and the SwiftUI app only
render views and send commands. Contract: `docs/server-api.md`.

| Module | Status | Engine | Shells / integration |
|---|---|---|---|
| Shopping | ✓ done | `secretary-core::shopping` (op-log sync, Verus + Quint/TLC models) | web and iOS lists via Lucina `/api/secretary/shopping` |
| Gym | ✓ engine, server, web tab | `secretary-gym` (from Lucina Gym; Verus kernel + lifecycle) | web Gym tab; SwiftUI port of the Lucina Gym app (in progress) on the same FFI functions |
| Body weight | ✓ engine, server, web tab | `secretary-body` (7-day average, trends, goal; Verus mean bound + proptest) | web Weight tab; iOS next to the gym screens |
| Calendar | next | `secretary-calendar`: events, recurrence (RRULE subset), reminders linked to Lucina's reminder jobs, timezone-correct day views | agenda tab replaces the plain list; Lucina reads it through `/v1/insights` |
| Languages | planned | `secretary-languages`: spaced repetition (SM-2 or FSRS, integer intervals and ease), decks, review log | review screen in both shells; **conversation practice with Lucina** through `/v1/chat` sessions seeded with due words, results written back as review commands |
| Memory exercises | planned | `secretary-memory`: exercise generators (n-back, sequences, method of loci prompts) with deterministic seeds and scored sessions | timed exercise screens; progress charts from the view |
| Purchases | planned | `secretary-purchases`: wish/shopping intents, quotes, order states; **never buys on its own** | Lucina proposes through Hermes plugins; every purchase needs an explicit confirmation by the owner in the app (a signed, single-use confirm command with the exact item, price and payee) before any plugin may act |

## Cross-cutting work

- **Shared engine skeleton.** Receipts, revision checks and atomic
  persistence are duplicated in `secretary-gym` and `secretary-body`. When a
  third module needs them, extract them into a small `secretary-engine`
  crate (a trait for `apply` + `validate` + `view`) without changing either
  wire protocol, so existing receipts keep their fingerprints.
- **Lucina insights.** `GET /v1/insights/summary` (service token) grows a
  section per module; wiring it into Lucina's prompts is a Lucina-side task.
- **Receipt retention.** Engines refuse writes after 100 000 receipts. Add a
  pruning command (receipts older than the longest offline window, e.g.
  90 days) before a heavy user gets there.
- **Gym body-weight migration.** The gym's old `record_measurement
  kind=body_weight` entries stay readable; a one-time import into
  `secretary-body` can follow if old data matters.
- **Avatar.** The 3D Lucina avatar uses `/v1/assets/{name}` (private files)
  and `/v1/tts` (Kokoro); lip-sync timestamps (`/dev/captioned_speech`) are a
  later addition.
