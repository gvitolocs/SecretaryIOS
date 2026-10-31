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
5. **Every deploy goes through `deploy-guard`** (protocol 7). Inside `/tmp/lucina-merge.lock`, build from
   `main`'s tip in a clean tree, run `deploy-guard check --runtime <name>` (it refuses unless the live build is an
   ancestor of the commit, the commit is on `main` and the tree is clean), deploy, then run
   `deploy-guard record --runtime <name>`, which also tags the deploy. Live but uncommitted work lands on `main` first.
   There are no deploy-line branches since 2026-10-30 (`docs/repo-workflow.md`).
6. Generated TLC output (`*_TTrace_*`, `states/`) is never committed (`formal/.gitignore`). `verify.sh` runs TLC
   with `-noGenerateSpecTE` and a temporary `-metadir`.

## How to run

On nezopt run every gate through `~/.local/bin/verify-gate` (`verify-gate`, or `verify-gate -- <cmd>` for a model
check or a single Verus file). It runs one gate at a time (`/tmp/lucina-verify.lock`) in a systemd scope with
`MemoryMax=12G` and no swap: on 2026-10-25 three parallel gates (about 10, 15 and 5 GiB) filled swap and got other
apps OOM-killed. It refuses `--deep` unless `VERIFY_DEEP_OK=1`, which is set only when the owner asks for an
exhaustive run. `scripts/verify.sh` prints a reminder when it is run outside verify-gate.

```sh
verify-gate                       # = scripts/verify.sh under the lock and the memory cap
verify-gate -- cargo run -p verification-models --bin verify-models -- --only deploy
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
| 7 | Multi-session merge and deploy (`/tmp/lucina-merge.lock`, the shared main checkout, deploys of secretary-server, the web app, lucina-rs) | `deploy.rs` (D1–D5) | `deploy/Deploy.tla` | `deploy_guard.rs` | `tools/deploy-guard`; the merge/deploy steps in each session's runbook (`deploy/README.md`, `deploy/lucina-rs/`) |
| 8 | Lucina's Pokémon Red: game control (one emulator, macros, pause/play from chat, iPhone, web, viewer) and the planner | `game.rs` (Pk) | `game/GameControl.tla` | `game_planner.rs` (P1–P4) | Lucina repo `rust/lucina-pokemon/src/{main,macros,world,plan}.rs` (branch `lucina-game-link`); `/control` callers: `src/game-bridge.js`, lucina-rs `lucina_intent::game` and `api.rs::game`, secretary-server `/v1/game/*` |
| 9 | Lucina media library: iPhone photo backup (resumable upload, dedupe, p1 rate limit) | `media_backup.rs` (M) | `media/MediaBackup.tla` | `media_upload.rs` | `crates/lucina-media/src/{protocol,http,ingest}.rs`, `ios/Secretary/Features/Gallery/PhotoBackup.swift`; p1 Caddy `@media_upload` (forward_auth, 60/min per device) |
| 10 | Voice memos: recorder + Live Activity, upload queue, pipeline, GPU lease, speaker labels, vault, notifications | `memos.rs` (V) | `memos/Memos.tla` | `memo_kernel.rs` (K1–K7, K11) | `crates/lucina-memo` (`kernel.rs`, `worker.rs`, `store.rs`, `notify.rs`; branch `lucina-rs`), `tests/upload_model.rs` (upload protocol, Stateright); iOS `Features/Memos/*` (`MemoRecorder`, `MemoUploadQueue.recoverOrphans`, `MemoOnDeviceTranscriber`; branch `ios-app`) |
| 11 | Media player resume positions across devices (iPhone, web, Mac) and the server | `player.rs` (`ResumeSync`) | `player/ResumeSync.tla` | `player_kernels.rs` MP1–MP5 | `crates/secretary-player/src/{progress,prefs}.rs` (every device: iOS through `secretary-ffi`, web through `crates/secretary-player-wasm`); server `crates/secretary-media-stream/src/{progress,token}.rs` (branch `media-player`, mounted by `lucina-media`) |
| 12 | Share to Lucina (`POST /v1/share`: clients, voice notes, gateway, receive, recovery, processing, chat bubble) | `share.rs` (SH) | `share/Share.tla` | `share_kernels.rs` SK1, SK4; `share_reconcile.rs` SK2, SK3 | `lucina-server/src/share/{mod.rs,router.rs}`, `lucina-store/src/timeline.rs::normalize` (branch `lucina-rs`); voice notes: `share/{mod.rs,router.rs,extract.rs}`, `chat.rs` (`ShareTurn`, `tools_turn`), `timeline.rs::valid_waveform` (branch `voice-notes`, 438d748); p1 Caddy `@share` (forward_auth + `X-Share-Gateway`); iOS `ios/Extensions/ShareShared/*`, `Features/Share/ShareUploads.swift`, `Features/Chat/{ChatShares,Timeline}.swift` (`ios-app`); Android `android/secretary/src/Share{Activity,JobService}.kt`, `crates/lucina-android-calls/src/share.rs` (`lucina-rs`); web `web/src/state/chat.ts` |
| 13 | Lucina OTP forwarding (phone SMS/notification → Rust core queue → HTTPS → p1 relay → Lucina `/otp` → owner) | `otp.rs` (OTP1–OTP10) | `otp/OtpForwarding.tla` | `otp_extract.rs` X1–X5; `otp_expiry.rs`; `otp_redact.rs`; `otp_owner.rs` | `lucina-android/src/{bridge,store,net}.rs`, `android/lucina-otp/src/*.kt`, `lucina-otp-relay/src/{store,http}.rs`, `lucina-server/src/otp.rs`, `lucina-phone-core/src/{otp,proto,reply}.rs` (branch `lucina-rs`, fixes merged at dd7ec9e); Node `Lucina/src/private-otp.js` + `tools/lucina-device` `otp-chat` |
| 14 | Grocery deal watch (digest, alerts, Node → Rust cutover) | `deals.rs` (G0–G5) | `deals/DealWatch.tla` | `deals_kernel.rs` | `lucina-deals/src/{watch,schedule,notify,engine,sources,matcher,validity}.rs`, `lucina-server/src/deals.rs` (branch `lucina-rs`, pinned in `verification/verus/diff/deals-revision`); Node `Lucina/src/grocery/deals/watch.js` (`68b38dc~1`); `crates/secretary-server/src/notify/{mod,store,routes}.rs` |
| 15 | In-app maps (place blocks, the map server: tiles, assets, geocoder, routes) | – | – | `map_kernels.rs` (MK1–MK7) | `crates/secretary-maps/src/{kernel,lib,geocode,routes,provider,cache}.rs` (reference `normalize_place`; lucina-rs and the web app mirror it), `deploy/secretary-maps.service`; differential test `crates/secretary-maps/tests/kernel_model.rs` |
| 16 | Meetings: Mac recorder upload (create, resumable tracks, complete), meetings pipeline (Whisper per track, diarization, voice profiles, roles/kinds, brief, archive, note, memory, notification, voice question) | `meetings.rs` (MT) | `meetings/Meetings.tla` | `meeting_kernel.rs` (MT1–MT8) | `crates/lucina-memo/src/meetings/{kernel,store,api,worker,publish,timeline}.rs`, `src/diarize/profiles.rs` (branch `meetings-local`); differential test `tests/verus_meeting_kernel.rs`; Mac uploader `SecretaryCode/packages/desktop/src/main/secretary/recorder/uploader.ts` |
| 17 | Node retirement: Rust owns Telegram, Discord and reminders (no Node change) | `retirement.rs` (R1–R5) | `retirement/{Reminders,Delivery,Telegram,Updates,Discord}.tla` | `retirement_kernels.rs` | `lucina-telegram/src/{outcome,lease_decision,lease,lib}.rs`, `lucina-store/src/reminders.rs::process_due`, `lucina-server/src/reminder_delivery.rs` (branch `lucina-rs`; checked at 3c22447), `lucina-telegram/src/{claim,poller}.rs`, `lucina-server/src/delivery_monitor.rs::step` (f26eb90, NR4 fix 5c87ae0; Verus `delivery_monitor.rs`), `lucina-discord/src/lease.rs` (290b946), `deploy/lucina-rs/*`; Node `Lucina/src/gateway-lock.js`, `index.js` (68b38dc), telegraf 4.16.3 |
| 18 | Lucina Drive: the folder index (every file of the Drive folders indexed in place; scans racing unmounts, deletes and edits; dedupe of a phone copy against a Drive file; the gallery/films/drive split; path guards; the thermal pause) | `drive_index.rs` (DI) | `drive/DriveIndex.tla` | `drive_kernels.rs` DK1–DK5 | `crates/lucina-media/src/{drive,drive_kernel,drive_kernel_shared,extract,ingest,db,http}.rs` (branch `cloud/drive-all-files` from `media-instant`) |
| 19 | Album share links (`/v1/media/a/{token}`: the only part of the library reachable from the internet without the upstream key) | – (a per-request check, no protocol between processes) | – | `album_share.rs` AS1 | `crates/lucina-media/src/{share,share_kernel_shared,share_http}.rs` |

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
| D1 24–25 Oct: lock, builds from the checkout or main, no guard | 53 631 | 29 | `no_rollback`, `prod_on_main`, `no_dirty_deploy` **violated** |
| D2 lock skipped (old habit), clean builds only | 417 346 | 24 | `no_rollback`, `single_holder` **violated** |
| D3 lock + clean builds, branch deploys, no guard | 85 221 | 33 | `no_rollback`, `prod_on_main` **violated** |
| D4 fix: lock + deploy-guard + commit live WIP | 62 062 | 31 | all hold (`--deep`, 3 sessions: 37 050 072 states, depth 45, 48 s, 4.3 GB) |
| D5 guard without the lock (check-then-push race) | 1 403 757 | 26 | `no_rollback`, `prod_on_main`, `single_holder` **violated** |
| Pk game control, lucina-game-link 2f837e0 | 26 628 | 40 | `no_clock_frame_during_input`, `pause_within_one_step`, `no_save_during_input` **violated**; one driver holds |
| Pk + fixes Pk1 (clock under lock), Pk2 (no save while busy) | 2 582 | 17 | all hold (+ `paused_in_battle_then_resumed` reached) |
| M before fixes (500 on a lost upload, retry on 429) | 4 088 275 | 25 | `stored_once`, `no_dead_end_finish`, `no_request_storm` **violated** |
| M current (410 + pause on 429; ingest decides once) | 3 019 895 | 25 | `stored_once` **violated** (M3) |
| M + fix M3 (ingest decides again under the lock) | 2 000 522 | 25 | all hold |
| V voice memos, current code (before 2026-10-25) | 27 999 | 47 | `no_duplicate_notes`, `telegram_at_most_once`, `whisper_never_downgraded`, `no_lost_audio`, `finished_…` **violated** |
| V + fixes (kernel K1/K6/K7/K11, iPhone recovery) | 26 874 | 40 | all hold |
| V fixed, gpu-lease without dead-holder pruning | 23 490 | 39 | `finished_with_one_note_pdf_notification` **violated** |
| V fixed, a new transcript keeps the old diarization attempt | 25 842 | 38 | `done_labels_follow_transcript` **violated** |
| MT meetings, current code (kernel MT1–MT8) | 18 605 | 36 | all hold |
| MT ablation: append at any offset | 18 971 | 36 | `stored_is_prefix` **violated** |
| MT ablation: create without the idempotency key | 18 605 | 36 | `one_meeting_per_recording` **violated** |
| MT ablation: complete without checking the tracks | 66 979 | 36 | `queued_only_when_whole`, `done_has_every_output` **violated** |
| MT ablation: side effect before its marker | 29 405 | 36 | `telegram_at_most_once`, `memory_at_most_once`, `voice_asked_at_most_once` **violated** |
| R before fixes R1, R2 (`updated_at` only) | 86 766 | 22 | `no_position_lost`, `devices_converge` **violated** |
| R + fix R1 only (device clock stamps) | 157 777 | 24 | `devices_converge` **violated** |
| R + fixes R1, R2 | 514 139 | 26 | all hold |
| R naive server (every push overwrites) | 1 066 693 | 28 | `no_position_lost`, `server_never_regresses` **violated** (rule witness) |
| R naive device (adopts any server entry) | 1 046 485 | 30 | `never_back_to_stale` **violated** (rule witness) |
| R naive acknowledge (ignores time) | 541 645 | 26 | `no_position_lost` **violated** (rule witness) |
| SH fixed: composer album, crash, faults, other writers, attacker | 21 400 | 21 | holds; retry/crash/album/auth witnesses |
| SH fixed: composer + extension with App Group (background) | 58 514 | 31 | holds; retry/bg-kill/crash/album witnesses |
| SH fixed: composer + extension without App Group (today) | 306 346 | 39 | holds; fg-kill-lost/retry/crash witnesses |
| SH window bug (recovery scans the newest N=1 items) | 23 712 | 25 | `sh2`, `sh3` **violated** |
| SH reconcile bug (bubble matched by the 202's userItemId only) | 21 400 | 21 | `sh5` **violated** |
| SH stuck bug (failed append → 500, no worker) | 14 504 | 21 | `sh6` **violated** |
| SH ablation: no `X-Share-Gateway` check, lucina-rs reachable directly | 260 843 | 29 | `sh4` **violated** |
| SH fixed: voice note (on-device / whisper / failed transcript), faults | 44 720 | 20 | all hold; voice/device/server/unheard/retry/crash/auth witnesses |
| SH ablation: "couldn't hear it" item without `replyTo` | 52 936 | 23 | `sh2`, `sh3`, `sh7` **violated** |
| SH fixed: voice note with the tools planner (progress items suppressed) | 62 000 | 21 | all hold; voice/tool/unheard/crash witnesses |
| SH ablation: tool progress items on a voice note | 78 656 | 21 | `sh2`, `sh3`, `sh7` **violated** |
| OTP1 phone reboot during the first send | 195 818 | 20 | all hold |
| OTP1_bug_pre_ac9e7be (dedup and queue saved after the send) | 119 820 | 19 | `no_silent_loss`, `at_most_once`, `never_after_expiry` **violated** |
| OTP2 `/otp arm` while a code waits; an MCP `/otp` | 86 552 | 19 | all hold |
| OTP2_bug_pre_60472d9 (arm clears pending) | 117 836 | 19 | `no_silent_loss` **violated** |
| OTP3 two `/otp` after a pause | 90 127 | 19 | all hold |
| OTP3_bug_pre_60472d9 (auto-arm race) | 91 880 | 20 | `no_silent_loss` **violated** |
| OTP4 relay restart, failed `seen.json` write (503 + rollback) | 1 017 387 | 25 | all hold |
| OTP4_bug_pre_60472d9 (seen ids in memory only) | 506 657 | 22 | `at_most_once` **violated** |
| OTP5 drops, reboot, relay restart | 2 406 551 | 24 | all hold |
| OTP6 pause/arm, two `/otp`, self-test | 814 164 | 22 | all hold |
| OTP7 timed windows: drops, reboot, restart | 2 022 712 | 26 | all hold |
| OTP8 ablation: no private-owner guard | 60 064 | 18 | `owner_only` **violated** |
| OTP9 ablation: Lucina skips its expiry check | 24 115 | 17 | `never_after_expiry` **violated** |
| OTP10 ablation: relay without seen ids | 106 961 | 21 | `at_most_once` **violated** |
| G0 current Rust, no faults | 138 980 | 29 | `no_repeat` (DW2), `no_skip` (DW3) **violated**; the others hold |
| G1 current Rust after the cutover, one fault | 1 172 708 | 29 | `no_repeat`, `one_digest_per_day`, `silent_when_unchanged` (DW1), `no_skip` **violated** |
| G1 + fixes F1, F2, F3 | 1 149 598 | 33 | all hold |
| G2 cutover as performed (stop Node, start Rust) | 1 035 974 | 31 | the same four **violated** (DW1 through the stop, DW2, DW3) |
| G2 + fixes in both runtimes | 958 309 | 35 | all hold |
| G3 Node and Rust both running (rule witness) | 1 969 101 | 29 | the four **violated** |
| G4 current, no server dedup | 1 215 167 | 29 | the four **violated** |
| G5 fixes, no server dedup | 1 188 512 | 33 | `no_repeat`, `one_digest_per_day`, `silent_when_unchanged` **violated** |
| DI current (DK3 upgrade, DK4 with the filesystem check) | 9 090 | 22 | all hold (+ prune, stray, walk-error, upgrade witnesses); `--deep` 22 284, depth 28 |
| DI ablation: DK4 without the filesystem check | 10 296 | 22 | `unmount_never_trashes` **violated** (a stray file in the empty mountpoint) |
| DI ablation: trash whatever the walk did not see | 15 768 | 22 | `unmount_never_trashes` **violated** (an unmount mid-scan) |
| DI before DK3 (a phone copy of Drive bytes was dropped) | 9 090 | 22 | `phone_original_kept` **violated** |
| R1–R5 (protocol 17, 21 cases) | 26 – 4 186 | ≤ 20 | see §17 |

TLC, default cfgs: from 20 to 82 317 distinct states per cfg (`Timeline_fixed`: 82 317, `Cutover_fixed`: 13 605,
`ShoppingOps_fixed`: 2 686, `PendingActions_fixed`: 2 457, `OAuth_fixed`: 2 214). `Timeline_fixed_deep.cfg`
(`--deep`): 1 053 488. `InkSync_status`: 35 800 and `InkSync_highlights`: 17 556 (both with the liveness
property `EventuallyAllApplied` under weak fairness); `InkSync_highlights_deep` (`--deep`, the Stateright I2
bounds): 501 634, the same space as I2 (about 5 min); `InkSync_status_deep` (the I1 bounds, with liveness):
1 715 162 (about 20 min; TLC's state differs from Stateright's in ghost variables, hence the different count).
`GameControl_fixed`: 11 208; `GameControl_current_driver` (one driver in the current code): 116 344.
`MediaBackup_fixed` (the current code): 2 000 522, the same space as the Stateright `M current` case;
`MediaBackup_fixed_deep` (`--deep`, the deep M bounds): 8 518 209, the same space as the deep Stateright case (about 80 s).
1 715 162 (about 20 min; TLC's state differs from Stateright's in ghost variables, hence the different count). `Memos_fixed`: 26 874, the same space as Stateright's M + fixes.
`OtpForwarding`: 950 738, `OtpForwarding_owner`: 1 877 816, `OtpForwarding_restart`: 1 017 387 (the failed
`seen.json` write included); four `OtpForwarding_*_witness.cfg` and five `_bug` cfgs fail as required.

`formal/deals/`: `DealWatch_fixed` 1 149 598, `DealWatch_cutover_fixed` 958 309 and `DealWatch_current_nofaults` 138 980
distinct states (the same counts as Stateright G1 + fixes, G2 + fixes and G0), each under 10 s; the `MUST FAIL` cfgs stop
at their first violation.

`formal/drive/`: `DriveIndex_fixed` (MUST PASS), `DriveIndex_fixed_deep` (`--deep`), three `MUST FAIL` bug cfgs
(`nofs_bug`, `noguard_bug`, `dedupe_bug`, the Stateright ablations) and four witnesses (pruned, stray, walk error,
upgrade). First `verify-gate` run on nezopt (2026-10-26): `DriveIndex_fixed` 9 090 distinct states (the same as
Stateright DI current); `dedupe_bug` 62 (`PhoneOriginalKept` violated), `nofs_bug` 885 and `noguard_bug` 521
(`UnmountNeverTrashes` violated); witnesses pruned 924, stray 570, upgraded 15, walk error 251; each under 2 s.
`DriveIndex_fixed_deep` not run.

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

### 7. Multi-session merge and deploy

Claude sessions share `/tmp/lucina-merge.lock` (a `mkdir` lock on nezopt), the branches of this repo, the main checkout
`~/Projects/SecretaryIOS` (which held uncommitted live work: the Desk/Hermes web view, `/v1/hermes`, the calls module)
and the deploys of secretary-server, the web app and lucina-rs. Each model session has one feature on its own branch,
may leave uncommitted work in the shared checkout, and deploys to every runtime. Commit content is a set of features,
so "A is an ancestor of B" is "A ⊆ B". Two sessions and two runtimes by default; `--deep` checks the fix with three.

Invariants (Stateright name / TLA+ name):

- `no_rollback` / `NoRollback`: every runtime runs a build that contains everything deployed to it before.
- `prod_on_main` / `ProdOnMain`: production is always a commit on the target branch (nothing uncommitted or unmerged
  is live).
- `single_holder` / `SingleHolder`: the merge/build/deploy section is never occupied by two sessions.
- `no_dirty_deploy` / `NoDirtyDeploy`: no deploy ships a dirty tree.
- Witnesses: `all_features_live` / `Deploy_fixed_witness.cfg` (every session's work reaches every runtime) and
  `guard_refused` / `Deploy_fixed_refused_witness.cfg` (the guard does refuse a deploy).

The switches are the procedures: D1 is what was done on 24–25 Oct (lock taken; builds from the dirty checkout or
from main; nothing compared with what is live), D2 a session that skips the lock, D3 deploys from feature branches,
D4 the fix, D5 the guard without the lock. D5 shows why both are needed: `check` and the push are separate steps, so
a check that passes can be overtaken by another session's deploy, and an unlocked merge can lose another merge.

Verus (`deploy_guard.rs`, 11 verified): D-K1 `guard_allows` is exactly the rule; D-K2 an allowed deploy keeps
everything deployed before; D-K3 production stays on main as main grows; and over any sequence of guarded deploys the
last build contains every earlier one.

| Model action | Code / procedure |
|---|---|
| `Branch` | `wt add <name> main` (a worktree off main's tip) and commits there |
| `EditCheckout` | edits left uncommitted in `~/Projects/SecretaryIOS` (the main checkout) |
| `Acquire` / `Release` | `mkdir /tmp/lucina-merge.lock` (+ an `owner` file) / `rmdir` |
| `MergeRead` / `MergeWrite` | fetch the target branch, merge into its tip, update the branch |
| `CommitWip` | commit the live uncommitted work to the target branch (25 Oct: 7298be7, b7388d0) |
| `BuildMain` / `BuildCheckout` / `BuildBranch` | `cargo build --release` / `vite build` from main's tip, from the main checkout's working tree, or from a feature branch |
| `Check` | `deploy-guard check --runtime <r>` (`tools/deploy-guard`) |
| `Push` | `install` of `secretary-server` / `rsync` of `web/dist` to p1, `~/.local/bin/lucina-rs` + restart |

`deploy-guard` keeps the live commit per runtime in `~/.local/state/deploy-guard/<runtime>.commit` on nezopt (where
every build and deploy runs): `check` before the push, `record` right after it, `status` to list. Exit codes: 0 ok,
1 refused, 2 usage or git error, 3 no ledger entry (record the live commit once with `--bootstrap`). Every
runtime deploys from `main` (`docs/repo-workflow.md`): any other `--target` is refused, `record` tags the deploy
`deploy/<runtime>/<stamp>-<sha8>`, and `plan` lists the runtimes `main` changed since their live commit
(`deploy/targets.tsv`).
Its tests (`cargo test --manifest-path tools/deploy-guard/Cargo.toml`, in the models section of `verify.sh`)
check the rule for every input combination and replay the 25 Oct incident on real git repos.
### 8. Lucina's Pokémon Red: game control and the planner

The game server (`lucina-pokemon serve`, nezopt 127.0.0.1:18767) owns one Boytacean emulator behind a mutex. Lucina
plays through `POST /macro` (her Hermes turns, via the MCP tools); the owner starts and pauses her from chat
(Telegram, lucina-rs), the Secretary iPhone and web screens (`/v1/game/control` → lucina-rs → `/control`), the Mac
viewer over an SSH tunnel, and the play loop pauses itself after three failed turns.

Invariants (`game.rs` / `GameControl.tla`):

| Property | Meaning |
|---|---|
| `one_driver` / `OneDriver` | at most one macro holds `busy` (two Hermes turns, or a turn left over after a restart, never both press buttons) |
| `keys_only_by_busy_holder` | a key is down only for the macro that won `busy` |
| `no_clock_frame_during_input` | the real-time clock thread never advances a frame while a macro holds a key |
| `pause_within_one_step` | after a pause is applied, at most one frame advances with input |
| `no_save_during_input` | no save state is written while a key is down (it would come back pressed after a restore) |
| `paused_in_battle_then_resumed` (sometimes; TLC `GameControl_fixed_witness`) | a pause during a battle, then play again and input in the same battle |

| Model action | Code (Lucina repo, `rust/lucina-pokemon/src/`) |
|---|---|
| `ClockRead`, `ClockFrame` | `main.rs::server`, the clock thread (`busy` read, `next_frame`, 30 s checkpoint `save`) |
| `MacroStart` | `main.rs::handle` `/macro`: `busy.compare_exchange(false, true)`, else "Another action is still running." |
| `Press`, `Lift` | `macros.rs` `tap` / `step` (`with(|g| g.key_press(..))`, `key_lift`) |
| `Frame` | `macros.rs::frames` (under the lock: `if !e.running { return Err("Paused by the owner") }`, `next_frame`) |
| `End` | `/macro` after `macros::run`: `save`, `save_journal`, `busy.store(false)` |
| `Control(src, run)` | `/control` (chat: `src/game-bridge.js`, lucina-rs `lucina_intent::game`; iPhone/web: secretary-server `/v1/game/control`; viewer buttons; `driver` auto-pause) |
| `Save` | `/save` |
| `TunnelDrop`, `TunnelUp` | the Mac viewer's `ssh -L 18767` tunnel (its requests are lost while down) |
| `Restart` | `systemctl --user restart lucina-pokemon` / `redeploy.sh` (`running` is not persisted) |

Verus (`verification/verus/game_planner.rs`) proves, for every map up to 128 × 128 cells and every start and goal:
P1 a legal move lands on floor, on a Cut tree only with Cut, on a card-key door only with the Card Key, never on a
wall or a blocked cell (people, the Saffron guards, Snorlax, learned stops); a two-cell move hops a ledge in its own
direction from floor. P2 every route `plan` returns is a chain of legal moves from the start to the goal. P3 the
search and the route rebuild terminate (decreases on the rounds and on the parent distance). P4 the story step
`progress` shows next has every prerequisite done, for the `STORY_REQUIRES` table. P5 a Strength push is allowed only onto free
floor at the player's elevation (never stairs, a wall, a sprite or another boulder), moves exactly the boulder in front,
and a refused push changes nothing; P6 any push sequence keeps every boulder on a floor cell and no two in one cell.
The boulder search itself (`world.rs::solve_boulders_to`, a Sokoban search over boulder cells, switches and the
player's region, with Victory Road's switches, its 3F hole and the boulders that hole toggles) is tied to P5/P6 by
`victory_road_route`, which solves Victory Road from the ROM (1F 18 pushes, 2F 5, 3F 28 including the hole, 2F 15,
then the exit to Route 23) and replays every push through the kernel's push rule. Warps, edge connections, spinners
and the Mansion holes change maps and are outside the kernel. Two differential tests in the Lucina repo tie it to the
code: `world.rs::routes_only_enter_walkable_cells` checks every step of planned ROM routes (Diglett's Cave, Rocket
Hideout spinners, Viridian Gym, Silph Co. card-key doors, Cut on Route 9, ledges on Route 3, the Mansion with the
switch on: 267 steps) against the kernel's step rule, and `plan.rs::story_requirements_come_first` checks that
`REQUIRES` equals the kernel's `STORY_REQUIRES` line. `scripts/verify.sh` runs them when the crate is checked out
(`LUCINA_POKEMON_DIR`).
### 9. Lucina media library: photo backup

The iPhone backs up its photo library to lucina-media on nezopt through p1 (`docs/media-library.md`). Per asset the
phone sends `POST /v1/media/uploads`, `PUT /uploads/{id}?offset=N` with the rest of the file (a background
URLSession, which resends a cut-off request from the offset it last heard), and `POST /finish`. The server stores
the bytes content-addressed (blake3) and dedupes by hash; an edit of a known phone asset becomes a new version of
its item. p1's forward_auth counts every request against the device's 60 a minute and answers 429 beyond.

The model has two devices that both hold photo 0 (device 0 also has photo 1). Responses get lost, PUTs get cut,
lucina-media restarts, and p1's window allows `limit` requests. Finish is three server steps, as in the code: decide
(`known_for` + `ingest_decision` under the catalog lock), insert (`store::place_new`, then `insert_item` under the
lock again), mark (`finished = 1`). A restart can fall between any two.

Invariants (Stateright name / TLA+ name):

- `stored_once` / `StoredOnce`: the catalog never holds two items made from the same bytes.
- `no_partial_object` / `NoPartialObject`: every item's object is the whole file (declared size).
- `no_dead_end_finish` / `NoDeadEndFinish`: a finish never meets a lost upload with an error the client cannot get
  past (it gets 410 and starts the asset again).
- `no_request_storm` / `NoRequestStorm`: a device sends at most `limit + 1` requests per window (one 429, then it
  waits).
- `done_means_backed_up` / `DoneMeansBackedUp`: when both queues are empty, every photo is in the catalog.

Witnesses (`sometimes` / `MediaBackup_witness_*.cfg`): `resumed` (a resent PUT overlaps stored bytes), `rate_limited`
(a 429), `dedupe_hit` (device 1 sends bytes device 0 stored), `lost_upload_restarted` (a 410 and a fresh upload),
`all_backed_up`.

Verus (`media_upload.rs`, 13 verified): (a) `accept_chunk` never accepts a chunk past the stored offset (no gap) or
past the declared size, acknowledges a chunk wholly below the offset without storing it, and refuses a finished
upload; (b) the merge: stored bytes ++ the chunk minus its `skip` overlap equals the file's longer prefix, so over
any sequence of resent chunks the stored bytes stay a prefix: no gap, no duplicated byte; (c) `check_finish`
needs every declared byte, so with (b) the stored object is the whole file; (d) `ingest_decision`: an edit of a
known asset versions that item, known bytes are a duplicate, New only for unknown bytes and never shadows an
earlier item; (e) the client's `after_error`: a 429 waits at least 10 s (Retry-After, or 60 s), transport errors 120
s, 410 restarts the asset, anything else skips it; at 4 s per asset and at most 3 requests each, the backup uses 48
of the 60 requests a minute.

| Model action | Code |
|---|---|
| `Request(d, _)` at `Create` | `PhotoBackup.upload` → `GalleryAPI.createUpload` → `http::create_upload` |
| `Request(d, _)` at `Put` | background `URLSession` upload task → `http::put_chunk` → `protocol::accept_chunk` (skips `skip` bytes of the body) |
| `Request(d, _)` at `Finish` | `GalleryAPI.finish` → `http::finish_upload`: finished → same item; file missing → 410; `check_finish`; then `ingest::ingest_file` decide (`known_for`, `ingest_decision`; a duplicate's file is removed) |
| `FinishStep` (insert) | `ingest_file`: `store::place_new`, `insert_item` |
| `FinishStep` (mark) | `finish_upload`: `UPDATE uploads SET finished = 1, item_id` (same blocking task as the ingest since 8ee5623) |
| `Delivery::Lost`, `Delivery::Cut` | background-session cut-offs, timeouts; the phone resends from `known` |
| `Restart` | lucina-media restart (`systemctl --user restart lucina-media`, a crash) |
| rate limit, `Tick` | p1 `forward_auth GET /v1/avatar` → `secretary-server` `ratelimit.rs` (token bucket, 60/min per device); `PhotoBackup.run` on `APIError.rateLimited` waits `max(retryAfter ?? 60, 10)` s |
### 10. Voice memos

A recording on the iPhone to its transcript, summary, PDF, Obsidian note and notification (`docs/memos.md`). The
iPhone records (pause/resume in the app or from the Live Activity, both through `MemoRecordingCommands`), iOS may
kill the app at any time, the upload queue sends the file over a network that fails, a short memo also sends its
on-device transcript; the `lucina-memo` worker runs step 0 (ADTS → .m4a), 1 transcript (Whisper under a
`gpu-lease`, or the iPhone's transcript while the GPU is busy), 2 summary, 3 PDF, 4 Obsidian push, 5 notification,
done, and the idle Whisper upgrade; it may crash at any point (systemd restarts it; it continues from the recorded
steps). Another GPU job takes and releases the lease; `gpu-lease` drops a dead holder. The owner renames (any time)
or reprocesses (once done). Calendar link (2026-10-26, cases "V calendar link", `Memos_events_*.cfg`): step 1b
reads the calendar and then writes the link (two steps, so the owner can act in between); the owner picks the event
(a meeting, then none) before the summary or once the memo is done or failed; a linked note lives in its event's
folder and moves when the event changes; the worker may give up (`failed`, e.g. the old note's delete keeps failing).
Late events (2026-10-28, cases "V late event", `Memos_late_event_*.cfg`): the event may reach the server only after
the memo was processed; step 1b then leaves it link-pending, and the re-link job (`worker::relink_one`, over
lucina-memo's own calendar cache) links it when the event appears, with a quiet re-summary (no second notification).

Invariants (Stateright name / TLA+ name):

- `no_duplicate_notes` / `NoDuplicateNotes`: every note of the memo in the vault is its current one or a copy
  listed for deletion (`obsidian_old_notes`), whatever the crashes, renames and event changes; with the liveness
  property below, the vault ends with exactly one note.
- `owner_choice_kept` / `OwnerChoiceKept`: once the owner picked an event (or none), the memo keeps exactly that
  choice; the worker's link never overwrites it (K9), nor does the re-link job.
- `lecture_memo_ends_linked` (eventually, Stateright): a memo recorded in a lecture ends linked to it (or to the
  owner's choice) however late the event reaches the server; violated without the re-link job (the old one-shot link).
  Witness `relinked_quietly` / `Memos_late_event_witness.cfg`: linked late, note in the course folder, one notification.
- `telegram_at_most_once` / `TelegramAtMostOnce`: at most one Telegram message per revision.
- `app_inbox_once_per_revision` / `AppInboxOncePerRevision`: the app inbox has one entry per revision (dedup key).
- `whisper_never_downgraded` / `WhisperNeverDowngraded`: a Whisper transcript is never replaced by the iPhone's.
- `no_lost_audio` / `NoLostAudio`: a recording is never left unreadable and unqueued.
- `live_activity_matches_recorder` / `LiveActivityMatchesRecorder`: while the app runs, the Live Activity shows
  exactly the recorder's state (recording, paused, none).
- `done_has_every_output` / `DoneHasEveryOutput`: `done` only with transcript, summary, PDF, note and notification.
- `done_labels_follow_transcript` / `DoneLabelsFollowTranscript`: a finished memo's speaker labels (step 1a) were made
  for its current transcript (a Whisper upgrade or reprocess clears the attempt, K11); `Memos_diarize_reset_bug.cfg`
  and the Stateright ablation show the stale labels without the reset.
- `finished_with_one_note_pdf_notification` (eventually, Stateright): every recording ends done with one note (in
  its event's folder), the PDF, its notification, and the GPU lease released. Witness `note_moved_to_the_owner_s_event`. Witnesses: `gpu_busy_then_free` / `Memos_fixed_witness.cfg`,
  `device_transcript_replaced` / `Memos_fixed_upgrade_witness.cfg`.

The upload protocol itself (chunks, offsets, cut-off bodies, crashes, duplicates) has its own exhaustive model in
`crates/lucina-memo/tests/upload_model.rs` (prefix, one memo per recording, complete only when whole); here an
upload is one step that may fail.

Bugs found with the calendar link: B5 (Codex's first draft, never deployed) kept one old-note slot, so two event
changes around a failed publish left a second note (`Memos_events_old_note_bug.cfg`, 23-step trace); a link
written without re-checking the owner loses a choice made while the calendar was read (`Memos_events_recheck_bug.cfg`).
Fixed: `obsidian::move_note` keeps every old path; the worker's update closure re-checks `may_auto_link`.

Verus (`memo_kernel.rs`, 34 verified): K1 `transcript_choice` (never downgrades Whisper; Whisper wins whenever it
ran; the iPhone transcript is used only, and then always, when the GPU is busy and no Whisper transcript exists),
K2 `upgrade_replaces` (iff Whisper heard at least half as much; no overflow), K3 `route_on_device` (only with a
model for the language and up to the threshold), K4 `status_allowed` (`uploading` left only for `queued` and never
re-entered; `done`/`failed` entered only from a pipeline step; everything else can be re-queued), K5
`ready_to_finish`, K6 `send_telegram` with `lemma_telegram_at_most_once` (any number of retries from any marker:
at most one send), K7 `note_base` with `lemma_one_note` (a memo keeps its first note name over any sequence of
titles), K8 `overlap_ms`/`eligible`/`better`/`pick_event` (the exact clamped overlap, symmetric and never longer
than either interval; the pick is eligible, no eligible event is better, it beats every earlier eligible one, None
iff none is eligible; `lemma_better_order`: a strict order), K9 `may_auto_link`, K10 `title_from_event`. K11 `diarize_memo` with `lemma_diarize_once_per_transcript` (labels only when a diarizer is available,
none yet and no attempt on this transcript; at most one attempt per transcript over any runs). Mutations checked:
the old step 1 fails K1; sending before recording the marker fails K6. Differential
test: `crates/lucina-memo/tests/verus_kernel.rs` (production kernel vs the proved bodies, every small input);
`Store::update` asserts `status_allowed` on every write.

| Model action | Code |
|---|---|
| `TogglePauseInApp`, `TogglePauseFromLiveActivity` | `MemoRecorder.pause/resume` (both update the activity); `Pause/ResumeMemoRecordingIntent` → `MemoRecordingCommands.handle` |
| `Stop` | `MemosStore.finishRecording` → `MemoUploadQueue.add` |
| `KillApp`, `Relaunch` | iOS jetsam / SIGKILL; launch: `MemoRecorder.endStaleActivities`, `MemoUploadQueue.recoverOrphans` |
| `UploadOk`, `UploadFails` | `MemoUploadQueue.upload` (create by client id, chunks at the server offset, complete with SHA-256) |
| `SendDeviceTranscript` | `MemoUploadQueue.sendTranscript` → `PUT /v1/memos/{id}/device-transcript` |
| `WorkerStep` 1 | `worker::steps` step 1: `transcribe::transcribe` (the `gpu-lease` shim) + `kernel::transcript_choice` |
| `WorkerStep` 2, 3 | `summarize::summarize`, `render::print_pdf` |
| `WorkerStep` 4 / `Pushed` | `kernel::note_base`, `obsidian_note` recorded, `obsidian::push` (rsync), `published_at` |
| `WorkerStep` 5 / `Sent` | `kernel::send_telegram` (marker first), `notify::send` (app inbox `dedupKey memo:<id>:<revision>`), `notified_at` |
| `WorkerStep` done | `kernel::ready_to_finish`, `status = done` |
| `WorkerCrash` | process death; `gpu-lease` holder dies (`Dead`) |
| `OtherTakesGpu`, `OtherReleasesGpu`, `PruneDeadLease` | other `gpu-lease run` jobs; `gpu-lease` prunes dead pids on every access |
| `OwnerRename`, `OwnerReprocess` | `PATCH /v1/memos/{id}`; `POST …/reprocess {fromTranscript}` |
| `Upgrade` | `worker::upgrade_one` + `kernel::upgrade_replaces` |
| `WorkerStep` 10 / `Linking` | step 1b: `events::candidates` (`/v1/calendar/events`), `kernel::pick_event`, then `store.update` re-checking `kernel::may_auto_link` |
| `OwnerChoosesEvent` | `PUT /v1/memos/{id}/event` (`choose_event`), then `worker::request_resummary` |
| `WorkerStep` 4 / `Pushed` (events) | `obsidian::event_folder`, `obsidian::move_note` before the push, `obsidian::delete_note` of every old path after it |
| `WorkerGivesUp` | the last attempt fails: `status = failed` |
| `CalendarGainsEvent` | lucina-memo's calendar cache (`calendar.rs`) gains the event (iPhone report, any source) |
| `WorkerRelink` | `worker::relink_one`: one `store.update` re-checking `kernel::may_auto_link`, then the quiet re-summary |

### 11. Media player resume positions

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
### 12. Share to Lucina

Every share (the iOS share extension and chat composer, Shortcuts, Android's share target, the web share target and
composer) is one `POST /v1/share` with a client-made `shareId` (`docs/share.md`). p1 checks the owner's device token
(forward_auth) and adds `X-Share-Gateway`; lucina-rs refuses anything without it. `receive` runs under one mutex:
a known `shareId` returns the saved result (`duplicate`); otherwise files go into lucina-media, `status.json` is
saved (processing, with the user item and its fixed id), the user item is appended if missing, and a worker reads
every part and writes one Lucina answer. On restart `recover` respawns every share still processing.

The model has the composer's two-file album (retried with the same id), a share extension with or without the App
Group (without it, a kill loses the body and the owner re-shares under a new id), an attacker with no owner auth,
lost requests and responses, crashes between any two server steps, other timeline writers, and the chat view.

Voice notes (branch `voice-notes`, 438d748): the chat composer sends a voice note as one audio `file` part with
`meta.files[i].voice = true`, an optional on-device `transcript` and a `waveform`. `receive` routes it with
`router::route_for` and stores timeline kind `voice` (`timeline_kind_for`, only for audio); `process` reads it with
the audio handler, subtype `voice`. A share that is exactly one voice note with an empty caption is answered as if
the transcript had been typed: one user item and one Lucina item carrying `meta.transcript` and `meta.voiceReply`.
A failed transcription writes one "couldn't hear it" Lucina item (`replyTo` = the user item) instead. With
`LUCINA_PLANNER=tools` the turn is `tools_turn`, and its `on_tool` progress items (role lucina, `replyTo` = the user
item) are suppressed for share turns. The voice cases replace the album with that voice note: one initial state
with an on-device transcript and one without, where whisper (`ProcHear`) succeeds or fails, re-run after a crash;
the tools-planner case adds `ProcTool`. Transcript text and metadata are abstracted (Verus covers them).

Invariants (Stateright name / TLA+ name):

- `sh1_one_user_item` / `OneUserItem`: each accepted share has exactly one user item in the timeline.
- `sh2_answered_once` / `AnsweredOnce`: Lucina answers each share at most once (exactly once when it completes);
  "couldn't hear it" and tool progress items count as Lucina items.
- `sh3_retry_idempotent` / `RetryIdempotent`: a retry with the same id never adds a user item or an answer.
- `sh4_owner_only` / `OwnerOnly`: nothing is stored, ingested or answered for a request without owner auth.
- `sh5_one_bubble` / `OneBubble`: the chat shows each share once after reconciliation.
- `sh6_accepted_completes` / `AcceptedCompletes`: with nothing in flight, no accepted share is left processing.
- `sh7_voice_one_item` / `VoiceOneItem`: a voice note has at most one Lucina item, exactly one once done, whether
  the transcript came from the device, from whisper or nowhere (and, with the tools planner, whether a tool ran);
  it is "couldn't hear it" exactly when the run that answered had no transcript.

Witnesses (`sometimes` / `Share_fixed_*_witness.cfg`): a retry answered `duplicate`, a background-upload kill that
still delivers, a foreground kill that loses the share, a crash and recovery that completes, a two-file album with
one item and one answer, and a refused unauthenticated request. Voice: `voice_note_answered` /
`Share_fixed_voice_witness.cfg` (a voice note heard on the device or by whisper is done with one user item and one
Lucina item), `voice_device_answered`, `voice_server_answered`, `voice_unheard_answered` (the
`Share_fixed_voice_{device,server,unheard}_witness.cfg`) and `voice_tool_answered` /
`Share_fixed_voice_tools_witness.cfg` (answered after a tool ran, still one Lucina item).

TLC matches Stateright state for state: `Share_fixed_voice.cfg` 44 720, `Share_fixed_voice_tools.cfg` 62 000, and
the album, background and foreground cases keep 21 400, 58 514 and 306 346 (all under 10 s). The ablations
`Share_voice_noreply.cfg` (the "couldn't hear it" item without `replyTo`: written, crash before `done`, recovery
misses it and answers again) and `Share_voice_tool_items.cfg` (progress items not suppressed: the progress item
plus the answer are two Lucina items; after a crash the progress item alone would pass for the answer) MUST FAIL
`VoiceOneItem`. The code has neither: the "couldn't hear it" item carries `reply_to`, and `on_tool` returns early
when the turn carries `reply_meta`.

Verus (`share_kernels.rs`, 77 verified; `share_reconcile.rs`, 25 verified; no `assume`/`admit`/`external_body`):
- SK1 the router is total and returns one kind of `KINDS`; a decisive sniff wins over name and mime; zip containers
  route to document/sheet/slides/archive; text-like claims on non-text bytes fall to `other`; every timeline kind
  passes `normalize`'s whitelist and `take(32)`.
- SK1.6 (`voice_routing`, `timeline_kind_for_whitelisted`) the voice flag keeps `route_for` total and in `KINDS`;
  it changes the handler only for an audio-MIME MP4/Matroska container that sniff calls video (to `audio`); a part
  gets timeline kind `voice` exactly when it is flagged and routed audio; a flagged part that is not routed audio
  gets the timeline kind of the same upload without the flag (the flag is ignored); every result is whitelisted.
- SK1.7 (`receive_and_extract_agree`) `extract::read`, which re-routes with the flag read back from the stored
  kind, picks the handler `receive` picked, and exactly the parts stored as `voice` run the audio handler with
  subtype `voice`.
- SK2 one userItemId and at most one user item per shareId across crashes before or after the save, recovery and
  other writers; every 202 (fresh or duplicate) carries it; matching bubbles by the 202's userItemId OR
  `meta.shareId` gives one bubble per share in any order.
- SK3 the full-scan recovery answers at most once; the `raw.contains(id)` prefilter is sound. SK3.3
  (`voice_recovery_once`): the answer to an on-device or whisper transcript, "couldn't hear it" and a tool
  progress item are all role lucina with `replyTo` = the user item, so each is found after a crash (which is why
  progress items must not be written for a share turn).
- SK4 receive limits: ≤ 32 parts (the location check never fires: `meta` is itself a counted field), ≤ 2 GiB, the
  per-field caps, id/session/character limits, and the fixed `sanitize` (non-empty, ≤ 120 bytes, no separators,
  control characters or `..`).
- SK4.5 voice-note fields. `valid_waveform_exactly`: `valid_waveform` keeps exactly the (trimmed) strings that
  base64-decode (STANDARD, padding optional, trailing bits rejected, as base64 0.22.1) to 1..=64 bytes; the
  88-character pre-check never rejects one of them, so a stored waveform is 1..=88 characters for 1..=64 levels.
  `waveform_limits_witness`: 88 characters can decode to 66 bytes (both checks are needed); 86 unpadded and 88
  padded characters decode to exactly 64 and are kept. `voice_transcript_bounded`: a stored transcript (on the
  attachment or as `meta.transcript`) is non-empty, ≤ 4000 UTF-16 units (so ≤ 4000 characters) and a prefix of
  its input; the 4000-character clip of `receive`/`process` before it never cuts what the UTF-16 clip keeps.
Witnesses: `unfixed_window_scan`, `unfixed_match_by_id_only`, `unfixed_sanitize_keeps_dotdot`,
`waveform_limits_witness`. Differential test: `crates/lucina-server/tests/verus_share.rs` (branch `lucina-rs`) runs
the real `route`/`sniff`/`timeline_kind`/`normalize`/`sanitize` and the `/v1/share` limits against the kernels; on
branch `voice-notes` (0ce757c) it also runs the real `route_for`/`timeline_kind_for` over every header ×
extension × mime × flag (and extract's re-route), `valid_waveform` and the base64 engine against
`b64_decoded_len`/`valid_waveform` (every string of up to 4 symbols, 20 000 random strings, the 64/66-byte edges),
and the transcripts `normalize` stores against `clip16`.

Observations (no invariant broken): the `status.json` copy of a transcript is clipped to 4000 characters, the
timeline copy to 4000 UTF-16 units, so an emoji-heavy transcript is shorter on the timeline than in the status.
In the JSON planner a voice note whose transcript starts a background job gets the job's result as a later Lucina
item (`spawn_job`, `meta.jobId`), as a typed message does; the models count only the share pipeline's items.

| Action (model) | Code |
|---|---|
| `Send` / `Reshare` / `Kill` | iOS `ShareRequestBuilder.build` + `ShareUploads.enqueue`/`sendNow`, `ChatShares`; Android `ShareActivity` + `ShareJobService`; web `sendAttachmentMessage` |
| `Attack` / gateway | p1 Caddy `@share` (`forward_auth`, CSRF, `X-Share-Gateway`); `share::authenticate` |
| `Deliver`, `RxIngest`, `RxSave`, `RxFinish`, `RxAppendFails` | `share::receive` (`find`, `store_in_library`, `save`, `ensure_user`, `spawn`) |
| `ProcBegin`, `ProcRead` / `ProcHear`, `ProcTurn`, `ProcFail`, `ProcSave` | `share::process` (`ensure_user`, `existing_reply`, `extract::read` / `extract::audio`, the "couldn't hear it" branch, `chat::share_turn`), `spawn`'s error path |
| `ProcTool` | `chat::tools_turn`'s `on_tool` (returns early for a share turn: `reply_meta`) |
| `Crash`, `Restart`, `Recover` | lucina-rs restart; `share::recover` |
| `OtherAppend` | Node and other turns appending to `timeline.jsonl` |
| `TimelineDeliver` | the app's timeline stream/poll (iOS `Timeline.merge`, web `mergeItems`/`matchItem`) |

### 13. Lucina OTP forwarding

Lucina's phone (S20 FE, `tech.getsecretary.lucinaotp`) forwards one-time codes from SMS and, when allowed, from
notifications (`docs/lucina-android.md`). The Rust core parses first, dedups a code across sources for 120 s, writes
the code with a stable event id to its encrypted queue, and sends one `POST /v1/phone/otp/incoming` to the relay on
p1. The relay keeps codes in memory only (TTL 2 min, ≤ 8 pending, consume-once) and the seen event ids on disk.
The owner gets a code by `/otp` in the Secretary app (Rust Lucina) or private Telegram (Node Lucina); MCP callers and
other chats are refused.

The model (two codes, code 0 by SMS and notification, code 1 by SMS; TTL 2 ticks; queue and pending scaled to 2) has
phone reboots, lost requests and responses, relay restarts and a failing `seen.json` write, `/otp stop` and
`/otp arm`, several concurrent `/otp`, an MCP caller, timed windows and the self-test. Clocks agree within a tick.

Invariants (Stateright / TLA+): `at_most_once` / `AtMostOnce`, `never_after_expiry` / `NeverAfterExpiry` (from the
first time the phone sees the code), `owner_only` / `OwnerOnly`, `test_never_delivered`, `no_silent_loss` (a
received, unexpired, undelivered code that no explicit pause/unpaired/expired answer dropped is queued, in flight or
pending), `pending_bounded`. Witnesses: a deduplicated retry, an expiry, a delivery after a reboot, a delivery from
the notification.

Verus (121 verified, no `assume`/`admit`/`external_body`):
- `otp_extract.rs` (18): the result is one candidate (or Google's), has a digit, 4–10 characters, never all digits
  over 8, never after `+`, never in a URL, within 64 bytes of a context and the nearest one; no context → None;
  the fixed kernel never returns an amount or a date part.
- `otp_expiry.rs` (53): `expires_at`'s four bounds; the relay store (exec = spec state machine, inductive invariant)
  never returns a code at or after expiry, consumes it, keeps pending ≤ 8 and seen ≤ 64, never stores an id seen
  within the TTL, and arm never drops a fresh code; the phone queue POSTs only fresh codes and writes ahead.
- `otp_redact.rs` (33): merged spans cover exactly the union; every code `parse_otp` can select is covered in the
  original bytes; text outside spans is kept; identity without spans.
- `otp_owner.rs` (17): the exact `private_owner` truth table; `/otp` returns a code only to a private owner, never
  to an MCP caller.
Differential tests (`verification/verus/diff/otp_*.rs`, `diff/otp_run.sh`, default `REV=lucina-rs`): the kernels
against the real crates on 30 000 bodies/texts, 24 000 askers, 600 random relay runs and 40 queue scenarios.
Not covered: `restore_seen` after a restart (only the model covers it); the at-most-once lifetime proof assumes the
relay clock never goes back (the diff test also steps it back).

| Action (model) | Code |
|---|---|
| `Arrive` (SMS / notification), `Serve` | `SmsReceiver.kt`, `OtpNotificationListener.kt` → `Bridge::on_sms`/`on_notification` → `forward` (write-ahead, then `POST /incoming`) |
| `Crash`, `Flush` | process death or reboot; `Core.ensure` + `BootReceiver` + `RetryJobService` → `Bridge::flush` |
| `LoseRequest`, `LoseResponse` | `net.rs` (network error, 5xx → keep queued) |
| `PhoneGets`, `SeenWriteFails`, `RelayRestart` | `lucina-otp-relay/src/http.rs` (`/incoming`, `seen.json` write with rollback + 503), `Relay::open` |
| `OwnerAsks`, `LucinaStep` (take → status → arm), `OwnerStop`, `OwnerArm` | `lucina-server/src/otp.rs::secretary_chat` (Rust), `private-otp.js` + `otp-chat` (Node); relay `Store::{take,arm,disarm}` |
| `OtherAsks` | an MCP caller or a non-owner chat: `reply::private_owner`, `secretary_chat`'s MCP refusal |
| `SelfTest`, `Tick` | `selfTest(network=true)` (`test: true`, never stored); time |

### 14. Grocery deal watch

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
`NeverCutover` (Rust loads the file after Node stopped; in G3 both load), `digest_relists` / `NeverDigestRelists`.
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
| G0 | Rust | none | on | – |
| G1 | Rust | one | on | – |
| G1 + fixes | Rust | one | on | F1 F2 F3 |
| G2 | Node, `StopNode` at any step, `StartRust` | the stop | on | – |
| G2 + fixes | as G2 | the stop | on | F1 F2 F3 in both runtimes |
| G3 | Node and Rust at once | none | on | – |
| G4 | Rust | one | off | – |
| G5 | Rust | one | off | F1 F2 F3 |

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
the G0 price bounce); (d) matcher: a token without the term as a suffix never matches (`lemma_no_suffix_no_match`), a
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

**DW1, a send whose save is lost repeats offers** (G1, G2; `no_repeat`, `one_digest_per_day`,
`silent_when_unchanged`). `run` delivers, then acknowledges and saves. If the response is lost (`AppNotify` reports
`Failed` after the server stored the notification) or the process dies before the rename, the file does not know the
owner got the message. The server only suppresses an identical retry on the same date:

```text
G1 no_repeat (9):            Tick; Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost);
                             Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered)
G1 one_digest_per_day (9):   Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost); Edit(0,0);
                             Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered)
G1 silent_when_unchanged (9): as no_repeat (the alert's observation equals the digest's)
G2 one_digest_per_day (11):  Load(0,Digest); Poll(0,0); Decide(0); Send(0,Delivered); StopNode; Edit(0,0);
                             StartRust; Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered)
```

The first: the digest reached the owner, the alert at 09:00 does not see it acknowledged and sends the food-waste
offer again (the digest key and the alert key differ by kind). Code: `watch.rs::DealWatch::run` (deliver, then
`acknowledge` + `save`), `compose` (`dedup_key` from kind, date and delivered offers),
`notify.rs::AppNotify::deliver`. The cutover hits the same gap when Node is stopped between its send and its save
(G2). Fix F1; G1 + fixes and G2 + fixes hold, and G5 shows F1 needs the server dedup.

**DW2, price bounce** (G0, no fault; `no_repeat`). `acknowledge` overwrites the notified price with the current one,
also upwards (`WatchState::set_notified`). Notified at the low price, then a later delivered message acknowledges the
high price (the next day's digest), then the price drops back and is alerted again:

```text
G0 no_repeat (20): Tick; Tick; Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Tick; Tick;
                   Edit(0,2); Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Edit(0,1);
                   Load(1,Alert); Poll(1,0); Decide(1); Send(1,Delivered)
```

Verus: `lemma_overwrite_forgets_cheaper`; real code: `price_bounce_alerts_a_received_price_again`. Fix F3.

**DW3, an alternative is acknowledged without being shown** (G0; `no_skip`). Messages show each item's best offer
only, but `acknowledge` records every active offer (`for item in &prepared.active { for deal in &item.deals …`;
Node's comment: "Their alternatives are already known"). When the best offer goes away, the alternative is the best
offer, active across alert ticks, never shown, and every alert skips it until the next digest:

```text
G0 no_skip (11): Load(1,Digest); Poll(1,0); Decide(1); Send(1,Delivered); Save(1); Edit(0,0); Tick; Tick;
                 Load(1,Alert); Poll(1,0); Decide(1)
```

Real code: `alternative_acknowledged_without_delivery`. This is deliberate in Node and Rust, but it breaks `no_skip`
as stated. Fix F2.

**G3, two watchers** (rule witness, like P3/P4): stale snapshots and lost updates on the shared file give two digests
on one date without any fault (16 actions). Never run both; the performed cutover stopped Node first.

**G4/G5, the dedup key's role.** Without server dedup the identical retry after a lost response is a second digest
(G4, 8 actions: `Load(1,Digest); Poll(1,0); Decide(1); Send(1,Lost); Load(1,Digest); Poll(1,0); Decide(1);
Send(1,Delivered)`); with dedup the same retry is suppressed and the G1 trace needs a changed batch. F1 relies on
dedup: without it the pending re-send repeats (G5, 7 actions).

**Key domain.** `sources.rs::food_waste` joins raw upstream strings, so `fw:` keys are injective only for store ids
without `:` (above); `relatedKeys` grouping and `deal_keys` keep every member, so acknowledging a group covers all of
its branches.

### 15. In-app maps

A place appears as a map card everywhere (contract: the `place` block of `docs/lucina-message-format.md`, a
proactive `meta.place`). iOS uses MapKit; the web app and the Android WebView use MapLibre GL with the self-hosted
Protomaps extract from `secretary-maps` on nezopt (`:8840`, behind p1 Caddy `/v1/maps/*` with forward_auth and
`X-Maps-Upstream-Key`; `crates/secretary-maps/README.md`). There is no multi-process protocol to model: the server is
stateless apart from Places results (memory + atomic JSON snapshot, 5000 entries / 30 days) and
per-mode route results (memory, drive/transit 5 min, walk/bike 24 h), so there is
no Stateright model or TLA+ twin. The decisions every client depends on are pure functions, proved in Verus on
integer coordinates in degrees × 10^7 (e7).

Verus (`map_kernels.rs`; no `assume`/`admit`/`external_body`), each mirroring `crates/secretary-maps/src/kernel.rs`:
- MK1 `coord_valid` is exactly the range predicate: |lat| ≤ 90e7, |lng| ≤ 180e7, not (0, 0) (`parse_coord` rounds a
  finite f64 pair to e7, then applies it).
- MK2 `fit_bounds` over valid points: `None` iff empty; otherwise min ≤ max on both axes, every point inside, every
  bound attained by a point, and the bounds in range. The antimeridian is not special (a set straddling ±180° gets
  a box around the globe); the maps cover Denmark.
- MK3 `safe_asset_path` equals its byte-level spec (1..=512 bytes, no leading/trailing `/`, no empty, `.` or `..`
  segment, no `..` at all, no `\`, `%`, NUL or other control byte); `lemma_safe_rejects_traversal`: every path with
  a `..` segment or a leading `/` is refused. The server also canonicalizes and checks the result stays in
  `MAPS_DIR/assets`.
- MK4 `travel_minutes` (the web "≈" estimate: walk 4.8, bike 15, car 35 km/h, × 1.3 detour, so
  `ceil(78 d / speed)` minutes for `d` whole metres, clamped to 1e8 m) is at least 1, overflow-free and monotone
  non-decreasing in the distance for each mode; `travel_estimate_min` rounds the f64 distance up to whole metres.
  The server's no-provider fallback (`/v1/maps/routes?…&estimate=1`, `osrm::estimate`) calls the same
  `travel_estimate_min` over `distance_m`, so it inherits MK4 unchanged.
- MK5 the normalizer's keep/drop decision `place_keep`: kept iff the title is non-empty and (the address is
  non-empty or both coordinates are present and valid); a kept block carries both coordinates or neither, and they
  are the input pair. Examples: title only is dropped; one coordinate with an address keeps the block without
  coordinates; (0, 0) never counts.

- MK6 `simplify`: an increasing index witness proves the result is a subsequence of the input, length ≤ max,
  and first/last points retained when max ≥ 2 and the input is nonempty. max < 2 yields empty (two endpoints
  cannot fit). Adaptive stride ceil(remaining/slots) skips points without changing their order.
- MK7 `leave_at`: computes arrival milliseconds − (duration seconds + buffer seconds) × 1000, saturated at
  i64::MIN; no overflow and result ≤ arrival. Production uses the same i128 intermediate.

Boundaries: Google polyline precision-5 decoding (10 000 malformed/random UTF-8 inputs tested for panic freedom),
provider JSON and RFC3339 timestamps, cache rounding. MK6/MK7 have fixed-seed differential/property tests.
Other boundaries: f64 → e7 rounding, the haversine distance, string cleaning (one line, control characters, clipping
with "…") and JSON. `crates/secretary-maps/tests/kernel_model.rs` (part of `cargo test -p secretary-maps`) runs the
real functions against a plain-Rust copy of the specs on fixed-seed pseudo-random inputs biased to the boundaries:
40 000 coordinates and `parse_coord` pairs, 5 000 point sets, 20 000 paths over an adversarial alphabet, 20 000
distances (with monotonicity), 20 000 keep decisions and 5 000 JSON blocks through `normalize_place`.

```sh
verify-gate -- ~/tools/verus/verus-x86-linux/verus verification/verus/map_kernels.rs   # 0 errors
cargo test -p secretary-maps
```

| Kernel (Verus) | Code |
|---|---|
| `coord_valid` | `kernel::coord_valid`, `parse_coord`; `Config::from_env` (home), `geocode::google`, route endpoints, the geocode bias |
| `fit_bounds` | `kernel::fit_bounds` (the clients' fit-to-pins) |
| `simplify` | `kernel::simplify` → route polylines (Google, OSRM), ≤400 points; step polylines, ≤100 points |
| `leave_at` | `kernel::leave_at` → route `leaveAt`, 300 s buffer |
| `safe_asset_path` | `kernel::safe_asset_path` → `resolve_asset` → `GET /v1/maps/assets/{*path}` |
| `travel_minutes` | `kernel::travel_minutes`, `travel_estimate_min` (with `distance_m`) |
| `coord_pair`, `place_keep` | `kernel::coord_pair`, `place_keep` inside `normalize_place` |

### 16. Meetings

The owner's Mac uploads a meeting (Secretary Code's recorder, `MEETINGS_API.md`) and `lucina-memo` turns it into a
diarized transcript, a brief, an archive folder on the MyBook, a note, a memory entry and a notification
(`docs/meetings.md`). The Mac creates the meeting (a lost response is retried with the same `Idempotency-Key`), asks
for the stored offset and sends chunks; the network reorders, duplicates and cuts requests; the store appends by MT2,
checks the SHA-256 at the declared size and queues on `complete` by MT3. The worker transcribes under the GPU lease,
diarizes, analyses and briefs (the model may fail; on the last attempt the deterministic versions), writes the folder,
the note, the memory entry, the notification and the voice question (each side effect after its marker, MT6/MT8),
and finishes by MT4; it may crash anywhere, another job may hold the GPU, the owner may name a speaker afterwards.

Invariants (Stateright name / TLA+ name):

- `stored_is_prefix` / `StoredIsPrefix`: the stored bytes are always a prefix of the Mac's file.
- `one_meeting_per_recording` / `OneMeetingPerRecording`: a retried create returns the same meeting.
- `queued_only_when_whole` / `QueuedOnlyWhenWhole`: the pipeline starts only on whole, verified tracks.
- `status_transitions_allowed` / `StatusTransitionsAllowed`: every status change is an MT1 transition.
- `telegram_at_most_once`, `memory_at_most_once`, `voice_asked_at_most_once` (and the TLA+ names): one Telegram
  message, one memory entry, one "who is this voice?" question, whatever the crashes.
- `done_has_every_output` / `DoneHasEveryOutput`: `done` only with every output.
- `finished_with_brief_note_and_notification` (eventually, Stateright).
- Witnesses: `resumed_after_a_cut`, `fallback_brief_done`, `gpu_busy_then_done` (TLA+: the `Meetings_witness_*` cfgs).

Each fix switched off is caught: append at any offset (`Meetings_offset_bug.cfg`), create without the key
(`Meetings_idempotency_bug.cfg`), complete without checking the tracks (`Meetings_complete_bug.cfg`), side effects
before their markers (`Meetings_marker_bug.cfg`). The Stateright cases and TLC explore the same 18 605 states.

Verus (`meeting_kernel.rs`, 24 verified): MT1 `status_allowed`; MT2 `chunk` (append only at the stored offset, never
past the declared size, never to a whole track) with `lemma_prefix_after_appends` (over any sequence of requests,
cut-off bodies included, the stored length stays within the size); MT3 `may_complete`; MT4 `ready_to_finish`; MT5
`best_match` (reaches the threshold, the highest, the first of equals; None only when nothing reaches it); MT6
`should_ask` with `lemma_ask_at_most_once`; MT7 `speaker_for`; MT8 `send_once` with `lemma_send_at_most_once`.
Mutations checked: MT2 without the offset check and MT8 always sending both fail. Differential test
`crates/lucina-memo/tests/verus_meeting_kernel.rs` (every small input); `MeetingStore::update` refuses any status
change MT1 does not allow.

| Model action | Code |
|---|---|
| `Create`, `RetryCreate` | `uploader.ts` `uploadRecording` (`Idempotency-Key`) → `meetings::api::create` → `MeetingStore::create` |
| `Query`, `Send` | `uploader.ts` `uploadTrack` (`bytes */N`, then chunks at the server's offset) |
| `Deliver`, `Duplicate`, `Cut` | `meetings::api::upload` → `MeetingStore::put` (`kernel::chunk`, kept bytes of a broken body, SHA-256 at the size, reset on mismatch) |
| `Complete` | `meetings::api::complete` → `MeetingStore::complete` (`kernel::may_complete`) |
| `WorkerStep` transcribe / `FinishStep` | `meetings::worker::steps` step 1 (`Stt::transcribe_words`, the `gpu-lease` shim) |
| `WorkerStep` diarize, analyze | steps 2 and 3 (`diarize`, `analyze::analyze`, `brief::write`, fallbacks on the last attempt) |
| `LlmFails` | `process` → `StepError::Failed` (attempt counted, back to `queued`) |
| `WorkerStep` publish | step 4: `write_outputs`, `publish::note`, `publish::remember` (`memory_rev` first), `publish::notify` (`notify_telegram_rev` first), `publish::ask_unknown_voices` (`VoiceBook::mark_asked` first) |
| `WorkerStep` done | `kernel::ready_to_finish`, `status = done` |
| `WorkerCrash`, `OtherTakesGpu`, `PruneDeadLease` | process death; other `gpu-lease` jobs; `gpu-lease` prunes dead pids |
| `OwnerNamesSpeaker` | `POST /v1/meetings/{id}/speakers` (`rename_in_meeting`: files and note again, no new notification) |

### 17. Node retirement: Rust owns Telegram, Discord and reminders

The Node Lucina (`lucina-nez`) is removed one capability at a time, with **no Node code change**: the phase-2
"Node shim" of PORT_PLAN (P2a's fixed order) is dropped. Code checked: branch `lucina-rs` (merged from `node-retire`) at 3c22447
(`lucina-telegram/src/{outcome,lease_decision,lease,lib}.rs`, `lucina-store/src/reminders.rs`,
`lucina-server/src/reminder_delivery.rs`) and at f26eb90 (`lucina-telegram/src/{claim,poller}.rs`: the batch's
claim is written before any handler, `should_handle(id, claimed) = id > claimed`, `claim = max`, every
`getUpdates` sends `next_offset(claimed) = claimed + 1`; `lucina-server/src/delivery_monitor.rs`); Node
`Lucina/src/gateway-lock.js` and `src/index.js` (68b38dc) with telegraf 4.16.3. The Discord message session's
`discord-<appid>.flock` (`lucina-discord/src/lease.rs`, 290b946) landed after the model was written; R5 models its
design, which the code follows (the flock alone decides). The handover was done by hand (see below). Every model has one Rust crash (the watchdog restart) at any step; files and Telegram's queue
survive it, volatile work and flocks do not.

Invariants:

- **R1 reminders handover:** `single_reminders_writer` (Node enabled, Rust `writer=local` and up never at once),
  `no_lost_reminder_record`, `no_double_delivery`, `no_silent_loss` (at the drained end every reminder is
  delivered, `failed` or `cancelled`); witnesses `handed_over`, `disabled_visible` (a create while nobody owns
  the file fails visibly), `rolled_back`. One reminder due at the start, one app/Telegram create or cancel.
- **R2 delivery outcome:** `no_double_delivery` (Telegram accepts the message at most once), `no_silent_loss`
  (eventually `sent`, or `failed` and visible); witnesses `delivered`, `uncertain_failed`. The network refuses,
  answers an error envelope (429/5xx), replies `ok`, loses the body after 200 headers, times out after delivering,
  or times out after losing the request. The outcome is computed by the mirrors of `classify` and
  `BotApi::call` / `transport_error` / `reminder_delivery.rs` (`Network::outcome`); one transient fault, then a
  healthy send.
- **R3 Telegram single poller:** `single_poller` (at most one process inside a `getUpdates` window, counted before
  Telegram's 409 ends one); witnesses `rust_polls`, `node_deferred`, `rust_takeover` (Rust acquires over its own
  instance's live record after a crash), `rust_refused` (an unusable lock dir stops Rust), `node_polls_after_rust`.
  Processes: Rust A, a stray Rust B (same `HERMES_GATEWAY_INSTANCE`), a Node container that may start at any time.
  Ages count 30 s heartbeats; 3 means older than 90 s. The passing case assumes **A1** no process stalls more than
  250 ms between reading the record and writing its own, **A2** a polling holder's heartbeat never lapses for 90 s,
  **A3** Node is not started while the lock dir is unusable. Each assumption dropped is a `MUST FAIL` case (NR1).
- **R4 update at most once:** `no_double_reply`, `no_silent_skip` (an update nobody handled that Telegram will
  not deliver again is in the log); witnesses `all_handled`, `crash_skip_logged`, `node_then_rust`. Two queued
  updates; Node (telegraf) polls first and is stopped, then Rust starts with an empty claim file.
- **R5 Discord single gateway:** `single_discord_gateway` (Node's session plus both Rust processes); witnesses
  `rust_connects`, `rolled_back`. This adds the second Rust process and its flock to protocol 2's P3.

The order the passing cases check, which the cutover and rollback scripts must follow:

1. **Reminders.** Restart Node with `LUCINA_REMINDERS_ENABLED=false` only while its tick and its reminder requests
   are idle (else the batch it sent but did not commit is sent again by Rust). Then restart Rust with
   `LUCINA_REMINDERS_WRITER=local`, `LUCINA_SCHEDULER=true`. Between the two nobody writes `reminders.json`; due
   reminders wait as `pending`. Rollback: Rust back to `writer=node`, scheduler off, **only when its tick is idle
   and no reminder is `sending`** (NR3), then Node re-enabled.
2. **Telegram.** Stop Node's Telegram polling only while its handlers are idle (NR5). Rust's first `getUpdates`
   after each lease acquisition sends no offset (NR4, fixed in 5c87ae0), and Rust never sets `drop_pending_updates`. Keep Node's Telegram token
   out of its environment while Rust polls, so a stray `docker start` cannot poll at all (NR1); the record is a
   second line of defence. Rollback: turn Rust's poller off in its config (the watchdog restarts a stopped
   service), then give Node its token back; Node drops what was queued in between (NR6).
3. **Discord.** Restart Node with an empty Discord token, wait until its gateway session is gone, then enable Rust;
   each Rust process holds `discord-<appid>.flock` for its message session. Rollback: Rust's Discord off first.

**Performed on 2026-10-27, in this order:** reminders at 08:24Z (Node's tick idle), Telegram at 12:40Z (Node idle,
no unconfirmed updates), Bot Hospital at 12:41Z, Discord at 12:43Z; the Node container `lucina-nez` was removed at
12:49Z, image included. Since the Node app can no longer be started, the rules behind NR1, NR5 and NR6 are moot:
they keep their witnesses as the record of why the order mattered, and apply again only if a Node gateway comes
back. The R1 rollback rule (NR3) and the update claim (NR4) still describe the Rust code. Bot Hospital's
`last-audit.json` (2ab3ddc, one audit a day across restarts) needs no model.

| Model action / kernel | Code |
|---|---|
| R1 `Operator` | the handover/rollback steps above (`deploy/lucina-rs/*` on `lucina-rs`; performed by hand on 2026-10-27): Node `LUCINA_REMINDERS_ENABLED`, Rust `LUCINA_REMINDERS_WRITER`, `LUCINA_SCHEDULER` (`lucina-server/src/{config,capabilities}.rs`) |
| R1 `CreateApp`, `CancelApp`, `CreateTelegram`, `CancelTelegram` | Rust `ReminderStore::{create,cancel}` (mutex + flock) once local, forwarded to Node before; Node `createReminder` / cancel; "reminders disabled" when off |
| R1 `Read(1)`, `Send(1)`, `Save(1)`; R2 `Claim`, `Save`, `Recover` | `reminders.rs::process_due`: mutex + `reminders.json.lock` flock for the whole tick, `fail_stale_claims` (`status_at_tick_start`), claim `sending` and write, send each, one write of the outcomes |
| R1 `Read(0)`, `Send(0)`, `Save(0)` | Node `processDueReminders` (snapshot, send, commit; P2b) |
| R2 `Send(Network)`; `classify`, `call_outcome`, `delivery_outcome`, `claimed_next` | `lucina-telegram/src/outcome.rs::classify`, `lib.rs::BotApi::{call,transport_error,send_text}`, `lucina-server/src/reminder_delivery.rs` |
| R3 `Check`, `Write`, `ReadBack` (Rust) | `lease.rs::Lease::acquire_in`: `create_dir_all`, `try_lock_exclusive` on `telegram-<bot>.flock`, `read_record` + `lease_decision`, `write_record`, 250 ms, read back |
| R3 `Heartbeat` (Rust) | `lease.rs` heartbeat task (30 s): a live record of another holder or a failed write sets `lost`; the poller stops (README contract) |
| R3 `Check`, `Write`, `ReadBack`, `Heartbeat` (Node) | `gateway-lock.js::acquireGatewayLock` (degraded on any exception), its `setInterval` heartbeat (`onLost` stops the bot; a failed write only warns) |
| R3 `Conflict` | Telegram's 409 "terminated by other getUpdates request" |
| R3 `Crash`, `Restart` | watchdog restart: the kernel drops the flock, the record stays (`Held::drop` only runs on a clean stop) |
| R4 `NodeReceive`, `NodeHandle`, `NodeSync`, `NodeStop` | telegraf `Polling` (offset = last + 1 on the next `getUpdates`, `syncUpdateOffset` on stop); `index.js::shutdown` |
| R4 `Receive`, `Handle`, `Advance`, `RustStart`, `Restart`; `should_handle`, `claim`, `next_offset`, `poll_offset`, `plan_batch` | `lucina-telegram/src/claim.rs` (`ClaimFile::store`: temp file, fsync, rename) and `poller.rs::{plan_batch, Poller::poll}`: claim before dispatch, the first `getUpdates` after each lease acquisition with offset 0, later ones with `next_offset(claimed)`, skip + warning + `skippedReplayed` at or below the claim; the code is `start_from_claim = false` since 5c87ae0 (true before) |
| R4 `Rollback` | `index.js::launchManagedTelegramPolling`: `deleteWebhook({ drop_pending_updates: true })` after the lock |
| R5 `Operator`, `Connect` | Node Discord token emptied; `lucina-discord/src/lease.rs`: message session behind an exclusive non-blocking `discord-<appid>.flock` (290b946) |

Kernels (`retirement_kernels.rs`, 32 verified): `classify` with Uncertain exactly for (Sent, no status, no API
answer), equal to the design's "Sent and no status" on the transport's inputs (C1); Delivered only with
`api_ok == Some(true)`, never for the transport's inputs, and with a 2xx status only under a consistent response
(C2, NR2); `call_outcome` never NotDelivered when Telegram may have the message and never Uncertain after a
complete answer (C3); `claimed_next` and `status_at_tick_start` (S1, S2); over any legal history a sent or failed
reminder never changes again, so a delivered or uncertain send is never resent (S3); `lease_decision` defers
exactly for a live record of another holder that is not our own previous run, acquires for no record, a stale or
unparseable heartbeat, another boot, our holder or our previous run (L1); the same instance id never defers (L2:
only the flock separates Rust A from B); Rust and Node defer to each other's live record (L3); claims are monotone
and an update is handled at most once over any re-delivery and crash history (U1, U2); `next_offset(c) = c + 1`
and `poll_offset` sends 0 on the first poll after a lease acquisition (U1, NR4) and `plan_batch` hands out only ids above the old claim, at most the new one, strictly increasing (U3).
`delivery_monitor.rs` (10 verified) mirrors `delivery_monitor.rs::step`: an up check resets and sends Recovered only
after an alerted outage (M1, M4); Down only once the outage and the process have lasted `after_ms` (the start-up
grace, M2), at most once per outage (M3), and never missed once due (M5), for any i64 times. Before 5c87ae0
`step` used plain subtraction, and a corrupt `downSince` near `i64::MIN` could wrap in release builds and keep that
outage from being alerted; it now uses `saturating_sub`, mirrored by `sat_sub`.

Default Stateright results (all 21 cases meet their expectations; the TLC twins have the same state counts):

| Case | States | Depth | Result |
|---|---:|---:|---|
| R1 idle handover + drained Rust rollback | 1 229 | 19 | all hold |
| R1 early local writer witness | 4 186 | 11 | all four **violated** |
| R1 mid-tick Node restart witness | 2 610 | 20 | `no_double_delivery` **violated** |
| R1 rollback without drain (NR3) | 1 648 | 19 | `no_silent_loss` **violated**; the others hold |
| R2 three-way outcomes (classify + transport) | 46 | 9 | all hold |
| R2 uncertain retried witness | 55 | 9 | `no_double_delivery` **violated** |
| R2 not-delivered terminal ablation | 36 | 6 | all hold; `retryable_failed` reached (a retryable send is given up) |
| R3 flock + record + read-back (A1–A3) | 1 250 | 12 | holds |
| R3 stall inside the rendezvous (NR1a) | 2 390 | 8 | `single_poller` **violated** |
| R3 heartbeat lapse while polling (NR1b) | 3 077 | 14 | `single_poller` **violated** |
| R3 lock dir lost, Node starts degraded (NR1c) | 2 054 | 9 | `single_poller` **violated** |
| R3 record only, no flock witness | 2 142 | 8 | `single_poller` **violated** |
| R3 Rust starts degraded witness | 2 698 | 15 | `single_poller` **violated** |
| R4 code: claim first, no-offset restart (5c87ae0) | 70 | 12 | all hold |
| R4 witness: restart at claimed+1 (NR4, pre-5c87ae0) | 52 | 11 | `no_silent_skip` **violated** |
| R4 offset-only witness | 66 | 13 | `no_double_reply` **violated** |
| R4 Node stopped mid-handler (NR5) | 188 | 14 | `no_double_reply` **violated** |
| R4 rollback drops pending updates (NR6) | 96 | 13 | `no_silent_skip` **violated** |
| R5 ordered Discord + flock | 26 | 7 | holds |
| R5 early Discord witness | 32 | 7 | `single_discord_gateway` **violated** |
| R5 second Rust without flock witness | 28 | 7 | `single_discord_gateway` **violated** |

TLC (`formal/retirement/`): `Reminders` 1 229, `Reminders_rollback_safety` 1 648 (NR3 keeps the safety invariants),
`Delivery` 46 and `Delivery_fail_known` 36 (both with `<>Finished` under weak fairness), `Telegram` 1 250,
`Updates` 70, `Discord` 26 distinct states, each under 2 s; 18 `*_witness.cfg` and 15 `*_bug.cfg` fail as required.
### 18. Lucina Drive: the folder index

The Drive (`docs/media-library.md`, "The Drive") indexes every regular file of the Drive folders
(`LUCINA_MEDIA_FOLDERS` minus the film folders) in place. Every 30 minutes a scan walks a folder
(`drive::index_drive_folder`: junk and symlinks skipped, an unchanged file costs one lookup, a new or changed one is
hashed and registered with its path), then trashes, in the catalog only, the rows of files it did not see. The big
disk can be unmounted at any time; an unmounted mountpoint is an empty directory on the NVMe's filesystem, where
anything can write a stray file. A phone can upload the same bytes as a Drive file.

Invariants (Stateright name / TLA+ name):

- `unmount_never_trashes` / `UnmountNeverTrashes`: a scan never trashes the row of a file that stayed on the disk
  through the scan (an unmount, an empty mountpoint or a stray file in it never empties the Drive).
- `phone_original_kept` / `PhoneOriginalKept`: a phone's upload always has its bytes: owned by the library, or still in
  the Drive file whose bytes they matched.
- `owned_never_downgraded` / `OwnedNeverDowngraded`: once the library owns a copy of some bytes it keeps owning it.

Witnesses (`sometimes` / `DriveIndex_witness_*.cfg`): `pruned_a_deleted_file`, `stray_file_seen`, `walk_error`,
`dedupe_upgraded`.

Verus (`drive_kernels.rs`, the same bodies the server compiles from `drive_kernel_shared.rs`):
DK1 `segment_ok` accepts only Normal segments, and a root joined with Normal segments never climbs out of it, while
one `..` can (`safe_rel` / `join_under` split on `/` and require every segment to pass, so `browse?path=`, catalog
paths and the walk's paths all stay inside the folder); DK2 `scope_of` / `in_scope` (its `phone` input is "came from a device" since 2026-10-27: `db::FROM_DEVICE`, a phone source or a `device` set) partition the library into
gallery, films and drive (`db::scope_condition` is that predicate in SQL; `tests/drive.rs::scope_sql_matches_the_kernel`
checks all four combinations through the API); DK3 `blob_action`; DK4 `may_prune` / `may_adopt_fs`; DK5
`drive_may_work` (Tctl at or above `LUCINA_MEDIA_DRIVE_MAX_TEMP`, 90 °C, or the load limit pauses the hashing, text
extraction, PDF pages and Office conversion). Verus on nezopt (2026-10-26, through `verify-gate`): 15 verified, 0 errors.

| Model action | Code |
|---|---|
| `StartScan` | `drive::index_drive_folder`: `root.is_dir()`, the folder's `st_dev` against `drive_folders.dev` (`same_fs`) |
| `Visit` | the `walkdir` loop: `entry.metadata()` (an error counts in `errors`), `index_one` → `ingest::register_external` + the `drive_files` row |
| `Finish` | `may_adopt_fs` → `drive_folders.dev`; `may_prune` → `drive::prune` (items `trashed_at`, `drive_files` and `drive_fts` rows; never a file) |
| `Unmount`, `Mount`, `StrayWrite` | mybook unmounted or back; something writing into `~/mnt/mybook/…` while it is away |
| `Delete`, `Edit` | the owner deletes or edits a Drive file (an edit is a new mtime: re-hashed at the next visit) |
| `PhoneUpload` | `ingest::ingest_file` → `keep_owned_bytes` (DK3) under the catalog lock |

## Bugs found

Severity reflects today's deployment. **Fixed** means fixed on branch `verify-fixes` (from `main`, commit 1e19f27),
with regression tests. The other bugs are in code on branches other people work on, in uncommitted work, or in
plans, so they are reported here with the proposed fix and not changed.

### D1: a deploy from a clean main removed live code (high; production regression 25 Oct 2026). **Fixed** (procedure + `deploy-guard`)

On 24 Oct secretary-server and the web were deployed from the main checkout's working tree, which held uncommitted
work: `/v1/hermes`, the calls module and the Desk web view went live without being on any branch. On 25 Oct the
web-design merge kept the Desk work by rebuilding from the same tree, but at 13:52 UTC Lucina Ink deployed
secretary-server from a clean build of main (6b59881), which had no calls module: `/v1/calls/capabilities` and
`/v1/calls/transcribe` went from 401 to 404, and the Android Phone card lost its backend. Every step was done under
the merge lock, so the lock alone did not prevent it. The model finds it as D1's shortest `no_rollback`
counterexample (16 steps): session 0 merges and deploys a build of the dirty checkout, session 1 merges and deploys a
clean build of main.

Fix: the live work was committed (main 7298be7 for `/v1/hermes` and the web, b7388d0 for calls) and production was
redeployed from main at 14:16 UTC; `deploy-guard check` now refuses a deploy whose commit does not contain the
live one, a dirty tree, or a commit not on the target branch, and the procedure in the policy makes it mandatory.

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

Fixed order, modeled and holding (with a Node shim; superseded by protocol 17, which needs no Node change):

1. Rust opens only `/api/internal/reminders`. Its secretary path still forwards to Node, and nothing calls the
   internal endpoint yet.
2. Node restarts with the shim on and its scheduler off, after draining its tick.
3. Rust writes locally and starts its scheduler.

Rollback: Rust stops writing first (scheduler off, forwarding, internal endpoint closed; Node's shim calls fail
visibly for a few seconds), then Node takes the file and the scheduler back.

The shim-free order (Node restarted with `LUCINA_REMINDERS_ENABLED=false` while idle, then Rust `writer=local`) is
protocol 17, R1; its rollback must also resolve Rust's `sending` claims (NR3).

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
`no_double_delivery` and `no_silent_loss` then both hold. The claim is in the Rust code (`process_due`, branch
`lucina-rs`); protocol 17 checks it with the three-way delivery outcome (R2) and the shim-free handover (R1).

### P3/P4: handovers (no bug)

The documented order holds. Two witnesses document the rules: starting Rust on Discord before Node lost its token,
and running with `HERMES_GATEWAY_LOCK=false`, each give two consumers. The model treats lock acquisition as one
step; protocol 17 (R3, R5) splits Node's rendezvous, adds Rust's flock, a second Rust process, heartbeat expiry and
an unusable lock dir, and finds NR1.

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

### Pk1: the clock thread advances a frame while a macro holds a key, also while paused (low-medium). **Fixed.**

The clock thread read `busy` and then took the emulator lock. In between, a macro could take `busy` and press a key
(`with(..)` has no pause check), so the clock advanced a frame with the macro's key down. While paused, every macro
call of a Hermes turn still running presses a key before its first `frames(..)` check, so each call could leak one
input frame: the pause did not hold within one step. Shortest trace: `ClockRead`, `MacroStart(0)`, `Press(0)`,
`ClockFrame`. Fix (branch `lucina-game-link`): the clock thread takes the lock first and reads `busy` under it.

### Pk2: `/control` and `/save` save while a macro holds a key (low). **Fixed.**

A pause from chat or the app during a macro wrote the save state with the macro's key down; after a restart the key
came back pressed. Trace: `MacroStart(0)`, `Press(0)`, `Control(Chat, false)`. Fix: both save only when no macro holds
`busy` (the macro saves when it ends, and a pause ends it at its next frame); `/save` answers `saved: false` then.

### Pk3: a story step claimed more than its check (low). **Fixed.**

Writing the requirement table showed that "Get HM03 Surf and HM04 Strength" was marked done by the Surf flag
alone, so the plan never sent Lucina for Strength, which Victory Road needs. It is now "Get HM03 Surf", with a
separate "Get HM04 STRENGTH" step (EVENT_GOT_HM04) before the Elite Four.
### M1: a restart between ingest and the "finished" mark stalled the backup for good (medium). **Fixed.**

Found live (the backup stopped at one asset; every retried finish was a 500), reproduced by the
model (`M before fixes`, `no_dead_end_finish`):

```
Request(0, Ok) x3      device 0: create, put, finish (decide: new)
FinishStep(0)          insert: the spool file moved into the store, the item written
Restart                before `finished = 1`
Tick Request(0, Ok)    the resent finish: the upload is unfinished and its file is gone → 500, forever
```

Fixed in 8ee5623 (branch `media-library`): the ingest and the mark run in one blocking task, so a client hang-up
no longer drops the handler between them, and a finish without its file answers 410; the phone drops the upload
and sends the asset again, which dedupe makes cheap. A crash can still fall between the two steps, which is why
the 410 path matters: the model's `M current` case reaches `lost_upload_restarted` and keeps `no_dead_end_finish`.

### M2: the backup retried a 429 at once (medium). **Fixed.**

Found live (a 429 storm against p1's 60/min, which the gallery's tiles also spent), reproduced by the model
(`no_request_storm`: 5 requests in a window of 3). Fixed in 19b1e63: the backup stops on 429 and waits
`max(Retry-After ?? 60, 10)` s, pauses 120 s on gateway errors and paces 4 s per asset; renditions moved behind
play tokens so tiles no longer spend the device's requests. Verus (e) proves the decision and the 48-per-minute
budget.

### M3: two finishes of the same bytes could both create an item (low; found by the model). **Fixed.**

`ingest::ingest_file` reads `known_for` (by hash, by asset) under one catalog lock, then moves the file into the
store (`place_new`, a durable copy to the big disk: seconds for a video) and runs `insert_item` under the lock
again. Two finishes of the same bytes in that window both decide New. Shortest trace (`M before fix M3`):

```
Request(0, Ok) x3      device 0: create, put, finish photo 0 (decide: unknown bytes → New)
Request(1, Ok) x3      device 1: the same for its copy of photo 0 (decide: still unknown → New)
FinishStep(0)          insert item A
FinishStep(1)          insert item B, the same hash: the photo shows twice
```

The two finishes can come from two devices or from a phone upload racing a Lucina ingest (`/internal/v1/ingest`:
a Telegram or share-sheet attachment) of the same picture. There is no unique index on `items.hash` (an edit can
legitimately bring two items to the same bytes), so nothing catches it.

Fixed in cc5e663 (modelled as `M current`, all hold): `insert_new_or_duplicate` looks for an item with this hash
again in the same `Db::with` as the insert and turns the insert into a duplicate of that item when one appeared
meanwhile (recording the phone asset on it, as the Duplicate branch does). Regression tests in `ingest.rs`: the
decided-then-raced insert, and two threads ingesting the same new bytes 20 times, which fails 3 runs out of 3 on
the old code.

### DI1: a phone copy of bytes a Drive file held was dropped (low-medium). **Fixed.**

`ingest::ingest_file` removed the uploaded file whenever a `blobs` row existed for its hash, also when that row only
referenced a file in place (a folder file, a voice memo). The phone's item then depended on a Drive file the owner
can move, edit or delete; the phone itself never deletes, but the owner may, trusting the backup. The folder
indexer already saw the owner's computer photo backups, so this was reachable before the Drive indexed everything.
Shortest trace (`DI before DK3`): `Delete(0)`, `PhoneUpload` (the bytes are gone from the disk; the upload is dropped
because the row exists). Fixed by DK3 (`keep_owned_bytes`): an in-place row is upgraded to the phone's owned copy;
`tests/drive.rs::a_phone_copy_of_a_drive_file_is_owned_by_the_library`.

### DI2: an empty mountpoint with a stray file would have emptied the Drive (medium; found by the model before release). **Fixed.**

The first prune guard (an existing folder, a clean walk, at least one file seen) let a scan of the unmounted
mountpoint with one stray file in it trash every real row. Trace (`DI ablation: DK4 without the filesystem check`):
`Unmount`, `StartScan`, `Visit`, `Visit`, `StrayWrite`, `Visit`, `Finish`. Fixed before merge: each folder's `st_dev`
is recorded (`drive_folders`), pruning needs the same filesystem, and a new filesystem is adopted only by a clean walk
that sees every known file (`may_adopt_fs`); `drive.rs::prune_only_after_a_clean_full_scan`.

### DI3: a phone backup filed in a film album was in both the Gallery and Films (low). **Fixed.**

`scope=films` was "in a film album", without "not a phone backup", so the scopes overlapped. Now DK2 is a partition
(phone backups are the Gallery).
### B1: a crash after the vault copy, then a rename, makes a second note (low). **Fixed.**

The worker recorded the note's path only after the rsync. If it crashed in between and the owner renamed the memo
(allowed in any status), the next publish used the new title, so the vault got a second note for the memo:

```
Stop UploadOk WorkerStep×5 (… push) WorkerCrash OwnerRename WorkerStep WorkerStep -> 2 notes
```

Fix (lucina-rs): the path is recorded before the copy (`kernel::note_base`, Verus K7).

### B2: a reprocess while the GPU is busy replaces Whisper with the iPhone transcript (low). **Fixed.**

`reprocess --transcript` clears the step; with the GPU busy and an iPhone transcript on file, step 1 took the
iPhone's text over the Whisper transcript the memo already had:

```
Stop UploadOk SendDeviceTranscript WorkerStep×9 OtherTakesGpu OwnerReprocess WorkerStep
```

Fix (lucina-rs): `kernel::transcript_choice` waits for the GPU when a Whisper transcript exists (Verus K1).

### B3: a crash after the notification is sent sends Telegram again (low). **Fixed.**

`notified_at` was written after sending. The app inbox deduplicates (`dedupKey`), Telegram does not:

```
Stop UploadOk WorkerStep×7 (… send) WorkerCrash WorkerStep -> Telegram twice
```

Fix (lucina-rs): the revision is recorded before Telegram is called; a retry of that revision skips Telegram
(at most once; app inbox still exactly once) (Verus K6).

### B4: a recording cut off by iOS is lost (medium; one real recording lost on 2026-10-24). **Fixed.**

The iPhone recorded AAC in `.m4a`, whose index is written when the recording stops. When iOS killed the app
mid-recording, the file had no index (`AudioFileOpenURL failed` on the 72 KB orphan found on the iPhone) and was
never queued:

```
KillApp Relaunch -> no_lost_audio violated
```

Fix: the iPhone records ADTS AAC (`.aac`, self-framed: a cut-off file decodes up to its last frame) and on launch
`MemoUploadQueue.recoverOrphans` queues any recording not in the queue; the server remuxes ADTS to `.m4a` in step 0
(`transcribe::remux_adts`, re-encoding when the last frame is cut). Checked on the iPhone (iOS 27.0.1): SIGKILL after
6 s of recording, the relaunch recovered 6.2 s; and in `cut_off_adts_is_recovered` (a file cut at two thirds).

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
### SH1: recovery answered a share twice after 500 newer timeline items (medium). **Fixed.**

`existing_reply` looked for Lucina's reply only among the newest 500 items. A crash after the reply was written and
before `status.json` said done, followed by 500 items from other writers (Node, Telegram, proactive), made the
restarted worker answer again (Stateright SH window: `sh2`, `sh3`; Verus `unfixed_window_scan`). Fixed on
`lucina-rs` (share-server 4810c5a): every item is checked, with a `raw.contains(user item id)` prefilter before any
JSON parsing; regression test `recovery_finds_a_reply_far_behind_newer_items` fails on the old scan.

### SH2: a failed append left an accepted share stuck until a restart (medium). **Fixed.**

`receive` saved `status.json` as processing, then returned 500 when appending the user item failed (flock, I/O,
ENOSPC, a failed rotation) without spawning the worker. Every retry got `duplicate`/`received`; nothing ran until
lucina-rs restarted (Stateright SH stuck: `sh6`). Fixed (share-server 115222f): the error is logged and the worker is
spawned (it appends again); a live set also respawns a processing share whose worker is gone (error or panic) when
its retry arrives. Regression test `a_failed_append_still_processes_the_share`.

### SH3: the web chat showed a share twice when its item arrived before the 202 (low-medium). **Fixed.**

`matchItem` in `web/src/state/chat.ts` matched user items only by item id or `clientMessageId`, and a share's
server item has neither for the local bubble, and `parseMeta` dropped `meta.shareId`. A stream copy before the 202
was a second bubble until `adopt()`; with the 202 lost it stayed (Stateright SH reconcile: `sh5`; Verus
`unfixed_match_by_id_only`). Fixed on `main` (this change): `parseMeta` keeps `kind: "share"`, `shareId`, `intent`,
and `matchItem` matches by `meta.shareId` like iOS `Timeline.merge`; the new chatShare test fails without it.

### SH4: `sanitize` could produce `..` (low). **Fixed.**

When the 120-byte cut of a long file name ended right after a dot, `.ext` was appended to it
(`a*115 + "." + b*10 + ".jpg"` → `a*115 + "..jpg"`). Not a traversal (no separator, an `NN-` prefix), but it broke the
no-`..` rule (Verus SK4.4). Fixed (share-server cd23ecc): trailing dots are dropped before the extension.
Refuted while checking: a shared location cannot make a 33rd part (`meta` is one of the 32 counted fields); the added
`check_parts(parts.len() + 1)` is defence in depth.

Open, by design: a re-share after a lost 202 or a foreground kill (no App Group yet) is a new `shareId`, so it is
answered again; files moved into lucina-media by a request killed before `status.json` stay in the Shared album.

### OTP1: a phone reboot during the first send could lose a code or deliver it twice (medium). **Fixed.**

`Bridge::forward` saved the dedup entry and the queue entry only after `send` returned. A process death during the
send lost the code (`no_silent_loss`); the notification copy of the same code then got a new event id, and both were
delivered (`at_most_once`), the second possibly after expiry. Fixed (`ac9e7be`, in `lucina-rs` dd7ec9e): the queue
entry with its stable event id, then the dedup fingerprint, are written before any network call; only a network
error or 5xx keeps the entry for `flush`; `open()` rebuilds fingerprints from the queue; the Kotlin shell schedules a
flush at start-up when codes are queued (dc422ad).

### OTP2/OTP3: arming dropped codes the relay had already accepted (medium). **Fixed.**

`Store::arm` always ran `pending.clear()`. `/otp arm` while a code waited, or two `/otp` after a pause (both see "not
armed", one arms, a code arrives, the other's arm drops it), lost a code. Fixed (`60472d9`): arm keeps unexpired
pending codes and a live window's id; only disarm clears.

### OTP4: a relay restart forgot seen event ids, so a retry was delivered twice (low-medium). **Fixed.**

`Store.seen` lived in memory. A phone retry after a lost response, landing after a relay restart, was stored again
after the owner had taken the first copy. Fixed (`60472d9`): seen ids are persisted as SHA-256 hashes with their
expiry in `seen.json` (0600, atomic, before the 200); a failed write rolls the store back and answers 503 (retry).
Codes still never touch the relay's disk.

### X1: amounts and dates were returned as codes (medium). **Fixed.**

`parse_otp` returned `1500` for "Your verification fee is 1500 DKK." and `2026` for "Your login on 2026-10-25…"
(also "DKK 1500", "$1999", "EUR 1250,00"). Found by the Verus extraction kernel's differential test. Fixed
(`52b9140`): all-digit candidates next to a currency marker, or joined to another number by `. , : / -`, are skipped;
`294-728` and `294 728` still parse (Verus `otp_extract.rs`, fixed kernel; old behaviour kept as `parse_before_f4`).

### X2/X3: redaction missed codes `parse_otp` finds (medium; codes could reach logs or model text). **Fixed.**

`otp_spans` worked on the raw text, `parse_otp` on digit-normalised, URL-stripped text: a code in Arabic-Indic or
full-width digits (`٢٩٤٧٢٨`), or one more than 64 bytes from its context word because of a URL in between, was parsed
but not redacted. Fixed (`52b9140`): redaction also tokenises the normalised, stripped view and maps spans back to the
original bytes (a superset of the raw spans); Verus `otp_redact.rs` proves every selectable code is covered.

### DW1–DW3: the grocery deal watch repeats or skips offers (low; notification noise, lucina-rs)

Details and traces in §14. DW1: a send whose save is lost (lost response, crash, or the cutover's stop) lets the
next run repeat offers, because secretary-server only suppresses an identical message on the same date. DW2:
`acknowledge` raises a notified price, so a price that goes up and back down is alerted twice. DW3: alternatives are
acknowledged without being shown, so an alternative that becomes the best offer is not alerted until the next
digest. Proposed fixes (lucina-rs, `watch.rs`): F1 save the composed message and its dedup key before sending and
re-send it first after a restart; F2 acknowledge only the delivered best offers; F3 never raise a notified price. The
model holds with all three (G1 + fixes, G2 + fixes).

### NR1: a Node container started with its Telegram token can poll next to Rust (low-medium; design) **Moot since 2026-10-27** (Node removed)

The cutover followed the order in §17 (Node idle, Node's token gone), and on 2026-10-27 the
`lucina-nez` container and its image were removed, so no Node gateway can start. Kept as the record of the rule.

Rust's own side holds: the flock separates Rust A from a stray Rust B (the record cannot, L2: both have the
instance id `lucina-rs`, so each takes over the other's live record as "own previous run"), Rust reads its record
back after 250 ms like Node, stops when a heartbeat finds another live holder or cannot write, and refuses to start
on an unusable lock dir. Node's record-only rendezvous, unchanged, admits a second poller in three ways (R3, each
assumption A1–A3 dropped):

```
NR1a  Check(Rust A) Check(Node) Write(Rust A) ReadBack(Rust A) Write(Node) ReadBack(Node)   (Node stalls > 250 ms)
NR1b  Check(Rust A) ReadBack(Rust A) Tick Tick Tick Check(Node) ReadBack(Node)   (Rust's heartbeat lapses 90 s)
NR1c  Check(Rust A) ReadBack(Rust A) BreakDir Check(Node)   (lock dir unusable: Node starts degraded)
```

Both then consume updates until Rust's next heartbeat sees Node's record (a, b) or fails to write (c) and stops,
or Telegram's 409 ends one; a 409 cannot undo the overlap. Without a Node change the record is only a second line
of defence: after the Telegram handover, run Node without its Telegram token, as for Discord, so a stray
`docker start` cannot poll. Both runtimes must also use the same lock dir: on 2026-10-26 the Node container has
`HERMES_GATEWAY_LOCK_DIR=/run/hermes-gateway-locks`, a bind mount of the host's
`~/.local/state/hermes-gateway-locks`, which is Rust's default for `HOME=/home/nez`. A lucina-rs with another `HOME`
or `HERMES_GATEWAY_LOCK_DIR` never sees Node's record.

### NR2: `classify` lets the success bit win over the status (no impact; contract)

`classify(Sent, Some(500), Some(true)) == Delivered` and `classify(Connect, None, Some(true)) == Delivered`, so the
design's "Delivered only with `ok: true` and a 2xx status" is false for all inputs; it holds under a consistent
response (C2). Uncertain also needs `api_ok == None`, not only "Sent and no status" (C1). Neither matters today:
`BotApi::call` never passes `Some(true)` (a success envelope returns before `classify`, whatever the HTTP status,
as Telegram's contract allows), and on the inputs it does pass, both rules equal the design's. A new caller of
`classify` must keep those input shapes.

### NR3: a rollback can leave a reminder `sending` forever (medium if followed; procedure)

R1 with the rollback order as specified (Rust `writer=node` and scheduler off, then Node on), 7 actions:

```
Operator [Node disabled]  Operator [Rust local]  CreateTelegram [fails visibly]  Read(Rust) [reminder 0 claimed: sending]
Operator [Rust restarted writer=node, the tick dies before the send]  Operator [Node enabled]  Settle
```

Node schedules only `pending` reminders and Rust's recovery (`fail_stale_claims`) runs only inside `process_due`, so
the reminder is never sent and never `failed`. Fix in the rollback script: restart Rust only when its tick is idle
and no reminder is `sending`, or, once Rust no longer writes and before Node is enabled, set every `sending` to
`failed` with "interrupted; not resent" (the `status_at_tick_start` rule). The drained case holds, and the safety
invariants hold even without it (`Reminders_rollback_safety.cfg`).

### NR4: the first `getUpdates` at `claimed + 1` skipped a claimed update silently (medium; `poller.rs`, f26eb90). **Fixed (5c87ae0).**

```
NodeStop RustStart Receive [claim 1 written]  Crash [before the handler]  Restart [offset = 2]  Receive
```

The request with `offset = 2` confirms update 1, so Telegram never re-delivers it and the "skipped" log never runs.
At most once still held. At f26eb90 `poll` sent `claim::next_offset(claimed)` on every request, the first one too,
so a crash (watchdog kill, OOM) between `ClaimFile::store` and the end of the handlers lost the rest of that batch
without a log line. Fixed in 5c87ae0 (branch `lucina-rs`): the first `getUpdates` after each lease
acquisition sends offset 0, Telegram re-delivers what is unconfirmed, and `plan_batch` skips the claimed ids with a
warning and the `skippedReplayed` counter (test `a_restart_after_claiming_never_handles_an_update_twice`). The R4
code case now models this and holds; the old behaviour stays as the `MUST FAIL` witness
(`Updates_start_from_claim_bug.cfg`).

### NR5: stopping Node in the middle of a Telegram handler makes Rust reply again (low-medium; cutover) **Moot since 2026-10-27** (Node removed)

```
NodeReceive NodeHandle [Node replied]  NodeStop [offset not synced]  RustStart Receive Handle   -> two replies
```

telegraf confirms a batch only with its next `getUpdates` (or `syncUpdateOffset` when polling stops), and Node's
`shutdown` exits when its HTTP server closes. A handled but unconfirmed update reaches Rust, whose claim file is
empty. Fix: stop Node's Telegram only while it is long-polling with no handler running (its pending `getUpdates`
has then confirmed everything it handled), the same idle rule as for its reminder tick. Rust cannot tell these
updates apart without dropping new ones.

### NR6: a rollback to Node drops the Telegram updates queued in between (low; rollback) **Moot since 2026-10-27** (Node removed)

```
NodeStop RustStart Rollback [Rust stops; Node takes the lock and calls deleteWebhook(drop_pending_updates: true)]
```

`index.js::launchManagedTelegramPolling` drops Telegram's queue whenever Node acquires the lock (every Node restart
does this today). Messages sent between Rust's last `getUpdates` and Node's start are lost without a log. Without a
Node change this cannot be closed; the rollback runbook should say so and run when the owner is not writing.

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
- **Pokémon.** Every handler and macro step is one critical section of the emulator lock (as in the code, except the
  current clock thread, modelled in two steps). Two macro callers, one restart, battles start on a frame with input.
  Story steps are checked by their event flags; the kernel does not model RAM.
- **Media backup.** A file's hash is its identity (blake3 collisions are ignored); each photo is two bytes, so a
  PUT can be cut once in the middle. Each server step is atomic under the catalog's `Mutex<Connection>`. p1's token
  bucket is a fixed window of `limit` requests that `Tick` refills; the 4 s pacing is in Verus (e), not in the
  model, so the model's client is more aggressive than the real one. Edits (NewVersion) are in Verus (d), not in
  the model.
- **Voice memos.** One recording; each worker step is atomic except the two split ones (rsync then meta,
  send then meta). The upload is one step (its protocol is modelled in `tests/upload_model.rs`). The app
  inbox deduplicates by key (secretary-server `NewNotification::keys`). Liveness assumes the other GPU job and
  the network eventually let go (bounded budgets).
- **Deal watch.** A run takes seconds, so clock windows and source changes happen between runs; the digest
  cooldown (10 min) is not modelled (a retry may come at any later time). Item names, body length and the five-item
  limit are not modelled (the real-code tests keep `compose` exact). The server keeps every dedup key (true for a few
  watch messages: 4096 LRU keys, 500 inbox items, no deletions).
- **Drive index.** Two Drive files and a stray position; the folder is the disk's mountpoint (the harder case: a
  Drive folder below it simply disappears when the disk does, and `root_ok` is false). A file read by path while its
  filesystem is gone is an error (Linux: `metadata` fails); a scan started on the empty mountpoint sees only what is
  there. blake3 collisions are ignored; the text index and the worker are not in the model (they only add rows).
- **Node retirement.** One Rust crash per model; operator steps are atomic restarts. R3 has three Telegram ticks
  of 30 s and one boot (the Verus lease kernel covers boot ids and exact times); R4 has two queued updates and no
  new arrivals; the Discord presence watcher (a separate read-only session) is out of scope.
- **Bounds.** The bounds are small (2–4 lines per writer, 2 jobs, 2 reminders, 3–5 requests). The Verus kernels
  cover the per-step invariants for all inputs.
