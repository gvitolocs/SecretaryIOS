# The feedback loop: local first, proofs on GitHub

The fast checks run on nezopt, in the local `lagent` pipeline. GitHub runs only the Verus proofs ([VERUS.md](VERUS.md)). There is no Rust job on GitHub for this repository: fmt, Clippy and tests run here.

| Tier | Runs | Where | Decides |
| --- | --- | --- | --- |
| Fast check | `scripts/check-fast` | Your worktree, before a commit | Nothing by itself; it tells you early |
| Lane result | `lane review-run` | Automatic, when a lane finishes | Nothing: it queues the result for a reviewer |
| Merge gates | `.lucina/pipeline.toml` | `lagent merge` | Whether the change lands |
| Proofs | `.github/workflows/verus.yml` | GitHub | `verus-gate`, read by the merge gate |

## `scripts/check-fast`

Run it before you commit. It checks only what the change touched, against `--base` (use `origin/main`: the local `main` ref of the main checkout can be stale):

- `diffgate`; rustfmt and Clippy on the touched files; the unit tests (`--lib`) of the touched workspace crates; standalone crates (their own `Cargo.toml` outside the workspace) through `--manifest-path`
- `shellcheck` on changed shell scripts, `actionlint` on changed workflows, `gitleaks` on the changed files, each only when installed
- the proof inventory (`lucina-ci inventory --check`). The Verus proofs the change reaches run on GitHub; `--proofs` runs them here (`--mutants` adds their mutants, `--all-proofs` lifts the limit of 12). It never waits on `/tmp/lucina-verify.lock`; if the lock is busy it says so

A missing tool or a skipped step is printed as skipped, never counted as passed. Older rustfmt and Clippy findings in files you did not touch do not fail the loop; findings in a file you touched do, even if they are old.

With `--merge-gate` the `diffgate` step honours the lead's waivers: it reads `LAGENT_ALLOW`, the entries of `lagent merge --allow RULE=reason`, and passes each one that is not one of lagent's own rules (`gate-change`, `spec-change`) to diffgate as `--allow RULE=reason`, printing every waiver it takes as `waived: <rule> = <reason>` and counting them on the step's line. Without `--merge-gate` the variable is ignored and the step says so, so neither a worker nor a lane verify step can waive diffgate for itself; an entry that is not `RULE=reason` fails the step and is named.

**The job's waivers file.** `lagent merge --allow RULE=reason` also writes every allow of the job to `<job dir>/waivers.jsonl` — one JSON object per line with exactly the four string fields `rule`, `reason`, `by`, `job` — and names that file in `LAGENT_WAIVERS_FILE` for its gate commands (`crates/lucina-agentd/src/pipeline/repo.rs` `waiver_lines`, LA-C014); it is set only when the job was given at least one allow, so an ordinary run of the fast check reads nothing and its `diffgate` step is the plain command it always was. The step of `check-fast --merge-gate` reads it and forwards each waiver whose rule the repository lists as `--allow RULE=reason` with the reason unchanged (so diffgate's counted form `RULE=N:reason` reaches it byte for byte), prints one line `diffgate waivers: N forwarded (…), M not forwarded (…)`, and FAILS the step — waiving nothing, never skipping — when the file cannot be read exactly: an unreadable or empty file, a line that is not JSON, a missing, repeated or fifth field, a non-string value, a blank reason each name the file, the line and the field (finding GATE-01: job_4ee819df2427 was allowed at the merge and its own diffgate pass then refused those very lines with "5 refusal(s), 0 waived"); a rule `LAGENT_ALLOW` already handed over travels once, because diffgate answers a waiver with the refusals the earlier entry left and calls the copy of it a failed waiver. Which rules may travel is data, not a constant: `tools/ci/merge-waiver-rules.txt` holds one rule name per line, `#` comments allowed, `skipped-test` and `tmp-write` today, a waiver of a rule that is not listed stays with the merge tool and is counted as not forwarded — `gate-change` and `spec-change` among them, because diffgate knows no such rule and forwarding one would fail the whole call with `--allow: unknown rule` — a list this step cannot read forwards nothing, adding a line there is a gate change a reviewer reads, and outside `--merge-gate` the file is ignored exactly like the variable (tests: `tools/ci/lucina-ci/tests/merge_gate_waivers.rs`). A rule name may carry one marker, `counted`: then its waivers travel only in diffgate's counted form `N:reason` and an uncounted one fails the step with `diffgate waivers: <rule> is forwarded in counted form only: write --allow <rule>=N:<reason> with N the exact number of refusals`, diffgate never run with the entries this form did like; `tmp-write` is marked so by the coordinator's allow of 2026-11-18 (LA-C013), `merge waivers: tmp-write may be forwarded, counted form only, for test lines that name /tmp on purpose`, and a word after the rule name that is not `counted` is a marker this step cannot read: it names the line and fails the step rather than read it as no marker.

Every step runs in a capped scope: 8 GiB, 4 CPUs, at most 256 tasks, its own time limit. All worktrees share one Cargo target, `~/.cache/lucina-check-target` (the one LucinaOS uses), so the loop adds no large target directory, and it refuses to build with under 20 GiB free. The launcher `tools/ci/lucina-ci` builds apart, in the small `~/.cache/secretary-ci-tool`, because LucinaOS's launcher has the same package name.

Measured on nezopt, 2026-11-10, on this CI change (25 files, the launcher crate, scripts and workflow): 24 s, 8 steps passed, the 55 reachable proofs left to GitHub.

## In `lagent merge`

Besides diffgate and `cargo check --workspace`:

- **proofs green on GitHub** (`scripts/gate-github-checks.sh`): a branch that reaches a proof unit needs `verus-gate` = `success` on its exact commit. Push the branch first (`git push origin <branch>`). See [VERUS.md](VERUS.md).
- **fast local check** (`scripts/check-fast --base HEAD^1 --merge-gate`): a step is capped at 180 s (`LUCINA_CHECK_FAST_MERGE_CAP_S` injects another limit for tests); a step past its limit FAILS the merge (`<step>: past its N s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree`) and is never reported as skipped (2026-11-12: unit-tests were skipped at the merge gate under load and the merge went through with `0 failed`, job_b6094541ca4c).
- **deploy shell tests** (`sh scripts/check-deploy-sh.sh`, since 2026-11-17, when a change touches `deploy/`): every `deploy/**/test-*.sh` in its own process. The shell scripts under `deploy/` have no Rust harness and check-fast only shellchecks them, so a watchdog rule that stopped watching anything used to merge green. Each test is hermetic: a stub `systemctl`/`curl` first on PATH, `HOME` and the XDG dirs in a scratch tree removed on exit, and no install, start, stop or restart. Run them the same way in a worktree: `sh scripts/check-deploy-sh.sh`.

## Delegated lanes

When a lane finishes successfully, `lane` (or `qcode-run` for Qwen) runs `lane review-run` detached: diffgate, then this repository's local check, and queues the result in `~/.local/state/lane-review/queue.jsonl`. It never merges or commits: a reviewer still reads each diff before `lagent merge`.

```bash
lane review
```

```bash
lane review --done <id>
```

## Not set up here yet

Security (`cargo deny`, `cargo audit`), performance baselines and integration tiers, as LucinaOS has in `scripts/check-tier`, do not exist for SecretaryIOS yet.
