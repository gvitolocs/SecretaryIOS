#!/usr/bin/env bash
# Install lucina-agentd and lagent on nezopt from this checkout (user unit, no root):
#   deploy/lucina-agentd/install.sh        (what `lagent deploy lucina-agentd` runs, after deploy-guard check)
# - builds both binaries (cargo is incremental; an old binary must never be installed by mistake);
# - keeps the running ones as ~/.local/bin/<name>.bak-<UTC stamp>-pre-<sha8>;
# - restarts lucina-agentd and waits for /health on its socket. The unit file and its drop-ins
#   (Honcho MCP, Infisical) are not touched: deploy/lucina-agentd.service is only the template.
# A pipeline job that runs this keeps going while agentd restarts: jobs are their own systemd units.
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
BIN=$HOME/.local/bin
SHA=$(git -C "$REPO" rev-parse --short=8 HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)

(cd "$REPO" && cargo build --release -p lucina-agentd --bins)

for name in lucina-agentd lagent; do
  [ -f "$BIN/$name" ] && cp -p "$BIN/$name" "$BIN/$name.bak-$STAMP-pre-$SHA"
  install -m 0755 "$TARGET/release/$name" "$BIN/$name.new"
  mv -f "$BIN/$name.new" "$BIN/$name"
done
echo "installed lucina-agentd + lagent at $SHA (previous: *.bak-$STAMP-pre-$SHA)"

# `restart` does not fight the unit's Restart=always: systemd queues no start for a stop it was asked
# for, and a restart job is that suppressed stop plus one start. No maintenance marker wanted: the
# watchdog only reads a marker while the unit is inactive, and a restart leaves it active.
systemctl --user restart lucina-agentd.service
# A deploy overrules an earlier stop on purpose (deploy/lucina-maint wrote that marker, and the unit
# is up now); leaving it would excuse the next unexpected exit until it expired.
rm -f "$HOME/.local/state/lucina/maintenance/lucina-agentd.service"
for _ in $(seq 1 30); do
  if "$BIN/lagent" --json providers >/dev/null 2>&1; then
    systemctl --user --no-pager --lines=0 status lucina-agentd.service | head -3
    exit 0
  fi
  sleep 1
done
echo "lucina-agentd did not answer within 30 s; roll back: cp -p $BIN/lucina-agentd.bak-$STAMP-pre-$SHA $BIN/lucina-agentd && systemctl --user restart lucina-agentd" >&2
exit 1
