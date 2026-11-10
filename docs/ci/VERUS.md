# Verus proofs on GitHub Actions

SecretaryIOS's Verus proofs run on GitHub Actions, one proof unit per runner. Fmt, Clippy, tests and diffgate run locally in the `lagent` pipeline: [SHIFT-LEFT.md](SHIFT-LEFT.md). This is the LucinaOS Rust CI standard (LucinaOS `docs/handoffs/RUST-CI-STANDARD.md`, `docs/ci/VERUS.md`), adapted. Status and measurements are at the end.

## Rules

- Proofs run on GitHub Actions, **one proof unit per job and runner** (`fail-fast: false`).
- The repository is **private, so its runners have 8 GB** (about 7.6 GiB usable). Each proof is capped at **6 GiB**, not 12. Nothing here claims a 16 GB runner.
- Runners are split, not pooled: a proof over the cap is split at a sound boundary or reported as a resource blocker.
- **CI never deploys, restarts a service or runs a migration.** It has read-only permissions and no secrets. Deploys go only through `lagent deploy` from `main`.
- The visibility of the repository is never changed for CI.

## What runs

Everything is in `tools/ci/` and `.github/workflows/verus.yml`. `tools/ci/lucina-ci` is LucinaOS's launcher, adapted (inventory, one job per unit, the lock file path).

| Kind | Found at | Unit | Passes when |
| --- | --- | --- | --- |
| Verus file | `verification/verus/<stem>.rs`, `crates/<c>/verus/<stem>.rs` | `verus/<stem>`, `verus/<c>/<stem>` | Verus exits 0 with more than 0 verified and 0 errors |
| Shipped file with `//@` proofs | the `files=(...)` list in `scripts/verify-shipped.sh` | `shipped/<crate>/<stem>` | the same, and what Verus reads is the shipped text |
| Mutant files | `verification/verus/mutants/<kernel>/*.rs` | `mutants/verus-<kernel>` | every file is refused **with a verification error** (`tools/ci/verus-mutant-files.sh`); a mutant that does not compile is `STALE` and fails |
| Mutant script | `verification/<dir>/mutants.sh` | `mutants/<dir>` | the script exits 0 (every planted bug refused, none `NOT FOUND`) |
| Stateright models | `verification/models` | `models/stateright` | `cargo test -p verification-models` passes (`tools/ci/run-models.sh`) |

- **Nothing can be left out.** `lucina-ci inventory --check` runs first in every run and fails if `tools/ci/proof-inventory.tsv` differs from what is on disk. A tracked `.rs` file with a `verus!` block that no unit runs, a shipped file with `//@` lines that `verify-shipped.sh` does not list, an unplaceable file under `verification/`, or a key with no group in `tools/ci/proof-groups.tsv` is an error. [PROOF-MATRIX.md](PROOF-MATRIX.md) is generated from the same list.
- **Why the units can run on separate machines.** Every proof is plain `verus <file>`; no file uses `--import`/`--export` and no crate declares `[package.metadata.verus]`, so no unit trusts another's verified output. A unit's inputs are its own file, what it pulls in with `#[path]`/`include!`, and the toolchain.
- **Not in CI** (they stay in `scripts/verify.sh`, run locally through `verify-gate`): `verification/verus/diff/` (plain-Rust differential tests against `lucina-deals` and `lucina-pokemon` at pinned revisions in other repositories), and the TLC and Quint specs under `formal/` (they need Java and `tla2tools.jar`, not pinned in this repository yet).

## Memory and time

Each unit runs in its own cgroup v2 scope (`systemd-run --scope`, `MemoryMax`, `MemorySwapMax=0`, `OOMPolicy=continue`). The launcher reads `memory.max` back and refuses to start if it is not what was asked, then reads `memory.peak` and `memory.events` after the run. The cap covers the whole process tree (Verus, Z3, rustc, Cargo).

- The launcher reads `MemTotal`. A 12 GiB cap needs 14 GiB; this runner has about 7.6, so it falls back to `VERUS_SMALL_RUNNER_CAP_GIB` (6 by default in the workflow; a repository variable can override it). Each report says `runner_class: small-runner`, `mem_total_bytes` and the real `cap_bytes`. A runner too small even for that is refused, never run uncapped.
- An OOM kill is `oom`, never a failed proof and never a refused mutant. A unit past 30 minutes is `timeout`; each job also has `timeout-minutes: 60`. Under 1 GiB of free disk the unit is stopped as `infra_error`.

| Outcome | Meaning | Exit |
| --- | --- | --- |
| `verified` | Exit 0 and, for proofs, more than 0 verified with 0 errors | 0 |
| `proof_failed` | Verus refused it | 1 |
| `vacuous` | Exit 0, nothing verified | 1 |
| `mutant_survived` | A planted bug was accepted, or a mutant went stale | 1 |
| `infra_error` | Cap not applied, disk full, launcher could not start | 2 |
| `oom` | The cap was hit | 3 |
| `timeout` | The wall-clock limit was hit | 4 |

One JSON report per unit is uploaded as an artifact: outcome, peak, cap, MemTotal, runner class and name, duration, verified count, commit, Verus/Z3 versions and binary checksums.

## Pinning

`tools/ci/verus.lock` pins the Verus release (the same as LucinaOS and nezopt's `~/tools/verus`), the zip's sha256, the sha256 of `verus` and `rust_verify`, and the Rust toolchain. `tools/ci/install-verus.sh` checks the zip; `lucina-ci` checks both binaries before any proof. Actions are pinned by full SHA. Caches hold only the Verus zip (checked again); proof results are never cached.

## When CI runs

| Event | What runs |
| --- | --- |
| Push to any branch, `main` included | The units the change reaches (from the previous commit, or from the merge base with `origin/main` for a new branch). Nothing reached: no proof job, `verus-gate` still runs |
| Push to `proof/<unit>` | Exactly that unit (`tools/ci/create-proof-branches.sh <remote>` makes one branch per unit) |
| Weekly (Monday 03:17 UTC), `workflow_dispatch`, pull request | All units |

A change to `tools/ci/`, the workflow, `tools/ci/verus.lock` or `scripts/verify-shipped.sh` reaches every unit. `main` is not run in full on every push because each `lagent merge` pushes it and a private repository pays for its minutes; the weekly run covers drift. `max-parallel` is 10 (repository variable `VERUS_MAX_PARALLEL`). Runs are never cancelled by newer ones.

## The required check

`verus-gate` always runs (`if: always()`) and passes only if the plan job succeeded, the self-test and proof jobs succeeded (or were both skipped because nothing was planned), and every planned unit has a `verified` report for this exact commit. A skipped, cancelled, timed-out or missing job or report fails it. No required job is skipped by a path filter.

The self-test, on the same kind of runner: a grandchild allocating past a cap is `oom`, an allocation past the job's real cap (6 GiB) is `oom`, a sleeper past its limit is `timeout`, and `tools/ci/fixtures/good.rs`, `bad.rs`, `vacuous.rs` are `verified`, `proof_failed`, `vacuous`.

## The lagent merge gate

`.lucina/pipeline.toml` gate "proofs green on GitHub" runs `scripts/gate-github-checks.sh` in the merge candidate (`HEAD^1` = main, `HEAD^2` = the branch commit):

- If the branch reaches no proof unit, it passes and says so.
- Otherwise GitHub must report `verus-gate` = `success` from GitHub Actions on that exact commit. Missing, queued, running, failed, cancelled, timed out, skipped or neutral refuses, and so does GitHub not answering.
- It also refuses when `main` moved in the inputs of a unit the branch changes: merge `main` into the branch, push, let CI run.

The flow: `lagent task start` → commit → `git push origin <branch>` → CI → `lagent merge`. Proofs are never rerun on nezopt. To debug one by hand there, hold the verify lock: `verify-gate -- <cmd>`, or `flock /tmp/lucina-verify.lock tools/ci/lucina-ci/target/release/lucina-ci run --unit <unit> --verus ~/tools/verus/verus-x86-linux/verus --out /tmp/r --mem-cap-gib 6`.

## Adding a proof

Add the file, give its stem (or crate, or mutant folder) a group in `tools/ci/proof-groups.tsv`, run `lucina-ci inventory --write`, commit the result. CI refuses the branch until then.

## Status and evidence

RESULTS
