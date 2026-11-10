#!/usr/bin/env bash
# lagent merge gate "proofs green on GitHub" (docs/ci/VERUS.md, "The merge gate"). Run in the merge candidate, where HEAD^1 is main
# and HEAD^2 the branch commit. If the branch reaches no proof unit (tools/ci/proof-inventory.tsv), there is nothing to verify and
# it passes, saying so. Otherwise GitHub Actions must have run `verus-gate` on that exact commit with `success`: missing, queued,
# running, failed, cancelled, skipped or neutral refuses, and so does GitHub not answering. It also refuses when main moved in
# the inputs of a unit the branch changes (merge main into the branch, push, let CI run). Proofs are never rerun here.
# Flow: lagent task start -> commit -> git push origin <branch> -> CI -> lagent merge.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
repo=${SECRETARY_CI_REPO:-gvitolocs/SecretaryIOS}
if git rev-parse -q --verify HEAD^2 >/dev/null; then main=$(git rev-parse HEAD^1); sha=$(git rev-parse HEAD^2); else main=$(git rev-parse origin/main); sha=$(git rev-parse HEAD); fi
base=$(git merge-base "$main" "$sha")
export CARGO_TARGET_DIR=${SECRETARY_CI_TOOL_TARGET:-$HOME/.cache/secretary-ci-tool}
cargo build --quiet --release --locked --manifest-path tools/ci/lucina-ci/Cargo.toml
ci=$CARGO_TARGET_DIR/release/lucina-ci
ci_files=$(git diff --name-only "$base" "$sha" -- tools/ci .github/workflows scripts/verify-shipped.sh | head -1)
plan=$(mktemp); trap 'rm -f "$plan"' EXIT
"$ci" plan --event push --ref refs/heads/merge-candidate --before "$base" --sha "$sha" --plan "$plan" >/dev/null
n=$(grep -c . "$plan" || true)
if [ "$n" -eq 0 ] && [ -z "$ci_files" ]; then
    echo "gate: the branch reaches no proof unit: nothing for GitHub to verify"
    exit 0
fi
echo "gate: the branch reaches $n proof unit(s); asking GitHub about ${sha:0:12}"
if ! "$ci" merge-check --repo "$repo" --sha "$sha" --require verus-gate --base "$main"; then
    echo "gate: push the branch (git push origin HEAD), wait for verus-gate on ${sha:0:12}, then lagent merge again"
    exit 1
fi
