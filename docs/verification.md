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
| 6 | Media player resume positions across devices (iPhone, web, Mac) and the server | `player.rs` (`ResumeSync`) | `player/ResumeSync.tla` | `player_kernels.rs` M1–M5 | `crates/secretary-player/src/{progress,prefs}.rs` (every device: iOS through `secretary-ffi`, web through `crates/secretary-player-wasm`); server `crates/secretary-media-stream/src/{progress,token}.rs` (branch `media-player`, mounted by `lucina-media`) |

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
| R before fixes R1, R2 (`updated_at` only) | 86 766 | 22 | `no_position_lost`, `devices_converge` **violated** |
| R + fix R1 only (device clock stamps) | 157 777 | 24 | `devices_converge` **violated** |
| R + fixes R1, R2 | 514 139 | 26 | all hold |
| R naive server (every push overwrites) | 1 066 693 | 28 | `no_position_lost`, `server_never_regresses` **violated** (rule witness) |
| R naive device (adopts any server entry) | 1 046 485 | 30 | `never_back_to_stale` **violated** (rule witness) |
| R naive acknowledge (ignores time) | 541 645 | 26 | `no_position_lost` **violated** (rule witness) |

TLC, default cfgs: from 20 to 82 317 distinct states per cfg (`Timeline_fixed`: 82 317, `Cutover_fixed`: 13 605,
`ShoppingOps_fixed`: 2 686, `PendingActions_fixed`: 2 457, `OAuth_fixed`: 2 214, `ResumeSync_fixed`: 514 139, the
same count as the Stateright case). `Timeline_fixed_deep.cfg`
(`--deep`): 1 053 488.

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

### 6. Media player resume positions

One film, two devices (three with `--deep`), the server. Devices play (a new position, stamped), push their
unacknowledged entry one request at a time, pull the server's entry, and go offline (their messages in flight are
lost). The network delays and reorders. Clocks are shared milliseconds, so two devices can stamp the same
`updated_at`.

Invariants:

- `no_position_lost`: once every device is online and clean and nothing is in flight, the server holds the newest
  position anyone played (newest by `(updated_at, device)`).
- `devices_converge`: ...and every device that pulled since the server's last change shows that position.
- `never_back_to_stale`: a device never adopts an entry older than its own.
- `server_never_regresses`: the server never replaces its entry with an older one.
- Witnesses: `concurrent_playback` (two devices played between syncs) and `offline_gap_resolved` (an offline gap
  with unsynced positions ended with every device on the newest position).

The Verus kernels (`player_kernels.rs`) prove the per-step rules for all inputs: the `(updated_at, device)` order is
a strict total order; `merge` takes only a newer entry, so a device never goes back; `Store::put` keeps the newer
entry and returns exactly what is newer; an acknowledgement never clears a later write; merging is commutative,
associative and idempotent; `write_stamp` stamps after the held entry; positions are clamped to the duration; the
resume point never passes the saved position; the play token's base-36 expiry round-trips and `verify` accepts
only an unexpired, untampered token; the subtitle choice per film wins, "off" stays off, other films keep theirs,
and the language default is the best-ranked track; every FFI request path returns an answer (bridge_error unless
the engine ran); and the wasm answer pack round-trips exactly for 32-bit addresses.

| Model action | Code |
|---|---|
| `Play` | `Book::tick` (`write_stamp`, `clamp_position`): iOS `MediaPlayer.reportNow`, web `state/media.ts::report` |
| `Push` | `pushProgress` (iOS `MediaPlayer`, web `state/media.ts`): `pending` → `POST /v1/media/progress` |
| `Deliver(Push)` | server `progress::Store::put` (`crates/secretary-media-stream/src/progress.rs`) |
| `Deliver(Reply)` | `Book::acknowledge`, then `Book::merge` of the returned entries |
| `Pull` + `Deliver(PullReply)` | `syncProgress`: `GET /v1/media/progress` → `Book::merge`; `/play`'s `progress` field → `merge` |
| `Offline` | a failed request: the entry stays pending (`pending`) and is pushed again later |

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

### R1: two devices writing in the same millisecond end on different positions (low). **Fixed.**

`Store::put` and `Book::merge` compared `updated_at` only. With equal stamps the server kept the first push, the
second device's entry was acknowledged and not replaced, and a device that pulled kept its own:

```
Play(0) Push(0) Play(1) Push(1) Deliver(Push 0 @1) Deliver(Push 1 @1) Deliver(Reply 0) Deliver(Reply 1)
-> server holds device 0's position, the newest (by device) is device 1's
```

Fixed in `crates/secretary-player/src/progress.rs`: `Entry::is_newer_than` orders by `(updated_at, device)`, and
`merge` uses it. The server's `Store::put` must use it too (branch `media-player`). Test:
`same_millisecond_writes_have_one_winner`.

### R2: a position played after adopting another device's entry is dropped (medium). **Fixed.**

Found by the model once R1 was in. A device stamped a new write with its own clock. After it adopted an entry
from a device whose clock is ahead (or from the same millisecond), its next position was older than the entry it
held. The acknowledgement of its earlier push then cleared it, and the devices stayed apart:

```
Play(0) Push(0) Play(1) Push(1) Deliver(Push 0) Deliver(Push 1) Pull(0) Deliver(Reply 1)
Deliver(PullReply 0: device 1's entry) Play(0) Deliver(Reply 0: acked @1 clears the new write)
```

Clocks differ by seconds between an iPhone, a laptop and a browser, so this is the ordinary case after a hand-off
from a device whose clock is ahead. Fixed with `write_stamp`: a write is stamped `max(now, held + 1)`
(Lamport-style). Test: `a_write_after_adopting_a_faster_clock_still_wins`.

## Modeling assumptions

- **Timeline.** A single `write()` of a whole line to an O_APPEND regular file is atomic (Linux; lines stay far
  below Node's 512 KiB `writeFile` chunk). ULID random bits never collide; a writer tag stands in for them. The
  model covers one SSE client and at most one Rust restart. A broadcast `Lagged` is not modelled: it falls back to
  `after(cursor)`, which the follow tick already models.
- **Ownership.** Each operator step is atomic. ssh failures stop a script between steps.
- **Shopping.** Each batch is one serialisable transaction (list row `FOR UPDATE`). The model has one list and two
  objects; `move`, `reorder` and `edit_item` are not modelled.
- **OAuth.** The attacker cannot forge a PKCE verifier and cannot register an allowed redirect_uri.
- **Player.** One film (entries of different films never interact). Each push and each `Store::put` is atomic
  (the server's progress store takes a lock per batch). A write's position is unique, so a stamp identifies one
  write. HMAC-SHA256 is a secure MAC: the token proof abstracts the tag comparison as `tag_matches`.
- **Bounds.** The bounds are small (2–4 lines per writer, 2 jobs, 2 reminders, 3–5 requests). The Verus kernels
  cover the per-step invariants for all inputs.
