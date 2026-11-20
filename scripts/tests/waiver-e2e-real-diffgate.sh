#!/bin/bash
# Manual proof, run on a host that has the real `diffgate` (no cargo test reads this file; CI hosts have no diffgate).
#
# A scratch repo whose base holds two tests and whose work commit removes both, so diffgate fires exactly two
# `test-removed` refusals and nothing else. For each allow below it runs the merge's own diffgate pass and then the
# `diffgate` step of the built lucina-ci (`check-fast --merge-gate`, the allow in the job's waivers file) and prints
# PASS or FAIL per case; the exit status is non-zero if any case fails.
#   a  --allow test-removed=2:<reason>   merge passes, step passes and logs the reason and "1 forwarded"
#   b  =1                                merge fails, step fails
#   c  =3                                merge fails, step fails
#   d  no allow                          merge fails, step fails
#   e  uncounted test-removed=<reason>   merge passes, step fails (counted form only)
#
# Usage: scripts/tests/waiver-e2e-real-diffgate.sh <repo root>
# Builds nothing: it needs tools/ci/lucina-ci/target/debug/lucina-ci already built and diffgate on PATH or in ~/.local/bin.
set -u
[ $# -eq 1 ] || { echo "usage: $0 <repo root>" >&2; exit 2; }
REPO=$(cd "$1" && pwd) || exit 2
CI=$REPO/tools/ci/lucina-ci
BIN=$CI/target/debug/lucina-ci
[ -x "$BIN" ] || { echo "no built lucina-ci at $BIN" >&2; exit 2; }
DG=$(command -v diffgate || true)
[ -n "$DG" ] || DG=$HOME/.local/bin/diffgate
[ -x "$DG" ] || { echo "diffgate is not installed here: this proof needs the real tool" >&2; exit 2; }
TMPDIR=${TMPDIR:-$HOME/scratch/tmp}
mkdir -p "$TMPDIR"
D=$(mktemp -d "$TMPDIR/waiver-e2e.XXXXXX") || exit 2
trap 'rm -rf "$D"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
unset GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0

W=$D/repo
mkdir -p "$W/tools/ci" "$W/fixture/src" "$D/bin"
printf '#!/bin/sh\nexit 0\n' > "$D/bin/gitleaks"
chmod +x "$D/bin/gitleaks"
cp "$REPO/tools/ci/merge-waiver-rules.txt" "$W/tools/ci/"
# A launcher that checks its own sources needs them beside the repo under test.
if grep -q 'verify_self_copy' "$CI/src/main.rs"; then
  mkdir -p "$W/tools/ci/lucina-ci"
  cp -R "$CI/src" "$CI/Cargo.toml" "$CI/Cargo.lock" "$W/tools/ci/lucina-ci/"
  [ -f "$CI/build.rs" ] && cp "$CI/build.rs" "$W/tools/ci/lucina-ci/"
fi
cd "$W" || exit 2
git init -q -b main
cat > fixture/src/lib.rs <<'RS'
pub fn a() {}

#[cfg(test)]
mod t {
    #[test]
    fn one() {
        super::a();
    }

    #[test]
    fn two() {
        super::a();
    }
}
RS
git add -A
git commit -qm base
git checkout -q -b work
printf 'pub fn a() {}\n' > fixture/src/lib.rs
git commit -qam "remove both tests"

REASON="two obsolete tests deleted with the code they covered"
FAILS=0
verdict() { # $1 = case, $2 = 0 when it held
  if [ "$2" -eq 0 ]; then echo "PASS $1"; else echo "FAIL $1"; FAILS=$((FAILS + 1)); fi
}
# Runs the merge's diffgate (merge_ok = its exit status) and the step (step_line, step_out). $1 = allow after test-removed=,
# empty for no allow.
run_case() {
  local allow=$1 waivers=
  if [ -n "$allow" ]; then
    "$DG" --repo . --base main --allow "test-removed=$allow" > "$D/merge.out" 2>&1
    merge_ok=$?
    printf '{"rule":"test-removed","reason":"%s","by":"lead","job":"job_1"}\n' "$allow" > "$D/waivers.jsonl"
    waivers=$D/waivers.jsonl
  else
    "$DG" --repo . --base main > "$D/merge.out" 2>&1
    merge_ok=$?
  fi
  if [ -n "$waivers" ]; then
    step_out=$(PATH="$D/bin:$PATH" LAGENT_WAIVERS_FILE=$waivers "$BIN" check-fast --base main --merge-gate --no-build --mem-cap-gib 1 2>&1)
  else
    step_out=$(PATH="$D/bin:$PATH" env -u LAGENT_WAIVERS_FILE "$BIN" check-fast --base main --merge-gate --no-build --mem-cap-gib 1 2>&1)
  fi
  step_line=$(printf '%s\n' "$step_out" | grep -E '^(ok|FAIL) +diffgate' | head -1)
  echo "  merge diffgate exit=$merge_ok: $(grep -E '^diffgate:|waiver expects' "$D/merge.out" | tr '\n' ' ')"
  echo "  step: $step_line"
  printf '%s\n' "$step_out" | grep -E 'diffgate waivers:|waiver expects' | sed 's/^/    /'
}

echo "=== a: test-removed=2"
run_case "2:$REASON"
ok=0
[ "$merge_ok" -eq 0 ] || ok=1
case $step_line in ok*) ;; *) ok=1 ;; esac
printf '%s\n' "$step_out" | grep -qF "diffgate waivers: 1 forwarded (test-removed: 2:$REASON), 0 not forwarded" || ok=1
verdict "a (=2 passes the merge and the step, reason logged)" $ok

for n in 1 3; do
  echo "=== $n: test-removed=$n"
  run_case "$n:$REASON"
  ok=0
  [ "$merge_ok" -ne 0 ] || ok=1
  case $step_line in FAIL*) ;; *) ok=1 ;; esac
  printf '%s\n' "$step_out" | grep -qF "waiver expects exactly $n test-removed refusals, found 2" || ok=1
  verdict "$([ "$n" -eq 1 ] && echo b || echo c) (=$n fails the merge and the step)" $ok
done

echo "=== d: no allow"
run_case ""
ok=0
[ "$merge_ok" -ne 0 ] || ok=1
case $step_line in FAIL*) ;; *) ok=1 ;; esac
verdict "d (no allow fails the merge and the step)" $ok

echo "=== e: uncounted test-removed=<reason>"
run_case "$REASON"
ok=0
[ "$merge_ok" -eq 0 ] || ok=1
case $step_line in FAIL*) ;; *) ok=1 ;; esac
printf '%s\n' "$step_out" | grep -qF "test-removed is forwarded in counted form only" || ok=1
verdict "e (uncounted passes the merge, fails the step)" $ok

[ "$FAILS" -eq 0 ] && echo "all cases held" || echo "$FAILS case(s) failed"
[ "$FAILS" -eq 0 ]
