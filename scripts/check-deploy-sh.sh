#!/bin/sh
# The deploy scripts' own tests, as one command: every deploy/**/test-*.sh, each in its own process.
# They are hermetic - a stub systemctl and curl first on PATH, HOME and the XDG dirs in a scratch tree
# under ${TMPDIR:-$HOME/scratch/tmp} that each script removes on exit, failure included - and none of
# them installs, starts, stops or restarts anything.
#
# lagent merge runs this as the gate "deploy shell tests" (.lucina/pipeline.toml) whenever a change
# touches deploy/. scripts/check-fast covers shellcheck and gitleaks on those files but runs no shell
# test, so a deploy-script regression with nothing red is exactly what this gate is for.
set -eu
CDPATH=
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

n=0
for t in deploy/test-*.sh deploy/*/test-*.sh deploy/*/*/test-*.sh; do
    [ -f "$t" ] || continue
    printf '== %s\n' "$t"
    sh "$t"
    n=$((n + 1))
done
if [ "$n" = 0 ]; then
    printf 'check-deploy-sh: no test script found under deploy/: that is not a pass\n' >&2
    exit 1
fi
printf 'check-deploy-sh: %s test script(s) passed\n' "$n"
