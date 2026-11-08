# Formal-verification gaps (Rust crates on `lucina-rs`)

Audit of 2026-10-26, branch `cloud/verify-gaps` (from `lucina-rs` 61749c0). It covers the pure logic in `crates/*` that
is safety-relevant: parsers, permission checks, rate limits, money and budget math, time windows, dedupe, retry and
backoff. The baseline is the protocol table and policy in `docs/verification.md` (on `main`; protocols 1–14) plus the
kernels already on `lucina-rs` (`body_kernel`, `gym_kernel`, `gym_lifecycle`, `memory_surfaces`, `sync_kernel`). An
item counts as covered when it has a Verus kernel with a differential test against the real code, or a Stateright
model.

This audit left out the areas other teams are changing right now: chat, share and voice notes, maps and navigation,
voice memos, media and the player, Pokémon, Telegram, Discord and reminders (being ported from Node), and deals.
Those areas are listed at the end only where something was noticed in passing. Their code was not changed.

## Ranked list

Risk is impact × how reachable the logic is from outside. **Done** means this branch adds the proof (see the next
section). The other rows are still open.

| # | Risk | Crate / module | Logic | Status |
|---|---|---|---|---|
| 1 | high | `lucina-android-calls/src/number.rs` | `call_number`: the only gate between Lucina's `phone_call` tool or the dialer and Android Telecom. An MMI or USSD code (`*21*…#` call forwarding), a `tel:` URI or a pause would act on the SIM. | **Done**: `dial_number.rs` N1–N6 |
| 2 | high | `secretary-server/src/links.rs` | `private_ip` / `ics_url`: the SSRF guard for ICS subscriptions, which the server fetches. | **Done**: `address_guard.rs` A1–A3. **Bug L1 fixed.** Gap L2 remains (below). |
| 3 | high | `lucina-secrets/src/{policy,reference}.rs` | `Policy::check`, `SecretRef::in_folder`, `hard_denied`: which secrets Lucina may resolve. | **Done**: `secrets_policy.rs` F1–F4, H1–H2, P1–P3. Gap S1 kept as a witness. |
| 4 | high | `secretary-server/src/owner.rs` | Password lockout (`lock_for`, `verify_at`): how fast the owner's password can be guessed. | **Done**: `login_lockout.rs` K1–K5 |
| 5 | med-high | `secretary-mcp/src/{scope,ratelimit}.rs`, scope handling in `oauth.rs` | OAuth scopes at consent and refresh; the attempt limiter for login, registration and token. | **Done**: `mcp_scopes.rs` M1–M5. Latent gap R2 kept as a witness. |
| 6 | med-high | `lucina-delegate/src/{policy,state}.rs` | `hard_limits` (no money, no MCP, depth 1, proactive read-only) and the daily budget. | **Done**: `delegate_limits.rs` D1–D5, B2–B4, plus Stateright `budget_model.rs`. **Finding B1** kept as a `Violated` witness. |
| 7 | medium | `secretary-server/src/device.rs` + `lucina-store/src/device.rs` | Device-event dedupe across retries, restarts and the LRU. A multi-step protocol. | **Done**: Stateright `device_events_model.rs`. **Bug E1 fixed.** |
| 8 | medium | `secretary-server/src/auth.rs` | Bearer vs cookie, CSRF on cookie writes, service token. | Open. The decision is spread over private async glue (`identify`, `authorize`, `csrf_ok`, `state_changing`). A kernel needs a small pure `decide(...)` extracted first. |
| 9 | medium | `secretary-server/src/ratelimit.rs` | Per-device token bucket (`check_at`, f64). | Open. The arithmetic is floating point, which Verus cannot reason about well. Options: a fixed-point rewrite with a kernel, or a Stateright model with rational tokens. |
| 10 | medium | `secretary-calendar/src/view.rs::free_slots` | The gaps Lucina proposes as free time. | **Done** (soundness): `free_slots.rs` FS1–FS4. Completeness is checked only by the diff test. |
| 11 | medium | `secretary-server/src/notify/settings.rs`, the hold rule in `notify/mod.rs` | Quiet hours, DND, the reminder bypass, device overrides. | **Done**: `quiet_hours.rs` Q1–Q5, P1. The held-then-digest protocol is open (item 17). |
| 12 | medium | `secretary-calendar/src/rrule.rs` | The RRULE subset: COUNT, UNTIL, BYDAY ordinals, BYMONTHDAY < 0, BYSETPOS. | Open. It is heavy on chrono. A kernel over (year, month, day) integers for `month_dates` and `set_pos`, with a diff test against `expand`, is feasible. |
| 13 | medium | `lucina-meshy/src/budget.rs` (`Ledger::check_in`, `reserve`, `settle`) | The daily credit ledger. K5 on `main` covers only `worst_case` and `step_fits`. | Open. Not touched: Meshy was uncommitted work in another checkout. Note that `spent_on` sums with `u32 +`, so a huge `consumed_credits` from the API would panic in debug and wrap in release. |
| 14 | med-low | `lucina-llm/src/lib.rs::send` | Retry and backoff (429, 5xx, `Retry-After` capped at 10 s, at most 8 retries). | Open. The logic is inline in async code. Note that `Retry-After: 0` gives up to 8 immediate retries (bounded). |
| 15 | med-low | `lucina-delegate/src/sandbox.rs::check_repo` | Repo path containment for delegated code jobs. | Open. It works on the filesystem (`canonicalize`) and is not pure. `Path::starts_with` is component-wise (correct). There is a TOCTOU window between the check and the clone. |
| 16 | med-low | `secretary-server/src/sessions.rs`, `devices.rs` | Session expiry and revocation, constant-time comparison. | Open. A small kernel ("valid iff unexpired and the device is active") is possible. Most of the logic is file I/O. |
| 17 | med-low | `secretary-server/src/notify/mod.rs` | Held pushes and the digest after quiet hours. Also shared with the deals protocol (G4/G5 server dedup). | Open. Not touched (protocol 14's area). |
| 18 | low | `lucina-secrets/src/cache.rs` | TTL cache that zeroizes on every exit. | Open. Unit tests cover it. A Stateright model of get, insert and purge over time would be cheap. |
| 19 | low | `lucina-tools/src/tools/{calculator,units}.rs` | Arithmetic parser (no eval) and unit tables. | Open. Correctness only, nothing safety-relevant. |
| 20 | low | `lucina-store/src/jobs.rs::may_mark_interrupted` | Ownership rule P1. | Covered by the Stateright `ownership.rs` model on `main`. A three-line Verus kernel would be trivial. |
| 21 | low | `secretary-core/src/validate.rs` | Input validation of device events and commands. | Open. |

Covered elsewhere, so not listed above: OTP extraction, redaction, expiry and the owner rule (protocol 13), shopping
ops (3), OAuth codes and refresh (4), the confirmation gate and attention budget (5), timeline ids (1), memo kernel
(10), share (12), deals (14), the secrets redactor (`secrets_redactor.rs` on `main`), memory surfaces, body and gym.

## What this branch proves

All files are in `verification/verus/` (Verus), `crates/<crate>/tests/` (differential tests and Stateright) or
`formal/<area>/` (TLA+ twins).
Every kernel has a header that maps it to the production code, lists what is proven and gives the reasons for any
missing Stateright model. None uses `assume`, `admit` or `external_body`. Each one has a witness lemma or a
non-vacuity assertion in its diff test.

| Kernel | Verified items | Differential test | What it says |
|---|---:|---|---|
| `dial_number.rs` | 18 | `lucina-android-calls/tests/verus_number.rs` (200k random strings plus every string of up to 4 characters) | An accepted number is `+`? followed by 6–15 ASCII digits, or 112/911. Only whitespace, `(`, `)` and `-` are dropped. Any `*`, `#`, `,`, `;` or letter is refused. Normalising is idempotent. |
| `address_guard.rs` | 11 | `secretary-server/tests/verus_ics_url.rs` (60k literals, IPv4-mapped forms, WHATWG IPv4 spellings) | The guard equals the documented table. `::ffff:a` is refused iff `a` is. Every named range is refused whole. The old code's hole is the lemma `witness_l1_mapped_loopback`. |
| `secrets_policy.rs` | 36 | `lucina-secrets/tests/verus_policy.rs` (60k random refs × scopes × allow lists) | The folder scope is segment-aware (`/lucina-evil` and `/lucinax` are outside `/lucina`), ignores trailing slashes, and fails closed for a folder without its leading `/`. Hard terms survive case and separators in the key and the path. A hard term beats any allow name. |
| `login_lockout.rs` | 21 | `secretary-server/tests/verus_lockout.rs` (the real `OwnerStore` on disk, Argon2 included) | The lock schedule is 15 min, doubling, at most 24 h, and monotone. Inductive invariant over every step: from the 10th failure on, password checks are at least `lock_for(failed)` ≥ 900 s apart. |
| `mcp_scopes.rs` | 34 | `secretary-mcp/tests/verus_scopes.rs` (every list of up to 4 names; the real `Limiter`) | Unknown names add nothing. Consent is within the request. Refreshes never widen, along any chain. Inductive per-key limiter bound: at most `max` accepted attempts in any window ending at a check. |
| `delegate_limits.rs` | 15 | `lucina-delegate/tests/verus_limits.rs` (50k requests; `StateStore` runs over day changes) | MCP turns, depth ≥ 1, the marker and money intents never delegate. Proactive delegation is read-only with no workspace. A reserve is accepted only below every cap, and a pause notice comes once. |
| `free_slots.rs` | 4 | `secretary-calendar/tests/verus_free_slots.rs` (3,000 random three-day calendars, plus a brute-force completeness grid) | Slots fall within the hours and the window, are long enough, overlap no event, come in order, and number at most `max`. |
| `quiet_hours.rs` | 14 | `secretary-server/tests/verus_quiet_hours.rs` (100k random settings) | DND holds everything except a bypassing reminder. The window is half-open and wraps midnight. Exactly `(end − start) mod 1440` minutes a day are quiet. Device overrides only narrow. |

Stateright models (exhaustive BFS, run by `cargo test`), each with a TLA+ twin that has the same state and the same
distinct-state counts under TLC (`formal/run-gap-checks`, 17 cfgs, each with `MUST PASS` or `MUST FAIL` on its first
line):

| Model | Cases | Result |
|---|---|---|
| `lucina-delegate/tests/budget_model.rs` / `formal/delegate/DelegateBudget.tla`: two agents, three days, reserve, skip/release, run, midnight | current code (17,788 states) | `day_total_within_cap` **violated** (B1). `agent_within_cap` holds. All four `sometimes` witnesses are reached. |
| | fix switch: release counts against its reservation's day (14,094 states) | all hold |
| `secretary-server/tests/device_events_model.rs` / `formal/devices/DeviceEvents.tla`: phone outbox (retries, lost responses), secretary-server `SeenIds` (LRU 1, restart), Lucina dedupe and append failure | current code, E1 fixed (4,875 states) | all hold. Witnesses: a duplicate dropped by each side, and a retry after a failed append. |
| | before E1 | `no_silent_loss` **violated** |
| | no Lucina dedupe: concurrent retry / restart / LRU eviction | each **violates** `stored_at_most_once` (rule witnesses: secretary-server's in-memory LRU alone does not give at-most-once) |
| | two devices reuse one event id | `no_silent_loss` **violated** (rule witness for an assumption, below) |

## Findings

### L1: IPv4-mapped IPv6 literals passed the ICS address guard (medium). **Fixed.**

`links::private_ip` judged an IPv6 address only by its own prefixes. As a result,
`https://[::ffff:127.0.0.1]/x.ics`, `[::ffff:10.0.0.5]`, `[::ffff:192.168.1.1]`, the cloud metadata address
`[::ffff:169.254.169.254]` and the Tailscale range were all accepted as public hosts. A dual-stack connect to those
reaches the IPv4 address. The fix judges a mapped address as the IPv4 address it carries (4 lines in `links.rs`).
`tests/verus_ics_url.rs::mapped_ipv4_literals_are_refused` fails on the old code, and the whole diff test fails on it
too.

### L2: the ICS fetch does not check resolved addresses or redirects (medium, open)

`ics_url` checks only literal IPs and `localhost` / `.localhost` / `.internal`. `calendar::fetch_ics` then follows up
to 5 redirects (`Policy::limited(5)`) and resolves names without a check. So a public URL that redirects to
`http://127.0.0.1:…`, or a name that resolves to a private address, still gets fetched. Adding a subscription needs
the owner's device token, which limits the exposure to links the owner pastes. The share extractor already has the
right pattern (`lucina-server/src/share/extract.rs::download`: an allow-list `public_ip`, DNS pinned with
`resolve_to_addrs`, redirects followed by hand and re-checked). Porting it is a larger change than a proof fix, so it
is left for the owner.

### S1: deny terms glued to a digit are not matched (low, kept visible)

`policy::tokens` keeps digits attached to letters (`"api-key2"` → `key2`, documented by the unit test). The short
deny terms are matched only as whole tokens (`pin`, `pwd`), so `BANK_PIN2`, `PIN1` and `SIM_PIN0` pass, while
`BANK_PIN_2` is refused. The hard list (login, sudo, root) is not affected, and the Infisical identity is confined to
Lucina's folder. `tests/verus_policy.rs::witness_s1_pin_with_digit_passes` asserts the current behaviour and must be
flipped when the tokenizer splits digits. Whether to split them is the owner's call: the existing unit test shows
the gluing is deliberate.

### B1: a release after midnight undercounts the new day's delegations (low, kept as a witness)

`Delegator::start` reserves a call, then runs the self-review (a model call) and prepares the sandbox (a git clone).
When the review skips the job, or the sandbox fails, it calls `StateStore::release`, which decrements whatever day
the release runs on. If midnight passed in between, that is the new day. Its shared `total_calls` loses a call it
never counted, so the day can run more than `total_calls_per_day` delegations: one more per agent that was in flight
at midnight, so at most two over the cap of 200. Per-agent caps are not affected, because the one-job-per-agent claim
means the agent has no other reservation on the new day. Shortest path (TLC, caps 2): A reserves on day 0, midnight,
B reserves, A skips (total → 0), A reserves, runs and finishes, A reserves and runs, B runs: day 1 ran 3. The fix is
to pass the reservation's day to `release` and give back only on that day (the model's `fixed` switch). It was not
applied: this is a runaway guard off by at most two, so the fix is the owner's call.

### E1: a failed device-event append made the retry a "duplicate" (low-medium). **Fixed.**

`DeviceEventStore::ingest` (lucina-store) inserted the fresh ids into its dedupe set before appending them to
`device-context.jsonl`. When the append failed, the route returned an error but the ids stayed marked. The phone's
retry was then answered `duplicates`, secretary-server and the phone dropped the events, and they were never stored
(until a restart reloaded the file). The pattern is the same as OTP4 on `main`. Shortest path (TLC, 8 steps): the
phone sends twice, the first request's first event fails to append, the second request forwards it (Lucina:
"duplicate") and the other event, then answers 200, and the phone drops both. Fix: a failed append forgets the
batch's ids before returning the error (7 lines). After a partial write the file may hold a line twice; `load`
already dedupes by id. `crates/lucina-store/tests/device_events_retry.rs` fails on the old code and passes now.

### R2: `Limiter::new(0, _)` panics on the first check (latent, kept visible)

With `max == 0`, `check` indexes `v[0]` of an empty vector. No production limiter has `max` < 5, so this is
unreachable today. `tests/verus_scopes.rs::witness_r2_zero_max_panics` keeps it visible.

### Smaller observations (no change)

- `secretary-mcp` Retry-After is `as_secs()` (truncated) of the remaining window, so it can be up to one second early.
  The client is then refused once more. Harmless.
- `secretary-server` quiet hours with `start == end` silence nothing. Someone who sets `00:00–00:00` may expect "all
  day". This is consistent with `validate`, but worth a UI hint.
- Device events: secretary-server dedupes by (device, id) and Lucina by id alone. If two devices ever send the same
  event id, Lucina drops one (the model's `shared_id_two_devices` witness). This is safe as long as clients use
  globally unique ids (UUIDs). That should be stated in `docs/device-bridge.md`, which does not fix an id format today.
- `lucina-meshy` ledger: `spent_on` sums `u32` values without saturation (see row 13).
- Off-limits areas, noted only: `lucina-server/src/share/extract.rs::public_ip` is an allow-list and handles mapped
  addresses correctly (no L1 there).

## How to run

Verus on nezopt: `verify-gate -- scripts/verify-verus.sh`, or one file with
`verify-gate -- ~/tools/verus/verus-x86-linux/verus verification/verus/<file>.rs`. Every top-level
`verification/verus/*.rs` is picked up automatically. Checked here with the official release **0.2026.10.18.7d82994**
(`verus-0.2026.10.18.7d82994-x86-linux.zip`, Rust toolchain 1.99.1): all 13 files verify (219 items, about 23 s).

Differential tests and models:

```sh
cargo test -p lucina-android-calls --test verus_number
cargo test -p secretary-server --test verus_ics_url --test verus_lockout --test verus_quiet_hours --test device_events_model
cargo test -p lucina-secrets --test verus_policy
cargo test -p secretary-mcp --test verus_scopes
cargo test -p lucina-delegate --test verus_limits --test budget_model
cargo test -p secretary-calendar --test verus_free_slots
cargo test -p lucina-store --test device_events_retry
```

TLA+ twins: `verify-gate -- formal/run-gap-checks` (checked here with tla2tools from the tlaplus v1.8.0 release,
TLC 2026.10.24, OpenJDK 21; under 10 s in total).

When this branch reaches `main`, add the two models and the eight kernels to the inventory in `docs/verification.md`,
add the B1 / E1 / L1 cases to its "States explored" table and its "Bugs found" section, and move the twins' cfgs
into `scripts/verify.sh`'s TLC list (`formal/run-gap-checks` covers them until then).
