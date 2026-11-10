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

Every step runs in a capped scope: 8 GiB, 4 CPUs, at most 256 tasks, its own time limit. All worktrees share one Cargo target, `~/.cache/lucina-check-target` (the one LucinaOS uses), so the loop adds no large target directory, and it refuses to build with under 20 GiB free. The launcher `tools/ci/lucina-ci` builds apart, in the small `~/.cache/secretary-ci-tool`, because LucinaOS's launcher has the same package name.

Measured on nezopt, 2026-11-10, on this CI change (25 files, the launcher crate, scripts and workflow): 24 s, 8 steps passed, the 55 reachable proofs left to GitHub.

## In `lagent merge`

Besides diffgate and `cargo check --workspace`:

- **proofs green on GitHub** (`scripts/gate-github-checks.sh`): a branch that reaches a proof unit needs `verus-gate` = `success` on its exact commit. Push the branch first (`git push origin <branch>`). See [VERUS.md](VERUS.md).
- **fast local check** (`scripts/check-fast --base HEAD^1 --merge-gate`): the queue is shared, so a step past 180 s is skipped and printed rather than holding up every other merge.

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
