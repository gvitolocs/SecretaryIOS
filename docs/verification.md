# Formal verification

The multi-process protocols of Secretary / Lucina are checked three ways:

- **Stateright models in Rust** (`verification/models`, a workspace crate). They are exhaustive BFS model checks of
  the protocol with the real processes as actors, run as `cargo test` and as the `verify-models` table.
- **TLA+ specs checked by TLC** (`formal/<protocol>/`). Each is the twin of a Stateright model, with the same
  actions and invariants. The older lucina-bring sync specs (`formal/OriginalSync.tla`, `formal/MigratedSync.tla`)
  run in the same gate.
- **Verus proofs** (`verification/verus/*.rs`) of the pure kernels: the decision functions behind a protocol,
  proved for all inputs rather than within the models' bounds.

`scripts/verify.sh` runs all three. It exits non-zero on any failure and ends with a summary table.

## Policy

1. **Every protocol change updates its model, in the same change.** That covers a new writer of a shared file, a
   new lock, a change to an ops/409 rule, a reorder of a cutover step, or a new confirmation path. Update the
   Stateright module, its TLA+ twin and the mapping table below. If the change touches a Verus kernel, update that
   kernel too.
2. **The gates include `scripts/verify.sh`.** Run it before merging into `main` and before any deploy of the
   runtimes it covers (secretary-server, secretary-mcp, lucina-rs, lucina-proactive, the web app).
3. **Known bugs stay visible as witnesses.** A model case that reproduces a known bug expects `Violated` (a TLA+
   cfg says `MUST FAIL` on its first line). When the fix lands in the code, flip the case's switch so it models
   the fixed code. The expectation then becomes `Holds` (`MUST PASS`). A witness that stops failing makes the gate
   fail, because either the model got too weak or the bug was fixed without updating the model.
4. **No vacuous passes.** Every passing configuration has a `sometimes` property (Stateright) or a `*_witness.cfg`
   (TLC) proving that it reaches the interesting states: a rotation, a resume, a 409, a handover, a refresh.
5. Generated TLC output (`*_TTrace_*`, `states/`) is never committed (`formal/.gitignore`). `verify.sh` runs TLC
   with `-noGenerateSpecTE` and a temporary `-metadir`.

## How to run

```sh
scripts/verify.sh                 # everything, default bounds (about 3 min on nezopt)
scripts/verify.sh --only models   # or: tlc | verus
scripts/verify.sh --deep          # exhaustive Stateright bounds + formal/**/*_deep*.cfg (slow)

cargo test -p verification-models                         # the Stateright gate alone (about 20 s)
cargo run -p verification-models --bin verify-models -- --traces            # table + shortest counterexamples
cargo run -p verification-models --bin verify-models -- --only T1 --states  # one case, with its last state
```

Tools (nezopt): Java `~/tools/jdk-21.0.12.1+1/bin/java`, `~/tools/tla/tla2tools.jar`, Verus
`~/tools/verus/verus-x86-linux/verus`. Override with `JAVA`, `TLA_JAR`, `VERUS_BIN` and `VERUS_DIR`. Stateright is
pinned to `=0.31.0` (2025-07-27). The traces quoted below are the shortest ones: `verify-models` runs a second plain
BFS for each counterexample, because Stateright reports the last violating path it found, not the shortest.

## Inventory

| # | Protocol | Stateright (`verification/models/src/`) | TLA+ (`formal/`) | Verus kernel | Code |
|---|---|---|---|---|---|
| 1 | Shared `timeline.jsonl` writers + SSE readers | `timeline.rs` (T1–T7) | `timeline/Timeline.tla` | `protocol_kernels.rs` K1, K2; `timeline_ids.rs` | Node `Lucina/src/timeline.js`; Rust `lucina-store/src/timeline.rs`, `lucina-server/src/api.rs::timeline_stream` (branch `lucina-rs`); proactive `lucina-proactive/src/channels.rs` (branch `lucina-proactive`); web `web/src/api/timeline.ts` |
| 2 | Phase-1 cutover/rollback, single-writer ownership, phase-2 scheduler, phase-3/4 channels | `ownership.rs` (`Cutover`, `Scheduler`, `Channels`) | `ownership/Cutover.tla`, `SchedulerHandover.tla`, `ChannelHandover.tla` | – | `deploy/lucina-rs/{cutover,rollback}.sh`, `*.env`, `lucina-store/src/{jobs,reminders,scheduler}.rs` (branch `lucina-rs`); Node `secretary-api.js` jobs, `reminders.js`; `docs/lucina-rust/PORT_PLAN.md` §5–§6 |
| 3 | Shopping v2 ops + client outbox | `shopping.rs` (S1–S3) | `shopping/ShoppingOps.tla` (+ `MigratedSync.tla`) | `protocol_kernels.rs` K3; `shopping_ops.rs`; `sync_kernel.rs` | Node `Lucina/src/grocery/shopping.js::applyShoppingOps`; Rust `lucina-grocery/src/lib.rs` (branch `lucina-rs`); `crates/secretary-server/src/shopping.rs`; web `web/src/state/shopping.ts`, `lib/shoppingOps.ts` (item objects: branch `shopping-items`) |
| 4 | Lucina MCP OAuth 2.1 + PKCE + refresh rotation | `oauth.rs` | `oauth/OAuth.tla` | `protocol_kernels.rs` K4 | `crates/secretary-mcp/src/oauth.rs` |
| 5 | Confirmation and approval gates | `gates.rs` (`Pending`, `Attention`, `Meshy`) | `gates/PendingActions.tla`, `AttentionBudget.tla`, `MeshyCredits.tla` | `protocol_kernels.rs` K5, K6; `confirmation_gate.rs`; `attention_engine.rs` | `lucina-tools/src/{pending,agent}.rs`, `lucina-server/src/chat.rs::tools_turn` (branch `lucina-rs`); `lucina-proactive/src/{attention,engine}.rs`; `lucina-meshy/src/{budget,service}.rs`, `lucina-tools/src/tools/meshy.rs` (uncommitted in `SecretaryIOS-meshy`) |
| 6 | Lucina Ink device sync (outbox, `POST /v1/library/sync`) | `ink_sync.rs` (I1–I7) | `inksync/InkSync.tla` | `library_merge.rs` | `crates/ink-sync` (`outbox.rs`, `client.rs`, `state.rs`, `transport.rs`); `crates/secretary-server/src/library.rs::sync`; `crates/ink-library/src/import.rs::merge` |
| 7 | Grocery deal watch (digest, alerts, Node → Rust cutover) | `deals.rs` (D0–D5) | `deals/DealWatch.tla` | `deals_kernel.rs` | `lucina-deals/src/{watch,schedule,notify,engine,sources,matcher,validity}.rs`, `lucina-server/src/deals.rs` (branch `lucina-rs`, pinned in `verification/verus/diff/deals-revision`); Node `Lucina/src/grocery/deals/watch.js` (`68b38dc~1`); `crates/secretary-server/src/notify/{mod,store,routes}.rs` |

Other Verus files in `verification/verus/` include `body_kernel.rs`, `gym_kernel.rs`, `gym_lifecycle.rs` and
`sync_kernel.rs`. Branch `verus-proofs` adds `avatar_validation.rs`, `notes_kernel.rs`, `secrets_redactor.rs` and the
files named in the table. `verify.sh` runs every top-level `verification/verus/*.rs`. Subdirectories such as
`verification/verus/diff/` hold plain-Rust differential tests and are not Verus input.

### States explored (default bounds, `verify-models`)

| Case | States | Depth | Result |
|---|---:|---:|---|
| T1 late_id_resume (current code) | 19 435 | 17 | `reader_never_skips` **violated** |
| T1 + fix A (arrival-order cursor) | 23 402 | 16 | holds |
| T2 rotation_race (current code) | 9 053 | 15 | `index_complete`, `reader_never_skips` **violated** |
| T2 + fix B (watch `.1` tail) | 9 465 | 15 | holds |
| T3 docker_cat_split (proactive sink) | 82 156 | 16 | `lines_whole`, `rotation_keeps_lines` **violated** |
| T4 two_rotators (Node limit not raised) | 13 680 | 16 | `rotation_keeps_lines` **violated** |
| T5 fresh_reconnect_gap (web EventSource) | 474 | 11 | `reader_never_skips` **violated** |
| T5 + fix D (resync) | 398 | 11 | holds |
| T6 all writers + fixes A, B, D | 2 441 449 | 21 | all hold |
| T7 all writers + fixes C, D (single writer) | 6 594 | 22 | all hold |
| P1 cutover/rollback, shared jobs file (current) | 51 252 | 22 | `no_false_interrupted`, `no_lost_job_update` **violated**; routing, rollback, channel owners hold |
| P1 + fix (own jobs file, owner + boot) | 13 073 | 21 | all hold |
| P2 scheduler handover as written | 963 | 23 | `single_reminders_writer`, `no_lost_reminder_record`, `no_double_delivery` **violated** |
| P2 fixed order, batch commit (today's tick) | 326 | 17 | `no_double_delivery` **violated** |
| P2 fixed order + claim-before-send + drain | 337 | 17 | all hold |
| P2 rollback after the fixed handover | 515 | 19 | all hold |
| P3/P4 handover + rollback as documented | 50 | 9 | all hold |
| P4 Discord started before Node stopped | 20 | 5 | `single_discord_gateway` **violated** (rule witness) |
| P3 gateway lock disabled | 22 | 6 | `single_telegram_poller` **violated** (rule witness) |
| S1 lost ack then 409 rebase (current) | 623 | 19 | `no_duplicate_application`, `acked_applied_exactly_once` **violated** |
| S1 + fix (duplicates in the 409 body) | 617 | 19 | all hold |
| S2 op on an unacked optimistic row (current) | 432 | 16 | `no_silent_drop` **violated** |
| S2 + fix (server keeps `rowId`, merge aliases) | 459 | 16 | all hold |
| S3 two clients, same object, both fixes | 34 634 | 31 | all hold |
| O current main | 1 595 | 21 | `code_replay_revokes_grant`, `refresh_reuse_revokes_grant` **violated** |
| O + fixes O1, O2 (branch `verify-fixes`) | 1 195 | 20 | all hold |
| G pending actions, owner turns only | 1 442 | 14 | `confirmed_after_shown` **violated** |
| G pending actions + MCP turns (current) | 6 880 | 14 | `only_owner_yes_executes` **violated** |
| G pending actions + fixes G1, G2 | 2 150 | 13 | all hold |
| G attention budget, daemon + `once` (current) | 1 172 | 13 | `daily_budget`, `cooldown` **violated** |
| G attention budget + fix G3 (tick lock) | 548 | 12 | all hold |
| G Meshy credits (WIP) | 232 | 10 | `charged_within_daily_cap` **violated** |
| G Meshy + MCP yes (G1 open) | 463 | 10 | `only_owner_confirms` **violated** |
| G Meshy + fix G4 (per-step guard) | 115 | 10 | all hold |
| I1 ink sync: progress/status races, owner edit | 2 949 071 | 30 | all hold (+ `all_ops_applied` eventually) |
| I2 ink sync: highlights and a deletion request | 501 633 | 29 | all hold |
| I3 ablation: no op-id gate | 20 617 | 24 | `applied_at_most_once` **violated** |
| I4 ablation: acks in memory only | 30 891 | 26 | `never_resend_acked` **violated** |
| I5 ablation: ops leave the outbox when sent | 8 731 | 20 | `acked_never_lost`, `all_ops_applied` **violated** |
| I6 ablation: plain last-writer-wins status | 9 034 | 21 | `owner_completed_never_reverted`, `device_completed_sticky` **violated** |
| I7 ablation: the last delivery wins (no stamps) | 9 418 | 21 | `converges` **violated** |
| D0 current Rust, no faults | 138 980 | 29 | `no_repeat` (DW2), `no_skip` (DW3) **violated**; the others hold |
| D1 current Rust after the cutover, one fault | 1 172 708 | 29 | `no_repeat`, `one_digest_per_day`, `silent_when_unchanged` (DW1), `no_skip` **violated** |
| D1 + fixes F1, F2, F3 | 1 149 598 | 33 | all hold |
| D2 cutover as performed (stop Node, start Rust) | 1 035 974 | 31 | the same four **violated** (DW1 through the stop, DW2, DW3) |
| D2 + fixes in both runtimes | 958 309 | 35 | all hold |
| D3 Node and Rust both running (rule witness) | 1 969 101 | 29 | the four **violated** |
| D4 current, no server dedup | 1 215 167 | 29 | the four **violated** |
| D5 fixes, no server dedup | 1 188 512 | 33 | `no_repeat`, `one_digest_per_day`, `silent_when_unchanged` **violated** |

TLC, default cfgs: from 20 to 82 317 distinct states per cfg (`Timeline_fixed`: 82 317, `Cutover_fixed`: 13 605,
`ShoppingOps_fixed`: 2 686, `PendingActions_fixed`: 2 457, `OAuth_fixed`: 2 214). `Timeline_fixed_deep.cfg`
(`--deep`): 1 053 488. `InkSync_status`: 35 800 and `InkSync_highlights`: 17 556 (both with the liveness
property `EventuallyAllApplied` under weak fairness); `InkSync_highlights_deep` (`--deep`, the Stateright I2
bounds): 501 634, the same space as I2 (about 5 min); `InkSync_status_deep` (the I1 bounds, with liveness):
1 715 162 (about 20 min; TLC's state differs from Stateright's in ghost variables, hence the different count).

`formal/deals/`: `DealWatch_fixed` 1 149 598, `DealWatch_cutover_fixed` 958 309 and `DealWatch_current_nofaults` 138 980
distinct states (the same counts as Stateright D1 + fixes, D2 + fixes and D0), each under 10 s; the `MUST FAIL` cfgs stop
at their first violation.

## Invariants, models and mapping

### 1. Shared timeline writers

Invariants (Stateright name / TLA+ name):

- `lines_whole` / `LinesWhole`: every complete line in a retained file parses, so there are no torn or interleaved
  lines.
- `ids_unique` / `IdsUnique`: no two lines share an id.
- `rotation_keeps_lines` / `RotationKeepsLines`: every completed write is in a retained file, and no rotation moves
  an empty file (which would drop a generation early).
- `index_complete` / `IndexComplete`: once quiet, the Rust index holds every line.
- `reader_never_skips` / `ReaderNeverSkips`: once quiet, the app has every line it was connected for, through
  replay, live delivery or resync.

| Model action | Code |
|---|---|
| `NodeGen` | `timeline.js::normalizeTimelineItem` → `newTimelineId(Date.now())`, before `withLock` |
| `NodeRefreshOpen` / `NodeOpen` | `append` → `withLock` → `refresh()`, then `writeItems` (rotation check against `LUCINA_TIMELINE_MAX_BYTES`), then the `fs.appendFile` open |
| `NodeWrite` | `fs.appendFile` (one O_APPEND write) |
| `ProGen` | `channels.rs` deliver: `lucina_store::ulid::new_id(m.now)` (decision time) |
| `ProOpen`, `ProWrite`, `ProWriteTail` | `DockerSink::append`: `docker exec -i <c> sh -c 'cat >> "$0"'`; `cat` writes what each pipe read returns |
| `RustBegin` | `TimelineStore::append`: `normalize(now_ms)`, `lock_sidecar` (flock), `refresh`, `successor(last.id)`, rotation check in `write_lines` |
| `RustRotate` | `Inner::rotate` (renames `.1→.2`, `cur→.1`) |
| `RustWrite` | `Inner::write_lines` (`open_append` + `write_all`), then `refresh` + `publish` |
| `RustPoll` | `TimelineStore::poll` |
| `RustCrash` / `RustStart` | process restart; the index is rebuilt by `load_all` |
| `SseConnect` | `api.rs::timeline_stream`: `poll`, `subscribe`, `after(Last-Event-ID)` replay, `replay_end` |
| `SseDeliver` | the `live.recv()` arm and the `seen` / `it.id <= replay_end` filter |
| `SseFollow` | the 1 s follow tick: `poll` + `after(&cursor)` |
| `SseDisconnect` | network drop; the browser `EventSource` reconnects (`web/src/api/timeline.ts::TimelineStream`) |
| `Settle` | quiescence: writers done, one last follow tick |

### 2. Ownership: cutover, rollback, handovers

Invariants:

- `routes_to_live_runtime`: secretary-server never points at a Rust instance in shadow mode.
- `rollback_restores_node`: after `rollback.sh`, p1 points at Node and Rust is back in shadow.
- `single_channel_and_scheduler_owner`: in phase 1 only Node owns Telegram, Discord and the scheduler. This
  follows from `shadow.env`/`cutover.env`, the build without channels, and the `writer=node` scheduler guard.
- `no_false_interrupted`: a job is never marked `interrupted` while its runtime still runs it.
- `no_lost_job_update`: no read-modify-write drops a job record or rolls its status back.
- Phase 2: `single_scheduler`, `single_reminders_writer` (no two read-modify-writes of `reminders.json` in flight
  at once), `no_lost_reminder_record`, `no_double_delivery`, `no_silent_loss` (every reminder is delivered or
  visibly failed).
- Phases 3/4: `single_telegram_poller`, `single_discord_gateway`.

| Model action | Code |
|---|---|
| `OperatorCutover`, `ScriptStep` | `cutover.sh`: preflight; chown + `cutover.env` + restart `lucina-rs`; p1 `SECRETARY_LUCINA_URL` + restart |
| `OperatorRollback` | `rollback.sh`: p1 first, then `shadow.env` + restart |
| `ScriptFails` | `set -euo pipefail` (an ssh step fails) |
| `RustCrashRestart` | `Restart=on-failure`, `lucina-rs-health.timer` |
| `AppStartJob` | job actions: Node `insertJob` (`secretary-api.js`), Rust `JobStore::insert` |
| `RuntimeFinish` | Node `updateJob`, Rust `JobStore::update` + `finish` |
| `AppPoll` | `GET …/jobs/:id`: Node `findJob`, Rust `JobStore::find` (marks a running job it is not working on as `interrupted`) |
| `RmwWrite` | Node `writeJobs` (tmp + rename), Rust `write_atomic`; in-process locks only |
| Scheduler `Operator` | PORT_PLAN §5 phase 2 steps (restart with new env) |
| `CreateViaTelegram` / `CreateViaApp` | Node `createReminder`, Node shim → Rust `/api/internal/reminders`, Rust `reminder_writer.rs` (forward when `LUCINA_REMINDERS_WRITER=node`) |
| `TickRead`, `TickSend`, `TickWrite` | Node `processDueReminders`, Rust `ReminderStore::process_due` / `scheduler::tick` (send all, write at the end) |
| `Crash` | SIGTERM/SIGKILL mid-tick (Node `shutdown()` exits without waiting for the tick) |
| Channels `Operator`, `Kill`, `LockExpires`, `Acquire` | PORT_PLAN §5 phases 3/4; gateway-lock record (heartbeat 30 s, live 90 s); `releaseGatewayLocks` on SIGTERM |

### 3. Shopping v2 ops + outbox

Invariants:

- `no_duplicate_application`: every user intent changes the server at most once, across rebases.
- `acked_applied_exactly_once`.
- `no_silent_drop`: no intent is dropped from an outbox as `rejected` without having been applied.
- `deleted_stays_deleted`.
- `one_active_row_per_object`.
- `clients_converge` / `RevisionIdentifiesState`: the same revision means the same rows.

| Model action | Code |
|---|---|
| `User` | `shopping.ts::enqueue` (new `opId`; optimistic `rowId` from `addOpFor`) |
| `Send` | `flushOutbox` → `postOps(listId, base.revision, every queued op of the list)` |
| `Serve` | `applyShoppingOps`: receipts dedup, `findConflicts` (409 when stale and conflicting, nothing recorded), `applyOne` (merge into the object's active row; removed stays removed; unknown row → rejected receipt), revision + 1 |
| `Receive` 200 | drop every sent op, `remapIds`, `storeServerState` |
| `Receive` 409 | refetch, `rebaseOps` (new op ids, drop no-ops), `MAX_CONFLICT_RETRIES`; secretary-server `shopping.rs::ops` maps the 409 |
| `Lose` | `isTransient` → `scheduleRetry` (same op ids) |

### 4. OAuth

Invariants:

- `code_used_once`.
- `no_token_without_login_and_consent`.
- `verifier_bound`: no tokens without the PKCE verifier.
- `code_replay_revokes_grant`: a code presented twice leaves no live grant.
- `refresh_reuse_revokes_grant`.

The redirect_uri binding needs no separate invariant here. The code goes only to the request's registered
`redirect_uri` (`authorize_get`); a token request naming another `redirect_uri` fails; and PKCE binds the code to
the verifier holder. A token request without `redirect_uri` is accepted, as OAuth 2.1 allows when PKCE is used.

| Model action | Code |
|---|---|
| `OwnerLogin`, `OwnerApprove` | `authorize_get` (pending request), `authorize_post` (`check_owner`, consent, code) |
| `CodeLeaks`, `RefreshLeaks` | attacker capability |
| `Redeem` + `Step` (code 1–4) | `token()` authorization_code: lookup under `codes` (redeemed → replay → `revoke_grant`), client/redirect/PKCE/resource checks, set `redeemed`, `issue_tokens`. Fixed: `OAuth::redeem_code` |
| `UseRefresh` + `Step` (refresh 1–2) | `token()` refresh_token: `file.read` (used → replay → revoke), `file.update` mark used, `issue_tokens`. Fixed: `OAuth::rotate_refresh` |

### 5. Gates

Invariants:

- Pending actions: `never_in_the_requesting_turn`, `only_owner_yes_executes`, `expired_never_executes` (10 min),
  `at_most_once`, `confirmed_after_shown`.
- Attention: `daily_budget` (≤ `max_per_day` unsolicited messages; the Pulse is separate), `quiet_hours`,
  `cooldown`, `never_external_actions` (the outreach action type holds only a route or feedback).
- Meshy: `charged_within_daily_cap`, `no_spend_without_yes`, `no_spend_without_reservation`,
  `only_owner_confirms`.

| Model action | Code |
|---|---|
| `Message` | `handle_chat` → `run_turn` (`LUCINA_PLANNER=tools` → `tools_turn`) |
| `TurnStart` | `tools_turn`: `st.pending.current`, `classify_reply`, `take` + `agent::execute`; agent loop `handle_call` → `pending.create` |
| `TurnEnd` | the reply returned to the client |
| `Tick` | `PendingAction::expired` |
| `Gate` | `engine.rs::tick`: `self.ledger(kind, now)` + `attention::gate` per candidate |
| `Send` | `self.send` (`channels.rs` delivery + `record_outreach`) |
| `Pulse` | `pulse.rs` |
| `Confirm` | a confirmed pending action (`ctx.confirmed`, `tools/meshy.rs`) |
| `Reserve` | `Service::start` → `Ledger::reserve` |
| `StepRuns` | `Service::run` step loop (`consumed_credits`) |
| `Settle` | `Ledger::settle` |

### 6. Lucina Ink device sync

Local-first sync of reader devices with the Library (`crates/ink-sync`, `docs/server-api.md` "Library",
`POST /v1/library/sync`). Each device appends ops to a durable outbox (JSONL + fsync) before any network I/O and
appends acks the same way; the server applies ops through a per-device watermark (largest contiguous counter plus a
bounded sparse set) and joins them into registers: progress last-writer-wins by `(at, device, counter)`, status by
`(completed, stamp)` with an owner freeze and owner floors, highlights a grow-only set, deletions unconfirmed
tombstones, sessions min/max registers.

Invariants (Stateright name / TLA+ name):

- `acked_never_lost` / `AckedNeverLost`: an op the device dropped from its outbox as acked is applied on the server.
- `never_resend_acked` / `NeverResendAcked`: after a durable ack (and any crash), the op is never sent again.
- `applied_at_most_once` / `AppliedAtMostOnce`: duplicate deliveries never apply an op's effect twice.
- `owner_completed_never_reverted` / `OwnerCompletedNeverReverted`: after the owner marks the book completed, no
  device op changes the status.
- `device_completed_sticky` / `DeviceCompletedSticky`: device ops never move a book back from completed.
- `highlights_only_grow_and_deletes_are_recorded` / `HighlightsOnlyGrow`.
- `converges` / `Converges`: the server's content equals what a server applying the same ops in another order shows
  (Stateright: two replays, ascending and descending; TLA+: the registers are the max / union over the applied set).
- `all_ops_applied` (eventually) / `EventuallyAllApplied` (`<>`, weak fairness): every recorded op reaches the
  server. Witnesses: `all_synced` / `InkSync_witness.cfg`, `out_of_order_applied` / `InkSync_ooo_witness.cfg`,
  `older_op_superseded`, `owner_protection_used`.

The model client may pipeline two single-op requests, a superset of the real client (one batch at a time, oldest
first), so out-of-order arrival and the sparse set are explored. Ablations I3–I7 (and `InkSync_nogate`,
`_volatileacks`, `_plainstatus`) switch off one mechanism each and must fail: they show the properties are not
vacuous. The real code is also checked directly by `cargo test -p ink-sync` (in the models section of `verify.sh`):
512 proptest cases of random ops from three devices applied in a random permutation with duplicates (same content,
replays are no-ops, highlights only grow) and 16 seeded runs of two devices with crashes, torn appends, dropped
requests and responses, duplicate and delayed (reordered) deliveries and partitions, which must end with empty
outboxes, every op applied once and the same content as an in-order replay.

Verus (`library_merge.rs`, 25 verified): (a) `step` is idempotent per op id and the raw effect is idempotent;
(b) highlights and tombstones only grow over any op sequence, a delete removes nothing; (c) the stamp order is a
strict total order, distinct op ids never tie, `step` of two ops with distinct ids commutes (progress, status,
highlights, deletes), including with a replayed op; the owner freeze holds over any sequence and device `completed`
is sticky; (d) the import merge (`ink_library::import::merge`: files, clippings, the `kindle-usb` presence pass)
never deletes a book or a highlight and never changes a reading status; (e) watermark insert/absorb membership.
The model is one book's registers (ops on different books touch disjoint registers) with device ids as naturals.

| Model action | Code |
|---|---|
| `Record` | `Outbox::record` (append + `sync_data`), `Syncer::record` |
| `Send` | `ClientMachine::next` → `Syncer::step` (the batch from `Outbox::pending`) |
| `Timeout` | `TransportError::Network` → `ClientMachine::on_failure` (same op ids; backoff `Backoff::delay`, equal jitter, capped) |
| `Crash` | process restart: `Outbox::open` (torn last line truncated, acks replayed), a fresh `ClientMachine` |
| `Deliver(Req)` | `transport::serve` → `state::apply_mut` (watermark gate, then `effect`); the server endpoint `LibraryStore::sync` writes `sync.json` before the records |
| `Deliver(Resp)` | `Outbox::ack` (append + `sync_data`), `ClientMachine::on_response` |
| `Drop`, `Dup` | `FlakyTransport` (`Faults`) |
| `Owner` | `PATCH /v1/library/books/{id}` → `state::owner_set_status` / `owner_set_progress` |

### 7. Grocery deal watch

The deal watch of the Rust Lucina (`crates/lucina-deals`, branch `lucina-rs`, pinned at
`verification/verus/diff/deals-revision`; Node original `Lucina/src/grocery/deals/watch.js` at `68b38dc~1`, the same
logic line for line): a digest at 07:30 owner time (catch-up until 11:00, once per local date via `lastDigestDate`),
alerts every 90 minutes inside 09:00–20:00 (at most four a day), one state file `data/grocery/deal-watch.json` for
both runtimes. Each run loads the file, polls Salling food waste and Tjek flyers (either may fail), composes,
sends through secretary-server `POST /v1/notify` with `dedupKey` `grocery-deals:<digest|alert>:<date>:<hash of the
delivered keys and prices>`, and writes the file only after a successful send. The cutover (2026-10-25) stopped Node
(container restart without the deals code) before the Rust watcher started.

**The notify server's dedup, as it is** (`crates/secretary-server/src/notify/mod.rs::Notifier::admit`,
`store.rs`, `routes.rs::notify`): under `admit_lock`, a key is a duplicate when it is in `seen` (an in-memory LRU of
4096 keys, empty after a server restart) or on any notification in the inbox (`notifications.json`, at most 500
items, read ones evicted first; there is no delete API). A duplicate is answered 200 `{"status":"duplicate"}`, which
`AppNotify` reports as `Delivered`, so the watcher acknowledges it. There is no time limit: a key is suppressed until
it leaves both stores, which a few watch messages cannot cause, so the model's server keeps every key. The key holds
the run's local date and only the delivered best offers (`dedup_key_is_kind_date_and_delivered_offers` checks this on
the real `compose`): an identical retry on the same date is suppressed; a retry with one more offer, a changed price
or on the next date is a new notification.

Model (`deals.rs`, twin `deals/DealWatch.tla`; the TLC state counts equal Stateright's): two items; item 0 has a
food-waste offer and a flyer alternative, item 1 a flyer offer; prices none/low/high; two edits of the sources; two
days of three clock slots (digest window, catch-up + first alert tick, later alert ticks); at most four run starts;
one fault (a crash at any step, a lost response after the server admitted the message, the server down, or a failed
source in a poll). The clock and the sources move between runs: a run takes seconds and reads the sources only at
`Poll`. Switches: Node first and the cutover, Node and Rust at once, server dedup, and three proposed fixes:

- **F1** write the composed message (`pending`, with its dedup key) into the state file before sending; a run that
  finds one re-sends it with the same key (the server answers `duplicate` if it already has it), acknowledges it and
  ends.
- **F2** acknowledge only the delivered best offers (and their `relatedKeys`), not every active offer.
- **F3** a notified price never goes up (`set_notified` keeps the lower price).

Invariants (Stateright / TLA+), checked over the owner's receipts (ghost state), not over the file:

- `no_repeat` / `NoRepeat`: an alert never carries an (item, key, price) the owner already received in any message.
  The digest is a daily summary and may list received offers again (`digest_relists` is a reached witness of that
  design); two digests on one date are `one_digest_per_day`.
- `one_digest_per_day` / `OneDigestPerDay`: at most one admitted digest per digest date.
- `silent_when_unchanged` / `SilentWhenUnchanged`: no digest is admitted whose run observed the same active offers
  and prices as the last admitted digest's run, and no alert whose run observed the same as the last admitted
  message's run. "Observed": a failed source shows fewer offers.
- `no_skip` / `NoSkip`, safety over the history: when an alert run ends successfully (no source failed, and it either
  skipped or delivered and saved), every item whose best offer has been the same since before the previous alert
  tick, and which the run saw, has reached the owner at that price or lower. A capped run never starts.
- `acked_only_after_admission` / `AckedOnlyAfterAdmission`: the file records a message only after the server has its
  key (a failed send writes nothing). It holds in every case.

Witnesses: `new_alert` / `NeverNewAlert` (an offer added or repriced by an edit reaches the owner in an alert),
`restart_continues` / `NeverRestartContinues` (after a send whose save was lost, a later run saves), `cutover` /
`NeverCutover` (Rust loads the file after Node stopped; in D3 both load), `digest_relists` / `NeverDigestRelists`.
TLC: one `MUST PASS` cfg per holding case with all its invariants, `_witness` cfgs for it, and one `MUST FAIL` cfg
per bug, each with the single invariant it breaks (16 cfgs).

| Model action | Code (`crates/lucina-deals/src/` on `lucina-rs` unless noted) |
|---|---|
| `Tick` | `schedule.rs::Scheduler::tick`: digest from `GROCERY_DEALS_DIGEST_TIME` until 11:00, alerts in `GROCERY_DEALS_ALERT_WINDOW` every `GROCERY_DEALS_ALERT_INTERVAL_MIN` |
| `Edit(offer, price)` | Salling / Tjek upstream changes (source caches: food waste 15 min, search 30 min, shorter than the alert interval) |
| `Load(actor, kind)` | `watch.rs::DealWatch::run`: queue mutex, `time`/`date`, `WatchState::load`, `prune`, `already digested today`, `daily alert cap`; F1: a pending message |
| `Poll(actor, mask)` | `engine.rs::list_matches` → `collect` (`sources.rs::food_waste`, `flyers`; a failure gives no deals of that source), `validity`, `match_names` (`matcher.rs::match_deal`, `group_deals`) |
| `Decide(actor)` | `watch.rs::compose`: active offers, `digest_fingerprint`, `no active matches`, `unchanged digest`, best offer per item, the alert `notified_price <= price` filter, `dedup_key` |
| `Claim(actor)` | F1 only (proposed): save the composed message as `pending` |
| `Send(actor, outcome)` | `notify.rs::AppNotify::deliver` (one retry on network error, timeout or 5xx) → secretary-server `notify/routes.rs::notify` → `Notifier::publish`/`admit`; `Lost` = admitted but the response failed (`Delivery::Failed`), `Down` = not admitted |
| `Save(actor)` | `watch.rs::acknowledge` + `WatchState::save` (tmp file, rename) |
| `SkipSave(actor)` | `run`: a skipped digest sets `lastDigestDate` and saves |
| `Crash(actor)` | the process dies anywhere; the queue and the digest cooldown are lost, the file and the inbox stay |
| `StopNode`, `StartRust` | the cutover: Node's container restarted without the deals code (an in-flight run dies), then `lucina-server/src/deals.rs::start` |

| Case | Node / Rust | Faults | Dedup | Fixes |
|---|---|---|---|---|
| D0 | Rust | none | on | – |
| D1 | Rust | one | on | – |
| D1 + fixes | Rust | one | on | F1 F2 F3 |
| D2 | Node, `StopNode` at any step, `StartRust` | the stop | on | – |
| D2 + fixes | as D2 | the stop | on | F1 F2 F3 in both runtimes |
| D3 | Node and Rust at once | none | on | – |
| D4 | Rust | one | off | – |
| D5 | Rust | one | off | F1 F2 F3 |

Verus (`deals_kernel.rs`, 60 verified, no `assume`/`external_body`): (a) validity: active, upcoming and expired
partition every (from, to, now), an offer is never active at or after `to`, `validity_exec` is the code's branch
order; (b) keys: `tjek:<id>` / `fw:<store>:<ean>` bytes are injective over (source, ids) when food-waste store ids
have no `:` (`lemma_wire_injective`), and not otherwise (`lemma_wire_collision`: store `a:b` + ean `c` and store `a` +
ean `b:c`; Salling store ids are UUIDs, but `sources.rs` does not check it); `group_exec` (the code's `group_deals`)
gives one group per group id and keeps every member's key in its group, `deal_keys_exec` keeps the key and every
related key, and acknowledging a group covers each member alone (`lemma_group_ack_covers_members`); (c) the new-match
decision: after a run's acknowledgment (disjoint key sets), any offer sharing a key with an acknowledged deal is not
new at that price or above (`lemma_ack_idempotent`), strictly cheaper on its keys is new
(`lemma_strict_cheaper_is_new`), the alert filter selects exactly the offers with no key notified at or below their
price (`lemma_alert_guard`), and the overwrite forgets a cheaper notification (`lemma_overwrite_forgets_cheaper`,
the D0 price bounce); (d) matcher: a token without the term as a suffix never matches (`lemma_no_suffix_no_match`), a
one-word term scores only on an equal token or a head-final compound with a >= 2-letter prefix
(`lemma_single_term_needs_suffix`, `lemma_prefix_only_never_matches`), the four examples `skinkepizza`,
`skinkeschnitzler`, `rugbrødschips`, `mælkechokolade` never match (`lemma_prefix_only_examples`), any exclude hit
makes `match_deal` 0 (`lemma_exclude_veto`, reachable: `lemma_pizza_is_vetoed`); `term_score_exec` and
`match_deal_exec` return the code's integers. Boundaries: date parsing, Unicode lowercasing/NFD and the token regex,
the `\p{L}` class (per-character flags), `display` of raw ids, SHA-256; prices are integers (the code's f64 compare
the same way on the prices sources return).

`verification/verus/diff/run-deals.sh` (in the models section of `verify.sh`) extracts the pinned `lucina-deals` and
`lucina-grocery` with `git archive` into `target/deals-diff` and runs `deals.rs` (14 tests) and `deals_group.rs`
against the real functions: 20 000 validity triples at and around the boundaries, generated food-waste keys through
the real `Sources::food_waste` and every fixture flyer key, 5 000 `deal_keys`, 3 000 alert batches (selection,
acknowledgment, idempotence, strictly cheaper), 60 000 `term_score` and 20 000 `match_deal` cases on generated Danish
headings, 4 000 `group_deals` inputs, and the real-code witnesses of the findings below. The kernel's exec functions
enter as a plain-Rust copy (`deals_kernel_exec.rs`); the script stops when `deals_kernel.rs` no longer has the
SHA-256 in `deals-kernel.sha256`, so the copy cannot drift silently.

#### Findings (current code; production code unchanged, kept as `Violated` / `MUST FAIL`)

Traces are the shortest (plain BFS), actor 0 = Node, 1 = Rust; `Poll(a, 0)` = both sources answered; `Edit(0, 0)`
removes the food-waste offer, `Edit(0, 2)` reprices it high.

**DW1, a send whose save is lost repeats offers** (D1, D2; `no_repeat`, `one_digest_per_day`,
`silent_when_unchanged`). `run` delivers, then acknowledges and saves. If the response is lost (`AppNotify` reports
`Failed` after the server stored the notification) or the process dies before the rename, the file does not know the
owner got the message. The server only suppresses an identical retry on the same date:

```text
D1 no_repeat (9):            Tick; Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost);
                             Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered)
D1 one_digest_per_day (9):   Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost); Edit(0,0);
                             Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered)
D1 silent_when_unchanged (9): as no_repeat (the alert's observation equals the digest's)
D2 one_digest_per_day (11):  Load(0,Digest); Poll(0,0); Decide(0); Send(0,Delivered); StopNode; Edit(0,0);
                             StartRust; Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered)
```

The first: the digest reached the owner, the alert at 09:00 does not see it acknowledged and sends the food-waste
offer again (the digest key and the alert key differ by kind). Code: `watch.rs::DealWatch::run` (deliver, then
`acknowledge` + `save`), `compose` (`dedup_key` from kind, date and delivered offers),
`notify.rs::AppNotify::deliver`. The cutover hits the same gap when Node is stopped between its send and its save
(D2). Fix F1; D1 + fixes and D2 + fixes hold, and D5 shows F1 needs the server dedup.

**DW2, price bounce** (D0, no fault; `no_repeat`). `acknowledge` overwrites the notified price with the current one,
also upwards (`WatchState::set_notified`). Notified at the low price, then a later delivered message acknowledges the
high price (the next day's digest), then the price drops back and is alerted again:

```text
D0 no_repeat (20): Tick; Tick; Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Tick; Tick;
                   Edit(0,2); Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Edit(0,1);
                   Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered)
```

Verus: `lemma_overwrite_forgets_cheaper`; real code: `price_bounce_alerts_a_received_price_again`. Fix F3.

**DW3, an alternative is acknowledged without being shown** (D0; `no_skip`). Messages show each item's best offer
only, but `acknowledge` records every active offer (`for item in &prepared.active { for deal in &item.deals …`;
Node's comment: "Their alternatives are already known"). When the best offer goes away, the alternative is the best
offer, active across alert ticks, never shown, and every alert skips it until the next digest:

```text
D0 no_skip (11): Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Edit(0,0); Tick; Tick;
                 Load(1,Alert); Poll(1,0); Decide(1)
```

Real code: `alternative_acknowledged_without_delivery`. This is deliberate in Node and Rust, but it breaks `no_skip`
as stated. Fix F2.

**D3, two watchers** (rule witness, like P3/P4): stale snapshots and lost updates on the shared file give two digests
on one date without any fault (16 actions). Never run both; the performed cutover stopped Node first.

**D4/D5, the dedup key's role.** Without server dedup the identical retry after a lost response is a second digest
(D4, 8 actions: `Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost); Load(1,Digest); Poll(1,0); Decide(1);
Send(1,Delivered)`); with dedup the same retry is suppressed and the D1 trace needs a changed batch. F1 relies on
dedup: without it the pending re-send repeats (D5, 7 actions).

**Key domain.** `sources.rs::food_waste` joins raw upstream strings, so `fw:` keys are injective only for store ids
without `:` (above); `relatedKeys` grouping and `deal_keys` keep every member, so acknowledging a group covers all of
its branches.

## Bugs found

Severity reflects today's deployment. **Fixed** means fixed on branch `verify-fixes` (from `main`, commit 1e19f27),
with regression tests. The other bugs are in code on branches other people work on, in uncommitted work, or in
plans, so they are reported here with the proposed fix and not changed.

### T1: a late timeline line is skipped on resume (medium; the phase-1 "known weakness" does harm)

The model confirms that the weakness in PHASE1 §5.3 is harmful. Shortest trace (`T1`):

```
NodeGen(0) NodeRefreshOpen NodeWrite      Node writes N1
NodeGen(0) NodeRefreshOpen                Node takes N2's id, file open, write not landed yet
RustBegin(1) RustWrite                    Rust writes R with R > N2 (same ms)
SseDisconnect SseConnect                  the app reconnects; replay ends at R (replay_end = R)
NodeWrite                                 N2 lands after R in the file
Settle                                    N2 arrives live but `it.id <= replay_end` drops it;
                                          a later resume from R also skips it (after(R) is by id)
```

The cause is that id order is not commit order. Three paths produce a late line:

- A Node id taken before Node's write lands.
- Rust's own id, taken at `normalize` before the locks; this also hits Rust's own lines.
- The proactive engine's decision-time id, which can be seconds old.

A client that never reconnects still gets the line live, which is what PHASE1 noted. A resume or a replay that
overlaps the line loses it. The two fixes:

- **Fix A** (lucina-rs, `lucina-store` + `api.rs`): keep an arrival sequence per index entry and add
  `after_arrival(id, limit)` (entries after `id`'s position in file order). Replay and follow by arrival order,
  put replayed ids into `seen`, and drop the `replay_end` filter. Model T1 + fix A and T6 hold.
- **Fix C** (clean): one writer. Node and the proactive engine append through the Rust store (a Node shim like the
  phase-2 reminders one, `POST /api/internal/timeline`). Ids are then assigned under the flock in file order. T7
  holds.

### T2: a Node line written into `.1` after a Rust rotation is never indexed (medium-low)

```
NodeGen(0) NodeRefreshOpen     Node opened timeline.jsonl
RustBegin(0) RustWrite         Rust writes R1
RustBegin(0)                   Rust decides to rotate (it holds the flock; Node does not care)
NodeWrite                      Node's write lands in the old inode...
RustRotate RustWrite           ...which is now `.1`; Rust's known size for it is stale
Settle                         Rust never reads `.1` again: N is missing from list/SSE until lucina-rs restarts
```

Fix B (lucina-rs): after `rotate`, keep `(inode, known)` of the previous generation and index its tail on every
`refresh`. That costs one `stat` per refresh. Fix C also removes this race.

### T3: `docker exec … cat >>` can tear lines (low-medium)

`cat` writes whatever each read of the docker stdin stream returns. A line that arrives in two chunks becomes two
writes, and another writer's line can land in between. Both lines are then lost to every reader (neither parses).

```
ProGen ProOpen ProWrite(head)   RustBegin RustWrite   (ProWriteTail)
```

Fix (lucina-proactive): the cutover chowned `timeline.jsonl` to nez, so use `FileSink` (a single `write_all` on an
O_APPEND fd), or append through Rust (fix C). The engine runs dry-run today, so this is latent.

### T4: two rotators (low)

`cutover.env` says "Node never reaches its own limit". That holds only while Rust appends: Rust rotates only inside
its own `append`. With Telegram-only use the file grows past 50 MB, Node rotates, and if both decide at the same
moment, one of them rotates an empty file. That drops a 40 MB generation of history one rotation early:

```
NodeGen NodeRefreshOpen NodeWrite NodeGen RustBegin(decides) NodeRefreshOpen(rotates) RustRotate(empty)
```

The fix is environment only: set Node's `LUCINA_TIMELINE_MAX_BYTES` very high, as PORT_PLAN §5 phase 1 already says.
Rust could also rotate from its 1 s poll.

### T5: the web stream misses items after an early drop (low). **Fixed.**

The browser sends `Last-Event-ID` only for an event it saw. A connection that drops before its first item
therefore reconnects fresh, the server replays nothing, and the client did not resync on that path:

```
RustBegin RustWrite      R1 written (queued for the stream)
SseDisconnect            dropped before delivering it
SseConnect               auto-reconnect without an id: fresh stream, cursor = newest
RustBegin RustWrite Settle  R2 arrives, R1 never does
```

Fixed in `web/src/api/timeline.ts`: a soft drop before the first item marks the stream for resync, and `onopen`
calls `loadLatest`. Regression test: `chat.test.ts`, "resyncs when the stream drops before its first item". It
fails without the fix. The iOS app should be checked for the same pattern.

### P1a / P1b: the cutover shares `secretary-jobs.json` (medium)

`cutover.env` points Rust at Node's live jobs file. A `cursor_task` started on Node just before the switch runs for
minutes:

```
OperatorCutover ScriptStep ScriptStep    Rust live, p1 still Node
AppStartJob RmwWrite(Node)               job J on Node
ScriptStep                               p1 -> Rust
AppPoll(J) RmwWrite(Rust)                Rust sees J running and not in its `active` set: writes "interrupted"
```

The app stops polling at that terminal status, although Node finishes J later (P1a). Both runtimes also
read-modify-write the file under in-process locks only, so a Node `updateJob` and a Rust insert can lose one
another (P1b: `AppStartJob ×2, RmwWrite(Node), RmwWrite(Rust)`).

Fix (lucina-rs): Rust keeps its own jobs file in live mode (`data/lucina-rs/secretary-jobs.json`). It reads Node's
file read-only for ids it does not know and returns those records as they are. It marks only its own records from
an older boot as interrupted (store `owner` and `bootId` in the record). P1 + fix holds.

### P2a: the phase-2 handover as written runs two `reminders.json` writers (planned; high if followed)

```
Operator (Rust up as the store, writer=rust)  CreateViaTelegram(0) [Node RMW]  CreateViaApp(1) [Rust RMW]
CreateWrite(Node) CreateWrite(Rust)          -> reminder 0 is lost
```

Fixed order, modeled and holding:

1. Rust opens only `/api/internal/reminders`. Its secretary path still forwards to Node, and nothing calls the
   internal endpoint yet.
2. Node restarts with the shim on and its scheduler off, after draining its tick.
3. Rust writes locally and starts its scheduler.

Rollback: Rust stops writing first (scheduler off, forwarding, internal endpoint closed; Node's shim calls fail
visibly for a few seconds), then Node takes the file and the scheduler back.

### P2b: a restart during a scheduler tick re-sends the batch (medium, today in Node, planned in Rust)

Both `processDueReminders` (Node) and `ReminderStore::process_due` (Rust) send every due reminder and write `sent`
once at the end:

```
CreateViaTelegram(0) CreateWrite(Node) TickRead(Node) TickSend(Node) Crash(Node) TickRead(Node) TickSend(Node)
```

PORT_PLAN §5 phase 2 promises that a restart in the middle of a delivery does not double-send; neither
implementation keeps that promise. Fix: claim before send (write `sending` atomically before `sendMessage`). A claim
found after a restart becomes `failed` with `lastError: "interrupted; not resent"`, shown in the reminder list. That
gives at-most-once delivery with a visible failure. Also restart Node only with an idle tick (drain on SIGTERM).
`no_double_delivery` and `no_silent_loss` then both hold.

### P3/P4: handovers (no bug)

The documented order holds. Two witnesses document the rules: starting Rust on Discord before Node lost its token,
and running with `HERMES_GATEWAY_LOCK=false`, each give two consumers.

### S1: a lost ack followed by a 409 applies an op twice (medium)

```
User(0) add milk+spec, Send, User(0) purchase, Serve (applied), Lose (response lost)
Send [add (dup), purchase] with the old revision
User(1)/Send/Serve/Receive   B buys milk meanwhile
Serve(0) -> 409 (purchase conflicts); nothing recorded; `duplicates` not in the body
Receive(0): rebase both on the fresh state with NEW op ids -> the add moves milk back to needed: applied twice
```

Fix: the 409 body carries the batch's `duplicates`, which `applyShoppingOps` already computes before the conflict
check. That change goes in Node and Rust (`lucina-grocery`). secretary-server must forward the 409 details: today
`shopping.rs::ops` replaces them with a fixed message, and that file is being changed on `shopping-items`. The
client drops those op ids from the outbox before `rebaseOps`. S1 + fix holds.

### S2: an op on an unacknowledged optimistic row is silently dropped (medium)

The real servers (Node, Rust) ignore the client's `rowId` and create a server uuid. `web/mock/shopping-v2.mjs`
accepts it, so the web tests cannot see this. An offline "add, then undo" goes out as one batch, the remove is
rejected `item_not_found`, and the client drops it, so the item stays on the list:

```
User(0) add bread (row A0)  User(0) remove A0  Send Serve  Receive  -> remove rejected, dropped
```

The same happens after a lost ack, or when another client's add made the add merge into an existing row.

Fix (Node and Rust servers): use the client's fresh `rowId` as the new row's id. When an add merges into an existing
row, record `rowId → row` and resolve op targets through that alias. S2 + fix and S3 (two clients, the same object,
lost messages, both fixes) hold. Verus K3 proves that one active row per object and "deleted stays deleted" survive
every op.

### O1: an OAuth code replay can leave the grant live (low; defence in depth). **Fixed.**

`token()` marked the code redeemed and called `issue_tokens` in two critical sections. A replay in between ran
`revoke_grant` on a grant not yet in `oauth.json` (a no-op). A request that lost the race answered "code already
used" without revoking:

```
OwnerLogin OwnerApprove Redeem Redeem  Step(A:lookup) Step(A:set redeemed) Step(B:lookup -> revoke no-op) Step(A:issue)
```

Fixed in `crates/secretary-mcp/src/oauth.rs` (`OAuth::redeem_code`): both happen under the `codes` lock, and losing
the race counts as a replay. Tests: `concurrent_code_redemption_revokes_the_grant` and
`a_redeemed_code_always_has_its_grant_on_disk`.

### O2: a concurrent refresh reuse does not revoke (low). **Fixed.**

Two requests with one refresh token both read it as unused; the loser answered "already used" and kept the grant.
Fixed in `OAuth::rotate_refresh`, which revokes like a sequential reuse. Test:
`concurrent_refresh_with_one_token_revokes_the_grant`. Both tests fail with the old revocation logic (checked by
mutation).

### G1: with `LUCINA_PLANNER=tools`, MCP callers can create and confirm external actions (high if enabled; latent)

`run_turn` calls `tools_turn(st, message, session, cmid, channel)` before any MCP check and drops `mcp_turn`. An MCP
caller in session `mcp-<caller>` can ask for an external tool and then send "yes":

```
Message(mcp, Request) Message(mcp, Yes) TurnStart TurnStart -> executed on an MCP yes
```

The MCP guard, the timeline `via: mcp` meta and the Meshy tool's `ctx.mcp_caller` refusal are all bypassed. The
Meshy WIP sets `ctx.confirmed` on that path, so this is a credit spend on an MCP "yes". Live config is
`LUCINA_PLANNER=json`. Fix (lucina-rs): pass `mcp_turn` into `tools_turn`. There, refuse gated tools for MCP
turns, set `ctx.mcp_caller`, and treat an MCP "yes" as an ordinary message.

### G2: a "yes" typed before the question was shown confirms it (low)

Two turns of one session in flight: the second `TurnStart` sees the pending action the first just created, before
the owner saw the question. Fix: a pending action is confirmable only after the reply that asks for it was delivered
(`shown`).

### G3: the attention budget and cooldown hold only within one tick (low-medium)

The ledger is read per candidate with nothing serialising ticks. A `lucina-proactive once` run (live) next to the
`run` daemon sends four unsolicited messages against a budget of three, and two of the same kind inside a cooldown:

```
Gate Gate Send Gate Send Gate Send Send
```

Fix (lucina-proactive): `pg_try_advisory_lock` for the whole tick, skipping the tick when the lock is held.

### G4: Meshy's daily cap is enforced on estimates (medium, money; uncommitted WIP)

`Ledger::reserve` checks the estimate and `settle` records what Meshy reports, but nothing stops a step when Meshy
charges more than estimated:

```
Confirm(0) Reserve(0)=20 StepRuns(0,10) StepRuns(0,13) Confirm(1) Reserve(1)=20 StepRuns(1,23) -> 46 > 45
```

Fix: reserve the worst case per step (the estimate plus a margin bounding Meshy's drift), and before each step stop
when consumed plus the next worst case would exceed the reservation. Verus K5 proves the arithmetic.

### DW1–DW3: the grocery deal watch repeats or skips offers (low; notification noise, lucina-rs)

Details and traces in §7. DW1: a send whose save is lost (lost response, crash, or the cutover's stop) lets the
next run repeat offers, because secretary-server only suppresses an identical message on the same date. DW2:
`acknowledge` raises a notified price, so a price that goes up and back down is alerted twice. DW3: alternatives are
acknowledged without being shown, so an alternative that becomes the best offer is not alerted until the next
digest. Proposed fixes (lucina-rs, `watch.rs`): F1 save the composed message and its dedup key before sending and
re-send it first after a restart; F2 acknowledge only the delivered best offers; F3 never raise a notified price. The
model holds with all three (D1 + fixes, D2 + fixes).

## Modeling assumptions

- **Timeline.** A single `write()` of a whole line to an O_APPEND regular file is atomic (Linux; lines stay far
  below Node's 512 KiB `writeFile` chunk). ULID random bits never collide; a writer tag stands in for them. The
  model covers one SSE client and at most one Rust restart. A broadcast `Lagged` is not modelled: it falls back to
  `after(cursor)`, which the follow tick already models.
- **Ownership.** Each operator step is atomic. ssh failures stop a script between steps.
- **Shopping.** Each batch is one serialisable transaction (list row `FOR UPDATE`). The model has one list and two
  objects; `move`, `reorder` and `edit_item` are not modelled.
- **OAuth.** The attacker cannot forge a PKCE verifier and cannot register an allowed redirect_uri.
- **Deal watch.** A run takes seconds, so clock windows and source changes happen between runs; the digest
  cooldown (10 min) is not modelled (a retry may come at any later time). Item names, body length and the five-item
  limit are not modelled (the real-code tests keep `compose` exact). The server keeps every dedup key (true for a few
  watch messages: 4096 LRU keys, 500 inbox items, no deletions).
- **Bounds.** The bounds are small (2–4 lines per writer, 2 jobs, 2 reminders, 3–5 requests). The Verus kernels
  cover the per-step invariants for all inputs.
